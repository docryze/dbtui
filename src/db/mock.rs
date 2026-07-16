//! Mock database backend for unit testing (architecture §9.3).
//!
//! Implements [`Database`] with preset data — no `MySQL` required.

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::db::{
    ColumnInfo, ColumnMeta, Database, ExecResult, MAX_ROWS, PAGE_SIZE, QueryMeta, QueryPage,
    SchemaInfo, TableInfo,
};
use crate::error::DbError;
use crate::event::{DbMessage, QueryId};

/// A test-double backend that returns preset data for every method.
#[derive(Debug, Clone, Default)]
pub struct MockBackend {
    /// Schemas returned by `list_schemas`.
    pub schemas: Vec<SchemaInfo>,
    /// Tables returned by `list_tables`.
    pub tables: Vec<TableInfo>,
    /// Columns returned by `describe_table`.
    pub columns: Vec<ColumnInfo>,
    /// Column metadata for `query_stream`.
    pub query_columns: Vec<ColumnMeta>,
    /// Rows for `query_stream`.
    pub query_rows: Vec<Vec<crate::db::CellValue>>,
    /// Result for `execute`.
    pub exec_result: ExecResult,
}

#[async_trait]
impl Database for MockBackend {
    async fn ping(&self) -> Result<(), DbError> {
        Ok(())
    }

    async fn list_schemas(&self) -> Result<Vec<SchemaInfo>, DbError> {
        Ok(self.schemas.clone())
    }

    async fn list_tables(&self, _schema: &str) -> Result<Vec<TableInfo>, DbError> {
        Ok(self.tables.clone())
    }

    async fn describe_table(
        &self,
        _schema: &str,
        _table: &str,
    ) -> Result<Vec<ColumnInfo>, DbError> {
        Ok(self.columns.clone())
    }

    async fn list_table_columns(
        &self,
        _schema: &str,
    ) -> Result<std::collections::HashMap<String, Vec<String>>, DbError> {
        Ok(std::collections::HashMap::new())
    }

    async fn execute(&self, _sql: &str) -> Result<ExecResult, DbError> {
        Ok(self.exec_result.clone())
    }

    async fn query_stream(
        &self,
        _sql: &str,
        query_id: QueryId,
        tx: mpsc::Sender<DbMessage>,
    ) -> Result<(), DbError> {
        if self.query_rows.is_empty() {
            let _ = tx
                .send(DbMessage::QueryComplete(
                    query_id,
                    Ok(QueryMeta {
                        affected_rows: None,
                        rows_returned: 0,
                        elapsed: std::time::Duration::ZERO,
                        truncated: false,
                    }),
                ))
                .await;
            return Ok(());
        }

        let mut first_page = true;
        let mut total: usize = 0;

        for chunk in self.query_rows.chunks(PAGE_SIZE) {
            let cols = if first_page {
                first_page = false;
                Some(self.query_columns.clone())
            } else {
                None
            };

            total = total.saturating_add(chunk.len());
            let _ = tx
                .send(DbMessage::QueryPage(
                    query_id,
                    Ok(QueryPage {
                        columns: cols,
                        rows: chunk.to_vec(),
                    }),
                ))
                .await;
        }

        let _ = tx
            .send(DbMessage::QueryComplete(
                query_id,
                Ok(QueryMeta {
                    affected_rows: None,
                    rows_returned: total as u64,
                    elapsed: std::time::Duration::ZERO,
                    truncated: total > MAX_ROWS,
                }),
            ))
            .await;

        Ok(())
    }

    async fn cancel(&self, _query_id: QueryId) -> Result<(), DbError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CellValue;
    use crate::event::QueryId;
    use std::sync::Arc;

    #[tokio::test]
    async fn mock_ping_ok() -> Result<(), DbError> {
        let backend = MockBackend::default();
        backend.ping().await?;
        Ok(())
    }

    #[tokio::test]
    async fn mock_list_schemas() -> Result<(), DbError> {
        let backend = MockBackend {
            schemas: vec![
                SchemaInfo {
                    name: "test".into(),
                },
                SchemaInfo {
                    name: "mysql".into(),
                },
            ],
            ..Default::default()
        };

        let schemas = backend.list_schemas().await?;
        assert_eq!(schemas.len(), 2);
        assert_eq!(schemas[0].name, "test");
        assert_eq!(schemas[1].name, "mysql");
        Ok(())
    }

