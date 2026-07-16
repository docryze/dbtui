//! `MySQL` backend implementation using sqlx (architecture §2.1, §3.2).
//!
//! Connects via `MySqlPool`, supports schema introspection via
//! `information_schema`, and streams query results page-by-page.

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use chrono::{NaiveDate, NaiveDateTime};
use futures::StreamExt;
use sqlx::mysql::{MySqlConnectOptions, MySqlPool, MySqlPoolOptions, MySqlSslMode};
use sqlx::{AssertSqlSafe, Column, Row, TypeInfo};
use tokio::sync::mpsc;

use crate::config::{ConnectionConfig, TlsMode};
use crate::db::{
    CellKind, CellValue, ColumnInfo, ColumnMeta, Database, ExecResult, MAX_ROWS, PAGE_SIZE,
    QueryMeta, QueryPage, SchemaInfo, TableInfo,
};
use crate::error::DbError;
use crate::event::{DbMessage, QueryId};

/// `MySQL` backend backed by a [`MySqlPool`].
#[derive(Debug)]
pub struct MySqlBackend {
    pool: MySqlPool,
}

impl MySqlBackend {
    /// Establish a connection pool and return a trait object.
    ///
    /// # Errors
    /// Returns [`DbError::Sqlx`] if the pool cannot be created.
    pub async fn connect(cfg: &ConnectionConfig) -> Result<Arc<dyn Database>, DbError> {
        let mut options = MySqlConnectOptions::new()
            .host(&cfg.host)
            .port(cfg.port)
            .username(&cfg.user)
            .ssl_mode(map_ssl_mode(cfg.tls));

        if let Some(ref pw) = cfg.password {
            options = options.password(pw.reveal());
        }
        if let Some(ref db) = cfg.database {
            options = options.database(db);
        }
        if let Some(ref ca) = cfg.ssl_ca {
            options = options.ssl_ca(ca);
        }
        if let Some(ref cert) = cfg.ssl_client_cert {
            options = options.ssl_client_cert(cert);
        }
        if let Some(ref key) = cfg.ssl_client_key {
            options = options.ssl_client_key(key);
        }

        let pool = MySqlPoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await
            .map_err(DbError::from)?;

        let backend: Arc<dyn Database> = Arc::new(Self { pool });
        Ok(backend)
    }
}

