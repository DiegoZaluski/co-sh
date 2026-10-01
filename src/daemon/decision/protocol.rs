//! The decision daemon's protocol: methods, params, results, error codes.
//!
//! The v1 surface is exactly what has a consumer (see the approved schema):
//! `hello`, `decision/decide`, `health`, `shutdown`. `decide` mirrors
//! [`cosh_onnx::DecisionModel::decide`] with NO translation — `state` and
//! `questions` are opaque JSON in, the model's raw result object out. The
//! parameters the only consumer never passes (`lang`, `max_len`,
//! `head_max_len`) and the batch method with no caller are deliberately
//! absent; every future addition is an additive change (a new variant or a
//! `#[serde(default)]` field), so v1 clients keep working.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The daemon's name — the socket file stem and the activation key.
pub const NAME: &str = "decision";

/// Protocol version. Monotonic; a major bump means an incompatible change.
pub const PROTOCOL: u32 = 1;

/// Method names on the wire.
pub mod method {
    pub const HELLO: &str = "hello";
    pub const DECIDE: &str = "decision/decide";
    pub const HEALTH: &str = "health";
    pub const SHUTDOWN: &str = "shutdown";
}

/// Application error codes (JSON-RPC reserved codes live in `ipc::RpcError`).
///
/// Every one of them maps to the consumer's fail-open policy — the audit is
/// an improver, never a single point of failure.
pub mod code {
    /// A method arrived before `hello`, or the protocol version is
    /// incompatible. Client: fail open for the whole session.
    pub const HANDSHAKE: i32 = -32001;
    /// Unknown kind, or the model load failed. Client: fail open; later
    /// requests retry the load (the daemon throttles retries itself).
    pub const MODEL: i32 = -32002;
    /// The inference semaphore was not acquired within the window.
    /// Client: one short-backoff retry, then fail open.
    pub const BUSY: i32 = -32003;
    /// The model ran and failed (ORT error, malformed checkpoint output).
    pub const INFERENCE: i32 = -32004;
    /// The daemon is draining for shutdown. Client: fail open; the next
    /// connection re-activates a fresh daemon.
    pub const DRAINING: i32 = -32005;
}

/// Which checkpoint to decide with.
///
/// THREE wire shapes, one type:
///
/// 1. **Flat string** (the approved schema's `decide` shape): `"english"`,
///    `"multilingual"`, `"typed-decisions"`.
/// 2. **Custom object**: `{"custom": {"repo": "...", "subfolder": null}}`.
/// 3. **The config shape** (`cosh::setup::DecisionModel`, tagged by
///    `kind`, snake case): `{"kind": "custom", "repo": ..., "subfolder":
///    ...}` — accepted on input so the harness converts config → wire by
///    serde alone, with no translation map and no TUI dependency.
///
/// Serialization emits shapes 1/2 (the flat wire form); deserialization
/// accepts all three.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// The English root checkpoint (`english`).
    English,
    /// The multilingual checkpoint (`multilingual`).
    Multilingual,
    /// The typed-decisions checkpoint (`typed-decisions`).
    TypedDecisions,
    /// A raw hub repo id, a bundled `(repo, subfolder)` pair, or a local
    /// checkpoint directory.
    Custom {
        /// The repo id or local path.
        repo: String,
        /// One checkpoint out of a bundling repo.
        subfolder: Option<String>,
    },
}

impl Kind {}

/// The untagged wire encoding (shapes 1 and 2).
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
enum KindWire {
    /// A named checkpoint, flat.
    Named(String),
    /// A custom checkpoint, discriminated by the `custom` key.
    Custom { custom: CustomShape },
    /// The config shape (shape 3), input-only.
    Tagged {
        kind: String,
        #[serde(default, skip_serializing)]
        repo: Option<String>,
        #[serde(default, skip_serializing)]
        subfolder: Option<String>,
    },
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CustomShape {
    repo: String,
    #[serde(default)]
    subfolder: Option<String>,
}

impl serde::Serialize for Kind {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            // Shape 1: flat named string.
            Self::English => serializer.serialize_str("english"),
            Self::Multilingual => serializer.serialize_str("multilingual"),
            Self::TypedDecisions => serializer.serialize_str("typed-decisions"),
            // Shape 2: the custom object.
            Self::Custom { repo, subfolder } => KindWire::Custom {
                custom: CustomShape {
                    repo: repo.clone(),
                    subfolder: subfolder.clone(),
                },
            }
            .serialize(serializer),
        }
    }
}

impl<'de> serde::Deserialize<'de> for Kind {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = KindWire::deserialize(deserializer)?;
        Ok(match wire {
            KindWire::Named(name) => match name.as_str() {
                "english" => Self::English,
                "multilingual" => Self::Multilingual,
                "typed-decisions" => Self::TypedDecisions,
                other => {
                    return Err(serde::de::Error::unknown_variant(
                        other,
                        &["english", "multilingual", "typed-decisions"],
                    ));
                }
            },
            KindWire::Custom { custom } => Self::Custom {
                repo: custom.repo,
                subfolder: custom.subfolder,
            },
            // Shape 3: the config shape, `kind`-tagged. A named variant is
            // the tag itself; a custom carries repo/subfolder alongside.
            KindWire::Tagged {
                kind,
                repo,
                subfolder,
            } => match kind.as_str() {
                "english" => Self::English,
                "multilingual" => Self::Multilingual,
                "typed-decisions" => Self::TypedDecisions,
                "custom" => Self::Custom {
                    repo: repo.ok_or_else(|| serde::de::Error::missing_field("repo"))?,
                    subfolder,
                },
                other => {
                    return Err(serde::de::Error::unknown_variant(
                        other,
                        &["english", "multilingual", "typed-decisions", "custom"],
                    ));
                }
            },
        })
    }
}

