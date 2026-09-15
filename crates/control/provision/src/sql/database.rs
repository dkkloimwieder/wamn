//! Database creation, ownership, and extensions.

use super::{DB_OWNER_ROLE, quote_ident};

/// Idempotently create or harden the database-owner role [`DB_OWNER_ROLE`] as
/// NOLOGIN, under the shared `wamn_role_bootstrap` advisory lock — the
/// [`super::ensure_control_author_acl_role_sql`] shape, so a replay that finds a
/// drifted attribute re-`ALTER`s it instead of reporting success.
///
/// **Zero grants, zero memberships, no generation pair.** The role exists only
/// to hold title; nothing connects as it, so it is deliberately outside the
/// credential-rotation machinery. Ownership is conferred declaratively by the
/// `Database` CR's `spec.owner`
/// ([`render_project_env_database`](crate::render_project_env_database)) and
/// convergently by [`set_database_owner_sql`].
///
/// **Apply this to the target cluster BEFORE any `Database` CR naming it** —
/// CNPG maps `spec.owner` to `CREATE DATABASE … OWNER` / `ALTER DATABASE …
/// OWNER TO`, and both fail against a role that does not exist yet.
pub fn ensure_db_owner_role_sql() -> &'static str {
    "DO $db_owner$ BEGIN \
       PERFORM pg_advisory_xact_lock(hashtext('wamn_role_bootstrap')); \
       IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles \
                      WHERE rolname = 'wamn_db_owner') THEN \
         CREATE ROLE wamn_db_owner NOLOGIN NOSUPERUSER NOCREATEDB \
           NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS; \
       ELSIF EXISTS (SELECT FROM pg_catalog.pg_roles \
                     WHERE rolname = 'wamn_db_owner' \
                       AND (rolcanlogin OR rolsuper OR rolcreatedb OR rolcreaterole \
                            OR rolinherit OR rolreplication OR rolbypassrls)) THEN \
         ALTER ROLE wamn_db_owner NOLOGIN NOSUPERUSER NOCREATEDB \
           NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS; \
       END IF; \
     END $db_owner$;"
}

/// `ALTER DATABASE "<database>" OWNER TO "wamn_db_owner"` — the convergence
/// half of the ownership migration, for databases that already exist.
///
/// A `REVOKE` cannot express ownership, so moving an already-provisioned
/// project-env database off its old owner is this `ALTER`, not a grant edit.
/// It is naturally idempotent (setting the owner a database already has is a
/// no-op), so the privilege batch stays re-runnable and no one-shot migration
/// script is needed: fresh databases arrive owned correctly from the CR, old
/// ones converge here, and re-applying converges again.
///
/// **Order is load-bearing: this must run BEFORE the privilege batch's
/// `CONNECT` revokes.** `ALTER DATABASE … OWNER TO` rewrites the outgoing
/// owner's ACL entry to the incoming owner, so a revoke applied before it can be
/// undone by the entry the owner change carries over.
///
/// Run as the superuser provisioning principal (which needs no membership in
/// the new owner), connected to any database on the target cluster, AFTER the
/// database exists.
pub fn set_database_owner_sql(database: &str) -> String {
    format!(
        "ALTER DATABASE {db} OWNER TO {owner}",
        db = quote_ident(database),
        owner = quote_ident(DB_OWNER_ROLE),
    )
}

/// `CREATE DATABASE "<database>"`, naming the database directly (not derived from
/// a project id). The name is double-quoted (a slug-derived name cannot contain a `"`, so it is
/// injection-safe). Must run as its own autocommit statement (Postgres forbids
/// `CREATE DATABASE` inside a transaction block).
///
/// Pass a name from [`project_env_database_name`](crate::project_env_database_name)
/// (`wamn-db-<org>--<project>--<env>--<instance>`). In production the CNPG `Database` CRD
/// creates the per-project-env database; this is the plain-SQL equivalent the
/// substrate-agnostic gate uses off-cluster (wamn-q3n.8).
/// PostgreSQL extensions the PLATFORM installs in every project-environment
/// database. Closed, and owned here rather than by any package.
///
/// A package cannot install one: the migration policy lists `extension` among
/// its refused object classes, so `CREATE EXTENSION` refuses inside package
/// DDL. Anything a package's own DDL depends on must therefore be provisioned,
/// and this list is that promise.
///
/// `btree_gist` carries the equality operator classes a `gist` index needs, so
/// an `EXCLUDE USING gist (<scalar> WITH =, <range> WITH &&)` overlap
/// constraint is expressible at all. Without it that constraint refuses at
/// apply with an operator-class error, and the platform's own documented
/// overlap rule is unreachable from a package (`wamn-yk9l`). Three independent
/// agents authoring a dock-appointment package hit exactly that and all three
/// fell back to a row lock.
pub const PLATFORM_EXTENSIONS: [&str; 1] = ["btree_gist"];

/// `CREATE EXTENSION IF NOT EXISTS` for every platform extension, for the
/// CURRENT database.
///
/// Idempotent by construction, so it converges a database provisioned before
/// this list existed rather than refusing on one that already has them. It
/// needs the administrator connection: extension installation is not a
/// privilege the package-owner role holds.
pub fn install_platform_extensions_sql() -> String {
    PLATFORM_EXTENSIONS
        .iter()
        .map(|extension| format!("CREATE EXTENSION IF NOT EXISTS {}", quote_ident(extension)))
        .collect::<Vec<_>>()
        .join(";\n")
        + ";\n"
}

pub fn create_database_named_sql(database: &str) -> String {
    format!("CREATE DATABASE {}", quote_ident(database))
}

/// `DROP DATABASE IF EXISTS "<database>" WITH (FORCE)`, naming the database
/// directly (teardown / gate only; destructive). Autocommit.
pub fn drop_database_named_sql(database: &str) -> String {
    format!(
        "DROP DATABASE IF EXISTS {} WITH (FORCE)",
        quote_ident(database)
    )
}
