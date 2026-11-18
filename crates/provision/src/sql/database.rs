//! Database creation, ownership, and extensions.

use super::escape_identifier;

/// Assign a project's database to its single migration login.
pub fn set_database_owner_sql(database: &str) -> String {
    format!(
        "ALTER DATABASE {} OWNER TO {}",
        escape_identifier(database),
        escape_identifier(crate::migration::login().as_str()),
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
/// substrate-agnostic condition uses off-cluster (wamn-q3n.8).
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
///
/// `pg_stat_statements` gives the operator the calls and the time of each
/// statement (`wamn-smtdg`). Its view reads only where the server preloads the
/// library. Without the preload, the extension still installs.
pub const PLATFORM_EXTENSIONS: [&str; 2] = ["btree_gist", "pg_stat_statements"];

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
        .map(|extension| {
            format!(
                "CREATE EXTENSION IF NOT EXISTS {}",
                escape_identifier(extension)
            )
        })
        .collect::<Vec<_>>()
        .join(";\n")
        + ";\n"
}

pub fn create_database_named_sql(database: &str) -> String {
    format!("CREATE DATABASE {}", escape_identifier(database))
}

/// `DROP DATABASE IF EXISTS "<database>" WITH (FORCE)`, naming the database
/// directly (teardown / condition only; destructive). Autocommit.
pub fn drop_database_named_sql(database: &str) -> String {
    format!(
        "DROP DATABASE IF EXISTS {} WITH (FORCE)",
        escape_identifier(database)
    )
}
