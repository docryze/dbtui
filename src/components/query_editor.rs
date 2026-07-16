//! Query editor panel — single-line SQL input with syntax highlighting,
//! cursor, history, and autocomplete.
//!
//! Handles character input, backspace, cursor movement, Enter to execute,
//! and Tab/Up/Down for autocomplete when suggestions are visible.

use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListDirection, ListState, Paragraph};

use crate::components::{AppContext, Component, Panel};
use crate::db::SchemaSnapshot;
use crate::event::{Action, Event};
use crate::sql::{TokenKind, autocomplete_keywords, tokenize, word_prefix_at};

/// Prompt prefix shown before the SQL text.
const PROMPT: &str = "> ";

/// Indentation for continuation lines (matches PROMPT width).
const PROMPT_INDENT: &str = "  ";

/// Maximum number of history entries.
const MAX_HISTORY: usize = 50;

/// Minimum prefix length to trigger autocomplete.
const MIN_AUTOCOMPLETE_PREFIX: usize = 1;

/// Maximum number of autocomplete suggestions shown.
const MAX_SUGGESTIONS: usize = 10;

/// Popup width for the autocomplete list.
const AUTOCOMPLETE_WIDTH: u16 = 30;

// ---------------------------------------------------------------------------
// Autocomplete state
// ---------------------------------------------------------------------------

/// Autocomplete suggestion state for the query editor.
#[derive(Debug, Default)]
struct AutocompleteState {
    /// Whether the popup is currently visible.
    visible: bool,
    /// Current suggestion list.
    suggestions: Vec<String>,
    /// Selected index in the suggestion list.
    selected: usize,
    /// The prefix that was used to generate the current suggestions.
    prefix: String,
    /// Byte offset where the prefix starts in the buffer.
    prefix_start: usize,
}

impl AutocompleteState {
    /// Hide the popup and clear suggestions.
    fn hide(&mut self) {
        self.visible = false;
        self.suggestions.clear();
        self.selected = 0;
    }

    /// Get the currently selected suggestion.
    fn current(&self) -> Option<&str> {
        if self.visible && self.selected < self.suggestions.len() {
            Some(&self.suggestions[self.selected])
        } else {
            None
        }
    }

    /// Navigate selection by `delta` (+1 = down, -1 = up).
    fn navigate(&mut self, delta: i32) {
        if !self.visible || self.suggestions.is_empty() {
            return;
        }
        let len = self.suggestions.len() as i32;
        let new = (self.selected as i32 + delta).rem_euclid(len) as usize;
        self.selected = new;
    }
}

// ---------------------------------------------------------------------------
// QueryEditor
// ---------------------------------------------------------------------------

