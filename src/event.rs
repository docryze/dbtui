//! `Event`, `Action`, and `DbMessage` types — the three-way separation of concerns
//! described in architecture §2.3.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crossterm::event::{self as crossterm_event, EventStream, KeyEvent, MouseEvent};
use futures::StreamExt;
use tokio::sync::mpsc;

use crate::components::{Panel, PopupKind};
use crate::config::ConnectionConfig;
use crate::db::{ConnectionHandle, QueryMeta, QueryPage, SchemaSnapshot};
use crate::error::DbError;

// ---------------------------------------------------------------------------
// Identifier types
// ---------------------------------------------------------------------------

/// Unique identifier for a query, used for cancellation and stale-message
/// filtering (architecture §4.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct QueryId(u64);

impl QueryId {
    /// Allocate a new unique query ID.
    pub fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        Self(COUNTER.fetch_add(1, Ordering::Relaxed))
    }
}

impl Default for QueryId {
    fn default() -> Self {
        Self::new()
    }
}

/// Stable identifier for a connection, used for multi-tab addressing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConnectionId(u64);

impl ConnectionId {
    /// Allocate a new unique connection ID.
    pub fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        Self(COUNTER.fetch_add(1, Ordering::Relaxed))
    }
}

impl Default for ConnectionId {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Event — "what happened" (architecture §2.3)
// ---------------------------------------------------------------------------

/// Terminal or timer event, produced by the event task and consumed by the
/// main loop.
#[derive(Debug, Clone)]
pub enum Event {
    /// A key was pressed.
    Key(KeyEvent),
    /// A mouse event occurred.
    Mouse(MouseEvent),
    /// Text was pasted.
    Paste(String),
    /// Terminal was resized to `(width, height)`.
    Resize(u16, u16),
    /// Terminal gained focus.
    FocusGained,
    /// Terminal lost focus.
    FocusLost,
    /// Periodic tick — drives status bar refresh, cursor blink, etc.
    Tick,
}

/// Convert a crossterm event into our [`Event`].
fn from_crossterm(ev: crossterm_event::Event) -> Event {
    match ev {
        crossterm_event::Event::Key(k) => Event::Key(k),
        crossterm_event::Event::Mouse(m) => Event::Mouse(m),
        crossterm_event::Event::Resize(w, h) => Event::Resize(w, h),
        crossterm_event::Event::Paste(s) => Event::Paste(s),
        crossterm_event::Event::FocusGained => Event::FocusGained,
        crossterm_event::Event::FocusLost => Event::FocusLost,
    }
}

// ---------------------------------------------------------------------------
// Action — "what the component wants to do" (architecture §2.3)
// ---------------------------------------------------------------------------

/// Intent returned by a component's `handle_event`, executed by the App.
#[derive(Debug, Clone)]
pub enum Action {
    /// No-op.
    None,
    /// Quit the application.
    Quit,
    /// Request an immediate redraw.
    RequestRender,
    /// Change focus to a panel.
    Focus(Panel),
    /// Switch to a tab by index.
    SwitchTab(usize),
    /// Open a popup overlay.
    OpenPopup(PopupKind),
    /// Close the current popup.
    ClosePopup,
    /// Initiate a new connection.
    Connect(ConnectionConfig),
    /// Disconnect a connection.
    Disconnect(ConnectionId),
    /// Execute a SQL query.
    ExecuteQuery(String),
    /// Cancel an in-progress query.
    CancelQuery(QueryId),
    /// Refresh the schema tree for a connection.
    LoadSchema(ConnectionId),
}

// ---------------------------------------------------------------------------
// DbMessage — "the async result arrived" (architecture §2.3)
// ---------------------------------------------------------------------------

/// Messages from DB tasks back to the main loop.
pub enum DbMessage {
    /// Connection attempt completed.
    Connected(Result<ConnectionHandle, DbError>),
    /// Schema load completed for a connection.
    SchemaLoaded(ConnectionId, Result<SchemaSnapshot, DbError>),
    /// Query has started.
    QueryStarted(QueryId),
    /// One page of query results arrived.
    QueryPage(QueryId, Result<QueryPage, DbError>),
    /// Query completed.
    QueryComplete(QueryId, Result<QueryMeta, DbError>),
    /// Query was cancelled.
    Cancelled(QueryId),
}

// ---------------------------------------------------------------------------
// Event task (architecture §4.1)
// ---------------------------------------------------------------------------

/// Spawn the long-lived event task that merges crossterm's [`EventStream`]
/// with a periodic `Tick` into `tx`.
///
/// The task runs until `tx` is dropped (i.e. the main loop has exited).
pub fn spawn_event_task(tx: mpsc::Sender<Event>) {
    tokio::spawn(async move {
        let mut reader = EventStream::new();
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                biased;

                maybe_event = reader.next() => {
                    match maybe_event {
                        Some(Ok(ev)) => {
                            let our_event = from_crossterm(ev);
                            if tx.send(our_event).await.is_err() {
                                break;
                            }
                        }
                        Some(Err(_)) | None => break,
                    }
                }

                _ = tick.tick() => {
                    // Tick is droppable under backpressure (architecture §4.2).
                    let _ = tx.try_send(Event::Tick);
                }
            }
        }
    });
}
