//! Dedicated protected context block for tool TODOs.
//!
//! The [`ContextManager`](super::ContextManager) is the single owner of the
//! conversation timeline. This separate mini-manager owns ONLY the tool TODO
//! list — the plan the agent is executing — and renders it as a single
//! protected block placed at the END of the message list: after the
//! conversation history and the steering input, right where the model is
//! about to generate. When the last message is itself a `user` turn, the
//! block is merged into it (block first, steering after — avoiding two
//! consecutive `user` messages, which some providers reject); otherwise it is
//! injected as its own trailing `user` message. The model therefore always
//! sees the current plan without needing to call `plan_todo_read` first, and
//! cannot forget it.
//!
//! # Cache-friendly placement
//!
//! Every provider caches the prompt by EXACT PREFIX (Anthropic `cache_control`
//! auto mode, OpenAI automatic prefix caching, Gemini implicit caching). The
//! tail placement keeps the stable `[system → history]` prefix byte-identical
//! across requests, so a `plan_*` dispatch that re-renders the block only
//! invalidates the tail — which is new, uncached content anyway. A front
//! placement would instead invalidate the WHOLE history on every TODO change
//! (a full-price reprocessing of the entire context on that request). The
//! block "moving" one position per appended history turn costs nothing:
//! prefix matching only needs the leading portion to match, and the history
//! is append-only.
//!
//! # Protection
//!
//! The block is NOT a [`ContextItem`](super::ContextItem): it is re-rendered
//! from the structured plan projection on every
//! [`ContextManager::build_messages`](super::ContextManager::build_messages)
//! call, so it is structurally immune to every compaction phase:
//!
//! - the useless-chain sweep never touches it (it is not an item at all);
//! - even the LLM compaction — which hides the WHOLE pre-existing timeline
//!   behind its summary anchor — leaves it intact, because the block is
//!   re-rendered from the mirror afterwards.
//!
//! Its tokens still count toward the context budget ([`ContextManager`]'s
//! `total_tokens`), so the 80% compaction trigger stays honest — but nothing
//! can ever compact or summarize the block away.
//!
//! # Removal rules
//!
//! The block can leave the model's context ONLY under these two rules:
//!
//!   1. **All tasks are terminal** — every task is `Completed` or
//!      `Cancelled` (nothing `Pending`/`InProgress` remains): the plan is
//!      done, the block disappears.
//!   2. **The model manually removes the tasks** — the list becomes empty
//!      through the existing `Remove`/`Clean` `plan_todo_write` actions:
//!      nothing to show, the block disappears.
//!
//! The harness mirrors the tools' `Plan` state into this manager at loop
//! start and after every `plan_*` dispatch, so the block is always a faithful
//! rendering of the real todo list.

use crate::util::TokenEncoding;
use cosh_tools::plan::types::{TodoList, TodoStatus};

/// The dedicated, fully-protected TODO context block.
///
/// Holds the mirror of the tools' `Plan` list and the memoized rendering of
/// the block. See the module docs for the protection and removal rules.
#[derive(Debug, Clone, Default)]
pub struct TodoContext {
    /// The latest structured plan. `None` means no plan has been recorded;
    /// empty and completed lists are retained even when not rendered.
    list: Option<TodoList>,
    /// Memoized rendering of `list`: `Some(text)` = the block text, `None` =
    /// no block (fresh context, cleared mirror, or the plan is over). Kept
    /// because `tokens()` is read on EVERY budget check (`total_tokens`) —
    /// re-rendering + re-estimating per call would be pure waste; the text
    /// only changes when the list changes (`sync`/`clear`).
    rendered: Option<String>,
}

impl TodoContext {
    /// A fresh empty `TodoContext` (no block rendered).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the mirrored TODO list with the tools' authoritative state.
    /// Visibility follows the two removal rules automatically: the block
    /// appears for a live plan and disappears when the plan is over. The
    /// rendering is computed here, once per change.
    pub fn sync(&mut self, list: TodoList) {
        self.rendered = render(&list);
        self.list = Some(list);
    }

    /// The structured projection persisted as session deltas, not its rendering.
    pub(crate) fn list(&self) -> Option<&TodoList> {
        self.list.as_ref()
    }

    /// Drop the mirror when loading a state with no recorded plan.
    pub fn clear(&mut self) {
        self.list = None;
        self.rendered = None;
    }

