//! SQLx applies image-owned platform migrations under authenticated platform authority.

use anyhow::{Context as _, ensure};
use sqlx::migrate::{Migration, Migrator};
use wamn_control_provision::migrations::MigrationDatabase;

pub use wamn_package_control::schema_baseline::embedded_sources;

#[derive(Debug, Clone, Copy)]
pub enum MigrationTarget<'a> {
    System,
    Project(&'a crate::environment::lock::LifecycleLock),
}
impl MigrationTarget<'_> {
    fn database(self) -> MigrationDatabase {
        match self {
            Self::System => MigrationDatabase::System,
            Self::Project(_) => MigrationDatabase::Project,
        }
    }
}

pub async fn apply_reported(
    client: &mut sqlx::PgConnection,
    target: MigrationTarget<'_>,
    sources: &[Migration],
) -> anyhow::Result<usize> {
    apply_target(client, target.database(), sources).await
}

pub(crate) async fn initialize_project(
    admin: &mut sqlx::PgConnection,
    migration_input: &str,
    sources: &[Migration],
) -> anyhow::Result<usize> {
    let present: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT FROM pg_event_trigger WHERE evtname='wamn_migration_privileges')",
    )
    .fetch_one(&mut *admin)
    .await?;
    if !present {
        sqlx::raw_sql(wamn_control_provision::MIGRATION_PRIVILEGES_SQL)
            .execute(admin)
            .await?;
    }
    restore_project(migration_input, sources).await
}
pub async fn restore_project(
    migration_input: &str,
    sources: &[Migration],
) -> anyhow::Result<usize> {
    let mut connection = wamn_postgres::connect(migration_input).await?;
    apply_target(&mut connection, MigrationDatabase::Project, sources).await
}

pub async fn run(
    client: &mut sqlx::PgConnection,
    target: MigrationTarget<'_>,
    sources: &[Migration],
    apply: bool,
    mut output: impl std::io::Write,
) -> anyhow::Result<()> {
    let database = target.database();
    database.require_login(client).await?;
    let report = wamn_package_control::schema_baseline::observe(client, database, sources).await?;
    write!(output, "{report}")?;
    output.flush()?;
    if apply {
        apply_reported(client, target, sources).await?;
        write!(
            output,
            "{}",
            wamn_package_control::schema_baseline::observe(client, database, sources).await?
        )?;
        output.flush()?;
    }
    Ok(())
}

async fn apply_target(
    client: &mut sqlx::PgConnection,
    database: MigrationDatabase,
    sources: &[Migration],
) -> anyhow::Result<usize> {
    let (schema, owner, lock) = match database {
        MigrationDatabase::System => (
            "registry",
            "wamn_system",
            "SELECT pg_advisory_xact_lock(hashtextextended('wamn.platform.system-schema:' || current_database(), 0))",
        ),
        MigrationDatabase::Project => (
            "platform",
            "wamn_db_owner",
            wamn_package_control::apply_package::LOCK_SQL,
        ),
    };
    let login = database.require_login(client).await?;
    let key = wamn_package_control::migrations::lock_session(client, lock, &[]).await?;
    let result = async {
        let component = wamn_control_provision::PlatformComponent::Provisioning;
        sqlx::query(
            "SELECT set_config('app.user_id',$1,false), set_config('app.operation',$2,false)",
        )
        .bind(component.principal_id().to_string())
        .bind(component.principal_name())
        .execute(&mut *client)
        .await?;
        let table = format!("{schema}._sqlx_migrations");
        wamn_control_provision::migration::prepare_history(client, &table, &login).await?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "ALTER TABLE {table} OWNER TO {owner}"
        )))
        .execute(&mut *client)
        .await?;
        if database == MigrationDatabase::System {
            sqlx::raw_sql("GRANT SELECT ON registry._sqlx_migrations TO wamn_apply")
                .execute(&mut *client)
                .await?;
        }
        let migrator = Migrator::with_migrations(sources.to_vec());
        wamn_package_control::migrations::run(client, migrator, table).await
    }
    .await;
    // Connections are dedicated. A cancellation closes them; a completed call releases exclusion.
    let unlocked: bool = sqlx::query_scalar("SELECT pg_advisory_unlock($1)")
        .bind(key)
        .fetch_one(client)
        .await?;
    ensure!(unlocked, "platform schema session lock disappeared");
    result
}

/// The reset caller keeps its recorded platform substrate until S6-B unifies reset.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct ProjectReset {
    pub initialization_sha256: String,
    pub applied: Vec<wamn_package_control::schema_baseline::MigrationFile>,
}
impl ProjectReset {
    pub async fn read(
        client: &mut sqlx::PgConnection,
        sources: &[Migration],
    ) -> anyhow::Result<Self> {
        let report = wamn_package_control::schema_baseline::observe(
            client,
            MigrationDatabase::Project,
            sources,
        )
        .await?;
        ensure!(
            report.missing.is_empty() && report.warnings.is_empty(),
            "project SQLx history does not match this image"
        );
        Ok(Self {
            initialization_sha256: report
                .initialization
                .context("project initialization is absent")?
                .sha256,
            applied: report.installed,
        })
    }
    pub fn validate(&self, sources: &[Migration]) -> anyhow::Result<()> {
        ensure!(
            self.initialization_sha256 == wamn_control_provision::schema_baseline::project_sha256(),
            "project initialization differs"
        );
        for applied in &self.applied {
            ensure!(
                sources
                    .iter()
                    .any(|migration| migration.version == applied.version
                        && hex::encode(&migration.checksum) == applied.checksum),
                "recorded platform migration differs: {}",
                applied.version
            );
        }
        Ok(())
    }
    pub fn sources(&self, sources: &[Migration]) -> anyhow::Result<Vec<Migration>> {
        self.validate(sources)?;
        let selected = sources
            .iter()
            .filter(|source| {
                self.applied
                    .iter()
                    .any(|applied| applied.version == source.version)
            })
            .cloned()
            .collect();
        Ok(selected)
    }
}
