//! Database abstraction layer: shared types and the [`Database`] async trait.
//!
//! Backend implementations (`db/mysql.rs`) arrive in M2. This module
//! defines the trait and data shapes so that `DbMessage` and connection
//! handling compile from M1.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::error::DbError;
use crate::event::{ConnectionId, DbMessage, QueryId};

// Submodules
pub mod mock;
pub mod mysql;

/// Maximum rows per streaming page (architecture §4.4).
pub const PAGE_SIZE: usize = 100;

/// Maximum total rows before truncation (architecture §4.4).
pub const MAX_ROWS: usize = 50_000;

// ---------------------------------------------------------------------------
// Schema introspection types
// ---------------------------------------------------------------------------

/// A database/schema name (`MySQL`: a logical database).
#[derive(Debug, Clone)]
pub struct SchemaInfo {
    /// Schema name.
    pub name: String,
}

/// A table name within a schema.
#[derive(Debug, Clone)]
pub struct TableInfo {
    /// Table name.
    pub name: String,
}

/// Column metadata from `describe_table`.
#[derive(Debug, Clone)]
pub struct ColumnInfo {
    /// Column name.
    pub name: String,
    /// Type as reported by the database (e.g. `VARCHAR(255)`).
    pub type_name: String,
    /// Whether the column accepts NULL.
    pub nullable: bool,
    /// Default value expression, if any.
    pub default: Option<String>,
    /// Whether this column is part of the primary key.
    pub is_primary_key: bool,
}

// ---------------------------------------------------------------------------
// Query result types
// ---------------------------------------------------------------------------

/// Coarse type category used for rendering hints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellKind {
    /// Integer type.
    Int,
    /// Floating-point type.
    Float,
    /// Text/character type.
    Text,
    /// Date/time type.
    DateTime,
    /// Binary type.
    Bytes,
    /// SQL NULL.
    Null,
    /// Unrecognized type.
    Unknown,
}

/// Column metadata for a result set.
#[derive(Debug, Clone)]
pub struct ColumnMeta {
    /// Column name.
    pub name: String,
    /// Original type name from the database.
    pub type_name: String,
    /// Coarse type category.
    pub kind: CellKind,
}

/// A single cell value, already stringified for rendering.
#[derive(Debug, Clone)]
pub enum CellValue {
    /// SQL NULL.
    Null,
    /// Text value.
    Text(String),
    /// Binary data rendered as hexadecimal.
    BytesHex(String),
}

/// One page of streaming query results.
#[derive(Debug, Clone)]
pub struct QueryPage {
    /// Column definitions; present only on the first page, `None` thereafter.
    pub columns: Option<Vec<ColumnMeta>>,
    /// Rows in this page.
    pub rows: Vec<Vec<CellValue>>,
}

/// Query completion metadata.
#[derive(Debug, Clone)]
pub struct QueryMeta {
    /// Rows affected (for INSERT/UPDATE/DELETE), if applicable.
    pub affected_rows: Option<u64>,
    /// Total rows returned (for SELECT).
    pub rows_returned: u64,
    /// Query elapsed time.
    pub elapsed: Duration,
    /// Whether the result was truncated at `MAX_ROWS`.
    pub truncated: bool,
}

/// Accumulated result set for table display.
#[derive(Debug, Clone, Default)]
pub struct ResultSet {
    /// Column definitions.
    pub columns: Vec<ColumnMeta>,
    /// All accumulated rows.
    pub rows: Vec<Vec<CellValue>>,
    /// Completion metadata (set when query finishes).
    pub meta: Option<QueryMeta>,
    /// Whether the query has completed.
    pub complete: bool,
}

/// Result of a non-query execution (INSERT/UPDATE/DDL).
#[derive(Debug, Clone, Default)]
pub struct ExecResult {
    /// Rows affected.
    pub rows_affected: u64,
}

/// A snapshot of schemas and tables for a connection.
#[derive(Debug, Clone)]
pub struct SchemaSnapshot {
    /// Tree of `(schema_name, table_names)` pairs.
    pub tree: Vec<(String, Vec<String>)>,
}

// ---------------------------------------------------------------------------
// Connection handle
// ---------------------------------------------------------------------------

/// A live connection held by the application after a successful connect.
pub struct ConnectionHandle {
    /// Connection identifier.
    pub id: ConnectionId,
    /// Human-readable connection name (for status bar display).
    pub name: String,
    /// Backend handle (trait object).
    pub backend: Arc<dyn Database>,
    /// Schema snapshot, populated after `LoadSchema`.
    pub schema_snapshot: Option<SchemaSnapshot>,
}

// ---------------------------------------------------------------------------
// Database trait
// ---------------------------------------------------------------------------

/// Unified database backend interface (architecture §2.1).
///
/// Implementations: `MySqlBackend` (M2). Future: `PostgresBackend`,
/// `SqliteBackend`.
#[async_trait]
pub trait Database: Send + Sync {
    /// Test connectivity.
    async fn ping(&self) -> Result<(), DbError>;

    /// List all schemas/databases.
    async fn list_schemas(&self) -> Result<Vec<SchemaInfo>, DbError>;

    /// List tables in the given schema.
    async fn list_tables(&self, schema: &str) -> Result<Vec<TableInfo>, DbError>;

    /// Describe the columns of a table.
    async fn describe_table(&self, schema: &str, table: &str) -> Result<Vec<ColumnInfo>, DbError>;

    /// Execute a non-query statement (INSERT/UPDATE/DDL).
    async fn execute(&self, sql: &str) -> Result<ExecResult, DbError>;

    /// Stream query results page-by-page via `tx`.
    async fn query_stream(
        &self,
        sql: &str,
        query_id: QueryId,
        tx: mpsc::Sender<DbMessage>,
    ) -> Result<(), DbError>;

    /// Cancel an in-progress query (best-effort).
    async fn cancel(&self, query_id: QueryId) -> Result<(), DbError>;
}
