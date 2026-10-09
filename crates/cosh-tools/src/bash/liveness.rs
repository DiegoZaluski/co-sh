//! Liveness watchdog for spawned bash commands.
//!
//! A child process is "hung" when it makes progress in *no* observable way
//! for a sustained window: no output, no CPU time growth and no I/O growth
//! across its whole process tree (bash `-c` spawns descendants, so the tree
//! — not just the shell — must be sampled). Any single signal of progress
//! resets the quiescence counter, which is what makes the watchdog safe for
//! long-running legitimate commands: a `cargo build` burns CPU, a download
//! moves bytes over the network, a database import writes to disk — none of
//! them can be killed by this watchdog no matter how long they run, while a
//! command blocked on a prompt or a lock that will never arrive dies within
//! the quiescence window.
//!
//! "Process alive" is deliberately **not** a progress signal: a hung process
//! stays alive forever, so treating aliveness as progress would defeat the
//! watchdog. Caller-side logic uses `try_wait` for the opposite case — the
//! child died but orphaned descendants keep the output pipes/PTY open — and
//! escapes immediately instead of waiting for quiescence.
//!
//! Sampling uses [`sysinfo`], which exposes per-process accumulated CPU time
//! and I/O counters with one API on Linux, macOS and Windows. Sampling runs
//! on a bounded cadence ([`LivenessConfig::sample_interval`]), not on every
//! caller tick, so callers may tick as often as they like.

use std::collections::{HashMap, HashSet, VecDeque};
#[cfg(unix)]
use std::sync::Arc;
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use sysinfo::Pid;
use sysinfo::ProcessRefreshKind;
use sysinfo::ProcessesToUpdate;
use sysinfo::System;

/// Liveness watchdog tuning.
///
/// The defaults are set for agent-driven commands: a command that shows no
/// progress whatsoever for the quiescence window is almost certainly hung,
/// while any real work (compilation, downloads, writes) trips at least one
/// progress signal well inside one sampling interval.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LivenessConfig {
    /// How often callers invoke the watchdog tick.
    pub tick: Duration,
    /// Minimum spacing between expensive process-tree samples.
    pub sample_interval: Duration,
    /// Kill the tree after this much continuous quiescence (no output, no
    /// CPU growth, no I/O growth).
    pub quiescent_after: Duration,
    /// CPU-time growth below this is noise (a single stray syscall), not
    /// progress.
    pub cpu_min_ms: u64,
    /// Byte growth below this is noise (a stat here and there), not progress.
    pub io_min_bytes: u64,
}

impl LivenessConfig {
    /// The default quiescence window in whole seconds, exposed so the
    /// model-facing tool description can state it without duplicating the
    /// constant.
    pub(crate) const fn default_quiescent_secs() -> u64 {
        120
    }

    pub(crate) const fn default_tick() -> Duration {
        Duration::from_secs(10)
    }

    pub(crate) const fn default_sample_interval() -> Duration {
        Duration::from_secs(10)
    }

    /// Aggressive config for tests: quiescence is detected in about a second
    /// instead of two minutes.
    #[cfg(test)]
    pub(crate) fn test_fast() -> Self {
        Self {
            tick: Duration::from_millis(100),
            sample_interval: Duration::from_millis(250),
            quiescent_after: Duration::from_millis(900),
            cpu_min_ms: 1,
            io_min_bytes: 1,
        }
    }
}

impl Default for LivenessConfig {
    fn default() -> Self {
        Self {
            tick: Self::default_tick(),
            sample_interval: Self::default_sample_interval(),
            quiescent_after: Duration::from_secs(Self::default_quiescent_secs()),
            cpu_min_ms: 50,
            io_min_bytes: 64 * 1024,
        }
    }
}

/// Aggregated progress counters for one process tree at one point in time.
///
/// All values are cumulative since process start, so progress between two
/// samples is a positive delta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TreeSample {
    /// Sum of accumulated CPU time over the tree, in milliseconds.
    pub cpu_ms: u64,
    /// Sum of cumulative I/O (read + written; on Windows this includes
    /// network I/O) over the tree, in bytes.
    pub io_bytes: u64,
}

