use crate::extract_action::{ParseOptions, jsonish};
use serde_json::Value as JsonValue;

/// Schema for a registered tool.
#[derive(Debug, Clone)]
pub struct ToolSchema {
    pub name: String,
    pub input_schema: JsonValue,
    /// Canonical, hand-curated example of valid arguments. When present,
    /// schema-rejection hints show this example instead of the derived
    /// type-only skeleton, so the model can copy a complete, correct
    /// payload (optional fields, conditional rules and all) on retry.
    pub example_args: Option<JsonValue>,
}

/// A validated tool call extracted from LLM output.
#[derive(Debug, Clone)]
pub struct ToolCallData {
    /// Tool call ID from the API's native `tool_calls` mechanism.
    /// Empty string when the call was extracted from inline JSON text.
    pub id: String,
    pub name: String,
    pub arguments: JsonValue,
    /// Gemini 3.x thought signature: the sibling `thoughtSignature` the
    /// model attached to a native `functionCall` part. MUST be replayed
    /// verbatim when the call is re-sent in the conversation history — the
    /// Gemini API rejects a functionCall without its original signature with
    /// HTTP 400 (`Function call is missing a thought_signature`). Empty for
    /// inline-JSON calls and for every other provider.
    pub thought_signature: String,
}

/// A tool call the provider delivered NATIVELY (structured `tool_calls` /
/// `tool_use` / `functionCall` parts), handed to the extractor for the SAME
/// schema validation the inline-JSON text path performs.
///
/// The provider already split the call into structured fields, so no text
/// parsing happens — but the envelope is still validated against the
/// registered schemas, failures are counted, and `last_failed_raw` is
/// recorded for the correction memory, exactly like a failed inline call.
#[derive(Debug, Clone)]
pub struct NativeToolCall {
    /// Tool call ID from the API's native mechanism.
    pub id: String,
    /// Tool name.
    pub name: String,
    /// Raw accumulated `arguments` JSON text as the provider streamed it
    /// (empty when the provider sent no arguments at all — e.g. Ollama).
    pub arguments: String,
    /// Gemini 3.x thought signature (sibling of a native `functionCall`),
    /// replayed verbatim on the follow-up request. Empty for every other
    /// provider.
    pub thought_signature: String,
}

/// Action returned by [`ExtractAction::extract_stream`].
#[derive(Debug)]
pub enum StreamAction {
    /// Emit this text to the user.
    Text(String),
    /// A complete tool call was detected and validated.
    ToolCall(ToolCallData),
    /// Still buffering a potential tool call; emit nothing yet.
    Pending,
}

/// One item in a [`BatchResult`]: either plain text or a tool call.
#[derive(Debug)]
pub enum Item {
    Text(String),
    ToolCall(ToolCallData),
}

/// Why a candidate tool call was rejected during validation.
///
/// Carried in the failure message so the model learns *what* went wrong
/// (which tool, which part of the envelope, and whether the tool even exists
/// in the current mode) instead of only "something failed".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCallRejection {
    /// The candidate was not a JSON object at the top level.
    NotAnObject,
    /// The tool name is present but not registered: unknown, disabled, or not
    /// exposed in the current mode.
    UnknownTool { name: String },
    /// A recognized tool name but no `arguments` / `input` / `args` /
    /// `parameters` field was found.
    MissingArguments { name: String },
    /// A recognized tool name but the parsed arguments do not satisfy the
    /// tool's input schema.
    SchemaMismatch { name: String },
    /// No name, and the bare-arguments fallback matched zero or many tools.
    NoToolMatch,
    /// The candidate text could not be parsed as JSON at all.
    ParseError,
    /// The object is well-formed but does not look like a tool call (e.g. an
    /// unrecognized top-level key aborted streaming buffering).
    NotAToolCall,
}

impl ToolCallRejection {
    /// Human-readable, model-facing description of this rejection.
    fn describe(&self) -> String {
        match self {
            Self::NotAnObject => "the tool call is not a JSON object".to_string(),
            Self::UnknownTool { name } => {
                format!("tool `{name}` is not available in this mode")
            }
            Self::MissingArguments { name } => {
                format!("tool `{name}` is missing its `arguments` block")
            }
            Self::SchemaMismatch { name } => {
                format!("invalid arguments for tool `{name}`")
            }
            Self::NoToolMatch => "could not tell which tool was meant".to_string(),
            Self::ParseError => "could not parse the tool call".to_string(),
            Self::NotAToolCall => "the object does not look like a tool call".to_string(),
        }
    }
}

/// Result of a batch extraction.
#[derive(Debug)]
pub struct BatchResult {
    pub items: Vec<Item>,
}

#[derive(Clone, Copy, PartialEq)]
enum Depth1State {
    ExpectKey,
    InValue,
}

