//! Lifecycle hooks — user-defined shell commands that fire around each tool
//! call (`PreToolUse` before, `PostToolUse` after a successful execution),
//! returning decisions that control agent behavior.
//!
//! Hooks are configured in `setup.json` (sections `PreToolUse` and
//! `PostToolUse`) and run via the system shell. Each hook receives the tool
//! name and input as a JSON payload on stdin (`tool_output` is also present
//! for post-execution hooks), and responds with a JSON decision on stdout.
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

    let base_obj = base_val.as_object().ok_or("base is not a JSON object")?;
    let patch_obj = patch_val.as_object().ok_or("patch is not a JSON object")?;

    let mut merged = base_obj.clone();
    for (k, v) in patch_obj {
        merged.insert(k.clone(), v.clone());
    }
    Ok(serde_json::to_string(&merged).unwrap_or_default())
}

// ── Events ──────────────────────────────────────────────────────────────────

/// Lifecycle points where hooks can fire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    /// Before a tool call — can deny, allow or rewrite its input.
    PreToolUse,
    /// After a tool call completed successfully — can inject context into
    /// the result or halt the turn.
    PostToolUse,
}

impl HookEvent {
    /// Canonical event name surfaced to hook scripts (`COSH_EVENT` and the
    /// stdin payload) and used as the `setup.json` section key.
    pub const fn name(self) -> &'static str {
        match self {
            Self::PreToolUse => "PreToolUse",
            Self::PostToolUse => "PostToolUse",
        }
    }
}

// ── Runner ──────────────────────────────────────────────────────────────────

/// Compiled hook with pre-compiled regex matcher.
struct CompiledHook {
    config: HookConfig,
    matcher: Option<regex::Regex>,
}

impl CompiledHook {
    /// Regex match against the tool name; empty matcher = all tools.
    fn matches(&self, tool_name: &str) -> bool {
        self.matcher
            .as_ref()
            .is_none_or(|re| re.is_match(tool_name))
    }
}

fn compile(configs: &[HookConfig]) -> Vec<CompiledHook> {
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
    hooks
}

/// Executes hook commands and aggregates results.
///
/// Holds one compiled list per lifecycle event; both share the same
/// execution/decision machinery and differ only in when they fire and what
/// the stdin payload carries.
pub struct HookRunner {
    pre: Vec<CompiledHook>,
    post: Vec<CompiledHook>,
    cwd: String,
}

impl HookRunner {
    /// Create a runner from pre/post hook configs. Invalid matchers are
    /// skipped with a warning.
    pub fn new(pre: &[HookConfig], post: &[HookConfig], cwd: &str) -> Self {
        Self {
            pre: compile(pre),
            post: compile(post),
            cwd: cwd.to_string(),
        }
    }

    /// Returns true if no hooks are configured at all.
    pub fn is_empty(&self) -> bool {
        self.pre.is_empty() && self.post.is_empty()
    }

    /// Run all matching PreToolUse hooks for the given tool call.
    pub fn run_pre(&self, tool_name: &str, tool_input: &str) -> AggregateResult {
        let matching: Vec<&CompiledHook> =
            self.pre.iter().filter(|h| h.matches(tool_name)).collect();
        if matching.is_empty() {
            return AggregateResult::default();
        }
        let payload = build_payload(HookEvent::PreToolUse, tool_name, tool_input, None);
        self.execute(&matching, &payload, tool_input)
    }

    /// Run all matching PostToolUse hooks for a completed tool call. The
    /// raw tool output is exposed to scripts as `tool_output` in the stdin
    /// payload.
    pub fn run_post(
        &self,
        tool_name: &str,
        tool_input: &str,
        tool_output: &str,
    ) -> AggregateResult {
        let matching: Vec<&CompiledHook> =
            self.post.iter().filter(|h| h.matches(tool_name)).collect();
        if matching.is_empty() {
            return AggregateResult::default();
        }
        let payload = build_payload(
            HookEvent::PostToolUse,
            tool_name,
            tool_input,
            Some(tool_output),
        );
        self.execute(&matching, &payload, tool_input)
    }

    fn execute(&self, hooks: &[&CompiledHook], payload: &str, orig_input: &str) -> AggregateResult {
        let results: Vec<(HookConfig, HookResult)> = hooks
            .iter()
            .map(|h| {
                let result = self.run_one(&h.config, payload);
                (h.config.clone(), result)
            })
            .collect();
        aggregate(&results, orig_input)
    }

    fn run_one(&self, config: &HookConfig, payload: &str) -> HookResult {
        use std::io::Write;
        use std::process::Stdio;
        use std::time::Instant;

        let timeout = config.timeout_duration();
        let tool_name = extract_tool_name_from_payload(payload);

        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg(&config.command)
            .current_dir(&self.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("COSH_EVENT", payload_event(payload))
            .env("COSH_TOOL_NAME", &tool_name);

        // Own process group so a timeout kill takes grandchildren (jq, cargo,
        // …) down together with the `sh` wrapper.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                log::warn!("Hook failed to execute: {} ({e})", config.command);
                return HookResult::default();
            }
        };

