use crate::extract_action::{ParseOptions, jsonish};
use serde_json::Value as JsonValue;

/// Schema for a registered tool.
#[derive(Debug, Clone)]
pub struct ToolSchema {
    pub name: String,
    pub input_schema: JsonValue,
}

/// A validated tool call extracted from LLM output.
#[derive(Debug, Clone)]
pub struct ToolCallData {
    pub name: String,
    pub arguments: JsonValue,
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

#[derive(Default)]
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
            tool_failure_message: "\n\n> ⚠ Tool call failure\n\n".to_string(),
            tool_failure_count: 0,
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

    /// Set the message emitted when a tool call is detected but fails validation.
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

    /// Set the message emitted when a tool call is detected but fails validation.
    pub fn set_tool_failure_message<S: Into<String>>(&mut self, msg: S) {
        self.tool_failure_message = msg.into();
    }

    /// Drain the count of tool call failures since the last call to `take_tool_failures`.
    #[must_use]
    pub fn take_tool_failures(&mut self) -> usize {
        std::mem::take(&mut self.tool_failure_count)
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
            }

            if let Some(tool_call) = self.parse_and_validate(candidate) {
                items.push(Item::ToolCall(tool_call));
            } else {
                // Invalid unfenced JSON — suppress, show warning instead
                self.tool_failure_count += 1;
                items.push(Item::Text(self.tool_failure_message.clone()));
            }

            last_end = end + 1;
        }

        if last_end < text.len() {
            items.push(Item::Text(text[last_end..].to_string()));
        }

        BatchResult { items }
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
                    if let Some(call) = self.parse_and_validate(&buffer) {
                        if next < input.len() {
                            self.state.deferred_tail = input[next..].to_string();
                        }
                        if output.is_empty() {
                            return StreamAction::ToolCall(call);
                        }
                        self.state.pending_tool_call = Some(call);
                        return StreamAction::Text(std::mem::take(&mut output));
                    }
                    self.tool_failure_count += 1;
                    output.push_str(&self.tool_failure_message);
                } else if self.check_early_exit() {
                    self.tool_failure_count += 1;
                    output.push_str(&self.tool_failure_message);
                    self.state = StreamState::default();
                }
            } else if ch == '`' {
                self.state.fence_count += 1;
                output.push('`');
            } else {
                if self.state.fence_count >= 3 {
                    self.state.in_code_block = !self.state.in_code_block;
                }
                self.state.fence_count = 0;

                if self.state.in_code_block {
                    output.push(ch);
                    continue;
                }

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
                ':' => {
                    if self.state.depth == 1_i32 {
                        if let Some(ref key) = self.state.pending_key.take()
                            && !is_known_key(key, &self.tool_keys)
                        {
                            self.state.early_exit = true;
                        }
                        self.state.depth1_state = Some(Depth1State::InValue);
                    }
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

    fn parse_and_validate(&self, candidate: &str) -> Option<ToolCallData> {
        if let Ok(value) = serde_json::from_str::<JsonValue>(candidate)
            && let Some(call) = validate_tool_call(&value, &self.tools)
        {
            return Some(call);
        }

        if let Ok(value) = jsonish::parse(candidate, ParseOptions::default(), true)
            && let Some(json) = jsonish_value_to_json(&value)
            && let Some(call) = validate_tool_call(&json, &self.tools)
        {
            return Some(call);
        }

        None
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

fn validate_tool_call(value: &JsonValue, tools: &[ToolSchema]) -> Option<ToolCallData> {
    let obj = value.as_object()?;

    // Standard tool call envelope: {"name": "...", "arguments": {...}}
    if let Some(name) = obj
        .get("name")
        .or_else(|| obj.get("tool"))
        .or_else(|| obj.get("function"))
        .and_then(|v| v.as_str())
    {
        let tool = tools.iter().find(|t| t.name == name)?;

        let args = obj
            .get("arguments")
            .or_else(|| obj.get("input"))
            .or_else(|| obj.get("args"))
            .or_else(|| obj.get("parameters"))?;

        if !validate_against_schema(args, &tool.input_schema) {
            return None;
        }

        return Some(ToolCallData {
            name: name.to_string(),
            arguments: args.clone(),
        });
    }

    // Bare-arguments fallback: if the JSON has no name/tool/function field
    // but matches exactly one registered tool's input schema, treat it
    // as a bare tool call (arguments only). This catches cases where the
    // LLM outputs the arguments directly without an envelope.
    let mut matches: Vec<&ToolSchema> = Vec::new();
    for tool in tools {
        if validate_against_schema(value, &tool.input_schema) {
            matches.push(tool);
        }
    }
    if matches.len() == 1 {
        return Some(ToolCallData {
            name: matches[0].name.clone(),
            arguments: value.clone(),
        });
    }

    None
}

fn validate_against_schema(value: &JsonValue, schema: &JsonValue) -> bool {
    // Handle oneOf: value must match at least one sub-schema
    if let Some(one_of) = schema.get("oneOf").and_then(|o| o.as_array()) {
        return one_of.iter().any(|sub| validate_against_schema(value, sub));
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

    if let Some(properties) = schema.get("properties").and_then(|p| p.as_object()) {
        for (field_name, field_schema) in properties {
            if let Some(field_value) = obj.get(field_name) {
                if field_schema.get("oneOf").is_some() {
                    if !validate_against_schema(field_value, field_schema) {
                        return false;
                    }
                } else {
                    if let Some(const_val) = field_schema.get("const") {
                        if field_value != const_val {
                            return false;
                        }
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