#[allow(clippy::struct_excessive_bools)]
struct StreamState {
    buffer: String,
    pending_tool_call: Option<ToolCallData>,
    deferred_tail: String,
    depth: i32,
    in_string: bool,
    escape: bool,
    depth1_state: Option<Depth1State>,
    current_key: String,
    pending_key: Option<String>,
    early_exit: bool,
    in_code_block: bool,
    fence_count: usize,
    /// Whether the next character to be consumed is at the start of a line
    /// (the previously consumed character was a newline, or we are at the start
    /// of the stream). Markdown fences only open/close at a line start.
    at_line_start: bool,
    /// Whether the current run of backticks (if any) began at a line start.
    /// Captured when the first backtick of a run is seen so a mid-line ````
    /// (e.g. prose that merely *quotes* a fence) never toggles the code-block
    /// state — doing so would swallow a following inline tool call as "fenced
    /// display text" and leak its schema to the user.
    fence_at_line_start: bool,
}

impl Default for StreamState {
    fn default() -> Self {
        Self {
            buffer: String::new(),
            pending_tool_call: None,
            deferred_tail: String::new(),
            depth: 0,
            in_string: false,
            escape: false,
            depth1_state: None,
            current_key: String::new(),
            pending_key: None,
            early_exit: false,
            in_code_block: false,
            fence_count: 0,
            at_line_start: true,
            fence_at_line_start: false,
        }
    }
}

const KNOWN_KEYS: &[&str] = &[
    "name",
    "arguments",
    "input",
    "args",
    "function",
    "tool",
    "type",
    "id",
    "tool_call_id",
    "parameters",
    "description",
    "thought_signature",
];

fn is_known_key(key: &str, tool_keys: &[String]) -> bool {
    KNOWN_KEYS.contains(&key) || tool_keys.iter().any(|k| k == key)
}

/// Extractor that detects and validates tool calls embedded in LLM text output.
///
/// Supports both batch (full-text) and streaming (token-by-token) modes.
/// Validation is schema-driven: tool calls must match both the expected envelope
/// structure and the per-tool input schema.
pub struct ExtractAction {
    tools: Vec<ToolSchema>,
    tool_keys: Vec<String>,
    state: StreamState,
    tool_failure_message: String,
    tool_failure_count: usize,
    /// Stores the raw JSON of the most recent failed tool call attempt
    /// so the correction memory can give the model specific feedback.
    last_failed_raw: String,
}

fn extract_tool_property_keys(schema: &JsonValue) -> Vec<String> {
    let mut keys = Vec::new();
    if let Some(properties) = schema.get("properties").and_then(|p| p.as_object()) {
        for key in properties.keys() {
            keys.push(key.clone());
        }
    }
    keys
}

impl ExtractAction {
    /// Create an empty extractor with no registered tools.
    ///
    /// Use [`add_tool`](Self::add_tool) or [`with_tool`](Self::with_tool) to register schemas.
    #[must_use]
    pub fn new() -> Self {
        Self {
            tools: Vec::new(),
            tool_keys: Vec::new(),
            state: StreamState::default(),
            tool_failure_message: "\n\n> ⚠ Tool call failure".to_string(),
            tool_failure_count: 0,
            last_failed_raw: String::new(),
        }
    }

    /// Register a tool schema (builder-style).
    #[must_use]
    pub fn with_tool(mut self, schema: ToolSchema) -> Self {
        self.tool_keys
            .extend(extract_tool_property_keys(&schema.input_schema));
        self.tools.push(schema);
        self
    }

    /// Set the failure label emitted when a tool call is detected but fails
    /// validation. The rejection reason and the offending raw payload are
    /// always appended to this label.
    #[must_use]
    pub fn with_tool_failure_message<S: Into<String>>(mut self, msg: S) -> Self {
        self.tool_failure_message = msg.into();
        self
    }

    /// Register a tool schema.
    pub fn add_tool(&mut self, schema: ToolSchema) {
        self.tool_keys
            .extend(extract_tool_property_keys(&schema.input_schema));
        self.tools.push(schema);
    }

    /// Set the failure label emitted when a tool call is detected but fails
    /// validation. The rejection reason and the offending raw payload are
    /// always appended to this label.
    pub fn set_tool_failure_message<S: Into<String>>(&mut self, msg: S) {
        self.tool_failure_message = msg.into();
    }

    /// Drain the count of tool call failures since the last call to `take_tool_failures`.
    #[must_use]
    pub fn take_tool_failures(&mut self) -> usize {
        std::mem::take(&mut self.tool_failure_count)
    }

