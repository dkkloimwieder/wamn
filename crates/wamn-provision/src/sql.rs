//! Pure Postgres text builders for project provisioning (SR3 house rule 3:
//! pure text + validated/quoted identifiers; the driver holds the connection).
//!
//! Every builder takes an **already-validated** project id (see
//! [`crate::validate_project_id`]); the database name it derives is
//! double-quoted, so a slug (which cannot contain a `"`) is injection-safe. The
//! `wamn_app` role name is a pinned constant. Values that vary (a probe's
//! database name, a role password) travel as `$n` params or quoted literals.

use crate::name::{APP_ROLE, database_name};

/// Quote a SQL identifier (double-quoted, embedded `"` doubled). Mirrors the
/// canonical `wamn_ddl::sql::quote_ident` (inlined to keep this crate's
/// dependency closure to `serde_json`).
pub(crate) fn quote_ident(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

/// Quote a SQL string literal (single-quoted, embedded `'` doubled). Mirrors the
/// canonical `wamn_ddl::sql::quote_literal`.
fn quote_literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Idempotently bootstrap the shared, cluster-global [`APP_ROLE`]. Runs in a
/// `DO` block so re-running against a cluster that already has the role is a
/// no-op. `NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS` — the role can only
/// do DML under RLS on tables explicitly granted to it (the S2/2.2 model). In
/// production the role is pre-created once; this makes the tool self-contained.
pub fn ensure_app_role_sql(password: &str) -> String {
    format!(
        "DO $$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = {role_lit}) THEN \
             CREATE ROLE {role} LOGIN PASSWORD {pw} \
               NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS; \
           END IF; \
         END $$;",
        role = quote_ident(APP_ROLE),
        role_lit = quote_literal(APP_ROLE),
        pw = quote_literal(password),
    )
}

/// `CREATE DATABASE "<database>"`, naming the database directly (not derived from
/// a project id) — the per-project-env counterpart of [`create_database_sql`],
/// mirroring [`grant_connect_on_database_sql`] taking an already-derived name.
/// The name is double-quoted (a slug-derived name cannot contain a `"`, so it is
/// injection-safe). Must run as its own autocommit statement (Postgres forbids
/// `CREATE DATABASE` inside a transaction block).
///
/// Pass a name from [`project_env_database_name`](crate::project_env_database_name)
/// (`wamn-db-<org>--<project>--<env>`). In production the CNPG `Database` CRD
/// creates the per-project-env database; this is the plain-SQL equivalent the
/// substrate-agnostic gate uses off-cluster (wamn-q3n.8).
pub fn create_database_named_sql(database: &str) -> String {
    format!("CREATE DATABASE {}", quote_ident(database))
}

/// `CREATE DATABASE "wamn-db-<project>"`. Must run as its own autocommit
/// statement (Postgres forbids `CREATE DATABASE` inside a transaction block).
pub fn create_database_sql(project: &str) -> String {
    create_database_named_sql(&database_name(project))
}

/// `DROP DATABASE IF EXISTS "<database>" WITH (FORCE)`, naming the database
/// directly — the per-project-env counterpart of [`drop_database_sql`] (teardown /
/// gate only; destructive). Autocommit.
pub fn drop_database_named_sql(database: &str) -> String {
    format!(
        "DROP DATABASE IF EXISTS {} WITH (FORCE)",
        quote_ident(database)
    )
}

/// `DROP DATABASE IF EXISTS "wamn-db-<project>" WITH (FORCE)` — teardown / gate
/// only (destructive; the production tool never drops). Autocommit.
pub fn drop_database_sql(project: &str) -> String {
    drop_database_named_sql(&database_name(project))
}

/// Probe whether a database exists. The database **name** is the `$1`
/// parameter (a value, not an interpolated identifier); pass
/// [`database_name`](crate::database_name)`(project)`.
pub fn database_exists_sql() -> &'static str {
    "SELECT EXISTS (SELECT FROM pg_database WHERE datname = $1)"
}

