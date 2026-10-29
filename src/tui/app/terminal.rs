use std::io;
use std::io::Write;

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

pub(super) fn init_terminal() -> io::Result<Terminal<CrosstermBackend<io::Stdout>>> {
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(
        stdout,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableFocusChange,
        crossterm::event::EnableBracketedPaste,
        crossterm::event::EnableMouseCapture,
    )?;
    // Bracketed paste stays enabled on ALL platforms, Windows included: with
    // the patched crossterm (see `[patch.crates-io]` in Cargo.toml —
    // upstream PR crossterm-rs/crossterm#1030) `EnableBracketedPaste` scopes
    // ENABLE_VIRTUAL_TERMINAL_INPUT to its own lifetime, so modern Windows
    // terminals deliver a real `Event::Paste` exactly like Unix — the prompt
    // placeholder lands atomically with zero streaming. Stock crossterm
    // could not decode the markers (#737) and the request even corrupted
    // pasted newlines into Ctrl+Enter (#962); both are fixed by the patch.
    // Consoles without VT input (legacy conhost) still yield the old
    // VK-record stream, covered by the paste-burst coalescer fallback.
    // Enable keyboard enhancement on all platforms (Windows Terminal + Kitty protocol)
    // This allows detecting modifier+key combinations like Ctrl+Backspace.
    //
    // REPORT_ALL_KEYS_AS_ESCAPE_CODES rides in the SAME push (one push, one
    // pop — the restore path below pops exactly once): without it, keys
    // with no legacy byte encoding never arrive with their modifiers.
    // `Ctrl+Backspace` is the prime case — plain terminals deliver a bare
    // `0x08` (indistinguishable from Backspace) — and the same applies to
    // the Alt+digit /agents pane hotkeys and the Enter/arrow chords the
    // keymap distinguishes. With the full protocol, supporting terminals
    // send `CSI 13;2u`-style sequences, which crossterm parses with the
    // modifiers intact. Terminals
    // without Kitty-protocol support ignore the request and behave exactly
    // as before (the plain-digit fallback in `focus_pane_hotkey` still
    // applies while the draft is empty).
    let _ = crossterm::execute!(
        stdout,
        crossterm::event::PushKeyboardEnhancementFlags(
            crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | crossterm::event::KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES,
        ),
    );
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

pub(super) fn restore_terminal() -> io::Result<()> {
    let mut stdout = io::stdout();
    crossterm::execute!(
        stdout,
        crossterm::event::DisableMouseCapture,
        crossterm::event::DisableFocusChange,
        crossterm::event::DisableBracketedPaste,
        crossterm::terminal::LeaveAlternateScreen,
    )?;
    // Mirrors `init_terminal`: with the patched crossterm the disable also
    // restores the VT-input bit it scoped (upstream PR #1030 lifecycle).
    let _ = crossterm::execute!(stdout, crossterm::event::PopKeyboardEnhancementFlags,);
    stdout.flush()?;
    crossterm::terminal::disable_raw_mode()?;
    Ok(())
}
