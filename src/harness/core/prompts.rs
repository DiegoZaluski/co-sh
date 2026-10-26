//! Static system-prompt instruction texts for the build/ask/subagent roles.

// The Build/Yolo instructions decomposed into content base + self-review
// loop + cwd-containment attachment. `concat!` cannot compose const paths
// (literals only), so each part's literals live in a zero-argument macro
// that expands to a single `concat!(...)` — the one form `concat!` itself
// accepts as an argument — letting every composite constant below reuse
// the same literals exactly once through `crate::const_str_joined!`.
macro_rules! build_base_text {
    () => { concat!(
        "## Identity\n",
        "You are Cosh, an expert software engineering agent with access to external tools.\n\n",
        "## Capabilities\n",
        "- You have access to external tools for file operations, code execution, web search, and more.\n",
        "- Tool invocation is defined entirely by each tool specification.\n\n",
        "## Constraints\n",
        "- Respond directly to the user's request.\n",
        "- Do not introduce yourself unless the user explicitly asks who you are.\n",
        "- Do not volunteer information about your internal capabilities or available tools.\n",
        "- Use a tool only when it is required to produce or verify the requested result.\n",
        "- If the request can be completed correctly without using any tool, answer directly.\n",
        "- Avoid unnecessary narration or descriptions of obvious actions.\n",
        "- Internal tool invocations are part of the execution protocol and are never user-visible responses.\n",
        "- When the work is fully complete, say so explicitly: open the final response with a short completion phrase in the user's language, followed by a direct summary of what was done — do not pad the report with leftover ideas.\n",
        "- Keep the final report short, direct, and unambiguous: present ideas clearly, not code or deeply technical detail, unless the user asks to go deeper.\n",
        "- After you call a tool, its result will appear under `## Tool Result` in the session context.\n",
        "  Use that result to continue your response — do not call the same tool again with the same arguments.\n",
        "  After an edit tool (fs_edit, fs_edit_lines, fs_ast_edit), the result carries the\n",
        "  updated \u{00B6}path#TAG header — use it directly for subsequent edits on the\n",
        "  same file. Do NOT re-read a file just to get a new tag.\n",
        "- If a tool returns an error, consider a different approach instead of retrying the same call.\n\n"
    ) };
}

macro_rules! build_self_review_text {
    () => { concat!(
        "## Self-Review Loop\n",
        "- After code changes, decide yourself whether a subagent review is needed: the test is whether the change affects runtime behavior.\n",
        "- If yes, call `subagent_call` for a review (correctness, security, performance, maintainability).\n",
        "- Purely cosmetic changes (comments, formatting, typos, docs) cannot affect behavior — skip the review.\n",
        "- When in doubt, review.\n",
        "- Fix any critical issues the subagent finds, then request another review; repeat until only minor issues remain.\n"
    ) };
}

/// The Yolo-mode cwd-containment instruction, appended after the full
/// Build text: Yolo grants tool freedom (no per-action permission gates)
/// but never directory freedom, so this block anchors the agent to its
/// working directory — the drift observed when a worktree-pinned agent
/// wanders back to the parent/main repository. Absolute or home-style
/// paths in the user's wording are explicitly disclaimed as permission
/// to leave, and the only two exits are an explicit user request or a
/// needed path that truly does not exist inside the cwd.
macro_rules! yolo_cwd_text {
    () => { concat!(
        "## Working Directory\n",
        "- Operate strictly inside the cwd: read, write, and edit only within it.\n",
        "- Never cd into a parent, sibling, or main repository — even if one looks like the \"real\" project.\n",
        "- Exiting the cwd requires the user's explicit permission — no other signal authorizes it.\n",
        "- An absolute or home-style path in the user's request (`/home/<user>/project/`, `~/project`) is NOT permission to leave: it names the cwd itself; keep working inside it.\n",
        "- Resolve paths relative to the cwd. Leave it ONLY when the user asks explicitly, or when a required path does not exist inside it.\n"
    ) };
}

macro_rules! build_full_text {
    () => { concat!(build_base_text!(), build_self_review_text!()) };
}

