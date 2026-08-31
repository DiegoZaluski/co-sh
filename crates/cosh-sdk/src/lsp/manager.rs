//! The LSP manager: one engine instance per workspace, many servers inside.
//!
//! Owns discovery and lifecycle for every language server serving a
//! workspace:
//!
//! * **Keying** — clients are keyed by `(root_path, server_name)`, so the
//!   same server can run against nested projects and several servers can
//!   serve one file. opencode's fix for crush's single-server bug (#1751).
//! * **Root resolution** — walks up from the touched file toward the
//!   workspace root matching the spec's root markers; specs without markers
//!   always use the workspace root. A spec whose markers appear nowhere in
//!   the ancestry is skipped entirely — starting a server rooted somewhere
//!   unrelated is how crush fired a Lua server inside a TS repo (#1746).
//! * **Lazy spawn** — nothing starts until a file is touched. The first
//!   caller for a key inserts a `Pending` placeholder under the lock and a
//!   detached driver task resolves it; concurrent callers share the outcome
//!   through a watch channel. Drivers hold `Weak` state so manager drop is
//!   never blocked (the phase-2 deadlock lesson applied at this layer).
//! * **Backoff** — failed spawns park their key for [`SPAWN_BACKOFF`];
//!   retries inside the window fail fast with [`LspError::Unavailable`]
//!   (crush's lesson against hot-looping dead binaries).
//! * **Soft skips** — binaries missing from `PATH` surface as
//!   [`LspError::Unavailable`] and never mask other servers that did answer;
//!   auto-discovery stays quiet about languages you never installed.
//!
//! Server binaries must be on `PATH`; there is no auto-install. Spawning is
//! delegated to a pluggable factory so tests drive the whole manager over
//! in-memory fakes.
use std::{
    collections::HashMap,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Weak},
    time::{Duration, Instant},
};

use tokio::sync::{mpsc, watch};

use super::{
    CATALOG,
    catalog::{ServerSpec, lookup_on_path},
    client::{Event, LanguageServer, LanguageServerConfig, ServerState},
    error::LspError,
};

/// How long a failed spawn keeps its key parked before the next touch may
/// retry.
const SPAWN_BACKOFF: Duration = Duration::from_secs(30);

/// Deadline for the default factory's initialize handshake.
const CLIENT_START_TIMEOUT: Duration = Duration::from_secs(30);

/// Identity of one client connection inside this workspace.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ClientKey {
    /// Project root the server runs against.
    pub root: PathBuf,
    /// Catalog name (`gopls`, …).
    pub server: String,
}

/// What happened to one managed server, tagged with its identity.
#[derive(Clone, Debug)]
pub struct ManagedEvent {
    /// Server catalog name.
    pub server: String,
    /// Project root it runs against.
    pub root: PathBuf,
    /// The underlying client event.
    pub event: Event,
}

/// Aggregate lifecycle snapshot exposed through [`Manager::states`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientLifecycle {
    Starting,
    Ready,
    Failed,
}

/// Knobs for the engine (test injection points included).
pub struct ManagerConfig {
    /// Workspace root every resolved project root must live under.
    pub root: PathBuf,
    /// Where per-server events are delivered (already tagged).
    pub events: Option<mpsc::UnboundedSender<ManagedEvent>>,
    /// Backoff window for failed spawns. Tests shrink this to zero.
    pub spawn_backoff: Duration,
    /// Resolve spec commands against PATH before calling the spawn factory.
    /// Real spawning wants this; injected test factories bring their own
    /// transports and disable it.
    pub resolves_binaries: bool,
}

impl ManagerConfig {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            events: None,
            spawn_backoff: SPAWN_BACKOFF,
            resolves_binaries: true,
        }
    }
}

/// Builds and initializes the client for one key.
pub type SpawnFuture = Pin<Box<dyn Future<Output = Result<LanguageServer, LspError>> + Send>>;
pub type ClientFactory = Arc<dyn Fn(LanguageServerConfig) -> SpawnFuture + Send + Sync>;

/// Terminal result of one spawn attempt, shared with every waiter.
#[derive(Clone)]
enum SpawnOutcome {
    Pending,
    Ready(Arc<LanguageServer>),
    Failed {
        detail: String,
        missing_binary: bool,
    },
}

