//! System-context assembly: the correction block, the /agents panes block
//! and their tail-rendered placement in the conversation.

use super::Harness;

impl Harness {
    /// Build the system context for the LLM.
    ///
    /// Returns the header context (instructions + tool definitions). The
    /// correction memory is NOT part of it: it churns on every tool failure
    /// (new entry, duplicate reordering, eviction), and the system prompt
    /// sits at position 0 — any change there invalidates the exact-prefix
    /// cache of the WHOLE conversation. The block is mirrored into the
    /// [`ContextManager`] instead and rendered at the TAIL of the messages
    /// (before the TODO block), where a change only costs the tail itself.
    /// The conversation itself lives entirely in the [`ContextManager`] and
    /// is delivered as structured messages via
    /// [`stream_chat_with_messages`](Self::stream_chat_with_messages) — never
    /// duplicated into the system prompt.
    pub(super) fn build_chat_context(&mut self) -> String {
        self.context_manager
            .set_correction_block(self.correction_memory.format());
        // Mirror the /agents panes listing into the context manager's TAIL
        // (rendered between the corrections and the TODO block). Rebuilt
        // per request: a pane bind/unbind changes only the tail, never the
        // system prompt — the exact-prefix cache of the whole conversation
        // stays valid. The MAIN loop mirrors the full listing (the
        // addressable pane set — it owns coordination). A PANE loop mirrors
        // the reduced peer view instead: WHICH siblings share its
        // workspace, without the `agents_call` vocabulary (the replication
        // bug `with_own_pane_id` documents) — a pane knows its peers but
        // never coordinates them. ACP panes never run this harness: their
        // awareness rides a prompt preamble (`panes::take_preamble`) at
        // dispatch time instead.
        self.context_manager.set_panes_block(match &self.panes_own_id {
            None => super::panes::context_block(),
            Some(own_id) => super::panes::peer_block(own_id),
        });
        self.header_context.clone()
    }
}
