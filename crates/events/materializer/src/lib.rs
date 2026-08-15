//! # wamn-materializer (EVT-MAT, D19 v3 §5 / l5i9.17) — CDC event → run decisions
//!
//! The **pure per-event pipeline** the Service-first materializer guest
//! (`components/execution/materializer`) drives: given a subscribing flow's declaration
//! (an [`EventRegistration`] + its subscribed flow identity) and one
//! delivered CDC envelope with its JetStream `stream_seq`, decide — fire a
//! run, skip, or refuse — and, for a fire, mint everything the enqueue needs:
//! the deterministic zero-padded run id ([`wamn_run_state::queue::mint_evt_run_id`]),
//! the author-visible event business input, the child lineage stamp, the
//! trusted invocation context, and the numeric global-FIFO stream position.
//!
//! Like `wamn-run-state` / `wamn-runner`, this crate is **pure**: no DB, no
//! NATS, no clock. The guest supplies the `wamn:postgres` transaction
//! (the centralized callable-flow admission transition), the
//! `wamn:jetstream` fetch/ack, and the post-commit doorbell.
//!
//! ## The decision contract (the .17 design fields, load-bearing)
//!
//! - **Tenant guard**: an event fires only for the tenant the workload is
//!   bound to (`new.tenant_id` vs the injected tenant). Another tenant's event
//!   is a normal [`Skip`](Verdict::Skip) (that tenant's own materializer owns
//!   it); an event that CANNOT be tenant-scoped — a DELETE under REPLICA
//!   IDENTITY DEFAULT (old image = `id` only), or a table with no `tenant_id`
//!   column — is an alertable [`Refuse`](Verdict::Refuse), never a cross-tenant
//!   enqueue. Under the per-entity REPLICA IDENTITY FULL knob (l5i9.31) a FULL
//!   delete's old image carries `tenant_id`, so the delete becomes scopable and
//!   the unscopable refusal retires for that entity.
//! - **Old-image contract** (Q2/l5i9.56 + l5i9.31): old-absent =
//!   cannot-evaluate, never condition-false. A condition that references the
//!   ROOT `old` image is **serviceable** (it compiles), but each event is
//!   guarded: when the delivered event carries NO old image (REPLICA IDENTITY
//!   DEFAULT, or an op with no prior row) the predicate is refused
//!   ([`RefuseReason::OldImageAbsent`], alertable) rather than evaluated against
//!   an absent `old`. The reconciler (`wamn_schema_control::reconcile_replica_identity`)
//!   flips the entity to FULL so the old image is present going forward — the
//!   flip is NON-RETROACTIVE (only WAL written after it carries the old image).
//! - **Causation budget** (depth 16, l5i9.1): a child run's depth is
//!   `parent.depth + 1` (0 for an organic write); over-budget is a distinct,
//!   alertable refusal. Child causation is separate from the author-visible
//!   input and belongs to trusted lineage persistence.
mod condition;
mod context;
mod decide;
mod input;
pub mod sql;

pub use condition::{CompiledCondition, ConditionOutcome, compile_condition};
// The single root-`old` detector lives in wamn-event-reg (the reconciler that
// derives the REPLICA IDENTITY FULL set and this crate share it, never diverge).
pub use context::{event_context, tenant_of};
pub use decide::{
    DecideError, FirePlan, FlowDeclaration, RefuseReason, SkipReason, Verdict,
    VerifiedSourceEventId, child_causation, decide, event_invocation_context_json,
    mint_registered_evt_run_id, serviceable, source_event_id, verified_source_event_id,
};
pub use input::evt_input_json;
pub use wamn_event_reg::{condition_references_old, references_old};

pub use wamn_event_reg::EventRegistration;
pub use wamn_event_wire::{Causation, Envelope, Op};

/// The causation depth ceiling (l5i9.1 sign-off: owner set 16, overriding the
/// doc's proposed ~8). A child at depth > this is refused, alertably.
pub const MAX_CAUSATION_DEPTH: u32 = 16;

/// Verify one applied release member is the stored canonical event-entry plan.
pub fn event_execution_plan_is_valid(
    execution_bundle_hash: &str,
    artifact_hash: &str,
    exact_bytes: &[u8],
) -> bool {
    let Ok(plan) = wamn_catalog::read_execution_plan(execution_bundle_hash, exact_bytes) else {
        return false;
    };
    if plan.header.root_artifact_hash != artifact_hash {
        return false;
    }
    plan.body
        .nodes
        .iter()
        .find(|node| node.local_node_id == plan.body.entry_instruction)
        .is_some_and(|entry| entry.node_type == "event")
}