/// Right-top panel for SQL editing with syntax highlighting and autocomplete.
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
    /// Autocomplete popup state.
    autocomplete: AutocompleteState,
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

    /// Which line (0-based) the cursor is on.
    fn cursor_line(&self) -> usize {
        self.buffer[..self.cursor].matches('\n').count()
    }

    /// Compute (row, display_col) for terminal cursor positioning.
    /// The display column accounts for the prompt indentation on every line.
    fn cursor_display_pos(&self) -> (usize, usize) {
        let before = &self.buffer[..self.cursor];
        let row = before.matches('\n').count();
        let col = match before.rfind('\n') {
            Some(i) => before[i + 1..].chars().count(),
            None => before.chars().count(),
        };
        // All lines are indented by PROMPT.len() for visual alignment.
        (row, col.saturating_add(PROMPT.len()))
    }

    /// Move cursor up one line, preserving the column as closely as possible.
    fn cursor_up(&mut self) {
        let before = &self.buffer[..self.cursor];
        let current_line_start = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
        if current_line_start == 0 {
            self.cursor = 0;
            return;
        }
        let prev_end = current_line_start - 1;
        let prev_text = &self.buffer[..prev_end];
        let prev_line_start = prev_text.rfind('\n').map(|i| i + 1).unwrap_or(0);
        let col = self.cursor - current_line_start;
        let prev_line_len = prev_end - prev_line_start;
        self.cursor = prev_line_start + col.min(prev_line_len);
    }

    /// Move cursor down one line, preserving the column as closely as possible.
    fn cursor_down(&mut self) {
        let after = &self.buffer[self.cursor..];
        match after.find('\n') {
            None => {
                self.cursor = self.buffer.len();
            }
            Some(rel_nl) => {
                let current_col = {
                    let before = &self.buffer[..self.cursor];
                    let line_start = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
                    self.cursor - line_start
                };
                let next_line_start = self.cursor + rel_nl + 1;
                let remaining = &self.buffer[next_line_start..];
                let next_line_end = remaining
                    .find('\n')
                    .map(|i| next_line_start + i)
                    .unwrap_or(self.buffer.len());
                let next_line_len = next_line_end - next_line_start;
                self.cursor = next_line_start + current_col.min(next_line_len);
            }
        }
    }

    /// Total number of lines in the buffer (0-based count → at least 1).
    fn line_count(&self) -> usize {
        self.buffer.matches('\n').count() + 1
    }

    /// Replace the buffer contents (e.g. from schema tree table selection).
    pub fn set_text(&mut self, text: String) {
        self.buffer = text;
        self.cursor = self.buffer.len();
        // Reset history navigation when external text is set.
        self.history_index = None;
        self.autocomplete.hide();
    }

    /// Whether the autocomplete popup is currently visible.
    #[must_use]
    pub fn is_autocomplete_visible(&self) -> bool {
        self.autocomplete.visible
    }

    /// Replace the in-memory history with the given list (most recent first).
    pub fn sync_history(&mut self, sql_list: &[String]) {
        self.history = sql_list.to_vec();
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

    /// Update autocomplete suggestions based on the current word at cursor.
    fn update_autocomplete(&mut self, schema: Option<&SchemaSnapshot>, active_db: Option<&str>) {
        let (prefix, prefix_start) = word_prefix_at(&self.buffer, self.cursor);

        if prefix.len() < MIN_AUTOCOMPLETE_PREFIX {
            self.autocomplete.hide();
            return;
        }

        let prefix_upper = prefix.to_ascii_uppercase();
        let prefix_lower = prefix.to_ascii_lowercase();
        let mut suggestions: Vec<String> = Vec::new();

        // SQL keywords and functions.
        for &kw in autocomplete_keywords() {
            if kw.starts_with(prefix_upper.as_str()) && kw != prefix.as_str() {
                suggestions.push(kw.to_string());
            }
        }

        if let Some(snap) = schema {
            // Column names from the current table (detected via FROM clause).
            if let Some(table) = crate::sql::detect_from_table(&self.buffer) {
                if let Some(columns) = snap.active_table_columns.get(&table) {
                    for col in columns {
                        if col.to_ascii_lowercase().starts_with(prefix_lower.as_str())
                            && col.as_str() != prefix
                        {
                            suggestions.push(col.clone());
                        }
                    }
                }
            }

            // Table names: only from the active database (if set), otherwise all.
            if let Some(db) = active_db {
                for (schema_name, tables) in &snap.tree {
                    if schema_name == db {
                        for table in tables {
                            if table.to_ascii_lowercase().starts_with(prefix_lower.as_str())
                                && table.as_str() != prefix
                            {
                                suggestions.push(table.clone());
                            }
                        }
                    }
                }
            } else {
                // No active DB — show all schemas and tables.
                for (schema_name, tables) in &snap.tree {
                    if schema_name.to_ascii_lowercase().starts_with(prefix_lower.as_str())
                        && schema_name.as_str() != prefix
                    {
                        suggestions.push(schema_name.clone());
                    }
                    for table in tables {
                        if table.to_ascii_lowercase().starts_with(prefix_lower.as_str())
                            && table.as_str() != prefix
                        {
                            suggestions.push(table.clone());
                        }
                    }
                }
            }
        }

        // Deduplicate and limit.
        suggestions.sort_unstable();
        suggestions.dedup();
        suggestions.truncate(MAX_SUGGESTIONS);

        if suggestions.is_empty() {
            self.autocomplete.hide();
        } else {
            self.autocomplete.visible = true;
            self.autocomplete.suggestions = suggestions;
            self.autocomplete.selected = 0;
            self.autocomplete.prefix = prefix;
            self.autocomplete.prefix_start = prefix_start;
        }
    }

    /// Accept the currently selected autocomplete suggestion.
    /// Replaces the prefix at cursor with the full suggestion text.
    fn accept_autocomplete(&mut self) {
        if let Some(suggestion) = self.autocomplete.current() {
            let start = self.autocomplete.prefix_start;
            let end = self.cursor;
            // Replace [start, end) with the suggestion.
            self.buffer.replace_range(start..end, suggestion);
            self.cursor = start + suggestion.len();
        }
        self.autocomplete.hide();
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
            autocomplete: AutocompleteState::default(),
        }
    }
}

