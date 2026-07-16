//! Schema tree panel — database/table navigation (architecture §6: left panel).
//!
//! Full tree navigation arrives in M5; this is a placeholder for M1 that
//! renders a bordered panel with a connection-status hint.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::components::{AppContext, Component, Panel};
use crate::event::{Action, Event};

/// Left panel showing database schema as a tree.
#[derive(Debug, Default)]
pub struct SchemaTree;

impl Component for SchemaTree {
    fn render(&self, frame: &mut Frame<'_>, area: Rect, ctx: &AppContext<'_>) {
        let focused = ctx.focus == Panel::SchemaTree;
        let border_color = if focused {
            ctx.theme.border_focused
        } else {
            ctx.theme.border_normal
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Schema ")
            .border_style(Style::default().fg(border_color));

        let text = if ctx.connection_name.is_some() {
            " Connected \u{2014} loading..."
        } else {
            " No connection"
        };

        frame.render_widget(Paragraph::new(text).block(block), area);
    }

    fn handle_event(&mut self, _event: &Event, _ctx: &AppContext<'_>) -> Action {
        Action::None
    }
}
