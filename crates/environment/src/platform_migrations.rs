//! PostgreSQL platform migration execution and reporting.

use crate::migrations::{MigrationFile, SchemaReport};
use crate::registry::lifecycle_lock::LifecycleLock;
use sqlx::PgConnection as Client;
use sqlx::migrate::Migration;
use wamn_postgres::migration_scope::MigrationDatabase;
use wamn_registry::Triple;

use wamn_postgres::migrations::{apply_target, embedded_sources};

pub async fn observe(
    client: &mut sqlx::PgConnection,
    database: MigrationDatabase,
    sources: &[Migration],
) -> anyhow::Result<SchemaReport> {
    let table = match database {
        MigrationDatabase::System => wamn_postgres::REGISTRY_HISTORY_TABLE,
        MigrationDatabase::Project => "platform._sqlx_migrations",
    };
    observe_history(client, table, sources).await
}
pub async fn observe_system(client: &mut sqlx::PgConnection) -> anyhow::Result<SchemaReport> {
    observe(
        client,
        MigrationDatabase::System,
        &embedded_sources(MigrationDatabase::System),
    )
    .await
}
pub async fn observe_project(client: &mut sqlx::PgConnection) -> anyhow::Result<SchemaReport> {
    observe(
        client,
        MigrationDatabase::Project,
        &embedded_sources(MigrationDatabase::Project),
    )
    .await
}

/// Apply or inspect substrate SQL under the caller's environment lock.
pub(crate) async fn project_schema(
    lock: &LifecycleLock,
    triple: &Triple,
    migration_database_url: &str,
    sources: &[Migration],
    apply: bool,
    output: impl std::io::Write,
) -> anyhow::Result<()> {
    crate::environment_registry::require_connected(&mut *lock.client().await, triple).await?;
    let mut client = wamn_postgres::connect(migration_database_url).await?;
    report(
        &mut client,
        MigrationDatabase::Project,
        sources,
        apply,
        output,
    )
    .await
}

async fn report(
    client: &mut sqlx::PgConnection,
    database: MigrationDatabase,
    sources: &[Migration],
    apply: bool,
    mut output: impl std::io::Write,
) -> anyhow::Result<()> {
    database.require_login(client).await?;
    let report = observe(client, database, sources).await?;
    write!(output, "{report}")?;
    output.flush()?;
    if apply {
        apply_target(client, database, sources).await?;
        write!(output, "{}", observe(client, database, sources).await?)?;
        output.flush()?;
    }
    Ok(())
}

/// Read applied migrations without creating a history table.
pub(crate) async fn observe_history(
    client: &mut Client,
    table: &str,
    sources: &[sqlx::migrate::Migration],
) -> anyhow::Result<SchemaReport> {
    use sqlx::migrate::Migrate as _;
    let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
        .bind(table)
        .fetch_one(&mut *client)
        .await?;
    let applied = if exists {
        client.list_applied_migrations(table).await?
    } else {
        Vec::new()
    };
    let mut report = SchemaReport::default();
    for row in &applied {
        let file = MigrationFile {
            version: row.version,
            checksum: hex::encode(&row.checksum),
        };
        if !sources
            .iter()
            .any(|source| source.migration_type.is_up_migration() && source.version == row.version)
        {
            report.missing.push(file.clone());
        }
        report.installed.push(file);
    }
    for source in sources
        .iter()
        .filter(|source| source.migration_type.is_up_migration())
    {
        match applied.iter().find(|row| row.version == source.version) {
            None => report.pending.push(MigrationFile {
                version: source.version,
                checksum: hex::encode(&source.checksum),
            }),
            Some(row) if row.checksum != source.checksum => report.warnings.push(format!(
                "SQLx migration {} checksum differs",
                source.version
            )),
            Some(_) => {}
        }
    }
    if exists && let Some(version) = client.dirty_version(table).await? {
        report
            .warnings
            .push(format!("SQLx migration {version} is incomplete"));
    }
    Ok(report)
}

/// Bootstrap the project schemas in one transaction as the database owner.
pub async fn install_project(client: &mut sqlx::PgConnection) -> anyhow::Result<bool> {
    use sqlx::Connection as _;

    MigrationDatabase::Project.require_login(client).await?;
    let mut tx = client.begin().await?;
    let present: bool = sqlx::query_scalar(r#"SELECT to_regclass('platform."user"') IS NOT NULL"#)
        .fetch_one(&mut *tx)
        .await?;
    if !present {
        for sql in [
            crate::project_setup::CATALOG_SCHEMA_SQL,
            crate::project_setup::APP_SCHEMA_SQL,
            include_str!("../../../deploy/sql/run-state.sql"),
            include_str!("../../../deploy/sql/run-queue.sql"),
        ] {
            sqlx::raw_sql(sql).execute(&mut *tx).await?;
        }
        for surface in [
            crate::project_setup::grant_dynamic_sql_surface_sql(),
            crate::project_setup::grant_platform_surface_sql(),
        ] {
            sqlx::raw_sql(sqlx::AssertSqlSafe(surface))
                .execute(&mut *tx)
                .await?;
        }
    }
    tx.commit().await?;
    Ok(!present)
}