struct Inner {
    config: ManagerConfig,
    catalog: Vec<ServerSpec>,
    /// One shared outcome per key; `Pending` means a driver owns the spawn.
    outcomes: std::sync::Mutex<HashMap<ClientKey, watch::Receiver<SpawnOutcome>>>,
    /// Failure timestamps backing the retry window.
    failures: std::sync::Mutex<HashMap<ClientKey, Instant>>,
    spawn_client: ClientFactory,
}

/// The workspace-wide language-server engine. Cheap to clone.
#[derive(Clone)]
pub struct Manager {
    inner: Arc<Inner>,
}

impl Manager {
    /// Engine bound to `root`, using the built-in catalog and real process
    /// spawning (PATH lookup + initialize handshake).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self::with_config(ManagerConfig::new(root))
    }

    /// Engine with explicit knobs (events channel, test backoff, …).
    pub fn with_config(config: ManagerConfig) -> Self {
        let factory: ClientFactory = Arc::new(|config| {
            Box::pin(async move {
                let client = LanguageServer::start(config)?;
                // Bound the handshake so a wedged server cannot hang the
                // caller forever; failure parks the key in backoff.
                tokio::time::timeout(
                    CLIENT_START_TIMEOUT,
                    client.initialize(CLIENT_START_TIMEOUT),
                )
                .await
                .map_err(|_| LspError::Spawn {
                    server: client.name().into(),
                    detail: format!("initialize timed out after {CLIENT_START_TIMEOUT:?}"),
                })??;
                Ok(client)
            })
        });
        Self::build(config, CATALOG.to_vec(), factory)
    }

    /// Catalog + spawn-factory injection. Production callers keep the default
    /// PATH-based factory ([`Manager::new`]/[`Self::with_config`]); tests and
    /// embedders inject their own transports here.
    pub fn build(
        config: ManagerConfig,
        catalog: Vec<ServerSpec>,
        spawn_client: ClientFactory,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                config,
                catalog,
                outcomes: std::sync::Mutex::new(HashMap::new()),
                failures: std::sync::Mutex::new(HashMap::new()),
                spawn_client,
            }),
        }
    }

    /// Workspace root this engine serves.
    pub fn root(&self) -> &Path {
        &self.inner.config.root
    }

    // ── Matching ─────────────────────────────────────────────────────────

    /// Resolve which `(key, spec-name)` pairs claim `path`, in catalog order,
    /// duplicates removed.
    pub fn matches_for_file(&self, path: &Path) -> Vec<(ClientKey, &'static str)> {
        let mut seen = std::collections::HashSet::new();
        self.inner
            .catalog
            .iter()
            .filter(|spec| spec.handles(path))
            .filter_map(|spec| {
                let root = resolve_project_root(path, spec, &self.inner.config.root)?;
                let key = ClientKey {
                    root,
                    server: spec.name.to_owned(),
                };
                seen.insert(key.clone()).then_some((key, spec.name))
            })
            .collect()
    }

    // ── Lifecycle ────────────────────────────────────────────────────────

    /// Ensure every applicable server for `path` is running; return handles.
    ///
    /// Soft errors ([`LspError::Unavailable`] for missing binaries/backoff,
    /// [`LspError::Spawn`] for handshake failures) never mask servers that did
    /// start; they only surface when *nothing* answered.
    pub async fn ensure_for_file(&self, path: &Path) -> Result<Vec<Arc<LanguageServer>>, LspError> {
        let mut handles = Vec::new();
        let mut soft_error: Option<LspError> = None;

        for (key, _spec_name) in self.matches_for_file(path) {
            match self.ensure_client(&key).await {
                Ok(client) => handles.push(client),
                Err(err @ (LspError::Unavailable(_) | LspError::Spawn { .. })) => {
                    soft_error.get_or_insert(err);
                }
                Err(err) => return Err(err),
            }
        }

        match (handles.is_empty(), soft_error) {
            (true, Some(err)) => Err(err),
            _ => Ok(handles),
        }
    }

    /// Currently-running clients claiming `path`, without spawning anything.
    pub fn clients_for_file(&self, path: &Path) -> Vec<Arc<LanguageServer>> {
        self.matches_for_file(path)
            .into_iter()
            .filter_map(|(key, _)| {
                let rx = self
                    .inner
                    .outcomes
                    .lock()
                    .expect("outcomes lock")
                    .get(&key)?
                    .clone();
                match rx.borrow().clone() {
                    SpawnOutcome::Ready(client) => Some(client),
                    _ => None,
                }
            })
            .collect()
    }

    /// Every currently-ready client, regardless of language.
    ///
    /// Workspace-level queries (symbol search) fan out over these instead of
    /// a per-file anchor.
    pub fn running_clients(&self) -> Vec<Arc<LanguageServer>> {
        self.inner
            .outcomes
            .lock()
            .expect("outcomes lock")
            .values()
            .filter_map(|rx| match rx.borrow().clone() {
                SpawnOutcome::Ready(client) => Some(client),
                _ => None,
            })
            .collect()
    }

    /// Snapshot of every tracked key's lifecycle state.
    pub fn states(&self) -> Vec<(ClientKey, ClientLifecycle)> {
        self.inner
            .outcomes
            .lock()
            .expect("outcomes lock")
            .iter()
            .map(|(key, rx)| {
                let lifecycle = match rx.borrow().clone() {
                    SpawnOutcome::Pending => ClientLifecycle::Starting,
                    SpawnOutcome::Ready(_) => ClientLifecycle::Ready,
                    SpawnOutcome::Failed { .. } => ClientLifecycle::Failed,
                };
                (key.clone(), lifecycle)
            })
            .collect()
    }

    /// Gracefully stop every ready client (session end). In-flight drivers
    /// re-check the map before publishing and discard clients that lost their
    /// slot, so stopping cannot leak freshly built processes.
    pub async fn stop_all(&self) {
        let clients: Vec<Arc<LanguageServer>> = {
            let mut outcomes = self.inner.outcomes.lock().expect("outcomes lock");
            outcomes
                .drain()
                .filter_map(|(_, rx)| match rx.borrow().clone() {
                    SpawnOutcome::Ready(client) => Some(client),
                    _ => None,
                })
                .collect()
        };
        self.inner.failures.lock().expect("failures lock").clear();

        for client in clients {
            if let Err(err) = client.shutdown().await {
                log::warn!("shutdown of `{}` failed: {err}", client.name());
            }
        }
    }

    /// Gracefully stop ONE client and forget its key, leaving siblings alone.
    ///
    /// This is the primitive behind the `lsp_restart` tool: the next touch of
    /// a matching file respawns the server from scratch.
    pub async fn stop_client(&self, key: &ClientKey) {
        let client = {
            let mut outcomes = self.inner.outcomes.lock().expect("outcomes lock");
            outcomes
                .remove(key)
                .and_then(|rx| match rx.borrow().clone() {
                    SpawnOutcome::Ready(client) => Some(client),
                    _ => None,
                })
        };
        self.inner
            .failures
            .lock()
            .expect("failures lock")
            .remove(key);

        if let Some(client) = client
            && let Err(err) = client.shutdown().await
        {
            log::warn!("shutdown of `{}` failed: {err}", client.name());
        }
    }

    // ── Internals ────────────────────────────────────────────────────────

    /// Get-or-spawn one client, sharing in-flight spawns between callers.
    ///
    /// Lock discipline: decision AND placeholder publication happen inside one
    /// critical section (`tokio::spawn` is synchronous, nothing here awaits).
    /// Splitting them would open a TOCTOU window where two tasks both decide
    /// `Start` and the second silently replaces the first's placeholder — two
    /// drivers, one untracked process.
    async fn ensure_client(&self, key: &ClientKey) -> Result<Arc<LanguageServer>, LspError> {
        enum Action {
            Ready(Arc<LanguageServer>),
            Wait(watch::Receiver<SpawnOutcome>),
            Started(watch::Receiver<SpawnOutcome>),
            Unavailable,
        }

        let action = {
            let mut outcomes = self.inner.outcomes.lock().expect("outcomes lock");
            match outcomes.get(key).map(|rx| rx.borrow().clone()) {
                Some(SpawnOutcome::Ready(client)) => Action::Ready(client),
                Some(SpawnOutcome::Pending) => {
                    Action::Wait(outcomes.get(key).expect("just matched").clone())
                }
                Some(SpawnOutcome::Failed { .. }) if self.backoff_remaining(key).is_some() => {
                    Action::Unavailable
                }
                // No entry at all, or a parked failure whose backoff elapsed:
                // publish a fresh placeholder atomically with that decision.
                _ => {
                    let Some(spec) = self
                        .inner
                        .catalog
                        .iter()
                        .find(|spec| spec.name == key.server)
                        .copied()
                    else {
                        return Err(LspError::InvalidState(
                            "spawn started for non-catalog server",
                        ));
                    };

                    let (outcome_tx, outcome_rx) = watch::channel(SpawnOutcome::Pending);
                    outcomes.insert(key.clone(), outcome_rx.clone());

                    let state = Arc::downgrade(&self.inner);
                    let driver_key = key.clone();
                    // Detached on purpose: completion belongs to the task, not
                    // to the caller's future, so caller cancellation cannot
                    // strand the placeholder. The task holds Weak state plus
                    // its own sender — nothing that pins the manager.
                    tokio::spawn(async move {
                        let outcome = drive_spawn(&state, &driver_key, spec).await;
                        let _ = outcome_tx.send(outcome);
                    });

                    Action::Started(outcome_rx)
                }
            }
        };

        match action {
            Action::Ready(client) => Ok(client),
            Action::Wait(rx) | Action::Started(rx) => wait_for_outcome(self, key, rx).await,
            Action::Unavailable => Err(LspError::Unavailable(key.server.clone().into_boxed_str())),
        }
    }

    fn backoff_remaining(&self, key: &ClientKey) -> Option<Duration> {
        let failures = self.inner.failures.lock().expect("failures lock");
        let at = *failures.get(key)?;
        let elapsed = at.elapsed();
        let backoff = self.inner.config.spawn_backoff;
        (elapsed < backoff).then(|| backoff - elapsed)
    }
}

