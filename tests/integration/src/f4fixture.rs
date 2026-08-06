//! Shared authoritative F4 fixture used by both live F4 proof surfaces.

use anyhow::{Context as _, bail};
use serde_json::json;
use sha2::{Digest as _, Sha256};
use tokio_postgres::Client;
use wamn_catalog::{Artifact, NodeImplementation};
use wamn_node_manifest::{
    CapabilityClass, ConnectionTypeDescriptor, ExecutableRecoveryContract,
    PortableConnectionRequirement, ResolvedNodeInterface,
};

pub const FLOW_ID: &str = "disposition-recorded";
pub const REGISTRATION_ID: &str = "r-disp";
pub const ENTITY_ID: &str = "dispositions";
pub const CONNECTION_NAME: &str = "erp-callback";
pub const ERP_CREDENTIAL_HANDLE: &str = "erp-callback";
pub const SIGNING_KEY: &str = "wamn-f4-node-signing-key-66cd77a109d7";
pub const ERP_CREDENTIAL_JSON: &str = r#"{"headers":{}}"#;
const RETENTION_MS: u64 = 86_400_000;

const REGISTER_CATALOG_SQL: &str = "INSERT INTO catalog.catalogs \
   (tenant_id,catalog_id,version,environment,schema_version,state) \
 VALUES ($1,$2,$3,$4,'0.1','staged') \
 ON CONFLICT (tenant_id,catalog_id,version) DO NOTHING";
const REGISTER_ARTIFACT_SQL: &str = "SELECT catalog.register_flow_artifact( \
   $1,$2,$3,'0.1',$4::text::jsonb,$5,$6,$7,$8,$9,$10,$11)";
const REGISTER_MANIFEST_SQL: &str = "SELECT catalog.register_release_manifest($1,$2,$3,$4)";
const REGISTER_FLOW_SQL: &str = "INSERT INTO catalog.release_flows \
   (tenant_id,catalog_id,catalog_version,flow_id,flow_version) \
 VALUES ($1,$2,$3,$4,$3) ON CONFLICT DO NOTHING";
const REGISTER_EXPOSURE_SQL: &str =
    "SELECT catalog.register_release_exposure_manifest($1,$2,$3,'{}')";
const UPSERT_HEAD_SQL: &str = "INSERT INTO catalog.catalog_heads \
   (tenant_id,catalog_id,environment,applied_catalog_version) \
 VALUES ($1,$2,$3,$4) \
 ON CONFLICT (tenant_id,catalog_id,environment) DO UPDATE \
 SET applied_catalog_version=EXCLUDED.applied_catalog_version,updated_at=now()";

#[derive(Debug, Clone)]
pub struct PublishedF4 {
    pub artifact_hash: String,
    pub implementation_digest: String,
}

pub fn implementation_digest(component: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(component)))
}

fn erp_requirement() -> PortableConnectionRequirement {
    PortableConnectionRequirement::stable_key_dedup_v1(
        ConnectionTypeDescriptor::http_v1(),
        RETENTION_MS,
    )
}

/// One portable graph. Executable placement and ERP authority are environment
/// facts and therefore deliberately absent from these bytes.
pub fn graph_json(flow_version: u32) -> String {
    let requirement = erp_requirement();
    json!({
        "schema-version": "0.1",
        "flow-id": FLOW_ID,
        "version": flow_version,
        "name": "F4 disposition-recorded (authoritative gate)",
        "connection-requirements": [{
            "name": CONNECTION_NAME,
            "requirement": requirement,
        }],
        "nodes": [
            { "id": "event", "type": "event" },
            { "id": "capture", "type": "transform",
              "config": { "expression": "@", "ctx": "{disposition: new}" } },
            { "id": "shape", "type": "transform",
              "config": { "expression": "{hold: new, decision: new.decision}" } },
            { "id": "recommend", "type": "custom",
              "label": "F2 disposition recommendation" },
            { "id": "callback", "type": "http-request",
              "label": "POST ERP callback", "connection": CONNECTION_NAME,
              "config": { "method": "POST", "path-and-query": "/dispositions", "body": "@" } }
        ],
        "edges": [
            { "from": "event", "to": "capture" },
            { "from": "capture", "to": "shape" },
            { "from": "shape", "to": "recommend" },
            { "from": "recommend", "to": "callback" }
        ]
    })
    .to_string()
}

pub fn registration_json(catalog_id: &str) -> String {
    json!({
        "schema-version": "0.1",
        "registration-id": REGISTRATION_ID,
        "catalog-id": catalog_id,
        "flow-id": FLOW_ID,
        "entity": ENTITY_ID,
        "ops": ["insert"],
        "condition": null,
        "partition-key": null,
    })
    .to_string()
}

