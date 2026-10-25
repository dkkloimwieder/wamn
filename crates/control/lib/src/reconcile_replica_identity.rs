//! Reconcile per-model PostgreSQL REPLICA IDENTITY from captured schemas.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::Context as _;
use tokio_postgres::NoTls;
use wamn_schema_control::{
    ManagedModel, ReplicaIdentity, ReplicaIdentityPlan, plan_package_migrations,
    reconcile_replica_identity, select_replica_identity_sql,
};

/// Inputs of one replica identity reconciliation for a package.
#[derive(Debug)]
pub struct ReconcileReplicaIdentityRequest {
    /// Superuser connection to the project database.
    pub admin_database_url: String,

    /// Package root whose strict manifest maps model keys to physical tables.
    pub package: PathBuf,

    /// Plan without applying.
    pub dry_run: bool,
}

/// Reconcile the replica identity of one package's models and return the plan.
pub async fn reconcile_package_replica_identity(
    request: ReconcileReplicaIdentityRequest,
) -> anyhow::Result<ReplicaIdentityPlan> {
    let directory = crate::apply_package::read_package_directory(&request.package)?;
    let package =
        plan_package_migrations(&directory, None).context("derive package model mapping")?;

    let (client, connection) = tokio_postgres::connect(&request.admin_database_url, NoTls)
        .await
        .context("connect to project database")?;
    let connection_task = tokio::spawn(connection);
    let result = reconcile(&client, &package.models, !request.dry_run).await;
    drop(client);
    if result.is_err() {
        connection_task.abort();
    } else {
        connection_task
            .await
            .context("join replica-identity database connection")?
            .context("drive replica-identity database connection")?;
    }
    result
}

pub async fn reconcile(
    client: &tokio_postgres::Client,
    models: &[ManagedModel],
    apply: bool,
) -> anyhow::Result<ReplicaIdentityPlan> {
    let schemas = models
        .iter()
        .map(|model| model.schema.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let current = read_current_identities(client, &schemas).await?;
    let captured_schemas = client.query("SELECT DISTINCT n.nspname::text FROM pg_publication_namespace p JOIN pg_namespace n ON n.oid = p.pnnspid", &[]).await.context("read captured schemas for replica identity")?.iter().map(|row| row.get(0)).collect::<BTreeSet<String>>();
    let plan = reconcile_replica_identity(models, &current, &captured_schemas);
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
