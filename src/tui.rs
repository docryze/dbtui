//! Terminal lifecycle: an RAII guard that enables raw mode and the alternate
//! screen on construction and restores the terminal on drop — even on panic.

use std::io::{self, Stderr};

use color_eyre::Result;
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

/// The concrete terminal type used throughout dbtui.
pub type Tui = Terminal<CrosstermBackend<Stderr>>;

/// RAII guard owning the terminal in raw-mode + alternate-screen state.
///
/// Construct with [`TerminalGuard::setup`]; draw through
/// [`TerminalGuard::terminal_mut`]. When the guard is dropped (including on
/// unwind from a panic) the terminal is restored to its original state.
pub struct TerminalGuard {
    terminal: Tui,
}

impl TerminalGuard {
    /// Enable raw mode, enter the alternate screen, and return a guard whose
    /// `Drop` reverses both.
    ///
    /// # Errors
    /// Returns an error if enabling raw mode or entering the alternate screen
    /// fails.
    pub fn setup() -> Result<Self> {
        enable_raw_mode()?;
        execute!(io::stderr(), EnterAlternateScreen)?;
        let terminal = Terminal::new(CrosstermBackend::new(io::stderr()))?;
        Ok(Self { terminal })
    }

    /// Borrow the inner terminal mutably for drawing.
    pub fn terminal_mut(&mut self) -> &mut Tui {
        &mut self.terminal
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // Best-effort restore. `Drop` cannot propagate errors, so failures are
        // intentionally discarded — restoring the terminal is a best-effort
        // cleanup that must not panic.
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}
