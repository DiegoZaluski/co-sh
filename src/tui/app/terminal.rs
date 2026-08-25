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
    let _ = crossterm::execute!(
        stdout,
        crossterm::event::PushKeyboardEnhancementFlags(
            crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES,
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