/// Restrict `CONNECT` on a database — named by its full, already-derived name —
/// to the shared app role: revoke it from `PUBLIC` (every new database grants
/// `PUBLIC` `CONNECT` by default) and grant it to [`APP_ROLE`]. The name is
/// double-quoted (a slug-derived name cannot contain a `"`, so it is
/// injection-safe). Both statements are idempotent; issue as one batch.
///
/// This is the "thin imperative privilege step" the CNPG `Database` CRD does not
/// cover (per-project-env provisioning, wamn-q3n.7): the CRD creates the database
/// declaratively, but `REVOKE CONNECT FROM PUBLIC` / `GRANT` is run here. It is
/// defense-in-depth — the primary cross-project isolation is that a component is
/// routed to exactly one project's database (see the crate docs).
pub fn grant_connect_on_database_sql(database: &str) -> String {
    let db = quote_ident(database);
    format!(
        "REVOKE CONNECT ON DATABASE {db} FROM PUBLIC; \
         GRANT CONNECT ON DATABASE {db} TO {role};",
        role = quote_ident(APP_ROLE),
    )
}

/// Restrict `CONNECT` on the per-project database `wamn-db-<project>` (2.3) — a
/// thin wrapper over [`grant_connect_on_database_sql`] with the project's derived
/// database name.
pub fn grant_connect_sql(project: &str) -> String {
    grant_connect_on_database_sql(&database_name(project))
}

// --- CDC capture provisioning (wamn-l5i9.9, D19 v3 §4) -----------------------
//
// The per-project-env CDC substrate: a REPLICATION role (the R8b credential
// tier above `wamn_app` query creds and the dispatch role), a publication over
// the app data schema, and a failover-enabled logical replication slot. The
// publication and the slot are DATABASE-BOUND — apply their SQL connected to
// the project-env database; the role is cluster-global. Pass the shared
// `cdc_object_name` (`wamn_cdc_<org>__<project>__<env>`) as the role /
// publication / slot name.

/// Idempotently bootstrap a per-project-env **replication** role: `REPLICATION
/// LOGIN`, otherwise least-privilege (`NOSUPERUSER NOCREATEDB NOCREATEROLE
/// NOBYPASSRLS`). One role per project-env (a leaked credential's blast radius
/// is one registration) — but note `REPLICATION` itself is CLUSTER-WIDE in
/// Postgres: any replication role can read any database's WAL on that cluster,
/// so on a shared pool the input-side isolation rests on handing each reader
/// only its own slot/publication/credentials (documented in the runbook).
pub fn ensure_replication_role_sql(role: &str, password: &str) -> String {
    format!(
        "DO $$ BEGIN \
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = {role_lit}) THEN \
             CREATE ROLE {role} LOGIN REPLICATION PASSWORD {pw} \
               NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS; \
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
/// database, `USAGE` on the app data schema, and `SELECT` on the schema's
/// **current** tables (an initial-snapshot/backfill needs table reads; logical
/// *decoding* itself reads WAL, not tables, so tables added later decode fine
/// without a re-grant). Idempotent; run connected to the project-env database
/// AFTER the schema exists.
pub fn grant_replication_access_sql(database: &str, role: &str, schema: &str) -> String {
    let role = quote_ident(role);
    format!(
        "GRANT CONNECT ON DATABASE {db} TO {role}; \
         GRANT USAGE ON SCHEMA {schema} TO {role}; \
         GRANT SELECT ON ALL TABLES IN SCHEMA {schema} TO {role};",
        db = quote_ident(database),
        schema = quote_ident(schema),
    )
}

/// The decode-time entity map (wamn-l5i9.11, D19 v3 §4): `relation_oid` →
/// stable catalog entity id. **OID-keyed** so a reader resolving events is
/// timeless under catch-up — pg_class OIDs survive `ALTER TABLE RENAME`, so a
/// session decoding pre-rename backlog still resolves correctly, and a rename
/// only updates the informational `table_name`. Maintained by
/// `publish-catalog`/`migrate-catalog` in the same transaction as the DDL;
/// rows are upsert-only (a dropped entity's row keeps old-WAL decode
/// resolvable). No RLS: it holds no tenant data, and the CDC role's decode
/// stream sees every row of every table anyway.
pub fn ensure_entity_map_sql(schema: &str) -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {schema}.wamn_entities ( \
           relation_oid oid PRIMARY KEY, \
           entity_id text NOT NULL, \
           table_name text NOT NULL)",
        schema = quote_ident(schema),
    )
}