    /// Drop the streaming buffer and any half-parsed tool call. Called by
    /// the harness when the SDK retries a mid-stream failure: the partial
    /// text of the failed attempt must not leak into the retried response
    /// (which restarts from the beginning).
    pub fn reset_stream_state(&mut self) {
        self.state = StreamState::default();
        self.last_failed_raw.clear();
    }

    /// Drain the raw JSON of the last failed tool call attempt.
    #[must_use]
    pub fn take_last_failed_raw(&mut self) -> String {
        std::mem::take(&mut self.last_failed_raw)
    }

    /// Render the failure warning for a rejected tool call: the configured
    /// label, the human-readable rejection reason, the truncated offending
    /// raw payload, and — for schema-shaped rejections — the expected argument
    /// shape (or the available tool set, for unknown-tool calls). Every
    /// failure path (batch, streaming, and native) funnels through this so
    /// the model always sees *what* failed, *why*, and what to send instead.
    fn failure_message(&self, reason: &ToolCallRejection, raw: &str) -> String {
        let mut out = format!("{}: {}\n", self.tool_failure_message, reason.describe());
        if !raw.is_empty() {
            out.push_str(&format!("> Rejected call: `{}`\n", truncate_payload(raw)));
        }
        let hint = self.schema_hint_line(reason);
        if hint.is_empty() {
            out.push('\n');
        } else {
            out.push_str(&hint);
        }
        out
    }

    /// The "teach the shape" line for a rejection: the tool's minimal
    /// argument example for schema failures, the registered tool set for
    /// unknown-tool calls. Empty when there is nothing to teach.
    fn schema_hint_line(&self, reason: &ToolCallRejection) -> String {
        match reason {
            ToolCallRejection::SchemaMismatch { name }
            | ToolCallRejection::MissingArguments { name } => self
                .tools
                .iter()
                .find(|t| &t.name == name)
                .map(|t| {
                    // A curated example teaches the full contract (optional
                    // fields, exact values, conditional rules); the derived
                    // skeleton only renders types and cannot express
                    // conditionally-required fields.
                    if let Some(example) = &t.example_args {
                        // Same cap as the rejected-payload echo: an MCP-supplied
                        // example must never flood the context on a failure streak.
                        return format!(
                            "> Expected `{}` arguments, e.g.: {}\n",
                            t.name,
                            truncate_payload(&example.to_string())
                        );
                    }
                    format!(
                        "> Expected `{}` arguments — minimal shape: {}\n",
                        t.name,
                        super::skeleton::schema_skeleton(&t.input_schema)
                    )
                })
                .unwrap_or_default(),
            ToolCallRejection::UnknownTool { .. } if !self.tools.is_empty() => {
                const MAX_LISTED: usize = 15;
                let names: Vec<&str> = self.tools.iter().map(|t| t.name.as_str()).collect();
                let (shown, more) = if names.len() > MAX_LISTED {
                    (
                        &names[..MAX_LISTED],
                        format!(" (+{} more)", names.len() - MAX_LISTED),
                    )
                } else {
                    (&names[..], String::new())
                };
                format!("> Available tools: {}{more}\n", shown.join(", "))
            }
            _ => String::new(),
        }
    }

    /// Process a complete text and extract all embedded tool calls.
    ///
    /// Returns the text split into alternating [`Item::Text`] and [`Item::ToolCall`]
    /// segments in their original order.
    #[must_use]
    pub fn extract_batch(&mut self, text: &str) -> BatchResult {
        let objects = find_json_objects(text);
        if objects.is_empty() {
            return BatchResult {
                items: vec![Item::Text(text.to_string())],
            };
        }

        let mut items = Vec::new();
        let mut last_end = 0;

        for (start, end) in objects {
            if start > last_end {
                items.push(Item::Text(text[last_end..start].to_string()));
            }

            let candidate = &text[start..=end];

            // Inside a fenced code block → display JSON, keep as text
            if is_in_code_block(text, start) {
                items.push(Item::Text(candidate.to_string()));
                last_end = end + 1;
                continue;
            } else {
                match self.parse_and_validate(candidate) {
                    Ok(tool_call) => items.push(Item::ToolCall(tool_call)),
                    Err(reason) => {
                        // Invalid unfenced JSON — suppress, show warning instead
                        self.tool_failure_count += 1;
                        self.last_failed_raw = candidate.to_string();
                        items.push(Item::Text(self.failure_message(&reason, candidate)));
                    }
                }
            }

            last_end = end + 1;
        }

        if last_end < text.len() {
            items.push(Item::Text(text[last_end..].to_string()));
        }

        BatchResult { items }
    }