/// Diagnostic emitted when the liveness watchdog kills a quiescent tree.
///
/// Deliberately DESCRIPTIVE ONLY — no suggested commands. The model infers
/// the retry itself; any specific advice could be wrong for the unknown
/// case (same policy as the input-wait watchdog message).
pub(crate) fn liveness_kill_message(quiescent_secs: u64) -> String {
    format!(
        "liveness watchdog: killed a command that produced no output, no CPU time and no \
         I/O progress for {quiescent_secs}s — presumed hung waiting on something that \
         will never arrive (a prompt, a lock, or a dead connection)."
    )
}

/// Diagnostic emitted when the caller escapes a stream whose main child
/// exited but orphaned descendants keep the output channel open.
pub(crate) const ORPHAN_ESCAPE_MESSAGE: &str = concat!(
    "liveness watchdog: the command exited but orphaned child processes were still ",
    "holding the output channel; detached them and returned the command's exit ",
    "status."
);

/// Handle to a running liveness-watchdog thread.
///
/// Dropping the handle signals the thread to stop; it exits within a tick
/// slice. The caller sets the output flag whenever the child produces
/// output — that is one of the progress signals.
///
/// Unix PTY path only: the piped path and the Windows ConPTY path sample
/// inline in their async loops instead of a background thread.
#[cfg(unix)]
pub(crate) struct LivenessThreadHandle {
    stop: Arc<AtomicBool>,
    output_flag: Arc<AtomicBool>,
}

#[cfg(unix)]
impl LivenessThreadHandle {
    /// Record that the child produced output since the last sample.
    pub(crate) fn note_output(&self) {
        self.output_flag.store(true, Ordering::Relaxed);
    }
}

#[cfg(unix)]
impl Drop for LivenessThreadHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Sampling cadence slice of the liveness thread: it wakes up at least
/// this often so it can honor stop requests and the sampling interval.
#[cfg(unix)]
const LIVENESS_THREAD_SLICE: Duration = Duration::from_millis(250);

/// Spawn a background thread that samples the process tree and kills it on
/// sustained quiescence.
///
/// The thread owns the whole sampling/decision cycle — the caller's I/O
/// loop never blocks on process-table refreshes (a full refresh can take
/// hundreds of milliseconds, which would stall output reads if done
/// inline). `on_kill` receives the diagnostic message and must terminate
/// the child; the caller's loop then observes the death through its normal
/// EOF/hangup path.
///
/// Unix PTY path only.
#[cfg(unix)]
pub(crate) fn spawn_liveness_thread(
    child_pid: u32,
    config: LivenessConfig,
    on_kill: Box<dyn FnOnce(&str) + Send + 'static>,
) -> LivenessThreadHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let output_flag = Arc::new(AtomicBool::new(false));
    let stop_flag = stop.clone();
    let output_for_thread = output_flag.clone();
    std::thread::spawn(move || {
        let mut state = LivenessState::new(Some(child_pid), config);
        loop {
            if stop_flag.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(LIVENESS_THREAD_SLICE);
            if stop_flag.load(Ordering::Relaxed) {
                return;
            }
            if !state.sample_due(Instant::now()) {
                continue;
            }
            let now = Instant::now();
            let had_output = output_for_thread.swap(false, Ordering::Relaxed);
            let sample = sample_tree(child_pid);
            if let Some(message) = state.on_sample(now, sample, had_output) {
                on_kill(&message);
                return;
            }
        }
    });
    LivenessThreadHandle { stop, output_flag }
}

