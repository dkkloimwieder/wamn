//! Reset application objects while retaining the provisioned database and CDC.

use anyhow::ensure;
use sqlx::Connection as _;

/// Delete migration-owned objects and application state. Callers stop users first.
pub async fn reset_database(
    system_url: &str,
    database_url: &str,
    migration_url: &str,
    triple: &wamn_registry::Triple,
) -> anyhow::Result<()> {
    let mut system = wamn_postgres::connect(system_url).await?;
    let disposable = wamn_registry::deployment::disposable(&mut system, triple).await?;
    ensure!(disposable, "environment is not recorded as disposable");
    let mut client = wamn_postgres::connect(database_url).await?;
    let database: String = sqlx::query_scalar("SELECT current_database()::text")
        .fetch_one(&mut client)
        .await?;
    let login = wamn_provision::migration::login(&database);
    let mut tx = client.begin().await?;
    sqlx::raw_sql(crate::apply_package::LOCK_SQL)
        .execute(&mut *tx)
        .await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_provision::bind_platform_principal_sql(
            wamn_provision::PlatformComponent::Provisioning,
        ),
    ))
    .execute(&mut *tx)
    .await?;
    let mut migration = wamn_postgres::connect(migration_url).await?;
    let migration_login = wamn_provision::migration::require_login(&mut migration).await?;
    ensure!(
        migration_login == login,
        "migration and owner connections name different databases"
    );
    let schemas: Vec<String> = sqlx::query_scalar(
        "SELECT nspname::text FROM pg_namespace WHERE nspowner = (SELECT datdba FROM pg_database WHERE datname = current_database()) AND nspname NOT LIKE 'pg_%' AND nspname NOT IN ('public','information_schema','platform','wamn_authority','wamn_history','catalog','wamn_run') AND NOT EXISTS (SELECT FROM pg_depend WHERE classid='pg_namespace'::regclass AND objid=pg_namespace.oid AND deptype='e')",
    )
    .fetch_all(&mut *tx)
    .await?;
    let functions: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT p.oid::regprocedure::text FROM pg_trigger t \
         JOIN pg_class c ON c.oid=t.tgrelid JOIN pg_namespace n ON n.oid=c.relnamespace \
         JOIN pg_proc p ON p.oid=t.tgfoid \
         WHERE n.nspname=ANY($1) AND p.pronamespace='wamn_history'::regnamespace \
           AND starts_with(p.proname::text,n.nspname::text||'__')",
    )
    .bind(&schemas)
    .fetch_all(&mut *tx)
    .await?;
    for schema in schemas {
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "DROP SCHEMA IF EXISTS {} CASCADE",
            postgres_protocol::escape::escape_identifier(&schema)
        )))
        .execute(&mut *tx)
        .await?;
    }
    for function in functions {
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP FUNCTION {function}")))
            .execute(&mut *tx)
            .await?;
    }
    // SQLx histories are trusted platform objects, separate from application DDL ownership.
    for (table,) in sqlx::query_as::<_, (String,)>("SELECT quote_ident(n.nspname)||'.'||quote_ident(c.relname) FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='wamn_history' AND c.relname LIKE 'migrations_%' AND c.relkind='r'").fetch_all(&mut *tx).await? {
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP TABLE {table}"))).execute(&mut *tx).await?;
    }
    let tables: Vec<String> = sqlx::query_scalar("SELECT quote_ident(n.nspname)||'.'||quote_ident(c.relname) FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname IN ('catalog','wamn_run') AND c.relkind='r'")
        .fetch_all(&mut *tx).await?;
    if !tables.is_empty() {
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "TRUNCATE {} CASCADE",
            tables.join(", ")
        )))
        .execute(&mut *tx)
        .await?;
    }
    sqlx::raw_sql("TRUNCATE platform.write_log")
        .execute(&mut *tx)
        .await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "{} {}",
        wamn_provision::sql::grant_workload_login_sql(
            wamn_provision::WorkloadRoleFamily::Migration,
            &database,
            &login,
        ),
        wamn_provision::migration::grants_sql(&login)
    )))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}