    /// Validate a tool call the provider delivered NATIVELY (structured
    /// `tool_calls`/`tool_use`/`functionCall` parts) through the same schema
    /// funnel as the inline-JSON text path — minus the text parsing.
    ///
    /// Empty arguments default to `{}` (some providers — e.g. Ollama —
    /// stream tool calls with no `arguments` at all); non-empty arguments
    /// are passed through as a JSON string so [`validate_tool_call`] can
    /// attempt a parse, falling back to schema rejection — the model gets
    /// the same correction feedback as an inline-JSON failure.
    ///
    /// Returns [`StreamAction::ToolCall`] when the call validates against a
    /// registered schema, [`StreamAction::Text`] (the tool-failure message)
    /// when it does not, or [`StreamAction::Pending`] when nothing applies.
    pub fn register_native_call(&mut self, call: &NativeToolCall) -> StreamAction {
        let mut obj = serde_json::Map::new();
        obj.insert("name".to_string(), JsonValue::String(call.name.clone()));
        // The provider's accumulated `arguments` JSON text, verbatim. Empty
        // arguments default to `{}` so a call with no payload still
        // validates against tools that take no required fields.
        let args = if call.arguments.trim().is_empty() {
            JsonValue::Object(serde_json::Map::new())
        } else {
            JsonValue::String(call.arguments.clone())
        };
        obj.insert("arguments".to_string(), args);
        if !call.id.is_empty() {
            obj.insert("id".to_string(), JsonValue::String(call.id.clone()));
        }
        if !call.thought_signature.is_empty() {
            obj.insert(
                "thought_signature".to_string(),
                JsonValue::String(call.thought_signature.clone()),
            );
        }
        let envelope = JsonValue::Object(obj);
        match validate_tool_call(&envelope, &self.tools) {
            Ok(tc) => StreamAction::ToolCall(tc),
            Err(reason) => {
                self.tool_failure_count += 1;
                self.last_failed_raw = envelope.to_string();
                StreamAction::Text(self.failure_message(&reason, &self.last_failed_raw))
            }
        }
    }

    /// Process a single token in streaming mode.
    ///
    /// Call this for each token/chunk as it arrives from the LLM stream.
    /// The extractor maintains internal state across calls.
    ///
    /// Returns [`StreamAction::ToolCall`] when a complete validated tool call is found,
    /// [`StreamAction::Text`] for text to forward to the user,
    /// or [`StreamAction::Pending`] when the extractor is still buffering.
    pub fn extract_stream(&mut self, token: &str) -> StreamAction {
        if let Some(call) = self.state.pending_tool_call.take() {
            return StreamAction::ToolCall(call);
        }

        let input = if self.state.deferred_tail.is_empty() {
            token.to_string()
        } else {
            let tail = std::mem::take(&mut self.state.deferred_tail);
            tail + token
        };

        let mut output = String::new();

        for (i, ch) in input.char_indices() {
            if self.state.depth > 0_i32 {
                self.state.buffer.push(ch);
                self.handle_json_char(ch);

                if self.state.depth == 0_i32 {
                    let buffer = std::mem::take(&mut self.state.buffer);
                    self.state = StreamState::default();

                    let next = i + ch.len_utf8();
                    match self.parse_and_validate(&buffer) {
                        Ok(call) => {
                            if next < input.len() {
                                self.state.deferred_tail = input[next..].to_string();
                            }
                            if output.is_empty() {
                                return StreamAction::ToolCall(call);
                            }
                            self.state.pending_tool_call = Some(call);
                            return StreamAction::Text(std::mem::take(&mut output));
                        }
                        Err(reason) => {
                            self.tool_failure_count += 1;
                            self.last_failed_raw = buffer;
                            output.push_str(&self.failure_message(&reason, &self.last_failed_raw));
                        }
                    }
                } else if self.check_early_exit() {
                    self.tool_failure_count += 1;
                    self.last_failed_raw = std::mem::take(&mut self.state.buffer);
                    output.push_str(
                        &self.failure_message(
                            &ToolCallRejection::NotAToolCall,
                            &self.last_failed_raw,
                        ),
                    );
                    self.state = StreamState::default();
                }
            } else if ch == '`' {
                // Fences only open/close when their opening backtick sits at the
                // start of a line (mirroring `is_in_code_block`). A ````
                // quoted in the middle of prose (e.g. an agent describing a
                // markdown snippet) must NOT flip the code-block state — doing
                // so would swallow a following inline tool call as "fenced
                // display text" and leak its schema to the user.
                if self.state.fence_count == 0 {
                    self.state.fence_at_line_start = self.state.at_line_start;
                }
                self.state.fence_count += 1;
                output.push('`');
                self.state.at_line_start = false;
            } else {
                if self.state.fence_count >= 3 && self.state.fence_at_line_start {
                    self.state.in_code_block = !self.state.in_code_block;
                }
                self.state.fence_count = 0;
                self.state.fence_at_line_start = false;

                if self.state.in_code_block {
                    output.push(ch);
                    self.state.at_line_start = ch == '\n';
                    continue;
                }

                self.state.at_line_start = ch == '\n';

                if ch == '{' {
                    if !output.is_empty() {
                        let text = std::mem::take(&mut output);
                        self.state.depth = 1_i32;
                        self.state.depth1_state = Some(Depth1State::ExpectKey);
                        self.state.buffer.push('{');
                        let next = i + ch.len_utf8();
                        if next < input.len() {
                            self.state.deferred_tail = input[next..].to_string();
                        }
                        return StreamAction::Text(text);
                    }
                    self.state.depth = 1_i32;
                    self.state.depth1_state = Some(Depth1State::ExpectKey);
                    self.state.buffer.push('{');
                } else {
                    output.push(ch);
                }
            }
        }

        if self.state.depth == 0_i32 && !output.is_empty() {
            return StreamAction::Text(output);
        }

        StreamAction::Pending
    }

