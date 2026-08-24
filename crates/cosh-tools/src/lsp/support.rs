//! Shared plumbing for the lsp tool engines.
//!
//! Private to the module: engines take these deps explicitly so they stay
//! testable without the wrapper.

use std::{ops::Range, path::Path, sync::Arc, time::Duration};

use cosh_sdk::lsp::{
    DiagnosticsEngine, LanguageServer, LspError, Manager, lsp_types::OneOf, lsp_types::Position,
    lsp_types::ServerCapabilities, offset_to_position, position_to_offset,
};

/// Everything an engine needs from the wrapper, borrowed.
pub struct Deps<'a> {
    pub manager: &'a Manager,
    pub diagnostics: &'a DiagnosticsEngine,
    /// Per-request deadline for definition/references/symbol queries.
    pub request_timeout: Duration,
    /// Budget handed to [`DiagnosticsEngine::wait_for_settle`] after touching
    /// files.
    pub settle_cap: Duration,
}

/// Ensure every applicable server for `path` is running, open the document on
/// each of them, and let diagnostics settle.
///
/// Fails when no catalog spec claims the file (unsupported type) or every
/// spawn failed — an explicit message beats an empty result the model would
/// misread as "clean".
pub async fn prepare(deps: &Deps<'_>, path: &Path) -> Result<Vec<Arc<LanguageServer>>, String> {
    let handles = deps
        .manager
        .ensure_for_file(path)
        .await
        .map_err(|err| format!("no language server for `{}`: {err:#}", path.display()))?;

    if handles.is_empty() {
        return Err(format!(
            "no language server for `{}` (file type not supported or binary missing)",
            path.display()
        ));
    }

    for handle in &handles {
        if let Err(err) = handle.touch_file(path).await {
            return Err(format!(
                "failed to open `{}` on {}: {err}",
                path.display(),
                handle.name()
            ));
        }
    }

    deps.diagnostics.wait_for_settle(deps.settle_cap).await;
    Ok(handles)
}

/// Resolve where a symbol query points: explicit 1-based `position`, or the
/// first whole-word occurrence of `symbol` located directly in the file.
///
/// All coordinate math goes through the SDK's position helpers honoring the
/// server-negotiated encoding — engines never hand-roll it.
pub async fn resolve_target(
    deps: &Deps<'_>,
    path: &Path,
    position: &Option<super::types::Position1>,
    symbol: &Option<String>,
) -> Result<(Position, Vec<Arc<LanguageServer>>), String> {
    let clients = prepare(deps, path).await?;
    if clients.is_empty() {
        return Err(format!("no language server handles `{}`", path.display()));
    }
    let encoding = clients[0].position_encoding();

    let text = std::fs::read_to_string(path)
        .map_err(|err| format!("cannot read `{}`: {err}", path.display()))?;

    // Model coordinates are 1-based UTF-16 units (the protocol default). Both
    // branches normalize through byte offsets so out-of-range input clamps and
    // the negotiated encoding is respected end-to-end.
    let target = match position {
        Some(given) => {
            let raw = Position {
                line: given.line.saturating_sub(1),
                character: given.character.saturating_sub(1),
            };
            let offset = position_to_offset(&text, raw, encoding);
            offset_to_position(&text, offset, encoding).unwrap_or(raw)
        }
        None => {
            let symbol = symbol
                .as_deref()
                .ok_or("provide either `position` or `symbol`")?;
            let offset = locate_symbol(&text, symbol)?;
            offset_to_position(&text, offset, encoding)
                .ok_or_else(|| format!("symbol `{symbol}` landed mid-character"))?
        }
    };

    Ok((target, clients))
}

/// Byte offset of the first whole-word occurrence of `symbol`.
///
/// Direct scan: the file is already read, so a walk with word-boundary checks
/// beats pulling in a search engine. Boundaries mirror `\b` semantics for
/// identifier-like words.
fn locate_symbol(text: &str, symbol: &str) -> Result<usize, String> {
    if symbol.is_empty() {
        return Err("empty symbol".into());
    }
    let is_word_char = |c: char| c.is_alphanumeric() || c == '_';

    let mut from = 0usize;
    while let Some(rel) = text[from..].find(symbol) {
        let start = from + rel;
        let end = start + symbol.len();
        let before_ok = text[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !is_word_char(c));
        let after_ok = text[end..].chars().next().is_none_or(|c| !is_word_char(c));
        if before_ok && after_ok {
            return Ok(start);
        }
        from = end.max(from + 1);
    }
    Err(format!("symbol `{symbol}` not found"))
}

/// `file://` URI for a workspace path.
pub fn uri(path: &Path) -> Result<cosh_sdk::lsp::lsp_types::Uri, String> {
    cosh_sdk::lsp::uri_from_path(path).map_err(|err| err.to_string())
}

