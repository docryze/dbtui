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
use crate::history::QueryHistory;
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
    /// Active/default database for unqualified table references.
    active_database: Option<String>,
    /// Persistent query history (loaded from file).
    history: QueryHistory,
    /// SQL text of the last submitted query (for saving on success).
    last_executed_sql: Option<String>,
    /// Selected index in the history popup.
    history_selected: usize,
    /// Search filter text for the history popup.
    history_search: String,
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
            active_database: None,
            history: QueryHistory::new(),
            last_executed_sql: None,
            history_selected: 0,
            history_search: String::new(),
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

            // Popup mode — Esc/q close; History popup allows navigation + search.
            if matches!(self.mode, AppMode::Popup(PopupKind::History)) {
                return match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => Action::ClosePopup,
                    KeyCode::Up => {
                        if self.history_selected > 0 {
                            self.history_selected -= 1;
                        }
                        Action::RequestRender
                    }
                    KeyCode::Down => {
                        let filtered = self.history.search(&self.history_search);
                        let max = filtered.len().saturating_sub(1);
                        if self.history_selected < max {
                            self.history_selected += 1;
                        }
                        Action::RequestRender
                    }
                    KeyCode::Enter => {
                        let filtered = self.history.search(&self.history_search);
                        if let Some(entry) = filtered.get(self.history_selected) {
                            let sql = entry.sql.clone();
                            self.mode = AppMode::Normal;
                            self.history_search.clear();
                            Action::FillQuery(sql)
                        } else {
                            Action::ClosePopup
                        }
                    }
                    KeyCode::Char('g') if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                        self.history_selected = 0;
                        Action::RequestRender
                    }
                    KeyCode::Char('G') => {
                        let filtered = self.history.search(&self.history_search);
                        self.history_selected = filtered.len().saturating_sub(1);
                        Action::RequestRender
                    }
                    KeyCode::Backspace => {
                        self.history_search.pop();
                        self.history_selected = 0;
                        Action::RequestRender
                    }
                    KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.history_search.push(ch);
                        self.history_selected = 0;
                        Action::RequestRender
                    }
                    _ => Action::None,
                };
            }

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
                KeyCode::Char('H') => {
                    if self.connection.is_some() {
                        self.history_selected = 0;
                        return Action::OpenPopup(PopupKind::History);
                    }
                }
                KeyCode::Char('D') => {
                    if let Some(ref conn) = self.connection {
                        return Action::Disconnect(conn.id);
                    }
                }
                KeyCode::Tab => {
                    // If the editor has an autocomplete popup open, let Tab
                    // fall through to the editor to accept the suggestion.
                    if self.focus == Panel::QueryEditor
                        && self.components.query_editor.is_autocomplete_visible()
                    {
                        // Fall through to component dispatch below.
                    } else if self.connection.is_some() {
                        return Action::Focus(self.focus.next());
                    } else {
                        return Action::None;
                    }
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
            active_database: self.active_database.as_deref(),
            schema: self
                .connection
                .as_ref()
                .and_then(|c| c.schema_snapshot.as_ref()),
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
                self.history_search.clear();
                self.history_selected = 0;
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
                                    config: cfg,
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
                    // Cancel any running query first.
                    if let Some(handle) = self.query_handle.take() {
                        handle.abort();
                    }
                    self.notice = None;
                    self.last_error = None;
                    self.last_executed_sql = Some(sql.clone());
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
                            let _ = backend
                                .query_stream(&sql_owned, query_id, tx)
                                .await;
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
                    let active_db = self.active_database.clone();
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

                                // Load column names for the active database.
                                let active_table_columns = match &active_db {
                                    Some(db) => {
                                        backend.list_table_columns(db).await.unwrap_or_default()
                                    }
                                    None => std::collections::HashMap::new(),
                                };

                                let _ = tx
                                    .send(DbMessage::SchemaLoaded(
                                        conn_id,
                                        Ok(SchemaSnapshot {
                                            tree,
                                            active_table_columns,
                                        }),
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
                self.active_database = None;
                tracing::info!("disconnected");
            }
            Action::SelectDatabase(db) => {
                // Reconnect with the selected database as the pool default.
                // This guarantees every pooled connection uses `db`, so
                // unqualified queries (e.g. `SELECT * FROM orders`) resolve
                // correctly without `database.table` prefixes.
                self.active_database = Some(db.clone());
                self.notice = Some(format!("Switching to database `{db}`…"));
                self.mode = AppMode::Connecting;
                tracing::info!("switching database to `{db}`");
                if let Some(ref conn) = self.connection {
                    let tx = self.db_tx.clone();
                    let mut cfg = conn.config.clone();
                    cfg.database = Some(db.clone());
                    let db_name = db.clone();
                    tokio::spawn(async move {
                        match MySqlBackend::connect(&cfg).await {
                            Ok(backend) => match backend.ping().await {
                                Ok(()) => {
                                    let _ = tx
                                        .send(DbMessage::DatabaseSwitched(db_name, Ok(backend)))
                                        .await;
                                }
                                Err(e) => {
                                    let _ = tx
                                        .send(DbMessage::DatabaseSwitched(db_name, Err(e)))
                                        .await;
                                }
                            },
                            Err(e) => {
                                let _ = tx
                                    .send(DbMessage::DatabaseSwitched(db_name, Err(e)))
                                    .await;
                            }
                        }
                    });
                }
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
                        // Save to history only on success.
                        if let Some(ref sql) = self.last_executed_sql {
                            self.history.add(sql);
                            self.components.query_editor.push_history(sql);
                        }
                    }
                    Err(e) => {
                        self.set_error(e.to_string());
                        self.error_popup = Some(e.to_string());
                        self.mode = AppMode::Popup(PopupKind::Error);
                    }
                }
                self.pending_query = None;
                self.last_executed_sql = None;
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
            DbMessage::DatabaseSwitched(db_name, result) => {
                self.mode = AppMode::Normal;
                match result {
                    Ok(backend) => {
                        if let Some(ref mut conn) = self.connection {
                            conn.backend = backend;
                            conn.schema_snapshot = None;
                            conn.config.database = Some(db_name.clone());
                        }
                        // Reload per-database history.
                        self.history = QueryHistory::for_database(&db_name);
                        // Sync editor in-memory history.
                        self.components.query_editor.sync_history(&self.history.sql_list());
                        self.notice = Some(format!("Using database `{db_name}`"));
                        tracing::info!("database switched to `{db_name}`");
                        // Reload schema tree for the new default database.
                        if let Some(ref conn) = self.connection {
                            let conn_id = conn.id;
                            self.apply_action(&Action::LoadSchema(conn_id));
                        }
                    }
                    Err(e) => {
                        self.set_error(e.to_string());
                        self.active_database = None;
                    }
                }
            }
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
            active_database: self.active_database.as_deref(),
            schema: self
                .connection
                .as_ref()
                .and_then(|c| c.schema_snapshot.as_ref()),
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

        // Status bar is below the schema tree (left column bottom).
        self.components.status_bar.render(frame, right_bottom, &ctx);

        // Draw popup overlays.
        match self.mode {
            AppMode::Popup(PopupKind::Help) => {
                render_help_popup(frame, area, ctx.theme);
            }
            AppMode::Popup(PopupKind::History) => {
                let filtered: Vec<&crate::history::HistoryEntry> =
                    self.history.search(&self.history_search);
                render_history_popup(
                    frame,
                    area,
                    &filtered,
                    self.history_selected,
                    &self.history_search,
                    ctx.theme,
                );
            }
            AppMode::Popup(PopupKind::Error) if self.error_popup.is_some() => {
                render_error_popup(frame, area, self.error_popup.as_ref().unwrap(), ctx.theme);
            }
            _ => {}
        }
    }
}

