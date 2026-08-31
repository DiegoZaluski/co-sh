# Setup Reference

Cosh stores all persistent configuration in `~/.config/cosh/setup.json`. A missing or corrupt file is treated as defaults — every field has a safe fallback.

This guide documents every section of the file.

## Full skeleton

```json
{
  "appearance": {
    "theme": "",
    "bell_enabled": true,
    "transparent_background": false,
    "anim_enabled": true
  },
  "tools": {
    "tool_call_mode": "native",
    "disabled": []
  },
  "routing": {
    "fallbacks": []
  },
  "hooks": {
    "pre_tool_use_enabled": true,
    "post_tool_use_enabled": true,
    "PreToolUse": [],
    "PostToolUse": []
  },
  "providers": {
    "local": {},
    "zen_public_opt_in": null
  },
  "model": {
    "provider": "",
    "model": "",
    "reasoning": null
  },
  "cache": {
    "anthropic_ttl_min": 0,
    "openai_retention_min": 0
  }
}
```

## appearance

Controls the look and feel of the TUI.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `theme` | string | `""` | Selected theme name. Empty uses the built-in default. Change via `/themes`. |
| `bell_enabled` | bool | `true` | Ring terminal bell when the agent loop finishes. Toggle via `/bell`. |
| `transparent_background` | bool | `false` | Paint the TUI background with the terminal's default color. Toggle via `/background`. |
| `anim_enabled` | bool | `true` | Show animated chat logo on the empty-session landing screen. Toggle via `/anim`. |

## tools

Controls which tools are available and how they are delivered to the model.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `tool_call_mode` | string | `"native"` | `"native"` sends tool calls as structured parts. `"inline"` sends them as JSON text the harness parses. Toggle via `/toolcall`. |
| `disabled` | string[] | `[]` | Tool names the user disabled. Disabled tools are hidden from the model entirely. |

## routing

Controls the fallback provider/model chain used when the primary provider is unavailable.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `fallbacks` | object[] | *(see below)* | Ordered list of `{ "provider": "...", "model": "..." }` entries tried in sequence. |

### Default fallbacks

When `fallbacks` is empty or missing, Cosh uses:

1. `nvidia` → `deepseek-ai/deepseek-v4-pro`
2. `openrouter` → `deepseek/deepseek-v4-pro`
3. `groq` → `openai/gpt-oss-120b`
4. `charm` → `deepseek-ai/deepseek-v4-pro`

## hooks

Controls lifecycle hooks that fire around each tool call.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `pre_tool_use_enabled` | bool | `true` | Master switch for PreToolUse hooks. Toggle in the Settings screen. |
| `post_tool_use_enabled` | bool | `true` | Master switch for PostToolUse hooks. Toggle in the Settings screen. |
| event keys (`"PreToolUse"`, `"PostToolUse"`) | array | `[]` | Hook configurations keyed by event name. Each value is an array of hook entries. |

### Hook entry fields

| Field | Required | Description |
|-------|----------|-------------|
| `name` | No | Friendly display name. Falls back to `command` if empty. |
| `matcher` | No | Regex pattern tested against the tool name. Empty = match all tools. |
| `command` | Yes | Shell command to execute. |
| `timeout` | No | Timeout in seconds (default: 30). |

See [hooks.md](hooks.md) for detailed documentation on hook behavior, exit codes, and examples.

## providers

Configures provider endpoints and the free-gateway opt-in.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `local` | object | `{}` | Map of local provider name → endpoint config. Configured via the ADD Provider screen. |
| `zen_public_opt_in` | bool/null | `null` | One-time answer to the OpenCode Zen free-gateway prompt. `true` = opted in, `false` = declined, `null` = never asked. Write-once from the UI — the prompt is shown exactly once and never re-asked. Reset by deleting this field from setup.json. |

### Local endpoint

```json
{
  "local": {
    "my-server": {
      "base_url": "http://localhost:8080"
    }
  }
}
```

## model

Persists the last-selected model so new sessions start with the same provider/model/reasoning.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `provider` | string | `""` | Provider of the last selected model. Empty = auto-detect. |
| `model` | string | `""` | Model id of the last selected model. Empty = no selection yet. |
| `reasoning` | string/null | `null` | Reasoning effort for the last model. `None` = model default. Common values: `"low"`, `"medium"`, `"high"`. |

## cache

Controls prompt-cache behavior for Anthropic and OpenAI providers.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `anthropic_ttl_min` | integer | `0` | Anthropic prompt-cache TTL wish in minutes. `0` = the 5-minute default. Any value above 5 maps onto the 1-hour TTL (the only extended value the API supports). Configure in the Settings screen. |
| `openai_retention_min` | integer | `0` | OpenAI prompt-cache retention wish in minutes. `0` = model-dependent default. Any value above 0 maps onto the 24-hour retention (the only extended value the API supports). Configure in the Settings screen. |

### Duration input format

When you select a cache setting in the Settings screen, Cosh opens a duration input box. Accepted formats:

| Input | Meaning |
|-------|---------|
| `default` or empty | Provider default |
| `45` or `45m` | 45 minutes |
| `1h` | 1 hour |
| `1h30m` | 1 hour 30 minutes |
| `24h` | 24 hours (maximum) |

The input is case-insensitive and spaces are allowed (e.g. `90 M`, `2 h`).

## Environment variables

These override `setup.json` values at startup. They are read once and not refreshed during the session.

| Variable | Default | Description |
|----------|---------|-------------|
| `COSH_PROVIDER` | auto-detected (falls back to `openai`) | Initial provider. |
| `COSH_MODEL` | none | Initial model id. |
| `COSH_REASONING` | none | Initial reasoning effort (`low`, `medium`, `high`). |
| `COSH_TOOL_CALL_MODE` | `"native"` | Tool call delivery mode (`native` or `inline`). |
| `COSH_LSP` | on | Set to `off`, `0`, or `false` to disable language-server integration. |

## Slash commands that modify setup.json

These are typed in the chat input and persist their result to `setup.json`:

| Command | Field modified | Description |
|---------|---------------|-------------|
| `/models` | `model.*` | Manage providers and models. |
| `/themes` | `appearance.theme` | Change the color theme. |
| `/background` | `appearance.transparent_background` | Toggle transparent background. |
| `/bell` | `appearance.bell_enabled` | Toggle terminal bell on completion. |
| `/anim` | `appearance.anim_enabled` | Toggle animated chat logo. |
| `/toolcall` | `tools.tool_call_mode` | Switch between native and inline tool call modes. |
