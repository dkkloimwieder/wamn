//! Pure release-bound call-flow resolution before execution claim grant.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use wamn_catalog::{
    CALLABLE_CONTRACT_VERSION, CallFlowInstruction, CallableEffectCeiling, CallableReturnContract,
    ExecutionRuntimeRevision, read_execution_plan,
};

use crate::FailKind;

/// The already selected and locked run pins resolution must obey.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedRunResolution {
    pub tenant_id: String,
    pub run_id: String,
    pub root_flow_id: String,
    pub catalog_id: String,
    pub catalog_version: i32,
    pub environment: String,
    pub execution_bundle_hash: String,
}

/// One immutable release member with its exact execution-plan bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogResolutionPlan {
    pub flow_id: String,
    pub execution_bundle_hash: String,
    pub source_artifact_hash: String,
    pub exact_bytes: Vec<u8>,
}

/// The narrowed draft/test candidate override for exactly one flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidatePlanOverride {
    pub flow_id: String,
    pub execution_bundle_hash: String,
    pub draft_artifact_hash: String,
    pub source_artifact_hash: String,
    pub exact_bytes: Vec<u8>,
}

/// A verified release binding for one artifact-local connection requirement.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct BoundConnectionRequirement {
    pub source_artifact_hash: String,
    pub requirement_name: String,
    pub requirement_type: String,
    pub contract: String,
}

/// One durable row of `wamn_run.run_flow_resolutions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct RunFlowResolution {
    pub tenant_id: String,
    pub run_id: String,
    pub flow_id: String,
    pub execution_bundle_hash: String,
    pub source_artifact_hash: String,
}

/// A typed .4.11 claim refusal produced before guest execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowResolutionRefusal {
    pub kind: FailKind,
    pub flow_id: String,
    pub reason: String,
}

struct ResolvedPlan {
    row: CatalogResolutionPlan,
    plan: wamn_catalog::ExecutionPlanV2,
}

/// Resolve the finite reachable call-flow name set in the run's pinned release.
pub fn resolve_run_flow_resolutions(
    run: &PinnedRunResolution,
    release_plans: &[CatalogResolutionPlan],
    bindings: &[BoundConnectionRequirement],
    candidate: Option<&CandidatePlanOverride>,
) -> Result<Vec<RunFlowResolution>, FlowResolutionRefusal> {
    let release = release_plans
        .iter()
        .map(|plan| (plan.flow_id.as_str(), plan))
        .collect::<BTreeMap<_, _>>();
    if release.len() != release_plans.len() {
        return refuse(
            FailKind::ForeignRevision,
            &run.root_flow_id,
            "pinned release contains duplicate flow names",
        );
    }
    if let Some(candidate) = candidate
        && candidate.flow_id != run.root_flow_id
    {
        return refuse(
            FailKind::ForeignRevision,
            &candidate.flow_id,
            "candidate override is only admitted for the run root flow",
        );
    }

    let bound = bindings.iter().cloned().collect::<BTreeSet<_>>();
    let mut visited = BTreeSet::new();
    let mut stack = Vec::new();
    let mut rows = Vec::new();

    let root_plan = resolve_one(&release, candidate, &run.root_flow_id)?;
    if root_plan.row.execution_bundle_hash != run.execution_bundle_hash {
        return refuse(
            FailKind::ForeignRevision,
            &run.root_flow_id,
            "root resolution differs from the run execution-bundle pin",
        );
    }
    let expected_runtime_revision = root_plan.plan.header.runtime_revision.clone();
    verify_plan_revision(&root_plan, &expected_runtime_revision)?;
    stack.push(run.root_flow_id.clone());

    while let Some(flow_id) = stack.pop() {
        if !visited.insert(flow_id.clone()) {
            continue;
        }
        let resolved = resolve_one(&release, candidate, &flow_id)?;
        verify_plan_revision(&resolved, &expected_runtime_revision)?;
        verify_bindings(&resolved, &bound)?;

        for node in &resolved.plan.body.nodes {
            if node.node_type == "call-flow" {
                let instruction = serde_json::from_value::<CallFlowInstruction>(
                    node.config.clone(),
                )
                .map_err(|error| FlowResolutionRefusal {
                    kind: FailKind::IncompatibleContract,
                    flow_id: flow_id.clone(),
                    reason: format!("call-flow config is invalid after plan read: {error}"),
                })?;
                let callee = instruction.flow_id;
                let callee_plan = resolve_one(&release, candidate, &callee)?;
                verify_plan_revision(&callee_plan, &expected_runtime_revision)?;
                verify_callable_contract(&callee, &callee_plan)?;
                stack.push(callee);
            }
        }

        rows.push(RunFlowResolution {
            tenant_id: run.tenant_id.clone(),
            run_id: run.run_id.clone(),
            flow_id,
            execution_bundle_hash: resolved.row.execution_bundle_hash.clone(),
            source_artifact_hash: resolved.row.source_artifact_hash.clone(),
        });
    }

    rows.sort_by(|left, right| left.flow_id.cmp(&right.flow_id));
    Ok(rows)
}

