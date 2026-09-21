use cosh_sdk::connector::{ToolDefinition, ToolFunction};
use cosh_sdk::extract_action::ToolSchema;
use rmcp::model::{
    CallToolResult, GetPromptResult, Prompt, PromptMessage, ReadResourceResult, Resource,
    ResourceTemplate, Tool,
};
use serde::{Deserialize, Serialize};

/// Cosh-side view of one MCP resource or resource template. Templates and
/// concrete resources share one shape so the TUI can list them uniformly,
/// but they are NOT interchangeable: a template's `uri` is its raw RFC 6570
/// form and `is_template` is true. Callers must expand it with
/// [`expand_resource_template`] (or match with [`match_resource_template`])
/// before reading — sending the raw `{var}` form to `resources/read` will
/// be rejected by the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceInfo {
    pub uri: String,
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub mime_type: Option<String>,
    /// True when `uri` is a raw RFC 6570 template needing expansion.
    pub is_template: bool,
}

/// Cosh-side view of one MCP prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptInfo {
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub arguments: Vec<PromptArgumentInfo>,
}

/// Cosh-side view of one prompt argument.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptArgumentInfo {
    pub name: String,
    pub description: Option<String>,
    pub required: bool,
}

/// Expose one MCP tool to the inline-JSON extractor.
pub fn tool_to_schema(tool: &Tool) -> ToolSchema {
    ToolSchema {
        name: tool.name.to_string(),
        input_schema: serde_json::Value::Object((*tool.input_schema).clone()),
        example_args: None,
    }
}

/// Expose one MCP tool to the native function-calling API. Tools without a
/// description omit the field instead of sending an empty string.
/// `title`, `annotations` (e.g. `destructive_hint`), `output_schema` and
/// `icons` are intentionally unmapped: the native function shape has no
/// fields for them. Wiring safety hints into permission gating is a
/// follow-up, not MVP scope.
pub fn tool_to_definition(tool: &Tool) -> ToolDefinition {
    let mut function = ToolFunction::new(tool.name.to_string());
    if let Some(description) = tool.description.as_deref()
        && !description.is_empty()
    {
        function = function.with_description(description);
    }
    function = function.with_parameters(serde_json::Value::Object((*tool.input_schema).clone()));
    ToolDefinition::new(function)
}

/// Render a tool result as plain text: text blocks joined by newline, falling
/// back to compact structured JSON when the server sent no text at all.
/// Non-text blocks (images, audio, resources) are out of scope for the
/// tools-first MVP and ignored. Callers must check [`is_tool_error`]
/// separately: error results render like successes here by design.
pub fn result_to_text(result: &CallToolResult) -> String {
    let text: Vec<String> = result
        .content
        .iter()
        .filter_map(|block| block.as_text().map(|text| text.text.clone()))
        .collect();
    if !text.is_empty() {
        return text.join("\n");
    }
    match &result.structured_content {
        Some(value) => serde_json::to_string(value).unwrap_or_default(),
        None => String::new(),
    }
}

/// Whether the server reported the tool call itself as failed.
pub const fn is_tool_error(result: &CallToolResult) -> bool {
    matches!(result.is_error, Some(true))
}

/// Expose one MCP resource to cosh: the identity, naming, and hints the TUI
/// and header need. Contents never appear here — those arrive per read.
pub fn resource_to_info(resource: &Resource) -> ResourceInfo {
    ResourceInfo {
        uri: resource.uri.clone(),
        name: resource.name.clone(),
        title: resource.title.clone(),
        description: resource.description.clone(),
        mime_type: resource.mime_type.clone(),
        is_template: false,
    }
}

/// Expose one RFC 6570 resource template. `uri_template` is kept raw and
/// `is_template` is true: the caller must expand it with
/// [`expand_resource_template`] before calling `resources/read`.
pub fn template_to_info(template: &ResourceTemplate) -> ResourceInfo {
    ResourceInfo {
        uri: template.uri_template.clone(),
        name: template.name.clone(),
        title: template.title.clone(),
        description: template.description.clone(),
        mime_type: template.mime_type.clone(),
        is_template: true,
    }
}