/// Await a shared spawn outcome owned by someone else's driver task.
///
/// If the driver dies without publishing (factory panic), the channel closes;
/// a still-`Pending` entry is then removed so the next touch can start fresh
/// instead of waiting on a dead placeholder forever.
async fn wait_for_outcome(
    manager: &Manager,
    key: &ClientKey,
    mut outcome_rx: watch::Receiver<SpawnOutcome>,
) -> Result<Arc<LanguageServer>, LspError> {
    loop {
        if outcome_rx.changed().await.is_err() {
            let outcomes = manager.inner.outcomes.lock().expect("outcomes lock");
            let stuck = matches!(
                outcomes.get(key),
                Some(rx) if matches!(rx.borrow().clone(), SpawnOutcome::Pending)
            );
            if stuck {
                drop(outcomes);
                manager
                    .inner
                    .outcomes
                    .lock()
                    .expect("outcomes lock")
                    .remove(key);
            }
            return Err(LspError::NotRunning);
        }
        match outcome_rx.borrow_and_update().clone() {
            SpawnOutcome::Pending => continue,
            SpawnOutcome::Ready(client) => return Ok(client),
            SpawnOutcome::Failed {
                detail,
                missing_binary,
            } => {
                if missing_binary {
                    return Err(LspError::Unavailable(key.server.clone().into_boxed_str()));
                }
                return Err(LspError::Spawn {
                    server: key.server.clone().into_boxed_str(),
                    detail,
                });
            }
        }
    }
}