fn implementations(component_digest: &str) -> anyhow::Result<Vec<NodeImplementation>> {
    let mut implementations = ["event", "transform", "http-request"]
        .into_iter()
        .map(|node_type| {
            let descriptor = wamn_standard_nodes::describe(node_type)
                .with_context(|| format!("missing standard-node descriptor for {node_type}"))?;
            let contract =
                wamn_standard_nodes::resolve_descriptor(descriptor).map_err(anyhow::Error::new)?;
            NodeImplementation::from_resolved_platform_contract(contract)
                .map_err(anyhow::Error::new)
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    implementations.push(NodeImplementation::supplied(
        ResolvedNodeInterface::new(
            "custom",
            "wamn:node/node@0.1.0",
            vec!["main".to_string()],
            vec![CapabilityClass::Pure],
            Vec::new(),
        ),
        component_digest,
        ExecutableRecoveryContract::pure(),
    )?);
    implementations
        .sort_by(|left, right| left.interface().node_type.cmp(&right.interface().node_type));
    Ok(implementations)
}

pub fn artifact(
    tenant: &str,
    flow_version: u32,
    component_digest: &str,
) -> anyhow::Result<(wamn_flow::Flow, Artifact)> {
    let flow = wamn_flow::Flow::from_json(&graph_json(flow_version))
        .map_err(|error| anyhow::anyhow!("parse F4 release graph: {error}"))?;
    let artifact = Artifact::new(tenant, &flow, implementations(component_digest)?)
        .map_err(|error| anyhow::anyhow!("build F4 release artifact: {error}"))?;
    Ok((flow, artifact))
}

/// Publish and activate the exact F4 artifact plus its one environment-owned
/// ERP connection. The custom component digest is pinned only in the Artifact.
#[expect(
    clippy::too_many_arguments,
    reason = "fixture publication requires the complete release and environment identity"
)]
pub async fn publish(
    admin: &mut Client,
    tenant: &str,
    catalog_id: &str,
    environment: &str,
    flow_version: u32,
    component_digest: &str,
    erp_authority: &str,
    bind_callback: bool,
) -> anyhow::Result<PublishedF4> {
    let flow_version = i32::try_from(flow_version).context("F4 flow version exceeds i32")?;
    let (flow, artifact) = artifact(tenant, flow_version as u32, component_digest)?;
    let canonical_graph =
        String::from_utf8(flow.canonical_bytes()).expect("canonical F4 graph is UTF-8");
    let interfaces = String::from_utf8(artifact.interface_bundle().canonical_bytes().to_vec())
        .expect("canonical F4 interfaces are UTF-8");
    let components = serde_json::to_value(artifact.supplied_components())?;
    let occurrence_recovery = String::from_utf8(artifact.occurrence_recovery_bytes().to_vec())
        .expect("canonical F4 occurrence recovery is UTF-8");
    let artifact_hash = artifact.identity().artifact_hash().as_str().to_string();
    let connection = flow
        .connection_requirements
        .iter()
        .find(|connection| connection.name == CONNECTION_NAME)
        .context("F4 ERP connection requirement")?;
    let requirement_json = serde_json::to_string(&connection.requirement)?;
    let requirement_hash = wamn_schema_control::connections::ArtifactConnectionRequirement::new(
        artifact_hash.as_str(),
        CONNECTION_NAME,
        connection.requirement.clone(),
    )
    .requirement_hash();
    let instance_id = format!("{catalog_id}-{CONNECTION_NAME}");
    let definition = json!({
        "primary-authority": format!("http://{}/", erp_authority.trim_end_matches('/')),
        "failover-authorities": [],
        "tls-verification": "disabled",
        "tls-names": [],
        "redirect-policy": "same-authority",
        "proxy-transport": null,
        "credential-set-handle": ERP_CREDENTIAL_HANDLE,
    });
    let definition_json = serde_json::to_string(&definition)?;
    let definition_hash = implementation_digest(definition_json.as_bytes());
    let members = json!([{
        "flow-id": FLOW_ID,
        "flow-version": flow_version,
        "artifact-hash": artifact_hash,
    }]);
    let registration = registration_json(catalog_id);

    let transaction = admin.transaction().await?;
    transaction
        .execute(
            REGISTER_CATALOG_SQL,
            &[&tenant, &catalog_id, &flow_version, &environment],
        )
        .await?;
    transaction
        .execute(
            REGISTER_ARTIFACT_SQL,
            &[
                &tenant,
                &FLOW_ID,
                &flow_version,
                &canonical_graph,
                &artifact.graph_hash(),
                &artifact_hash,
                &interfaces,
                &artifact.interface_bundle().hash(),
                &components,
                &occurrence_recovery,
                &artifact.occurrence_recovery_hash(),
            ],
        )
        .await?;
    transaction
        .execute(
            wamn_schema_control::connections::insert_connection_requirement_sql(),
            &[
                &tenant,
                &artifact_hash,
                &CONNECTION_NAME,
                &requirement_json,
                &requirement_hash,
            ],
        )
        .await?;
    transaction
        .execute(
            REGISTER_MANIFEST_SQL,
            &[&tenant, &catalog_id, &flow_version, &members],
        )
        .await?;
    transaction
        .execute(
            REGISTER_FLOW_SQL,
            &[&tenant, &catalog_id, &flow_version, &FLOW_ID],
        )
        .await?;
    transaction
        .execute(
            REGISTER_EXPOSURE_SQL,
            &[&tenant, &catalog_id, &flow_version],
        )
        .await?;
    transaction
        .execute(
            "INSERT INTO catalog.event_registrations \
               (tenant_id,catalog_id,registration_id,flow_id,entity_id,registration) \
             VALUES ($1,$2,$3,$4,$5,$6::text::jsonb) ON CONFLICT DO NOTHING",
            &[
                &tenant,
                &catalog_id,
                &REGISTRATION_ID,
                &FLOW_ID,
                &ENTITY_ID,
                &registration,
            ],
        )
        .await?;
    transaction
        .execute(
            "INSERT INTO catalog.connection_instances \
               (tenant_id,environment,instance_id,requirement_type,contract) \
             VALUES ($1,$2,$3,'http','wamn:connection/http@0.1.0') \
             ON CONFLICT (tenant_id,environment,instance_id) DO NOTHING",
            &[&tenant, &environment, &instance_id],
        )
        .await?;
    transaction
        .execute(
            "INSERT INTO catalog.connection_generations \
               (tenant_id,environment,instance_id,generation,definition_json,definition_hash,credential_set_handle) \
             VALUES ($1,$2,$3,1,$4::text::jsonb,$5,$6) \
             ON CONFLICT (tenant_id,environment,instance_id,generation) DO NOTHING",
            &[
                &tenant,
                &environment,
                &instance_id,
                &definition_json,
                &definition_hash,
                &ERP_CREDENTIAL_HANDLE,
            ],
        )
        .await?;
    let generation_matches: bool = transaction
        .query_one(
            "SELECT definition_json=$4::text::jsonb AND definition_hash=$5 \
                    AND credential_set_handle=$6 \
               FROM catalog.connection_generations \
              WHERE tenant_id=$1 AND environment=$2 AND instance_id=$3 AND generation=1",
            &[
                &tenant,
                &environment,
                &instance_id,
                &definition_json,
                &definition_hash,
                &ERP_CREDENTIAL_HANDLE,
            ],
        )
        .await?
        .get(0);
    if !generation_matches {
        bail!("existing F4 connection generation differs from the requested target");
    }
    transaction
        .execute(
            "UPDATE catalog.connection_instances \
                SET lifecycle_status='enabled',active_generation=1,revision=revision+1, \
                    updated_at=GREATEST(clock_timestamp(),updated_at+interval '1 microsecond') \
              WHERE tenant_id=$1 AND environment=$2 AND instance_id=$3 \
                AND (lifecycle_status<>'enabled' OR active_generation IS DISTINCT FROM 1)",
            &[&tenant, &environment, &instance_id],
        )
        .await?;
    if bind_callback {
        let binding_hash = implementation_digest(
            format!("{catalog_id}:{environment}:{artifact_hash}:{CONNECTION_NAME}").as_bytes(),
        );
        transaction
            .execute(
                "INSERT INTO catalog.connection_bindings \
               (tenant_id,catalog_id,catalog_version,artifact_hash,requirement_name, \
                environment,instance_id,binding_status,validation_status,validation_hash) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,'active','valid',$8) \
             ON CONFLICT (tenant_id,catalog_id,catalog_version,artifact_hash,requirement_name) \
             DO NOTHING",
                &[
                    &tenant,
                    &catalog_id,
                    &flow_version,
                    &artifact_hash,
                    &CONNECTION_NAME,
                    &environment,
                    &instance_id,
                    &binding_hash,
                ],
            )
            .await?;
    }
    transaction
        .execute(
            "UPDATE catalog.catalogs SET state='superseded' \
             WHERE tenant_id=$1 AND catalog_id=$2 AND environment=$3 \
               AND version<>$4 AND state='applied'",
            &[&tenant, &catalog_id, &environment, &flow_version],
        )
        .await?;
    transaction
        .execute(
            "UPDATE catalog.catalogs SET state='applied' \
             WHERE tenant_id=$1 AND catalog_id=$2 AND version=$3",
            &[&tenant, &catalog_id, &flow_version],
        )
        .await?;
    transaction
        .execute(
            UPSERT_HEAD_SQL,
            &[&tenant, &catalog_id, &environment, &flow_version],
        )
        .await?;
    transaction.commit().await?;

    Ok(PublishedF4 {
        artifact_hash,
        implementation_digest: component_digest.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wamn_node_manifest::ExecutableIdentity;

    #[test]
    fn graph_is_portable_and_artifact_pins_custom_bytes() {
        let digest = format!("sha256:{}", "1".repeat(64));
        let (flow, artifact) = artifact("tenant", 1, &digest).expect("F4 artifact builds");
        assert_eq!(flow.connection_requirements.len(), 1);
        assert!(flow.allowed_hosts.is_empty());
        assert!(!graph_json(1).contains(&digest));
        assert_eq!(artifact.supplied_components().len(), 1);
        assert!(matches!(
            &artifact.supplied_components()[0].contract.executable,
            ExecutableIdentity::Component { digest: pinned } if pinned == &digest
        ));
    }
}
