//! The `upgrade-schema` subcommand of `docs/plan/schema-upgrade.md`: apply
//! the pending platform migrations to one installed database.
//!
//! With `--system-database-url` alone, the verb upgrades the system database.
//! With `--admin-database-url`, it upgrades that project-environment database,
//! and it reads `registry.project_envs` through the system URL to make sure
//! that the database is a registered one. One run is one transaction: it locks
//! the record table, checks the recorded files against the files of the
//! binary, applies the pending suffix in order and records it. A failure
//! leaves the database as it was. Without `--confirm`, the verb prints the
//! pending files and rolls back.

use anyhow::{Context as _, bail, ensure};
use tokio_postgres::{Client, NoTls, Transaction};
use wamn_control_provision::project_env_database_name;
use wamn_control_provision::schema_migrations::{Migration, MigrationTarget};

/// The name of the system database.
const SYSTEM_DATABASE: &str = "wamn_system";

/// Inputs of one `upgrade-schema` run.
#[derive(Debug)]
pub struct UpgradeSchemaRequest {
    /// Superuser URL of the system database.
    pub system_database_url: String,
    /// Superuser URL of one project-environment database. Without it, the
    /// run upgrades the system database.
    pub admin_database_url: Option<String>,
    /// On the first run of a database installed before its record table: the
    /// last file that the database already holds, recorded without running.
    pub baseline: Option<i32>,
    /// Apply. Without it, the run prints the pending files and changes nothing.
    pub confirm: bool,
}

/// Upgrade the database that `request` names with the files of the binary.
pub async fn upgrade_schema(request: &UpgradeSchemaRequest) -> anyhow::Result<()> {
    let target = match request.admin_database_url {
        None => MigrationTarget::System,
        Some(_) => MigrationTarget::Project,
    };
    upgrade_schema_with(request, target.migrations()).await
}

/// [`upgrade_schema`] with the files that the run applies. Tests pass their own.
pub async fn upgrade_schema_with(
    request: &UpgradeSchemaRequest,
    migrations: &[Migration],
) -> anyhow::Result<()> {
    let (mut system, system_task) =
        connect(&request.system_database_url, "the system database").await?;
    let result = async {
        match &request.admin_database_url {
            None => {
                let database = current_database(&system).await?;
                ensure!(
                    database == SYSTEM_DATABASE,
                    "the system URL reaches the database {database}, not {SYSTEM_DATABASE}"
                );
                upgrade(&mut system, MigrationTarget::System, migrations, request).await
            }
            Some(admin_url) => {
                let (mut client, task) = connect(admin_url, "the project database").await?;
                let result = async {
                    let database = current_database(&client).await?;
                    ensure!(
                        registered(&system, &database).await?,
                        "the database {database} is not a project environment of \
                         registry.project_envs"
                    );
                    upgrade(&mut client, MigrationTarget::Project, migrations, request).await
                }
                .await;
                drop(client);
                let _ = task.await;
                result
            }
        }
    }
    .await;
    drop(system);
    let _ = system_task.await;
    result
}

async fn connect(url: &str, name: &str) -> anyhow::Result<(Client, tokio::task::JoinHandle<()>)> {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .with_context(|| format!("connect to {name}"))?;
    let task = tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok((client, task))
}

async fn current_database(client: &Client) -> anyhow::Result<String> {
    Ok(client
        .query_one("SELECT current_database()", &[])
        .await
        .context("read the database name")?
        .get(0))
}

/// Whether `database` is the database of a `registry.project_envs` row.
async fn registered(system: &Client, database: &str) -> anyhow::Result<bool> {
    let rows = system
        .query(
            "SELECT org, project, env, instance_suffix FROM registry.project_envs",
            &[],
        )
        .await
        .context("read registry.project_envs")?;
    Ok(rows.iter().any(|row| {
        project_env_database_name(row.get(0), row.get(1), row.get(2), row.get(3)) == database
    }))
}