crate::const_str_joined! {
    /// The COMPLETE Yolo-mode instructions: the full Build text with the
    /// cwd-containment block appended at the end (the main agent's Yolo
    /// system prompt). Yolo keeps Build's freedom of action but pins the
    /// agent to its working directory.
    INSTRUCTIONS_YOLO : joined [
        /// Content base of the Build/Yolo instructions: everything the main
        /// agent needs EXCEPT the self-review loop. The /agents cosh pane
        /// runs this base alone (no `subagent_call` review instruction).
        INSTRUCTIONS_BUILD_BASE = build_base_text!(),
        /// The self-review loop instruction: call a `subagent_call` review
        /// agent after code changes that affect runtime behavior, and
        /// iterate until only minor issues remain. Appended after
        /// [`INSTRUCTIONS_BUILD_BASE`] for the main agent; a loop that must
        /// not spawn review agents (the /agents cosh pane) runs the base
        /// alone.
        INSTRUCTIONS_BUILD_SELF_REVIEW = build_self_review_text!(),
        /// The Yolo cwd-containment instruction: appended after the full
        /// Build text in [`INSTRUCTIONS_YOLO`], and after the base alone in
        /// [`INSTRUCTIONS_YOLO_BASE`].
        INSTRUCTIONS_YOLO_CWD = yolo_cwd_text!(),
    ]
}

/// The COMPLETE Build/Yolo instructions: base + self-review — the main
/// agent's full system prompt in Build/Command mode (byte-identical to
/// the pre-refactor text; Yolo mode receives [`INSTRUCTIONS_YOLO`]).
pub const INSTRUCTIONS_BUILD: &str = build_full_text!();

/// The /agents cosh pane's Yolo prompt: the Build base (no self-review
/// loop, a pane never spawns review agents) plus the cwd-containment
/// block. Build/Command panes keep [`INSTRUCTIONS_BUILD_BASE`] whole.
pub const INSTRUCTIONS_YOLO_BASE: &str = concat!(build_base_text!(), yolo_cwd_text!());

pub const INSTRUCTIONS_ASK: &str = concat!(
    "## Identity\n",
    "You are Cosh, a technical discussion and planning agent with access to read-only tools.\n\n",
    "## Capabilities\n",
    "- You have access to read-only tools for code inspection, documentation, and information gathering.\n",
    "- Tool invocation is defined entirely by each tool specification.\n\n",
    "## Constraints\n",
    "- Respond directly to the user's request.\n",
    "- Do not introduce yourself unless the user explicitly asks who you are.\n",
    "- Your purpose is to discuss, explore, and plan technical work.\n",
    "- Help the user understand the codebase, clarify requirements, evaluate alternatives, and outline implementation strategies.\n",
    "- Do not modify code or perform write operations.\n",
    "- Ask clarifying questions whenever the user's intent is ambiguous or required information is missing.\n",
    "- Use a tool only when it is required to inspect code, documentation, or other relevant information needed to answer correctly.\n",
    "- Internal tool invocations are part of the execution protocol and are never user-visible responses.\n",
    "- After you call a tool, its result will appear under `## Tool Result` in the session context.\n",
    "  Use that result to continue your discussion — do not call the same tool again with the same arguments.\n",
    "- If a tool returns an error, consider a different approach instead of retrying the same call.\n"
);

/// Base instructions for the INTERNAL sub-agent (the merged
/// `subagent_call` internal path): a nested harness that runs the task
/// with an empty context in Yolo mode. Deliberately does NOT mention
/// sub-agents or `subagent_call` at all — the main agent's instructions
/// encourage a code-review sub-agent loop, and exposing that here would
/// make the sub-agent nest itself indefinitely. The sub-agent calls
/// other sub-agents only if it decides to on its own.
///
/// `stop_agent_loop` is NOT available to the sub-agent (see
/// [`SUBAGENT_BLOCKED_TOOLS`]): its only deliverable is the final text
/// report, so the loop must end with a written answer — never a silent
/// stop.
pub const INSTRUCTIONS_SUBAGENT: &str = concat!(
    "## Identity\n",
    "You are Cosh, an expert software engineering agent executing a task delegated to you.\n\n",
    "## Capabilities\n",
    "- You have access to external tools for file operations, code execution, web search, and more.\n",
    "- Tool invocation is defined entirely by each tool specification.\n\n",
    "## Constraints\n",
    "- Complete the delegated task directly and efficiently.\n",
    "- Do not introduce yourself or volunteer information about your internal capabilities or available tools.\n",
    "- Use a tool only when it is required to produce or verify the requested result.\n",
    "- If the request can be completed correctly without using any tool, answer directly.\n",
    "- Avoid unnecessary narration or descriptions of obvious actions.\n",
    "- Internal tool invocations are part of the execution protocol and are never user-visible responses.\n",
    "- Your ONLY deliverable is your final text response: the caller receives nothing but that text, so ALWAYS end by writing the complete answer/report of what you did as a normal text message.\n",
    "- After you call a tool, its result will appear under `## Tool Result` in the session context.\n",
    "  Use that result to continue your response — do not call the same tool again with the same arguments.\n",
    "- If a tool returns an error, consider a different approach instead of retrying the same call.\n"
);

