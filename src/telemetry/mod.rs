//! Telemetry for cosh — privacy-first, default-on (opt-out), server-economical.
//!
//! Design lives in `docs/telemetry-plan.md`. The rules that matter here:
//!
//! 1. **Default-on, opt-out** (industry standard): telemetry ships enabled;
//!    the user can disable it in Settings or via `COSH_TELEMETRY=off`, and
//!    CI environments are hard-off regardless (checked via
//!    [`Telemetry::enabled`]).
//! 2. **Allowlist boundary**: only closed enums from `schema` and normalizers
//!    from `sanitize` may enter an event. No free-form strings cross.
//! 3. **Single exit point**: everything is enqueued through [`Telemetry::enqueue`]
//!    which runs the final [`sanitize`] guard; a matching forbidden pattern
//!    drops the payload (fail-closed).
//! 4. **Server economy**: discrete events only for rare/valuable moments
//!    (install, crash); usage is aggregated per session into one
//!    `session_summary` at exit.
//! 5. **Never blocks the TUI, never panics**: all fallible operations are
//!    best-effort and silent.

pub mod events;
pub mod queue;
pub mod sanitize;
pub mod schema;
pub mod session;
pub mod sink;

use crate::telemetry::events::EventEnvelope;
use crate::telemetry::queue::EventQueue;

/// Pure decision logic for consent precedence (unit-testable, no env access).
/// Order matters: CI hard-off > explicit off > explicit on > user config.
fn resolve_enabled(env_value: &str, ci: bool, consented_in_config: bool) -> bool {
    if ci {
        return false; // unattended environment: telemetry never sends
    }
    match env_value.trim().to_lowercase().as_str() {
        "off" | "0" | "false" => false,
        "on" | "1" | "true" => true,
        _ => consented_in_config,
    }
}

/// Global facade. Holds the resolved runtime state (consent + paths).
pub struct Telemetry {
    queue: EventQueue,
    enabled: bool,
}

impl Telemetry {
    /// Resolve the runtime state: consent flag from config/env, queue in the
    /// platform data dir.
    ///
    /// Consent precedence (plan §6):
    /// 1. `COSH_TELEMETRY=off` → disabled (off wins over everything, incl. on).
    /// 2. `CI` env set → disabled (hard-off in CI).
    /// 3. `COSH_TELEMETRY=on` → enabled.
    /// 4. Otherwise the persisted user config decides (default ON — opt-out;
    ///    users disable via Settings or `COSH_TELEMETRY=off`).
    pub fn resolve(consented_in_config: bool) -> Self {
        let env = std::env::var("COSH_TELEMETRY").unwrap_or_default();
        let ci = std::env::var("CI").is_ok();
        let enabled = resolve_enabled(&env, ci, consented_in_config);
        Self {
            queue: EventQueue::new(queue_dir()),
            enabled,
        }
    }

    /// Is telemetry currently enabled (consented and not force-disabled)?
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Queue reference for `/telemetry status|export|clear`.
    pub fn queue(&self) -> &EventQueue {
        &self.queue
    }

    /// Enqueue an event after the final sanitize guard. Silently drops the
    /// event when telemetry is disabled or the envelope fails validation
    /// (typed field checks + payload denylist — see [`EventEnvelope::validate`]).
    pub fn enqueue(&self, envelope: &EventEnvelope) -> bool {
        if !self.enabled {
            return false;
        }
        if !envelope.validate() {
            log::warn!(
                "telemetry: event {} dropped by the validation guard",
                envelope.type_name()
            );
            return false;
        }
        self.queue.push(envelope);
        true
    }

    /// CONSENT-AWARE flush: uploads queued events to
    /// the ingest endpoint only when telemetry is currently enabled. This is
    /// the ONLY supported way to transmit — the queue, the sender and the
    /// hardened client stay crate-internal, and consent is re-checked HERE,
    /// at call time, then sealed into an unforgeable `sink::Consent` token
    /// the sender requires. A queue that outlives its enabling decision can
    /// never be uploaded by accident. Every persisted event is re-validated
    /// by the sink before sending.
    pub async fn flush(&self, config: &sink::SinkConfig) -> sink::FlushOutcome {
        if !self.enabled {
            return sink::FlushOutcome::Idle;
        }
        sink::flush(
            &self.queue,
            config,
            &sink::client(),
            sink::Consent::granted(),
        )
        .await
    }
}

/// The persistent install id: a random UUIDv4 created on first use and
/// stored in the telemetry data dir (a LOCAL random identifier, never
/// derived from hostname/MAC/user — plan §4.1). `None` when entropy or the
/// filesystem fails: callers drop the event (fail closed).
pub fn install_id() -> Option<events::UuidId> {
    let dir = queue_dir();
    let path = dir.join("install_id");
    if let Ok(existing) = std::fs::read_to_string(&path)
        && let Some(id) = events::UuidId::validate(existing.trim())
    {
        return Some(id);
    }
    let id = events::UuidId::generate()?;
    std::fs::create_dir_all(&dir).ok()?;
    std::fs::write(&path, id.as_str()).ok()?;
    Some(id)
}

/// Directory holding the local queue file (platform data dir).
fn queue_dir() -> std::path::PathBuf {
    if let Some(dir) =
        directories::ProjectDirs::from("", "", "cosh").map(|d| d.data_dir().to_path_buf())
    {
        dir.join("telemetry")
    } else {
        std::env::temp_dir().join("cosh-telemetry")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::schema::EventType;

    // Test modules moved from the old `tests/` (integration) directory into
    // the crate (`src/telemetry/tests/`): the transport cases need
    // `pub(crate)` access, so all three live inside the crate now.
    mod telemetry_audit;
    mod telemetry_reaudit;
    mod transport_tests;

    fn envelope() -> EventEnvelope {
        events::synthetic(EventType::Install)
    }

    #[test]
    fn disabled_telemetry_never_enqueues() {
        let t = Telemetry {
            queue: EventQueue::new(std::env::temp_dir().join("cosh-tel-test-disabled")),
            enabled: false,
        };
        assert!(!t.enqueue(&envelope()));
        assert_eq!(t.queue().pending_count(), 0);
    }

    #[test]
    fn consent_precedence_off_wins_over_everything() {
        // off > CI > on > config (plan §6). Pure logic, no env mutation.
        assert!(!resolve_enabled("off", false, true));
        assert!(!resolve_enabled("off", true, true));
        assert!(!resolve_enabled("0", false, true));
        assert!(!resolve_enabled("false", false, true));
    }

    #[test]
    fn consent_precedence_ci_hard_off() {
        // CI kills telemetry even with explicit on + config consent: an
        // unattended environment never sends telemetry.
        assert!(!resolve_enabled("", true, true));
        assert!(!resolve_enabled("on", true, true));
        assert!(!resolve_enabled("1", true, true));
    }

    #[test]
    fn consent_precedence_on_overrides_config() {
        assert!(resolve_enabled("on", false, false));
        assert!(resolve_enabled("true", false, false));
        assert!(resolve_enabled(" ON ", false, false)); // trims + lowercases
    }

    #[test]
    fn consent_precedence_config_default_off() {
        assert!(!resolve_enabled("", false, false));
        assert!(resolve_enabled("", false, true));
        // Unknown env values fall through to the config decision.
        assert!(!resolve_enabled("banana", false, false));
        assert!(resolve_enabled("banana", false, true));
    }
}
