//! Connection list panel — shown when no connection is active.
//!
//! Renders available connections from the config file and returns
//! [`Action::Connect`] on Enter.

use crossterm::event::{KeyCode, KeyEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use crate::components::{AppContext, Component};
use crate::config::ConnectionConfig;
use crate::event::{Action, Event};

/// Left panel showing available connections for selection.
#[derive(Debug)]
pub struct ConnectionList {
    /// Connection configurations from the config file.
    configs: Vec<ConnectionConfig>,
    /// Selection state for the list widget.
    state: ListState,
}

impl ConnectionList {
    /// Create a new connection list from config.
    pub fn new(configs: Vec<ConnectionConfig>) -> Self {
        let mut state = ListState::default();
        if !configs.is_empty() {
            state.select(Some(0));
        }
        Self { configs, state }
    }
}

impl Component for ConnectionList {
    fn render(&self, frame: &mut Frame<'_>, area: Rect, ctx: &AppContext<'_>) {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Connections ")
            .border_style(Style::default().fg(ctx.theme.border_focused));

        if self.configs.is_empty() {
            let hint = format!(
                "\n\n\
                 {welcome:^width$}\n\n\
                 No connections configured yet.\n\
                 Create a connections.toml at:\n\
                 \n  \
                 {path}\n\n\
                 Press [?] for help\n  \
                 ",
                welcome = "Welcome to dbtui",
                path = "~/.config/dbtui/connections.toml",
                width = area.width.saturating_sub(4) as usize
            );
            frame.render_widget(
                Paragraph::new(hint)
                    .style(Style::default().fg(ctx.theme.text_dim))
                    .block(block),
                area,
            );
            return;
        }

        let items: Vec<ListItem<'_>> = self
            .configs
            .iter()
            .map(|c| {
                let name = Span::raw(&c.name);
                let arrow = Span::styled(" → ", Style::default().fg(ctx.theme.text_dim));
                let host = Span::styled(&c.host, Style::default().fg(ctx.theme.text_dim));
                let driver = Span::styled(
                    format!(" [{:?}]", c.driver),
                    Style::default().fg(ctx.theme.text_dim),
                );
                ListItem::new(Line::from(vec![name, arrow, host, driver]))
            })
            .collect();

        let list = List::new(items).block(block).highlight_style(
            Style::default()
                .fg(ctx.theme.highlight)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),
        );

        // ListState is Copy — copy the value for rendering.
        let mut state = self.state;
        frame.render_stateful_widget(list, area, &mut state);
    }

    fn handle_event(&mut self, event: &Event, _ctx: &AppContext<'_>) -> Action {
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
            KeyCode::Enter => {
                if let Some(idx) = self.state.selected()
                    && let Some(cfg) = self.configs.get(idx)
                {
                    return Action::Connect(cfg.clone());
                }
                Action::None
            }
            _ => Action::None,
        }
    }
}