/// Resolve + initialize one client, then publish into the map.
///
/// Publication rules (all under the outcomes lock, decided against the
/// placeholder we created):
/// * Still ours (`Pending`) and success → store a terminal `Ready` channel.
/// * Placeholder gone (`stop_all` drained) → drop the fresh client;
///   `kill_on_drop` ends the process.
/// * Failure → record the backoff timestamp and remove the placeholder so the
///   next touch after the window starts clean.
///
/// The returned outcome reaches parked waiters through the driver's own
/// sender in `ensure_client`.
async fn drive_spawn(inner: &Weak<Inner>, key: &ClientKey, spec: ServerSpec) -> SpawnOutcome {
    // Binary presence checked before anything expensive when this manager
    // owns real spawning; missing binaries are persistent and deserve their
    // own soft error kind. Test factories skip straight to their transports.
    let resolves_binaries = inner
        .upgrade()
        .as_ref()
        .map(|inner| inner.config.resolves_binaries);
    let binary = match resolves_binaries {
        Some(true) => lookup_on_path(spec.command),
        // Sentinel path: injected factories never read it.
        _ => Some(PathBuf::from(format!("/{}/{}", key.server, spec.command))),
    };

    let outcome = match binary {
        // Missing binary: published below WITHOUT a backoff timestamp — the
        // next touch re-runs the (cheap) PATH lookup, so installing the
        // binary recovers immediately instead of staying parked.
        None => failed("not found on PATH", true),
        Some(binary) => {
            let Some(inner) = inner.upgrade() else {
                // Manager dropped mid-spawn; nothing left to publish into.
                return failed("manager shut down", false);
            };

            let (events_tx, events_rx) = mpsc::unbounded_channel();
            if let Some(managed) = &inner.config.events {
                spawn_event_relay(events_rx, managed.clone(), key.clone());
            }

            let mut config = LanguageServerConfig::new(spec.name, binary, key.root.clone());
            config.args = spec.args.iter().map(|arg| (*arg).to_owned()).collect();
            config.events = Some(events_tx);

            match (inner.spawn_client)(config).await {
                Ok(client) => SpawnOutcome::Ready(Arc::new(client)),
                Err(err) => failed(&format!("{err:#}"), false),
            }
        }
    };

    if let Some(inner) = inner.upgrade() {
        let mut outcomes = inner.outcomes.lock().expect("outcomes lock");
        let still_ours = matches!(
            outcomes.get(key),
            Some(rx) if matches!(rx.borrow().clone(), SpawnOutcome::Pending)
        );
        match &outcome {
            SpawnOutcome::Ready(client) if still_ours => {
                let (tx, rx) = watch::channel(outcome.clone());
                outcomes.insert(key.clone(), rx);
                drop(tx); // terminal value stored; readers see Ready forever
                // Evict on death so the next touch respawns naturally.
                spawn_exit_evictor(&inner, key, client);
            }
            SpawnOutcome::Ready(_) => {
                // Lost the race against stop_all: discard quietly.
                log::info!(
                    "discarding freshly started `{}` — manager stopped meanwhile",
                    spec.name
                );
            }
            SpawnOutcome::Failed {
                missing_binary: true,
                ..
            } if still_ours => {
                // No backoff for absent binaries: forget the placeholder so
                // the next touch re-runs the PATH lookup.
                outcomes.remove(key);
            }
            SpawnOutcome::Failed { .. } if still_ours => {
                // Keep the terminal Failed value parked in the map: retries
                // consult the backoff timestamp recorded here and replace the
                // entry once the window elapses.
                inner
                    .failures
                    .lock()
                    .expect("failures lock")
                    .insert(key.clone(), Instant::now());
            }
            SpawnOutcome::Failed { .. } => {}
            SpawnOutcome::Pending => unreachable!("drive_spawn never produces Pending"),
        }
    }

    outcome
}

