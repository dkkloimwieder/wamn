//! Reconcile per-model PostgreSQL REPLICA IDENTITY from package registrations.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::Context as _;
use clap::Args;
use tokio_postgres::NoTls;
use wamn_schema_control::{
    EventRegistration, ManagedModel, ReplicaIdentity, ReplicaIdentityPlan, UnreadableRegistrations,
    UnreadableRegistrationsKind, plan_package_migrations, reconcile_replica_identity,
    select_replica_identity_sql,
};

const SELECT_REGISTRATIONS_SQL: &str = "\
SELECT registration::text FROM catalog.event_registrations \
 WHERE package_id = $1 ORDER BY tenant_id, registration_id";

#[derive(Debug, Args)]
pub struct ReconcileReplicaIdentityArgs {
    /// Superuser connection to the project database.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: String,

    /// Package root whose strict manifest maps model keys to physical tables.
    #[arg(long)]
    pub package: PathBuf,

    /// Print the plan without applying it.
    #[arg(long)]
    pub dry_run: bool,
}

pub async fn run(args: ReconcileReplicaIdentityArgs) -> anyhow::Result<()> {
    let directory = crate::apply_package::read_package_directory(&args.package)?;
    let package =
        plan_package_migrations(&directory, None).context("derive package model mapping")?;
    let package_id = package.coordinate.package_id().to_owned();

    let (client, connection) = tokio_postgres::connect(&args.admin_database_url, NoTls)
        .await
        .context("connect to project database")?;
    let connection_task = tokio::spawn(connection);
    let result = reconcile(&client, &package_id, &package.models, !args.dry_run).await;
    drop(client);
    if result.is_err() {
        connection_task.abort();
    } else {
        connection_task
            .await
            .context("join replica-identity database connection")?
            .context("drive replica-identity database connection")?;
    }
    let plan = result?;
    print_plan(&plan, args.dry_run);
    Ok(())
}

pub async fn reconcile(
    client: &tokio_postgres::Client,
    package_id: &str,
    models: &[ManagedModel],
    apply: bool,
) -> anyhow::Result<ReplicaIdentityPlan> {
    let registrations = read_registrations(client, package_id).await?;
    let schemas = models
        .iter()
        .map(|model| model.schema.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let current = read_current_identities(client, &schemas).await?;
    let plan = reconcile_replica_identity(models, &registrations, &current);
    if apply {
        for flip in &plan.flips {
            client
                .batch_execute(&flip.sql)
                .await
                .with_context(|| format!("apply {}", flip.sql))?;
        }
    }
    Ok(plan)
}

async fn read_registrations(
    client: &tokio_postgres::Client,
    package_id: &str,
) -> anyhow::Result<Vec<EventRegistration>> {
    let table_present: bool = client
        .query_one(
            "SELECT to_regclass('catalog.event_registrations') IS NOT NULL",
            &[],
        )
        .await
        .context("probe catalog.event_registrations")?
        .get(0);
    if !table_present {
        return Err(UnreadableRegistrations {
            kind: UnreadableRegistrationsKind::Absent,
            package_id: package_id.to_owned(),
        }
        .into());
    }
    client
        .batch_execute("SET row_security = off")
        .await
        .context("require a complete cross-tenant registration read")?;
    let read = client.query(SELECT_REGISTRATIONS_SQL, &[&package_id]).await;
    let _ = client.batch_execute("RESET row_security").await;
    let rows = read.map_err(|source| {
        anyhow::Error::new(source).context(UnreadableRegistrations {
            kind: UnreadableRegistrationsKind::Unreadable,
            package_id: package_id.to_owned(),
        })
    })?;
    rows.into_iter()
        .map(|row| {
            let document: String = row.get(0);
            EventRegistration::from_json(&document)
                .with_context(|| format!("parse stored registration document: {document}"))
        })
        .collect()
}

async fn read_current_identities(
    client: &tokio_postgres::Client,
    schemas: &[String],
) -> anyhow::Result<BTreeMap<(String, String), ReplicaIdentity>> {
    let rows = client
        .query(select_replica_identity_sql(), &[&schemas])
        .await
        .context("read pg_class.relreplident")?;
    let mut current = BTreeMap::new();
    for row in rows {
        let schema: String = row.get(0);
        let table: String = row.get(1);
        let identity: String = row.get(2);
        current.insert(
            (schema, table),
            ReplicaIdentity::from_relreplident(identity.chars().next().unwrap_or('d')),
        );
    }
    Ok(current)
}

fn identity_keyword(identity: ReplicaIdentity) -> &'static str {
    match identity {
        ReplicaIdentity::Full => "FULL",
        ReplicaIdentity::Default => "DEFAULT",
    }
}

fn print_plan(plan: &ReplicaIdentityPlan, dry_run: bool) {
    let verb = if dry_run { "would flip" } else { "flipped" };
    if plan.flips.is_empty() {
        println!(
            "replica identity already reconciled: {} model(s) at target",
            plan.unchanged.len()
        );
    }
    for flip in &plan.flips {
        println!(
            "{verb} {}.{} ({}): {} -> {}",
            flip.schema,
            flip.table,
            flip.model_id,
            identity_keyword(flip.from),
            identity_keyword(flip.to)
        );
    }
    for table in &plan.skipped_absent {
        println!("[skip] {table} is absent; apply the package before reconciling");
    }
}