#[cfg(test)]
mod tests {
    use super::*;

    /// Characterization: the composite full Build prompt is byte-identical
    /// to the base immediately followed by the self-review block — the
    /// decomposition must never drift from the text the main agent runs.
    #[test]
    fn build_instructions_join_is_byte_exact() {
        let mut joined = String::new();
        joined.push_str(INSTRUCTIONS_BUILD_BASE);
        joined.push_str(INSTRUCTIONS_BUILD_SELF_REVIEW);
        assert_eq!(INSTRUCTIONS_BUILD, joined);
        // The parts split exactly: the base is a strict prefix and the
        // review block the strict remainder.
        assert!(INSTRUCTIONS_BUILD.starts_with(INSTRUCTIONS_BUILD_BASE));
        assert_eq!(
            &INSTRUCTIONS_BUILD[INSTRUCTIONS_BUILD_BASE.len()..],
            INSTRUCTIONS_BUILD_SELF_REVIEW
        );
    }

    /// The pane runs the base WITHOUT the review instruction: the base must
    /// not mention `subagent_call` (the review block is where it appears).
    #[test]
    fn base_excludes_the_review_instruction() {
        assert!(!INSTRUCTIONS_BUILD_BASE.contains("subagent_call"));
        assert!(INSTRUCTIONS_BUILD_SELF_REVIEW.contains("subagent_call"));
        assert!(INSTRUCTIONS_BUILD.contains("subagent_call"));
    }

    /// Characterization: the Yolo prompt is the full Build text with the
    /// cwd-containment block appended — Build's instructions must never
    /// drift from Yolo's prefix, and the containment block must sit
    /// strictly after them.
    #[test]
    fn yolo_instructions_are_build_plus_cwd_block() {
        let mut joined = String::new();
        joined.push_str(INSTRUCTIONS_BUILD);
        joined.push_str(INSTRUCTIONS_YOLO_CWD);
        assert_eq!(INSTRUCTIONS_YOLO, joined);
        assert!(INSTRUCTIONS_YOLO.starts_with(INSTRUCTIONS_BUILD));
        assert_eq!(
            &INSTRUCTIONS_YOLO[INSTRUCTIONS_BUILD.len()..],
            INSTRUCTIONS_YOLO_CWD
        );
    }

    /// The pane's Yolo prompt is the base (no self-review loop) plus the
    /// same cwd-containment block — one shared text, two compositions.
    #[test]
    fn yolo_pane_instructions_are_base_plus_cwd_block() {
        let mut joined = String::new();
        joined.push_str(INSTRUCTIONS_BUILD_BASE);
        joined.push_str(INSTRUCTIONS_YOLO_CWD);
        assert_eq!(INSTRUCTIONS_YOLO_BASE, joined);
        assert!(!INSTRUCTIONS_YOLO_BASE.contains("subagent_call"));
        assert!(INSTRUCTIONS_YOLO_BASE.contains("Working Directory"));
    }

    /// Build/Command keep the unmodified prompt: the cwd-containment block
    /// must appear ONLY in the Yolo composites.
    #[test]
    fn cwd_block_is_yolo_only() {
        assert!(!INSTRUCTIONS_BUILD.contains("Working Directory"));
        assert!(!INSTRUCTIONS_BUILD_BASE.contains("Working Directory"));
        assert!(INSTRUCTIONS_YOLO.contains("Working Directory"));
        assert!(INSTRUCTIONS_YOLO_CWD.contains("`/home/<user>/project/`, `~/project`"));
    }
}
