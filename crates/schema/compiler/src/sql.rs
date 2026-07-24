//! Postgres literal / identifier quoting — the single source of truth shared by
//! this crate's DDL emission and the RLS policy builder (3.5, `wamn-schema-compiler`), so
//! both quote identically.

/// Quote PostgreSQL identifiers and literals with the canonical implementation.
pub use wamn_pg_core::{quote_ident, quote_literal};