    fn handle_json_char(&mut self, ch: char) {
        if self.state.in_string {
            if self.state.escape {
                self.state.escape = false;
            } else if ch == '\\' {
                self.state.escape = true;
            } else if ch == '"' {
                self.state.in_string = false;
                if self.state.depth == 1_i32
                    && self.state.depth1_state == Some(Depth1State::ExpectKey)
                {
                    self.state.pending_key = Some(std::mem::take(&mut self.state.current_key));
                }
            } else if self.state.depth == 1_i32
                && self.state.depth1_state == Some(Depth1State::ExpectKey)
            {
                self.state.current_key.push(ch);
            }
        } else {
            match ch {
                '{' | '[' => {
                    self.state.depth += 1_i32;
                    if self.state.depth == 1_i32 {
                        self.state.depth1_state = Some(Depth1State::ExpectKey);
                    } else if self.state.depth == 2_i32 {
                        self.state.depth1_state = Some(Depth1State::InValue);
                        self.state.pending_key = None;
                    }
                }
                '}' | ']' => {
                    self.state.depth -= 1_i32;
                    if self.state.depth == 1_i32 {
                        self.state.depth1_state = Some(Depth1State::InValue);
                    }
                }
                '"' => {
                    self.state.in_string = true;
                    self.state.escape = false;
                    if self.state.depth == 1_i32
                        && self.state.depth1_state == Some(Depth1State::ExpectKey)
                    {
                        self.state.current_key.clear();
                    }
                }
                ':' if self.state.depth == 1_i32 => {
                    if let Some(ref key) = self.state.pending_key.take()
                        && !is_known_key(key, &self.tool_keys)
                    {
                        self.state.early_exit = true;
                    }
                    self.state.depth1_state = Some(Depth1State::InValue);
                }
                ',' if self.state.depth == 1_i32 => {
                    self.state.depth1_state = Some(Depth1State::ExpectKey);
                }
                _ => {}
            }
        }
    }

    const fn check_early_exit(&mut self) -> bool {
        std::mem::replace(&mut self.state.early_exit, false)
    }

    fn parse_and_validate(&self, candidate: &str) -> Result<ToolCallData, ToolCallRejection> {
        // Parse strictly first, then leniently: a well-formed JSON object that
        // only fails a schema check must surface that rejection (not a
        // misleading parse error), so the model learns the actual reason.
        let mut parsed = false;
        let mut last_rejection = ToolCallRejection::ParseError;

        if let Ok(value) = serde_json::from_str::<JsonValue>(candidate) {
            parsed = true;
            match validate_tool_call(&value, &self.tools) {
                ok @ Ok(_) => return ok,
                Err(reason) => last_rejection = reason,
            }
        }

        if let Ok(value) = jsonish::parse(candidate, ParseOptions::default(), true)
            && let Some(json) = jsonish_value_to_json(&value)
        {
            parsed = true;
            match validate_tool_call(&json, &self.tools) {
                ok @ Ok(_) => return ok,
                Err(reason) => last_rejection = reason,
            }
        }

        if parsed {
            Err(last_rejection)
        } else {
            Err(ToolCallRejection::ParseError)
        }
    }
}

impl Default for ExtractAction {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[must_use]
pub fn find_json_objects(text: &str) -> Vec<(usize, usize)> {
    let mut results = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'{' {
            let start = i;
            let mut depth = 1i32;
            let mut in_string = false;
            let mut escaped = false;
            i += 1;

            while i < bytes.len() && depth > 0_i32 {
                let c = bytes[i];
                if in_string {
                    if escaped {
                        escaped = false;
                    } else if c == b'\\' {
                        escaped = true;
                    } else if c == b'"' {
                        in_string = false;
                    }
                } else {
                    match c {
                        b'"' => in_string = true,
                        b'{' => depth += 1_i32,
                        b'}' => depth -= 1_i32,
                        _ => {}
                    }
                }
                i += 1;
            }

            if depth == 0_i32 {
                results.push((start, i - 1));
            }
        } else {
            i += 1;
        }
    }

