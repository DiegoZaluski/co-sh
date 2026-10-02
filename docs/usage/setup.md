# Setup reference

This is the complete reference for Cosh's `setup.json`. It is intentionally
short: use the TUI first, and edit the file directly only when you need a
precise or advanced configuration.

## Start in the TUI

The distributed **slim** Cosh starts directly in a session. It has no Home
screen. Type these commands in the chat input and press Enter:

| Command | Opens |
|---|---|
| `/settings` | All persistent settings, grouped by section |
| `/models` | Provider, model, and reasoning selection |
| `/router` | Model fallback and prompt-correction chains |
| `/providers` | Local provider endpoints |
| `/tools` | Enable or disable internal tools |
| `/themes`, `/background`, `/bell`, `/anim`, `/toolcall` | Direct toggles |

Changes made in the TUI are saved to `setup.json`. Navigate with the arrow
keys, press Enter to edit, and Esc to go back.

## Where the file lives

On Unix, the default path is `~/.config/cosh/setup.json`. On other platforms,
Cosh uses the operating system's application configuration directory. Set
`COSH_CONFIG_DIR` to use another directory; Cosh then reads and writes
`<that-directory>/setup.json`.

The file is JSON. Missing fields use their defaults. A missing or invalid file
also starts with defaults. `COSH_DATA_DIR` changes the data directory for
sessions and caches; it does not change the location of `setup.json`.

## Quick map

| JSON section | Main TUI location | Purpose |
|---|---|---|
| `appearance` | `/themes`, `/background`, `/bell`, `/anim` | Theme and visual behavior |
| `tools` | `/toolcall`, `/tools` | Tool-call format and disabled tools |
| `routing` | `/router`; `/settings` → Automation | Fallback and summarization chains |
| `hooks` | `/settings` → Automation | Commands before/after tool calls |
| `providers` | `/providers` | Local model-server URLs |
| `model` | `/models` | Last selected provider/model |
| `mode` | Session mode cycle | Last selected agent mode |
| `cache` | `/settings` → Models & caching | Prompt-cache retention |
| `skills` | `/settings` → Environment | Skill discovery |
| `model_decision` | `/settings` → Automation, ONNX builds only | Local termination decision model |
| `lsp` | `/settings` → Environment | Language-server integration |
| `mcp` | `/settings` → Integrations | External MCP servers |
| `editor` | `/settings` → Environment | Editor opened by the file explorer |
| `telemetry` | `/settings` → General | Anonymous usage telemetry |

## `appearance`

| Field | Default | What it controls |
|---|---:|---|
| `theme` | `""` | Theme name. Empty uses the registry default. Use `/themes`. |
| `bell_enabled` | `true` | Rings the terminal bell when the agent loop finishes. |
| `transparent_background` | `false` | Uses the terminal's own background color for the TUI base. Panels keep their theme colors. |
| `anim_enabled` | `true` | Shows the animated chat logo on an empty session. |

The TUI commands `/background`, `/bell`, and `/anim` edit the corresponding
boolean fields.

## `tools`

| Field | Default | What it controls |
|---|---:|---|
| `tool_call_mode` | `"native"` | `native` sends structured tool calls; `inline` sends JSON text that Cosh parses. Change with `/toolcall`. |
| `disabled` | `[]` | Tool names hidden from the model. Manage them with `/tools` → Internal Tools. |

## `routing`

All chains are ordered: Cosh tries the first entry, then the next one.

| Field | Default | What it controls |
|---|---:|---|
| `fallbacks` | built-in chain | Provider/model fallback when the active model cannot answer. Edit in `/router`. |
| `fallback_prompt_corrector` | `[]` | Ordered models or ACP agents used to correct a prompt. Edit in `/router` → Fallback Prompt Corrector. |
| `summarization_models` | `[]` | Provider/model chain for context summarization. Empty uses the active agent model. Edit in `/settings` → Automation → Summarization models. |

When `fallbacks` is empty or missing, the built-in order is:

1. `nvidia` → `deepseek-ai/deepseek-v4-pro`
2. `openrouter` → `deepseek/deepseek-v4-pro`
3. `groq` → `openai/gpt-oss-120b`
4. `charm` → `deepseek-ai/deepseek-v4-pro`

A model fallback entry is:

