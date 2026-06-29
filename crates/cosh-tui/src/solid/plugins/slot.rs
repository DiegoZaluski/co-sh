use std::collections::HashMap;

use crate::core::renderable::Renderable;

/// How a slot should resolve its entries when multiple are registered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SlotMode {
    /// Render the first registered entry only.
    SingleWinner,
    /// Render all registered entries, replacing the previous output.
    Replace,
    /// Append each entry's output (default).
    #[default]
    Append,
}

/// Result of resolving a single slot entry.
pub struct ResolvedEntry {
    pub id: String,
    pub renderable: Box<dyn Renderable>,
}

/// Error event reported when a plugin fails during rendering.
#[derive(Debug, Clone)]
pub struct PluginErrorEvent {
    pub plugin_id: String,
    pub slot_name: String,
    pub phase: String,
    pub source: String,
    pub error: String,
}

/// A function that produces a renderable for a slot entry.
pub type SlotRenderer = Box<dyn Fn() -> Box<dyn Renderable>>;

type ErrorHandler = Box<dyn Fn(&PluginErrorEvent)>;

/// Registry that maps slot names to lists of registered renderers.
///
/// Analogous to `createSlotRegistry` from `@opentui/core`.  This version
/// is synchronous and does not integrate with a reactive framework.
pub struct SlotRegistry {
    entries: HashMap<String, Vec<(String, SlotRenderer)>>,
    error_handlers: Vec<ErrorHandler>,
}

impl SlotRegistry {
    #[must_use]
    pub fn new() -> Self {
        SlotRegistry {
            entries: HashMap::new(),
            error_handlers: Vec::new(),
        }
    }

    /// Register a renderer for the given slot name.
    pub fn register(&mut self, slot_name: &str, id: &str, renderer: SlotRenderer) {
        self.entries
            .entry(slot_name.to_string())
            .or_default()
            .push((id.to_string(), renderer));
    }

    /// Resolve all entries for a slot name according to the given mode.
    #[must_use]
    pub fn resolve(&self, slot_name: &str, mode: SlotMode) -> Vec<ResolvedEntry> {
        let Some(entry_list) = self.entries.get(slot_name) else {
            return Vec::new();
        };

        match mode {
            SlotMode::SingleWinner => entry_list
                .first()
                .map(|(id, renderer)| ResolvedEntry {
                    id: id.clone(),
                    renderable: renderer(),
                })
                .into_iter()
                .collect(),
            SlotMode::Replace | SlotMode::Append => entry_list
                .iter()
                .map(|(id, renderer)| ResolvedEntry {
                    id: id.clone(),
                    renderable: renderer(),
                })
                .collect(),
        }
    }

    /// Register an error handler.
    pub fn on_error(&mut self, handler: ErrorHandler) {
        self.error_handlers.push(handler);
    }

    /// Report a plugin error to all registered error handlers.
    pub fn report_error(&self, event: &PluginErrorEvent) {
        for handler in &self.error_handlers {
            handler(event);
        }
    }

    /// Remove all entries for a slot name.
    pub fn unregister(&mut self, slot_name: &str) {
        self.entries.remove(slot_name);
    }

    /// Check whether a slot has any registered entries.
    #[must_use]
    pub fn has_entries(&self, slot_name: &str) -> bool {
        self.entries.get(slot_name).is_some_and(|e| !e.is_empty())
    }

    /// Number of registered slots.
    #[must_use]
    pub fn slot_count(&self) -> usize {
        self.entries.len()
    }
}

impl Default for SlotRegistry {
    fn default() -> Self {
        Self::new()
    }
}
