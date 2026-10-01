//! The weights installer's application side: what to download, the
//! hardcoded explanation streamed into the chat, and the install run with
//! progress events.
//!
//! Lives in the BINARY (not the lib): it depends on the setup config
//! (`cosh::setup`), the TUI event channel and the `onnx` feature.
//! The mirror repo id lives HERE via setup.json (`model_decision.hub`) —
//! the `cosh-onnx` crate receives it as a parameter and carries no
//! default.
//!
//! The install itself is `cosh_onnx::hub::install`: cache-first, the
//! built-in progress bar disabled, `sha256sums.txt` verified before
//! success. This module only translates config → install request →
//! `HarnessEvent::WeightsInstall` events.

use std::sync::{Arc, Mutex};

use cosh::harness::events::{HarnessEvent, WeightsInstallEvent};
#[cfg(feature = "onnx")]
use cosh::setup::{DecisionHub, DecisionModel};

/// The graph candidates, quantized first (the mirror may not ship the
/// int8 twin yet — the installer falls back to fp32).
const GRAPHS: &[&str] = &["laya.int8.onnx", "laya.onnx"];

/// The explanation streamed into the chat BEFORE the download starts,
/// built around the model's display name. It enters the agent context as a
/// real assistant message, so the model can explain or translate it to the
/// user. Written as prose (not code-generated text): the model is
/// introduced as a general-purpose local classifier — the termination
/// audit is the capability shipping TODAY, not the model's definition —
/// because the same weights may classify preferences or other signals
/// tomorrow.
pub(crate) fn explanation(display_name: &str) -> String {
    format!(
        "I'm fetching {name} — a small classifier that runs entirely on your machine.\
\n\n\
cosh ships a family of local models for judgment calls the main agent \
shouldn't spend tokens on: classifying user preferences, scoring options, \
reading intent from a message. Right now I'm wiring it up as a second \
opinion on my own decisions — when I'm about to end a turn on an \
ambiguous signal, it reviews that call and pushes back if it isn't \
confident. It never takes part in this conversation itself.\
\n\n\
{name} ships as a quantized ONNX graph and downloads once; the progress \
bar below tracks it. After that it works silently on every turn.",
        name = display_name
    )
}

/// One install target resolved from the setup config: which repo, which
/// checkpoint inside it, which revision pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WeightsTarget {
    /// The mirror repo id (the effective one: env override applied).
    pub repo: String,
    /// The checkpoint's subfolder inside the mirror (`None` = repo root).
    pub subfolder: Option<String>,
    /// The revision pin. `Some` when the config pins one; `None` resolves
    /// the mirror's current revision on the first install — the snapshot
    /// stays cached, so later sessions reuse it without re-resolving.
    pub revision: Option<String>,
    /// The model's user-facing name (upstream family + checkpoint, or the
    /// repo id) for the install copy: the streamed explanation and the
    /// progress line both address the model by it.
    pub display_name: String,
}

impl WeightsTarget {
    /// The wire kind the decision daemon loads through: a `Custom` kind
    /// addressing the mirror checkpoint directly. Building it HERE (not by
    /// serde round-trip of the configured kind) is the whole point of the
    /// mirror indirection: the daemon's loader cache-probes the mirror
    /// snapshot the installer filled, and never touches the upstream repos.
    pub fn to_kind(&self) -> cosh::daemon::decision::Kind {
        cosh::daemon::decision::Kind::Custom {
            repo: self.repo.clone(),
            subfolder: self.subfolder.clone(),
        }
    }
}