async fn upgrade(
    client: &mut Client,
    target: MigrationTarget,
    migrations: &[Migration],
    request: &UpgradeSchemaRequest,
) -> anyhow::Result<()> {
    let transaction = client
        .transaction()
        .await
        .context("open the upgrade transaction")?;
    if target == MigrationTarget::System {
        // The files and the record table belong to the owner of the control store.
        transaction
            .batch_execute("SET LOCAL ROLE wamn_system")
            .await
            .context("assume the control owner")?;
    }
    let table = target.record_table();
    let exists: bool = transaction
        .query_one("SELECT to_regclass($1) IS NOT NULL", &[&table])
        .await
        .with_context(|| format!("look for {table}"))?
        .get(0);
    match (exists, request.baseline) {
        (false, None) => bail!(
            "{table} does not exist; the first run on this database takes --baseline <ordinal>"
        ),
        (false, Some(_)) => {
            transaction
                .batch_execute(target.record_table_sql())
                .await
                .with_context(|| format!("create {table}"))?;
        }
        (true, _) => (),
    }
    transaction
        .batch_execute(&format!("LOCK TABLE {table} IN EXCLUSIVE MODE"))
        .await
        .with_context(|| format!("lock {table}"))?;
    let recorded = transaction
        .query(
            &format!("SELECT ordinal, relative_path, sha256 FROM {table} ORDER BY ordinal"),
            &[],
        )
        .await
        .with_context(|| format!("read {table}"))?;
    let mut held = recorded.len();
    if let Some(baseline) = request.baseline {
        ensure!(
            recorded.is_empty(),
            "--baseline refused: {table} has a row; --baseline is for the first run only"
        );
        let count = usize::try_from(baseline)
            .ok()
            .filter(|count| *count <= migrations.len())
            .with_context(|| {
                format!(
                    "--baseline {baseline} refused: the binary has {} files",
                    migrations.len()
                )
            })?;
        for migration in &migrations[..count] {
            record(&transaction, target, migration).await?;
            println!("baseline {}", migration.relative_path);
        }
        held = count;
    } else {
        for row in &recorded {
            let ordinal: i32 = row.get(0);
            let path: &str = row.get(1);
            let sha256: &str = row.get(2);
            let migration = usize::try_from(ordinal - 1)
                .ok()
                .and_then(|index| migrations.get(index))
                .with_context(|| {
                    format!("{table} records {path}, and the binary has no such file")
                })?;
            ensure!(
                migration.relative_path == path && migration.sha256() == sha256,
                "schema-migration-drift; recorded={path} {sha256}; binary={} {}",
                migration.relative_path,
                migration.sha256()
            );
        }
    }
    let pending = &migrations[held..];
    if pending.is_empty() {
        println!("nothing pending in {table}");
    }
    if !request.confirm {
        for migration in pending {
            println!("pending {}", migration.relative_path);
        }
        transaction.rollback().await.context("roll back the plan")?;
        println!("without --confirm, nothing changed");
        return Ok(());
    }
    for migration in pending {
        transaction
            .batch_execute(migration.sql)
            .await
            .with_context(|| format!("apply {}", migration.relative_path))?;
        record(&transaction, target, migration).await?;
        println!("applied {}", migration.relative_path);
    }
    transaction.commit().await.context("commit the upgrade")?;
    Ok(())
}

async fn record(
    transaction: &Transaction<'_>,
    target: MigrationTarget,
    migration: &Migration,
) -> anyhow::Result<()> {
    transaction
        .execute(
            &target.record_sql(),
            &[
                &migration.ordinal(),
                &migration.relative_path,
                &migration.sha256(),
            ],
        )
        .await
        .with_context(|| format!("record {}", migration.relative_path))?;
    Ok(())
}

/// Record every file of `target` as held, in a fresh install whose full
/// schema files already hold them. Run it where the record table was created.
pub async fn record_fresh_install(
    client: &impl tokio_postgres::GenericClient,
    target: MigrationTarget,
) -> anyhow::Result<()> {
    for migration in target.migrations() {
        client
            .execute(
                &target.record_sql(),
                &[
                    &migration.ordinal(),
                    &migration.relative_path,
                    &migration.sha256(),
                ],
            )
            .await
            .with_context(|| format!("record {}", migration.relative_path))?;
    }
    Ok(())
}
