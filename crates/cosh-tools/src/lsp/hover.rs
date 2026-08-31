//! `lsp_hover` engine: type/signature documentation at a position.

use cosh_sdk::lsp::lsp_types::{
    Hover, HoverContents, MarkedString,
    request::{HoverRequest, Request as _},
};
use serde_json::json;

use super::{support, types::HoverInput, types::HoverOutput};

/// Resolve `textDocument/hover` on the first capable server.
pub async fn run_hover(
    deps: &super::support::Deps<'_>,
    input: &HoverInput,
) -> Result<HoverOutput, String> {
    let path = deps.manager.root().join(&input.file_path);
    let (position, clients) =
        support::resolve_target(deps, &path, &input.position, &input.symbol).await?;

    let params = json!({
        "textDocument": { "uri": support::uri(&path)? },
        "position": position,
    });

    let response: Option<Hover> = super::support::first_answer(
        &clients,
        deps.request_timeout,
        |caps| caps.hover_provider.as_ref().is_some(),
        |_client| (HoverRequest::METHOD.to_owned(), params.clone()),
    )
    .await?;

    let formatted = response
        .and_then(|hover| render_contents(&hover))
        .unwrap_or_else(|| "No hover information.".into());

    Ok(HoverOutput { formatted })
}

/// Flatten any of the three hover content shapes into plain text/markdown.
fn render_contents(hover: &Hover) -> Option<String> {
    match &hover.contents {
        HoverContents::Scalar(value) => render_marked_string(value),
        HoverContents::Array(values) => {
            let parts: Vec<String> = values.iter().filter_map(render_marked_string).collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n\n"))
            }
        }
        HoverContents::Markup(markup) => Some(markup.value.clone()),
    }
}

fn render_marked_string(value: &MarkedString) -> Option<String> {
    match value {
        MarkedString::String(text) => Some(text.clone()),
        MarkedString::LanguageString(block) => {
            Some(format!("```{}\n{}\n```", block.language, block.value))
        }
    }
}
