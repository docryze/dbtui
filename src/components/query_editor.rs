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

/// Right-top panel for SQL editing.
#[derive(Debug, Default)]
pub struct QueryEditor {
    /// Text buffer.
    buffer: String,
    /// Cursor byte offset into `buffer`.
    cursor: usize,
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

        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Query ")
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
                self.insert_char(ch);
                Action::RequestRender
            }
            KeyCode::Backspace => {
                self.backspace();
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
            KeyCode::Enter if !self.buffer.is_empty() => Action::ExecuteQuery(self.buffer.clone()),
            _ => Action::None,
        }
    }
}
