//! Expand deferred `replace block N:` edits into concrete inserts + deletes.
//!
//! The hashline parser cannot expand a block edit on its own — the line span is
//! unknown until file text + path (→ language) are available. This transform
//! runs at every apply/preview boundary that has text: it calls the injected
//! [`BlockResolver`] to resolve each block's `[start, end]` span, then emits
//! the exact same `before_anchor` replacement inserts + range deletes that
//! `replace start..end:` produces in the parser. After it runs, no `block`
//! edits remain, so [`apply_edits`] (and recovery) only ever see resolved edits.
use super::messages::{BLOCK_RESOLVER_UNAVAILABLE, block_unresolved_message};
use super::types::{
    Anchor, BlockResolver, BlockResolverRequest, BlockSpan, Cursor, Edit, Replacement,
    ResolveAction,
};

/// How to handle a block edit that cannot be resolved (missing resolver or a
///
/// `null` span). `"throw"` (default) raises a `blockUnresolvedMessage` error —
/// used by the authoritative apply + final preview paths. `"drop"` silently
/// skips the edit — used by the streaming preview, where a half-written file
/// or transient parse error must not throw.
#[derive(Debug, Clone)]
pub struct ResolveBlockEditsOptions {
    pub on_unresolved: ResolveAction,
}

impl Default for ResolveBlockEditsOptions {
    fn default() -> Self {
        Self {
            on_unresolved: ResolveAction::Throw,
        }
    }
}

/// True when at least one edit is an unresolved `replace block N:` edit.
#[must_use]
pub fn has_block_edit(edits: &[Edit]) -> bool {
    edits.iter().any(|edit| matches!(edit, Edit::Block { .. }))
}

/// Resolve every `replace block N:` edit in `edits` against `text` (parsed as
///
/// the language inferred from `path`). Non-block edits pass through untouched.
/// Returns a fresh edit list with no `block` variants. The fast path returns the
/// input unchanged when there is nothing to resolve.
///
/// Synthesized inserts/deletes carry sequential `index` values for readability
/// only — [`apply_edits`] re-derives every edit's index from array order, so
/// the passthrough edits keeping their original indices is harmless.
///
/// # Errors
///
/// Returns the `block_unresolved_message` diagnostic when an unresolvable
/// block edit is encountered and `on_unresolved` is set to [`ResolveAction::Throw`]
/// — never a panic: authored-input rejection flows through the same `Err`
/// channel as every other malformed edit (see the parity guarantee
/// `unresolved_block_is_an_error_not_a_panic`).
#[allow(clippy::similar_names)]
pub fn resolve_block_edits(
    edits: &[Edit],
    text: &str,
    path: &str,
    resolver: Option<BlockResolver>,
    options: Option<ResolveBlockEditsOptions>,
) -> Result<Vec<Edit>, String> {
    if !has_block_edit(edits) {
        return Ok(edits.to_vec());
    }

    let on_unresolved = options.unwrap_or_default().on_unresolved;
    let mut resolved: Vec<Edit> = Vec::new();
    let mut synth_index: u32 = 0;

    for edit in edits {
        let (anchor, payloads, line_num) = match edit {
            Edit::Block {
                anchor,
                payloads,
                line_num,
                ..
            } => (*anchor, payloads, *line_num),
            other => {
                resolved.push(other.clone());
                continue;
            }
        };

        let request = BlockResolverRequest {
            path: path.to_string(),
            text: text.to_string(),
            line: anchor.line,
        };
        let span = resolver.and_then(|r| r(request));

        let Some(BlockSpan { start, end }) = span else {
            match on_unresolved {
                ResolveAction::Drop => continue,
                ResolveAction::Throw => {
                    let msg = if resolver.is_some() {
                        block_unresolved_message(anchor.line)
                    } else {
                        BLOCK_RESOLVER_UNAVAILABLE.to_string()
                    };
                    return Err(format!("line {line_num}: {msg}"));
                }
            }
        };

        // Mirror the parser's `replace start..end:` expansion exactly: one
        // `before_anchor` replacement insert per payload row at `span.start`,
        // then one delete per line across `[span.start, span.end]`. An empty
        // `payloads` (from `delete block N`) emits no inserts — a pure deletion.
        for payload in payloads {
            resolved.push(Edit::Insert {
                cursor: Cursor::BeforeAnchor(Anchor { line: start }),
                text: payload.clone(),
                line_num,
                index: synth_index,
                mode: Some(Replacement::Replacement),
            });
            synth_index += 1;
        }
        for line in start..=end {
            resolved.push(Edit::Delete {
                anchor: Anchor { line },
                line_num,
                index: synth_index,
                old_assertion: None,
            });
            synth_index += 1;
        }
    }

    Ok(resolved)
}
