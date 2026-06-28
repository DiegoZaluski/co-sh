//! Input types for keyboard and mouse
//! Inline replacement for wezterm-input-types and `termwiz::input` types

use bitflags::bitflags;
use serde::{Deserialize, Serialize};

bitflags! {
    /// Keyboard modifier key flags
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub struct Modifiers: u8 {
        const NONE = 0;
        const SHIFT = 0b0000_0001;
        const ALT = 0b0000_0010;
        const CTRL = 0b0000_0100;
        const SUPER = 0b0000_1000;
        const HYPER = 0b0001_0000;
        const META = 0b0010_0000;
        const CAPS_LOCK = 0b0100_0000;
        const NUM_LOCK = 0b1000_0000;
    }
}

impl Default for Modifiers {
    fn default() -> Self {
        Modifiers::NONE
    }
}

bitflags! {
    /// Kitty keyboard protocol flags
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct KittyKeyboardFlags: u16 {
        const NONE = 0;
        const DISAMBIGUATE_ESCAPE_CODES = 1;
        const REPORT_EVENT_TYPES = 2;
        const REPORT_ALTERNATE_KEYS = 4;
        const REPORT_ALL_KEYS_AS_ESCAPE_CODES = 8;
        const REPORT_ASSOCIATED_TEXT = 16;
    }
}

impl Default for KittyKeyboardFlags {
    fn default() -> Self {
        KittyKeyboardFlags::NONE
    }
}

/// Keyboard encoding modes (replacement for `termwiz::input::KeyboardEncoding`)
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum KeyboardEncoding {
    Xterm,
    CsiU,
    Kitty(KittyKeyboardFlags),
    Win32,
}

/// Key code enum (minimal stub for `termwiz::input::KeyCode`)
/// Used by core for keyboard encoding
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum KeyCode {
    Char(char),
    Enter,
    Tab,
    Backspace,
    Escape,
    UpArrow,
    DownArrow,
    LeftArrow,
    RightArrow,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    F(u8),
    ApplicationUpArrow,
    ApplicationDownArrow,
    ApplicationLeftArrow,
    ApplicationRightArrow,
    NumpadUpArrow,
    NumpadDownArrow,
    NumpadLeftArrow,
    NumpadRightArrow,
    KeypadBegin,
    KeypadEnter,
    KeypadUpArrow,
    KeypadDownArrow,
    KeypadLeftArrow,
    KeypadRightArrow,
    KeypadHome,
    KeypadEnd,
    KeypadPageUp,
    KeypadPageDown,
    KeypadInsert,
    KeypadDelete,
    KeypadCenter,
    KeypadPlus,
    KeypadMinus,
    KeypadMultiply,
    KeypadDivide,
    KeypadNumLock,
    CapsLock,
    ScrollLock,
    NumLock,
    PrintScreen,
    Pause,
    Menu,
    Cancel,
    Clear,
    Prior,
    Return,
    Separator,
    Out,
    Oper,
    Again,
    Undo,
    Find,
    Help,
    Social(String),
    Physical(String),
    PageForward,
    PageBackward,
    ApplicationLeft,
    ApplicationRight,
    ApplicationUp,
    ApplicationDown,
    Null,
    BrowserBack,
    BrowserForward,
    BrowserRefresh,
    BrowserStop,
    BrowserSearch,
    BrowserFavorites,
    BrowserHome,
    VolumeMute,
    VolumeDown,
    VolumeUp,
    MediaNextTrack,
    MediaPrevTrack,
    MediaStop,
    MediaPlayPause,
    MediaSelect,
    Mail,
    Calculator,
    MyComputer,
    WWW,
    AppMenu,
    Terminal,
    Sleep,
    Wakup,
    LocalScreenBrightnessDown,
    LocalScreenBrightnessUp,
    KeyboardBrightnessDown,
    KeyboardBrightnessUp,
    Play,
    PauseMedia,
    Record,
    FastForward,
    Rewind,
    AppSwitch,
    AppSkipBack,
    AppSkipForward,
    AppMove,
    AppExpose,
    RotationLock,
    LaunchApplication1,
    LaunchApplication2,
    LaunchApplication3,
    LaunchApplication4,
    LaunchApplication5,
    LaunchApplication6,
    LaunchApplication7,
    LaunchApplication8,
    LaunchApplication9,
    LaunchApplication10,
    LaunchApplication11,
    LaunchApplication12,
    LaunchApplication13,
    LaunchApplication14,
    LaunchApplication15,
    LaunchApplication16,
}

/// Encoding modes for key events (replacement for `termwiz::input::KeyCodeEncodeModes`)
#[derive(Debug, Clone)]
pub struct KeyCodeEncodeModes {
    pub key: KeyCode,
    pub mods: Modifiers,
    pub encoding: KeyboardEncoding,
    pub newline_mode: bool,
    pub application_cursor_keys: bool,
    pub modify_other_keys: bool,
}

impl KeyCode {
    /// Stub implementation that always returns an empty sequence.
    /// # Errors
    /// This implementation never returns an error.
    pub fn encode(
        &self,
        _mods: Modifiers,
        _modes: KeyCodeEncodeModes,
        _is_down: bool,
    ) -> anyhow::Result<String> {
        // Minimal stub: output a placeholder sequence.
        // A full implementation would map to xterm/CSI-u/kitty encodings.
        Ok(String::new())
    }
}