```json
{ "provider": "openrouter", "model": "deepseek/deepseek-v4-pro" }
```

Prompt-corrector entries are tagged objects:

```json
[
  { "kind": "model", "provider": "openrouter", "model": "..." },
  { "kind": "acp", "agent": "my-agent" }
]
```

## `hooks`

Use `/settings` → Automation → **PreToolUse hooks** or **PostToolUse hooks**.
The detailed lifecycle and exit-code behavior is documented in
[hooks.md](hooks.md).

| Field | Default | What it controls |
|---|---:|---|
| `pre_tool_use_enabled` | `true` | Enables hooks before a tool call. |
| `post_tool_use_enabled` | `true` | Enables hooks after a successful tool call. |
| `PreToolUse` | `[]` | Hook entries for the pre-call event. |
| `PostToolUse` | `[]` | Hook entries for the post-call event. |

Each event array contains objects with:

| Field | Default | What it controls |
|---|---:|---|
| `name` | `""` | Display name; an empty name displays the command. |
| `matcher` | `""` | Regex matched against the tool name; empty matches every tool. |
| `command` | required | Shell command to run. |
| `timeout` | `30` seconds | Maximum runtime. |

Example:

```json
{
  "hooks": {
    "pre_tool_use_enabled": true,
    "post_tool_use_enabled": true,
    "PreToolUse": [
      { "name": "protect files", "matcher": "^write_file$", "command": "./check-write.sh", "timeout": 10 }
    ],
    "PostToolUse": []
  }
}
```

## `providers`

`providers.local` maps a local provider name to its endpoint. Add it with
`/providers` → **ADD Provider**.

```json
{
  "providers": {
    "local": {
      "ollama": { "base_url": "http://localhost:11434" }
    }
  }
}
```

API keys for regular providers are not `setup.json` settings: Cosh obtains
them from the OS keyring and/or provider-specific environment variables.

## `model` and `mode`

These sections persist the last choice for **new sessions**; they do not
rewrite existing session history.

| Field | Default | What it controls |
|---|---:|---|
| `model.provider` | `""` | Provider of the last selected model. |
| `model.model` | `""` | Model ID of the last selected model. |
| `model.reasoning` | `null` | Model-specific reasoning effort; `null` uses the model default. |
| `mode.mode` | `""` | Last agent mode: `build`, `ask`, `yolo`, or `command`. Empty/unknown uses `build`. |

Use `/models` for `model.*`. Cycle the agent mode with the session's mode
shortcut (Tab); the selected mode is saved automatically.

## `cache`

Configure these in `/settings` → Models & caching. The input accepts blank,
minutes, `45m`, `1h`, or `1h30m`, up to 24 hours. The stored values are wishes
in minutes; Cosh maps them to the API values that each provider supports.

| Field | Default | What it controls |
|---|---:|---|
| `anthropic_ttl_min` | `0` | `0` uses Anthropic's 5-minute behavior; a value above 5 selects the 1-hour cache (higher write cost). |
| `openai_retention_min` | `0` | `0` uses the model/provider default; a value above 0 selects 24-hour retention. |

## `skills`

Configure the source list in `/settings` → Environment → Skill directories.
Enter multiple directories separated by `:`. An empty list uses `~/.skills`.
Each source contains one directory per skill, with a `SKILL.md` inside; the
first source wins when names collide. A path beginning with `~` is expanded.

| Field | Default | What it controls |
|---|---:|---|
| `dirs` | `[]` | Skill directories in priority order. The TUI edits this field. |
| `recursive` | `false` | Scans for skills at any depth instead of only direct children. |
| `ignore` | `[]` | Glob-style skill-name exclusions, such as `deprecated-*`. File only. |
| `include` | `[]` | Exact skill-name allowlist. Empty allows all. File only. |

`recursive`, `ignore`, and `include` currently have no TUI control; edit them
directly in `setup.json` when needed.

## `model_decision` (ONNX builds only)

The distributed slim binary has no Home screen and is built without the ONNX
feature, so these settings are not shown in its TUI and no local decision
model runs there. They remain part of the file schema for binaries built with
`--features onnx`. In those builds, use `/settings` → Automation.

