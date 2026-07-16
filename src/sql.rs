//! SQL tokenizer for syntax highlighting and autocomplete suggestions.
//!
//! Provides a lightweight, fault-tolerant tokenizer that classifies SQL text
//! into colored segments. Designed for real-time highlighting of partial SQL
//! as the user types — never panics on incomplete input.

use std::collections::HashSet;
use std::sync::LazyLock;

// ---------------------------------------------------------------------------
// Token kinds
// ---------------------------------------------------------------------------

/// Classification of a SQL token for syntax highlighting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// SQL keyword (SELECT, FROM, WHERE, etc.)
    Keyword,
    /// SQL function (COUNT, SUM, etc.) — identifier immediately followed by `(`
    Function,
    /// Identifier (table/column/schema names)
    Identifier,
    /// String literal (`'...'` or `"..."`)
    String,
    /// Numeric literal (123, 45.67)
    Number,
    /// Operator (=, <=, !=, +, -, *, /, etc.)
    Operator,
    /// Punctuation ((, ), ,, ;, .)
    Punctuation,
    /// Comment (`-- ...`, `# ...`, or `/* ... */`)
    Comment,
    /// Whitespace (spaces, tabs, newlines)
    Whitespace,
}

/// A token with a reference to its source text.
#[derive(Debug, Clone)]
pub struct Token<'a> {
    /// The token text (borrowed from the source string).
    pub text: &'a str,
    /// The token kind.
    pub kind: TokenKind,
}

// ---------------------------------------------------------------------------
// Keyword sets
// ---------------------------------------------------------------------------

/// SQL keywords for syntax highlighting and autocomplete.
static KEYWORDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    HashSet::from([
        // Query clauses
        "SELECT", "FROM", "WHERE", "GROUP", "BY", "ORDER", "HAVING",
        "LIMIT", "OFFSET", "DISTINCT", "AS", "INTO", "VALUES",
        // DML
        "INSERT", "UPDATE", "DELETE", "SET",
        // DDL
        "CREATE", "DROP", "ALTER", "TABLE", "DATABASE", "SCHEMA",
        "INDEX", "VIEW", "TRIGGER", "PROCEDURE",
        // Joins
        "JOIN", "INNER", "LEFT", "RIGHT", "OUTER", "FULL", "CROSS",
        "ON", "USING", "NATURAL", "STRAIGHT_JOIN",
        // Conditions
        "AND", "OR", "NOT", "IN", "EXISTS", "BETWEEN", "LIKE",
        "IS", "NULL", "TRUE", "FALSE", "REGEXP",
        // Set operations
        "UNION", "ALL", "INTERSECT", "EXCEPT",
        // Case
        "CASE", "WHEN", "THEN", "ELSE", "END", "IF",
        // Modifiers
        "ASC", "DESC", "WITH",
        // Introspection
        "SHOW", "DESCRIBE", "EXPLAIN", "USE",
        // Table options
        "ENGINE", "CHARSET", "CHARACTER", "COLLATE", "DEFAULT",
        "AUTO_INCREMENT", "PRIMARY", "KEY", "FOREIGN", "REFERENCES",
        "CONSTRAINT", "UNIQUE", "CHECK",
        // Transaction
        "BEGIN", "COMMIT", "ROLLBACK", "START", "TRANSACTION",
        // Types
        "INT", "INTEGER", "BIGINT", "SMALLINT", "TINYINT", "MEDIUMINT",
        "VARCHAR", "CHAR", "TEXT", "MEDIUMTEXT", "LONGTEXT", "BLOB",
        "DECIMAL", "FLOAT", "DOUBLE", "NUMERIC", "BIT",
        "DATE", "DATETIME", "TIMESTAMP", "TIME", "YEAR",
        "BOOLEAN", "BOOL", "ENUM", "JSON", "BINARY", "VARBINARY",
    ])
});

/// SQL functions for autocomplete suggestions (uppercased).
static FUNCTIONS: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    vec![
        "COUNT", "SUM", "AVG", "MIN", "MAX",
        "CONCAT", "CONCAT_WS", "SUBSTRING", "SUBSTR", "TRIM",
        "LTRIM", "RTRIM", "LENGTH", "CHAR_LENGTH",
        "UPPER", "LOWER", "REPLACE", "REVERSE",
        "LEFT", "RIGHT", "LPAD", "RPAD",
        "NOW", "CURDATE", "CURTIME", "CURRENT_DATE", "CURRENT_TIME",
        "CURRENT_TIMESTAMP", "UNIX_TIMESTAMP", "FROM_UNIXTIME",
        "DATE_FORMAT", "STR_TO_DATE", "DATE_ADD", "DATE_SUB", "DATEDIFF",
        "YEAR", "MONTH", "DAY", "HOUR", "MINUTE", "SECOND",
        "COALESCE", "IFNULL", "NULLIF", "ISNULL",
        "CAST", "CONVERT", "GREATEST", "LEAST",
        "ABS", "CEIL", "CEILING", "FLOOR", "ROUND", "TRUNCATE", "MOD", "POW", "RAND",
        "MD5", "SHA1", "SHA2", "UUID",
        "VERSION", "DATABASE", "USER", "CONNECTION_ID",
        "GROUP_CONCAT", "STDDEV", "VARIANCE", "BIT_AND", "BIT_OR",
    ]
});

