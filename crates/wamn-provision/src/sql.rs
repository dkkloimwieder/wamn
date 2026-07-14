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
fn quote_ident(ident: &str) -> String {
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
    fn database_exists_is_parameterized() {
        assert_eq!(
            database_exists_sql(),
            "SELECT EXISTS (SELECT FROM pg_database WHERE datname = $1)"
        );
    }
}
