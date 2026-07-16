# dbtui

Terminal UI database client for MySQL. Built with Rust, Ratatui, sqlx, and Tokio.

## Features

- **Async event loop** — UI never blocks on DB operations (Tokio + channels)
- **Streaming results** — Large query results stream page-by-page, memory-bounded
- **Schema browser** — Navigate databases/tables with an expandable tree
- **TLS support** — CA cert verification, mutual TLS, hostname checking
- **Multi-backend ready** — `Database` trait abstraction; adding PostgreSQL/SQLite is one file

## Quick Start

```bash
# Build
cargo build --release

# Configure
mkdir -p ~/.config/dbtui
cp connections.toml.example ~/.config/dbtui/connections.toml
# Edit the config with your MySQL connection details

# Run
./target/release/dbtui
```

## Configuration

Config file: `~/.config/dbtui/connections.toml`

```toml
[[connections]]
name = "local"
driver = "mysql"
host = "127.0.0.1"
port = 3306
user = "root"
password = "yourpass"
database = "test"
tls = "disabled"           # or: preferred, required, verify_ca, verify_identity

# CA certificate verification
ssl_ca = "/path/to/ca.pem" # required for verify_ca / verify_identity

# Mutual TLS (optional)
# ssl_client_cert = "/path/to/client-cert.pem"
# ssl_client_key = "/path/to/client-key.pem"
```

### TLS modes

| Mode | Behavior |
|---|---|
| `disabled` | No encryption |
| `preferred` | Try TLS, fall back to plain (default) |
| `required` | Force TLS, no cert verification |
| `verify_ca` | Force TLS + verify server cert against `ssl_ca` |
| `verify_identity` | Force TLS + verify cert + verify hostname |

## Keybindings

| Key | Action |
|---|---|---|
| `j` / `↓` | Move down / Next history (in editor) |
| `k` / `↑` | Move up / Previous history (in editor) |
| `Tab` / `Shift+Tab` | Cycle focus between panels |
| `Enter` | Connect (in list) / Execute query (in editor) / Toggle expand (in tree) |
| `→` | Expand schema in tree |
| `←` | Collapse schema in tree |
| `Esc` | Return to query editor from results / Close popup |
| `r` | Refresh schema tree |
| `?` | Toggle help popup |
| `D` | Disconnect from current database |
| `Ctrl+C` | Cancel running query, or quit |
| `q` | Quit (except when typing in query editor / popup open) |

## Architecture

```
main.rs          Entry point, Tokio runtime, dependency wiring
app.rs           App state machine, event dispatch, async event loop
event.rs         Event / Action / DbMessage three-way separation
tui.rs           Terminal lifecycle (RAII guard for raw mode)
config.rs        TOML config load/save (dirs + serde)
error.rs         Layered error types (DbError / ConfigError / Error)
db.rs            Database trait + shared types
db/mysql.rs      MySQL backend (sqlx pool, streaming, introspection)
db/mock.rs       MockBackend for unit testing
components.rs    Component trait, AppContext, Panel, Theme
components/      UI components (editor, results, schema tree, status bar, ...)
```

### Key design decisions

- **UI never blocks**: All DB operations run in spawned Tokio tasks; results arrive via `mpsc` channels
- **Component trait**: Components return `Action` intents — no direct side effects (fully testable)
- **Streaming pagination**: `query_stream` sends results in 100-row pages, capped at 50,000 rows
- **Stale message filtering**: `QueryId` tracking discards results from cancelled/superseded queries

## Testing

```bash
# Unit tests (MockBackend, no MySQL needed)
cargo test

# Integration tests (requires live MySQL)
DBTUI_TEST_MYSQL_HOST=127.0.0.1 cargo test -- --ignored
```

## License

MIT OR Apache-2.0
