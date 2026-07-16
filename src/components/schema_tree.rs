//! Schema tree panel — database/table navigation with expand/collapse.
//!
//! Shows a two-level tree: schemas (expandable) → tables. Selecting a
//! table and pressing Enter fills the query editor with a `SELECT`
//! skeleton.

use std::collections::HashSet;

use crossterm::event::{KeyCode, KeyEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use crate::components::{AppContext, Component, Panel};
use crate::db::SchemaSnapshot;
use crate::event::{Action, Event};

/// A single visible entry in the flattened tree.
#[derive(Debug, Clone)]
enum TreeEntry {
    /// A schema node (expandable).
    Schema { name: String, expanded: bool },
    /// A table node (leaf).
    Table { schema: String, name: String },
}

/// Left panel showing database schema as a navigable tree.
#[derive(Debug, Default)]
pub struct SchemaTree {
    /// Raw tree data: `(schema_name, table_names)`.
    data: Vec<(String, Vec<String>)>,
    /// Expanded schema names.
    expanded: HashSet<String>,
    /// List selection state.
    state: ListState,
}

impl SchemaTree {
    /// Replace tree data from a loaded [`SchemaSnapshot`].
    pub fn set_data(&mut self, snapshot: &SchemaSnapshot) {
        self.data.clone_from(&snapshot.tree);
        self.expanded.clear();
        // Auto-expand the first database for convenience.
        if let Some((first_schema, _)) = self.data.first() {
            self.expanded.insert(first_schema.clone());
        }
        self.state
            .select(if self.data.is_empty() { None } else { Some(0) });
    }

    /// Build the flat list of visible entries.
    fn build_entries(&self) -> Vec<TreeEntry> {
        let mut entries = Vec::new();
        for (schema, tables) in &self.data {
            let is_expanded = self.expanded.contains(schema);
            entries.push(TreeEntry::Schema {
                name: schema.clone(),
                expanded: is_expanded,
            });
            if is_expanded {
                for table in tables {
                    entries.push(TreeEntry::Table {
                        schema: schema.clone(),
                        name: table.clone(),
                    });
                }
            }
        }
        entries
    }
}

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

        if self.data.is_empty() {
            let text = if ctx.connection_name.is_some() {
                " Loading..."
            } else {
                " No connection"
            };
            frame.render_widget(
                Paragraph::new(text)
                    .style(Style::default().fg(ctx.theme.text_dim))
                    .block(block),
                area,
            );
            return;
        }

        let entries = self.build_entries();
        let items: Vec<ListItem<'_>> = entries
            .iter()
            .map(|e| match e {
                TreeEntry::Schema { name, expanded } => {
                    let icon = if *expanded { "\u{25BE}" } else { "\u{25B8}" };
                    ListItem::new(format!(" {icon} {name}"))
                        .style(Style::default().add_modifier(Modifier::BOLD))
                }
                TreeEntry::Table { name, .. } => ListItem::new(format!("   {name}")),
            })
            .collect();

        let list = List::new(items).block(block).highlight_style(
            Style::default()
                .fg(ctx.theme.highlight)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),
        );

        let mut state = self.state;
        frame.render_stateful_widget(list, area, &mut state);
    }

    fn handle_event(&mut self, event: &Event, ctx: &AppContext<'_>) -> Action {
        if ctx.focus != Panel::SchemaTree {
            return Action::None;
        }

        let Event::Key(key) = event else {
            return Action::None;
        };
        if key.kind != KeyEventKind::Press {
            return Action::None;
        }

        let entries = self.build_entries();

        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.state.select_previous();
                Action::RequestRender
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.state.select_next();
                Action::RequestRender
            }
            KeyCode::Right => {
                // Expand the selected schema.
                if let Some(idx) = self.state.selected()
                    && let Some(TreeEntry::Schema {
                        name,
                        expanded: false,
                    }) = entries.get(idx)
                {
                    self.expanded.insert(name.clone());
                    return Action::RequestRender;
                }
                Action::None
            }
            KeyCode::Left => {
                // Collapse the selected schema.
                if let Some(idx) = self.state.selected()
                    && let Some(TreeEntry::Schema {
                        name,
                        expanded: true,
                    }) = entries.get(idx)
                {
                    self.expanded.remove(name);
                    return Action::RequestRender;
                }
                Action::None
            }
            KeyCode::Enter => {
                if let Some(idx) = self.state.selected() {
                    match entries.get(idx) {
                        Some(TreeEntry::Schema { name, expanded }) => {
                            if *expanded {
                                self.expanded.remove(name);
                            } else {
                                self.expanded.insert(name.clone());
                            }
                            Action::SelectDatabase(name.clone())
                        }
                        Some(TreeEntry::Table { schema, name }) => Action::FillQuery(format!(
                            "SELECT * FROM `{schema}`.`{name}` LIMIT 100"
                        )),
                        None => Action::None,
                    }
                } else {
                    Action::None
                }
            }
            _ => Action::None,
        }
    }
}
