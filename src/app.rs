//! Application state machine: event dispatch, action execution, and render
//! coordination (architecture §3.3, §4.3).

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use tokio::sync::mpsc;

use crate::components::{
    AppContext, AppMode, Component, Components, ConnectionList, Panel, PopupKind, Theme,
};
use crate::config::ConnectionConfig;
use crate::db::{ConnectionHandle, QueryMeta, SchemaSnapshot, mysql::MySqlBackend};
use crate::error::Error;
use crate::event::{Action, DbMessage, Event, QueryId};
use crate::tui::Tui;

/// Target frame rate for rendering (frames per second).
const FPS: u64 = 60;

/// The application state.
pub struct App {
    /// Current mode.
    mode: AppMode,
    /// Whether the app should exit.
    should_quit: bool,
    /// Currently focused panel.
    focus: Panel,
    /// Visual theme.
    theme: Theme,
    /// All component instances.
    components: Components,
    /// Connection list (shown when not connected).
    connection_list: ConnectionList,
    /// Dirty flag — when `true`, the next tick redraws.
    dirty: bool,
    /// Sender for DB messages (cloned per DB task).
    db_tx: mpsc::Sender<DbMessage>,
    /// Active connection (None = not connected).
    connection: Option<ConnectionHandle>,
    /// Last error message for status bar display.
    last_error: Option<String>,
    /// Pending query ID (for stale message filtering).
    pending_query: Option<QueryId>,
    /// Transient status notice (e.g. "42 rows in 0.3s").
    notice: Option<String>,
    /// Handle to the current query task (for cancellation).
    query_handle: Option<tokio::task::JoinHandle<()>>,
    /// When the last error was set (for auto-fade).
    error_at: Option<Instant>,
    /// Error message for popup overlay (for query errors).
    error_popup: Option<String>,
}

impl App {
    /// Create a new `App` with the given connection configurations.
    pub fn new(db_tx: mpsc::Sender<DbMessage>, configs: Vec<ConnectionConfig>) -> Self {
        Self {
            mode: AppMode::Normal,
            should_quit: false,
            focus: Panel::SchemaTree,
            theme: Theme::default(),
            components: Components::default(),
            connection_list: ConnectionList::new(configs),
            dirty: true,
            db_tx,
            connection: None,
            last_error: None,
            pending_query: None,
            notice: None,
            query_handle: None,
            error_at: None,
            error_popup: None,
        }
    }

    /// Set an error message and record the timestamp for auto-fade.
    fn set_error(&mut self, msg: String) {
        self.last_error = Some(msg);
        self.error_at = Some(Instant::now());
    }

    /// Called on each draw interval — auto-fade errors after 5 seconds.
    fn tick(&mut self) {
        if self.error_at.is_some_and(|at| at.elapsed().as_secs() >= 5) {
            self.last_error = None;
            self.error_at = None;
            self.dirty = true;
        }
    }

