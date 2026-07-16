//! Status bar component — displays connection status, mode, and key hints
//! (architecture §6: right-bottom panel).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::Paragraph;

use crate::components::{AppContext, Component, Panel};
use crate::event::{Action, Event};

/// Append active database indicator to a label string.
fn db_label(label: &mut String, active_db: Option<&str>) {
    if let Some(db) = active_db {
        use std::fmt::Write;
        let _ = write!(label, " \u{00b7} \u{1F4CB} {db} ");
    }
}

/// Bottom status bar showing current state and key hints.
#[derive(Debug, Default)]
pub struct StatusBar;

impl StatusBar {
    /// Build context-sensitive key hints based on application state.
    fn hints(ctx: &AppContext<'_>) -> &'static str {
        if ctx.is_connecting {
            return "";
        }
        match ctx.connection_name {
            None => " [?] help   [q] quit",
            Some(_) => match ctx.focus {
                Panel::SchemaTree => " [↑↓] navigate   [r] refresh   [?] help   [D] disconnect",
                Panel::QueryEditor => {
                    " [Enter] execute   [↑↓] history   [Tab] focus   [?] help"
                }
                Panel::ResultTable => {
                    " [↑↓] scroll   [Esc] editor   [Tab] focus   [?] help"
                }
            },
        }
    }
}

impl Component for StatusBar {
    fn render(&self, frame: &mut Frame<'_>, area: Rect, ctx: &AppContext<'_>) {
        let mut label: String = match (
            ctx.error_message,
            ctx.is_connecting,
            ctx.is_executing,
            ctx.connection_name,
            ctx.notice,
        ) {
            (Some(err), _, _, _, _) => format!(" \u{2716} ERROR: {err} "),
            (None, true, _, _, _) => " \u{25D4} connecting... ".into(),
            (None, false, true, Some(name), _) => {
                format!(" \u{25C9} {name}  \u{00b7} executing... ")
            }
            (None, false, true, None, _) => " \u{25D4} executing... ".into(),
            (None, false, _, Some(name), Some(notice)) => {
                format!(" \u{25C9} {name}  \u{00b7} {notice} ")
            }
            (None, false, _, Some(name), None) => {
                format!(" \u{25C9} {name}  \u{00b7} connected ")
            }
            (None, false, _, None, _) => " \u{25CB} ready ".into(),
        };
        // Append active database indicator (only when connected).
        if ctx.connection_name.is_some() {
            db_label(&mut label, ctx.active_database);
        }

        let hints = Self::hints(ctx);
        let label_len = u16::try_from(label.len()).unwrap_or(u16::MAX);
        let width: usize = area.width.saturating_sub(label_len).into();
        let full = format!("{label}{hints:>width$}");

        let color = if ctx.error_message.is_some() {
            ctx.theme.status_error
        } else if ctx.is_connecting {
            ctx.theme.warning
        } else if ctx.is_executing {
            ctx.theme.highlight
        } else if ctx.notice.is_some() {
            ctx.theme.success
        } else {
            ctx.theme.status_ready
        };

        let style = Style::default().fg(color);
        frame.render_widget(Paragraph::new(full).style(style), area);
    }

    fn handle_event(&mut self, _event: &Event, _ctx: &AppContext<'_>) -> Action {
        Action::None
    }
}