/// Capability predicate for `bool | registration-options` providers.
pub fn provider_enabled(provider: &Option<OneOf<bool, serde_json::Value>>) -> bool {
    match provider {
        Some(OneOf::Left(enabled)) => *enabled,
        Some(OneOf::Right(_)) => true,
        None => false,
    }
}

/// Run one raw query against every client that advertises `supported`,
/// returning the first non-null parsed answer.
///
/// Clients without the capability are skipped; per-client errors are logged
/// and skipped unless every capable client failed. A null result (server
/// understood but found nothing) does NOT stop the fan-out — a secondary
/// server may still know the answer. When NO client is capable, a distinct
/// error surfaces instead of an empty result masquerading as "not found".
pub async fn first_answer<T, P, F>(
    clients: &[Arc<LanguageServer>],
    timeout: Duration,
    supported: F,
    query: P,
) -> Result<Option<T>, String>
where
    T: serde::de::DeserializeOwned,
    P: Fn(&LanguageServer) -> (String, serde_json::Value),
    F: Fn(&ServerCapabilities) -> bool,
{
    let mut last_error: Option<String> = None;
    let mut saw_capable = false;

    for client in clients {
        let capable = client.capabilities().is_some_and(|caps| supported(&caps));
        if !capable {
            continue;
        }
        saw_capable = true;

        let (method, params) = query(client);
        match client.request_raw(&method, Some(params), timeout).await {
            Ok(value) if value.is_null() => continue,
            Ok(value) => {
                let parsed: T = serde_json::from_value(value.clone()).map_err(|err| {
                    format!("server `{}` returned malformed data: {err}", client.name())
                })?;
                return Ok(Some(parsed));
            }
            // MethodNotFound: this server simply lacks the method.
            Err(LspError::Rpc { code: -32601, .. }) => continue,
            Err(LspError::Timeout { .. }) => {
                last_error = Some(format!("server `{}` timed out", client.name()));
            }
            Err(err) => {
                log::debug!("server `{}` rejected query: {err:#}", client.name());
                last_error = Some(format!("{err:#}"));
            }
        }
    }

    if !saw_capable {
        return Err("no queried server advertises this capability".into());
    }
    match last_error {
        Some(err) => Err(err),
        None => Ok(None),
    }
}

/// LSP SymbolKind → readable label. Unrecognized numbers degrade to
/// `"symbol"` instead of being dropped (hiding declarations is worse).
pub(crate) fn symbol_kind_name(kind: cosh_sdk::lsp::lsp_types::SymbolKind) -> String {
    use cosh_sdk::lsp::lsp_types::SymbolKind as K;
    match kind {
        K::FILE => "file".into(),
        K::MODULE | K::NAMESPACE | K::PACKAGE => "module".into(),
        K::CLASS => "class".into(),
        K::METHOD => "method".into(),
        K::PROPERTY => "property".into(),
        K::FIELD => "field".into(),
        K::CONSTRUCTOR => "constructor".into(),
        K::ENUM => "enum".into(),
        K::INTERFACE => "interface".into(),
        K::FUNCTION => "function".into(),
        K::VARIABLE => "variable".into(),
        K::CONSTANT => "constant".into(),
        K::STRING | K::NUMBER | K::BOOLEAN | K::ARRAY | K::OBJECT | K::KEY | K::NULL => {
            "literal".into()
        }
        K::ENUM_MEMBER => "enum-member".into(),
        K::STRUCT => "struct".into(),
        K::EVENT => "event".into(),
        K::OPERATOR => "operator".into(),
        K::TYPE_PARAMETER => "type-parameter".into(),
        _ => "symbol".into(),
    }
}

// ── WorkspaceEdit application (shared by rename + code_actions) ────────

/// One concrete replacement on disk.
pub struct PlannedEdit {
    /// Byte range to replace.
    pub span: Range<usize>,
    pub new_text: String,
    /// 1-based line for previews.
    pub line: usize,
}

/// Splice one file's edits back-to-front. Overlaps abort before any write.
pub fn apply_edits(path: &Path, planned: &[PlannedEdit]) -> Result<(), String> {
    let mut sorted: Vec<&PlannedEdit> = planned.iter().collect();
    sorted.sort_by_key(|edit| std::cmp::Reverse(edit.span.start));

    for pair in sorted.windows(2) {
        let later = pair[0];
        let earlier = pair[1];
        if earlier.span.end > later.span.start {
            return Err(format!(
                "overlapping edits in {} — aborting without writing",
                path.display()
            ));
        }
    }

    let mut content = std::fs::read_to_string(path)
        .map_err(|err| format!("cannot read `{}`: {err}", path.display()))?;

    for edit in &sorted {
        if !content.is_char_boundary(edit.span.start) || !content.is_char_boundary(edit.span.end) {
            return Err(format!("edits no longer fit `{}`", path.display()));
        }
    }

    for edit in &sorted {
        content.replace_range(edit.span.clone(), &edit.new_text);
    }

    std::fs::write(path, content).map_err(|err| format!("cannot write `{}`: {err}", path.display()))
}
