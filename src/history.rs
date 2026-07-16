//! Query history persistence — per-database JSON files.
//!
//! History is stored at `~/.config/dbtui/history/{database}.json`. Each file
//! is a JSON array of [`HistoryEntry`] objects. Only successful queries are
//! saved. Entries are deduplicated and sorted by execution frequency
//! (most-used first).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::Config;

/// Maximum number of unique SQL entries to retain per database.
const MAX_HISTORY: usize = 500;

/// A single deduplicated query history entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// The SQL text that was executed.
    pub sql: String,
    /// Number of times this query was executed successfully.
    pub count: u32,
    /// ISO 8601 timestamp of the most recent execution.
    pub last_used: String,
}

/// Persistent query history for a single database.
#[derive(Debug, Clone, Default)]
pub struct QueryHistory {
    /// Unique entries sorted by count descending.
    entries: Vec<HistoryEntry>,
    /// File path for persistence.
    path: Option<PathBuf>,
}

impl QueryHistory {
    /// Create a new empty history store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Load history for a specific database name.
    /// File path: `~/.config/dbtui/history/{database}.json`.
    #[must_use]
    pub fn for_database(database: &str) -> Self {
        let path = match history_dir() {
            Some(dir) => dir.join(format!("{database}.json")),
            None => return Self::new(),
        };

        let mut entries: Vec<HistoryEntry> = match std::fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
            Err(_) => Vec::new(),
        };

        // Ensure sorted by count desc on load.
        entries.sort_by(|a, b| b.count.cmp(&a.count));

        Self {
            entries,
            path: Some(path),
        }
    }

    /// Add a successful query to history.
    ///
    /// If the SQL already exists, increment its count and update `last_used`.
    /// Otherwise insert a new entry. Entries are kept sorted by count
    /// descending (ties broken by most recent first).
    pub fn add(&mut self, sql: &str) {
        let trimmed = sql.trim();
        if trimmed.is_empty() {
            return;
        }

        let now = chrono::Utc::now().to_rfc3339();

        if let Some(pos) = self.entries.iter().position(|e| e.sql == trimmed) {
            // Existing entry — increment count, bump timestamp.
            self.entries[pos].count = self.entries[pos].count.saturating_add(1);
            self.entries[pos].last_used = now;
        } else {
            // New entry.
            self.entries.push(HistoryEntry {
                sql: trimmed.to_string(),
                count: 1,
                last_used: now,
            });
            // Trim to max size (remove the least-used entry).
            if self.entries.len() > MAX_HISTORY {
                // After sorting the lowest-count entry is last.
                self.entries.sort_by(|a, b| b.count.cmp(&a.count));
                self.entries.truncate(MAX_HISTORY);
            }
        }

        // Re-sort by count desc, then last_used desc.
        self.sort_entries();
        self.save();
    }

    /// Sort entries: count descending, then last_used descending.
    fn sort_entries(&mut self) {
        self.entries.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then_with(|| b.last_used.cmp(&a.last_used))
        });
    }

    /// Save history to the persistence file (best-effort).
    fn save(&self) {
        let path = match &self.path {
            Some(p) => p,
            None => return,
        };

        if let Ok(json) = serde_json::to_string_pretty(&self.entries) {
            let _ = std::fs::write(path, json);
        }
    }

    /// Get all history entries (sorted by count descending).
    #[must_use]
    pub fn entries(&self) -> &[HistoryEntry] {
        &self.entries
    }

    /// Filter entries by a case-insensitive substring search.
    /// Returns references to matching entries, preserving sort order.
    #[must_use]
    pub fn search<'a>(&'a self, query: &str) -> Vec<&'a HistoryEntry> {
        if query.is_empty() {
            return self.entries.iter().collect();
        }
        let q = query.to_ascii_lowercase();
        self.entries
            .iter()
            .filter(|e| e.sql.to_ascii_lowercase().contains(&q))
            .collect()
    }

    /// Get SQL text of all entries (for editor history sync).
    #[must_use]
    pub fn sql_list(&self) -> Vec<String> {
        self.entries.iter().map(|e| e.sql.clone()).collect()
    }
}

/// Get the history directory, creating it if needed.
fn history_dir() -> Option<PathBuf> {
    let dir = Config::config_dir()?.join("history");
    let _ = std::fs::create_dir_all(&dir);
    Some(dir)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_dedup_and_count() {
        let mut h = QueryHistory::new();
        h.add("SELECT 1");
        h.add("SELECT 2");
        h.add("SELECT 1"); // same SQL → count incremented
        h.add("SELECT 1"); // again
        assert_eq!(h.entries().len(), 2);
        // SELECT 1 has count 3 → should be first.
        assert_eq!(h.entries()[0].sql, "SELECT 1");
        assert_eq!(h.entries()[0].count, 3);
        assert_eq!(h.entries()[1].sql, "SELECT 2");
        assert_eq!(h.entries()[1].count, 1);
    }

    #[test]
    fn history_skip_empty() {
        let mut h = QueryHistory::new();
        h.add("");
        h.add("   ");
        assert!(h.entries().is_empty());
    }

    #[test]
    fn history_sorted_by_count() {
        let mut h = QueryHistory::new();
        h.add("SELECT 1"); // count=1
        h.add("SELECT 2"); // count=1
        h.add("SELECT 2"); // count=2
        h.add("SELECT 3"); // count=1
        h.add("SELECT 2"); // count=3
        h.add("SELECT 3"); // count=2
        // Order should be: SELECT 2 (3), SELECT 3 (2), SELECT 1 (1).
        assert_eq!(h.entries()[0].sql, "SELECT 2");
        assert_eq!(h.entries()[0].count, 3);
        assert_eq!(h.entries()[1].sql, "SELECT 3");
        assert_eq!(h.entries()[1].count, 2);
        assert_eq!(h.entries()[2].sql, "SELECT 1");
        assert_eq!(h.entries()[2].count, 1);
    }

    #[test]
    fn history_search_case_insensitive() {
        let mut h = QueryHistory::new();
        h.add("SELECT * FROM users");
        h.add("SELECT * FROM orders");
        h.add("SHOW TABLES");

        let results = h.search("from");
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|e| e.sql.contains("FROM")));

        let results = h.search("ORDER");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].sql, "SELECT * FROM orders");

        let results = h.search("");
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn history_max_size() {
        let mut h = QueryHistory::new();
        for i in 0..(MAX_HISTORY + 50) {
            h.add(&format!("SELECT {i}"));
        }
        assert_eq!(h.entries().len(), MAX_HISTORY);
    }
}
