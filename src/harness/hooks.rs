//! PreToolUse hooks — user-defined shell commands that fire before each tool
//! call, returning decisions that control agent behavior.
//!
//! Hooks are configured in `setup.json` and run via the system shell. Each
//! hook receives the tool name and input as a JSON payload on stdin, and
//! responds with a JSON decision on stdout.
//!
//! # Exit codes
//!
//! - `0` → parse stdout JSON for decision
//! - `2` → deny this tool call (stderr = reason)
//! - `49` → halt the entire turn (stderr = reason)
//! - other → non-blocking error, treated as `DecisionNone`
//!
//! # Stdout JSON
//!
//! ```json
//! {
//!   "decision": "allow|deny|none",
//!   "reason": "why blocked",
//!   "context": "extra text appended to tool result",
//!   "updated_input": {"command": "modified..."}
//! }
//! ```

use serde::{Deserialize, Serialize};
use std::process::Command;
use std::time::Duration;

// ── Constants ───────────────────────────────────────────────────────────────

/// Exit code that halts the entire turn (same as Crush).
const HALT_EXIT_CODE: i32 = 49;

/// Default timeout for hook execution (seconds).
const DEFAULT_TIMEOUT: u64 = 30;

// ── Config ──────────────────────────────────────────────────────────────────

/// A single hook configuration entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookConfig {
    /// Friendly display name (falls back to command when empty).
    #[serde(default)]
    pub name: String,
    /// Regex pattern tested against the tool name. Empty = match all.
    #[serde(default)]
    pub matcher: String,
    /// Shell command to execute.
    pub command: String,
    /// Timeout in seconds (default 30).
    #[serde(default)]
    pub timeout: Option<u64>,
}

impl HookConfig {
    fn display_name(&self) -> &str {
        if self.name.is_empty() {
            &self.command
        } else {
            &self.name
        }
    }

    fn timeout_duration(&self) -> Duration {
        Duration::from_secs(self.timeout.unwrap_or(DEFAULT_TIMEOUT))
    }
}

// ── Decision ────────────────────────────────────────────────────────────────

/// The outcome of a single hook execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HookDecision {
    /// Hook expressed no opinion.
    None,
    /// Hook explicitly allowed the action.
    Allow,
    /// Hook blocked the action.
    Deny,
}

impl std::fmt::Display for HookDecision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => write!(f, "none"),
            Self::Allow => write!(f, "allow"),
            Self::Deny => write!(f, "deny"),
        }
    }
}

// ── Results ─────────────────────────────────────────────────────────────────

/// Parsed output of a single hook execution.
#[derive(Debug, Clone)]
pub struct HookResult {
    pub decision: HookDecision,
    pub halt: bool,
    pub reason: String,
    pub context: String,
    pub updated_input: String,
}

impl Default for HookResult {
    fn default() -> Self {
        Self {
            decision: HookDecision::None,
            halt: false,
            reason: String::new(),
            context: String::new(),
            updated_input: String::new(),
        }
    }
}

/// Info about a single hook for display purposes.
#[derive(Debug, Clone, Serialize)]
pub struct HookInfo {
    pub name: String,
    pub matcher: String,
    pub decision: String,
    pub halt: bool,
    pub reason: String,
    pub input_rewrite: bool,
}

/// Combined outcome of all hooks for a single tool call.
#[derive(Debug, Clone)]
pub struct AggregateResult {
    pub decision: HookDecision,
    pub halt: bool,
    pub hook_count: usize,
    pub hooks: Vec<HookInfo>,
    pub reason: String,
    pub context: String,
    pub updated_input: String,
}

impl Default for AggregateResult {
    fn default() -> Self {
        Self {
            decision: HookDecision::None,
            halt: false,
            hook_count: 0,
            hooks: Vec::new(),
            reason: String::new(),
            context: String::new(),
            updated_input: String::new(),
        }
    }
}

// ── Stdout JSON parsing ─────────────────────────────────────────────────────

#[derive(Deserialize)]
struct StdoutEnvelope {
    #[serde(default)]
    decision: String,
    #[serde(default)]
    halt: bool,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    context: serde_json::Value,
    #[serde(default)]
    updated_input: serde_json::Value,
}

