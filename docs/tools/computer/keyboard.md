# `keyboard` — the typing engine shared by both pipelines

`keyboard.rs` is the typing engine that [`control`](control.md) and
[`act`](act.md) both dispatch keyboard steps through. Its public surface is
small and pure — parse, classify, name — which makes it the part of the
module you can reason about (and test) with no desktop at all.

```
keyboard::parse_key(name: &str, tool: &str) -> Result<Key, String>
keyboard::parse_keys(names: &[String], tool: &str) -> Result<Vec<Key>, String>
keyboard::parse_hold_keys(names: &[String], tool: &str) -> Result<Vec<Key>, String>
keyboard::key_name(key: &Key) -> String
keyboard::is_keyboard_step(key: Option<&str>, text: Option<&str>) -> bool
keyboard::is_wait_step(wait_ms: Option<u64>) -> bool
keyboard::run_keyboard_step(sim, step, tool) · keyboard::run_wait_step(ms)
keyboard::parse_hold_keys(names, tool) ·
keyboard::run_hold_step(sim, names, tool) ·
keyboard::run_release_step(sim, names, tool)
keyboard::MAX_WAIT_MS = 10_000                              // pipeline wait cap
```

---

## The model: names in, keys out

1. **Named keys** parse case-insensitively, with aliases:
   `enter`/`return`, `escape`/`esc`, `delete`/`del`, `up`/`arrowup` (and the
   other arrows), `pageup`/`pagedown`, plus `tab`, `space`, `backspace`,
   `insert`, `home`, `end`, and `f1`..`f12`.
2. **Single characters** are taken literally — `a`, `5`, `-`.
3. **UPPERCASE characters are REJECTED.** This is the engine's most
   important rule: a silently-lowercased `A` would send the wrong key AND
   report success. The error teaches the fix:

   ```text
   computer_control: uppercase `A` — pass the lowercase key with
   `held: ["shift"]`
   ```

   (Or use `text` — see below.)
4. **Modifiers** collapse from an alias table onto four keys:
   `shift` · `ctrl`/`control` · `alt`/`option` · `meta`/`cmd`/`command`/`super`/`win`.
   An unknown modifier names the accepted set.

## Where keys land

Typing goes into whatever element holds keyboard focus NOW. The engine does
not aim keystrokes — aiming is a click's job, which is why the canonical
[`control`](control.md) pipeline is *click the field, then type*.

## `key` vs `text`

| | `key: "a"` | `text: "aA"` |
|---|---|---|
| Case | literal; uppercase REJECTED (hold `shift`) | typed as-is, case handled |
| `held` | allowed (chords: `key`+`held`) | REJECTED |
| Use when | single keystroke, shortcut, `enter` | literal strings, mixed case |

`held` applies to `key` only — `text` handles case itself, so a `held` on a
`text` step is a validation error, not a silent override.

## Step classification

The helpers `is_keyboard_step(key, text)` / `is_wait_step(wait_ms)` are the
single source of truth for "what KIND of step is this", shared by the
validators and the dispatcher so the two can never disagree:

- `key` counts after **trimming** — a whitespace-only key is not a step;
- `text` counts **as-is** — a literal space IS text (a spacebar keystroke);
- any `wait` value is a wait step (even `0`);
- `hold`/`release` are keyboard steps of their OWN — same zero/target rule
  (they touch nothing but the pressed-key state), and their key names run
  through the same `parse_key` alias table as `key`. The pressed state
  lives for the whole chain, so a `hold` in one step is still down when a
  later step types its shifted characters.

## Edges

- Chained `wait` steps cap at `MAX_WAIT_MS` (10 s) — for longer patience,
  use the [`wait`](wait.md) tool, which caps at 60 s.
- A second `hold` of a down key just re-presses it, and `release` of a key
  nothing held is HARMLESS (the platform layer treats a redundant
  up-stroke as a no-op) — a `hold`/`release` pair needs no caller-side
  tracking and cannot desync into an error.
- `key_name` renders the human-facing names the tools' `sent` reports use —
  modifiers spell out, characters pass through.

## See also

[`control`](control.md) for the pipeline that carries keyboard steps ·
`cargo run --example computer-keyboard`
