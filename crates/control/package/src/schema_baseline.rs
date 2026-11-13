//! Record initialization identities and observe image-owned SQL history.

pub(super) mod plan;

use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};
use tokio_postgres::GenericClient;
use wamn_control_provision::schema_baseline::{project_sha256, system_sha256};
use wamn_control_provision::schema_migrations::{self, MigrationDatabase};
use wamn_schema_control::{MigrationOutcome, MigrationSource, RecordedMigration};

/// The immutable initialization digest and the database's creation identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitializationIdentity {
    pub sha256: String,
    pub installed_at: String,
}

/// One exact SQL file, independent of executable selections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationFile {
    pub ordinal: u32,
    pub relative_path: String,
    pub sha256: String,
}

/// Installed database facts and files pending for the selected image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SchemaReport {
    pub initialization: Option<InitializationIdentity>,
    pub installed: Vec<MigrationFile>,
    pub pending: Vec<MigrationFile>,
    pub uncertain: Vec<MigrationFile>,
    pub missing: Vec<MigrationFile>,
    pub warnings: Vec<String>,
}

/// Read-only adoption and file plan for an image.
#[derive(Debug)]
pub struct SchemaPlan {
    pub initialization: InitializationIdentity,
    pub adoption: bool,
    pub migrations: wamn_schema_control::migrations::MigrationPlan,
    pub recorded: Vec<RecordedMigration>,
    pub missing: Option<wamn_schema_control::PackageMigrationError>,
}

/// Record the system initialization schema in the caller's transaction or session.
pub async fn record_system(client: &impl GenericClient) -> anyhow::Result<()> {
    record(client, "registry.schema_baseline", &system_sha256()).await
}

/// Record the project initialization schema in the caller's transaction or session.
pub async fn record_project(client: &impl GenericClient) -> anyhow::Result<()> {
    record(client, "platform.schema_baseline", &project_sha256()).await
}

async fn record(client: &impl GenericClient, table: &str, sha256: &str) -> anyhow::Result<()> {
    client
        .execute(
            &format!("INSERT INTO {table} (sha256) VALUES ($1)"),
            &[&sha256],
        )
        .await
        .with_context(|| format!("record {table}"))?;
    Ok(())
}

/// Read the image's embedded directory listing.
pub fn embedded_sources(target: MigrationDatabase) -> Vec<MigrationSource> {
    let files = match target {
        MigrationDatabase::System => schema_migrations::SYSTEM_MIGRATIONS,
        MigrationDatabase::Project => schema_migrations::PROJECT_MIGRATIONS,
    };
    files
        .iter()
        .map(|(id, bytes)| MigrationSource {
            relative_path: (*id).into(),
            bytes: bytes.to_vec(),
        })
        .collect()
}

/// Observe system SQL independently of application and service selections.
pub async fn observe_system(client: &impl GenericClient) -> anyhow::Result<SchemaReport> {
    observe(
        client,
        MigrationDatabase::System,
        &embedded_sources(MigrationDatabase::System),
    )
    .await
}

/// Observe project SQL independently of application selections.
pub async fn observe_project(client: &impl GenericClient) -> anyhow::Result<SchemaReport> {
    observe(
        client,
        MigrationDatabase::Project,
        &embedded_sources(MigrationDatabase::Project),
    )
    .await
}

fn target_names(target: MigrationDatabase) -> (&'static str, &'static str, String) {
    match target {
        MigrationDatabase::System => ("system", "registry", system_sha256()),
        MigrationDatabase::Project => ("project", "platform", project_sha256()),
    }
}

async fn initialization(
    client: &impl GenericClient,
    schema: &str,
) -> anyhow::Result<Option<InitializationIdentity>> {
    let table = format!("{schema}.schema_baseline");
    let exists: bool = client
        .query_one("SELECT to_regclass($1) IS NOT NULL", &[&table])
        .await?
        .get(0);
    if !exists {
        return Ok(None);
    }
    Ok(client
        .query_opt(
            &format!("SELECT sha256, installed_at::text FROM {table} WHERE singleton"),
            &[],
        )
        .await
        .with_context(|| format!("read {table}"))?
        .map(|row| InitializationIdentity {
            sha256: row.get(0),
            installed_at: row.get(1),
        }))
}

/// Compare recorded state with explicit image files without writing SQL.
pub async fn observe(
    client: &impl GenericClient,
    target: MigrationDatabase,
    sources: &[MigrationSource],
) -> anyhow::Result<SchemaReport> {
    let (_, schema, _) = target_names(target);
    let initialization = initialization(client, schema).await?;
    let (_, recorded) = recorded(client, schema).await?;
    Ok(plan::observe(
        target,
        initialization.as_ref(),
        &recorded,
        sources,
    ))
}

/// Plan files through the shared prefix planner, including read-only adoption.
pub async fn plan(
    client: &impl GenericClient,
    target: MigrationDatabase,
    sources: &[MigrationSource],
) -> anyhow::Result<SchemaPlan> {
    let (label, schema, candidate) = target_names(target);
    let initialization = initialization(client, schema)
        .await?
        .context("initialization identity is absent")?;
    ensure!(
        initialization.sha256 == candidate,
        "BaselineMismatch: installed={} candidate={candidate}",
        initialization.sha256
    );
    let (adoption, recorded) = recorded(client, schema).await?;
    let (migrations, missing) =
        plan::migrations(sources, &recorded, &format!("migrations/{label}/"))?;
    Ok(SchemaPlan {
        initialization,
        adoption,
        migrations,
        recorded,
        missing,
    })
}

async fn recorded(
    client: &impl GenericClient,
    schema: &str,
) -> anyhow::Result<(bool, Vec<RecordedMigration>)> {
    let table = format!("{schema}.schema_migration");
    let exists: bool = client
        .query_one("SELECT to_regclass($1) IS NOT NULL", &[&table])
        .await?
        .get(0);
    let mut recorded = Vec::new();
    if exists {
        for row in client
            .query(
                &format!(
                    "SELECT ordinal, relative_path, sha256, outcome FROM {table} ORDER BY ordinal"
                ),
                &[],
            )
            .await?
        {
            let outcome: &str = row.get(3);
            recorded.push(RecordedMigration {
                ordinal: u32::try_from(row.get::<_, i32>(0))?,
                relative_path: row.get(1),
                sha256: row.get(2),
                outcome: match outcome {
                    "started" => MigrationOutcome::Started,
                    "applied" => MigrationOutcome::Applied,
                    _ => anyhow::bail!("unsupported schema migration outcome: {outcome}"),
                },
            });
        }
    }
    Ok((!exists, recorded))
}

/// Report recorded facts and image differences without execution refusals.
pub fn report(
    target: MigrationDatabase,
    planned: &SchemaPlan,
    sources: &[MigrationSource],
) -> SchemaReport {
    plan::report(target, planned, sources)
}

impl std::fmt::Display for SchemaReport {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(identity) = &self.initialization {
            writeln!(
                output,
                "initialization: {} installed_at={}",
                identity.sha256, identity.installed_at
            )?;
        } else {
            writeln!(output, "initialization: absent")?;
        }
        for (label, files) in [
            ("installed", &self.installed),
            ("pending", &self.pending),
            ("uncertain (started)", &self.uncertain),
        ] {
            writeln!(output, "{label} files:")?;
            for file in files {
                writeln!(output, "  {} {}", file.relative_path, file.sha256)?;
            }
        }
        for warning in &self.warnings {
            writeln!(output, "warning: {warning}")?;
        }
        Ok(())
    }
}