/// Terminate the whole process tree rooted at `pid`.
///
/// Windows counterpart of the Unix process-group kill (`kill_process_tree`
/// in `bsh.rs`, `kill(-pgid, SIGKILL)`): Windows has no process groups, so
/// the tree is walked explicitly from fresh process-table snapshots.
///
/// Two Windows realities shape this implementation:
///
/// 1. **Late forks.** Git Bash spawns `bin\bash.exe` → `usr\bin\bash.exe` →
///    the command, and MSYS `fork()` is slow: a kill issued milliseconds
///    after spawn lands mid-fork, so a single snapshot misses grandchildren
///    that are born right afterwards.
/// 2. **Dead parents.** Windows never reparents orphans: a grandchild whose
///    parent is already dead keeps the stale parent pid. A walk rooted at a
///    live pid therefore misses it.
///
/// So the kill tracks a `known` set seeded with the root: every pass takes a
/// fresh snapshot, expands `known` to fixpoint over parent links (dead
/// parents resolve fine - the links persist), and terminates every newly
/// found process with a **single** `taskkill /F /PID …` invocation (one
/// process spawn, not one per pid). Passes repeat with a settle delay until
/// a pass finds nothing new (bounded). Callers always follow up with a
/// direct kill of the root itself.
///
/// Without this, killing the shell alone orphans grandchildren that keep
/// the stdout/stderr pipes open: the stream yields its kill items but the
/// host process hangs until the orphan exits on its own (e.g. `sleep 30`
/// holds the run for the full 30s), and CPU-burning orphans leak forever.
///
/// This is a blocking call (process-table scan plus a `taskkill` spawn);
/// run it on a blocking thread from async contexts.
#[cfg(windows)]
pub(crate) fn kill_process_tree(pid: u32) {
    use std::process::Stdio;

    let mut known: HashSet<Pid> = HashSet::from([Pid::from_u32(pid)]);
    let mut killed: HashSet<Pid> = HashSet::new();
    for pass in 0..3 {
        if pass > 0 {
            // Let in-flight forks land so the next snapshot sees them.
            std::thread::sleep(Duration::from_millis(50));
        }
        let mut system = System::new();
        // Parent links come from the Toolhelp snapshot entry itself, so the
        // minimal refresh kind suffices - no counters are needed to kill.
        system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing(),
        );

        let mut children: HashMap<Pid, Vec<Pid>> = HashMap::new();
        for (process_pid, process) in system.processes() {
            if let Some(parent) = process.parent() {
                children.entry(parent).or_default().push(*process_pid);
            }
        }

        // Expand `known` to fixpoint: the queue starts with everything ever
        // known (parents may be dead - the links still resolve).
        let mut queue: VecDeque<Pid> = known.iter().copied().collect();
        while let Some(current) = queue.pop_front() {
            if let Some(descendants) = children.get(&current) {
                for descendant in descendants {
                    if known.insert(*descendant) {
                        queue.push_back(*descendant);
                    }
                }
            }
        }

        let fresh: Vec<Pid> = known.difference(&killed).copied().collect();
        if fresh.is_empty() {
            break;
        }
        // One taskkill for the whole set: spawning it once per pid would
        // cost ~100ms each and widen the fork race.
        let mut cmd = std::process::Command::new("taskkill.exe");
        cmd.arg("/F").stdin(Stdio::null());
        cmd.stdout(Stdio::null()).stderr(Stdio::null());
        for target in &fresh {
            cmd.arg("/PID").arg(target.to_string());
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // 0x08000000 == CREATE_NO_WINDOW: no console flash.
            cmd.creation_flags(0x08000000);
        }
        let _ = cmd.status();
        killed.extend(fresh);
    }
}

/// Snapshot the whole process tree rooted at `pid`, aggregating cumulative
/// CPU time and I/O counters.
///
/// Returns `None` when the tree cannot be sampled reliably (root process
/// missing from the process table — a transient that must never be
/// interpreted as quiescence). This is a blocking call; run it on a blocking
/// thread from async contexts.
pub(crate) fn sample_tree(pid: u32) -> Option<TreeSample> {
    let mut system = System::new();
    // Refresh every process: the tree is discovered by walking parent links,
    // so descendants must be present. Dead entries are removed so a recycled
    // pid can never masquerade as the old tree and fake progress deltas.
    // Only CPU time and I/O counters are fetched — the minimal refresh kind,
    // because a full refresh can take hundreds of milliseconds on a busy
    // system (the PTY path even runs it on a dedicated thread so output
    // reads are never stalled by a sample).
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_cpu().with_disk_usage(),
    );

    let mut children: HashMap<Pid, Vec<Pid>> = HashMap::new();
    for (process_pid, process) in system.processes() {
        if let Some(parent) = process.parent() {
            children.entry(parent).or_default().push(*process_pid);
        }
    }

    let root = Pid::from_u32(pid);
    if !system.processes().contains_key(&root) {
        return None;
    }

    let mut tree: HashSet<Pid> = HashSet::new();
    let mut queue = VecDeque::from([root]);
    while let Some(current) = queue.pop_front() {
        if tree.insert(current)
            && let Some(descendants) = children.get(&current)
        {
            queue.extend(descendants.iter().copied());
        }
    }

    let mut sample = TreeSample {
        cpu_ms: 0,
        io_bytes: 0,
    };
    for tree_pid in tree {
        let Some(process) = system.process(tree_pid) else {
            continue;
        };
        sample.cpu_ms += process.accumulated_cpu_time();
        let disk = process.disk_usage();
        sample.io_bytes += disk.total_read_bytes + disk.total_written_bytes;
    }
    Some(sample)
}