/// Render a centered error popup overlay with the given message.
fn render_error_popup(frame: &mut Frame<'_>, area: Rect, message: &str, theme: &Theme) {
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
        .border_style(Style::default().fg(theme.status_error));
    frame.render_widget(
        Paragraph::new(message)
            .block(block)
            .wrap(Wrap { trim: false }),
        popup,
    );
}

/// Render the help popup overlay showing keybindings.
fn render_help_popup(frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
    let popup = Rect {
        x: area.width.saturating_sub(62) / 2,
        y: area.height.saturating_sub(20) / 2,
        width: 62.min(area.width),
        height: 20.min(area.height),
    };

    let keybindings = [
        ("Tab / Shift+Tab", "Cycle panel focus"),
        ("↑/↓/←/→", "Navigate / Move cursor"),
        ("Enter", "Execute query / Connect / Toggle tree"),
        ("Alt+Enter", "Insert newline (multi-line SQL)"),
        ("Esc", "Return to editor from results"),
        ("r", "Refresh schema tree"),
        ("H", "Browse query history"),
        ("g / G", "Go to top / bottom in results"),
        ("PgUp / PgDn", "Scroll page in results"),
        ("Ctrl+U / Ctrl+D", "Scroll half page up / down"),
        ("Ctrl+L", "Clear query editor"),
        ("Ctrl+W", "Delete word backward"),
        ("Ctrl+U", "Delete to line start (editor)"),
        ("?", "Toggle this help"),
        ("D", "Disconnect"),
        ("Ctrl+C", "Cancel query / Quit"),
        ("q", "Quit (except when typing in editor)"),
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
        .border_style(Style::default().fg(theme.highlight));
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

/// Render the history popup overlay — split view: list (left) + preview (right).
fn render_history_popup(
    frame: &mut Frame<'_>,
    area: Rect,
    entries: &[&crate::history::HistoryEntry],
    selected: usize,
    search: &str,
    theme: &Theme,
) {
    let popup_width = 100.min(area.width);
    let popup_height = 24.min(area.height);
    let popup = Rect {
        x: area.width.saturating_sub(popup_width) / 2,
        y: area.height.saturating_sub(popup_height) / 2,
        width: popup_width,
        height: popup_height,
    };

    frame.render_widget(Clear, popup);

    // Split into left (list, 40%) and right (preview, 60%).
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(popup);

    let list_area = cols[0];
    let preview_area = cols[1];

    let title = if search.is_empty() {
        format!(" History ({}) ", entries.len())
    } else {
        format!(" History ({}) — \"{search}\" ", entries.len())
    };

    let footer = " [Enter] load  [↑↓] nav  [type] search  [Esc] close ";

    if entries.is_empty() {
        let msg = if search.is_empty() {
            " No history yet "
        } else {
            " No matches "
        };
        frame.render_widget(
            Paragraph::new(msg).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(title)
                    .border_style(Style::default().fg(theme.highlight)),
            ),
            popup,
        );
        return;
    }

    // --- Left: query list ---
    let max_sql_len = list_area.width.saturating_sub(8) as usize;

    let items: Vec<ratatui::text::Line<'_>> = entries
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let is_selected = i == selected;
            let style = if is_selected {
                Style::default()
                    .fg(ratatui::style::Color::Black)
                    .bg(theme.highlight)
            } else {
                Style::default().fg(theme.text)
            };

            // Collapse multi-line SQL for the list display.
            let single_line = entry.sql.replace('\n', " ");
            let display_sql = if single_line.len() > max_sql_len {
                format!("{}…", &single_line[..max_sql_len])
            } else {
                single_line
            };

            let count_tag = if entry.count > 1 {
                format!(" {}×", entry.count)
            } else {
                String::new()
            };

            ratatui::text::Line::styled(format!("{count_tag:>4} {display_sql}"), style)
        })
        .collect();

    let list = ratatui::widgets::List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(Style::default().fg(theme.highlight)),
    );

    let mut list_state = ratatui::widgets::ListState::default();
    list_state.select(Some(selected));
    frame.render_stateful_widget(list, list_area, &mut list_state);

    // --- Right: full SQL preview with syntax highlighting ---
    let preview_entry = entries.get(selected);
    let preview_lines: Vec<ratatui::text::Line<'_>> = match preview_entry {
        Some(entry) => {
            use crate::sql::{TokenKind, tokenize};
            let count_label = if entry.count > 1 {
                format!("  (executed {}× — last: {})\n", entry.count, short_timestamp(&entry.last_used))
            } else {
                format!("  (last: {})\n", short_timestamp(&entry.last_used))
            };

            let mut lines: Vec<ratatui::text::Line<'_>> = vec![ratatui::text::Line::styled(
                count_label,
                Style::default().fg(theme.text_dim),
            )];

            // Render each line of the SQL with syntax highlighting.
            for (i, line_text) in entry.sql.split('\n').enumerate() {
                let mut spans: Vec<ratatui::text::Span<'_>> = Vec::new();
                if i == 0 {
                    spans.push(ratatui::text::Span::raw("  "));
                } else {
                    spans.push(ratatui::text::Span::raw("  "));
                }
                let tokens = tokenize(line_text);
                for tok in &tokens {
                    let color = match tok.kind {
                        TokenKind::Keyword => theme.sql_keyword,
                        TokenKind::Function => theme.sql_function,
                        TokenKind::String => theme.sql_string,
                        TokenKind::Number => theme.sql_number,
                        TokenKind::Comment => theme.sql_comment,
                        TokenKind::Operator => theme.sql_operator,
                        _ => theme.text,
                    };
                    spans.push(ratatui::text::Span::styled(
                        tok.text,
                        Style::default().fg(color),
                    ));
                }
                lines.push(ratatui::text::Line::from(spans));
            }
            lines
        }
        None => vec![ratatui::text::Line::raw(" (no selection)")],
    };

    let preview_block = Block::default()
        .borders(Borders::ALL)
        .title(" Preview ")
        .border_style(Style::default().fg(theme.highlight))
        .title_bottom(footer);

    frame.render_widget(
        ratatui::widgets::Paragraph::new(preview_lines).block(preview_block),
        preview_area,
    );
}

/// Shorten an ISO 8601 timestamp to a human-readable "date time" string.
fn short_timestamp(iso: &str) -> String {
    // Input like "2026-07-16T05:39:47.123456789+00:00"
    // Output: "2026-07-16 05:39"
    if iso.len() >= 16 {
        format!("{} {}", &iso[..10], &iso[11..16])
    } else {
        iso.to_string()
    }
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

/// Compute the layout: left column (schema + status bar), right column (editor/results).
///
/// Returns `(schema_area, main_area, status_area)`:
/// - `schema_area` — top of left column (schema tree / connection list)
/// - `main_area` — full-height right column (query editor / result table)
/// - `status_area` — bottom of left column (status bar)
fn layout(area: Rect) -> (Rect, Rect, Rect) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(area);

    let left_col = columns.first().copied().unwrap_or_default();
    let right_col = columns.get(1).copied().unwrap_or_default();

    // Left column: schema tree (top) + status bar (bottom, 1 line).
    let left_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(1)])
        .split(left_col);

    let schema_area = left_rows.first().copied().unwrap_or_default();
    let status_area = left_rows.get(1).copied().unwrap_or_default();

    (schema_area, right_col, status_area)
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