/// Resolve the install target from the setup config. `None` = nothing to
/// install: a `Custom` kind pointing at a LOCAL directory loads directly
/// (no mirror, no download).
///
/// The revision pin is scoped to the MIRROR: `pinned_revision` records the
/// SHA the mirror resolved to on its first download, so it applies only
/// when the target's repo IS the effective mirror. A `Custom` kind
/// pointing at a different hub repo never carries it — a stale mirror SHA
/// would 404 against that repo and permanently disable the audit.
#[cfg(feature = "onnx")]
pub(crate) fn resolve_target(model: &DecisionModel, hub: &DecisionHub) -> Option<WeightsTarget> {
    let repo = hub.effective_repo();
    // The pin is repo-scoped (DecisionHub::pin_for): it applies only when
    // the target's repo is the one the SHA was resolved from.
    let mirror_pin = hub.pin_for(&repo);
    let display_name = model.display_name().to_string();
    match model {
        // The named kinds map onto the mirror's per-kind subfolders. The
        // mirror nests every kind (including `english/`, unlike the
        // upstream bundle where the English root sits at the repo root).
        DecisionModel::English => Some(WeightsTarget {
            repo: repo.clone(),
            subfolder: Some("english".to_string()),
            revision: mirror_pin,
            display_name,
        }),
        DecisionModel::Multilingual => Some(WeightsTarget {
            repo: repo.clone(),
            subfolder: Some("multilingual".to_string()),
            revision: mirror_pin,
            display_name,
        }),
        DecisionModel::TypedDecisions => Some(WeightsTarget {
            repo: repo.clone(),
            subfolder: Some("typed-decisions".to_string()),
            revision: mirror_pin,
            display_name,
        }),
        DecisionModel::Custom {
            repo: custom,
            subfolder,
        } => {
            if std::path::Path::new(custom).is_dir() {
                // Local checkpoint: the loader resolves it directly.
                return None;
            }
            // The pin travels ONLY with the mirror: a custom repo has (and
            // needs) its own revision story.
            let revision = (*custom == repo).then_some(mirror_pin).flatten();
            Some(WeightsTarget {
                repo: custom.clone(),
                subfolder: subfolder.clone(),
                revision,
                display_name,
            })
        }
    }
}

/// Is the target already fully cached (no download needed)?
#[cfg(feature = "onnx")]
pub(crate) fn is_cached(target: &WeightsTarget) -> bool {
    cosh_onnx::hub::cache_status(
        &target.repo,
        target.subfolder.as_deref(),
        GRAPHS,
        target.revision.as_deref(),
    )
    .is_some()
}

/// The cache-hit stage event: freezes any orphaned running install line in
/// the transcript (a crash mid-install persisted one; a cache-hit turn
/// emits NO other install event, so the stale bar would tick forever).
#[cfg(feature = "onnx")]
const fn cached_event() -> WeightsInstallEvent {
    WeightsInstallEvent::Cached
}

/// Install the target's checkpoint into the shared hf-hub cache, streaming
/// the explanation and the download progress as
/// [`HarnessEvent::WeightsInstall`] events. Cache hit → no events, no
/// network.
///
/// Returns the explanation text ONLY when a download actually ran — the
/// caller adds exactly that text to the agent context (after restoring
/// it), so the model never sees an explanation for a download that never
/// happened.
///
/// Async: the blocking install runs on the blocking thread pool (the
/// agent runtime must never block on a download). Fails with the error
/// message — the caller fails the turn's audit open, never the turn.
#[cfg(feature = "onnx")]
pub(crate) async fn ensure_installed(
    target: WeightsTarget,
    event_tx: tokio::sync::mpsc::UnboundedSender<HarnessEvent>,
) -> Result<Option<String>, String> {
    let send = |event: WeightsInstallEvent| {
        let _ = event_tx.send(HarnessEvent::WeightsInstall { event });
    };

    if is_cached(&target) {
        send(cached_event());
        return Ok(None);
    }

    // Stream the explanation the way an LLM emits tokens: one event per
    // word (with its trailing space), a few milliseconds between them —
    // `split_inclusive` keeps the pieces concatenating back into EXACTLY
    // the context text, and the event channel + TUI redraw absorb the
    // pacing. Runs only on the cold path: a cache hit never streams.
    let text = explanation(&target.display_name);
    let mut opened = false;
    for word in text.split_inclusive(char::is_whitespace) {
        let event = if opened {
            WeightsInstallEvent::TextDelta(word.to_string())
        } else {
            opened = true;
            WeightsInstallEvent::TextBegin(word.to_string())
        };
        send(event);
        tokio::time::sleep(std::time::Duration::from_millis(12)).await;
    }

    // Shared progress state. hf-hub calls `init(size, filename)` per file
    // (again on resume/retry — the receiver resets, not accumulates) and
    // `update(delta)` while bytes stream in. `update` carries no filename,
    // so the CURRENT file is tracked explicitly (the one init'd most
    // recently; downloads are strictly sequential here).
    let model_name = target.display_name.clone();
    let files: Arc<Mutex<Files>> = Arc::new(Mutex::new(Files::default()));
    let throttle: Arc<Mutex<Throttle>> = Arc::new(Mutex::new(Throttle::new()));
    let tx = event_tx.clone();

    let progress = {
        let files_init = Arc::clone(&files);
        let files_upd = Arc::clone(&files);
        let throttle = Arc::clone(&throttle);
        cosh_onnx::hub::ProgressCallbacks::new(
            move |size, filename| {
                if let Ok(mut f) = files_init.lock() {
                    f.start_file(filename.to_string(), size as u64);
                }
            },
            move |delta| {
                let (done, total) = {
                    let mut f = match files_upd.lock() {
                        Ok(f) => f,
                        Err(_) => return,
                    };
                    f.advance(delta as u64);
                    (f.done.min(f.total), f.total)
                };
                // Throttle: emit at most every 0.2% of progress or 250 ms,
                // so a 1 GB graph does not flood the event channel.
                let permille = if total == 0 {
                    0
                } else {
                    ((done * 1000) / total) as u32
                };
                if let Ok(mut last) = throttle.lock()
                    && (permille.saturating_sub(last.permille) >= 2
                        || last.at.elapsed() >= std::time::Duration::from_millis(250))
                {
                    last.permille = permille;
                    last.at = std::time::Instant::now();
                    let _ = tx.send(HarnessEvent::WeightsInstall {
                        event: WeightsInstallEvent::Progress {
                            model: model_name.clone(),
                            bytes_done: done,
                            bytes_total: total,
                        },
                    });
                }
            },
            move || {},
        )
    };

    let repo = target.repo.clone();
    let subfolder = target.subfolder.clone();
    let revision = target.revision.clone();
    let result = tokio::task::spawn_blocking(move || {
        cosh_onnx::hub::install(cosh_onnx::hub::InstallRequest {
            repo: &repo,
            subfolder: subfolder.as_deref(),
            revision: revision.as_deref(),
            graphs: GRAPHS,
            expected_sha256: None,
            progress,
        })
    })
    .await
    .unwrap_or_else(|join| {
        Err(cosh_onnx::Error::Runtime(format!(
            "cosh-onnx: install task panicked: {join}"
        )))
    });

    match result {
        Ok(installed) => {
            send(WeightsInstallEvent::Finished {
                repo: target.repo.clone(),
                revision: installed.revision.clone(),
            });
            Ok(Some(text))
        }
        Err(e) => {
            let reason = e.message().to_string();
            send(WeightsInstallEvent::Failed(reason.clone()));
            Err(reason)
        }
    }
}