#[cfg(test)]
mod execution_plan_tests {
    use serde_json::json;
    use wamn_catalog::{
        CALLABLE_CONTRACT_VERSION, CallableContract, CallableEffectCeiling, CallableReturnContract,
        ExecutionEffectPolicy, ExecutionNodeId, ExecutionPlanBody, ExecutionPlanEdge,
        ExecutionPlanNode, ExecutionPlanV2, ExecutionRuntimeRevision, HOST_EFFECT_CONTRACT_VERSION,
        RootTerminalBehavior, entry_input_schema_hash, execution_bundle_hash,
    };

    use super::event_execution_plan_is_valid;

    fn hash(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    fn plan(entry_type: &str) -> (Vec<u8>, String, String) {
        let entry = ExecutionNodeId::new("entry").unwrap();
        let guard = json!({"type": "object"});
        let (nodes, edges, terminal, entry_guard, callable_contract, source_map) =
            if entry_type == "request" {
                let respond = ExecutionNodeId::new("respond").unwrap();
                (
                    vec![
                        ExecutionPlanNode {
                            local_node_id: entry.clone(),
                            source_node_id: "entry".into(),
                            node_type: "request".into(),
                            config: json!({"input-schema": guard.clone()}),
                            effect_policy: ExecutionEffectPolicy::Pure,
                            source_connection_requirement: None,
                        },
                        ExecutionPlanNode {
                            local_node_id: respond.clone(),
                            source_node_id: "respond".into(),
                            node_type: "respond".into(),
                            config: json!({"status": 200}),
                            effect_policy: ExecutionEffectPolicy::Pure,
                            source_connection_requirement: None,
                        },
                    ],
                    vec![ExecutionPlanEdge {
                        source: entry.clone(),
                        source_port: "main".into(),
                        destination: respond.clone(),
                        destination_port: None,
                        fan_out_ordinal: 0,
                    }],
                    RootTerminalBehavior::Respond {
                        responders: vec![respond.clone()],
                    },
                    guard.clone(),
                    Some(CallableContract {
                        version: CALLABLE_CONTRACT_VERSION.into(),
                        input_schema_hash: entry_input_schema_hash(&guard),
                        return_contract: CallableReturnContract::UntypedJsonBody,
                        effect_ceiling: CallableEffectCeiling::Effectful,
                    }),
                    vec![
                        wamn_catalog::ExecutionSourceMapEntry {
                            local_node_id: entry.clone(),
                            source_node_id: "entry".into(),
                        },
                        wamn_catalog::ExecutionSourceMapEntry {
                            local_node_id: respond,
                            source_node_id: "respond".into(),
                        },
                    ],
                )
            } else {
                (
                    vec![ExecutionPlanNode {
                        local_node_id: entry.clone(),
                        source_node_id: "entry".into(),
                        node_type: entry_type.into(),
                        config: json!({}),
                        effect_policy: ExecutionEffectPolicy::Pure,
                        source_connection_requirement: None,
                    }],
                    vec![],
                    RootTerminalBehavior::FrontierExhaustion,
                    serde_json::Value::Bool(true),
                    None,
                    vec![wamn_catalog::ExecutionSourceMapEntry {
                        local_node_id: entry.clone(),
                        source_node_id: "entry".into(),
                    }],
                )
            };
        let artifact_hash = hash('c');
        let plan = ExecutionPlanV2::new(
            ExecutionRuntimeRevision {
                flowrunner_component_digest: hash('a'),
                effect_provider_revision: hash('b'),
                host_effect_contract_version: HOST_EFFECT_CONTRACT_VERSION.into(),
            },
            &artifact_hash,
            ExecutionPlanBody {
                entry_instruction: entry.clone(),
                nodes,
                edges,
                root_terminal_behavior: terminal,
                entry_input_schema_guard: entry_guard,
                callable_contract,
                source_map,
            },
        )
        .unwrap();
        let bytes = serde_json::to_vec(&plan).unwrap();
        let bundle_hash = execution_bundle_hash(&bytes);
        (bytes, bundle_hash, artifact_hash)
    }

    #[test]
    fn release_plan_requires_exact_hash_artifact_and_event_entry() {
        let (bytes, bundle_hash, artifact_hash) = plan("event");
        assert!(event_execution_plan_is_valid(
            &bundle_hash,
            &artifact_hash,
            &bytes
        ));
        assert!(!event_execution_plan_is_valid(
            &hash('d'),
            &artifact_hash,
            &bytes
        ));
        assert!(!event_execution_plan_is_valid(
            &bundle_hash,
            &hash('e'),
            &bytes
        ));

        let (request_bytes, request_hash, request_artifact) = plan("request");
        assert!(!event_execution_plan_is_valid(
            &request_hash,
            &request_artifact,
            &request_bytes
        ));
    }
}