/// Upsert one entity's map row, resolving the table's CURRENT `pg_class` OID
/// server-side — run in the SAME transaction as the DDL that created/renamed
/// the table, so the row is atomic with the physical state. `$1` = entity id,
/// `$2` = physical table name. A table that does not exist (a catalog entity
/// whose floor was never applied) upserts nothing — the SELECT is empty.
pub fn upsert_entity_map_sql(schema: &str) -> String {
    format!(
        "INSERT INTO {schema}.wamn_entities (relation_oid, entity_id, table_name) \
         SELECT c.oid, $1, $2::text FROM pg_class c \
         JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = {schema_lit} AND c.relname = $2::text AND c.relkind = 'r' \
         ON CONFLICT (relation_oid) DO UPDATE \
           SET entity_id = EXCLUDED.entity_id, table_name = EXCLUDED.table_name",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_app_role_is_least_privilege_and_idempotent() {
        let sql = ensure_app_role_sql("wamn_app");
        assert!(sql.contains("IF NOT EXISTS"), "idempotent guard");
        assert!(sql.contains("CREATE ROLE \"wamn_app\""));
        assert!(sql.contains("PASSWORD 'wamn_app'"));
        // Least privilege — every restrictive attribute is present.
        for attr in ["NOSUPERUSER", "NOCREATEDB", "NOCREATEROLE", "NOBYPASSRLS"] {
            assert!(sql.contains(attr), "missing {attr}");
        }
        // A password with a quote is escaped, not injected.
        assert!(ensure_app_role_sql("a'b").contains("PASSWORD 'a''b'"));
    }

    #[test]
    fn database_ddl_quotes_the_hyphenated_name() {
        assert_eq!(
            create_database_sql("acme-corp"),
            "CREATE DATABASE \"wamn-db-acme-corp\""
        );
        assert_eq!(
            drop_database_sql("acme-corp"),
            "DROP DATABASE IF EXISTS \"wamn-db-acme-corp\" WITH (FORCE)"
        );
    }

    #[test]
    fn named_database_ddl_targets_an_arbitrary_db_name_and_the_wrappers_delegate() {
        // The per-project-env path (wamn-q3n.7/.8) passes a full triple-derived name.
        assert_eq!(
            create_database_named_sql("wamn-db-acme--billing--dev"),
            "CREATE DATABASE \"wamn-db-acme--billing--dev\""
        );
        assert_eq!(
            drop_database_named_sql("wamn-db-acme--billing--dev"),
            "DROP DATABASE IF EXISTS \"wamn-db-acme--billing--dev\" WITH (FORCE)"
        );
        // The 2.3 project-taking wrappers delegate to the named builders.
        assert_eq!(
            create_database_sql("acme"),
            create_database_named_sql("wamn-db-acme")
        );
        assert_eq!(
            drop_database_sql("acme"),
            drop_database_named_sql("wamn-db-acme")
        );
    }

    #[test]
    fn grant_connect_revokes_public_then_grants_app_role() {
        let sql = grant_connect_sql("acme");
        // The REVOKE FROM PUBLIC must precede the GRANT (order is not load-bearing
        // for correctness, but both must be present — the isolation backstop).
        let revoke = sql
            .find("REVOKE CONNECT ON DATABASE \"wamn-db-acme\" FROM PUBLIC")
            .expect("revoke public");
        let grant = sql
            .find("GRANT CONNECT ON DATABASE \"wamn-db-acme\" TO \"wamn_app\"")
            .expect("grant app role");
        assert!(revoke < grant);
    }

    #[test]
    fn grant_connect_on_database_targets_an_arbitrary_db_name() {
        // The per-project-env path (wamn-q3n.7) passes a full triple-derived name.
        let sql = grant_connect_on_database_sql("wamn-db-acme--billing--dev");
        assert!(
            sql.contains("REVOKE CONNECT ON DATABASE \"wamn-db-acme--billing--dev\" FROM PUBLIC")
        );
        assert!(
            sql.contains(
                "GRANT CONNECT ON DATABASE \"wamn-db-acme--billing--dev\" TO \"wamn_app\""
            )
        );
        // The project-taking 2.3 wrapper delegates to it with the derived name.
        assert_eq!(
            grant_connect_sql("acme"),
            grant_connect_on_database_sql("wamn-db-acme")
        );
    }

    #[test]
    fn replication_role_is_replication_login_and_otherwise_least_privilege() {
        let sql = ensure_replication_role_sql("wamn_cdc_acme__billing__dev", "s3cr3t");
        assert!(sql.contains("IF NOT EXISTS"), "idempotent guard");
        assert!(sql.contains("CREATE ROLE \"wamn_cdc_acme__billing__dev\" LOGIN REPLICATION"));
        assert!(sql.contains("PASSWORD 's3cr3t'"));
        // The R8b tier: REPLICATION but nothing else elevated.
        for attr in ["NOSUPERUSER", "NOCREATEDB", "NOCREATEROLE", "NOBYPASSRLS"] {
            assert!(sql.contains(attr), "missing {attr}");
        }
        // A password with a quote is escaped, not injected.
        assert!(ensure_replication_role_sql("r", "a'b").contains("PASSWORD 'a''b'"));
    }

    #[test]
    fn publication_covers_the_schema_and_is_idempotent() {
        let sql = create_publication_sql("wamn_cdc_acme__billing__dev", "app");
        // FOR TABLES IN SCHEMA (auto-includes tables created later) — never the
        // per-table form and never FOR ALL TABLES (which would leak app_system
        // and any other schema into the stream).
        assert!(sql.contains(
            "CREATE PUBLICATION \"wamn_cdc_acme__billing__dev\" FOR TABLES IN SCHEMA \"app\""
        ));
        assert!(!sql.contains("FOR ALL TABLES"));
        // Idempotent: guarded by a pg_publication probe (no IF NOT EXISTS in PG).
        assert!(sql.contains("IF NOT EXISTS (SELECT FROM pg_publication WHERE pubname = 'wamn_cdc_acme__billing__dev')"));
        // The eager schema guard is a separate statement.
        assert_eq!(
            ensure_schema_sql("app"),
            "CREATE SCHEMA IF NOT EXISTS \"app\""
        );
    }

    #[test]
    fn failover_slot_uses_the_sql_function_form_with_failover_true() {
        let sql = create_failover_slot_sql("wamn_cdc_acme__billing__dev");
        // The PG17+ five-argument form: (slot, plugin, temporary, twophase,
        // FAILOVER) — pgoutput, non-temporary, no two-phase, failover=true, the
        // exact shape pg_walstream's ensure_replication_slot tolerates.
        assert!(sql.contains(
            "pg_create_logical_replication_slot('wamn_cdc_acme__billing__dev', 'pgoutput', false, false, true)"
        ));
        // Idempotent: guarded by a pg_replication_slots probe.
        assert!(
            sql.contains("IF NOT EXISTS (SELECT FROM pg_replication_slots WHERE slot_name = 'wamn_cdc_acme__billing__dev')")
        );
    }

    #[test]
    fn replication_grants_cover_connect_usage_and_current_tables() {
        let sql = grant_replication_access_sql("wamn-db-acme--billing--dev", "wamn_cdc_x", "app");
        assert!(sql.contains(
            "GRANT CONNECT ON DATABASE \"wamn-db-acme--billing--dev\" TO \"wamn_cdc_x\""
        ));
        assert!(sql.contains("GRANT USAGE ON SCHEMA \"app\" TO \"wamn_cdc_x\""));
        assert!(sql.contains("GRANT SELECT ON ALL TABLES IN SCHEMA \"app\" TO \"wamn_cdc_x\""));
    }

    /// The entity-map drift guard (wamn-l5i9.11): the PINNED SQL is the
    /// load-bearing contract — the reader's OID lookup, the same-transaction
    /// upsert, and the OID-keyed rename-proofing all ride these exact strings.
    #[test]
    fn entity_map_is_oid_keyed_and_upserted_from_pg_class() {
        assert_eq!(
            ensure_entity_map_sql("app"),
            "CREATE TABLE IF NOT EXISTS \"app\".wamn_entities ( \
               relation_oid oid PRIMARY KEY, \
               entity_id text NOT NULL, \
               table_name text NOT NULL)"
        );
        let upsert = upsert_entity_map_sql("app");
        // The OID is resolved server-side from pg_class IN the DDL transaction
        // (ordinary tables only), keyed for conflict on relation_oid — a
        // rename re-upserts the SAME row (new table_name, same entity/OID).
        assert!(
            upsert.contains(
                "INSERT INTO \"app\".wamn_entities (relation_oid, entity_id, table_name)"
            )
        );
        // `$2::text` in BOTH the projection and the WHERE — a bare `$2` would
        // be deduced `name` at `c.relname = $2` and `text` at the column,
        // which tokio_postgres rejects ("inconsistent types deduced").
        assert!(upsert.contains("SELECT c.oid, $1, $2::text FROM pg_class c"));
        assert!(
            upsert.contains("WHERE n.nspname = 'app' AND c.relname = $2::text AND c.relkind = 'r'")
        );
        assert!(upsert.contains("ON CONFLICT (relation_oid) DO UPDATE"));
        assert!(
            upsert.contains("SET entity_id = EXCLUDED.entity_id, table_name = EXCLUDED.table_name")
        );
    }

    #[test]
    fn cdc_teardown_builders_are_guarded() {
        assert_eq!(
            drop_publication_sql("wamn_cdc_x"),
            "DROP PUBLICATION IF EXISTS \"wamn_cdc_x\""
        );
        let drop_slot = drop_replication_slot_sql("wamn_cdc_x");
        assert!(drop_slot.contains("pg_drop_replication_slot('wamn_cdc_x')"));
        assert!(drop_slot.contains("IF EXISTS"));
    }

    #[test]
    fn database_exists_is_parameterized() {
        assert_eq!(
            database_exists_sql(),
            "SELECT EXISTS (SELECT FROM pg_database WHERE datname = $1)"
        );
    }

    /// The escapers are inlined to keep the prod dep closure at `serde_json`, but
    /// they MUST stay byte-identical to the canonical `wamn_ddl::sql::*` that back
    /// the injection-safety argument (a slug cannot contain a `"`, so the derived
    /// database/role DDL is safe). Assert over adversarial inputs plus an
    /// exhaustive single-ASCII-char sweep so any future divergence in either copy
    /// fails here. (`wamn-ddl` is a dev-dependency, so this costs the prod build
    /// nothing.)
    #[test]
    fn inlined_escapers_match_canonical_wamn_ddl() {
        let mut cases: Vec<String> = vec![
            "".into(),
            "a".into(),
            "plain_ident".into(),
            "a\"b".into(),
            "\"\"".into(),
            "a'b".into(),
            "''".into(),
            "a\"'b".into(),
            "back\\slash".into(),
            "tab\there".into(),
            "new\nline".into(),
            "nul\0byte".into(),
            "münz".into(),
            "wamn-db-acme--billing--dev".into(),
            "'; DROP TABLE x; --".into(),
        ];
        for c in 0u8..=0x7f {
            cases.push(format!("x{}y", c as char));
        }
        for s in &cases {
            assert_eq!(
                quote_ident(s),
                wamn_ddl::sql::quote_ident(s),
                "quote_ident drift on {s:?}"
            );
            assert_eq!(
                quote_literal(s),
                wamn_ddl::sql::quote_literal(s),
                "quote_literal drift on {s:?}"
            );
        }
    }
}
