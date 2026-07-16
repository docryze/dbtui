//! Query editor panel — single-line SQL input with cursor.
//!
//! Handles character input, backspace, cursor movement, and Enter to execute.

use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::components::{AppContext, Component, Panel};
use crate::event::{Action, Event};

/// Prompt prefix shown before the SQL text.
const PROMPT: &str = "> ";

/// Maximum number of history entries.
const MAX_HISTORY: usize = 50;

/// Right-top panel for SQL editing.
#[derive(Debug)]
pub struct QueryEditor {
    /// Text buffer.
    buffer: String,
    /// Cursor byte offset into `buffer`.
    cursor: usize,
    /// Query execution history (most recent last).
    history: Vec<String>,
    /// Position in history. `None` = showing the current (unsaved) buffer.
    history_index: Option<usize>,
    /// Saved buffer content when first entering history navigation.
    saved_working: String,
    /// Maximum number of history entries.
    max_history: usize,
}

impl QueryEditor {
    /// Insert a character at the cursor.
    fn insert_char(&mut self, ch: char) {
        self.buffer.insert(self.cursor, ch);
        self.cursor = self.cursor.saturating_add(ch.len_utf8());
    }

    /// Delete the character before the cursor.
    fn backspace(&mut self) {
        if self.cursor > 0 {
            let prev_len = self.buffer[..self.cursor]
                .chars()
                .last()
                .map_or(0, char::len_utf8);
            self.cursor = self.cursor.saturating_sub(prev_len);
            self.buffer.remove(self.cursor);
        }
    }

    /// Move cursor left by one character.
    fn cursor_left(&mut self) {
        if self.cursor > 0 {
            let prev_len = self.buffer[..self.cursor]
                .chars()
                .last()
                .map_or(0, char::len_utf8);
            self.cursor = self.cursor.saturating_sub(prev_len);
        }
    }

    /// Move cursor right by one character.
    fn cursor_right(&mut self) {
        if self.cursor < self.buffer.len() {
            let next_len = self.buffer[self.cursor..]
                .chars()
                .next()
                .map_or(0, char::len_utf8);
            self.cursor = self.cursor.saturating_add(next_len);
        }
    }

    /// Character count before cursor (for display positioning).
    fn cursor_display_offset(&self) -> usize {
        self.buffer[..self.cursor]
            .chars()
            .count()
            .saturating_add(PROMPT.len())
    }

    /// Replace the buffer contents (e.g. from schema tree table selection).
    pub fn set_text(&mut self, text: String) {
        self.buffer = text;
        self.cursor = self.buffer.len();
        // Reset history navigation when external text is set.
        self.history_index = None;
    }

    /// Save a query to the history stack. Called when a query is executed.
    pub fn push_history(&mut self, sql: &str) {
        let trimmed = sql.trim();
        if trimmed.is_empty() {
            return;
        }
        // Avoid duplicate consecutive entries.
        if self.history.last().is_some_and(|last| last == trimmed) {
            self.history_index = None;
            return;
        }
        self.history.push(trimmed.to_string());
        if self.history.len() > self.max_history {
            self.history.remove(0);
        }
        self.history_index = None;
    }

    /// Navigate to a history entry. `delta` is -1 (older) or +1 (newer).
    fn navigate_history(&mut self, delta: isize) {
        let hist_len = self.history.len();
        if hist_len == 0 {
            return;
        }

        match self.history_index {
            None => {
                // Save current working buffer first.
                self.saved_working = self.buffer.clone();
                let idx = if delta < 0 { hist_len.saturating_sub(1) } else { 0 };
                if let Some(entry) = self.history.get(idx) {
                    self.buffer = entry.clone();
                    self.cursor = self.buffer.len();
                    self.history_index = Some(idx);
                }
            }
            Some(idx) => {
                let new_idx = if delta < 0 {
                    idx.checked_sub(1)
                } else {
                    let next = idx.checked_add(1);
                    if next.is_none_or(|n| n >= hist_len) {
                        // Past the newest entry — restore saved working buffer.
                        self.buffer = std::mem::take(&mut self.saved_working);
                        self.cursor = self.buffer.len();
                        self.history_index = None;
                        return;
                    }
                    next
                };
                if let Some(new_idx) = new_idx
                    && let Some(entry) = self.history.get(new_idx)
                {
                    self.buffer = entry.clone();
                    self.cursor = self.buffer.len();
                    self.history_index = Some(new_idx);
                }
            }
        }
    }
}

