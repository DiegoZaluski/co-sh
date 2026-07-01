use crossterm::event::{KeyCode, KeyModifiers};

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Quit,
    ScrollUp,
    ScrollDown,
    ScrollUpPage,
    ScrollDownPage,
    ScrollToTop,
    ScrollToBottom,
    SendMessage,
    FocusInput,
    ToggleSidebar,
    ToggleHelp,
    ToggleConceal,
    ToggleThinking,
    ToggleToolDetails,
    ToggleGenericToolOutput,
    ToggleTimestamps,
    Confirm,
    Cancel,
    Interrupt,
    NextAgent,
    PrevAgent,
    HistoryUp,
    HistoryDown,
    ToggleCommandPalette,
    NextSession,
    PrevSession,
}

#[derive(Debug, Clone)]
pub struct KeyBinding {
    pub key: KeyCode,
    pub modifiers: KeyModifiers,
}

#[derive(Debug, Clone)]
pub struct KeyMap {
    pub bindings: Vec<(Action, KeyBinding)>,
}

impl KeyMap {
    pub fn default_vim() -> Self {
        use KeyCode::*;

        KeyMap {
            bindings: vec![

                (Action::ScrollUp, KeyBinding { key: Up, modifiers: KeyModifiers::NONE }),
                (Action::ScrollUp, KeyBinding { key: Char('k'), modifiers: KeyModifiers::NONE }),
                (Action::ScrollDown, KeyBinding { key: Down, modifiers: KeyModifiers::NONE }),
                (Action::ScrollDown, KeyBinding { key: Char('j'), modifiers: KeyModifiers::NONE }),
                (Action::ScrollUpPage, KeyBinding { key: PageUp, modifiers: KeyModifiers::NONE }),
                (Action::ScrollDownPage, KeyBinding { key: PageDown, modifiers: KeyModifiers::NONE }),
                (Action::ScrollToTop, KeyBinding { key: Home, modifiers: KeyModifiers::NONE }),
                (Action::ScrollToBottom, KeyBinding { key: End, modifiers: KeyModifiers::NONE }),
                (Action::SendMessage, KeyBinding { key: Enter, modifiers: KeyModifiers::NONE }),
                (Action::ToggleSidebar, KeyBinding { key: Char('b'), modifiers: KeyModifiers::CONTROL }),
                (Action::ToggleHelp, KeyBinding { key: Char('?'), modifiers: KeyModifiers::NONE }),
                (Action::ToggleConceal, KeyBinding { key: Char('c'), modifiers: KeyModifiers::CONTROL }),
                (Action::ToggleThinking, KeyBinding { key: Char('t'), modifiers: KeyModifiers::CONTROL }),
                (Action::ToggleToolDetails, KeyBinding { key: Char('d'), modifiers: KeyModifiers::CONTROL }),
                (Action::ToggleGenericToolOutput, KeyBinding { key: Char('g'), modifiers: KeyModifiers::CONTROL }),
                (Action::ToggleTimestamps, KeyBinding { key: Char('y'), modifiers: KeyModifiers::CONTROL }),
                (Action::ToggleCommandPalette, KeyBinding { key: Char('p'), modifiers: KeyModifiers::CONTROL }),
                (Action::Confirm, KeyBinding { key: Enter, modifiers: KeyModifiers::NONE }),
                (Action::Cancel, KeyBinding { key: Esc, modifiers: KeyModifiers::NONE }),
                (Action::Interrupt, KeyBinding { key: Esc, modifiers: KeyModifiers::NONE }),
                (Action::NextAgent, KeyBinding { key: Tab, modifiers: KeyModifiers::NONE }),
                (Action::PrevAgent, KeyBinding { key: BackTab, modifiers: KeyModifiers::NONE }),
                (Action::HistoryUp, KeyBinding { key: Up, modifiers: KeyModifiers::CONTROL }),
                (Action::HistoryDown, KeyBinding { key: Down, modifiers: KeyModifiers::CONTROL }),
                (Action::NextSession, KeyBinding { key: Char('n'), modifiers: KeyModifiers::NONE }),
                (Action::PrevSession, KeyBinding { key: Char('p'), modifiers: KeyModifiers::NONE }),
            ],
        }
    }

    pub fn lookup(&self, key: KeyCode, modifiers: KeyModifiers) -> Option<&Action> {
        self.bindings
            .iter()
            .find(|(_, binding)| binding.key == key && binding.modifiers == modifiers)
            .map(|(action, _)| action)
    }
}