/// Per-file download accounting: totals per repo path, the current file's
/// position, and the derived sums.
#[cfg(feature = "onnx")]
#[derive(Default)]
struct Files {
    /// `(total_bytes, downloaded_bytes)` per file, insertion-ordered (the
    /// current file is the last one `start_file` saw).
    table: std::collections::HashMap<String, (u64, u64)>,
    current: Option<String>,
    done: u64,
    total: u64,
}

#[cfg(feature = "onnx")]
impl Files {
    /// `init(size, filename)`: a file starts (or resumes). A NEW file adds
    /// its size to the running total; a RE-init (resume/retry) replaces the
    /// file's previous accounting — its old size and position leave the
    /// sums, the resumed window re-accumulates from zero.
    fn start_file(&mut self, name: String, size: u64) {
        match self.table.get_mut(&name) {
            Some(entry) => {
                let (prev_size, prev_pos) = *entry;
                self.done = self.done.saturating_sub(prev_pos);
                self.total = self.total.saturating_sub(prev_size) + size;
                *entry = (size, 0);
            }
            None => {
                self.table.insert(name.clone(), (size, 0));
                self.total += size;
            }
        }
        self.current = Some(name);
    }

    /// `update(delta)`: advance the current file.
    fn advance(&mut self, delta: u64) {
        if let Some(name) = self.current.clone()
            && let Some((_, pos)) = self.table.get_mut(&name)
        {
            *pos += delta;
            self.done += delta;
        }
    }
}

/// Progress-event throttle: at most one event per 0.2%-permille step or
/// 250 ms, whichever first.
#[cfg(feature = "onnx")]
struct Throttle {
    permille: u32,
    at: std::time::Instant,
}

#[cfg(feature = "onnx")]
impl Throttle {
    fn new() -> Self {
        Self {
            permille: u32::MAX,
            at: std::time::Instant::now() - std::time::Duration::from_secs(1),
        }
    }
}

#[cfg(all(test, feature = "onnx"))]
mod tests {
    use super::*;

    fn hub() -> DecisionHub {
        DecisionHub::default()
    }

    #[test]
    fn named_kinds_map_to_mirror_subfolders() {
        let h = hub();
        let t = resolve_target(&DecisionModel::English, &h).unwrap();
        assert_eq!(t.repo, "Zaluski/laya-onnx");
        assert_eq!(t.subfolder.as_deref(), Some("english"));
        assert!(t.revision.is_none());

        let t = resolve_target(&DecisionModel::Multilingual, &h).unwrap();
        assert_eq!(t.subfolder.as_deref(), Some("multilingual"));

        let t = resolve_target(&DecisionModel::TypedDecisions, &h).unwrap();
        assert_eq!(t.subfolder.as_deref(), Some("typed-decisions"));
    }