fn resolve_one(
    release: &BTreeMap<&str, &CatalogResolutionPlan>,
    candidate: Option<&CandidatePlanOverride>,
    flow_id: &str,
) -> Result<ResolvedPlan, FlowResolutionRefusal> {
    let row = if let Some(candidate) = candidate.filter(|candidate| candidate.flow_id == flow_id) {
        CatalogResolutionPlan {
            flow_id: candidate.flow_id.clone(),
            execution_bundle_hash: candidate.execution_bundle_hash.clone(),
            source_artifact_hash: candidate.source_artifact_hash.clone(),
            exact_bytes: candidate.exact_bytes.clone(),
        }
    } else {
        release
            .get(flow_id)
            .copied()
            .cloned()
            .ok_or_else(|| FlowResolutionRefusal {
                kind: FailKind::UnresolvableName,
                flow_id: flow_id.to_string(),
                reason: "flow is absent from pinned release".to_string(),
            })?
    };

    let plan =
        read_execution_plan(&row.execution_bundle_hash, &row.exact_bytes).map_err(|error| {
            FlowResolutionRefusal {
                kind: if matches!(
                    &error,
                    wamn_catalog::CatalogIdentityError::ExecutionBundleHashMismatch
                ) {
                    FailKind::HashInvalidBytes
                } else {
                    FailKind::IncompatibleContract
                },
                flow_id: flow_id.to_string(),
                reason: error.to_string(),
            }
        })?;
    let expected_artifact_hash = candidate
        .filter(|candidate| candidate.flow_id == flow_id)
        .map_or(row.source_artifact_hash.as_str(), |candidate| {
            candidate.draft_artifact_hash.as_str()
        });
    if plan.header.root_artifact_hash != expected_artifact_hash {
        return refuse(
            FailKind::ForeignRevision,
            flow_id,
            "plan root artifact hash differs from the resolved source artifact",
        );
    }
    Ok(ResolvedPlan { row, plan })
}

fn verify_plan_revision(
    resolved: &ResolvedPlan,
    expected_runtime_revision: &ExecutionRuntimeRevision,
) -> Result<(), FlowResolutionRefusal> {
    if &resolved.plan.header.runtime_revision != expected_runtime_revision {
        return refuse(
            FailKind::ForeignRevision,
            &resolved.row.flow_id,
            "plan runtime revision differs from the root plan revision",
        );
    }
    Ok(())
}

fn verify_callable_contract(
    flow_id: &str,
    resolved: &ResolvedPlan,
) -> Result<(), FlowResolutionRefusal> {
    let Some(contract) = &resolved.plan.body.callable_contract else {
        return refuse(
            FailKind::IncompatibleContract,
            flow_id,
            "callee plan is not intrinsically callable",
        );
    };
    if contract.version != CALLABLE_CONTRACT_VERSION
        || contract.return_contract != CallableReturnContract::UntypedJsonBody
        || contract.effect_ceiling != CallableEffectCeiling::Effectful
    {
        return refuse(
            FailKind::IncompatibleContract,
            flow_id,
            "callee callable contract is not the MVP 0.1 contract",
        );
    }
    Ok(())
}

fn verify_bindings(
    resolved: &ResolvedPlan,
    bindings: &BTreeSet<BoundConnectionRequirement>,
) -> Result<(), FlowResolutionRefusal> {
    for node in &resolved.plan.body.nodes {
        let Some(requirement) = &node.source_connection_requirement else {
            continue;
        };
        let expected = BoundConnectionRequirement {
            source_artifact_hash: resolved.row.source_artifact_hash.clone(),
            requirement_name: requirement.name.clone(),
            requirement_type: requirement.descriptor.requirement_type.clone(),
            contract: requirement.descriptor.contract.clone(),
        };
        if !bindings.contains(&expected) {
            return refuse(
                FailKind::UnboundRequirement,
                &resolved.row.flow_id,
                format!("connection requirement {:?} is not bound", requirement.name),
            );
        }
    }
    Ok(())
}