#[async_trait]
impl Database for MySqlBackend {
    async fn ping(&self) -> Result<(), DbError> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .map_err(DbError::from)?;
        Ok(())
    }

    async fn list_schemas(&self) -> Result<Vec<SchemaInfo>, DbError> {
        let rows =
            sqlx::query("SELECT SCHEMA_NAME FROM information_schema.SCHEMATA ORDER BY SCHEMA_NAME")
                .fetch_all(&self.pool)
                .await?;

        rows.iter()
            .map(|row| {
                let name: String = row.try_get(0)?;
                Ok(SchemaInfo { name })
            })
            .collect()
    }

    async fn list_tables(&self, schema: &str) -> Result<Vec<TableInfo>, DbError> {
        let rows = sqlx::query(
            "SELECT TABLE_NAME FROM information_schema.TABLES \
             WHERE TABLE_SCHEMA = ? ORDER BY TABLE_NAME",
        )
        .bind(schema)
        .fetch_all(&self.pool)
        .await?;

        rows.iter()
            .map(|row| {
                let name: String = row.try_get(0)?;
                Ok(TableInfo { name })
            })
            .collect()
    }

    async fn describe_table(&self, schema: &str, table: &str) -> Result<Vec<ColumnInfo>, DbError> {
        let rows = sqlx::query(
            "SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_DEFAULT, COLUMN_KEY \
             FROM information_schema.COLUMNS \
             WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? \
             ORDER BY ORDINAL_POSITION",
        )
        .bind(schema)
        .bind(table)
        .fetch_all(&self.pool)
        .await?;

        rows.iter()
            .map(|row| {
                let name: String = row.try_get(0)?;
                let type_name: String = row.try_get(1)?;
                let nullable: String = row.try_get(2)?;
                let default: Option<String> = row.try_get(3)?;
                let key: String = row.try_get(4)?;
                Ok(ColumnInfo {
                    name,
                    type_name,
                    nullable: nullable == "YES",
                    default,
                    is_primary_key: key == "PRI",
                })
            })
            .collect()
    }

    async fn execute(&self, sql_text: &str) -> Result<ExecResult, DbError> {
        let result = sqlx::query(AssertSqlSafe(sql_text))
            .execute(&self.pool)
            .await
            .map_err(DbError::from)?;
        Ok(ExecResult {
            rows_affected: result.rows_affected(),
        })
    }

    async fn query_stream(
        &self,
        sql_text: &str,
        query_id: QueryId,
        tx: mpsc::Sender<DbMessage>,
    ) -> Result<(), DbError> {
        let mut stream = sqlx::query(AssertSqlSafe(sql_text)).fetch(&self.pool);
        let start = Instant::now();

        let mut first_page_sent = false;
        let mut column_meta: Option<Vec<ColumnMeta>> = None;
        let mut buffered: Vec<Vec<CellValue>> = Vec::with_capacity(PAGE_SIZE);
        let mut total_rows: usize = 0;
        let mut truncated = false;

        while total_rows < MAX_ROWS {
            let row = match stream.next().await {
                Some(Ok(row)) => row,
                Some(Err(e)) => {
                    let db_err = DbError::from(e);
                    let _ = tx.send(DbMessage::QueryPage(query_id, Err(db_err))).await;
                    let _ = tx
                        .send(DbMessage::QueryComplete(
                            query_id,
                            Err(DbError::Other("query execution error".into())),
                        ))
                        .await;
                    return Ok(());
                }
                None => break,
            };

            if column_meta.is_none() {
                column_meta = Some(extract_columns(&row));
            }

            buffered.push(row_to_cells(&row));
            total_rows = total_rows.saturating_add(1);

            if buffered.len() >= PAGE_SIZE {
                let cols = if first_page_sent {
                    None
                } else {
                    first_page_sent = true;
                    column_meta.clone()
                };
                let _ = tx
                    .send(DbMessage::QueryPage(
                        query_id,
                        Ok(QueryPage {
                            columns: cols,
                            rows: std::mem::take(&mut buffered),
                        }),
                    ))
                    .await;
            }
        }

        if total_rows >= MAX_ROWS {
            truncated = true;
        }

        // Flush remaining rows.
        if !buffered.is_empty() {
            let cols = if first_page_sent {
                None
            } else {
                column_meta.clone()
            };
            let _ = tx
                .send(DbMessage::QueryPage(
                    query_id,
                    Ok(QueryPage {
                        columns: cols,
                        rows: std::mem::take(&mut buffered),
                    }),
                ))
                .await;
        }

        let _ = tx
            .send(DbMessage::QueryComplete(
                query_id,
                Ok(QueryMeta {
                    affected_rows: None,
                    rows_returned: total_rows as u64,
                    elapsed: start.elapsed(),
                    truncated,
                }),
            ))
            .await;

        Ok(())
    }

    async fn cancel(&self, _query_id: QueryId) -> Result<(), DbError> {
        // Best-effort: sqlx streams are cancelled by dropping them.
        // True cancellation (aborting the spawned query task) arrives in M4.
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// TLS mapping
// ---------------------------------------------------------------------------

/// Map our [`TlsMode`] to sqlx's [`MySqlSslMode`].
fn map_ssl_mode(mode: TlsMode) -> MySqlSslMode {
    match mode {
        TlsMode::Disabled => MySqlSslMode::Disabled,
        TlsMode::Preferred => MySqlSslMode::Preferred,
        TlsMode::Required => MySqlSslMode::Required,
        TlsMode::VerifyCa => MySqlSslMode::VerifyCa,
        TlsMode::VerifyIdentity => MySqlSslMode::VerifyIdentity,
    }
}

// ---------------------------------------------------------------------------
// Cell decoding helpers
// ---------------------------------------------------------------------------

/// Extract column metadata from the first row of a result set.
fn extract_columns(row: &sqlx::mysql::MySqlRow) -> Vec<ColumnMeta> {
    row.columns()
        .iter()
        .map(|col| {
            let type_name = col.type_info().name().to_string();
            ColumnMeta {
                name: col.name().to_string(),
                kind: classify_type(&type_name),
                type_name,
            }
        })
        .collect()
}

/// Convert a row to a vector of [`CellValue`] by trying common types.
fn row_to_cells(row: &sqlx::mysql::MySqlRow) -> Vec<CellValue> {
    let count = row.columns().len();
    (0..count).map(|i| decode_cell(row, i)).collect()
}

/// Decode a single cell, trying common types in order of likelihood.
fn decode_cell(row: &sqlx::mysql::MySqlRow, idx: usize) -> CellValue {
    // String — also detects SQL NULL via Option.
    if let Ok(Some(v)) = row.try_get::<Option<String>, _>(idx) {
        return CellValue::Text(v);
    }
    if matches!(row.try_get::<Option<String>, _>(idx), Ok(None)) {
        return CellValue::Null;
    }

    // Integers.
    if let Ok(Some(v)) = row.try_get::<Option<i64>, _>(idx) {
        return CellValue::Text(v.to_string());
    }
    if matches!(row.try_get::<Option<i64>, _>(idx), Ok(None)) {
        return CellValue::Null;
    }

    // Floats.
    if let Ok(Some(v)) = row.try_get::<Option<f64>, _>(idx) {
        return CellValue::Text(v.to_string());
    }
    if matches!(row.try_get::<Option<f64>, _>(idx), Ok(None)) {
        return CellValue::Null;
    }

    // Boolean.
    if let Ok(Some(v)) = row.try_get::<Option<bool>, _>(idx) {
        return CellValue::Text(v.to_string());
    }
    if matches!(row.try_get::<Option<bool>, _>(idx), Ok(None)) {
        return CellValue::Null;
    }

    // NaiveDateTime.
    if let Ok(Some(v)) = row.try_get::<Option<NaiveDateTime>, _>(idx) {
        return CellValue::Text(v.to_string());
    }
    if matches!(row.try_get::<Option<NaiveDateTime>, _>(idx), Ok(None)) {
        return CellValue::Null;
    }

    // NaiveDate.
    if let Ok(Some(v)) = row.try_get::<Option<NaiveDate>, _>(idx) {
        return CellValue::Text(v.to_string());
    }
    if matches!(row.try_get::<Option<NaiveDate>, _>(idx), Ok(None)) {
        return CellValue::Null;
    }

    // Binary.
    if let Ok(Some(v)) = row.try_get::<Option<Vec<u8>>, _>(idx) {
        return CellValue::BytesHex(to_hex(&v));
    }
    if matches!(row.try_get::<Option<Vec<u8>>, _>(idx), Ok(None)) {
        return CellValue::Null;
    }

    CellValue::Text("<unsupported>".into())
}

/// Classify a `MySQL` type name string into a coarse [`CellKind`].
fn classify_type(type_name: &str) -> CellKind {
    let upper = type_name.to_ascii_uppercase();
    if upper.contains("INT") {
        CellKind::Int
    } else if upper.contains("FLOAT")
        || upper.contains("DOUBLE")
        || upper.contains("DECIMAL")
        || upper.contains("NUM")
    {
        CellKind::Float
    } else if upper.contains("CHAR")
        || upper.contains("TEXT")
        || upper.contains("ENUM")
        || upper.contains("JSON")
    {
        CellKind::Text
    } else if upper.contains("DATE") || upper.contains("TIME") || upper.contains("YEAR") {
        CellKind::DateTime
    } else if upper.contains("BLOB") || upper.contains("BINARY") || upper.contains("BIT") {
        CellKind::Bytes
    } else {
        CellKind::Unknown
    }
}

/// Encode bytes as lowercase hexadecimal.
fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(
        String::with_capacity(bytes.len().saturating_mul(2)),
        |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        },
    )
}

