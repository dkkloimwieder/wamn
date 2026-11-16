//! Private worker entry points for the environment chart's retention tasks.

use clap::Args;
use sqlx::Connection as _;
use wamn_control_provision::{WorkloadRoleFamily, WorkloadRoleIdentity};

use crate::Configuration;

#[derive(Debug, Args)]
pub struct Target {
    #[arg(long)]
    project: wamn_registry::ProjectId,
    #[arg(long)]
    environment: wamn_registry::EnvName,
}

#[derive(Debug, Args)]
pub struct Runs {
    #[command(flatten)]
    target: Target,
    #[arg(long)]
    retention_days: u32,
    #[arg(long)]
    dry_run: bool,
}

fn database(
    config: &Configuration,
    target: &Target,
    family: WorkloadRoleFamily,
) -> anyhow::Result<String> {
    wamn_control_provision::validate_project_env(
        &config.org,
        &target.project,
        &target.environment,
    )?;
    let key =
        wamn_control_provision::control_administration_key(&target.project, &target.environment);
    let mut connection: wamn_postgres::ConnectionConfig =
        serde_json::from_slice(&std::fs::read(config.administration_dir.join(key))?)?;
    let project_env_key =
        wamn_registry::project_env_key(&config.org, &target.project, &target.environment);
    connection.username = wamn_control_provision::workload_login(
        family,
        WorkloadRoleIdentity::ProjectEnvKey {
            project_env_key: &project_env_key,
            database: &connection.database,
        },
    )?;
    connection.credentials.name = wamn_control_provision::workload_secret_name(
        family,
        &config.org,
        &target.project,
        &target.environment,
    );
    connection.to_json()
}

pub async fn records(config: Configuration, target: Target) -> anyhow::Result<()> {
    let removed = wamn_environment::prune_record_history::prune_expired_record_history(
        wamn_environment::prune_record_history::PruneRecordHistoryRequest {
            database_url: database(&config, &target, WorkloadRoleFamily::AuditRetention)?,
            org: config.org,
            project: target.project,
            environment: target.environment,
        },
    )
    .await?;
    for table in removed {
        println!(
            "{}",
            serde_json::json!({"schema":table.schema,"history":table.history,"days":table.days,"removed":table.removed})
        );
    }
    Ok(())
}

pub async fn runs(config: Configuration, args: Runs) -> anyhow::Result<()> {
    let removed = wamn_environment::prune_run_history::prune_terminal_run_history(
        wamn_environment::prune_run_history::PruneRunHistoryRequest {
            database_url: database(&config, &args.target, WorkloadRoleFamily::Retention)?,
            org: config.org,
            project: args.target.project,
            environment: args.target.environment,
            schema: "wamn_run".into(),
            retention_days: args.retention_days,
            dry_run: args.dry_run,
        },
    )
    .await?;
    println!("{removed}");
    Ok(())
}

async fn prune_identities(
    input: &str,
    scope: wamn_control_provision::migrations::MigrationDatabase,
) -> anyhow::Result<()> {
    // Bound credential lookup, connection establishment, role selection and delete.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let configuration = scope.configuration(input)?;
        let mut connection = wamn_postgres::connect(&configuration).await?;
        scope.require_login(&mut connection).await?;
        let mut transaction = connection.begin().await?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "SET LOCAL ROLE {}; SET LOCAL statement_timeout = '5s'",
            scope.owner_role()
        )))
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "DELETE FROM platform.tx_identity WHERE xid < pg_snapshot_xmin(pg_current_snapshot())",
        )
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(())
    })
    .await?
}

/// Remove identities that no running transaction can still use.
pub(crate) async fn identities(directory: &std::path::Path, system: &str) -> anyhow::Result<()> {
    use wamn_control_provision::migrations::MigrationDatabase;
    if let Err(error) = prune_identities(system, MigrationDatabase::System).await {
        tracing::warn!(%error, "system transaction identity retention failed");
    }
    let mut entries = tokio::fs::read_dir(directory).await?;
    while let Some(entry) = entries.next_entry().await? {
        if entry.file_name().to_string_lossy().starts_with('.')
            || !entry.file_type().await?.is_file()
        {
            continue;
        }
        let result = async {
            let input = tokio::fs::read_to_string(entry.path()).await?;
            prune_identities(&input, MigrationDatabase::Project).await
        }
        .await;
        if let Err(error) = result {
            tracing::warn!(%error, database = ?entry.file_name(), "transaction identity retention failed");
        }
    }
    Ok(())
}
