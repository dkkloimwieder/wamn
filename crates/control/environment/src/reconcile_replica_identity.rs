//! Reconcile per-model PostgreSQL REPLICA IDENTITY from captured schemas.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Context as _;
use wamn_schema_control::{
    ManagedModel, ReplicaIdentity, ReplicaIdentityPlan, reconcile_replica_identity,
    select_replica_identity_sql,
};

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