    results
}

/// Convert a [`jsonish::Value`] to a [`serde_json::Value`].
fn jsonish_value_to_json(value: &jsonish::Value) -> Option<JsonValue> {
    match value {
        jsonish::Value::String(s, _) => Some(JsonValue::String(s.clone())),
        jsonish::Value::Number(n, _) => Some(JsonValue::Number(n.clone())),
        jsonish::Value::Boolean(b) => Some(JsonValue::Bool(*b)),
        jsonish::Value::Null => Some(JsonValue::Null),
        jsonish::Value::Object(fields, _) => {
            let mut map = serde_json::Map::new();
            for (k, v) in fields {
                if let Some(jv) = jsonish_value_to_json(v) {
                    map.insert(k.clone(), jv);
                }
            }
            Some(JsonValue::Object(map))
        }
        jsonish::Value::Array(items, _) => {
            let mut arr = Vec::new();
            for item in items {
                if let Some(jv) = jsonish_value_to_json(item) {
                    arr.push(jv);
                }
            }
            Some(JsonValue::Array(arr))
        }
        jsonish::Value::Markdown(_, inner, _) | jsonish::Value::FixedJson(inner, _) => {
            jsonish_value_to_json(inner)
        }
        jsonish::Value::AnyOf(items, _) => items.iter().find_map(jsonish_value_to_json),
    }
}

