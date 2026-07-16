//! dbtui — a terminal UI database client.

use std::path::PathBuf;

use color_eyre::Result;
use color_eyre::eyre::WrapErr as _;
use tokio::sync::mpsc;

mod app;
mod components;
mod config;
mod db;
mod error;
mod event;
mod history;
mod sql;
mod tui;

use app::App;
use event::{DbMessage, Event, spawn_event_task};
use tui::TerminalGuard;

/// Initialize tracing with a daily-rotating file appender.
///
/// Logs are written to `~/.config/dbtui/logs/dbtui.log.YYYY-MM-DD`.
/// Returns the guard that must be held for the program lifetime;
/// returns `None` if the log directory cannot be created.
fn init_tracing() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let log_dir: PathBuf = config::Config::config_dir()?.join("logs");

    if std::fs::create_dir_all(&log_dir).is_err() {
        return None;
    }

    let file_appender = tracing_appender::rolling::daily(&log_dir, "dbtui.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("dbtui=info"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(non_blocking)
        .init();

    Some(guard)
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    let _tracing_guard = init_tracing();
    tracing::info!("dbtui starting");

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

    tracing::info!("dbtui exiting");
    Ok(())
}