    /// Route an event to the appropriate component (after global key handling)
    /// and return the resulting [`Action`].
    fn dispatch_event(&mut self, event: &Event) -> Action {
        if let Event::Key(key) = event {
            if key.kind != KeyEventKind::Press {
                return Action::None;
            }

            // Popup mode — only Esc/q close the popup.
            if matches!(self.mode, AppMode::Popup(_)) {
                return match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => Action::ClosePopup,
                    _ => Action::None,
                };
            }

            // 'r' refreshes the schema tree (only when SchemaTree is focused and connected).
            if key.code == KeyCode::Char('r')
                && !key.modifiers.contains(KeyModifiers::CONTROL)
                && self.focus == Panel::SchemaTree
                && let Some(ref conn) = self.connection
            {
                return Action::LoadSchema(conn.id);
            }

            match key.code {
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    // Cancel query if one is running, otherwise quit.
                    if let Some(qid) = self.pending_query {
                        return Action::CancelQuery(qid);
                    }
                    return Action::Quit;
                }
                KeyCode::Char('q') => {
                    // Don't quit when typing in the query editor.
                    if !(self.connection.is_some() && self.focus == Panel::QueryEditor) {
                        return Action::Quit;
                    }
                }
                KeyCode::Char('?') => {
                    return Action::OpenPopup(PopupKind::Help);
                }
                KeyCode::Char('D') => {
                    if let Some(ref conn) = self.connection {
                        return Action::Disconnect(conn.id);
                    }
                }
                KeyCode::Tab => {
                    if self.connection.is_some() {
                        return Action::Focus(self.focus.next());
                    }
                    return Action::None;
                }
                KeyCode::BackTab => {
                    if self.connection.is_some() {
                        return Action::Focus(self.focus.prev());
                    }
                    return Action::None;
                }
                _ => {}
            }
        }

        // Block input during connection attempt.
        if self.mode == AppMode::Connecting {
            return Action::None;
        }

        let ctx = AppContext {
            focus: self.focus,
            mode: self.mode,
            theme: &self.theme,
            connection_name: self.connection.as_ref().map(|c| c.name.as_str()),
            is_connecting: self.mode == AppMode::Connecting,
            is_executing: self.pending_query.is_some(),
            error_message: self.last_error.as_deref(),
            notice: self.notice.as_deref(),
        };

        if self.connection.is_some() {
            match self.focus {
                Panel::SchemaTree => self.components.schema_tree.handle_event(event, &ctx),
                Panel::QueryEditor => self.components.query_editor.handle_event(event, &ctx),
                Panel::ResultTable => self.components.result_table.handle_event(event, &ctx),
            }
        } else {
            self.connection_list.handle_event(event, &ctx)
        }
    }

    /// Execute an action's side effects.
    #[expect(
        clippy::too_many_lines,
        reason = "handles all Action variants centrally"
    )]
    fn apply_action(&mut self, action: &Action) {
        match action {
            Action::Quit => self.should_quit = true,
            Action::Focus(panel) => self.focus = *panel,
            Action::OpenPopup(kind) => self.mode = AppMode::Popup(*kind),
            Action::ClosePopup => {
                self.mode = AppMode::Normal;
                self.error_popup = None;
            }
            Action::Connect(cfg) => {
                self.mode = AppMode::Connecting;
                self.last_error = None;
                let tx = self.db_tx.clone();
                let cfg = cfg.clone();
                tokio::spawn(async move {
                    let id = cfg.id;
                    let name = cfg.name.clone();
                    match MySqlBackend::connect(&cfg).await {
                        Ok(backend) => match backend.ping().await {
                            Ok(()) => {
                                let handle = ConnectionHandle {
                                    id,
                                    name,
                                    backend,
                                    schema_snapshot: None,
                                };
                                let _ = tx.send(DbMessage::Connected(Ok(handle))).await;
                            }
                            Err(e) => {
                                let _ = tx.send(DbMessage::Connected(Err(e))).await;
                            }
                        },
                        Err(e) => {
                            let _ = tx.send(DbMessage::Connected(Err(e))).await;
                        }
                    }
                });
            }
            Action::ExecuteQuery(sql) => {
                if let Some(ref conn) = self.connection {
                    self.notice = None;
                    self.last_error = None;
                    tracing::info!("executing: {sql}");
                    if is_query_sql(sql) {
                        let query_id = QueryId::new();
                        self.pending_query = Some(query_id);
                        self.components.result_table.clear();
                        self.focus = Panel::ResultTable;
                        let backend = std::sync::Arc::clone(&conn.backend);
                        let tx = self.db_tx.clone();
                        let sql_owned = sql.clone();
                        let handle = tokio::spawn(async move {
                            let _ = backend.query_stream(&sql_owned, query_id, tx).await;
                        });
                        self.query_handle = Some(handle);
                    } else {
                        let backend = std::sync::Arc::clone(&conn.backend);
                        let tx = self.db_tx.clone();
                        let query_id = QueryId::new();
                        self.pending_query = Some(query_id);
                        let sql_owned = sql.clone();
                        let handle = tokio::spawn(async move {
                            match backend.execute(&sql_owned).await {
                                Ok(result) => {
                                    let _ = tx
                                        .send(DbMessage::QueryComplete(
                                            query_id,
                                            Ok(QueryMeta {
                                                affected_rows: Some(result.rows_affected),
                                                rows_returned: 0,
                                                elapsed: Duration::ZERO,
                                                truncated: false,
                                            }),
                                        ))
                                        .await;
                                }
                                Err(e) => {
                                    let _ =
                                        tx.send(DbMessage::QueryComplete(query_id, Err(e))).await;
                                }
                            }
                        });
                        self.query_handle = Some(handle);
                    }
                }
            }
            Action::FillQuery(sql) => {
                self.components.query_editor.set_text(sql.clone());
                self.focus = Panel::QueryEditor;
            }
            Action::LoadSchema(conn_id) => {
                let conn_id = *conn_id;
                if let Some(ref conn) = self.connection {
                    let backend = std::sync::Arc::clone(&conn.backend);
                    let tx = self.db_tx.clone();
                    tokio::spawn(async move {
                        match backend.list_schemas().await {
                            Ok(schemas) => {
                                let mut tree = Vec::new();
                                for s in &schemas {
                                    let tables: Vec<String> = backend
                                        .list_tables(&s.name)
                                        .await
                                        .unwrap_or_default()
                                        .into_iter()
                                        .map(|t| t.name)
                                        .collect();
                                    tree.push((s.name.clone(), tables));
                                }
                                let _ = tx
                                    .send(DbMessage::SchemaLoaded(
                                        conn_id,
                                        Ok(SchemaSnapshot { tree }),
                                    ))
                                    .await;
                            }
                            Err(e) => {
                                let _ = tx.send(DbMessage::SchemaLoaded(conn_id, Err(e))).await;
                            }
                        }
                    });
                }
            }
            Action::CancelQuery(_) => {
                if let Some(handle) = self.query_handle.take() {
                    handle.abort();
                }
                self.pending_query = None;
                self.notice = Some("Query cancelled".into());
                tracing::info!("query cancelled");
            }
            Action::Disconnect(_) => {
                // Cancel any running query.
                if let Some(handle) = self.query_handle.take() {
                    handle.abort();
                }
                self.connection = None;
                self.pending_query = None;
                self.query_handle = None;
                self.components.schema_tree = crate::components::SchemaTree::default();
                self.components.result_table = crate::components::ResultTable::default();
                self.components.query_editor = crate::components::QueryEditor::default();
                self.focus = Panel::SchemaTree;
                self.notice = Some("Disconnected".into());
                self.last_error = None;
                tracing::info!("disconnected");
            }
            // No-op variants — implemented in later milestones.
            Action::None | Action::RequestRender | Action::SwitchTab(_) => {
            }
        }
        self.dirty = true;
    }

    /// Handle a message from a DB task.
    fn handle_db_message(&mut self, msg: DbMessage) {
        match msg {
            DbMessage::Connected(result) => {
                self.mode = AppMode::Normal;
                match result {
                    Ok(handle) => {
                        let conn_id = handle.id;
                        self.connection = Some(handle);
                        self.focus = Panel::QueryEditor;
                        // Auto-load schema tree on connect.
                        self.apply_action(&Action::LoadSchema(conn_id));
                    }
                    Err(e) => {
                        self.set_error(e.to_string());
                    }
                }
            }
            DbMessage::QueryPage(query_id, result) => {
                if self.pending_query == Some(query_id) {
                    match result {
                        Ok(page) => {
                            if let Some(cols) = page.columns {
                                self.components.result_table.set_columns(cols);
                            }
                            self.components.result_table.append_rows(page.rows);
                        }
                        Err(e) => {
                            self.set_error(e.to_string());
                        }
                    }
                }
            }
            DbMessage::QueryComplete(query_id, result) if self.pending_query == Some(query_id) => {
                match result {
                    Ok(meta) => {
                        self.notice = Some(format_query_notice(&meta));
                        self.components.result_table.set_complete(&meta);
                    }
                    Err(e) => {
                        self.set_error(e.to_string());
                        self.error_popup = Some(e.to_string());
                        self.mode = AppMode::Popup(PopupKind::Error);
                    }
                }
                self.pending_query = None;
            }
            DbMessage::SchemaLoaded(_, result) => match result {
                Ok(snapshot) => {
                    if let Some(ref mut conn) = self.connection {
                        conn.schema_snapshot = Some(snapshot.clone());
                    }
                    self.components.schema_tree.set_data(&snapshot);
                }
                Err(e) => {
                    self.set_error(e.to_string());
                }
            },
            _ => {}
        }
        self.dirty = true;
    }

    /// Render the full UI (read-only — called from `terminal.draw`).
    fn render(&self, frame: &mut Frame<'_>) {
        let area = frame.area();
        let (left, right_top, right_bottom) = layout(area);

        let ctx = AppContext {
            focus: self.focus,
            mode: self.mode,
            theme: &self.theme,
            connection_name: self.connection.as_ref().map(|c| c.name.as_str()),
            is_connecting: self.mode == AppMode::Connecting,
            is_executing: self.pending_query.is_some(),
            error_message: self.last_error.as_deref(),
            notice: self.notice.as_deref(),
        };

        if self.connection.is_some() {
            self.components.schema_tree.render(frame, left, &ctx);

            if self.focus == Panel::ResultTable && self.components.result_table.has_data() {
                self.components.result_table.render(frame, right_top, &ctx);
            } else {
                self.components.query_editor.render(frame, right_top, &ctx);
            }
        } else {
            self.connection_list.render(frame, left, &ctx);
            self.components.query_editor.render(frame, right_top, &ctx);
        }

        self.components.status_bar.render(frame, right_bottom, &ctx);

        // Draw popup overlays.
        if self.mode == AppMode::Popup(PopupKind::Help) {
            render_help_popup(frame, area);
        } else if let Some(ref err) = self.error_popup {
            render_error_popup(frame, area, err);
        }
    }
}