fn validate_tool_call(
    value: &JsonValue,
    tools: &[ToolSchema],
) -> Result<ToolCallData, ToolCallRejection> {
    let obj = value.as_object().ok_or(ToolCallRejection::NotAnObject)?;

    // Extract the tool call ID (from API's native mechanism or synthetic).
    let id = obj
        .get("id")
        .or_else(|| obj.get("tool_call_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // Gemini 3.x thought signature (sibling of a native functionCall).
    let thought_signature = obj
        .get("thought_signature")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // Standard tool call envelope: {"name": "...", "arguments": {...}}
    if let Some(name) = obj
        .get("name")
        .or_else(|| obj.get("tool"))
        .or_else(|| obj.get("function"))
        .and_then(|v| v.as_str())
    {
        // A recognized name that is not registered (unknown / disabled / not
        // exposed in the current mode) gets its own rejection so the model can
        // tell "tool unavailable" apart from "invalid arguments".
        let tool = tools.iter().find(|t| t.name == name).ok_or_else(|| {
            ToolCallRejection::UnknownTool {
                name: name.to_string(),
            }
        })?;

        let args_ref = obj
            .get("arguments")
            .or_else(|| obj.get("input"))
            .or_else(|| obj.get("args"))
            .or_else(|| obj.get("parameters"))
            .ok_or_else(|| ToolCallRejection::MissingArguments {
                name: name.to_string(),
            })?;

        // Many LLMs output arguments as a JSON-encoded string (OpenAI-style).
        // Try to parse it as JSON so we can validate the actual object.
        let args = match args_ref {
            JsonValue::String(s) => serde_json::from_str(s).unwrap_or_else(|_| args_ref.clone()),
            _ => args_ref.clone(),
        };
        // Models sometimes double-encode individual fields while the
        // envelope itself is fine (`"questions": "[{...}]"`). Coerce every
        // string-encoded array/object back into its structured form before
        // validating — and return the coerced value so downstream consumers
        // deserialize the repaired payload, not the raw string.
        let args = repair_string_encoded(&args, &tool.input_schema, &tool.input_schema);

        if !validate_against_schema(&args, &tool.input_schema, &tool.input_schema) {
            return Err(ToolCallRejection::SchemaMismatch {
                name: name.to_string(),
            });
        }

        return Ok(ToolCallData {
            id,
            name: name.to_string(),
            arguments: args,
            thought_signature,
        });
    }

    // Bare-arguments fallback: if the JSON has no name/tool/function field
    // but matches exactly one registered tool's input schema, treat it
    // as a bare tool call (arguments only). This catches cases where the
    // LLM outputs the arguments directly without an envelope.
    let mut matches: Vec<&ToolSchema> = Vec::new();
    for tool in tools {
        // Same repair as the envelope path: a bare call with a
        // string-encoded field must recover identically to the enveloped
        // form, instead of failing schema validation here.
        if validate_against_schema(
            &repair_string_encoded(value, &tool.input_schema, &tool.input_schema),
            &tool.input_schema,
            &tool.input_schema,
        ) {
            matches.push(tool);
        }
    }
    if matches.len() == 1 {
        let repaired =
            repair_string_encoded(value, &matches[0].input_schema, &matches[0].input_schema);
        return Ok(ToolCallData {
            id,
            name: matches[0].name.clone(),
            arguments: repaired,
            thought_signature,
        });
    }

    Err(ToolCallRejection::NoToolMatch)
}

/// Resolve a local `$ref` (`#/$defs/name`) against the document root — the
/// pointer form the pipeline tool schemas use (`allOf: [{$ref: '#/$defs/step'}]`).
///
/// Fail-open by design: an unresolvable pointer yields the raw `{$ref}`
/// schema, which repairs nothing and validates as anything — the tool's own
/// deserializer still type-checks downstream, and a rejection hint beats a
/// dropped valid call.
fn resolve_ref<'a>(schema: &'a JsonValue, root: &'a JsonValue) -> Option<&'a JsonValue> {
    let path = schema.get("$ref")?.as_str()?.strip_prefix("#/")?;
    let mut current = root;
    for segment in path.split('/') {
        current = current.get(segment)?;
    }
    Some(current)
}

/// Cap on `$ref` hops followed per dispatch, mirroring the same cap in
/// `skeleton.rs`: a `serde_json::Value` is a finite tree, but a third-party
/// MCP schema may declare a cyclic `$ref` (`$defs/a → $defs/b → $defs/a`),
/// which must hit the cap instead of recursing forever.
const MAX_REF_HOPS: usize = 4;

/// The schema that actually describes THIS level: a `$ref` followed, and —
/// when the level carries no properties of its own — the first `allOf`
/// branch (the `allOf: [{$ref: '#/$defs/step'}]` idiom the pipeline tool
/// schemas use, same convention [`schema_skeleton`] renders). Without this
/// dispatch, `repair_string_encoded` sees a properties-less top level and
/// repairs nothing, so every nested field defect reaches the tool's
/// deserializer raw.
///
/// The dispatch never takes an `allOf` branch away from a level that ALSO
/// carries `oneOf`/`anyOf`/`properties`: those keywords belong to THIS
/// level, and replacing it with a branch would drop their semantics.
fn effective_schema<'a>(schema: &'a JsonValue, root: &'a JsonValue) -> &'a JsonValue {
    effective_schema_bounded(schema, root, 0)
}

fn effective_schema_bounded<'a>(
    schema: &'a JsonValue,
    root: &'a JsonValue,
    ref_hops: usize,
) -> &'a JsonValue {
    let resolved = if ref_hops >= MAX_REF_HOPS {
        // Cap reached: hand back the level as-is rather than follow another
        // hop — the schema's own `type`/`properties` (if any) still apply.
        schema
    } else {
        resolve_ref(schema, root).unwrap_or(schema)
    };
    if resolved.get("properties").is_none()
        && resolved.get("oneOf").is_none()
        && resolved.get("anyOf").is_none()
        && let Some(first) = resolved
            .get("allOf")
            .and_then(JsonValue::as_array)
            .and_then(|branches| branches.first())
    {
        return effective_schema_bounded(first, root, ref_hops + 1);
    }
    resolved
}

/// Coerce string-encoded fields back into their structured form, guided by
/// `schema`. Recurses into object properties and array items; a string value
/// in a position whose schema says `array`/`object` is parsed as JSON and
/// kept only when it parses to the expected kind, and a stringified scalar
/// (`"numeric_value": "70"`) is parsed into the schema's number/boolean
/// type. This repairs the common model mistakes of double-encoding a single
/// field (`{"questions": "[{...}]"}`) or quoting a scalar — without burning
/// a round-trip on the model. A string that cannot take the schema's type
/// stays a string and fails validation with the schema hint.
fn repair_string_encoded(value: &JsonValue, schema: &JsonValue, root: &JsonValue) -> JsonValue {
    let schema = effective_schema(schema, root);
    if let JsonValue::String(s) = value
        && let Some(kind) = schema.get("type").and_then(JsonValue::as_str)
    {
        match kind {
            "array" | "object" => {
                if let Ok(parsed) = serde_json::from_str::<JsonValue>(s)
                    && value_type_matches(&parsed, kind)
                {
                    return repair_string_encoded(&parsed, schema, root);
                }
            }
            "number" | "integer" => {
                if let Ok(n) = s.parse::<serde_json::Number>()
                    && value_type_matches(&JsonValue::Number(n.clone()), kind)
                {
                    return JsonValue::Number(n);
                }
            }
            "boolean" if s == "true" || s == "false" => {
                return JsonValue::Bool(s == "true");
            }
            _ => {}
        }
    }

    match value {
        JsonValue::Object(map) => {
            let properties = schema.get("properties").and_then(JsonValue::as_object);
            let mut out = serde_json::Map::new();
            for (key, val) in map {
                let repaired = match properties.and_then(|p| p.get(key)) {
                    Some(prop_schema) => repair_string_encoded(val, prop_schema, root),
                    None => val.clone(),
                };
                out.insert(key.clone(), repaired);
            }
            JsonValue::Object(out)
        }
        JsonValue::Array(items) => {
            let item_schema = schema.get("items");
            JsonValue::Array(
                items
                    .iter()
                    .map(|item| match item_schema {
                        Some(item_schema) => repair_string_encoded(item, item_schema, root),
                        None => item.clone(),
                    })
                    .collect(),
            )
        }
        other => other.clone(),
    }
}

