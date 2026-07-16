//! Connection configuration: types, TOML serialization, and file I/O.
//!
//! Config file location: `dirs::config_dir()/dbtui/connections.toml`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::ConfigError;
use crate::event::ConnectionId;

/// Supported database drivers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Driver {
    /// `MySQL` driver.
    Mysql,
    /// `PostgreSQL` driver (future).
    Postgres,
    /// `SQLite` driver (future).
    Sqlite,
}

/// TLS connection mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TlsMode {
    /// No TLS.
    Disabled,
    /// TLS if supported, plain otherwise.
    #[default]
    Preferred,
    /// Require TLS.
    Required,
    /// Require TLS and verify the server certificate against `ssl_ca`.
    VerifyCa,
    /// Require TLS, verify CA, and verify the server hostname matches the cert.
    VerifyIdentity,
}

/// A password wrapper that does not expose its contents via [`Debug`].
#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretString(String);

impl SecretString {
    /// Create a new secret from a string.
    pub fn new(value: String) -> Self {
        Self(value)
    }

    /// Reveal the secret contents.
    pub fn reveal(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for SecretString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretString(***)")
    }
}

/// A single connection definition as stored in the config file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionConfig {
    /// Stable identifier (generated at load time, not serialized).
    #[serde(skip)]
    pub id: ConnectionId,
    /// Human-readable display name.
    pub name: String,
    /// Database driver.
    pub driver: Driver,
    /// Hostname or IP address.
    pub host: String,
    /// Port number.
    pub port: u16,
    /// Username.
    pub user: String,
    /// Password (if any).
    #[serde(default)]
    pub password: Option<SecretString>,
    /// Default database/schema.
    #[serde(default)]
    pub database: Option<String>,
    /// TLS mode.
    #[serde(default)]
    pub tls: TlsMode,
    /// Path to a PEM file containing trusted CA certificate(s) for verifying
    /// the server. Required when `tls = "verify_ca"` or `"verify_identity"`.
    #[serde(default)]
    pub ssl_ca: Option<String>,
    /// Path to the client SSL certificate (for mutual TLS).
    #[serde(default)]
    pub ssl_client_cert: Option<String>,
    /// Path to the client SSL private key (for mutual TLS).
    #[serde(default)]
    pub ssl_client_key: Option<String>,
}

/// The root configuration structure, serialized as `connections.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// All configured connections.
    #[serde(default)]
    pub connections: Vec<ConnectionConfig>,
}

impl Config {
    /// Return the config directory path (`~/.config/dbtui/`).
    ///
    /// Always uses `~/.config/dbtui/` regardless of platform (does not
    /// follow `dirs::config_dir()` platform-specific paths).
    pub fn config_dir() -> Option<PathBuf> {
        dirs::home_dir().map(|h| h.join(".config").join("dbtui"))
    }

    /// Return the full path to `connections.toml`.
    pub fn config_path() -> Option<PathBuf> {
        Self::config_dir().map(|d| d.join("connections.toml"))
    }

    /// Load config from the default path. Returns an empty [`Config`] if the
    /// file does not exist.
    ///
    /// # Errors
    /// Returns [`ConfigError`] if the file exists but cannot be read or parsed.
    pub fn load() -> Result<Self, ConfigError> {
        let path = Self::config_path().ok_or_else(|| {
            ConfigError::Other("cannot determine platform config directory".into())
        })?;

        if !path.exists() {
            return Ok(Self::default());
        }

        let content = std::fs::read_to_string(&path)
            .map_err(|e| ConfigError::Other(format!("failed to read {}: {e}", path.display())))?;

        let mut config: Self = toml::from_str(&content)
            .map_err(|e| ConfigError::Other(format!("failed to parse TOML: {e}")))?;

        // Assign fresh IDs to each connection.
        for conn in &mut config.connections {
            conn.id = ConnectionId::new();
        }

        Ok(config)
    }

    /// Write config to the default path, creating the directory if needed.
    ///
    /// # Errors
    /// Returns [`ConfigError`] on I/O or serialization failure.
    pub fn save(&self) -> Result<(), ConfigError> {
        let dir = Self::config_dir().ok_or_else(|| {
            ConfigError::Other("cannot determine platform config directory".into())
        })?;

        std::fs::create_dir_all(&dir)
            .map_err(|e| ConfigError::Other(format!("failed to create {}: {e}", dir.display())))?;

        let path = dir.join("connections.toml");
        let content = toml::to_string_pretty(self)
            .map_err(|e| ConfigError::Other(format!("failed to serialize: {e}")))?;

        std::fs::write(&path, content)
            .map_err(|e| ConfigError::Other(format!("failed to write {}: {e}", path.display())))?;

        Ok(())
    }
}