/// Render a centered error popup overlay with the given message.
fn render_error_popup(frame: &mut Frame<'_>, area: Rect, message: &str) {
    let width = 60.min(area.width).max(30);
    let height = 8.min(area.height);
    let popup = Rect {
        x: area.width.saturating_sub(width) / 2,
        y: area.height.saturating_sub(height) / 2,
        width,
        height,
    };

    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Error ")
        .border_style(Style::default().fg(ratatui::style::Color::Red));
    frame.render_widget(
        Paragraph::new(message)
            .block(block)
            .wrap(Wrap { trim: false }),
        popup,
    );
}

/// Render the help popup overlay showing keybindings.
fn render_help_popup(frame: &mut Frame<'_>, area: Rect) {
    let popup = Rect {
        x: area.width.saturating_sub(50) / 2,
        y: area.height.saturating_sub(20) / 2,
        width: 50.min(area.width),
        height: 20.min(area.height),
    };

    let keybindings = [
        ("Tab / Shift+Tab", "Cycle panel focus"),
        ("↑/↓/←/→", "Navigate / Move cursor"),
        ("Enter", "Execute query / Connect / Toggle tree"),
        ("Esc", "Return to editor from results"),
        ("r", "Refresh schema tree"),
        ("g / G", "Go to top / bottom in results"),
        ("PgUp / PgDn", "Scroll page in results"),
        ("Ctrl+U / Ctrl+D", "Scroll half page up / down"),
        ("Ctrl+L", "Clear query editor"),
        ("?", "Toggle this help"),
        ("D", "Disconnect"),
        ("Ctrl+C", "Cancel query / Quit"),
        ("q", "Quit (except in editor)"),
    ];

    let text: String = {
        use std::fmt::Write;
        keybindings
            .iter()
            .fold(String::new(), |mut acc, (key, action)| {
                let _ = writeln!(acc, "  {key:<20} {action}");
                acc
            })
    };

    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Help ")
        .border_style(Style::default().fg(ratatui::style::Color::Cyan));
    frame.render_widget(
        Paragraph::new(text).block(block).wrap(Wrap { trim: false }),
        popup,
    );
}

