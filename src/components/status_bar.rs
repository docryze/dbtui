//! Status bar component — displays connection status, mode, and key hints
//! (architecture §6: right-bottom panel).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::Paragraph;

use crate::components::{AppContext, Component};
use crate::event::{Action, Event};

/// Bottom status bar showing current state and key hints.
#[derive(Debug, Default)]
pub struct StatusBar;

impl Component for StatusBar {
    fn render(&self, frame: &mut Frame<'_>, area: Rect, ctx: &AppContext<'_>) {
        let label: String = match (
            ctx.error_message,
            ctx.is_connecting,
            ctx.connection_name,
            ctx.notice,
        ) {
            (Some(err), _, _, _) => format!(" dbtui \u{00b7} ERROR: {err} "),
            (None, true, _, _) => " dbtui \u{00b7} connecting... ".into(),
            (None, false, Some(name), Some(notice)) => {
                format!(" dbtui \u{00b7} {name} \u{00b7} {notice} ")
            }
            (None, false, Some(name), None) => {
                format!(" dbtui \u{00b7} {name} \u{00b7} connected ")
            }
            (None, false, None, _) => " dbtui \u{00b7} ready \u{00b7} q:quit ".into(),
        };

        let color = if ctx.error_message.is_some() {
            ctx.theme.status_error
        } else {
            ctx.theme.status_ready
        };

        let style = Style::default().fg(color);
        frame.render_widget(Paragraph::new(label).style(style), area);
    }

    fn handle_event(&mut self, _event: &Event, _ctx: &AppContext<'_>) -> Action {
        Action::None
    }
}
