//! Result table panel — displays query results with scrolling.
//!
//! Shows column headers and rows from a [`ResultSet`], with adaptive
//! column widths and `TableState`-based scrolling.

use crossterm::event::{KeyCode, KeyEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, Cell, Row, Table, TableState};

use crate::components::{AppContext, Component, Panel};
use crate::db::{CellValue, ColumnMeta, QueryMeta};
use crate::event::{Action, Event};

/// Maximum column width in characters.
const MAX_COL_WIDTH: usize = 50;
/// Number of rows to sample for column width calculation.
const SAMPLE_ROWS: usize = 100;

/// Right-top panel showing query results.
#[derive(Debug, Default)]
pub struct ResultTable {
    /// Column definitions.
    columns: Vec<ColumnMeta>,
    /// Accumulated rows.
    rows: Vec<Vec<CellValue>>,
    /// Scroll state.
    state: TableState,
    /// Whether the query has completed.
    complete: bool,
    /// Whether results were truncated.
    truncated: bool,
    /// Total rows returned.
    rows_returned: u64,
    /// Rows affected (for INSERT/UPDATE).
    affected_rows: Option<u64>,
}

impl ResultTable {
    /// Set column definitions (called on first `QueryPage`).
    pub fn set_columns(&mut self, columns: Vec<ColumnMeta>) {
        self.columns = columns;
        self.rows.clear();
        self.complete = false;
        self.truncated = false;
        self.rows_returned = 0;
        self.affected_rows = None;
        self.state.select(Some(0));
    }

    /// Append rows from a `QueryPage`.
    pub fn append_rows(&mut self, rows: Vec<Vec<CellValue>>) {
        self.rows_returned = self.rows_returned.saturating_add(rows.len() as u64);
        self.rows.extend(rows);
    }

    /// Mark the query as complete with metadata.
    pub fn set_complete(&mut self, meta: &QueryMeta) {
        self.complete = true;
        self.truncated = meta.truncated;
        self.rows_returned = meta.rows_returned;
        self.affected_rows = meta.affected_rows;
    }

    /// Clear all data.
    pub fn clear(&mut self) {
        self.columns.clear();
        self.rows.clear();
        self.complete = false;
        self.truncated = false;
        self.rows_returned = 0;
        self.affected_rows = None;
        self.state.select(Some(0));
    }

    /// Whether the table has any data to show.
    pub fn has_data(&self) -> bool {
        !self.columns.is_empty()
    }

    /// Compute adaptive column widths based on header + sampled data.
    fn calculate_widths(&self) -> Vec<Constraint> {
        if self.columns.is_empty() {
            return Vec::new();
        }

        let mut widths: Vec<usize> = self
            .columns
            .iter()
            .map(|c| c.name.chars().count())
            .collect();

        for row in self.rows.iter().take(SAMPLE_ROWS) {
            for (i, cell) in row.iter().enumerate() {
                if let Some(width) = widths.get_mut(i) {
                    let len = cell_display_len(cell);
                    if len > *width {
                        *width = len;
                    }
                }
            }
        }

        widths
            .iter()
            .map(|&w| {
                let capped = w.min(MAX_COL_WIDTH);
                Constraint::Min(u16::try_from(capped).unwrap_or(u16::MAX))
            })
            .collect()
    }
}

/// Display length of a cell value.
fn cell_display_len(cell: &CellValue) -> usize {
    match cell {
        CellValue::Null => 4, // "NULL"
        CellValue::Text(s) => s.chars().count(),
        CellValue::BytesHex(s) => s.len(),
    }
}

impl Component for ResultTable {
    fn render(&self, frame: &mut Frame<'_>, area: Rect, ctx: &AppContext<'_>) {
        let focused = ctx.focus == Panel::ResultTable;
        let border_color = if focused {
            ctx.theme.border_focused
        } else {
            ctx.theme.border_normal
        };

        let title: String = if self.complete {
            if self.truncated {
                format!(" Results ({} rows, truncated) ", self.rows_returned)
            } else if self.affected_rows.is_some() {
                format!(" Results ({} affected) ", self.affected_rows.unwrap_or(0))
            } else {
                format!(" Results ({} rows) ", self.rows_returned)
            }
        } else if self.has_data() {
            " Results (querying...) ".into()
        } else {
            " Results ".into()
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(Style::default().fg(border_color));

        if !self.has_data() {
            frame.render_widget(
                ratatui::widgets::Paragraph::new(" No results").block(block),
                area,
            );
            return;
        }

        // Header row.
        let header_cells: Vec<Cell<'_>> = self
            .columns
            .iter()
            .map(|c| {
                Cell::from(c.name.as_str()).style(Style::default().add_modifier(Modifier::BOLD))
            })
            .collect();
        let header = Row::new(header_cells).height(1);

        // Data rows.
        let data_rows: Vec<Row<'_>> = self
            .rows
            .iter()
            .map(|row| {
                let cells: Vec<Cell<'_>> = row
                    .iter()
                    .map(|cell| match cell {
                        CellValue::Null => {
                            Cell::from("NULL").style(Style::default().fg(ctx.theme.text))
                        }
                        CellValue::Text(s) | CellValue::BytesHex(s) => Cell::from(s.as_str()),
                    })
                    .collect();
                Row::new(cells)
            })
            .collect();

        let widths = self.calculate_widths();
        let table = Table::new(data_rows, widths)
            .header(header)
            .block(block)
            .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));

        let mut state = self.state;
        frame.render_stateful_widget(table, area, &mut state);
    }

    fn handle_event(&mut self, event: &Event, ctx: &AppContext<'_>) -> Action {
        if ctx.focus != Panel::ResultTable {
            return Action::None;
        }

        let Event::Key(key) = event else {
            return Action::None;
        };
        if key.kind != KeyEventKind::Press {
            return Action::None;
        }

        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.state.select_previous();
                Action::RequestRender
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.state.select_next();
                Action::RequestRender
            }
            KeyCode::Esc => Action::Focus(Panel::QueryEditor),
            _ => Action::None,
        }
    }
}
