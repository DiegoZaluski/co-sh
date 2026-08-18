//! Demonstrate `subagent`: the `SubAgent` wrapper — the tool schema, the
//! input-reuse lifecycle (`resolve_input`), the description-note
//! interpolation (`set_note`), PATH-based agent detection, and agent-name
//! validation against the registry.
//!
//! This example deliberately does NOT invoke a real agent CLI: `call()`
//! spawns external AI agents (e.g. `opencode run --auto "…"`), which is a
//! heavyweight, side-effecting operation. Everything stateful and
//! deterministic is demonstrated instead.
//!
//! Run with:
//!
//! ```bash
//! cargo run --example subagent
//! ```

use cosh_tools::subagent::SubAgent;
use cosh_tools::subagent::call::{AGENTS, detect_installed, validate_agent};

fn main() {
    // ── 1. The tool schema ------------------------------------------------
    let sub = SubAgent::new();
    let schema = &sub.description_call["inputSchema"];
    println!("== 1. subagent_call schema ==");
    println!("  name: {}", sub.description_call["name"].as_str().unwrap());
    println!(
        "  required: {:?}  (neither agent nor input is required)",
        schema["required"].as_array().unwrap()
    );
    let agent_enum = schema["properties"]["agent"]["enum"].as_array().unwrap();
    println!("  agent enum: {:?}\n", agent_enum);

    // ── 2. Which CLIs are installed ----------------------------------------
    let installed = detect_installed();
    println!("== 2. detect_installed ==");
    if installed.is_empty() {
        println!("  no supported agent CLIs found in PATH");
    } else {
        println!("  installed: {}", installed.join(", "));
    }
    println!();

    // ── 3. The input-reuse lifecycle --------------------------------------
    println!("== 3. resolve_input lifecycle ==");
    // A fresh instance has nothing stored: omitting input errors.
    let err = sub.resolve_input(None).unwrap_err();
    println!("  fresh instance, input omitted -> error:");
    println!("    {err}");
    // First message: stored and returned.
    let first = sub.resolve_input(Some("review this PR".into())).unwrap();
    println!("  first call stored + returned: {first:?}");
    // Omitted input reuses the stored message (retry without re-typing).
    let reused = sub.resolve_input(None).unwrap();
    println!("  omitted input reuses:        {reused:?}");
    // Empty input also reuses it.
    let reused = sub.resolve_input(Some(String::new())).unwrap();
    println!("  empty input also reuses:     {reused:?}");
    // A new message overwrites the stored one.
    let _ = sub.resolve_input(Some("fix the bug".into())).unwrap();
    let reused = sub.resolve_input(None).unwrap();
    println!("  new message overwrites:      {reused:?}");
    // Each instance has its own store (sessions never share input).
    let other = SubAgent::new();
    println!(
        "  a fresh instance still errors: {}",
        other.resolve_input(None).is_err()
    );
    println!();

    // ── 4. set_note: description interpolation -----------------------------
    println!("== 4. set_note interpolation ==");
    let before = sub.description_call["description"].as_str().unwrap();
    println!(
        "  note absent by default: {}",
        !before.contains("internal agent runs the task instead")
    );
    let mut noted = SubAgent::new();
    noted.set_note("When `agent` is omitted or empty, an internal agent runs the task instead.");
    let after = noted.description_call["description"].as_str().unwrap();
    println!(
        "  after set_note, description mentions it: {}",
        after.contains("internal agent runs the task instead")
    );
    let mut empty_note = SubAgent::new();
    empty_note.set_note("");
    println!(
        "  empty note keeps the original: {}",
        empty_note.description_call == SubAgent::new().description_call
    );
    println!();

    // ── 5. Agent-name validation -------------------------------------------
    println!("== 5. validate_agent ==");
    println!("  opencode -> {:?}", validate_agent("opencode").is_ok());
    let err = validate_agent("nope").unwrap_err();
    println!("  nope -> error: {err}\n");

    // ── 6. The registry (display only — nothing is executed) ----------------
    println!("== 6. sample AGENTS invocations ==");
    for name in ["opencode", "claude", "aider"] {
        if let Some((_, binary, args)) = AGENTS.iter().find(|(n, _, _)| *n == name) {
            println!("  {name}: {binary} {} \"<input>\"", args.join(" "));
        }
    }
    println!("\n  ({} agents registered)", AGENTS.len());
}