/// Expand a minimal RFC 6570 template with `params`.
///
/// Simple `{name}` expressions substitute directly. Reserved/explode
/// expressions (`{+path}`, `{#frag}`, `{x*}`) fill from their base
/// variable (`params["path"]`) so the MCP spec's canonical `file:///{path}`
/// form works; value encoding per operator is out of scope (values
/// substitute raw). Any other operator form (path/query operators like
/// `{/x}` or `{?x}`, modifiers, composite values) is left as-is, as is a
/// placeholder whose key is missing — the caller can detect an unexpanded
/// URI instead of sending a silently wrong one.
pub fn expand_resource_template(
    uri_template: &str,
    params: &std::collections::HashMap<String, String>,
) -> String {
    let mut out = String::with_capacity(uri_template.len());
    let mut rest = uri_template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('}') {
            Some(end) => {
                let key = &after[..end];
                // Operator prefixes (`+`, `#`, …), explode `*` suffixes, and
                // `:N` prefix modifiers address the base variable name.
                let name = key
                    .trim_start_matches(['+', '#', '.', '/', ';', '?', '&'])
                    .trim_end_matches('*')
                    .split(':')
                    .next()
                    .unwrap_or("");
                if let Some(value) = params.get(name) {
                    out.push_str(value);
                } else {
                    out.push('{');
                    out.push_str(key);
                    out.push('}');
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Whether an RFC 6570 expression may span `/` path separators.
///
/// Reserved expansion (`{+var}`, `{#var}`) and explode modifiers (`{x*}`)
/// are multi-segment by definition. A simple `{name}` matches one segment
/// (`[^/]+`): a literal `/` in the candidate value belongs to the URI
/// structure, not the value (MCP Python SDK `UriTemplate.match`: "a simple
/// `{name}` will not match across a literal `/` in the URI", while a
/// percent-encoded `%2F` contains no literal slash and still matches).
/// Path/query operators (`{/x}`, `{?x}`, …) are out of scope for this
/// minimal matcher and treated as single-segment.
fn template_expr_allows_slash(expr: &str) -> bool {
    expr.starts_with(['+', '#']) || expr.ends_with('*')
}

/// Whether a concrete `uri` matches a raw `uri_template`.
///
/// Minimal RFC 6570 matching: a simple `{var}` matches one non-empty path
/// segment (`[^/]+`); reserved/explode forms (`{+path}`, `{x*}`) span
/// segments; literals must match exactly in order. Used so
/// `resource_owner_of` can route an expanded URI to the server that
/// advertised its template. Adjacent placeholders with no anchoring
/// literal (`{a}{b}`) are inherently ambiguous (RFC 6570 §1.4; the MCP SDK
/// rejects them at parse time) and match leniently here — the surrounding
/// checks still apply.
pub fn match_resource_template(uri_template: &str, uri: &str) -> bool {
    if !uri_template.contains('{') {
        return uri_template == uri;
    }
    // Split into literals with the raw expression between each pair.
    let mut literals: Vec<&str> = Vec::new();
    let mut exprs: Vec<&str> = Vec::new();
    let mut rest = uri_template;
    while let Some(start) = rest.find('{') {
        literals.push(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('}') {
            Some(end) => {
                exprs.push(&after[..end]);
                rest = &after[end + 1..];
            }
            None => return false,
        }
    }
    literals.push(rest);
    debug_assert_eq!(literals.len(), exprs.len() + 1);

    // Leading literal anchors at position 0.
    let mut pos = 0usize;
    if !uri[pos..].starts_with(literals[0]) {
        return false;
    }
    pos += literals[0].len();
    for (k, lit) in literals[1..].iter().enumerate() {
        // `exprs[k]` is the placeholder whose value fills the gap before
        // this literal.
        if lit.is_empty() {
            continue;
        }
        // Multi-segment gaps scan greedily (last anchor); single-segment
        // gaps scan lazily (first anchor).
        let idx = if template_expr_allows_slash(exprs[k]) {
            uri[pos..].rfind(lit)
        } else {
            uri[pos..].find(lit)
        };
        match idx {
            Some(idx) => {
                let gap = &uri[pos..pos + idx];
                if gap.is_empty() {
                    return false;
                }
                if !template_expr_allows_slash(exprs[k]) && gap.contains('/') {
                    return false;
                }
                pos += idx + lit.len();
            }
            None => return false,
        }
    }
    // Trailing placeholder (template ends with `}`) matches the remaining
    // tail under the same single/multi-segment rule; otherwise the whole
    // URI must be consumed.
    if uri_template.ends_with('}') {
        let tail = &uri[pos..];
        if tail.is_empty() {
            return false;
        }
        let last = exprs.last().copied().unwrap_or("");
        if !template_expr_allows_slash(last) && tail.contains('/') {
            return false;
        }
        true
    } else {
        pos == uri.len()
    }
}

/// Expose one MCP prompt and its argument spec, so callers can render a
/// fill-in form and validate required arguments before calling `get_prompt`.
pub fn prompt_to_info(prompt: &Prompt) -> PromptInfo {
    PromptInfo {
        name: prompt.name.clone(),
        title: prompt.title.clone(),
        description: prompt.description.clone(),
        arguments: prompt
            .arguments
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .map(|argument| PromptArgumentInfo {
                name: argument.name.clone(),
                description: argument.description.clone(),
                required: argument.required.unwrap_or(false),
            })
            .collect(),
    }
}

/// Flatten a prompt result into `role: text` lines. Only text blocks map:
/// images, audio, resource links, and embedded resources are out of scope
/// for the text-first MVP and are skipped. An empty result renders empty by
/// design — the caller decides how to present it. NOTE: `""` is ambiguous
/// (empty result vs all-non-text skipped); no truncation guard is applied
/// (MVP: prompts are small, large ones are caller-truncated).
pub fn prompt_to_text(result: &GetPromptResult) -> String {
    result
        .messages
        .iter()
        .filter_map(prompt_message_to_text)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render one prompt message as `role: text`, or `None` for non-text
/// content (see [`prompt_to_text`]).
fn prompt_message_to_text(message: &PromptMessage) -> Option<String> {
    let text = message.content.as_text()?.text.clone();
    let role = match message.role {
        rmcp::model::Role::User => "user",
        rmcp::model::Role::Assistant => "assistant",
    };
    Some(format!("{role}: {text}"))
}

/// Flatten a resource read into its text contents joined by newline. Blob
/// (binary) contents are skipped: there is no safe text rendering for them
/// in the text-first MVP. Empty when the server sent nothing readable.
/// NOTE: `""` is ambiguous (empty vs all-blob skipped); multi-content
/// provenance (`uri`/`mimeType` per part) is dropped on join; no
/// truncation guard (MVP: caller truncates large texts).
pub fn resource_to_text(result: &ReadResourceResult) -> String {
    result
        .contents
        .iter()
        .filter_map(|content| match content {
            rmcp::model::ResourceContents::TextResourceContents { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}