    #[tokio::test]
    async fn mock_query_stream_empty() -> Result<(), DbError> {
        let backend = MockBackend::default();
        let (tx, mut rx) = mpsc::channel(32);

        backend.query_stream("SELECT 1", QueryId::new(), tx).await?;

        // No QueryPage — should get QueryComplete immediately.
        let msg = rx
            .recv()
            .await
            .ok_or_else(|| DbError::Other("channel closed".into()))?;
        match msg {
            DbMessage::QueryComplete(_, Ok(meta)) => {
                assert_eq!(meta.rows_returned, 0);
                assert!(!meta.truncated);
            }
            _ => return Err(DbError::Other("expected QueryComplete".into())),
        }
        Ok(())
    }

    #[tokio::test]
    async fn mock_query_stream_paginated() -> Result<(), DbError> {
        let backend = MockBackend {
            query_columns: vec![ColumnMeta {
                name: "id".into(),
                type_name: "INT".into(),
                kind: crate::db::CellKind::Int,
            }],
            query_rows: (0..250)
                .map(|i| vec![CellValue::Text(i.to_string())])
                .collect(),
            ..Default::default()
        };

        let (tx, mut rx) = mpsc::channel(32);
        backend
            .query_stream("SELECT * FROM t", QueryId::new(), tx)
            .await?;

        // First page (100 rows + columns)
        let msg1 = rx
            .recv()
            .await
            .ok_or_else(|| DbError::Other("channel closed".into()))?;
        match msg1 {
            DbMessage::QueryPage(_, Ok(page)) => {
                assert!(page.columns.is_some(), "first page must have columns");
                assert_eq!(page.rows.len(), PAGE_SIZE);
            }
            _ => return Err(DbError::Other("expected first QueryPage".into())),
        }

        // Second page (100 rows, no columns)
        let msg2 = rx
            .recv()
            .await
            .ok_or_else(|| DbError::Other("channel closed".into()))?;
        match msg2 {
            DbMessage::QueryPage(_, Ok(page)) => {
                assert!(page.columns.is_none(), "second page must not have columns");
                assert_eq!(page.rows.len(), PAGE_SIZE);
            }
            _ => return Err(DbError::Other("expected second QueryPage".into())),
        }

        // Third page (50 rows, no columns)
        let msg3 = rx
            .recv()
            .await
            .ok_or_else(|| DbError::Other("channel closed".into()))?;
        match msg3 {
            DbMessage::QueryPage(_, Ok(page)) => {
                assert_eq!(page.rows.len(), 50);
            }
            _ => return Err(DbError::Other("expected third QueryPage".into())),
        }

        // QueryComplete
        let msg4 = rx
            .recv()
            .await
            .ok_or_else(|| DbError::Other("channel closed".into()))?;
        match msg4 {
            DbMessage::QueryComplete(_, Ok(meta)) => {
                assert_eq!(meta.rows_returned, 250);
                assert!(!meta.truncated);
            }
            _ => return Err(DbError::Other("expected QueryComplete".into())),
        }
        Ok(())
    }

    #[tokio::test]
    async fn mock_execute() -> Result<(), DbError> {
        let backend = MockBackend {
            exec_result: ExecResult { rows_affected: 42 },
            ..Default::default()
        };

        let result = backend.execute("INSERT INTO t VALUES (1)").await?;
        assert_eq!(result.rows_affected, 42);
        Ok(())
    }

    /// Verify `Arc<dyn Database>` is `Send + Sync` and spawnable on Tokio
    /// (architecture risk ②: async-trait + dyn Send).
    #[tokio::test]
    async fn mock_database_spawnable() -> Result<(), DbError> {
        let backend: Arc<dyn Database> = Arc::new(MockBackend::default());
        let handle = tokio::spawn(async move { backend.ping().await });
        handle
            .await
            .map_err(|e| DbError::Other(format!("join error: {e}")))??;
        Ok(())
    }
}
