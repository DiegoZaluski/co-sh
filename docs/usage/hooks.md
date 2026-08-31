# Lifecycle Hooks

Cosh lets you run custom shell commands **before** every tool call (PreToolUse) and **after** every successful tool call (PostToolUse). You can block dangerous operations, auto-approve safe ones, rewrite tool arguments, or inject extra context into tool results.

Hooks are configured in `~/.config/cosh/setup.json` under the `hooks` section.

## Configuration

Open `~/.config/cosh/setup.json` and add a `hooks` block:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "name": "Block dangerous bash",
        "matcher": "bash_run",
        "command": "if echo \"$COSH_TOOL_NAME\" | grep -q 'rm -rf'; then echo '{\"decision\": \"deny\", \"reason\": \"rm -rf is not allowed\"}'; exit 2; fi"
      }
    ],
    "PostToolUse": [
      {
        "name": "Log file writes",
        "matcher": "fs_write",
        "command": "echo '{\"context\": \"File write completed: \" $COSH_TOOL_NAME}'"
      }
    ]
  }
}
```

### Hook fields

| Field | Required | Description |
|-------|----------|-------------|
| `name` | No | Friendly display name. Falls back to `command` if empty. |
| `matcher` | No | Regex pattern tested against the tool name. Empty = match all tools. |
| `command` | Yes | Shell command to execute. |
| `timeout` | No | Timeout in seconds (default: 30). |

## PreToolUse hooks

PreToolUse hooks run **before** dispatching a tool call. They can block, allow, or rewrite the call.

### How it works

1. Before dispatching a tool call, Cosh finds all hooks whose `matcher` matches the tool name.
2. Each matching hook runs via `sh -c "<command>"` in parallel.
3. The tool name and input are available via:
   - **stdin**: JSON payload `{"event":"PreToolUse","tool_name":"bash_run","tool_input":{...}}`
   - **env vars**: `COSH_EVENT`, `COSH_TOOL_NAME`
4. The hook's exit code and stdout determine the outcome.
5. Results from all hooks are aggregated (deny wins over allow, halt is sticky).

### Exit codes

| Code | Meaning |
|------|---------|
| `0` | Parse stdout JSON for decision |
| `2` | **Deny** this tool call (stderr = reason) |
| `49` | **Halt** the entire turn (stderr = reason) |
| Other | Non-blocking error, treated as "no opinion" |

### Stdout JSON (exit code 0)

```json
{
  "decision": "allow|deny|none",
  "reason": "why this was blocked",
  "context": "extra text appended to the tool result",
  "updated_input": {"command": "modified command here"}
}
```

- `decision`: `allow` auto-approves the permission prompt. `deny` blocks the tool call. `none` (or missing) expresses no opinion.
- `reason`: shown to the user when the tool is blocked.
- `context`: injected into the tool result after execution.
- `updated_input`: shallow-merges onto the tool's input JSON before execution.

## PostToolUse hooks

PostToolUse hooks run **after** a successful tool execution and **before** the result enters the conversation history. They are useful for logging, validation, or annotating tool output.

### How it works

1. After a tool call succeeds, Cosh finds all PostToolUse hooks whose `matcher` matches the tool name.
2. Each matching hook runs via `sh -c "<command>"` in parallel.
3. The tool name, input, and **output** are available via:
   - **stdin**: JSON payload `{"event":"PostToolUse","tool_name":"bash_run","tool_input":{...},"tool_output":"..."}`
   - **env vars**: `COSH_EVENT`, `COSH_TOOL_NAME`
4. Results from all hooks are aggregated.

### Key differences from PreToolUse

- The stdin payload includes a `tool_output` field with the raw tool result.
- `updated_input` is ignored — the tool already ran, so rewriting its input has no effect.
- `deny` **cannot un-run the tool**. Instead, it appends a `[hook] <reason>` warning line to the output the model sees. This is useful for flagging suspicious output without blocking execution.
- `context` is appended to the tool output the model sees.
- `halt` stops the turn immediately (same as PreToolUse).

### Example: flag suspicious output

```json
{
  "hooks": {
    "PostToolUse": [
      {
        "name": "Flag large file reads",
        "matcher": "fs_read",
        "command": "output_len=$(echo '{\"tool_output\":\"\"}' | wc -c); if [ \"$output_len\" -gt 10000 ]; then echo '{\"decision\": \"deny\", \"reason\": \"Large output detected — review before trusting\"}'; fi"
      }
    ]
  }
}
```

## Examples

### Block specific bash commands

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "name": "No destructive commands",
        "matcher": "bash_run",
        "command": "if echo \"$COSH_TOOL_NAME\" | grep -qE 'rm -rf|mkfs|dd if='; then exit 2; fi; echo '{\"decision\": \"allow\"}'"
      }
    ]
  }
}
```

### Auto-approve file reads

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "name": "Allow reads",
        "matcher": "^fs_read$",
        "command": "echo '{\"decision\": \"allow\"}'"
      }
    ]
  }
}
```

### Rewrite tool arguments

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "name": "Add --color=never to grep",
        "matcher": "find_grep",
        "command": "echo '{\"updated_input\": {\"command\": \"--color=never $COSH_TOOL_NAME\"}}'"
      }
    ]
  }
}
```

### Halt the entire turn

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "name": "Emergency stop",
        "matcher": ".*",
        "command": "if [ -f /tmp/cosh-halt ]; then echo 'HALTED' >&2; exit 49; fi"
      }
    ]
  }
}
```

## Priority order

### PreToolUse

```
Hooks (allow/deny/halt) → Permission System → Tool Execution
```

- A hook `allow` auto-approves the permission prompt (skips the dialog).
- A hook `deny` blocks before any permission check.
- A hook `halt` stops the entire turn immediately.
- If no hooks match, the normal permission system applies.

### PostToolUse

```
Tool Execution → Hooks (context/deny-as-warning/halt) → History
```

- A hook `context` appends text to the tool output before it enters history.
- A hook `deny` appends a `[hook] <reason>` warning line (the tool already ran).
- A hook `halt` stops the turn immediately.
- If no hooks match, the tool output enters history unchanged.

## Notes

- Hooks run **synchronously** — keep them fast.
- Invalid regex in `matcher` is silently skipped (hook won't run).
- Duplicate commands are deduplicated (first wins).
- The `timeout` field kills the hook process after N seconds if it hasn't finished.
- Both hook types can be toggled independently in the Settings screen.