        // Deliver the JSON payload on its own thread, AFTER the stdout/stderr
        // readers exist: a payload larger than the pipe buffer (64 KiB —
        // realistic for PostToolUse, which embeds tool_output) must never
        // block the timeout loop below. Dropping the handle closes stdin so
        // scripts reading to EOF (`jq`, `grep`, …) terminate.
        if let Some(mut stdin) = child.stdin.take() {
            let owned = payload.to_string();
            std::thread::spawn(move || {
                let _ = stdin.write_all(owned.as_bytes());
                // Drop closes the pipe.
            });
        }

        // Read stdout/stderr in parallel before wait (avoids deadlock when
        // the pipe buffer fills while we wait).
        let stdout_handle = child.stdout.take().map(|mut out| {
            std::thread::spawn(move || {
                let mut buf = String::new();
                use std::io::Read;
                let _ = out.read_to_string(&mut buf);
                buf
            })
        });
        let stderr_handle = child.stderr.take().map(|mut err| {
            std::thread::spawn(move || {
                let mut buf = String::new();
                use std::io::Read;
                let _ = err.read_to_string(&mut buf);
                buf
            })
        });

        // Poll for completion instead of parking a killer thread: no fixed
        // latency for fast hooks, kill fires only when actually overdue.
        let deadline = Instant::now() + timeout;
        let mut timed_out = false;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) => {
                    if Instant::now() >= deadline {
                        timed_out = true;
                        #[cfg(unix)]
                        let _ = std::process::Command::new("kill")
                            .arg("-9")
                            .arg(format!("-{}", child.id())) // negative PID = group
                            .output();
                        #[cfg(not(unix))]
                        let _ = child.kill();
                        break child.wait();
                    }
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
                Err(e) => break Err(e),
            }
        };

        if timed_out {
            log::warn!("Hook timed out after {timeout:?}: {}", config.command,);
            return HookResult::default();
        }

        let exit_code = match status {
            Ok(s) => s.code().unwrap_or(-1),
            Err(e) => {
                log::warn!("Hook wait failed: {} ({e})", config.command);
                return HookResult::default();
            }
        };

        let stdout = stdout_handle
            .map(|h| h.join().unwrap_or_default())
            .unwrap_or_default();
        let stderr = stderr_handle
            .map(|h| h.join().unwrap_or_default())
            .unwrap_or_default();
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
                log::warn!("Hook failed with non-blocking error: exit={exit_code} stderr={stderr}");
                HookResult::default()
            }
        }
    }
}

// ── Payload ─────────────────────────────────────────────────────────────────

/// Extract the tool name from a JSON payload string.
fn extract_tool_name_from_payload(payload: &str) -> String {
    serde_json::from_str::<serde_json::Value>(payload)
        .ok()
        .and_then(|v| v.get("tool_name").cloned())
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default()
}

/// Build the JSON payload piped to hook commands via stdin. `tool_output`
/// is present only for post-execution events.
fn build_payload(
    event: HookEvent,
    tool_name: &str,
    tool_input: &str,
    tool_output: Option<&str>,
) -> String {
    let tool_input_val: serde_json::Value =
        serde_json::from_str(tool_input).unwrap_or(serde_json::json!({}));
    let mut payload = serde_json::json!({
        "event": event.name(),
        "tool_name": tool_name,
        "tool_input": tool_input_val,
    });
    if let Some(output) = tool_output {
        payload["tool_output"] = serde_json::Value::String(output.to_string());
    }
    payload.to_string()
}

/// Event name carried inside a payload (used for `COSH_EVENT`).
fn payload_event(payload: &str) -> String {
    serde_json::from_str::<serde_json::Value>(payload)
        .ok()
        .and_then(|v| v.get("event").cloned())
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| HookEvent::PreToolUse.name().to_string())
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
        let pre = [
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
        ];
        let runner = HookRunner::new(&pre, &[], "/tmp");
        assert_eq!(runner.pre.len(), 1);
    }

    #[test]
    fn runner_keeps_events_separate() {
        let pre = [HookConfig {
            name: "bash-only".into(),
            matcher: "bash".into(),
            command: "echo bash".into(),
            timeout: None,
        }];
        let post = [HookConfig {
            name: "reads".into(),
            matcher: "^fs_read$".into(),
            command: "echo reads".into(),
            timeout: None,
        }];
        let runner = HookRunner::new(&pre, &post, "/tmp");

        // Pre hooks never fire for post events and vice versa: matching is
        // scoped to the event's own list.
        assert!(runner.pre[0].matches("bash_run"));
        assert!(!runner.post[0].matches("bash_run"));
        assert!(runner.post[0].matches("fs_read"));
    }
}
