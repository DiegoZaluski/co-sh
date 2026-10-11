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
    // Enable keyboard enhancement on all platforms (Windows Terminal + Kitty protocol)
    // This allows detecting modifier+key combinations like Ctrl+Backspace.
    //
    // REPORT_ALL_KEYS_AS_ESCAPE_CODES rides in the SAME push (one push, one
    // pop — the restore path below pops exactly once): without it, keys
    // with no legacy byte encoding never arrive with their modifiers.
    // `Ctrl+0` is the prime case — it has no ASCII control code, so plain
    // terminals deliver a bare `0` (indistinguishable from typing) or the
    // emulator eats it for zoom; with the full protocol, supporting
    // terminals send `CSI 48;5u`, which crossterm parses as `Char('0')` +
    // CONTROL and the /agents pane hotkeys work as documented. Terminals
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
    let _ = crossterm::execute!(stdout, crossterm::event::PopKeyboardEnhancementFlags,);
    stdout.flush()?;
    crossterm::terminal::disable_raw_mode()?;
    Ok(())
}