fn failed(detail: &str, missing_binary: bool) -> SpawnOutcome {
    SpawnOutcome::Failed {
        detail: detail.to_owned(),
        missing_binary,
    }
}

/// Remove a client's entry once its process dies, so the next touch spawns a
/// fresh one. Weak-held: cannot outlive or pin the manager.
///
/// Identity-checked: the map entry is only removed if it still points at THIS
/// exact client (`Arc::ptr_eq`) — without that, a slow evictor could evict a
/// healthy respawn that replaced the dead one in the meantime.
fn spawn_exit_evictor(inner: &Arc<Inner>, key: &ClientKey, client: &Arc<LanguageServer>) {
    let mut state_rx = client.subscribe_state();
    let inner = Arc::downgrade(inner);
    let key = key.clone();
    let this_client = Arc::downgrade(client);
    tokio::spawn(async move {
        while state_rx.changed().await.is_ok() {
            let terminal = matches!(
                state_rx.borrow().clone(),
                ServerState::Exited { .. } | ServerState::Stopped
            );
            if !terminal {
                continue;
            }
            if let Some(inner) = inner.upgrade() {
                let mut outcomes = inner.outcomes.lock().expect("outcomes lock");
                // Only evict if the map still points at THIS instance; a
                // respawn may have replaced it already. The dead client's
                // Arc is still alive here because the map entry holds one.
                let this_client = this_client.upgrade();
                let still_this = this_client.is_some_and(|this_client| {
                    matches!(
                        outcomes.get(&key),
                        Some(rx) if matches!(&rx.borrow().clone(), SpawnOutcome::Ready(current) if Arc::ptr_eq(current, &this_client))
                    )
                });
                if still_this {
                    outcomes.remove(&key);
                    log::info!(
                        "evicted dead client `{}` at {}",
                        key.server,
                        key.root.display()
                    );
                }
            }
            return;
        }
    });
}