/// Sorted keyword list for autocomplete (keywords + functions combined).
static AUTOCOMPLETE_KEYWORDS: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    let mut all: Vec<&str> = KEYWORDS.iter().copied().chain(FUNCTIONS.iter().copied()).collect();
    all.sort_unstable();
    all.dedup();
    all
});

/// Get the autocomplete keyword list (keywords + functions).
#[must_use]
pub fn autocomplete_keywords() -> &'static [&'static str] {
    &AUTOCOMPLETE_KEYWORDS
}

// ---------------------------------------------------------------------------
// Tokenizer
// ---------------------------------------------------------------------------

/// Check if a character can start an identifier.
#[inline]
fn is_ident_start(ch: char) -> bool {
    ch.is_ascii_alphabetic() || ch == '_'
}

/// Check if a character can continue an identifier.
#[inline]
fn is_ident_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

/// Tokenize SQL text into colored segments.
///
/// The tokenizer is fault-tolerant: incomplete strings, unclosed comments, or
/// unexpected characters are emitted as their nearest matching kind rather than
/// causing an error.
#[must_use]
pub fn tokenize(sql: &str) -> Vec<Token<'_>> {
    let bytes = sql.as_bytes();
    let len = bytes.len();
    let mut tokens = Vec::new();
    let mut i = 0usize;

    while i < len {
        let start = i;

        // Peek next char safely.
        let next_char = || -> Option<char> {
            sql[i..].chars().next()
        };

        let ch = match next_char() {
            Some(c) => c,
            None => break,
        };

        // Whitespace
        if ch.is_whitespace() {
            i += ch.len_utf8();
            while let Some(c) = sql[i..].chars().next() {
                if c.is_whitespace() {
                    i += c.len_utf8();
                } else {
                    break;
                }
            }
            tokens.push(Token { text: &sql[start..i], kind: TokenKind::Whitespace });
            continue;
        }

        // Line comment: -- ... or # ...
        if (ch == '-' && sql.get(i..i + 2) == Some("--")) || ch == '#' {
            i += if ch == '#' { 1 } else { 2 };
            while i < len && bytes[i] != b'\n' {
                i += 1;
            }
            tokens.push(Token { text: &sql[start..i], kind: TokenKind::Comment });
            continue;
        }

        // Block comment: /* ... */
        if ch == '/' && sql.get(i..i + 2) == Some("/*") {
            i += 2;
            while i < len && sql.get(i..i + 2) != Some("*/") {
                i += 1;
            }
            if sql.get(i..i + 2) == Some("*/") {
                i += 2;
            } else {
                i = len;
            }
            tokens.push(Token { text: &sql[start..i], kind: TokenKind::Comment });
            continue;
        }

        // Single-quoted string: '...'
        if ch == '\'' {
            i += 1;
            while i < len {
                if bytes[i] == b'\\' {
                    i += 2; // skip escaped char
                } else if bytes[i] == b'\'' {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            tokens.push(Token { text: &sql[start..i], kind: TokenKind::String });
            continue;
        }

        // Double-quoted string: "..."
        if ch == '"' {
            i += 1;
            while i < len {
                if bytes[i] == b'\\' {
                    i += 2;
                } else if bytes[i] == b'"' {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            tokens.push(Token { text: &sql[start..i], kind: TokenKind::String });
            continue;
        }

        // Backtick-quoted identifier: `...`
        if ch == '`' {
            i += 1;
            while i < len && bytes[i] != b'`' {
                i += 1;
            }
            if i < len {
                i += 1; // skip closing `
            }
            tokens.push(Token { text: &sql[start..i], kind: TokenKind::Identifier });
            continue;
        }

        // Number: [0-9]+ (.[0-9]+)?
        if ch.is_ascii_digit() || (ch == '.' && sql[i + 1..].starts_with(|c: char| c.is_ascii_digit())) {
            while i < len && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i < len && bytes[i] == b'.' {
                i += 1;
                while i < len && bytes[i].is_ascii_digit() {
                    i += 1;
                }
            }
            tokens.push(Token { text: &sql[start..i], kind: TokenKind::Number });
            continue;
        }

        // Identifier or keyword
        if is_ident_start(ch) {
            i += ch.len_utf8();
            while let Some(c) = sql.get(i..).and_then(|s| s.chars().next()) {
                if is_ident_char(c) {
                    i += c.len_utf8();
                } else {
                    break;
                }
            }
            let word = &sql[start..i];
            let upper = word.to_ascii_uppercase();
            let kind = if KEYWORDS.contains(upper.as_str()) {
                TokenKind::Keyword
            } else {
                TokenKind::Identifier
            };
            tokens.push(Token { text: word, kind });
            continue;
        }

        // Multi-char operators: <=, >=, !=, <>, :=
        if let Some(two) = sql.get(i..i + 2) {
            if matches!(two, "<=" | ">=" | "!=" | "<>" | ":=") {
                i += 2;
                tokens.push(Token { text: &sql[start..i], kind: TokenKind::Operator });
                continue;
            }
        }

        // Single-char operators
        if matches!(ch, '=' | '<' | '>' | '+' | '-' | '*' | '/' | '%') {
            i += ch.len_utf8();
            tokens.push(Token { text: &sql[start..i], kind: TokenKind::Operator });
            continue;
        }

        // Punctuation
        if matches!(ch, '(' | ')' | ',' | ';' | '.') {
            i += ch.len_utf8();
            tokens.push(Token { text: &sql[start..i], kind: TokenKind::Punctuation });
            continue;
        }

        // Unknown character — emit as punctuation to avoid losing it
        i += ch.len_utf8();
        tokens.push(Token { text: &sql[start..i], kind: TokenKind::Punctuation });
    }

    // Post-process: mark identifiers immediately followed by `(` as functions.
    postprocess_functions(&mut tokens);

    tokens
}

/// Mark identifier tokens immediately followed by `(` as functions.
fn postprocess_functions(tokens: &mut Vec<Token<'_>>) {
    // Find indices of identifiers followed by `(` (skipping whitespace).
    let len = tokens.len();
    for i in 0..len {
        if tokens[i].kind == TokenKind::Identifier {
            // Look ahead for `(` skipping whitespace.
            let mut j = i + 1;
            while j < len && tokens[j].kind == TokenKind::Whitespace {
                j += 1;
            }
            if j < len && tokens[j].kind == TokenKind::Punctuation && tokens[j].text == "(" {
                tokens[i].kind = TokenKind::Function;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Word prefix extraction (for autocomplete)
// ---------------------------------------------------------------------------

/// Extract the word prefix at the given cursor position (byte offset).
///
/// Returns the prefix text and its starting byte offset. A "word" is a
/// contiguous run of identifier characters (`[a-zA-Z0-9_]`).
#[must_use]
pub fn word_prefix_at(sql: &str, cursor_byte: usize) -> (String, usize) {
    let safe_cursor = cursor_byte.min(sql.len());
    let before = &sql[..safe_cursor];

    // Walk backward from cursor to find the start of the current word.
    let mut start = before.len();
    for (byte_idx, ch) in before.char_indices().rev() {
        if is_ident_char(ch) {
            start = byte_idx;
        } else {
            break;
        }
    }

    let prefix = &before[start..];
    (prefix.to_string(), start)
}

/// Detect the table name in the FROM clause of a SQL query.
///
/// Uses the tokenizer to find the FROM keyword, then returns the next
/// identifier token (skipping whitespace). Handles `FROM table` and
/// `FROM schema.table` (returns `table` only).
#[must_use]
pub fn detect_from_table(sql: &str) -> Option<String> {
    let tokens = tokenize(sql);
    let len = tokens.len();
    let mut i = 0usize;

    while i < len {
        // Find FROM keyword.
        if tokens[i].kind == TokenKind::Keyword
            && tokens[i].text.to_ascii_uppercase() == "FROM"
        {
            // Skip whitespace after FROM.
            let mut j = i + 1;
            while j < len && tokens[j].kind == TokenKind::Whitespace {
                j += 1;
            }
            // Next non-whitespace token should be the table name.
            if j < len {
                let table_token = &tokens[j];
                if table_token.kind == TokenKind::Identifier {
                    let name = table_token.text.trim_matches('`');
                    // Check for schema.table — return the table part.
                    // If next non-whitespace is '.', return the identifier after it.
                    let mut k = j + 1;
                    while k < len && tokens[k].kind == TokenKind::Whitespace {
                        k += 1;
                    }
                    if k < len && tokens[k].kind == TokenKind::Punctuation && tokens[k].text == "."
                    {
                        let mut m = k + 1;
                        while m < len && tokens[m].kind == TokenKind::Whitespace {
                            m += 1;
                        }
                        if m < len && tokens[m].kind == TokenKind::Identifier {
                            return Some(tokens[m].text.trim_matches('`').to_string());
                        }
                    }
                    return Some(name.to_string());
                }
            }
            return None;
        }
        i += 1;
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_select_query() {
        let tokens = tokenize("SELECT * FROM users");
        assert_eq!(tokens[0].kind, TokenKind::Keyword); // SELECT
        assert_eq!(tokens[1].kind, TokenKind::Whitespace);
        assert_eq!(tokens[2].kind, TokenKind::Operator); // *
        assert_eq!(tokens[3].kind, TokenKind::Whitespace);
        assert_eq!(tokens[4].kind, TokenKind::Keyword); // FROM
        assert_eq!(tokens[5].kind, TokenKind::Whitespace);
        assert_eq!(tokens[6].kind, TokenKind::Identifier); // users
    }

    #[test]
    fn tokenize_string_literal() {
        let tokens = tokenize("WHERE name = 'hello world'");
        let kinds: Vec<_> = tokens.iter().map(|t| t.kind).collect();
        assert!(kinds.contains(&TokenKind::String));
        let str_tok = tokens.iter().find(|t| t.kind == TokenKind::String).unwrap();
        assert_eq!(str_tok.text, "'hello world'");
    }

    #[test]
    fn tokenize_function_call() {
        let tokens = tokenize("COUNT(*)");
        assert_eq!(tokens[0].kind, TokenKind::Function); // COUNT
        assert_eq!(tokens[0].text, "COUNT");
    }

    #[test]
    fn tokenize_function_case_insensitive() {
        let tokens = tokenize("count(*)");
        assert_eq!(tokens[0].kind, TokenKind::Function);
    }

    #[test]
    fn tokenize_line_comment() {
        let tokens = tokenize("SELECT 1 -- this is a comment");
        let comment = tokens.iter().find(|t| t.kind == TokenKind::Comment);
        assert!(comment.is_some());
        assert!(comment.unwrap().text.contains("this is a comment"));
    }

    #[test]
    fn tokenize_block_comment() {
        let tokens = tokenize("SELECT /* inline */ 1");
        let comment = tokens.iter().find(|t| t.kind == TokenKind::Comment);
        assert!(comment.is_some());
        assert_eq!(comment.unwrap().text, "/* inline */");
    }

    #[test]
    fn tokenize_number() {
        let tokens = tokenize("WHERE price > 29.99");
        let num = tokens.iter().find(|t| t.kind == TokenKind::Number);
        assert!(num.is_some());
        assert_eq!(num.unwrap().text, "29.99");
    }

    #[test]
    fn tokenize_operators() {
        let tokens = tokenize("a <= b >= c != d <> e");
        let ops: Vec<_> = tokens.iter().filter(|t| t.kind == TokenKind::Operator).collect();
        assert_eq!(ops.len(), 4); // <=, >=, !=, <>
    }

    #[test]
    fn tokenize_backtick_identifier() {
        let tokens = tokenize("SELECT `order` FROM `table`");
        let idents: Vec<_> = tokens
            .iter()
            .filter(|t| t.kind == TokenKind::Identifier)
            .collect();
        assert_eq!(idents.len(), 2);
        assert_eq!(idents[0].text, "`order`");
    }

    #[test]
    fn tokenize_incomplete_string() {
        let tokens = tokenize("WHERE name = 'unfinished");
        // Should not panic — unclosed string emitted as String token.
        let str_tok = tokens.iter().find(|t| t.kind == TokenKind::String);
        assert!(str_tok.is_some());
    }

    #[test]
    fn word_prefix_basic() {
        let sql = "SELECT * FROM or";
        let (prefix, start) = word_prefix_at(sql, sql.len());
        assert_eq!(prefix, "or");
        assert_eq!(&sql[start..], "or");
    }

    #[test]
    fn word_prefix_empty_at_space() {
        let sql = "SELECT * FROM ";
        let (prefix, _) = word_prefix_at(sql, sql.len());
        assert_eq!(prefix, "");
    }

    #[test]
    fn word_prefix_in_middle() {
        let sql = "SELECT col";
        let cursor = 9; // byte offset after "co" (S=0,E=1,L=2,E=3,C=4,T=5,space=6,c=7,o=8)
        let (prefix, start) = word_prefix_at(sql, cursor);
        assert_eq!(prefix, "co");
        assert_eq!(&sql[start..cursor], "co");
    }

    #[test]
    fn autocomplete_keywords_sorted() {
        let keywords = autocomplete_keywords();
        assert!(keywords.contains(&"SELECT"));
        assert!(keywords.contains(&"COUNT"));
        // Verify sorted
        let mut sorted = keywords.to_vec();
        sorted.sort_unstable();
        assert_eq!(keywords.to_vec(), sorted);
    }
}
