//! Component trait, application context, and shared UI types
//! (architecture §2.2, §6).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Color;

use crate::event::{Action, Event};

// Submodules
pub mod connection_list;
pub mod query_editor;
pub mod result_table;
pub mod schema_tree;
pub mod status_bar;

pub use connection_list::ConnectionList;
pub use query_editor::QueryEditor;
pub use result_table::ResultTable;
pub use schema_tree::SchemaTree;
pub use status_bar::StatusBar;

// ---------------------------------------------------------------------------
// Panel & PopupKind
// ---------------------------------------------------------------------------

/// Which panel currently holds focus. Determines border highlight and
/// event routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Panel {
    /// Left panel: schema/database tree.
    #[default]
    SchemaTree,
    /// Right-top panel: SQL editor.
    QueryEditor,
    /// Right-top panel (alternate): result table.
    ResultTable,
}

impl Panel {
    /// Cycle to the next focusable panel.
    pub fn next(self) -> Self {
        match self {
            Self::SchemaTree => Self::QueryEditor,
            _ => Self::SchemaTree,
        }
    }

    /// Cycle to the previous focusable panel.
    pub fn prev(self) -> Self {
        match self {
            Self::QueryEditor => Self::SchemaTree,
            _ => Self::QueryEditor,
        }
    }
}

/// Kind of popup overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopupKind {
    /// Error popup.
    Error,
    /// Confirmation dialog.
    Confirm,
    /// Text input dialog.
    Input,
    /// Help / keybindings.
    Help,
}

// ---------------------------------------------------------------------------
// AppMode & Theme
// ---------------------------------------------------------------------------

/// Application mode (architecture §3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AppMode {
    /// Normal operation.
    #[default]
    Normal,
    /// A connection is being established.
    Connecting,
    /// A popup is open.
    Popup(PopupKind),
}

/// Color theme for the UI.
#[derive(Debug, Clone)]
pub struct Theme {
    /// Border color for the focused panel.
    pub border_focused: Color,
    /// Border color for non-focused panels.
    pub border_normal: Color,
    /// Default text color.
    pub text: Color,
    /// Dimmed text color (NULL, secondary info).
    pub text_dim: Color,
    /// Default background color (for alternating row styles).
    pub bg: Color,
    /// Status bar "ready"/"connected" indicator color.
    pub status_ready: Color,
    /// Status bar error indicator color.
    pub status_error: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            border_focused: Color::Cyan,
            border_normal: Color::DarkGray,
            text: Color::Gray,
            text_dim: Color::DarkGray,
            bg: Color::Reset,
            status_ready: Color::Green,
            status_error: Color::Red,
        }
    }
}

// ---------------------------------------------------------------------------
// AppContext — read-only snapshot passed to components
// ---------------------------------------------------------------------------

/// Read-only snapshot passed to components during render and event handling
/// (architecture §2.2). Prevents components from mutating global state.
pub struct AppContext<'a> {
    /// Currently focused panel.
    pub focus: Panel,
    /// Current application mode.
    pub mode: AppMode,
    /// Visual theme.
    pub theme: &'a Theme,
    /// Active connection name, if connected.
    pub connection_name: Option<&'a str>,
    /// Whether a connection attempt is in progress.
    pub is_connecting: bool,
    /// Whether a query is currently being executed.
    pub is_executing: bool,
    /// Last error message for status bar display.
    pub error_message: Option<&'a str>,
    /// Transient notice (e.g. "42 rows in 0.3s").
    pub notice: Option<&'a str>,
}

// ---------------------------------------------------------------------------
// Component trait
// ---------------------------------------------------------------------------

/// Trait implemented by all renderable, interactive UI panels
/// (architecture §2.2).
///
/// Components are side-effect-free: `handle_event` only returns an
/// [`Action`]; the App decides whether to act on it.
pub trait Component: std::fmt::Debug {
    /// Render the component. **Read-only**: must not modify application state.
    fn render(&self, frame: &mut Frame<'_>, area: Rect, ctx: &AppContext<'_>);

    /// Handle a terminal event, returning an [`Action`] for the App to
    /// execute. **No side effects**: do not perform DB operations or
    /// state mutations here.
    fn handle_event(&mut self, event: &Event, ctx: &AppContext<'_>) -> Action;

    /// Whether this component currently holds focus (affects border/event
    /// routing). Default: `false`.
    fn is_focused(&self) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// Components aggregate
// ---------------------------------------------------------------------------

/// Aggregate of all component instances owned by [`App`](crate::app::App).
#[derive(Debug, Default)]
pub struct Components {
    /// Schema tree panel (left).
    pub schema_tree: SchemaTree,
    /// Query editor panel (right-top).
    pub query_editor: QueryEditor,
    /// Result table panel (right-top, alternate).
    pub result_table: ResultTable,
    /// Status bar (right-bottom).
    pub status_bar: StatusBar,
}
