//! Demonstrate the `extract_action` module: registering tool schemas, batch
//! and streaming extraction over realistic LLM output, envelope aliases, the
//! bare-arguments fallback, validation failures, and the jsonish parser.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p cosh-sdk --example extract-action
//! ```

use cosh_sdk::extract_action::{
    ExtractAction, Item, StreamAction, ToolCallData, ToolSchema, find_json_objects,
};

fn show_items(label: &str, items: Vec<Item>) {
    println!("== {label} ==");
    for (i, item) in items.iter().enumerate() {
        match item {
            Item::Text(t) => println!("  [{i}] Text: {:?}", truncate(t, 60)),
            Item::ToolCall(call) => println!(
                "  [{i}] ToolCall: {} {:#}",
                call.name,
                call.arguments
            ),
        }
    }
    println!();
}

fn show_stream(label: &str, actions: Vec<StreamAction>) {
    println!("== {label} ==");
    for (i, action) in actions.iter().enumerate() {
        match action {
            StreamAction::Text(t) => println!("  [{i}] Text: {:?}", truncate(t, 60)),
            StreamAction::ToolCall(call) => {
                println!("  [{i}] ToolCall: {} {:#}", call.name, call.arguments)
            }
            StreamAction::Pending => println!("  [{i}] Pending"),
        }
    }
    println!();
}

fn truncate(s: &str, max: usize) -> String {
    let trimmed = s.replace('\n', "\\n");
    if trimmed.chars().count() > max {
        let cut: String = trimmed.chars().take(max).collect();
        format!("{cut}…")
    } else {
        trimmed
    }
}

fn main() {
    // A tool schema is a name plus a JSON-Schema subset. The extractor uses
    // the `properties` keys for its streaming early-exit optimization, and
    // the `required`/`type`/`const`/`oneOf` fields for validation.
    let read_tool = ToolSchema {
        name: "fs.read".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"],
        }),
    };
    let ask_tool = ToolSchema {
        name: "ask_questions".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "string" },
                            "question": { "type": "string" },
                            "type": { "type": "string" },
                        },
                        "required": ["id", "question", "type"],
                    },
                }
            },
            "required": ["questions"],
        }),
    };

    let mut ex = ExtractAction::new()
        .with_tool(read_tool)
        .with_tool(ask_tool);

    // ── 1. Batch: no tool calls → the whole text is one Text item ──────────
    show_items(
        "batch: plain text only",
        ex.extract_batch("Let me look at the project structure first.").items,
    );

    // ── 2. Batch: one inline call, prose around it ──────────────────────────
    show_items(
        "batch: one inline tool call",
        ex.extract_batch(
            r#"I'll read the file. {"name": "fs.read", "arguments": {"path": "src/lib.rs"}}"#,
        )
        .items,
    );

    // ── 3. Batch: several calls interleaved with text, in order ─────────────
    show_items(
        "batch: multiple interleaved calls",
        ex.extract_batch(
            r#"First {"name": "fs.read", "arguments": {"path": "a.rs"}} then {"name": "fs.read", "arguments": {"path": "b.rs"}} done"#,
        )
        .items,
    );

    // ── 4. Envelope aliases: `input`/`args`/`parameters` and string args ────
    // `input` is accepted in place of `arguments`; OpenAI-style string
    // arguments are parsed as JSON before validation.
    show_items(
        "batch: envelope aliases",
        ex.extract_batch(r#"{"name": "fs.read", "input": {"path": "/x"}}"#)
            .items,
    );
    show_items(
        "batch: string-encoded arguments",
        ex.extract_batch(
            r#"{"name": "fs.read", "arguments": "{\"path\": \"/y\"}"}"#,
        )
        .items,
    );

    // ── 5. Bare-arguments fallback ──────────────────────────────────────────
    // No name field, but the object matches exactly one registered schema.
    show_items(
        "batch: bare arguments",
        ex.extract_batch(
            r#"{"questions": [{"id": "1", "question": "OK?", "type": "Text"}]}"#,
        )
        .items,
    );

    // ── 6. Validation failures are suppressed, not shown ────────────────────
    // Unknown tool name / missing required field / wrong arg type → the
    // failure warning text replaces the JSON in the output.
    show_items(
        "batch: unknown tool name",
        ex.extract_batch(r#"{"name": "nope", "arguments": {"path": "/x"}}"#)
            .items,
    );
    show_items(
        "batch: missing required field",
        ex.extract_batch(r#"{"name": "fs.read", "arguments": {}}"#).items,
    );
    println!(
        "== failure accounting ==\n  failures since last take: {}\n  last failed raw: {:?}\n",
        ex.take_tool_failures(),
        ex.take_last_failed_raw(),
    );

    // ── 7. Fenced JSON is display text, not a tool call ─────────────────────
    show_items(
        "batch: fenced code block",
        ex.extract_batch(
            "Example:\n```json\n{\"name\": \"fs.read\", \"arguments\": {\"path\": \"/x\"}}\n```\n",
        )
        .items,
    );

    // ── 8. find_json_objects — the standalone scanner ───────────────────────
    let spans = find_json_objects(r#"a {"x": 1} b {"y": {"z": 2}} c"#);
    println!("== find_json_objects ==  {spans:?}\n");

    // ── 9. Streaming: text, pending, then a complete call ───────────────────
    let mut stream = ExtractAction::new()
        .with_tool(ToolSchema {
            name: "fs.read".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"],
            }),
        });
    let mut actions = Vec::new();
    for token in [
        "Reading ",
        "the ",
        "file ",
        r#"{"name": "fs.read", "arguments": {"path": "/z"}}"#,
    ] {
        actions.push(stream.extract_stream(token));
    }
    show_stream("stream: split across tokens", actions);

    // ── 10. Streaming: explanatory JSON fails fast (early exit) ─────────────
    let mut stream = ExtractAction::new().with_tool(ToolSchema {
        name: "fs.read".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"],
        }),
    });
    let mut actions = Vec::new();
    for token in [
        "I think ",
        r#"{"result": "sure""#,
        " the answer is 42",
    ] {
        actions.push(stream.extract_stream(token));
    }
    show_stream("stream: explanatory JSON early-exits", actions);

    // ── 11. jsonish directly: markdown fence + malformed JSON ───────────────
    use cosh_sdk::extract_action::{ParseOptions, jsonish};
    let value = jsonish::parse(
        "```json\n{\"name\": \"fs.read\", \"arguments\": {\"path\": \"/t\"}}\n```",
        ParseOptions::default(),
        true,
    )
    .unwrap();
    println!("== jsonish: fenced JSON ==\n  {value:?}\n");
    let value = jsonish::parse(
        r#"{"name": "fs.read", "arguments": {"path": "/t",}}"#, // trailing comma
        ParseOptions::default(),
        true,
    )
    .unwrap();
    println!("== jsonish: trailing comma fixed ==\n  {value:?}\n");

    // ── 12. Round-trip a ToolCallData through the API ───────────────────────
    let out = ex
        .extract_batch(r#"{"name": "fs.read", "arguments": {"path": "x"}}"#);
    if let Item::ToolCall(call) = &out.items[0] {
        let ToolCallData { id, name, arguments, thought_signature } = call;
        println!(
            "== ToolCallData fields ==\n  id={id:?} name={name:?} arguments={arguments} thought_signature={thought_signature:?}"
        );
    }
}