fn refuse<T>(
    kind: FailKind,
    flow_id: &str,
    reason: impl Into<String>,
) -> Result<T, FlowResolutionRefusal> {
    Err(FlowResolutionRefusal {
        kind,
        flow_id: flow_id.to_string(),
        reason: reason.into(),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use wamn_catalog::{
        CALLABLE_CONTRACT_VERSION, CallableContract, ExecutionEffectPolicy, ExecutionNodeId,
        ExecutionPlanBody, ExecutionPlanEdge, ExecutionPlanNode, ExecutionPlanV2,
        HOST_EFFECT_CONTRACT_VERSION, RootTerminalBehavior, entry_input_schema_hash,
        execution_bundle_hash,
    };
    use wamn_node_manifest::ConnectionTypeDescriptor;

    use super::*;

    const ART_ROOT: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const ART_CALLEE: &str =
        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const ART_PARTNER: &str =
        "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
    const ART_BINDING_BASE: &str =
        "sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
    const REV_FLOWRUNNER: &str =
        "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
    const REV_EFFECT: &str =
        "sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

    fn node_id(value: &str) -> ExecutionNodeId {
        ExecutionNodeId::new(value).unwrap()
    }

    fn runtime_revision() -> ExecutionRuntimeRevision {
        ExecutionRuntimeRevision {
            flowrunner_component_digest: REV_FLOWRUNNER.into(),
            effect_provider_revision: REV_EFFECT.into(),
            host_effect_contract_version: HOST_EFFECT_CONTRACT_VERSION.into(),
        }
    }

    fn request_node(id: &str) -> ExecutionPlanNode {
        let guard = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object"
        });
        ExecutionPlanNode {
            local_node_id: node_id(id),
            source_node_id: id.into(),
            node_type: "request".into(),
            config: json!({"input-schema": guard}),
            effect_policy: ExecutionEffectPolicy::Pure,
            source_connection_requirement: None,
        }
    }

    fn respond_node(id: &str) -> ExecutionPlanNode {
        ExecutionPlanNode {
            local_node_id: node_id(id),
            source_node_id: id.into(),
            node_type: "respond".into(),
            config: json!({"status": 200}),
            effect_policy: ExecutionEffectPolicy::Pure,
            source_connection_requirement: None,
        }
    }

    fn call_node(id: &str, flow_id: &str) -> ExecutionPlanNode {
        ExecutionPlanNode {
            local_node_id: node_id(id),
            source_node_id: id.into(),
            node_type: "call-flow".into(),
            config: json!({"site": id, "flow-id": flow_id}),
            effect_policy: ExecutionEffectPolicy::Effectful,
            source_connection_requirement: None,
        }
    }

    fn http_node(id: &str, requirement_name: &str) -> ExecutionPlanNode {
        ExecutionPlanNode {
            local_node_id: node_id(id),
            source_node_id: id.into(),
            node_type: "http-call".into(),
            config: json!({}),
            effect_policy: ExecutionEffectPolicy::Effectful,
            source_connection_requirement: Some(wamn_catalog::ExecutionConnectionRequirement {
                name: requirement_name.into(),
                descriptor: ConnectionTypeDescriptor::http_v1(),
            }),
        }
    }

    fn source_map(nodes: &[ExecutionPlanNode]) -> Vec<wamn_catalog::ExecutionSourceMapEntry> {
        nodes
            .iter()
            .map(|node| wamn_catalog::ExecutionSourceMapEntry {
                local_node_id: node.local_node_id.clone(),
                source_node_id: node.source_node_id.clone(),
            })
            .collect()
    }

    fn request_plan(
        artifact_hash: &str,
        extra: Vec<ExecutionPlanNode>,
        mut edges: Vec<ExecutionPlanEdge>,
    ) -> CatalogResolutionPlan {
        let request = request_node("request");
        let respond = respond_node("respond");
        edges.push(ExecutionPlanEdge {
            source: node_id("request"),
            source_port: "main".into(),
            destination: extra
                .first()
                .map_or_else(|| node_id("respond"), |node| node.local_node_id.clone()),
            destination_port: None,
            fan_out_ordinal: 0,
        });
        if let Some(last) = extra.last() {
            edges.push(ExecutionPlanEdge {
                source: last.local_node_id.clone(),
                source_port: "main".into(),
                destination: node_id("respond"),
                destination_port: None,
                fan_out_ordinal: 0,
            });
        }
        let mut nodes = vec![request.clone()];
        nodes.extend(extra);
        nodes.push(respond);
        let guard = request.config["input-schema"].clone();
        let plan = ExecutionPlanV2::new(
            runtime_revision(),
            artifact_hash,
            ExecutionPlanBody {
                entry_instruction: node_id("request"),
                source_map: source_map(&nodes),
                nodes,
                edges,
                root_terminal_behavior: RootTerminalBehavior::Respond {
                    responders: vec![node_id("respond")],
                },
                entry_input_schema_guard: guard.clone(),
                callable_contract: Some(CallableContract {
                    version: CALLABLE_CONTRACT_VERSION.into(),
                    input_schema_hash: entry_input_schema_hash(&guard),
                    return_contract: CallableReturnContract::UntypedJsonBody,
                    effect_ceiling: CallableEffectCeiling::Effectful,
                }),
            },
        )
        .unwrap();
        let exact_bytes = serde_json::to_vec(&plan).unwrap();
        CatalogResolutionPlan {
            flow_id: "unused".into(),
            execution_bundle_hash: execution_bundle_hash(&exact_bytes),
            source_artifact_hash: artifact_hash.into(),
            exact_bytes,
        }
    }

    fn run(root_hash: &str) -> PinnedRunResolution {
        PinnedRunResolution {
            tenant_id: "t1".into(),
            run_id: "run-1".into(),
            root_flow_id: "root".into(),
            catalog_id: "cat".into(),
            catalog_version: 1,
            environment: "prod".into(),
            execution_bundle_hash: root_hash.into(),
        }
    }

    #[test]
    fn complete_resolution_walks_pinned_release_and_terminates_cycles() {
        let mut root = request_plan(ART_ROOT, vec![call_node("call-callee", "callee")], vec![]);
        root.flow_id = "root".into();
        let root_hash = root.execution_bundle_hash.clone();
        let mut callee = request_plan(
            ART_CALLEE,
            vec![
                call_node("call-partner", "partner"),
                http_node("http", "http-main"),
            ],
            vec![ExecutionPlanEdge {
                source: node_id("call-partner"),
                source_port: "main".into(),
                destination: node_id("http"),
                destination_port: None,
                fan_out_ordinal: 0,
            }],
        );
        callee.flow_id = "callee".into();
        let mut partner = request_plan(
            ART_PARTNER,
            vec![call_node("call-callee", "callee")],
            vec![],
        );
        partner.flow_id = "partner".into();
        let rows = resolve_run_flow_resolutions(
            &run(&root_hash),
            &[root, callee, partner],
            &[BoundConnectionRequirement {
                source_artifact_hash: ART_CALLEE.into(),
                requirement_name: "http-main".into(),
                requirement_type: "http".into(),
                contract: "wamn:connection/http@0.1.0".into(),
            }],
            None,
        )
        .unwrap();
        assert_eq!(
            rows.iter()
                .map(|row| row.flow_id.as_str())
                .collect::<Vec<_>>(),
            ["callee", "partner", "root"]
        );
    }

    #[test]
    fn candidate_override_is_only_for_the_flow_under_test() {
        let mut release_root = request_plan(ART_ROOT, vec![call_node("self", "root")], vec![]);
        release_root.flow_id = "root".into();
        let mut candidate_root = request_plan(ART_PARTNER, vec![call_node("self", "root")], vec![]);
        candidate_root.flow_id = "candidate-root".into();
        let candidate_hash = candidate_root.execution_bundle_hash.clone();
        let rows = resolve_run_flow_resolutions(
            &run(&candidate_hash),
            &[release_root],
            &[],
            Some(&CandidatePlanOverride {
                flow_id: "root".into(),
                execution_bundle_hash: candidate_hash.clone(),
                draft_artifact_hash: ART_PARTNER.into(),
                source_artifact_hash: ART_BINDING_BASE.into(),
                exact_bytes: candidate_root.exact_bytes,
            }),
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].execution_bundle_hash, candidate_hash);
        assert_eq!(rows[0].source_artifact_hash, ART_BINDING_BASE);
    }

    #[test]
    fn candidate_override_refuses_non_root_callee_before_resolution() {
        let mut root = request_plan(ART_ROOT, vec![call_node("call-callee", "callee")], vec![]);
        root.flow_id = "root".into();
        let root_hash = root.execution_bundle_hash.clone();
        let mut candidate_callee = request_plan(ART_CALLEE, Vec::new(), Vec::new());
        candidate_callee.flow_id = "callee".into();

        let refusal = resolve_run_flow_resolutions(
            &run(&root_hash),
            &[root],
            &[],
            Some(&CandidatePlanOverride {
                flow_id: "callee".into(),
                execution_bundle_hash: candidate_callee.execution_bundle_hash,
                draft_artifact_hash: ART_CALLEE.into(),
                source_artifact_hash: ART_BINDING_BASE.into(),
                exact_bytes: candidate_callee.exact_bytes,
            }),
        )
        .unwrap_err();
        assert_eq!(refusal.kind, FailKind::ForeignRevision);
        assert_eq!(refusal.flow_id, "callee");
        assert!(
            refusal
                .reason
                .contains("only admitted for the run root flow"),
            "{}",
            refusal.reason
        );
    }

    #[test]
    fn typed_refusals_cover_hash_contract_revision_and_bindings() {
        let mut root = request_plan(ART_ROOT, vec![call_node("call-callee", "callee")], vec![]);
        root.flow_id = "root".into();
        let root_hash = root.execution_bundle_hash.clone();
        let mut callee = request_plan(ART_CALLEE, vec![http_node("http", "http-main")], vec![]);
        callee.flow_id = "callee".into();
        assert_eq!(
            resolve_run_flow_resolutions(&run(&root_hash), &[root.clone()], &[], None)
                .unwrap_err()
                .kind,
            FailKind::UnresolvableName
        );
        let mut invalid_bytes = callee.clone();
        invalid_bytes.exact_bytes = br#"{"not":"a-plan"}"#.to_vec();
        assert_eq!(
            resolve_run_flow_resolutions(
                &run(&root_hash),
                &[root.clone(), invalid_bytes],
                &[],
                None
            )
            .unwrap_err()
            .kind,
            FailKind::HashInvalidBytes
        );
        let mut non_callable = request_plan(ART_CALLEE, Vec::new(), Vec::new());
        non_callable.flow_id = "callee".into();
        let mut value: Value = serde_json::from_slice(&non_callable.exact_bytes).unwrap();
        value["body"]["callable-contract"] = Value::Null;
        non_callable.exact_bytes = serde_json::to_vec(&value).unwrap();
        non_callable.execution_bundle_hash = execution_bundle_hash(&non_callable.exact_bytes);
        assert_eq!(
            resolve_run_flow_resolutions(
                &run(&root_hash),
                &[root.clone(), non_callable],
                &[],
                None
            )
            .unwrap_err()
            .kind,
            FailKind::IncompatibleContract
        );
        assert_eq!(
            resolve_run_flow_resolutions(&run(&root_hash), &[root, callee], &[], None)
                .unwrap_err()
                .kind,
            FailKind::UnboundRequirement
        );
    }

    #[test]
    fn expected_runtime_revision_is_derived_from_root_plan_not_caller_input() {
        let mut root = request_plan(ART_ROOT, vec![call_node("call-callee", "callee")], vec![]);
        root.flow_id = "root".into();
        let root_hash = root.execution_bundle_hash.clone();
        let mut callee = request_plan(ART_CALLEE, Vec::new(), Vec::new());
        callee.flow_id = "callee".into();
        let mut value: Value = serde_json::from_slice(&callee.exact_bytes).unwrap();
        value["header"]["runtime-revision"]["flowrunner-component-digest"] =
            json!("sha256:9999999999999999999999999999999999999999999999999999999999999999");
        callee.exact_bytes = serde_json::to_vec(&value).unwrap();
        callee.execution_bundle_hash = execution_bundle_hash(&callee.exact_bytes);

        assert_eq!(
            resolve_run_flow_resolutions(&run(&root_hash), &[root, callee], &[], None)
                .unwrap_err()
                .kind,
            FailKind::ForeignRevision
        );
    }
}
