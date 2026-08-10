//! Dedicated protected context block for tool TODOs.
//!
//! The [`ContextManager`](super::ContextManager) is the single owner of the
//! conversation timeline. This separate mini-manager owns ONLY the tool TODO
//! list — the plan the agent is executing — and renders it as a single
//! protected `user` message injected at the FRONT of the message list: right
//! after the system prompt (which the harness passes separately), before the
//! conversation history. The model therefore always sees the current plan
//! without needing to call `plan_todo_read` first, and cannot forget it.
//!
//! # Protection
//!
//! The block is NOT a [`ContextItem`](super::ContextItem): it is re-rendered
//! live from the tools' authoritative `Plan` state on every
//! [`ContextManager::build_messages`](super::ContextManager::build_messages)
//! call, so it is structurally immune to every compaction phase:
//!
//! - the TF-IDF → LSA → MMR pipeline never sees it (it is not an `Assistant`
//!   draft);
//! - the draft eviction and the useless-chain sweep never touch it (it is not
//!   an item at all);
//! - the tool-chain overflow drain ignores it;
//! - even the LLM compaction — which replaces the WHOLE timeline with a
//!   summary — leaves it intact, because the block is re-rendered from the
//!   mirror afterwards.
//!
//! Its tokens still count toward the context budget ([`ContextManager`]'s
//! `total_tokens`), so the 80% compaction trigger stays honest — but nothing
//! can ever compact, evict or summarize the block away.
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
use cosh_sdk::connector::{ChatMessage, user_message};
use cosh_tools::plan::types::{TodoList, TodoStatus};

/// The dedicated, fully-protected TODO context block.
///
/// Holds the mirror of the tools' `Plan` list and renders the block message
/// on demand. See the module docs for the protection and removal rules.
#[derive(Debug, Clone, Default)]
pub struct TodoContext {
    /// The latest mirrored TODO list from the tools' `Plan` state. `None`
    /// means the block is not rendered (nothing to show / plan over).
    list: Option<TodoList>,
}

impl TodoContext {
    /// A fresh empty `TodoContext` (no block rendered).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the mirrored TODO list with the tools' authoritative state.
    /// Visibility follows the two removal rules automatically: the block
    /// appears for a live plan and disappears when the plan is over.
    pub fn sync(&mut self, list: TodoList) {
        self.list = Some(list);
    }

    /// Drop the mirror entirely. Used when a session is restored — the
    /// harness re-syncs from the tools' `Plan` at the next loop start, so a
    /// restored session never surfaces a stale block.
    pub fn clear(&mut self) {
        self.list = None;
    }

    /// True when the block must be rendered: a non-empty list with at least
    /// one task that is not terminal. An empty list or an all-terminal list
    /// means the plan is over — the block is gone (removal rules 1 and 2).
    #[must_use]
    pub fn visible(&self) -> bool {
        self.render().is_some()
    }

    /// Render the block as a provider-ready `user` message, or `None` when
    /// the block is not visible (the plan is over).
    #[must_use]
    pub fn message(&self) -> Option<ChatMessage> {
        self.render().map(|text| user_message(&text))
    }

    /// Estimated token cost of the rendered block (`0` when not visible).
    /// Counted toward the [`ContextManager`] budget so the compaction trigger
    /// stays honest while the block itself stays protected.
    #[must_use]
    pub fn tokens(&self, enc: TokenEncoding) -> usize {
        self.render().map_or(0, |text| enc.estimate(&text))
    }

    /// The Markdown block, or `None` when not visible.
    fn render(&self) -> Option<String> {
        let list = self.list.as_ref()?;
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
        assert!(!tc.visible());
        assert!(tc.message().is_none());
        assert_eq!(tc.tokens(enc()), 0);
    }

    #[test]
    fn empty_list_hides_the_block() {
        let mut tc = TodoContext::new();
        tc.sync(TodoList::default());
        assert!(!tc.visible(), "an empty list has nothing to show");
        assert!(tc.message().is_none());
    }

    #[test]
    fn pending_tasks_show_the_block() {
        let mut tc = TodoContext::new();
        tc.sync(list(vec![group(
            "Database",
            vec![item("task-1", TodoStatus::Pending)],
        )]));
        assert!(tc.visible());
        let msg = tc.message().expect("a visible block renders");
        assert_eq!(msg.role, "user");
        let text = msg.content.unwrap_or_default();
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
        assert!(
            tc.visible(),
            "a single pending task keeps the whole block alive"
        );
        let text = tc.message().unwrap().content.unwrap();
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
            !tc.visible(),
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
            !tc.visible(),
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
        assert!(tc.visible());
        // The model removed the last task (Remove/Clean) → the list is empty.
        tc.sync(TodoList::default());
        assert!(
            !tc.visible(),
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
        assert!(tc.visible());
        tc.clear();
        assert!(!tc.visible());
        assert!(tc.message().is_none());
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
        let text = tc.message().unwrap().content.unwrap();
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
    fn build_messages_injects_the_block_at_the_front() {
        let mut m = cm(10_000);
        m.add_user("hello");
        m.set_todo_list(list(vec![group(
            "Database",
            vec![item("task-1", TodoStatus::Pending)],
        )]));
        let msgs = m.build_messages("");
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "user");
        assert!(
            msgs[0]
                .content
                .as_deref()
                .unwrap()
                .contains("## Tool TODOs"),
            "the block is the FIRST message — right after the system prompt"
        );
        assert_eq!(msgs[1].content.as_deref(), Some("hello"));
    }

    // The whole point of the dedicated block: EVERY compaction phase must
    // leave it untouched — the pipeline / draft eviction (`run`), the LLM
    // compaction (`apply_llm_summary`, which replaces the entire timeline)
    // and the tool-chain overflow drain.
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

        // Phases 1-2 (pipeline + draft eviction): the block survives.
        m.run();
        assert!(
            m.build_messages("")[0]
                .content
                .as_deref()
                .unwrap()
                .contains("## Tool TODOs")
        );

        // Phase 3 (LLM compaction replaces the WHOLE timeline): the block is
        // re-rendered from the mirror and survives.
        m.apply_llm_summary("## Objective\n- keep going".to_string());
        assert!(
            m.build_messages("")[0]
                .content
                .as_deref()
                .unwrap()
                .contains("## Tool TODOs"),
            "even the LLM compaction cannot remove the TODO block"
        );

        // The tool-chain overflow drain ignores the block too.
        m.add_tool_call("t1", "fs_read", "{}");
        m.add_tool_result("t1", "contents");
        m.evict_tool_chain_for_overflow();
        assert!(
            m.build_messages("")[0]
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
    fn restore_state_clears_the_stale_todo_mirror() {
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
            !msgs.iter().any(|msg| msg
                .content
                .as_deref()
                .is_some_and(|c| c.contains("## Tool TODOs"))),
            "a restored session must not surface a stale block — the harness \
             re-syncs it at the next loop start"
        );
    }
}
