//! Operator admission of one released wiring under a service identity.

use anyhow::{Context as _, ensure};
use serde_json::Value;
use tokio_postgres::{Client, NoTls};
use wamn_catalog::{ServingManifest, WiringDocument};
use wamn_runtime::plugins::route_authentication::queued_service_caller;
use wamn_schema_control::BareSchemaName;

/// Exact released work and the service principal responsible for its writes.
#[derive(Debug)]
pub struct EnqueueRun {
    pub tenant: String,
    pub environment: String,
    pub package_id: String,
    pub effective_release_id: u32,
    pub wiring_id: String,
    pub wiring_version: u32,
    pub service_principal_id: String,
    pub idempotency_key: String,
    pub input: Value,
}

/// Connect with project-admin authority and admit one run atomically.
pub async fn enqueue_run(
    database_url: &str,
    schema: &BareSchemaName,
    request: &EnqueueRun,
) -> anyhow::Result<String> {
    let (mut client, connection) = tokio_postgres::connect(database_url, NoTls)
        .await
        .context("connect to the automation admission database")?;
    let connection_task = tokio::spawn(connection);
    let result = enqueue(&mut client, schema, request).await;
    drop(client);
    let _ = connection_task.await;
    result
}

/// Persist a run and its queue row together; an identical retry returns its stored id.
pub async fn enqueue(
    client: &mut Client,
    schema: &BareSchemaName,
    request: &EnqueueRun,
) -> anyhow::Result<String> {
    ensure!(!request.tenant.is_empty(), "automation tenant is empty");
    ensure!(
        !request.idempotency_key.is_empty(),
        "automation idempotency key is empty"
    );
    let release_id = i32::try_from(request.effective_release_id)?;
    let wiring_version = i32::try_from(request.wiring_version)?;
    let transaction = client.transaction().await?;
    transaction
        .query_one(
            "SELECT set_config('search_path', $1, true), set_config('app.tenant', $2, true)",
            &[&schema.as_str(), &request.tenant],
        )
        .await?;
    let policy = transaction.query_opt(
        "SELECT durability_class FROM environment_policies WHERE tenant_id=$1 AND expected_environment=$2 FOR SHARE",
        &[&request.tenant, &request.environment],
    ).await?.context("automation environment policy is absent or differs from the request")?;
    let durability: String = policy.try_get(0)?;
    let bytes =
        crate::publish_release::read_release_snapshot(&transaction, &request.tenant, release_id)
            .await?
            .context("automation requires a minted release snapshot")?;
    let (manifest, _) = ServingManifest::from_canonical_bytes(&bytes)?;
    ensure!(
        manifest.release.tenant_id == request.tenant
            && manifest.release.environment == request.environment,
        "automation release scope does not match the request"
    );
    let wiring = manifest
        .workflow
        .wirings
        .iter()
        .find(|wiring| {
            wiring.package_id == request.package_id
                && wiring.wiring_id == request.wiring_id
                && wiring.wiring_version == request.wiring_version
        })
        .context("automation wiring is absent from the released manifest")?;
    let package = manifest
        .release
        .packages
        .iter()
        .find(|package| package.package_id() == request.package_id)
        .context("automation package is absent from the released manifest")?;
    let graph = transaction
        .query_one(
            "SELECT graph_json::text FROM catalog.wirings \
         WHERE tenant_id = $1 AND package_id = $2 AND wiring_id = $3 \
           AND version = $4 AND wiring_hash = $5 AND package_version = $6",
            &[
                &request.tenant,
                &request.package_id,
                &request.wiring_id,
                &wiring_version,
                &wiring.graph_hash.as_str(),
                &package.package_version(),
            ],
        )
        .await?;
    let document =
        WiringDocument::parse(&serde_json::from_str::<Value>(&graph.get::<_, String>(0))?)?;
    ensure!(
        document.wiring_hash() == wiring.graph_hash,
        "automation wiring hash differs from the release"
    );
    let caller =
        queued_service_caller(&transaction, &request.tenant, &request.service_principal_id).await?;
    let input = serde_json::to_string(&request.input)?;
    let inserted = transaction.query_opt(
        "INSERT INTO runs (tenant_id, package_id, effective_release_id, environment, \
             wiring_id, wiring_version, wiring_hash, trigger_source, service_principal_id, \
             idempotency_key, input_json, status, durability_class) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,'automation',$8::text::uuid,$9,$10::text::jsonb,'dispatched',$11) \
         ON CONFLICT (tenant_id, idempotency_key) WHERE idempotency_key IS NOT NULL DO NOTHING \
         RETURNING run_id",
        &[&request.tenant, &request.package_id, &release_id, &request.environment,
          &request.wiring_id, &wiring_version, &wiring.graph_hash.as_str(),
          &caller.principal_id(), &request.idempotency_key, &input, &durability],
    ).await?;
    let run_id = if let Some(row) = inserted {
        let run_id: String = row.try_get(0)?;
        transaction
            .execute(
                "INSERT INTO run_queue (tenant_id, run_id) VALUES ($1, $2)",
                &[&request.tenant, &run_id],
            )
            .await?;
        run_id
    } else {
        transaction
            .query_opt(
                "SELECT run_id FROM runs WHERE tenant_id = $1 AND package_id = $2 \
             AND effective_release_id = $3 AND environment = $4 AND wiring_id = $5 \
             AND wiring_version = $6 AND wiring_hash = $7 AND trigger_source = 'automation' \
             AND service_principal_id = $8::text::uuid AND idempotency_key = $9 \
             AND input_json = $10::text::jsonb",
                &[
                    &request.tenant,
                    &request.package_id,
                    &release_id,
                    &request.environment,
                    &request.wiring_id,
                    &wiring_version,
                    &wiring.graph_hash.as_str(),
                    &caller.principal_id(),
                    &request.idempotency_key,
                    &input,
                ],
            )
            .await?
            .context("automation idempotency key was used for a different request")?
            .try_get(0)?
    };
    transaction.commit().await?;
    Ok(run_id)
}
