//! CDC role, publication, capture position, and relation-map statements.

use anyhow::Context as _;
use postgres_protocol::escape::{escape_identifier, escape_literal};
use sqlx::Row as _;
use std::fmt::Write as _;

/// Whether the CDC publication `$1` exists: one row when it does. Run
/// connected to the project-env database, the database the publication is
/// bound to. `apply` judges its CDC ensure by this observation
/// (docs/architecture/overview.md#environment-lifecycle).
pub const PUBLICATION_PROBE_SQL: &str = "SELECT 1 FROM pg_publication WHERE pubname = $1";

/// Idempotently create the **failover-enabled** logical capture position via
/// the SQL-function form: `pg_create_logical_replication_slot(<name>,
/// 'pgoutput', temporary => false, twophase => false, failover => true)`
/// (PG17+ fifth argument) — a normal connection, no replication-protocol
/// syntax. The reader reads this exact shape at preflight and refuses a capture position
/// that differs from it (wamn-0h0g.19.19). The reader never creates a capture position.
/// Logical slots are DATABASE-BOUND: run connected to the project-env database.
/// WAL is pinned from creation (capture starts at CDC-enable), bounded by the
/// cluster's `max_slot_wal_keep_size`.
pub fn create_failover_capture_sql(name: &str) -> String {
    format!(
        "DO $$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_replication_slots WHERE slot_name = {name_lit}) THEN \
             PERFORM pg_create_logical_replication_slot({name_lit}, 'pgoutput', false, false, true); \
           END IF; \
         END $$;",
        name_lit = escape_literal(name),
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
/// nothing and cost a plain-SQL read of every project table to the same
/// credential — with none of the conditions (the `REPLICATION` attribute, the
/// walsender protocol, capture ownership) that guard the replication path.
///
/// The `REVOKE … ON ALL TABLES` is the **retroactive** half: environments
/// provisioned before this narrowing already executed the blanket grant, and a
/// narrower grant does not undo it. It precedes the entity-map grant because it
/// would otherwise strip it. Idempotent; run connected to the project-env
/// database AFTER the schema and both classification maps exist.
pub fn grant_replication_access_sql(database: &str, role: &str, schema: &str) -> String {
    let role = escape_identifier(role);
    format!(
        "GRANT CONNECT ON DATABASE {db} TO {role}; \
         GRANT USAGE ON SCHEMA {schema} TO {role}; \
         REVOKE SELECT ON ALL TABLES IN SCHEMA {schema} FROM {role}; \
         GRANT SELECT ON {schema}.wamn_entity TO {role}; \
         GRANT SELECT ON {schema}.wamn_cdc_exclusion TO {role};",
        db = escape_identifier(database),
        schema = escape_identifier(schema),
    )
}

/// The decode-time map of explicitly declared package mechanism relations that
/// must not enter the event plane. OID keys preserve the disposition for WAL
/// written before a rename or replacement; rows are therefore never deleted.
/// The schema owner owns the map, including when an administrator creates it.
pub fn ensure_cdc_exclusion_map_sql(schema: &str) -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {schema}.wamn_cdc_exclusion ( \
           relation_oid oid PRIMARY KEY, \
           package_id text NOT NULL, \
           relation_id text NOT NULL, \
           table_name text NOT NULL); \
         DO $map_owner$ BEGIN \
           EXECUTE format('ALTER TABLE %I.wamn_cdc_exclusion OWNER TO %I', {schema_lit}, \
             (SELECT pg_get_userbyid(nspowner) FROM pg_namespace WHERE nspname = {schema_lit})); \
         END $map_owner$;",
        schema = escape_identifier(schema),
        schema_lit = escape_literal(schema),
    )
}

/// Upsert one explicit CDC exclusion without permitting an OID to be rebound
/// to another package-local relation identity.
pub fn upsert_cdc_exclusion_map_sql(schema: &str) -> String {
    format!(
        "INSERT INTO {schema}.wamn_cdc_exclusion AS current \
               (relation_oid, package_id, relation_id, table_name) \
         SELECT c.oid, $1, $2, $3::text FROM pg_class c \
         JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = {schema_lit} AND c.relname = $3::text AND c.relkind = 'r' \
         ON CONFLICT (relation_oid) DO UPDATE \
           SET table_name = EXCLUDED.table_name \
         WHERE current.package_id = EXCLUDED.package_id \
           AND current.relation_id = EXCLUDED.relation_id",
        schema = escape_identifier(schema),
        schema_lit = escape_literal(schema),
    )
}

