//! CDC role, publication, replication slot, and relation-map statements.

use super::{quote_ident, quote_literal};

// --- CDC capture provisioning (wamn-l5i9.9, D19 v3 §4) -----------------------
//
// The per-project-env CDC substrate: a REPLICATION role (the R8b credential
// tier above `wamn_app` query creds and the dispatch role), a publication over
// the app data schema, and a failover-enabled logical replication slot. The
// publication and the slot are DATABASE-BOUND — apply their SQL connected to
// the project-env database; the role is cluster-global. Pass the shared
// `cdc_object_name` (`wamn_cdc_<org>__<project>__<env>__<instance>`) as the role /
// publication / slot name.

/// Idempotently bootstrap a per-project-env **replication** role: `REPLICATION
/// LOGIN`, otherwise least-privilege (`NOSUPERUSER NOCREATEDB NOCREATEROLE
/// NOINHERIT NOBYPASSRLS` — `NOINHERIT` matches every other role this crate
/// mints; the role holds no memberships, so it is exactness, not a live
/// change). One role per project-env names the intended capture scope; it does
/// not limit a leaked credential to one registration. `REPLICATION` itself is
/// CLUSTER-WIDE in Postgres: any replication role can read any database's WAL
/// on that cluster. The accepted T3 boundary is compound: production HBA has
/// no physical-replication entry; PUBLIC CONNECT is revoked and this role gets
/// CONNECT only on its own database; ordinary DML remains denied; and each
/// reader is configured with its own slot and publication. The M1 gate shows
/// those production-shaped legs.
pub fn ensure_replication_role_sql(role: &str, password: &str) -> String {
    format!(
        "DO $$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = {role_lit}) THEN \
             CREATE ROLE {role} LOGIN REPLICATION PASSWORD {pw} \
               NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOBYPASSRLS; \
           END IF; \
         END $$;",
        role = quote_ident(role),
        role_lit = quote_literal(role),
        pw = quote_literal(password),
    )
}

/// `CREATE SCHEMA IF NOT EXISTS "<schema>"` — the eager guard that makes the
/// CDC SQL order-robust: `FOR TABLES IN SCHEMA` auto-includes tables created
/// later, so the publication may be created BEFORE catalog-publish fills the
/// schema and still capture everything from the start. Catalog-publish's own
/// `CREATE SCHEMA IF NOT EXISTS` then no-ops.
pub fn ensure_schema_sql(schema: &str) -> String {
    format!("CREATE SCHEMA IF NOT EXISTS {}", quote_ident(schema))
}

/// Idempotently create the CDC publication over the project-env's app **data**
/// schema: `CREATE PUBLICATION <pub> FOR TABLES IN SCHEMA <schema>`, guarded by
/// a `pg_publication` probe (Postgres has no `CREATE PUBLICATION IF NOT
/// EXISTS`). `FOR TABLES IN SCHEMA` auto-includes tables created in the schema
/// later — the D19 v3 replacement for the retired per-table trigger emission.
/// Re-pointing an existing publication at a different schema is a manual
/// `ALTER PUBLICATION … SET TABLES IN SCHEMA` (the guard never rewrites).
/// Run connected to the project-env database.
pub fn create_publication_sql(publication: &str, schema: &str) -> String {
    format!(
        "DO $$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_publication WHERE pubname = {pub_lit}) THEN \
             CREATE PUBLICATION {publication} FOR TABLES IN SCHEMA {schema}; \
           END IF; \
         END $$;",
        publication = quote_ident(publication),
        pub_lit = quote_literal(publication),
        schema = quote_ident(schema),
    )
}

/// Idempotently create the **failover-enabled** logical replication slot via
/// the SQL-function form: `pg_create_logical_replication_slot(<slot>,
/// 'pgoutput', temporary => false, twophase => false, failover => true)`
/// (PG17+ fifth argument) — a normal connection, no replication-protocol
/// syntax; the reader's `ensure_replication_slot` tolerates the existing slot
/// (same plugin/twophase/failover shape). Logical slots are DATABASE-BOUND:
/// run connected to the project-env database. WAL is pinned from creation
/// (capture starts at CDC-enable), bounded by the cluster's
/// `max_slot_wal_keep_size`.
pub fn create_failover_slot_sql(slot: &str) -> String {
    format!(
        "DO $$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_replication_slots WHERE slot_name = {slot_lit}) THEN \
             PERFORM pg_create_logical_replication_slot({slot_lit}, 'pgoutput', false, false, true); \
           END IF; \
         END $$;",
        slot_lit = quote_literal(slot),
    )
}