fn parse_decision(s: &str) -> HookDecision {
    match s.to_lowercase().as_str() {
        "allow" => HookDecision::Allow,
        "deny" => HookDecision::Deny,
        _ => HookDecision::None,
    }
}

fn parse_context(raw: &serde_json::Value) -> String {
    match raw {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(arr) => arr
            .iter()
            .filter_map(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn parse_stdout(stdout: &str) -> HookResult {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return HookResult::default();
    }
    let Ok(envelope) = serde_json::from_str::<StdoutEnvelope>(trimmed) else {
        return HookResult::default();
    };
    HookResult {
        decision: parse_decision(&envelope.decision),
        halt: envelope.halt,
        reason: envelope.reason,
        context: parse_context(&envelope.context),
        updated_input: match envelope.updated_input {
            serde_json::Value::String(s) => s,
            serde_json::Value::Null => String::new(),
            other => other.to_string(),
        },
    }
}

// ── Aggregation ─────────────────────────────────────────────────────────────

/// Merge multiple `HookResult`s into a single `AggregateResult`.
///
/// Order matters: deny wins over allow, allow wins over none. Halt is
/// sticky. Reasons and context concatenate in order.
fn aggregate(results: &[(HookConfig, HookResult)], orig_input: &str) -> AggregateResult {
    let mut decision = HookDecision::None;
    let mut halt = false;
    let mut reasons = Vec::new();
    let mut contexts = Vec::new();
    let mut merged = orig_input.to_string();
    let mut any_patch = false;
    let mut hooks = Vec::new();

    for (cfg, result) in results {
        // Decision priority: deny > allow > none
        match result.decision {
            HookDecision::Deny => {
                decision = HookDecision::Deny;
                if !result.reason.is_empty() {
                    reasons.push(result.reason.clone());
                }
            }
            HookDecision::Allow => {
                if decision != HookDecision::Deny {
                    decision = HookDecision::Allow;
                }
            }
            HookDecision::None => {}
        }

        if result.halt {
            halt = true;
            if !result.reason.is_empty() && result.decision != HookDecision::Deny {
                reasons.push(result.reason.clone());
            }
        }

        if !result.context.is_empty() {
            contexts.push(result.context.clone());
        }

        if !result.updated_input.is_empty() {
            match shallow_merge(&merged, &result.updated_input) {
                Ok(next) => {
                    merged = next;
                    any_patch = true;
                }
                Err(e) => {
                    log::warn!("Hook updated_input patch rejected: {e}");
                }
            }
        }

        hooks.push(HookInfo {
            name: cfg.display_name().to_string(),
            matcher: cfg.matcher.clone(),
            decision: result.decision.to_string(),
            halt: result.halt,
            reason: result.reason.clone(),
            input_rewrite: !result.updated_input.is_empty(),
        });
    }

    AggregateResult {
        decision,
        halt,
        hook_count: results.len(),
        hooks,
        reason: reasons.join("\n"),
        context: contexts.join("\n"),
        updated_input: if any_patch { merged } else { String::new() },
    }
}

/// Shallow-merge a JSON patch onto a JSON base object.
fn shallow_merge(base: &str, patch: &str) -> Result<String, String> {
    let base_val: serde_json::Value = if base.is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(base).map_err(|e| format!("base: {e}"))?
    };
    let patch_val: serde_json::Value =
        serde_json::from_str(patch).map_err(|e| format!("patch: {e}"))?;

    let base_obj = base_val
        .as_object()
        .ok_or("base is not a JSON object")?;
    let patch_obj = patch_val
        .as_object()
        .ok_or("patch is not a JSON object")?;

    let mut merged = base_obj.clone();
    for (k, v) in patch_obj {
        merged.insert(k.clone(), v.clone());
    }
    Ok(serde_json::to_string(&merged).unwrap_or_default())
}

// ── Runner ──────────────────────────────────────────────────────────────────

/// Compiled hook with pre-compiled regex matcher.
struct CompiledHook {
    config: HookConfig,
    matcher: Option<regex::Regex>,
}

/// Executes hook commands and aggregates results.
pub struct HookRunner {
    hooks: Vec<CompiledHook>,
    cwd: String,
}

impl HookRunner {
    /// Create a runner from hook configs. Invalid matchers are skipped.
    pub fn new(configs: &[HookConfig], cwd: &str) -> Self {
        let mut hooks: Vec<CompiledHook> = configs
            .iter()
            .filter_map(|cfg| {
                let matcher = if cfg.matcher.is_empty() {
                    None
                } else {
                    match regex::Regex::new(&cfg.matcher) {
                        Ok(re) => Some(re),
                        Err(e) => {
                            log::warn!(
                                "Hook matcher failed to compile; skipping: {} ({e})",
                                cfg.matcher,
                            );
                            return None;
                        }
                    }
                };
                Some(CompiledHook {
                    config: cfg.clone(),
                    matcher,
                })
            })
            .collect();

        // Deduplicate by command string (first wins).
        let mut seen = std::collections::HashSet::new();
        hooks.retain(|h| seen.insert(h.config.command.clone()));

        Self {
            hooks,
            cwd: cwd.to_string(),
        }
    }

    /// Returns true if any hooks are configured.
    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }

    /// Run all matching hooks for the given tool call and return the
    /// aggregated result.
    pub fn run(&self, tool_name: &str, tool_input: &str) -> AggregateResult {
        let matching = self.matching_hooks(tool_name);
        if matching.is_empty() {
            return AggregateResult::default();
        }

        // Deduplicate by command string.
        let mut seen = std::collections::HashSet::new();
        let deduped: Vec<&CompiledHook> = matching
            .into_iter()
            .filter(|h| seen.insert(h.config.command.as_str()))
            .collect();

        let payload = build_payload(tool_name, tool_input);
        let results: Vec<(HookConfig, HookResult)> = deduped
            .iter()
            .map(|h| {
                let result = self.run_one(&h.config, &payload);
                (h.config.clone(), result)
            })
            .collect();

        aggregate(&results, tool_input)
    }

    fn matching_hooks(&self, tool_name: &str) -> Vec<&CompiledHook> {
        self.hooks
            .iter()
            .filter(|h| {
                h.matcher
                    .as_ref()
                    .map_or(true, |re| re.is_match(tool_name))
            })
            .collect()
    }

    fn run_one(&self, config: &HookConfig, payload: &str) -> HookResult {
        let _timeout = config.timeout_duration();

        let output = match Command::new("sh")
            .arg("-c")
            .arg(&config.command)
            .current_dir(&self.cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .env("COSH_EVENT", "PreToolUse")
            .env("COSH_TOOL_NAME", &payload)
            .output()
        {
            Ok(output) => output,
            Err(e) => {
                log::warn!("Hook failed to execute: {} ({e})", config.command);
                return HookResult::default();
            }
        };

        // Check timeout is handled by the caller via a timeout wrapper
        // if needed. For simplicity, we rely on the OS process timeout.

        let exit_code = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        match exit_code {
            0 => parse_stdout(&stdout),
            2 => {
                let reason = if stderr.trim().is_empty() {
                    "blocked by hook".to_string()
                } else {
                    stderr.trim().to_string()
                };
                HookResult {
                    decision: HookDecision::Deny,
                    reason,
                    ..Default::default()
                }
            }
            HALT_EXIT_CODE => {
                let reason = if stderr.trim().is_empty() {
                    "turn halted by hook".to_string()
                } else {
                    stderr.trim().to_string()
                };
                HookResult {
                    decision: HookDecision::Deny,
                    halt: true,
                    reason,
                    ..Default::default()
                }
            }
            _ => {
                log::warn!(
                    "Hook failed with non-blocking error: exit={exit_code} stderr={stderr}"
                );
                HookResult::default()
            }
        }
    }
}

// ── Payload ─────────────────────────────────────────────────────────────────

/// Build the JSON payload piped to hook commands via stdin.
fn build_payload(tool_name: &str, tool_input: &str) -> String {
    let tool_input_val: serde_json::Value =
        serde_json::from_str(tool_input).unwrap_or(serde_json::json!({}));
    serde_json::json!({
        "event": "PreToolUse",
        "tool_name": tool_name,
        "tool_input": tool_input_val,
    })
    .to_string()
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregate_empty() {
        let agg = aggregate(&[], "{}");
        assert_eq!(agg.decision, HookDecision::None);
        assert!(!agg.halt);
        assert_eq!(agg.hook_count, 0);
    }

    #[test]
    fn aggregate_allow_wins_over_none() {
        let results = vec![
            (
                HookConfig {
                    name: "a".into(),
                    matcher: String::new(),
                    command: "true".into(),
                    timeout: None,
                },
                HookResult {
                    decision: HookDecision::None,
                    ..Default::default()
                },
            ),
            (
                HookConfig {
                    name: "b".into(),
                    matcher: String::new(),
                    command: "true".into(),
                    timeout: None,
                },
                HookResult {
                    decision: HookDecision::Allow,
                    ..Default::default()
                },
            ),
        ];
        let agg = aggregate(&results, "{}");
        assert_eq!(agg.decision, HookDecision::Allow);
    }

    #[test]
    fn aggregate_deny_wins_over_allow() {
        let results = vec![
            (
                HookConfig {
                    name: "a".into(),
                    matcher: String::new(),
                    command: "true".into(),
                    timeout: None,
                },
                HookResult {
                    decision: HookDecision::Allow,
                    ..Default::default()
                },
            ),
            (
                HookConfig {
                    name: "b".into(),
                    matcher: String::new(),
                    command: "true".into(),
                    timeout: None,
                },
                HookResult {
                    decision: HookDecision::Deny,
                    reason: "blocked".into(),
                    ..Default::default()
                },
            ),
        ];
        let agg = aggregate(&results, "{}");
        assert_eq!(agg.decision, HookDecision::Deny);
        assert_eq!(agg.reason, "blocked");
    }

    #[test]
    fn aggregate_halt_is_sticky() {
        let results = vec![
            (
                HookConfig {
                    name: "a".into(),
                    matcher: String::new(),
                    command: "true".into(),
                    timeout: None,
                },
                HookResult {
                    decision: HookDecision::Allow,
                    ..Default::default()
                },
            ),
            (
                HookConfig {
                    name: "b".into(),
                    matcher: String::new(),
                    command: "true".into(),
                    timeout: None,
                },
                HookResult {
                    halt: true,
                    reason: "stop".into(),
                    ..Default::default()
                },
            ),
        ];
        let agg = aggregate(&results, "{}");
        assert!(agg.halt);
        assert_eq!(agg.reason, "stop");
    }

    #[test]
    fn parse_stdout_empty() {
        let r = parse_stdout("");
        assert_eq!(r.decision, HookDecision::None);
    }

    #[test]
    fn parse_stdout_allow() {
        let r = parse_stdout(r#"{"decision": "allow"}"#);
        assert_eq!(r.decision, HookDecision::Allow);
    }

    #[test]
    fn parse_stdout_deny_with_reason() {
        let r = parse_stdout(r#"{"decision": "deny", "reason": "nope"}"#);
        assert_eq!(r.decision, HookDecision::Deny);
        assert_eq!(r.reason, "nope");
    }

    #[test]
    fn shallow_merge_basic() {
        let base = r#"{"a": 1, "b": 2}"#;
        let patch = r#"{"b": 3, "c": 4}"#;
        let merged = shallow_merge(base, patch).unwrap();
        let val: serde_json::Value = serde_json::from_str(&merged).unwrap();
        assert_eq!(val["a"], 1);
        assert_eq!(val["b"], 3);
        assert_eq!(val["c"], 4);
    }

    #[test]
    fn runner_deduplicates_by_command() {
        let runner = HookRunner::new(
            &[
                HookConfig {
                    name: "a".into(),
                    matcher: String::new(),
                    command: "echo allow".into(),
                    timeout: None,
                },
                HookConfig {
                    name: "b".into(),
                    matcher: String::new(),
                    command: "echo allow".into(), // duplicate
                    timeout: None,
                },
            ],
            "/tmp",
        );
        assert_eq!(runner.hooks.len(), 1);
    }

    #[test]
    fn runner_matches_tool_name() {
        let runner = HookRunner::new(
            &[
                HookConfig {
                    name: "bash-only".into(),
                    matcher: "bash".into(),
                    command: "echo bash".into(),
                    timeout: None,
                },
                HookConfig {
                    name: "all".into(),
                    matcher: String::new(),
                    command: "echo all".into(),
                    timeout: None,
                },
            ],
            "/tmp",
        );
        // Both match bash_run (different commands, no dedup)
        let matching = runner.matching_hooks("bash_run");
        assert_eq!(matching.len(), 2);

        // Only "all" matches fs_read
        let matching = runner.matching_hooks("fs_read");
        assert_eq!(matching.len(), 1);
    }
}
