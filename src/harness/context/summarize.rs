//! Prompt construction and transcript serialization for the LLM compaction
//! and the split-and-concatenate contingency.
//!
//! The summarizer is a DEDICATED agent with its OWN instructions, completely
//! separate from the agent loop's fixed system prompt in `core.rs`: it never
//! sees the main system prompt, the tool definitions, or the harness
//! instructions (opencode-style).

use super::ContextItem;

/// The summarization template the LLM compaction asks the model to fill
/// (opencode's `SUMMARY_TEMPLATE`, kept as inspiration): a structured anchor
/// that preserves the objective, the work state and the next move so the
/// session can continue seamlessly from the summary.
pub(super) const SUMMARY_TEMPLATE: &str = "\
Output exactly the Markdown structure shown inside <template> and keep the section order unchanged. Do not include the <template> tags in your response.
<template>
## Objective
- [one or two brief sentences describing what the user is trying to accomplish]

## Important Details
- [constraints/preferences, decisions and why, important facts/assumptions, exact context needed to continue, or \"(none)\"]

## Work State
### Completed
- [finished work, verified facts, or changes made; otherwise \"(none)\"]

### Active
- [current work, partial changes, or investigation state; otherwise \"(none)\"]

### Blocked
- [blockers, failing commands, or unknowns; otherwise \"(none)\"]

## Next Move
1. [immediate concrete action, or \"(none)\"]
2. [next action if known, or \"(none)\"]

## Relevant Files
- [file path with its hashline anchor (¶path#TAG) and the line ranges that matter, then why it matters; or \"(none)\"]

## Code & Anchors
- [code blocks, function signatures, hashline anchors (¶path#TAG) and line numbers the next step needs to resume without re-reading whole files; otherwise \"(none)\"]
</template>

Rules:
- Keep every section, even when empty.
- Use terse bullets, not prose paragraphs — except code, which is ALWAYS copied verbatim inside Markdown code fences.
- Preserve exact file paths, hashline anchors (¶path#TAG), line numbers, symbols, commands, error strings, URLs, and identifiers when known.
- Carry code VERBATIM: quote the important code blocks, function signatures and error messages exactly as they appear in the transcript. Never paraphrase code — a paraphrased block cannot be applied or edited.
- The summary must let the agent resume editing where it stopped: for every file the next step touches, keep the hashline anchor of its last read/written state and the specific lines/symbols involved.
- Do not mention the summary process or that context was compacted.";

/// The SYSTEM prompt of the LLM summarizer — its OWN instructions, completely
/// separate from the agent loop's fixed system prompt in `core.rs`. The
/// summarizer is a dedicated agent with its own context and its own rules
/// (opencode-style): it never sees the main system prompt, the tool
/// definitions, or the harness instructions.
pub(super) const SUMMARIZER_SYSTEM: &str = "\
You are a conversation summarizer for an AI coding agent.
You read the transcript of everything that happened in the session and produce
a single anchored Markdown summary that lets the session continue seamlessly.
Rules:
- Follow the template exactly; never add sections, never change their order.
- Preserve exact file paths, hashline anchors (¶path#TAG), line numbers, symbols, commands, error strings, URLs, and identifiers when known.
- Carry code VERBATIM: quote the important code blocks, function signatures and error messages exactly as they appear in the transcript (inside fenced blocks). The agent edits files by hashline anchor and line number — paraphrased code cannot be used.
- Prefer a few exact code blocks over prose descriptions of the same code.
- Keep facts precise; do not invent details that are not in the transcript.
- In update mode, preserve still-true details, remove stale details, and merge in the new facts.
- Do not mention the summary process or that context was compacted.";

/// Serialize one conversation item into the opencode-style transcript the
/// LLM compaction sends to the model.
pub(super) fn serialize_item(item: &ContextItem) -> String {
    match item {
        ContextItem::User { original, .. } => format!("[User]: {original}"),
        ContextItem::Assistant { original, .. } => format!("[Assistant]: {original}"),
        ContextItem::ToolCall {
            name, arguments, ..
        } => format!("[Assistant tool call]: {name}({arguments})"),
        // Tool results are passed VERBATIM — the tools bound their own output,
        // and truncating a payload the model still needs only forces a
        // costlier re-read of the source.
        ContextItem::ToolResult { content, .. } => format!("[Tool result]: {content}"),
        ContextItem::Closure { content, .. } => format!("[Assistant]: {content}"),
        // The previous summary stays in the timeline (index 0 after
        // `apply_llm_summary`) — the update-mode instruction references it by
        // this exact label instead of embedding a second copy in the prompt.
        ContextItem::Compaction { summary, .. } => format!("[Previous summary]: {summary}"),
    }
}

/// Build the compaction prompt (opencode-style): create a new anchored summary
/// from the serialized conversation, or update the previous summary when one
/// exists (avoiding the summary-of-summary quality loss).
///
/// The previous summary is NOT embedded here — it stays in the timeline as the
/// FIRST line of the transcript (`[Previous summary]: …`, index 0), and update
/// mode only references it by that label. One copy, never duplicated: the
/// model sees the anchor exactly where the timeline naturally starts, followed
/// by the delta (everything that happened since) to merge into it.
pub(super) fn build_llm_prompt(has_previous_summary: bool, context: &str) -> String {
    let instruction = if has_previous_summary {
        "Update the summary labeled [Previous summary] at the top of the transcript below, \
         using the rest of the conversation history. Preserve still-true details, \
         remove stale details, and merge in the new facts."
    } else {
        "Create a new anchored summary from the conversation history."
    };
    format!("{instruction}\n\n{SUMMARY_TEMPLATE}\n\n{context}")
}

/// The SYSTEM prompt of the split summarizer — the dedicated agent that
/// processes the timeline in sequential chunks for the split-and-concatenate
/// contingency. Unlike [`SUMMARIZER_SYSTEM`] (one-shot over the whole
/// timeline), this one must keep the final buffer continuous: every
/// continuation chunk is glued onto the tail of the previous summary.
pub(super) const SPLIT_SUMMARIZER_SYSTEM: &str = "\
You are a conversation summarizer for an AI coding agent.
You summarize a long conversation in SEQUENTIAL CHUNKS: each chunk continues
where the previous one ended, and your summaries are concatenated into ONE
final anchored summary.
Rules:
- First chunk: build the anchored summary following the template exactly.
- Continuation chunks: extend the summary naturally from the tail of the
  previous summary shown in [Continuation context] — keep its structure and
  style, add the new facts, never restate what is already covered. The final
  result must read as ONE continuous document with no visible seam.
- Preserve exact file paths, hashline anchors (¶path#TAG), line numbers,
  symbols, commands, error strings, URLs, and identifiers when known.
- Carry code VERBATIM: quote the important code blocks, function signatures
  and error messages exactly as they appear (inside fenced blocks). Never
  paraphrase code.
- Keep facts precise; do not invent details that are not in the chunk.
- When a token limit is given, respect it strictly — prefer terse bullets.
- Do not mention the summary process or that context was compacted.";

/// Build the split-and-concatenate prompt for one chunk.
///
/// The FIRST chunk carries the full [`SUMMARY_TEMPLATE`] so it produces the
/// anchored skeleton; every continuation chunk gets the tail of the previous
/// summary as a continuity snippet and is told to extend it — never restart
/// the template — so the concatenated buffer reads seamlessly.
///
/// The token-limit line is only included when the buffer accumulation becomes
/// concerning ([`SPLIT_WARN_PCT`] of the ceiling): before that the model is
/// left to act naturally, per the "observe and only warn when it matters"
/// design.
pub(super) fn build_split_prompt(
    first_chunk: bool,
    continuity: &str,
    chunk: &str,
    target_tokens: Option<usize>,
    remaining_tokens: usize,
    buffer_tokens: usize,
) -> String {
    let mut out = String::new();
    if first_chunk {
        out.push_str(SUMMARY_TEMPLATE);
        out.push_str("\n\n");
    } else {
        out.push_str(
            "Continue the anchored summary you started in the previous call.\n\
             Preserve its section structure and style; extend the existing sections with the new \
             facts. Do NOT restart the template from scratch — the final summary must read as ONE \
             continuous document.\n\n\
             [Continuation context — the tail of your previous summary]:\n",
        );
        out.push_str(continuity);
        out.push('\n');
    }
    if let Some(target) = target_tokens {
        out.push_str(&format!(
            "\nTOKEN LIMIT: {buffer_tokens} tokens are already summarized and the final \
             summary must stay within budget. Compress the chunk below to AT MOST ~{target} \
             tokens — terse bullets, only the essential facts, paths, hashline anchors and code.\n"
        ));
    }
    out.push_str(&format!(
        "\n~{remaining_tokens} tokens of conversation remain to summarize.\n\n"
    ));
    out.push_str("[Conversation chunk to summarize]:\n");
    out.push_str(chunk);
    out
}