/// Grant the replication role its read surface: `CONNECT` on the project-env
/// database, `USAGE` on the app data schema, and `SELECT` on exactly the two
/// decode-time relation classification maps: [`ensure_entity_map_sql`] and
/// [`ensure_cdc_exclusion_map_sql`].
///
/// It deliberately does NOT grant `SELECT ON ALL TABLES IN SCHEMA`. Logical
/// *decoding* reads WAL, not tables, and the shipped reader performs no
/// initial snapshot and no backfill (`StreamingMode::Off` is the pgoutput
/// in-progress-transaction option, not a snapshot), so the blanket grant bought
/// nothing and cost a plain-SQL read of every tenant table to the same
/// credential — with none of the gates (the `REPLICATION` attribute, the
/// walsender protocol, slot ownership) that guard the replication path.
///
/// The `REVOKE … ON ALL TABLES` is the **retroactive** half: environments
/// provisioned before this narrowing already executed the blanket grant, and a
/// narrower grant does not undo it. It precedes the entity-map grant because it
/// would otherwise strip it. Idempotent; run connected to the project-env
/// database AFTER the schema and both classification maps exist.
pub fn grant_replication_access_sql(database: &str, role: &str, schema: &str) -> String {
    let role = quote_ident(role);
    format!(
        "GRANT CONNECT ON DATABASE {db} TO {role}; \
         GRANT USAGE ON SCHEMA {schema} TO {role}; \
         REVOKE SELECT ON ALL TABLES IN SCHEMA {schema} FROM {role}; \
         GRANT SELECT ON {schema}.wamn_entities TO {role}; \
         GRANT SELECT ON {schema}.wamn_cdc_exclusions TO {role};",
        db = quote_ident(database),
        schema = quote_ident(schema),
    )
}

/// The decode-time map of explicitly declared package mechanism relations that
/// must not enter the event plane. OID keys preserve the disposition for WAL
/// written before a rename or replacement; rows are therefore never deleted.
pub fn ensure_cdc_exclusion_map_sql(schema: &str) -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {schema}.wamn_cdc_exclusions ( \
           relation_oid oid PRIMARY KEY, \
           package_id text NOT NULL, \
           relation_id text NOT NULL, \
           table_name text NOT NULL)",
        schema = quote_ident(schema),
    )
}

/// Upsert one explicit CDC exclusion without permitting an OID to be rebound
/// to another package-local relation identity.
pub fn upsert_cdc_exclusion_map_sql(schema: &str) -> String {
    format!(
        "INSERT INTO {schema}.wamn_cdc_exclusions AS current \
               (relation_oid, package_id, relation_id, table_name) \
         SELECT c.oid, $1, $2, $3::text FROM pg_class c \
         JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = {schema_lit} AND c.relname = $3::text AND c.relkind = 'r' \
         ON CONFLICT (relation_oid) DO UPDATE \
           SET table_name = EXCLUDED.table_name \
         WHERE current.package_id = EXCLUDED.package_id \
           AND current.relation_id = EXCLUDED.relation_id",
        schema = quote_ident(schema),
        schema_lit = quote_literal(schema),
    )
}

/// The decode-time entity map (wamn-l5i9.11, D19 v3 §4): `relation_oid` →
/// stable package-local entity identity. **OID-keyed** so a reader resolving events is
/// timeless under catch-up — pg_class OIDs survive `ALTER TABLE RENAME`, so a
/// session decoding pre-rename backlog still resolves correctly, and a rename
/// only updates the informational `table_name`. Maintained by `apply-package`
/// in the same transaction as the DDL;
/// rows are upsert-only (a dropped entity's row keeps old-WAL decode
/// resolvable). No RLS: it holds no tenant data, and the CDC role's decode
/// stream sees every row of every table anyway.
pub fn ensure_entity_map_sql(schema: &str) -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {schema}.wamn_entities ( \
           relation_oid oid PRIMARY KEY, \
           package_id text NOT NULL, \
           entity_id text NOT NULL, \
           table_name text NOT NULL)",
        schema = quote_ident(schema),
    )
}

/// Upsert one entity's map row, resolving the table's CURRENT `pg_class` OID
/// server-side — run in the SAME transaction as the DDL that created/renamed
/// the table, so the row is atomic with the physical state. `$1` = package id,
/// `$2` = entity id, `$3` = physical table name. A table that does not exist
/// (a manifest model whose migration was never applied) upserts nothing — the
/// SELECT is empty. An existing OID may update its informational table name
/// only when its package/entity identity is unchanged; it is never rebound.
pub fn upsert_entity_map_sql(schema: &str) -> String {
    format!(
        "INSERT INTO {schema}.wamn_entities AS mapped \
               (relation_oid, package_id, entity_id, table_name) \
         SELECT c.oid, $1, $2, $3::text FROM pg_class c \
         JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = {schema_lit} AND c.relname = $3::text AND c.relkind = 'r' \
         ON CONFLICT (relation_oid) DO UPDATE \
           SET table_name = EXCLUDED.table_name \
         WHERE mapped.package_id = EXCLUDED.package_id \
           AND mapped.entity_id = EXCLUDED.entity_id",
        schema = quote_ident(schema),
        schema_lit = quote_literal(schema),
    )
}

/// `DROP PUBLICATION IF EXISTS "<publication>"` — teardown / gate only.
pub fn drop_publication_sql(publication: &str) -> String {
    format!("DROP PUBLICATION IF EXISTS {}", quote_ident(publication))
}

/// Drop the replication slot if it exists (teardown / gate only — dropping a
/// live slot severs the reader and releases the pinned WAL). Run connected to
/// the slot's database.
pub fn drop_replication_slot_sql(slot: &str) -> String {
    format!(
        "DO $$ BEGIN \
           IF EXISTS (SELECT FROM pg_replication_slots WHERE slot_name = {slot_lit}) THEN \
             PERFORM pg_drop_replication_slot({slot_lit}); \
           END IF; \
         END $$;",
        slot_lit = quote_literal(slot),
    )
}
