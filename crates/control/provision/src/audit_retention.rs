//! The record history retention surface of the audit retention family.
//!
//! A relation keeps its log entries for n whole days when its
//! `record_history_log` trigger argument is `P<n>D`. `pg_trigger` is the one
//! retention source. The retention verb reads it to find the history tables to
//! prune. apply-package reads it to grant [`AUDIT_RETENTION_ROLE`] exactly the
//! privileges that the verb needs, and the provisioning verifier reads it to
//! check those grants.

use wamn_pg_core::{quote_ident, quote_literal};

use crate::workload_role::AUDIT_RETENTION_ROLE;

/// The shared transaction advisory lock of the retention verb and of the
/// apply-package trigger reconciliation.
///
/// PostgreSQL keys an advisory lock by database, so one name serves every
/// database. A retention change cannot land between the read of the verb and
/// its delete.
pub const AUDIT_RETENTION_LOCK_SQL: &str =
    "SELECT pg_advisory_xact_lock(hashtextextended('wamn.audit-retention', 0))";

/// The history table columns that the retention delete reads.
pub const AUDIT_RETENTION_READ_COLUMNS: [&str; 3] = ["row_key", "position", "changed_at"];

/// Each relation of the current database whose log keeps its entries for n days.
///
/// The query reads every `record_history_log` trigger that runs
/// `wamn_history.log_row_change` with the one argument `P<n>D` on a relation
/// that has its history table. It returns `schema_name`, `relation_name`,
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
           AND trigger.tgname = 'record_history_log'
           AND trigger.tgnargs = 1
           AND routine_namespace.nspname = 'wamn_history'
           AND routine.proname = 'log_row_change'
       ) AS log
 WHERE log.days IS NOT NULL
   AND EXISTS (SELECT FROM pg_catalog.pg_class AS history
                WHERE history.relnamespace = log.relnamespace
                  AND history.relname = log.history_name
                  AND history.relkind = 'r')
 ORDER BY log.schema_name COLLATE "C", log.relation_name COLLATE "C"
"#;

/// Converge the grants of [`AUDIT_RETENTION_ROLE`] in the current database.
///
/// The role holds schema `USAGE`, `DELETE`, and `SELECT` on
/// [`AUDIT_RETENTION_READ_COLUMNS`] of each history table that
/// [`AUDIT_RETENTION_TARGETS_SQL`] returns, and nothing else. The block revokes
/// every privilege that the role holds on a schema or a relation, and then
/// grants the exact set. A database without the role gets no change, like the
/// run plane repair of the `wamn_run_retention` grants. The caller runs the block in the transaction that
/// reconciles the log triggers, after it takes [`AUDIT_RETENTION_LOCK_SQL`].
pub fn reconcile_audit_retention_grants_sql() -> String {
    let read_columns = AUDIT_RETENTION_READ_COLUMNS
        .iter()
        .map(|column| quote_ident(column))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "DO $audit_retention_grants$ DECLARE \
           role_name constant text := {role}; \
           held record; \
           target record; \
         BEGIN \
           IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = role_name) THEN \
             RETURN; \
           END IF; \
           FOR held IN \
             SELECT 'SCHEMA' AS kind, namespace.nspname::text AS schema_name, \
                    NULL::text AS relation_name \
               FROM pg_catalog.pg_namespace AS namespace \
              CROSS JOIN LATERAL pg_catalog.aclexplode(namespace.nspacl) AS acl \
              JOIN pg_catalog.pg_roles AS grantee ON grantee.oid = acl.grantee \
              WHERE grantee.rolname = role_name \
             UNION \
             SELECT 'TABLE', namespace.nspname::text, relation.relname::text \
               FROM pg_catalog.pg_class AS relation \
               JOIN pg_catalog.pg_namespace AS namespace \
                 ON namespace.oid = relation.relnamespace \
              CROSS JOIN LATERAL pg_catalog.aclexplode(relation.relacl) AS acl \
              JOIN pg_catalog.pg_roles AS grantee ON grantee.oid = acl.grantee \
              WHERE grantee.rolname = role_name \
             UNION \
             SELECT 'TABLE', namespace.nspname::text, relation.relname::text \
               FROM pg_catalog.pg_attribute AS attribute \
               JOIN pg_catalog.pg_class AS relation ON relation.oid = attribute.attrelid \
               JOIN pg_catalog.pg_namespace AS namespace \
                 ON namespace.oid = relation.relnamespace \
              CROSS JOIN LATERAL pg_catalog.aclexplode(attribute.attacl) AS acl \
              JOIN pg_catalog.pg_roles AS grantee ON grantee.oid = acl.grantee \
              WHERE grantee.rolname = role_name \
           LOOP \
             IF held.kind = 'SCHEMA' THEN \
               EXECUTE format('REVOKE ALL ON SCHEMA %I FROM %I', held.schema_name, role_name); \
             ELSE \
               EXECUTE format('REVOKE ALL ON TABLE %I.%I FROM %I', \
                              held.schema_name, held.relation_name, role_name); \
             END IF; \
           END LOOP; \
           FOR target IN {targets} LOOP \
             EXECUTE format('GRANT USAGE ON SCHEMA %I TO %I', target.schema_name, role_name); \
             EXECUTE format('GRANT DELETE, SELECT ({read_columns}) ON TABLE %I.%I TO %I', \
                            target.schema_name, target.history_name, role_name); \
           END LOOP; \
         END $audit_retention_grants$;",
        role = quote_literal(AUDIT_RETENTION_ROLE),
        targets = AUDIT_RETENTION_TARGETS_SQL.trim(),
    )
}