    #[test]
    fn local_custom_kind_needs_no_install() {
        let dir = std::env::temp_dir().join("cosh-weights-local-test");
        std::fs::create_dir_all(&dir).unwrap();
        let model = DecisionModel::Custom {
            repo: dir.to_string_lossy().into_owned(),
            subfolder: None,
        };
        assert!(resolve_target(&model, &hub()).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remote_custom_kind_installs_from_its_own_repo() {
        let model = DecisionModel::Custom {
            repo: "org/their-checkpoint".to_string(),
            subfolder: None,
        };
        let t = resolve_target(&model, &hub()).unwrap();
        assert_eq!(t.repo, "org/their-checkpoint");
        assert_eq!(t.subfolder, None);
    }

    #[test]
    fn config_pin_flows_into_the_target() {
        let mut h = hub();
        h.pinned_repo = h.effective_repo();
        h.pinned_revision = "abc123".to_string();
        let t = resolve_target(&DecisionModel::English, &h).unwrap();
        assert_eq!(t.revision.as_deref(), Some("abc123"));
    }

    #[test]
    fn a_foreign_pin_never_applies() {
        // A pin recorded from ANOTHER repo (or a legacy pre-scoping pin)
        // must not constrain the current mirror: the next install
        // re-resolves and re-records a properly scoped pin.
        let mut h = hub();
        h.pinned_revision = "abc123".to_string();
        assert_eq!(h.pin_for(&h.effective_repo()), None, "no pinned_repo");

        h.pinned_repo = "other/mirror".to_string();
        assert_eq!(h.pin_for(&h.effective_repo()), None, "different repo");
        assert_eq!(h.pin_for("other/mirror").as_deref(), Some("abc123"));

        let t = resolve_target(&DecisionModel::English, &h).unwrap();
        assert!(
            t.revision.is_none(),
            "foreign pin does not reach the target"
        );

        // A custom repo (even equal to the mirror) carries the pin only
        // when the pin belongs to that repo.
        h.pinned_repo = h.effective_repo();
        let t = resolve_target(
            &DecisionModel::Custom {
                repo: h.effective_repo(),
                subfolder: None,
            },
            &h,
        )
        .unwrap();
        assert_eq!(t.revision.as_deref(), Some("abc123"));
    }

    #[test]
    fn target_maps_to_a_custom_wire_kind() {
        let t = resolve_target(&DecisionModel::English, &hub()).unwrap();
        assert_eq!(t.to_kind().name(), "Zaluski/laya-onnx/english");
    }

    #[test]
    fn explanation_names_the_model_and_streams_back_intact() {
        // Each named kind is addressed by its full model name.
        for (model, name) in [
            (DecisionModel::English, "Laya"),
            (DecisionModel::Multilingual, "Laya-Multilingual"),
            (DecisionModel::TypedDecisions, "Laya-Typed-Decisions"),
        ] {
            let t = resolve_target(&model, &hub()).unwrap();
            assert_eq!(t.display_name, name);
            let text = explanation(&t.display_name);
            assert!(text.contains(name), "{name} missing from the copy");
        }

        // The word-streaming split must concatenate back into EXACTLY the
        // context text: the TUI appends every delta chunk, and the agent
        // context gets the unsplit original.
        let text = explanation("Laya");
        let joined: String = text.split_inclusive(char::is_whitespace).collect();
        assert_eq!(joined, text);
        // The first chunk is never empty (the TUI opens the message with it).
        let mut chunks = text.split_inclusive(char::is_whitespace);
        assert!(!chunks.next().unwrap().trim().is_empty());
    }

    #[test]
    fn file_accounting_survives_reinit_and_sequential_files() {
        let mut f = Files::default();
        f.start_file("a.json".into(), 100);
        f.advance(40);
        assert_eq!((f.done, f.total), (40, 100));
        // Resume: init resets the file's position without double-counting.
        f.start_file("a.json".into(), 100);
        f.advance(100);
        assert_eq!((f.done, f.total), (100, 100));
        // The next file starts after the first finished.
        f.start_file("graph.onnx".into(), 1_000_000);
        f.advance(500_000);
        assert_eq!((f.done, f.total), (500_100, 1_000_100));
    }
}
