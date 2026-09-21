use schemars::JsonSchema;
use serde::Deserialize;

/// Parameters for `bash_run`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct BashRunInput {
    /// The bash command to execute. Must not be an absolute path
    /// and must not match dangerous security patterns.
    pub command: String,
    /// Optional per-call timeout override in milliseconds. May only RAISE
    /// the harness-configured timeout: it must be an integer strictly
    /// greater than the default (interpolated into the tool description);
    /// values equal to or below the default are rejected. It never
    /// changes the configured default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}
