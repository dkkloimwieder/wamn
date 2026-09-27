//! The three statements of the write log, `app_system.write_log`
//! (`docs/plan/write-log.md` 4.2).
//!
//! Their text is fixed. The generator emits one copy of each per package and
//! lists all three in the contract of every claim operation, so every claim
//! operation carries the same three digests. The relation is schema-qualified
//! because `deploy/sql/app-schema.sql` installs it outside every package schema.

/// The package directory that holds the three statement files.
pub const WRITE_LOG_DIRECTORY: &str = "generated/sql/write_log";

/// Insert the claim of one key, or nothing when a committed row holds the key.
///
/// An uncommitted row of another transaction makes this insert wait until that
/// transaction ends. A commit makes it return no row, and a rollback lets it
/// insert.
pub const LOG_CLAIM_SQL: &str = "INSERT INTO app_system.write_log (operation, idempotency_key, request)\nVALUES ($1::text, $2::text, $3::bytea)\nON CONFLICT (operation, idempotency_key) DO NOTHING\nRETURNING idempotency_key;\n";

/// Read the request and the result that a committed claim of one key holds.
pub const LOG_READ_SQL: &str = "SELECT\n    request,\n    result\nFROM app_system.write_log\nWHERE operation = $1::text\n  AND idempotency_key = $2::text;\n";

/// Store the result of the work in the claim of this transaction.
pub const LOG_FINISH_SQL: &str = "UPDATE app_system.write_log\nSET result = $3::text\nWHERE operation = $1::text\n  AND idempotency_key = $2::text\n  AND result IS NULL\nRETURNING idempotency_key;\n";

/// The three statements as `(name, file stem, text)`, in contract order.
pub const WRITE_LOG_STATEMENTS: [(&str, &str, &str); 3] = [
    ("log_claim", "claim", LOG_CLAIM_SQL),
    ("log_read", "read", LOG_READ_SQL),
    ("log_finish", "finish", LOG_FINISH_SQL),
];