/// Map a [`TokenKind`] to the corresponding theme color.
fn kind_to_color(kind: TokenKind, theme: &crate::components::Theme) -> Color {
    match kind {
        TokenKind::Keyword => theme.sql_keyword,
        TokenKind::Function => theme.sql_function,
        TokenKind::String => theme.sql_string,
        TokenKind::Number => theme.sql_number,
        TokenKind::Comment => theme.sql_comment,
        TokenKind::Operator => theme.sql_operator,
        TokenKind::Identifier | TokenKind::Punctuation | TokenKind::Whitespace => theme.text,
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

        let prompt_style = if focused {
            Style::default().fg(ctx.theme.highlight)
        } else {
            Style::default().fg(ctx.theme.text_dim)
        };

        // Build multi-line syntax-highlighted lines.
        let lines: Vec<Line<'_>> = self
            .buffer
            .split('\n')
            .enumerate()
            .map(|(i, line_text)| {
                let mut spans: Vec<Span<'_>> = Vec::new();
                if i == 0 {
                    spans.push(Span::styled(PROMPT, prompt_style));
                } else {
                    // Indent to align after prompt.
                    spans.push(Span::raw(PROMPT_INDENT));
                }
                let tokens = tokenize(line_text);
                for tok in &tokens {
                    let color = kind_to_color(tok.kind, ctx.theme);
                    spans.push(Span::styled(tok.text, Style::default().fg(color)));
                }
                Line::from(spans)
            })
            .collect();

        let lines = if lines.is_empty() {
            vec![Line::from(Span::styled(PROMPT, prompt_style))]
        } else {
            lines
        };

        frame.render_widget(Paragraph::new(lines).block(block), area);

        // Position the terminal cursor when focused.
        if focused {
            let (row, col) = self.cursor_display_pos();
            let cursor_x = inner
                .x
                .saturating_add(u16::try_from(col).unwrap_or(u16::MAX));
            let cursor_y = inner
                .y
                .saturating_add(u16::try_from(row).unwrap_or(u16::MAX));
            frame.set_cursor_position((cursor_x, cursor_y));
        }

        // Render autocomplete popup overlay if visible.
        if self.autocomplete.visible && focused {
            render_autocomplete_popup(frame, inner, &self.autocomplete, ctx);
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

        // Handle keys when autocomplete popup is visible.
        if self.autocomplete.visible {
            match key.code {
                KeyCode::Tab | KeyCode::Enter => {
                    self.accept_autocomplete();
                    // After accepting, refresh suggestions for the new context.
                    self.update_autocomplete(ctx.schema, ctx.active_database);
                    return Action::RequestRender;
                }
                KeyCode::Up => {
                    self.autocomplete.navigate(-1);
                    return Action::RequestRender;
                }
                KeyCode::Down => {
                    self.autocomplete.navigate(1);
                    return Action::RequestRender;
                }
                KeyCode::Esc => {
                    self.autocomplete.hide();
                    return Action::RequestRender;
                }
                _ => {} // Fall through to normal handling.
            }
        }

        match key.code {
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Typing resets history navigation and restores saved buffer.
                if self.history_index.is_some() {
                    self.buffer = std::mem::take(&mut self.saved_working);
                    self.cursor = self.buffer.len();
                    self.history_index = None;
                }
                self.insert_char(ch);
                self.update_autocomplete(ctx.schema, ctx.active_database);
                Action::RequestRender
            }
            KeyCode::Backspace => {
                self.backspace();
                self.update_autocomplete(ctx.schema, ctx.active_database);
                Action::RequestRender
            }
            KeyCode::Up => {
                self.autocomplete.hide();
                // On the first line: navigate history backward.
                // Otherwise: move cursor up one line.
                if self.cursor_line() == 0 {
                    self.navigate_history(-1);
                } else {
                    self.cursor_up();
                }
                Action::RequestRender
            }
            KeyCode::Down => {
                self.autocomplete.hide();
                // On the last line: navigate history forward.
                // Otherwise: move cursor down one line.
                if self.cursor_line() + 1 >= self.line_count() {
                    self.navigate_history(1);
                } else {
                    self.cursor_down();
                }
                Action::RequestRender
            }
            KeyCode::Left => {
                self.cursor_left();
                self.autocomplete.hide();
                Action::RequestRender
            }
            KeyCode::Right => {
                self.cursor_right();
                self.autocomplete.hide();
                Action::RequestRender
            }
            KeyCode::Home => {
                // Move to start of current line.
                let before = &self.buffer[..self.cursor];
                let line_start = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
                self.cursor = line_start;
                self.autocomplete.hide();
                Action::RequestRender
            }
            KeyCode::End => {
                // Move to end of current line.
                let after = &self.buffer[self.cursor..];
                let line_end = after.find('\n').map(|i| self.cursor + i).unwrap_or(self.buffer.len());
                self.cursor = line_end;
                self.autocomplete.hide();
                Action::RequestRender
            }
            KeyCode::Tab => {
                // Tab with no popup visible — manually trigger autocomplete.
                self.update_autocomplete(ctx.schema, ctx.active_database);
                Action::RequestRender
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
                // Alt+Enter inserts a newline for multi-line SQL.
                self.insert_char('\n');
                self.update_autocomplete(ctx.schema, ctx.active_database);
                Action::RequestRender
            }
            KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.buffer.clear();
                self.cursor = 0;
                self.autocomplete.hide();
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
                self.update_autocomplete(ctx.schema, ctx.active_database);
                Action::RequestRender
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Delete from cursor to beginning of line.
                self.buffer.drain(..self.cursor);
                self.cursor = 0;
                self.autocomplete.hide();
                Action::RequestRender
            }
            KeyCode::Enter if !self.buffer.is_empty() => {
                let sql = self.buffer.clone();
                self.autocomplete.hide();
                Action::ExecuteQuery(sql)
            }
            _ => Action::None,
        }
    }
}