/// Forward raw client events to the owner, tagging them with identity.
///
/// Plain channels only — holding client state here would reintroduce the
/// teardown cycle the Weak design removed.
fn spawn_event_relay(
    mut events_rx: mpsc::UnboundedReceiver<Event>,
    managed_tx: mpsc::UnboundedSender<ManagedEvent>,
    key: ClientKey,
) {
    tokio::spawn(async move {
        while let Some(event) = events_rx.recv().await {
            let tagged = ManagedEvent {
                server: key.server.clone(),
                root: key.root.clone(),
                event,
            };
            if managed_tx.send(tagged).is_err() {
                break;
            }
        }
    });
}

/// Walk up from `path`'s directory to `workspace_root` looking for any of the
/// spec's markers; the first directory containing one wins. Marker-less specs
/// always resolve to the workspace root; specs whose markers appear nowhere in
/// the ancestry resolve to `None` (the spec does not apply here).
///
/// The walk is purely lexical (no symlink/canonicalization): a file reached
/// through a link pointing outside the workspace simply finds no markers and
/// is skipped — the safe direction. The *returned* root is canonicalized
/// (see [`canonical_root`]) so the key is stable across lexical aliases; a
/// symlinked project directory may therefore surface as its real path.
fn resolve_project_root(path: &Path, spec: &ServerSpec, workspace_root: &Path) -> Option<PathBuf> {
    if spec.root_markers.is_empty() {
        return Some(canonical_root(workspace_root));
    }

    let file_abs = absolute(path);
    let root_abs = absolute(workspace_root);

    let mut current = file_abs.parent();
    while let Some(dir) = current {
        if !dir.starts_with(&root_abs) {
            break; // left the workspace
        }
        if spec
            .root_markers
            .iter()
            .any(|marker| dir.join(marker).exists())
        {
            return Some(canonical_root(dir));
        }
        current = dir.parent();
    }
    None
}

/// Best-effort canonicalization of a resolved project root.
///
/// Without it, the same directory reached through different lexical paths
/// (a symlink into the tree, a non-canonical workspace root) produces
/// distinct `ClientKey`s — and thus two processes of the same server for one
/// project. When the path cannot be canonicalized (it may not exist yet),
/// the lexical path is kept: distinct-but-equal keys degrade to a redundant
/// server, never to a missing one.
fn canonical_root(path: &Path) -> PathBuf {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    strip_windows_verbatim(&canonical)
}

/// Windows `canonicalize` returns verbatim (`\\?\C:\…`) paths. Servers and
/// `uri_from_path` expect plain drive paths, so strip the prefix — keeping
/// the `\\?\UNC\` form mapped back to its `\\server\share` spelling.
#[cfg(windows)]
fn strip_windows_verbatim(path: &Path) -> PathBuf {
    let text = path.as_os_str().to_string_lossy();
    if let Some(stripped) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{stripped}"))
    } else if let Some(stripped) = text.strip_prefix(r"\\?\") {
        PathBuf::from(stripped)
    } else {
        path.to_path_buf()
    }
}

#[cfg(not(windows))]
fn strip_windows_verbatim(path: &Path) -> PathBuf {
    path.to_path_buf()
}

fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}