fn validate_against_schema(value: &JsonValue, schema: &JsonValue, root: &JsonValue) -> bool {
    // Handle oneOf: value must match at least one sub-schema
    if let Some(one_of) = schema.get("oneOf").and_then(|o| o.as_array()) {
        return one_of
            .iter()
            .any(|sub| validate_against_schema(value, sub, root));
    }

    let JsonValue::Object(obj) = value else {
        return schema.get("type").is_none_or(|t| t == "null");
    };

    if let Some(typ) = schema.get("type").and_then(|t| t.as_str())
        && typ != "object"
    {
        return false;
    }

    if let Some(required) = schema.get("required").and_then(|r| r.as_array()) {
        for field in required {
            let Some(field_name) = field.as_str() else {
                continue;
            };
            if !obj.contains_key(field_name) {
                return false;
            }
        }
    }

    // The `allOf: [{$ref: '#/$defs/step'}]` idiom the pipeline tools use:
    // this level may carry no properties of its own while the real shape
    // (typed fields) lives behind the ref — follow it, so nested field
    // types are validated instead of passing through unchecked.
    let schema = effective_schema(schema, root);

    if let Some(properties) = schema.get("properties").and_then(|p| p.as_object()) {
        for (field_name, field_schema) in properties {
            if let Some(field_value) = obj.get(field_name) {
                if field_schema.get("oneOf").is_some() {
                    if !validate_against_schema(field_value, field_schema, root) {
                        return false;
                    }
                } else {
                    // A `$ref` field (`then: {$ref: '#/$defs/step'}`) has
                    // neither `const` nor `type` of its own — resolve it
                    // before the checks, or the value validates as anything.
                    let field_schema = effective_schema(field_schema, root);
                    if let Some(const_val) = field_schema.get("const")
                        && field_value != const_val
                    {
                        return false;
                    }
                    if let Some(expected_type) = field_schema.get("type").and_then(|t| t.as_str())
                        && !value_type_matches(field_value, expected_type)
                    {
                        return false;
                    }
                }
            }
        }
    }

    true
}

/// Check whether `byte_pos` falls inside a fenced code block (` ``` `).
fn is_in_code_block(text: &str, byte_pos: usize) -> bool {
    let prefix = &text[..byte_pos.min(text.len())];
    let bytes = prefix.as_bytes();
    let mut in_block = false;
    let mut i = 0;
    while i < bytes.len() {
        if i + 3 <= bytes.len()
            && bytes[i] == b'`'
            && bytes[i + 1] == b'`'
            && bytes[i + 2] == b'`'
            && (i == 0 || bytes[i - 1] == b'\n')
        {
            in_block = !in_block;
            i += 3;
        } else {
            i += 1;
        }
    }
    in_block
}

/// Maximum number of characters of the offending raw payload included in a
/// failure message. Long enough to identify the call, short enough to not
/// flood the context during a failure streak.
const FAILURE_PAYLOAD_MAX_CHARS: usize = 240;

/// Collapse newlines/tabs to single spaces and truncate to
/// [`FAILURE_PAYLOAD_MAX_CHARS`] so the failure message stays on one readable
/// blockquote line and never swallows the whole context with a huge payload.
fn truncate_payload(raw: &str) -> String {
    let collapsed: String = raw
        .chars()
        .map(|c| {
            if c == '\n' || c == '\r' || c == '\t' {
                ' '
            } else {
                c
            }
        })
        .collect();
    let chars: Vec<char> = collapsed.chars().collect();
    if chars.len() <= FAILURE_PAYLOAD_MAX_CHARS {
        collapsed
    } else {
        let head: String = chars[..FAILURE_PAYLOAD_MAX_CHARS].iter().collect();
        format!("{head}...")
    }
}

fn value_type_matches(value: &JsonValue, expected_type: &str) -> bool {
    match expected_type {
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.is_i64() || value.is_u64(),
        "boolean" => value.is_boolean(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        "null" => value.is_null(),
        _ => true,
    }
}
