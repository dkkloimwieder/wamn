//! Postgres literal / identifier quoting — the single source of truth shared by
//! this crate's DDL emission and the RLS policy builder (3.5, `wamn-rls`), so
//! both quote identically.

/// Quote a SQL identifier (double-quoted, embedded `"` doubled).
pub fn quote_ident(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

/// Quote a SQL string literal (single-quoted, embedded `'` doubled).
pub fn quote_literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}
