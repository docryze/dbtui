//! Error types for dbtui, organized by layer (architecture §7.1).

use thiserror::Error;

/// Database-layer error.
#[derive(Debug, Error)]
pub enum DbError {
    /// Wraps a sqlx error.
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    /// Generic database error with a human-readable message.
    #[error("{0}")]
    Other(String),
}

/// Configuration-layer error.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Generic configuration error with a human-readable message.
    #[error("{0}")]
    Other(String),
}

/// Application-wide error aggregating all subsystem errors.
#[derive(Debug, Error)]
pub enum Error {
    /// Database error.
    #[error(transparent)]
    Db(#[from] DbError),
    /// I/O error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// Configuration error.
    #[error(transparent)]
    Config(#[from] ConfigError),
}