/// The decode-time entity map (wamn-l5i9.11, D19 v3 §4): `relation_oid` →
/// stable package-local entity identity. **OID-keyed** so a reader resolving events is
/// timeless under catch-up — pg_class OIDs survive `ALTER TABLE RENAME`, so a
/// session decoding pre-rename backlog still resolves correctly, and a rename
/// only updates the informational `table_name`. Maintained by `apply-package`
/// in the same transaction as the DDL;
/// rows are upsert-only (a dropped entity's row keeps old-WAL decode
/// resolvable). No RLS: it holds no project data, and the CDC role's decode
/// stream sees every row of every table anyway.
/// The schema owner owns the map, including when an administrator creates it.
/// The host reads the map to admit Data change runs.
pub fn ensure_entity_map_sql(schema: &str) -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {schema}.wamn_entity ( \
           relation_oid oid PRIMARY KEY, \
           package_id text NOT NULL, \
           entity_id text NOT NULL, \
           table_name text NOT NULL); \
         DO $map_owner$ BEGIN \
           EXECUTE format('ALTER TABLE %I.wamn_entity OWNER TO %I', {schema_lit}, \
             (SELECT pg_get_userbyid(nspowner) FROM pg_namespace WHERE nspname = {schema_lit})); \
         END $map_owner$; \
         GRANT SELECT ON {schema}.wamn_entity TO {role};",
        schema = escape_identifier(schema),
        schema_lit = escape_literal(schema),
        role = escape_identifier(wamn_role::PLATFORM_ROLE),
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
        "INSERT INTO {schema}.wamn_entity AS mapped \
               (relation_oid, package_id, entity_id, table_name) \
         SELECT c.oid, $1, $2, $3::text FROM pg_class c \
         JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = {schema_lit} AND c.relname = $3::text AND c.relkind = 'r' \
         ON CONFLICT (relation_oid) DO UPDATE \
           SET table_name = EXCLUDED.table_name \
         WHERE mapped.package_id = EXCLUDED.package_id \
           AND mapped.entity_id = EXCLUDED.entity_id",
        schema = escape_identifier(schema),
        schema_lit = escape_literal(schema),
    )
}

/// Drop the capture position if it exists (teardown / condition only — dropping a
/// live capture position severs the reader and releases the pinned WAL). Run connected to
/// the project's database.
pub fn drop_capture_sql(name: &str) -> String {
    format!(
        "DO $$ BEGIN \
           IF EXISTS (SELECT FROM pg_replication_slots WHERE slot_name = {name_lit}) THEN \
             PERFORM pg_drop_replication_slot({name_lit}); \
           END IF; \
         END $$;",
        name_lit = escape_literal(name),
    )
}

/// Restore the CDC position, maps and grants after package SQL creates the model schemas.
pub async fn apply_cdc_sql(
    admin_database_url: &str,
    capture_database_url: &str,
    schemas: &[wamn_package::canonical_name::CanonicalName],
    cdc_name: &str,
    database: &str,
) -> anyhow::Result<()> {
    let mut client = wamn_postgres::connect(admin_database_url).await?;
    let connected: String = sqlx::query("SELECT current_database()")
        .fetch_one(&mut client)
        .await?
        .get(0);
    anyhow::ensure!(
        connected == database,
        "CDC SQL targets a different database"
    );
    let login = wamn_role::CDC_LOGIN.parse()?;
    wamn_postgres::connection_readiness::wait_role(&mut client, &login).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(cdc_schema_sql_bundle(
        schemas, database,
    )))
    .execute(&mut client)
    .await
    .map(drop)
    .context("restore CDC maps and grants")?;
    let mut capture = wamn_postgres::connect(capture_database_url).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        crate::capture_sql::create_failover_capture_sql(cdc_name),
    ))
    .execute(&mut capture)
    .await
    .map(drop)
    .context("restore the capture slot")
}

/// The capture maps and grants of each model schema, or the database grant alone.
pub fn cdc_schema_sql_bundle(
    schemas: &[wamn_package::canonical_name::CanonicalName],
    db_name: &str,
) -> String {
    let mut bundle = String::new();
    if schemas.is_empty() {
        writeln!(
            bundle,
            "\nGRANT CONNECT ON DATABASE {} TO {};",
            postgres_protocol::escape::escape_identifier(db_name),
            postgres_protocol::escape::escape_identifier(wamn_role::CDC_LOGIN)
        )
        .expect("write the empty capture grant");
    }
    for schema in schemas {
        writeln!(
            bundle,
            "\n{entity_map};\n{exclusion_map};\n{grants}",
            entity_map = ensure_entity_map_sql(schema.as_str()),
            exclusion_map = ensure_cdc_exclusion_map_sql(schema.as_str()),
            grants = grant_replication_access_sql(db_name, wamn_role::CDC_LOGIN, schema.as_str()),
        )
        .expect("write CDC SQL into a string");
    }
    bundle
}