| Field | Default | What it controls |
|---|---:|---|
| `termination.enabled` | `true` | Enables the local model's termination review. |
| `termination.min_confidence` | `0.6` | Confidence floor for a “not finished” verdict to continue the loop. Range `0..=1`. |
| `model` | `english` | Checkpoint kind: `english`, `multilingual`, or `typed_decisions`; the TUI displays the last one as `typed-decisions`. |
| `hub.repo` | `Zaluski/laya-onnx` | Model-weight mirror repository. |
| `hub.pinned_repo` | `""` | Repository to which the recorded revision belongs. |
| `hub.pinned_revision` | `""` | Revision pin. Empty resolves and records the first installed revision. |

The named model kinds are selected by **Checkup model**. A custom checkpoint
is file-only:

```json
{
  "model_decision": {
    "model": { "kind": "custom", "repo": "/path/to/checkpoint", "subfolder": null }
  }
}
```

`COSH_ONNX_HUB_REPO` overrides `model_decision.hub.repo`. The legacy top-level
name `checkup` is still accepted when reading, but Cosh writes
`model_decision`.

## `lsp`, `mcp`, `editor`, and `telemetry`

| Setting | Default | TUI / behavior |
|---|---:|---|
| `lsp` | `true` | `/settings` → Environment → Language servers. `COSH_LSP=off`, `0`, or `false` disables it for the process. |
| `editor` | `""` | `/settings` → Environment → Editor. Empty auto-detects `nvim`, then `vim`, then `nano`; otherwise use a command such as `nvim -u NONE` or `code -w`. |
| `telemetry` | `true` | `/settings` → General → Telemetry. This is opt-out and applies on the next launch; `COSH_TELEMETRY=off` disables it. CI is always off. |

### `mcp`

Manage servers in `/settings` → Integrations → MCP servers. A server is
enabled by default; disabling it skips it at startup.

| Field | What it controls |
|---|---|
| `name` | Unique name shown in the TUI and used for runtime state. |
| `enabled` | Whether Cosh connects to the server at startup. |
| `transport.type` | `stdio` for a local process or `http` for a remote server. |
| `stdio.command`, `args` | Executable and arguments for the local process. |
| `stdio.env`, `cwd` | Environment variables and optional working directory for that process. |
| `http.url` | Remote `http://` or `https://` endpoint. |
| `http.headers` | Static extra request headers; do not put secrets here. |
| `http.api_key_env` | Environment-variable name used for the HTTP API key. |
| `http.timeout_ms` | Per-request timeout in milliseconds; default `30000`. |

```json
{
  "mcp": {
    "servers": [
      {
        "name": "local-tools",
        "enabled": true,
        "transport": {
          "type": "stdio",
          "command": "my-mcp-server",
          "args": ["--stdio"],
          "env": {},
          "cwd": null
        }
      },
      {
        "name": "remote-tools",
        "transport": {
          "type": "http",
          "url": "https://example.com/mcp",
          "headers": {},
          "api_key_env": "MCP_API_KEY",
          "timeout_ms": 30000
        },
        "enabled": true
      }
    ]
  }
}
```

In the TUI, enter a command and arguments for stdio, or an `http(s)://` URL
for HTTP. Timeout is entered in seconds; the file stores milliseconds and
defaults to `30000` (30 seconds). HTTP credentials entered as a literal key
are stored in the OS keyring, not in `setup.json`; entering `$VAR` stores the
environment-variable name in `api_key_env`. Stdio credentials belong in that
server's `transport.env`.

## Environment values that affect setup

These are convenience inputs, not additional JSON fields:

| Variable | Affects |
|---|---|
| `COSH_CONFIG_DIR` | Directory containing `setup.json`. |
| `COSH_DATA_DIR` | Session and data storage directory. |
| `COSH_PROVIDER`, `COSH_MODEL`, `COSH_REASONING` | Initial model defaults before a persisted `/models` choice. |
| `COSH_TOOL_CALL_MODE` | Initial tool-call mode before the persisted setting is applied. |
| `COSH_LSP` | Runtime LSP override. |
| `COSH_TELEMETRY` | Runtime telemetry override. |
| `COSH_ONNX_HUB_REPO` | ONNX mirror override in ONNX builds. |

For normal use, configure through the TUI and let Cosh write the file. Edit
`setup.json` directly for `skills` filters, custom decision checkpoints,
advanced routing, or automation that must be reproducible.
