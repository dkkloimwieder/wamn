//! Shared immutable release and execution-plan fixtures for production claims.
//!
//! The host-owned production claim resolves exact `ExecutionPlanV2` bytes from
//! the run's immutable release before it grants a lease. These helpers publish
//! that release with plans bound to the loaded execution-host revision and pin
//! fixture runs to its members.
//!
//!   * [`publish_release`] — the canonical catalog storage DDL, dropped and
//!     recreated, then ONE immutable release (catalog header + `flow_artifacts` +
//!     `release_manifests` + `release_flows`) committed as a unit, because the
//!     membership-coherence triggers are DEFERRABLE INITIALLY DEFERRED and hold
//!     only with the whole release present. `catalog` is a single GLOBAL schema
//!     per database (unlike the per-case ephemeral run schemas), so a leftover
//!     release from an earlier gate in the same database would mask a seeding
//!     bug: metricbench buys that hermeticity with a throwaway database, these
//!     gates buy it with the drop.
//!   * [`pin_run`] — pin a seeded run to that release and stamp the trusted
//!     invocation context admission would have written. The principal's
//!     `artifact-digest` is read FROM `catalog.flow_artifacts`, never written as a
//!     literal: `PinnedArtifact::from_storage` verifies the digest and skips an
//!     artifact that disagrees, so a placeholder pins nothing.
//!
//! Each gate keeps its own run-plane DDL, tenant, schema, and run seeding; this
//! module owns only the catalog arc.

use anyhow::Context as _;
use serde_json::{Value, json};
use tokio_postgres::{Client, NoTls};
use wamn_catalog::{
    Artifact, ExecutionConnectionRequirement, ExecutionEffectPolicy, ExecutionNodeId,
    ExecutionPlanBody, ExecutionPlanEdge, ExecutionPlanHeader, ExecutionPlanNode, ExecutionPlanV2,
    ExecutionRuntimeRevision, ExecutionSourceMapEntry, RootTerminalBehavior, execution_bundle_hash,
};
use wamn_flow::node_contract::{Capability, EffectPolicy, NodeInterface};
use wamn_flow::{CallFlowConfig, EntryKind, Flow, MAIN_PORT, RequestConfig};

/// The canonical catalog storage DDL — the same standalone deploy artifact the
/// retained gates lay down, so no gate fixture can drift from the schema of record.
const CATALOG_DDL: &str = include_str!("../../../deploy/sql/catalog-schema.sql");

/// The catalog each gate release is published under, suffixed per release. A
/// release pins at most ONE version of a flow (`catalog.release_flows` is keyed on
/// `(tenant, catalog, catalog_version, flow_id)`) and `catalog.catalogs` admits one
/// `applied` version per (catalog, environment), so repeated versions of one
/// fixture flow get separate catalogs.
const CATALOG_ID_PREFIX: &str = "gate-catalog-";
/// The version every gate catalog is published at.
const CATALOG_VERSION: i32 = 1;
/// The environment those releases are applied in.
const ENVIRONMENT: &str = "gate";

/// One flow artifact, published as a member of the gate's release.
struct Member {
    flow_id: String,
    flow_version: i32,
    artifact_hash: String,
    graph_json: String,
    graph_hash: String,
    execution_bundle_hash: String,
    exact_bytes: Vec<u8>,
}

/// The current interface for a retained standard node or flowrunner-private
/// fixture node.
pub(crate) fn interface(node_type: &str) -> anyhow::Result<NodeInterface> {
    if node_type == "conditional" {
        return Ok(NodeInterface {
            node_type: node_type.to_string(),
            output_ports: vec![MAIN_PORT.to_string()],
            capabilities: Vec::new(),
            connection_requirements: Vec::new(),
            effect_policy: EffectPolicy::Pure,
        });
    }
    if let Some(interface) = wamn_standard_nodes::describe_interface(node_type) {
        return Ok(interface.clone());
    }

    let capabilities = match node_type {
        "http-call" => vec![Capability::HttpEgress],
        "pg-write" => vec![Capability::Postgres],
        other => anyhow::bail!("gate fixture node type {other:?} has no retained interface"),
    };
    Ok(NodeInterface {
        node_type: node_type.to_string(),
        output_ports: vec![MAIN_PORT.to_string()],
        capabilities,
        connection_requirements: Vec::new(),
        effect_policy: EffectPolicy::Effectful,
    })
}