// ---------------------------------------------------------------------------
// Integration tests (require a live MySQL; run with --ignored)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Driver, TlsMode};
    use crate::event::ConnectionId;

    fn test_config() -> ConnectionConfig {
        ConnectionConfig {
            id: ConnectionId::new(),
            name: "test".into(),
            driver: Driver::Mysql,
            host: "127.0.0.1".into(),
            port: 3306,
            user: "root".into(),
            password: None,
            database: std::env::var("DBTUI_TEST_MYSQL_DB")
                .ok()
                .or(Some("test".into())),
            tls: TlsMode::Preferred,
            ssl_ca: None,
            ssl_client_cert: None,
            ssl_client_key: None,
        }
    }

    fn skip_if_no_mysql() -> Option<ConnectionConfig> {
        if std::env::var("DBTUI_TEST_MYSQL_URL").is_err()
            && std::env::var("DBTUI_TEST_MYSQL_HOST").is_err()
        {
            return None;
        }
        Some(test_config())
    }

    #[tokio::test]
    #[ignore = "requires a live MySQL instance"]
    async fn mysql_ping() -> Result<(), DbError> {
        let cfg = match skip_if_no_mysql() {
            Some(c) => c,
            None => return Ok(()),
        };
        let backend = MySqlBackend::connect(&cfg).await?;
        backend.ping().await?;
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires a live MySQL instance"]
    async fn mysql_list_schemas() -> Result<(), DbError> {
        let cfg = match skip_if_no_mysql() {
            Some(c) => c,
            None => return Ok(()),
        };
        let backend = MySqlBackend::connect(&cfg).await?;
        let schemas = backend.list_schemas().await?;
        assert!(!schemas.is_empty(), "should have at least one schema");
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires a live MySQL instance"]
    async fn mysql_query_stream() -> Result<(), DbError> {
        let cfg = match skip_if_no_mysql() {
            Some(c) => c,
            None => return Ok(()),
        };
        let backend = MySqlBackend::connect(&cfg).await?;

        let (tx, mut rx) = mpsc::channel(256);
        backend
            .query_stream("SELECT 1 AS n", QueryId::new(), tx)
            .await?;

        // Should receive at least one QueryPage with columns.
        let mut got_columns = false;
        let mut got_complete = false;
        while let Some(msg) = rx.recv().await {
            match msg {
                DbMessage::QueryPage(_, Ok(page)) => {
                    if page.columns.is_some() {
                        got_columns = true;
                    }
                }
                DbMessage::QueryComplete(_, Ok(_)) => {
                    got_complete = true;
                    break;
                }
                _ => {}
            }
        }
        assert!(got_columns, "should have received column metadata");
        assert!(got_complete, "should have received QueryComplete");
        Ok(())
    }
}