/// Check if SQL is a query (SELECT/SHOW/EXPLAIN/WITH) vs. execute.
fn is_query_sql(sql: &str) -> bool {
    let upper = sql.trim_start().to_ascii_uppercase();
    upper.starts_with("SELECT")
        || upper.starts_with("SHOW")
        || upper.starts_with("EXPLAIN")
        || upper.starts_with("WITH")
}

/// Format a query completion notice for the status bar.
fn format_query_notice(meta: &QueryMeta) -> String {
    meta.affected_rows.map_or_else(
        || {
            let trunc = if meta.truncated { " (truncated)" } else { "" };
            format!(
                "{} rows in {:.2}s{trunc}",
                meta.rows_returned,
                meta.elapsed.as_secs_f64()
            )
        },
        |affected| format!("{affected} rows affected"),
    )
}

/// Compute the three-panel layout: left (schema), right-top (editor),
/// right-bottom (status bar).
fn layout(area: Rect) -> (Rect, Rect, Rect) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(area);

    let left = columns.first().copied().unwrap_or_default();
    let right_col = columns.get(1).copied().unwrap_or_default();

    let right_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(3)])
        .split(right_col);

    let right_top = right_rows.first().copied().unwrap_or_default();
    let right_bottom = right_rows.get(1).copied().unwrap_or_default();

    (left, right_top, right_bottom)
}

/// Main event loop (architecture §4.3).
///
/// `event_rx` and `db_rx` are owned by the caller (not by [`App`]) to
/// avoid borrow conflicts between the `select!` futures and the branch
/// bodies.
///
/// # Errors
/// Returns [`Error::Io`] if a terminal draw or resize operation fails.
pub async fn run(
    app: &mut App,
    terminal: &mut Tui,
    event_rx: &mut mpsc::Receiver<Event>,
    db_rx: &mut mpsc::Receiver<DbMessage>,
) -> Result<(), Error> {
    let mut draw_interval = tokio::time::interval(Duration::from_millis(1000 / FPS));
    draw_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        if app.should_quit {
            break;
        }

        tokio::select! {
            biased;

            Some(ev) = event_rx.recv() => {
                if let Event::Resize(w, h) = &ev {
                    let _ = terminal.resize(Rect::new(0, 0, *w, *h));
                } else {
                    let action = app.dispatch_event(&ev);
                    app.apply_action(&action);
                }
                app.dirty = true;
            }

            Some(msg) = db_rx.recv() => {
                app.handle_db_message(msg);
            }

            _ = draw_interval.tick() => {
                app.tick();
                if app.dirty {
                    terminal.draw(|f| app.render(f))?;
                    app.dirty = false;
                }
            }
        }
    }

    Ok(())
}
