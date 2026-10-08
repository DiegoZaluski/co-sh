use schemars::JsonSchema;
use serde::Deserialize;

/// Parameters for `bash_run`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct BashRunInput {
    /// The bash command to execute. Must not be an absolute path
    /// and must not match dangerous security patterns.
    pub command: String,
}
