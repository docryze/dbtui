//! dbtui — a terminal UI database client.
//!
//! M0: minimal TUI lifecycle. M1: async event loop + three-panel layout.
//! M2: data layer (Database trait + `MySQL` backend). M3: connection flow.

use color_eyre::Result;
use color_eyre::eyre::WrapErr as _;
use tokio::sync::mpsc;

mod app;
mod components;
mod config;
mod db;
mod error;
mod event;
mod tui;

use app::App;
use event::{DbMessage, Event, spawn_event_task};
use tui::TerminalGuard;

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    let mut guard = TerminalGuard::setup().wrap_err("failed to initialize the terminal")?;

    let config = config::Config::load().unwrap_or_default();

    let (event_tx, mut event_rx) = mpsc::channel::<Event>(1024);
    let (db_tx, mut db_rx) = mpsc::channel::<DbMessage>(256);

    spawn_event_task(event_tx);

    let mut application = App::new(db_tx, config.connections);

    app::run(
        &mut application,
        guard.terminal_mut(),
        &mut event_rx,
        &mut db_rx,
    )
    .await
    .wrap_err("dbtui exited unexpectedly")?;

    Ok(())
}