/// Decision state machine for the liveness watchdog.
///
/// Pure bookkeeping: callers decide when to sample (cheap ticks must never
/// trigger expensive process-table scans — use [`LivenessState::sample_due`]
/// to throttle) and feed the result to [`LivenessState::on_sample`]. For the
/// Unix PTY path, [`spawn_liveness_thread`] wraps both into a background
/// thread so the poll loop never blocks on a refresh.
pub(crate) struct LivenessState {
    config: LivenessConfig,
    child_pid: Option<u32>,
    last_sample: Option<Instant>,
    previous: Option<TreeSample>,
    quiescent_since: Option<Instant>,
}

impl LivenessState {
    pub(crate) fn new(child_pid: Option<u32>, config: LivenessConfig) -> Self {
        Self {
            config,
            child_pid,
            last_sample: None,
            previous: None,
            quiescent_since: None,
        }
    }

    /// Whether the guard is enabled (a child pid is known).
    pub(crate) fn enabled(&self) -> bool {
        self.child_pid.is_some()
    }

    /// The watched child's pid, if known.
    pub(crate) fn child_pid(&self) -> Option<u32> {
        self.child_pid
    }

    /// Whether an expensive process-tree sample is due at `now`.
    ///
    /// `false` when the guard is disabled (no child pid) or the previous
    /// sample is younger than [`LivenessConfig::sample_interval`], so
    /// callers may tick as often as they like.
    pub(crate) fn sample_due(&self, now: Instant) -> bool {
        self.child_pid.is_some()
            && self
                .last_sample
                .is_none_or(|last| now.duration_since(last) >= self.config.sample_interval)
    }