/// Render the autocomplete popup as an overlay below the editor text line.
fn render_autocomplete_popup(
    frame: &mut Frame<'_>,
    editor_inner: Rect,
    state: &AutocompleteState,
    ctx: &AppContext<'_>,
) {
    if state.suggestions.is_empty() {
        return;
    }

    let count = state.suggestions.len() as u16;
    let height = count.min(MAX_SUGGESTIONS as u16) + 2; // +2 for border
    let width = AUTOCOMPLETE_WIDTH.min(editor_inner.width);

    // Position: below the cursor line, left-aligned with the cursor.
    let popup_x = editor_inner.x;
    let popup_y = editor_inner.y.saturating_add(1); // Below the text line.

    let popup_area = Rect {
        x: popup_x,
        y: popup_y,
        width,
        height,
    };

    // Clear the popup area.
    frame.render_widget(Clear, popup_area);

    // Build the list items with the prefix highlighted.
    let items: Vec<Line<'_>> = state
        .suggestions
        .iter()
        .enumerate()
        .map(|(idx, suggestion)| {
            let is_selected = idx == state.selected;
            let style = if is_selected {
                Style::default().fg(Color::Black).bg(ctx.theme.highlight)
            } else {
                Style::default().fg(ctx.theme.text)
            };
            Line::styled(format!(" {suggestion}"), style)
        })
        .collect();

    let list = List::new(items)
        .direction(ListDirection::TopToBottom)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(ctx.theme.highlight)),
        );

    // Render list with selection state.
    let mut list_state = ListState::default();
    list_state.select(Some(state.selected));
    frame.render_stateful_widget(list, popup_area, &mut list_state);
}
