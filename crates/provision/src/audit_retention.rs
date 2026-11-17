//! The record history retention surface of the audit retention family.
//!
//! A relation keeps its log entries for n whole days when its
//! `wamn_record_history_log` trigger argument is `P<n>D`. `pg_trigger` is the one
//! retention source. The retention verb reads it to find the history tables to
//! prune. apply-package reads it to grant `wamn_platform` exactly the
//! privileges that the verb needs, and the provisioning verifier reads it to
//! test those grants.
//! The same job prunes all write-log claims after 30 days. The write log provides idempotency protection.

/// The shared transaction advisory lock of the retention verb and of the
/// apply-package trigger reconciliation.
///
/// PostgreSQL keys an advisory lock by database, so one name serves every
/// database. A retention change cannot land between the read of the verb and
/// its delete.
pub const AUDIT_RETENTION_LOCK_SQL: &str =
    "SELECT pg_advisory_xact_lock(hashtextextended('wamn.audit-retention', 0))";

/// The history table columns that pruning reads and returns.
pub const AUDIT_RETENTION_READ_COLUMNS: [&str; 4] =
    ["row_key", "position", "changed_at", "transaction_id"];

/// Read only the age and completion state when pruning idempotency records.
pub const WRITE_LOG_RETENTION_READ_COLUMNS: [&str; 2] = ["created_at", "result"];

/// Expire every claim after 30 days, far beyond the 30-second call boundary.
pub const WRITE_LOG_RETENTION_SQL: &str = "DELETE FROM platform.write_log \
    WHERE created_at < transaction_timestamp() - interval '30 days'";

/// The optional write log target in databases that install the application schema.
pub const WRITE_LOG_RETENTION_TARGET_SQL: &str = "SELECT 'platform'::text AS schema_name, \
    'write_log'::text AS history_name WHERE to_regclass('platform.write_log') IS NOT NULL";

/// Each relation of the current database whose log keeps its entries for n days.
///
/// The query reads every `wamn_record_history_log` trigger that runs a
/// `wamn_history` function with the one argument `P<n>D` on a relation that
/// has its history table. A platform relation runs the shared
/// `wamn_history.log_row_change`, and a package relation runs its generated
/// log function (`wamn-c8a5v.2`). It returns `schema_name`, `relation_name`,
/// `history_name`, and `days`, the text of n, in byte order of schema and
/// relation.
/// An `unlimited` argument returns no row. The query reads catalogs only, so it
/// runs in a database that has no `wamn_history` schema.
pub const AUDIT_RETENTION_TARGETS_SQL: &str = r#"
SELECT log.schema_name, log.relation_name, log.history_name, log.days
  FROM (
        SELECT namespace.nspname::text AS schema_name,
               relation.relname::text AS relation_name,
               relation.relnamespace,
               relation.relname::text || '_history' AS history_name,
               substring(pg_catalog.encode(trigger.tgargs, 'escape')
                         FROM '^P([1-9][0-9]*)D\\000$') AS days
          FROM pg_catalog.pg_trigger AS trigger
          JOIN pg_catalog.pg_class AS relation ON relation.oid = trigger.tgrelid
          JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace
          JOIN pg_catalog.pg_proc AS routine ON routine.oid = trigger.tgfoid
          JOIN pg_catalog.pg_namespace AS routine_namespace
            ON routine_namespace.oid = routine.pronamespace
         WHERE NOT trigger.tgisinternal
           AND trigger.tgname = 'wamn_record_history_log'
           AND trigger.tgnargs = 1
           AND routine_namespace.nspname = 'wamn_history'
       ) AS log
 WHERE log.days IS NOT NULL
   AND EXISTS (SELECT FROM pg_catalog.pg_class AS history
                WHERE history.relnamespace = log.relnamespace
                  AND history.relname = log.history_name
                  AND history.relkind = 'r')
 ORDER BY log.schema_name COLLATE "C", log.relation_name COLLATE "C"
"#;