    /// The block text, or `None` when not visible.
    #[must_use]
    pub(crate) fn text(&self) -> Option<String> {
        self.rendered.clone()
    }

    /// Estimated token cost of the rendered block (`0` when not visible).
    /// Counted toward the [`ContextManager`] budget so the compaction trigger
    /// stays honest while the block itself stays protected.
    #[must_use]
    pub fn tokens(&self, enc: TokenEncoding) -> usize {
        self.rendered.as_ref().map_or(0, |text| enc.estimate(text))
    }
}

/// The Markdown block for `list`, or `None` when the plan is over (empty or
/// all-terminal — removal rules 1 and 2).
fn render(list: &TodoList) -> Option<String> {
    if list.groups.is_empty() || all_terminal(list) {
        return None;
    }
    let mut out = String::from(
        "## Tool TODOs\n\
         This protected block always shows the current plan. Update it with \
         plan_todo_write / plan_todo_cross_off / plan_todo_edit.\n",
    );
    for group in &list.groups {
        if group.items.is_empty() {
            continue;
        }
        out.push_str(&format!("\n### {}\n", group.title));
        for item in &group.items {
            let mut line = format!("- [{}] {}", status_marker(item.status), item.description);
            if !item.depends_on.is_empty() {
                line.push_str(&format!("  (depends: {})", item.depends_on.join(", ")));
            }
            out.push_str(&line);
            out.push('\n');
        }
    }
    Some(out)
}

/// Checkbox marker for a task status: `[ ]` pending, `[*]` in progress,
/// `[x]` completed, `[-]` cancelled.
fn status_marker(status: TodoStatus) -> char {
    match status {
        TodoStatus::Pending => ' ',
        TodoStatus::InProgress => '*',
        TodoStatus::Completed => 'x',
        TodoStatus::Cancelled => '-',
    }
}