/// The implementations a graph requires: exactly one per DISTINCT node type,
/// ordered by node type (artifact identity demands both). Derived from the graph,
/// so editing a fixture cannot leave a stale interface set behind.
fn implementations(flow: &Flow) -> anyhow::Result<Vec<NodeInterface>> {
    let mut node_types: Vec<&str> = flow
        .nodes
        .iter()
        .map(|node| node.node_type.as_str())
        .collect();
    node_types.sort_unstable();
    node_types.dedup();

    node_types.into_iter().map(interface).collect()
}

/// Build the immutable artifact for one gate fixture graph.
fn compile_execution_plan(
    flow: &Flow,
    root_artifact_hash: &str,
    runtime_revision: ExecutionRuntimeRevision,
) -> anyhow::Result<(String, Vec<u8>)> {
    let entry = flow
        .nodes
        .iter()
        .find(|node| node.entry_kind().is_some())
        .context("validated fixture has no entry node")?;
    let entry_instruction = ExecutionNodeId::new(&entry.id)?;
    let entry_input_schema_guard = match entry.entry_kind() {
        Some(EntryKind::Request) => {
            serde_json::from_value::<RequestConfig>(entry.config.clone())
                .context("validated request entry config is invalid")?
                .input_schema
        }
        Some(EntryKind::Event) => Value::Bool(true),
        None => unreachable!("entry node selected by entry-kind membership"),
    };

    let mut requirements = flow.connection_requirements.iter().collect::<Vec<_>>();
    requirements.sort_by(|left, right| left.name.cmp(&right.name));
    let mut semantic_nodes = flow.nodes.iter().collect::<Vec<_>>();
    semantic_nodes.sort_by(|left, right| left.id.cmp(&right.id));
    let nodes = semantic_nodes
        .iter()
        .map(|node| {
            if node.node_type == "call-flow" {
                let config = serde_json::from_value::<CallFlowConfig>(node.config.clone())
                    .with_context(|| format!("call-flow node {:?} has invalid config", node.id))?;
                return Ok(ExecutionPlanNode {
                    local_node_id: ExecutionNodeId::new(&node.id)?,
                    source_node_id: node.id.clone(),
                    node_type: "call-flow".to_string(),
                    config: json!({"site": node.id.clone(), "flow-id": config.flow_id}),
                    effect_policy: ExecutionEffectPolicy::Effectful,
                    source_connection_requirement: None,
                });
            }
            let resolved_interface = interface(&node.node_type)?;
            let source_connection_requirement = node
                .connection
                .as_ref()
                .map(|name| {
                    let requirement = requirements
                        .binary_search_by(|candidate| candidate.name.as_str().cmp(name.as_str()))
                        .ok()
                        .map(|index| requirements[index])
                        .with_context(|| {
                            format!("node {:?} has unresolved connection {name:?}", node.id)
                        })?;
                    Ok::<_, anyhow::Error>(ExecutionConnectionRequirement {
                        name: name.clone(),
                        descriptor: requirement.requirement.clone(),
                    })
                })
                .transpose()?;
            Ok(ExecutionPlanNode {
                local_node_id: ExecutionNodeId::new(&node.id)?,
                source_node_id: node.id.clone(),
                node_type: node.node_type.clone(),
                config: node.config.clone(),
                effect_policy: match resolved_interface.effect_policy {
                    EffectPolicy::Pure => ExecutionEffectPolicy::Pure,
                    EffectPolicy::Effectful => ExecutionEffectPolicy::Effectful,
                },
                source_connection_requirement,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    let mut semantic_edges = flow.edges.iter().collect::<Vec<_>>();
    semantic_edges.sort_by(|left, right| {
        (
            left.from.as_str(),
            left.from_port.as_str(),
            left.ordinal.unwrap_or(0),
            left.to.as_str(),
            left.to_port.as_deref().unwrap_or(""),
        )
            .cmp(&(
                right.from.as_str(),
                right.from_port.as_str(),
                right.ordinal.unwrap_or(0),
                right.to.as_str(),
                right.to_port.as_deref().unwrap_or(""),
            ))
    });
    let edges = semantic_edges
        .iter()
        .map(|edge| {
            Ok(ExecutionPlanEdge {
                source: ExecutionNodeId::new(&edge.from)?,
                source_port: edge.from_port.clone(),
                destination: ExecutionNodeId::new(&edge.to)?,
                destination_port: edge.to_port.clone(),
                fan_out_ordinal: edge.ordinal.unwrap_or(0),
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let responders = flow
        .nodes
        .iter()
        .filter(|node| node.node_type == "respond")
        .map(|node| ExecutionNodeId::new(&node.id).map_err(anyhow::Error::new))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let root_terminal_behavior = if responders.is_empty() {
        RootTerminalBehavior::FrontierExhaustion
    } else {
        RootTerminalBehavior::Respond { responders }
    };
    let source_map = nodes
        .iter()
        .map(|node| ExecutionSourceMapEntry {
            local_node_id: node.local_node_id.clone(),
            source_node_id: node.source_node_id.clone(),
        })
        .collect();
    let mut body = ExecutionPlanBody {
        entry_instruction,
        nodes,
        edges,
        root_terminal_behavior,
        entry_input_schema_guard,
        callable_contract: None,
        source_map,
    };
    let probe = ExecutionPlanV2 {
        header: ExecutionPlanHeader {
            format_version: wamn_catalog::EXECUTION_PLAN_FORMAT_VERSION.to_string(),
            plan_compiler_revision: wamn_catalog::PLAN_COMPILER_REVISION.to_string(),
            runtime_revision: runtime_revision.clone(),
            root_artifact_hash: root_artifact_hash.to_string(),
        },
        body: body.clone(),
    };
    body.callable_contract = probe.derived_callable_contract()?;
    let plan = ExecutionPlanV2::new(runtime_revision, root_artifact_hash, body)?;
    let exact_bytes = serde_json::to_vec(&plan).context("serialize fixture execution plan")?;
    Ok((execution_bundle_hash(&exact_bytes), exact_bytes))
}

fn member(
    tenant: &str,
    flow_json: &str,
    runtime_revision: ExecutionRuntimeRevision,
) -> anyhow::Result<Member> {
    let flow = Flow::from_json(flow_json)
        .map_err(|error| anyhow::anyhow!("parse gate fixture graph: {error}"))?;
    let flow_version = i32::try_from(flow.version).context("gate fixture flow version")?;
    let artifact = Artifact::new(tenant, &flow, implementations(&flow)?).map_err(|error| {
        anyhow::anyhow!("build the immutable {} artifact: {error}", flow.flow_id)
    })?;
    let artifact_hash = artifact.identity().artifact_hash().as_str().to_string();
    let (execution_bundle_hash, exact_bytes) =
        compile_execution_plan(&flow, &artifact_hash, runtime_revision)?;
    Ok(Member {
        flow_id: flow.flow_id.clone(),
        flow_version,
        artifact_hash,
        graph_json: String::from_utf8(flow.canonical_bytes())
            .expect("canonical flow graph is UTF-8"),
        graph_hash: artifact.graph_hash().to_string(),
        execution_bundle_hash,
        exact_bytes,
    })
}

/// Lay the canonical catalog schema down hermetically. The DDL grants to
/// `wamn_scenario_author`, a NOLOGIN authoring role the gate databases do not
/// otherwise need, so it is ensured first.
async fn apply_catalog_preamble(client: &Client) -> anyhow::Result<()> {
    client
        .batch_execute(
            "DO $$ BEGIN \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_scenario_author') \
                 THEN CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOBYPASSRLS; \
               END IF; \
             END $$;",
        )
        .await
        .context("ensure the wamn_scenario_author role")?;
    client
        .batch_execute("DROP SCHEMA IF EXISTS catalog CASCADE;")
        .await
        .context("drop any leftover catalog schema")?;
    client
        .batch_execute(CATALOG_DDL)
        .await
        .context("apply the canonical catalog storage DDL")?;
    Ok(())
}

/// Publish `flows` as immutable release members on a fresh catalog schema.
/// `admin_url` is the superuser URL each gate already provisions with.
///
/// A release pins at most one version of a flow, so the flows are grouped by
/// flow id and the Nth version of any flow lands in the Nth release. Every
/// release's four rows share a transaction: `release_flows` carries a foreign key
/// to its manifest and the membership-coherence trigger is DEFERRABLE INITIALLY
/// DEFERRED, so a release is coherent only at commit.
pub(crate) async fn publish_release(
    admin_url: &str,
    tenant: &str,
    flows: &[String],
    runtime_revision: &ExecutionRuntimeRevision,
) -> anyhow::Result<()> {
    let mut releases: Vec<Vec<Member>> = Vec::new();
    for flow_json in flows {
        let member = member(tenant, flow_json, runtime_revision.clone())?;
        let slot = releases
            .iter()
            .position(|release: &Vec<Member>| {
                !release.iter().any(|held| held.flow_id == member.flow_id)
            })
            .unwrap_or_else(|| {
                releases.push(Vec::new());
                releases.len() - 1
            });
        releases[slot].push(member);
    }

    let (mut client, connection) = tokio_postgres::connect(admin_url, NoTls)
        .await
        .context("admin connect to publish the gate release")?;
    let connection_task = tokio::spawn(async move {
        let _ = connection.await;
    });
    let result = async {
        apply_catalog_preamble(&client).await?;
        for (index, members) in releases.iter().enumerate() {
            let catalog_id = format!("{CATALOG_ID_PREFIX}{}", index + 1);
            let members_json = Value::Array(
                members
                    .iter()
                    .map(|member| {
                        json!({
                            "flow-id": member.flow_id,
                            "flow-version": member.flow_version,
                            "artifact-hash": member.artifact_hash,
                        })
                    })
                    .collect(),
            );
            let transaction = client.transaction().await?;
            transaction
                .execute(
                    "INSERT INTO catalog.catalogs \
                       (tenant_id,catalog_id,version,environment,schema_version,state,document) \
                     VALUES ($1,$2,$3,$4,'0.1','applied','{}')",
                    &[&tenant, &catalog_id, &CATALOG_VERSION, &ENVIRONMENT],
                )
                .await
                .context("seed the release's catalog header")?;
            for member in members {
                transaction
                    .execute(
                        "INSERT INTO catalog.flow_artifacts \
                           (tenant_id,flow_id,flow_version,schema_version,graph_json,graph_hash, \
                            artifact_hash) \
                         VALUES ($1,$2,$3,'0.1',$4::text::jsonb,$5,$6)",
                        &[
                            &tenant,
                            &member.flow_id,
                            &member.flow_version,
                            &member.graph_json,
                            &member.graph_hash,
                            &member.artifact_hash,
                        ],
                    )
                    .await
                    .with_context(|| format!("publish the {} flow artifact", member.flow_id))?;
            }
            for member in members {
                let byte_length = i32::try_from(member.exact_bytes.len())
                    .context("fixture execution-bundle length exceeds PostgreSQL int")?;
                transaction
                    .execute(
                        "INSERT INTO catalog.execution_bundles \
                           (tenant_id,execution_bundle_hash,format_version,exact_bytes,byte_length) \
                         VALUES ($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING",
                        &[
                            &tenant,
                            &member.execution_bundle_hash,
                            &wamn_catalog::EXECUTION_PLAN_FORMAT_VERSION,
                            &member.exact_bytes,
                            &byte_length,
                        ],
                    )
                    .await
                    .with_context(|| {
                        format!("seed the {} fixture execution bundle", member.flow_id)
                    })?;
            }
            transaction
                .execute(
                    "INSERT INTO catalog.release_manifests \
                       (tenant_id,catalog_id,catalog_version,members_json) VALUES ($1,$2,$3,$4)",
                    &[&tenant, &catalog_id, &CATALOG_VERSION, &members_json],
                )
                .await
                .context("publish the release manifest")?;
            for member in members {
                transaction
                    .execute(
                        "INSERT INTO catalog.release_flows \
                           (tenant_id,catalog_id,catalog_version,flow_id,flow_version, \
                            execution_bundle_hash) \
                         VALUES ($1,$2,$3,$4,$5,$6)",
                        &[
                            &tenant,
                            &catalog_id,
                            &CATALOG_VERSION,
                            &member.flow_id,
                            &member.flow_version,
                            &member.execution_bundle_hash,
                        ],
                    )
                    .await
                    .with_context(|| format!("register {} as a release member", member.flow_id))?;
            }
            transaction
                .commit()
                .await
                .context("commit the immutable gate release")?;
            for member in members {
                println!(
                    "release {catalog_id}@{CATALOG_VERSION} pins {} v{} {}",
                    member.flow_id, member.flow_version, member.artifact_hash
                );
            }
        }
        anyhow::Ok(())
    }
    .await;
    drop(client);
    let _ = connection_task.await;
    result
}

/// Pin a seeded run to the published release and stamp the trusted invocation
/// context admission would have written.
///
/// `client` must already be scoped to the gate's schema + tenant claim: the
/// statement is unqualified and the `runs` predicate reads `app.tenant`. The
/// release the run is pinned to and the principal's `artifact-digest` are BOTH
/// joined out of the published membership, so the pin, the release, and the
/// principal cannot disagree.
pub(crate) async fn pin_run(client: &Client, run_id: &str) -> anyhow::Result<()> {
    let pinned = client
        .execute(
            "UPDATE runs AS r \
                SET catalog_id = rf.catalog_id, catalog_version = rf.catalog_version, \
                    environment = $2, execution_bundle_hash = rf.execution_bundle_hash, \
                    invocation_context = jsonb_build_object( \
                      'version', 1, \
                      'principal', jsonb_build_object( \
                        'tenant-id', r.tenant_id, 'environment', $2::text, \
                        'catalog-id', rf.catalog_id, \
                        'catalog-version', rf.catalog_version::bigint, \
                        'run-id', r.run_id, 'flow-id', r.flow_id, \
                        'flow-version', r.flow_version, \
                        'artifact-digest', a.artifact_hash), \
                      'source', jsonb_build_object('producer', r.trigger_source)) \
               FROM catalog.release_flows AS rf \
               JOIN catalog.flow_artifacts AS a \
                 ON a.tenant_id = rf.tenant_id AND a.flow_id = rf.flow_id \
                AND a.flow_version = rf.flow_version \
              WHERE rf.tenant_id = r.tenant_id AND rf.flow_id = r.flow_id \
                AND rf.flow_version = r.flow_version \
                AND r.tenant_id = current_setting('app.tenant', true) AND r.run_id = $1",
            &[&run_id, &ENVIRONMENT],
        )
        .await
        .context("pin the seeded run to its release member")?;
    anyhow::ensure!(
        pinned == 1,
        "run {run_id} was not release-pinned (no published member for its flow?)"
    );
    Ok(())
}

/// A fixture a gate publishes must be a graph a REAL release could carry:
/// [`Artifact::new`] parses it against the current flow schema and validates it
/// (entry kind, `respond` legality and config, and resolved interfaces). Each
/// gate names this over the fixtures it publishes so an unpublishable edit
/// fails locally instead of at claim time.
#[cfg(test)]
pub(crate) fn assert_releasable(fixture: &str, flow_json: &str) {
    let runtime_revision = ExecutionRuntimeRevision {
        flowrunner_component_digest:
            "sha256:1111111111111111111111111111111111111111111111111111111111111111".to_string(),
        effect_provider_revision:
            "sha256:2222222222222222222222222222222222222222222222222222222222222222".to_string(),
        host_effect_contract_version: wamn_catalog::HOST_EFFECT_CONTRACT_VERSION.to_string(),
    };
    if let Err(error) = member("gate-drift-tenant", flow_json, runtime_revision) {
        panic!("{fixture} is not publishable as a release member: {error:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both retained `poc-receipt` versions compile into canonical plans.
    #[test]
    fn the_shared_receipt_fixtures_are_releasable_graphs() {
        assert_releasable("flowbench::flow_json(1)", &crate::flowbench::flow_json(1));
        assert_releasable("flowbench::flow_json(2)", &crate::flowbench::flow_json(2));
    }
}