impl Default for QueryEditor {
    fn default() -> Self {
        Self {
            buffer: String::new(),
            cursor: 0,
            history: Vec::new(),
            history_index: None,
            saved_working: String::new(),
            max_history: MAX_HISTORY,
        }
    }
}

impl Component for QueryEditor {
    fn render(&self, frame: &mut Frame<'_>, area: Rect, ctx: &AppContext<'_>) {
        let focused = ctx.focus == Panel::QueryEditor;
        let border_color = if focused {
            ctx.theme.border_focused
        } else {
            ctx.theme.border_normal
        };

        let title = if self.history_index.is_some() {
            " Query (history) "
        } else {
            " Query "
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(Style::default().fg(border_color));

        let inner = block.inner(area);
        let display = format!("{PROMPT}{}", self.buffer);
        frame.render_widget(Paragraph::new(display).block(block), area);

        // Position the terminal cursor when focused.
        if focused {
            let offset = self.cursor_display_offset();
            let cursor_x = inner
                .x
                .saturating_add(u16::try_from(offset).unwrap_or(u16::MAX));
            frame.set_cursor_position((cursor_x, inner.y));
        }
    }

    fn handle_event(&mut self, event: &Event, ctx: &AppContext<'_>) -> Action {
        if ctx.focus != Panel::QueryEditor {
            return Action::None;
        }

        let Event::Key(key) = event else {
            return Action::None;
        };
        if key.kind != KeyEventKind::Press {
            return Action::None;
        }

        match key.code {
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Typing resets history navigation.
                self.history_index = None;
                self.insert_char(ch);
                Action::RequestRender
            }
            KeyCode::Backspace => {
                self.backspace();
                Action::RequestRender
            }
            KeyCode::Up => {
                self.navigate_history(-1);
                Action::RequestRender
            }
            KeyCode::Down => {
                self.navigate_history(1);
                Action::RequestRender
            }
            KeyCode::Left => {
                self.cursor_left();
                Action::RequestRender
            }
            KeyCode::Right => {
                self.cursor_right();
                Action::RequestRender
            }
            KeyCode::Home => {
                self.cursor = 0;
                Action::RequestRender
            }
            KeyCode::End => {
                self.cursor = self.buffer.len();
                Action::RequestRender
            }
            KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.buffer.clear();
                self.cursor = 0;
                Action::RequestRender
            }
            KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Delete word backward (until previous space or beginning).
                if self.cursor > 0 {
                    let before = &self.buffer[..self.cursor];
                    let word_start = before
                        .char_indices()
                        .rev()
                        .skip_while(|(_, ch)| ch.is_whitespace())
                        .skip_while(|(_, ch)| !ch.is_whitespace())
                        .map(|(i, _)| i)
                        .next()
                        .unwrap_or(0);
                    self.buffer.drain(word_start..self.cursor);
                    self.cursor = word_start;
                }
                Action::RequestRender
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Delete from cursor to beginning of line.
                self.buffer.drain(..self.cursor);
                self.cursor = 0;
                Action::RequestRender
            }
            KeyCode::Enter if !self.buffer.is_empty() => {
                let sql = self.buffer.clone();
                self.push_history(&sql);
                Action::ExecuteQuery(sql)
            }
            _ => Action::None,
        }
    }
}
