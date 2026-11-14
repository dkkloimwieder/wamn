//! Initialization identities and observations of SQLx platform migration history.

use serde::{Deserialize, Serialize};
use sqlx::migrate::Migration;
use wamn_control_provision::migrations::{self, MigrationDatabase};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitializationIdentity {
    pub sha256: String,
    pub installed_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationFile {
    pub version: i64,
    pub checksum: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SchemaReport {
    pub initialization: Option<InitializationIdentity>,
    pub installed: Vec<MigrationFile>,
    pub pending: Vec<MigrationFile>,
    pub missing: Vec<MigrationFile>,
    pub warnings: Vec<String>,
}

pub async fn record_system(client: &mut sqlx::PgConnection) -> anyhow::Result<()> {
    record(
        client,
        "registry.schema_baseline",
        &wamn_control_provision::schema_baseline::system_sha256(),
    )
    .await
}
pub async fn record_project(client: &mut sqlx::PgConnection) -> anyhow::Result<()> {
    record(
        client,
        "platform.schema_baseline",
        &wamn_control_provision::schema_baseline::project_sha256(),
    )
    .await
}
async fn record(client: &mut sqlx::PgConnection, table: &str, digest: &str) -> anyhow::Result<()> {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO {table} (sha256) VALUES ($1)"
    )))
    .bind(digest)
    .execute(client)
    .await?;
    Ok(())
}

/// Read native embedded SQLx files without another build-time embedder.
pub fn embedded_sources(target: MigrationDatabase) -> Vec<Migration> {
    migrations::embedded(target).iter().cloned().collect()
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

pub async fn observe(
    client: &mut sqlx::PgConnection,
    target: MigrationDatabase,
    sources: &[Migration],
) -> anyhow::Result<SchemaReport> {
    use sqlx::migrate::Migrate as _;
    let schema = match target {
        MigrationDatabase::System => "registry",
        MigrationDatabase::Project => "platform",
    };
    let baseline = format!("{schema}.schema_baseline");
    let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
        .bind(&baseline)
        .fetch_one(&mut *client)
        .await?;
    let initialization = if exists {
        sqlx::query_as::<_, (String, String)>(sqlx::AssertSqlSafe(format!(
            "SELECT sha256, installed_at::text FROM {baseline} WHERE singleton"
        )))
        .fetch_optional(&mut *client)
        .await?
        .map(|(sha256, installed_at)| InitializationIdentity {
            sha256,
            installed_at,
        })
    } else {
        None
    };
    let table = format!("{schema}._sqlx_migrations");
    let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
        .bind(&table)
        .fetch_one(&mut *client)
        .await?;
    let applied = if exists {
        client.list_applied_migrations(&table).await?
    } else {
        Vec::new()
    };
    let installed = applied
        .iter()
        .map(|migration| MigrationFile {
            version: migration.version,
            checksum: hex::encode(&migration.checksum),
        })
        .collect();
    let pending = sources
        .iter()
        .filter(|migration| !applied.iter().any(|row| row.version == migration.version))
        .map(|migration| MigrationFile {
            version: migration.version,
            checksum: hex::encode(&migration.checksum),
        })
        .collect();
    let missing = applied
        .iter()
        .filter(|migration| {
            !sources
                .iter()
                .any(|source| source.version == migration.version)
        })
        .map(|migration| MigrationFile {
            version: migration.version,
            checksum: hex::encode(&migration.checksum),
        })
        .collect();
    let mut warnings = Vec::new();
    if exists && let Some(version) = client.dirty_version(&table).await? {
        warnings.push(format!("SQLx migration {version} is incomplete"));
    }
    for migration in sources {
        if applied
            .iter()
            .any(|row| row.version == migration.version && row.checksum != migration.checksum)
        {
            warnings.push(format!(
                "SQLx migration {} checksum differs",
                migration.version
            ));
        }
    }
    Ok(SchemaReport {
        initialization,
        installed,
        pending,
        missing,
        warnings,
    })
}

impl std::fmt::Display for SchemaReport {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(identity) = &self.initialization {
            writeln!(
                output,
                "initialization: {} installed_at={}",
                identity.sha256, identity.installed_at
            )?;
        }
        for (label, files) in [
            ("installed", &self.installed),
            ("pending", &self.pending),
            ("missing", &self.missing),
        ] {
            writeln!(output, "{label} migrations:")?;
            for file in files {
                writeln!(output, "  {} {}", file.version, file.checksum)?;
            }
        }
        for warning in &self.warnings {
            writeln!(output, "warning: {warning}")?;
        }
        Ok(())
    }
}
