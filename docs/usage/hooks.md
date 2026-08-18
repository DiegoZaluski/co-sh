# PreToolUse Hooks

PreToolUse hooks let you run custom shell commands **before** every tool call. You can block dangerous operations, auto-approve safe ones, rewrite tool arguments, or inject extra context into tool results.

Hooks are configured in `~/.config/cosh/setup.json` under the `hooks` section.

## Configuration

Open `~/.config/cosh/setup.json` and add a `hooks` block:

```json
{
  "appearance": { "theme": "", "bell_enabled": true },
  "tools": { "tool_call_mode": "native", "disabled": [] },
  "routing": { "fallbacks": [] },
  "hooks": {
    "PreToolUse": [
      {
        "name": "Block dangerous bash",
        "matcher": "bash_run",
        "command": "if echo \"$COSH_TOOL_NAME\" | grep -q 'rm -rf'; then echo '{\"decision\": \"deny\", \"reason\": \"rm -rf is not allowed\"}'; exit 2; fi"
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

```
Hooks (allow/deny/halt) → Permission System → Tool Execution
```

- A hook `allow` auto-approves the permission prompt (skips the dialog).
- A hook `deny` blocks before any permission check.
- A hook `halt` stops the entire turn immediately.
- If no hooks match, the normal permission system applies.

## Notes

- Hooks run **synchronously** before each tool call — keep them fast.
- Invalid regex in `matcher` is silently skipped (hook won't run).
- Duplicate commands are deduplicated (first wins).
- The `timeout` field kills the hook process after N seconds if it hasn't finished.