/// True when NO task is pending or in progress — every task is terminal
/// (completed or cancelled): the plan is done (removal rule 1).
fn all_terminal(list: &TodoList) -> bool {
    !list.groups.iter().any(|g| {
        g.items
            .iter()
            .any(|i| matches!(i.status, TodoStatus::Pending | TodoStatus::InProgress))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosh_tools::plan::types::{TaskGroup, TodoItem};

    fn enc() -> TokenEncoding {
        TokenEncoding::Cl100k
    }

    fn item(id: &str, status: TodoStatus) -> TodoItem {
        TodoItem {
            id: id.to_string(),
            description: format!("do {id}"),
            status,
            depends_on: Vec::new(),
        }
    }

    fn group(title: &str, items: Vec<TodoItem>) -> TaskGroup {
        TaskGroup {
            title: title.to_string(),
            items,
            tests_verified: false,
        }
    }

    fn list(groups: Vec<TaskGroup>) -> TodoList {
        TodoList { groups }
    }

    #[test]
    fn empty_context_renders_no_block() {
        let tc = TodoContext::new();
        assert!(tc.text().is_none());
        assert_eq!(tc.tokens(enc()), 0);
    }

    #[test]
    fn empty_list_hides_the_block() {
        let mut tc = TodoContext::new();
        tc.sync(TodoList::default());
        assert!(tc.text().is_none(), "an empty list has nothing to show");
    }

    #[test]
    fn pending_tasks_show_the_block() {
        let mut tc = TodoContext::new();
        tc.sync(list(vec![group(
            "Database",
            vec![item("task-1", TodoStatus::Pending)],
        )]));
        let text = tc.text().expect("a visible block renders");
        assert!(text.contains("## Tool TODOs"));
        assert!(text.contains("### Database"));
        assert!(text.contains("- [ ] do task-1"));
    }

    #[test]
    fn completed_tasks_stay_visible_until_everything_is_done() {
        let mut tc = TodoContext::new();
        tc.sync(list(vec![group(
            "Database",
            vec![
                item("task-1", TodoStatus::Completed),
                item("task-2", TodoStatus::Pending),
            ],
        )]));
        let text = tc
            .text()
            .expect("a single pending task keeps the block alive");
        assert!(
            text.contains("- [x] do task-1"),
            "completed tasks remain visible, crossed off"
        );
        assert!(text.contains("- [ ] do task-2"));
    }

    #[test]
    fn all_completed_hides_the_block() {
        let mut tc = TodoContext::new();
        tc.sync(list(vec![group(
            "Database",
            vec![item("task-1", TodoStatus::Completed)],
        )]));
        assert!(
            tc.text().is_none(),
            "removal rule 1: all tasks completed → the block is gone"
        );
    }

    #[test]
    fn all_cancelled_hides_the_block() {
        let mut tc = TodoContext::new();
        tc.sync(list(vec![group(
            "Database",
            vec![item("task-1", TodoStatus::Cancelled)],
        )]));
        assert!(
            tc.text().is_none(),
            "cancelled tasks are terminal too → the block is gone"
        );
    }

    #[test]
    fn empty_list_via_manual_removal_hides_the_block() {
        let mut tc = TodoContext::new();
        tc.sync(list(vec![group(
            "Database",
            vec![item("task-1", TodoStatus::Pending)],
        )]));
        assert!(tc.text().is_some());
        // The model removed the last task (Remove/Clean) → the list is empty.
        tc.sync(TodoList::default());
        assert!(
            tc.text().is_none(),
            "removal rule 2: the model emptied the plan → the block is gone"
        );
    }

    #[test]
    fn clear_drops_the_mirror() {
        let mut tc = TodoContext::new();
        tc.sync(list(vec![group(
            "Database",
            vec![item("task-1", TodoStatus::Pending)],
        )]));
        assert!(tc.text().is_some());
        tc.clear();
        assert!(tc.text().is_none());
    }

    #[test]
    fn render_includes_task_ids_and_dependencies() {
        let mut tc = TodoContext::new();
        tc.sync(list(vec![group(
            "API",
            vec![TodoItem {
                id: "task-3".to_string(),
                description: "user endpoints".to_string(),
                status: TodoStatus::InProgress,
                depends_on: vec!["task-1".to_string()],
            }],
        )]));
        let text = tc.text().unwrap();
        assert!(text.contains("- [*] user endpoints  (depends: task-1)"));
    }

    #[test]
    fn tokens_count_only_when_visible() {
        let mut tc = TodoContext::new();
        assert_eq!(tc.tokens(enc()), 0);
        tc.sync(list(vec![group(
            "Database",
            vec![item("task-1", TodoStatus::Pending)],
        )]));
        assert!(tc.tokens(enc()) > 0, "the rendered block has a token cost");
        tc.sync(list(vec![group(
            "Database",
            vec![item("task-1", TodoStatus::Completed)],
        )]));
        assert_eq!(tc.tokens(enc()), 0, "a done plan costs nothing");
    }

    // Integration with the ContextManager

    fn cm(max_tokens: usize) -> super::super::ContextManager {
        super::super::ContextManager::new(max_tokens)
    }

    #[test]
    fn build_messages_merges_the_block_into_the_trailing_user_turn() {
        let mut m = cm(10_000);
        m.add_user("hello");
        m.set_todo_list(list(vec![group(
            "Database",
            vec![item("task-1", TodoStatus::Pending)],
        )]));
        let msgs = m.build_messages("");
        // Merged: a single user message carrying the block AND the prompt —
        // never two consecutive `user` messages. The block leads the merged
        // message; the prompt follows it.
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "user");
        let text = msgs[0].content.as_deref().unwrap();
        assert!(
            text.contains("## Tool TODOs"),
            "the block leads the merged message"
        );
        assert!(text.ends_with("hello"), "the prompt follows the block");
    }

    #[test]
    fn build_messages_injects_the_block_when_the_last_turn_is_not_user() {
        let mut m = cm(10_000);
        // A restored/compacted timeline whose visible history is just the
        // assistant anchor: the block cannot be merged — it is appended as
        // its own trailing `user` message.
        m.set_todo_list(list(vec![group(
            "Database",
            vec![item("task-1", TodoStatus::Pending)],
        )]));
        m.add_user("hello");
        m.add_assistant("doing it", true);
        m.apply_llm_summary("## Objective\n- keep going".into());
        let msgs = m.build_messages("");
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "assistant");
        let last = msgs.last().unwrap();
        assert_eq!(last.role, "user");
        assert!(
            last.content.as_deref().unwrap().contains("## Tool TODOs"),
            "the block is its own trailing message here"
        );
    }

    // The whole point of the dedicated block: every compaction phase must
    // leave it untouched — the useless-chain sweep (`run`) and the LLM
    // compaction (`apply_llm_summary`, which hides the entire pre-existing
    // timeline behind its anchor).
    #[test]
    fn todo_block_survives_every_compaction_phase() {
        let mut m = cm(1000); // trigger = 800
        m.set_todo_list(list(vec![group(
            "Database",
            vec![item("task-1", TodoStatus::Pending)],
        )]));
        m.add_user("start");
        let prose: String = (0..40)
            .map(|i| {
                format!(
                    "Paragraph {i} discusses the interplay between rivers, mountains \
                     and the changing weather patterns that define the region. "
                )
            })
            .collect();
        m.add_assistant(&prose, true);
        m.add_tool_call("t0", "find_grep", "{}");
        m.add_tool_result_flagged("t0", "no matches", true);
        assert!(m.total_tokens() >= 800, "precondition: over the trigger");

        // The useless-chain sweep: the block survives.
        m.run();
        assert!(
            m.build_messages("")
                .last()
                .unwrap()
                .content
                .as_deref()
                .unwrap()
                .contains("## Tool TODOs")
        );

        // Phase 3 (LLM compaction hides the WHOLE pre-existing timeline):
        // the block is re-rendered from the mirror and survives.
        m.apply_llm_summary("## Objective\n- keep going".to_string());
        assert!(
            m.build_messages("")
                .last()
                .unwrap()
                .content
                .as_deref()
                .unwrap()
                .contains("## Tool TODOs"),
            "even the LLM compaction cannot remove the TODO block"
        );

        // The useless-chain sweep ignores the block too.
        m.add_tool_call("t1", "fs_read", "{}");
        m.add_tool_result_flagged("t1", "no matches", true);
        m.sweep_useless_chains();
        assert!(
            m.build_messages("")
                .last()
                .unwrap()
                .content
                .as_deref()
                .unwrap()
                .contains("## Tool TODOs")
        );
    }

    #[test]
    fn todo_block_tokens_count_toward_the_budget() {
        let mut m = cm(10_000);
        assert_eq!(m.display_info().total_tokens, 0);
        m.set_todo_list(list(vec![group(
            "Database",
            vec![item("task-1", TodoStatus::Pending)],
        )]));
        assert!(
            m.display_info().total_tokens > 0,
            "the block counts toward the budget so the trigger stays honest"
        );
    }

    #[test]
    fn restore_state_preserves_the_protected_plan() {
        let mut m = cm(10_000);
        m.set_todo_list(list(vec![group(
            "Database",
            vec![item("task-1", TodoStatus::Pending)],
        )]));
        let state = m.save_state();
        let mut restored = cm(10_000);
        restored.restore_state(&state);
        let msgs = restored.build_messages("");
        assert!(
            msgs.iter().any(|msg| msg
                .content
                .as_deref()
                .is_some_and(|c| c.contains("## Tool TODOs"))),
            "a restored session must preserve its protected plan"
        );
    }

    #[test]
    fn empty_and_terminal_plans_restore_without_resurrecting_old_work() {
        for plan in [
            TodoList::default(),
            list(vec![group(
                "Done",
                vec![item("task-1", TodoStatus::Completed)],
            )]),
        ] {
            let mut source = cm(10_000);
            source.set_todo_list(plan.clone());
            let state = serde_json::from_str(&serde_json::to_string(&source.save_state()).unwrap())
                .unwrap();
            let mut restored = cm(10_000);
            restored.set_todo_list(list(vec![group(
                "Stale",
                vec![item("old", TodoStatus::Pending)],
            )]));
            restored.restore_state(&state);
            assert_eq!(
                serde_json::to_value(restored.todo_list()).unwrap(),
                serde_json::to_value(Some(plan)).unwrap()
            );
            assert!(restored.todo.text().is_none());
        }
    }

    #[test]
    fn legacy_state_without_plan_clears_the_previous_projection() {
        let mut restored = cm(10_000);
        restored.set_todo_list(list(vec![group(
            "Stale",
            vec![item("old", TodoStatus::Pending)],
        )]));
        let mut state = serde_json::to_value(restored.save_state()).unwrap();
        state.as_object_mut().unwrap().remove("todo");
        restored.restore_state(&serde_json::from_value(state).unwrap());
        assert!(restored.todo_list().is_none());
        assert!(restored.todo.text().is_none());
    }
}