    /// Decision logic for one sample — pure, unit-testable.
    ///
    /// `had_output` reports whether stdout/stderr produced bytes since the
    /// previous sample. Returns `Some(message)` when the tree must be killed
    /// (quiescence window elapsed), `None` to continue.
    pub(crate) fn on_sample(
        &mut self,
        now: Instant,
        sample: Option<TreeSample>,
        had_output: bool,
    ) -> Option<String> {
        // Record the sample time so `sample_due` throttles the expensive
        // process-table scans to `sample_interval` - even for unreliable
        // (None) samples, which still cost a full scan.
        self.last_sample = Some(now);

        // An unreliable sample (tree vanished from the process table — a
        // transient) makes no decision and resets the clock: never count it
        // as progress and never count it as quiescence.
        let Some(sample) = sample else {
            self.quiescent_since = None;
            return None;
        };

        let progressed = had_output
            || match &self.previous {
                // First observation: no delta yet — start the quiescence
                // clock but give the tree the benefit of the doubt.
                None => false,
                Some(previous) => {
                    sample.cpu_ms.saturating_sub(previous.cpu_ms) >= self.config.cpu_min_ms
                        || sample.io_bytes.saturating_sub(previous.io_bytes)
                            >= self.config.io_min_bytes
                }
            };

        self.previous = Some(sample);

        if progressed {
            self.quiescent_since = None;
            return None;
        }

        let since = *self.quiescent_since.get_or_insert(now);
        let quiescent_for = now.duration_since(since);
        if quiescent_for >= self.config.quiescent_after {
            self.quiescent_since = None;
            return Some(liveness_kill_message(quiescent_for.as_secs()));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> LivenessConfig {
        LivenessConfig {
            tick: Duration::from_millis(100),
            sample_interval: Duration::from_millis(250),
            quiescent_after: Duration::from_millis(1000),
            cpu_min_ms: 50,
            io_min_bytes: 1024,
        }
    }

    fn sample(cpu_ms: u64, io_bytes: u64) -> Option<TreeSample> {
        Some(TreeSample { cpu_ms, io_bytes })
    }

    /// Synthetic clock: `t(ms)` is `ms` milliseconds after an arbitrary base.
    /// Only elapsed durations matter to the state machine.
    fn t(ms: u64) -> Instant {
        static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        *BASE.get_or_init(Instant::now) + Duration::from_millis(ms)
    }

    /// A tree that never progresses must be killed after quiescent_after.
    #[test]
    fn kills_after_sustained_quiescence() {
        let mut state = LivenessState::new(Some(1), config());
        // First sample starts the clock; the process is alive and idle.
        assert_eq!(state.on_sample(t(0), sample(0, 0), false), None);
        assert_eq!(state.on_sample(t(500), sample(0, 0), false), None);
        // Still inside the window (0.5s of quiescence).
        assert_eq!(state.on_sample(t(900), sample(0, 0), false), None);
        // Window elapsed → kill.
        let msg = state.on_sample(t(1100), sample(0, 0), false).unwrap();
        assert!(msg.contains("liveness watchdog"), "{msg}");
        assert!(msg.contains("no CPU time"), "{msg}");
    }

    /// Any progress signal — output, CPU or I/O — resets the counter.
    #[test]
    fn progress_resets_quiescence() {
        let mut state = LivenessState::new(Some(1), config());
        assert_eq!(state.on_sample(t(0), sample(0, 0), false), None);
        assert_eq!(state.on_sample(t(500), sample(0, 0), false), None);
        // CPU burst resets the clock.
        assert_eq!(state.on_sample(t(1000), sample(200, 0), false), None);
        assert_eq!(state.on_sample(t(1500), sample(200, 0), false), None);
        // I/O burst resets the clock.
        assert_eq!(state.on_sample(t(2000), sample(200, 4096), false), None);
        assert_eq!(state.on_sample(t(2500), sample(200, 4096), false), None);
        // Output resets the clock.
        assert_eq!(state.on_sample(t(3000), sample(200, 4096), true), None);
        assert_eq!(state.on_sample(t(3500), sample(200, 4096), false), None);
        // From here, continuous silence kills.
        let msg = state.on_sample(t(4600), sample(200, 4096), false).unwrap();
        assert!(msg.contains("liveness watchdog"), "{msg}");
    }

    /// Sub-threshold deltas (noise) do NOT reset the counter.
    #[test]
    fn sub_threshold_deltas_do_not_reset() {
        let mut state = LivenessState::new(Some(1), config());
        assert_eq!(state.on_sample(t(0), sample(0, 0), false), None);
        assert_eq!(state.on_sample(t(400), sample(0, 0), false), None);
        // A stray syscall: 10ms CPU and 100 bytes of I/O — both under the
        // thresholds — must not reset the clock.
        assert_eq!(state.on_sample(t(800), sample(10, 100), false), None);
        let msg = state
            .on_sample(t(1_800), sample(10, 100), false)
            .expect("sub-threshold noise must not postpone the kill");
        assert!(msg.contains("liveness watchdog"), "{msg}");
    }

    /// A vanished tree (unreliable sample) must never kill and must never
    /// accumulate quiescence across repeated failures.
    #[test]
    fn unreliable_sample_never_kills() {
        let mut state = LivenessState::new(Some(1), config());
        assert_eq!(state.on_sample(t(0), sample(0, 0), false), None);
        for i in 1..10 {
            assert_eq!(
                state.on_sample(t(i * 500), None, false),
                None,
                "missing sample must not kill"
            );
        }
        // The clock restarted: a subsequent real sample starts fresh.
        assert_eq!(state.on_sample(t(5_000), sample(0, 0), false), None);
    }

    /// A disabled guard (no pid) never samples.
    #[test]
    fn disabled_without_pid() {
        let state = LivenessState::new(None, config());
        assert!(!state.sample_due(t(0)));
        assert!(!state.sample_due(t(600_000)));
    }

    /// The kill clears the quiescence window: a survived tree starts fresh.
    #[test]
    fn kill_resets_state() {
        let mut state = LivenessState::new(Some(1), config());
        assert_eq!(state.on_sample(t(0), sample(0, 0), false), None);
        assert!(state.on_sample(t(1100), sample(0, 0), false).is_some());
        // After a kill decision the window restarts (caller may keep the
        // process alive one more round on its own discretion).
        assert_eq!(state.on_sample(t(1200), sample(0, 0), false), None);
    }
}