impl Kind {
    /// The canonical name the model registry keys residents by (the same
    /// names `cosh_onnx::ModelKind::name()` publishes for the variants).
    pub fn name(&self) -> String {
        match self {
            Self::English => "english".to_owned(),
            Self::Multilingual => "multilingual".to_owned(),
            Self::TypedDecisions => "typed-decisions".to_owned(),
            Self::Custom { repo, subfolder } => match subfolder {
                Some(s) => format!("{repo}/{s}"),
                None => repo.clone(),
            },
        }
    }
}

/// `hello` params: the first message on every connection.
#[derive(Debug, Serialize, Deserialize)]
pub struct HelloParams {
    /// The client's protocol version.
    pub protocol: u32,
    /// The connecting process — diagnostics only.
    pub pid: u32,
}

/// `hello` result.
#[derive(Debug, Serialize, Deserialize)]
pub struct HelloResult {
    /// The daemon's protocol version; a major mismatch makes the client
    /// fail open for the session.
    pub protocol: u32,
    /// Always [`NAME`] — guards against a foreign daemon on our socket.
    pub daemon: String,
    /// The daemon binary's version.
    pub version: String,
}

/// `decision/decide` params — [`cosh_onnx::DecisionModel::decide`] verbatim.
#[derive(Debug, Serialize, Deserialize)]
pub struct DecideParams {
    /// Which checkpoint to run.
    pub kind: Kind,
    /// The decision state (opaque to the protocol).
    pub state: Value,
    /// The question schema (opaque to the protocol).
    pub questions: Map<String, Value>,
}

/// `decision/decide` result: the model's raw result object (`answers`,
/// `usage`), uninterpreted here — the semantics live with the consumer.
#[derive(Debug, Serialize, Deserialize)]
pub struct DecideResult {
    /// The full result object, unchanged.
    pub result: Value,
}

/// `health` result — the observability that proves the sharing is real
/// (one resident, many consumers).
#[derive(Debug, Serialize, Deserialize)]
pub struct HealthResult {
    /// Seconds since daemon start.
    pub uptime_secs: u64,
    /// The checkpoint currently resident, by name; `None` when nothing is
    /// loaded yet.
    pub active_kind: Option<String>,
    /// Decides served since start.
    pub requests_served: u64,
    /// Failed loads + failed inferences since start.
    pub failed: u64,
}

/// `shutdown` result: the daemon accepted the drain.
#[derive(Debug, Serialize, Deserialize)]
pub struct ShutdownResult {
    /// Always true on the success path.
    pub draining: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shape 3: the config shape (`DecisionModel`, tagged by `kind`) must
    /// deserialize — the harness converts config → wire by serde alone.
    #[test]
    fn kind_parses_the_config_shape() {
        let config_value = serde_json::json!({
            "kind": "custom", "repo": "~/models/laya", "subfolder": null
        });
        let kind: Kind = serde_json::from_value(config_value).unwrap();
        assert_eq!(
            kind,
            Kind::Custom {
                repo: "~/models/laya".into(),
                subfolder: None
            }
        );
        let named: Kind = serde_json::from_value(serde_json::json!({ "kind": "multilingual" }))
            .expect("a config-shaped named kind");
        assert_eq!(named, Kind::Multilingual);
    }

    /// Shape 1 (the approved `decide` wire shape): a flat string — and
    /// serializing ALWAYS emits it for named kinds, never the config tag.
    #[test]
    fn named_kinds_wire_as_flat_strings() {
        assert_eq!(
            serde_json::to_value(Kind::Multilingual).unwrap(),
            serde_json::json!("multilingual")
        );
        assert_eq!(
            serde_json::to_value(Kind::English).unwrap(),
            serde_json::json!("english")
        );
        let parsed: Kind = serde_json::from_value(serde_json::json!("english")).unwrap();
        assert_eq!(parsed, Kind::English);
    }

    /// Shape 2: a custom checkpoint wires as the `custom`-keyed object.
    #[test]
    fn custom_kind_wires_as_the_custom_object() {
        let kind = Kind::Custom {
            repo: "~/models/laya".into(),
            subfolder: Some("typed-decisions".into()),
        };
        let value = serde_json::to_value(&kind).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "custom": {"repo": "~/models/laya", "subfolder": "typed-decisions"}
            })
        );
        let parsed: Kind = serde_json::from_value(value).unwrap();
        assert_eq!(parsed, kind);
    }

    /// A wire kind no one publishes must fail the params parse (the
    /// responder turns it into `-32602`), not silently become another kind.
    #[test]
    fn unknown_named_kind_is_a_parse_error() {
        let parsed: Result<Kind, _> = serde_json::from_value(serde_json::json!("trilingual"));
        assert!(parsed.is_err());
    }

    #[test]
    fn custom_kind_with_subfolder_names_repo_slash_subfolder() {
        let kind = Kind::Custom {
            repo: "hub/repo".into(),
            subfolder: Some("multilingual".into()),
        };
        assert_eq!(kind.name(), "hub/repo/multilingual");
    }
}
