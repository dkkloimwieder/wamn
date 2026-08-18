//! Publish step A — verify a tested draft and mint its control release
//! (wamn-0h0g.8.7).
//!
//! One control-database transaction, independently callable by the A/B/C
//! orchestrator, holding **no project connection** and claiming no atomicity
//! across the two databases. It locks and verifies, in order:
//!
//! 1. the exact immutable validated draft named by `validated_draft_id`, which is
//!    what derives the release coordinate — every coordinate the caller supplies
//!    is an assertion checked against that row, never a free choice;
//! 2. that draft's own green finalized test report;
//! 3. the target release coordinate and the environment it belongs to;
//! 4. the immutable source artifact the release member names;
//! 5. the flow's own plan, read hash-before-parse through
//!    [`wamn_catalog::read_execution_plan`] and required to name that same
//!    artifact as its root;
//! 6. the callable-contract facts the tested release requires — every `call-flow`
//!    callee is a released member of this exact release and its own plan carries a
//!    callable contract;
//! 7. the tested resolution map, canonicalized to exact RFC 8785 bytes by the one
//!    landed canonicalizer.
//!
//! Only then does it append exactly two rows: one `catalog.release_flows` member
//! and one `catalog.release_flow_test_evidence` row through the landed
//! wamn-0h0g.9.9 routine. Every conflicting reuse is refused by the pre-reads
//! *before* either append runs, and an exact retry returns the release already
//! minted with its original server-minted `created_at`.
//!
//! # `tested_resolution_map` is bytes, never JSONB
//!
//! The report stores its map as `jsonb` with a hash over PostgreSQL's `jsonb::text`
//! rendering. That rendering is not RFC 8785 — it inserts spaces — so the evidence
//! hash and the report hash deliberately differ. Step A derives the authoritative
//! bytes with [`wamn_flow::canonical_json_bytes`], the workspace's single
//! canonicalizer, and persists those exact bytes; identity is byte equality and
//! never a reserialized projection (wamn-0h0g.9.9, owner amendment 2026-08-15).
//!
//! Step A reads the map as an opaque evidence snapshot. It deliberately derives no
//! callee resolution from it: the map's producer is broken today
//! (wamn-0h0g.15.29 — with case-level maps deleted, reports mint `{}`), and its
//! key/value semantics are re-sourced from the release manifest there. Callability
//! is verified against the release itself, which is well defined now.
//!
//! # Out of scope, by construction
//!
//! Step A touches no deployed map, no project binding or connection generation, no
//! project retention, no activation, no deployment attestation, no lifecycle
//! state, and no project relation at all. The attestation is wamn-0h0g.8.21's
//! transaction C; sealing the release manifest is wamn-0h0g.15.14's. Both
//! [`wamn_control_provision::publish_release`] and the source audit below assert
//! that rather than assuming it.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use tokio_postgres::{Client, Transaction};
use wamn_catalog::{CallFlowInstruction, ExecutionPlanV2, read_execution_plan};
use wamn_control_provision::publish_release::{
    PublishReleaseError, PublishReleaseErrorKind, PublishTestedRelease, ReleaseEvidenceFacts,
    ReleaseMemberFacts, TestReportFacts, ValidatedDraftFacts, claim_publishing_tenant_sql,
    lock_release_coordinate_sql, lock_release_evidence_sql, lock_release_member_sql,
    lock_source_artifact_sql, lock_test_report_sql, lock_validated_draft_sql,
    register_release_evidence_sql, select_plan_bytes_sql, select_released_callee_sql,
    verify_release_evidence, verify_release_member, verify_test_report, verify_validated_draft,
};

/// The one plan node type whose config names a callee flow.
const CALL_FLOW_NODE_TYPE: &str = "call-flow";

/// What one committed publish step A left in the control database.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MintedTestedRelease {
    /// Server-minted immutable evidence timestamp; an exact retry returns it again.
    pub created_at: DateTime<Utc>,
    /// SHA-256 over the exact RFC 8785 tested-resolution-map bytes persisted.
    pub tested_resolution_map_hash: String,
    /// Whether THIS call minted the release member. `false` is an exact retry.
    pub minted: bool,
}

/// Run publish step A in exactly one control-database transaction.
///
/// The connection must be the portable store's owner: registering evidence is
/// owner-only, and row locking needs a write privilege the author role does not
/// hold (see [`wamn_control_provision::publish_release`]).
pub async fn mint_tested_release(
    client: &mut Client,
    request: &PublishTestedRelease<'_>,
) -> Result<MintedTestedRelease, PublishReleaseError> {
    let transaction = client
        .transaction()
        .await
        .map_err(|error| storage("begin publish step A", error))?;
    // Bound to a `let` so the borrow of `transaction` ends before it is consumed.
    let outcome = publish(&transaction, request).await;
    match outcome {
        Ok(minted) => {
            transaction
                .commit()
                .await
                .map_err(|error| storage("commit publish step A", error))?;
            Ok(minted)
        }
        Err(refusal) => {
            let _ = transaction.rollback().await;
            Err(refusal)
        }
    }
}

fn storage(context: &'static str, error: tokio_postgres::Error) -> PublishReleaseError {
    PublishReleaseError::with_source(PublishReleaseErrorKind::Storage, context, error)
}

async fn publish(
    transaction: &Transaction<'_>,
    request: &PublishTestedRelease<'_>,
) -> Result<MintedTestedRelease, PublishReleaseError> {
    transaction
        .query_one(claim_publishing_tenant_sql(), &[&request.tenant_id])
        .await
        .map_err(|error| storage("claim the publishing tenant", error))?;

    // 1 — the exact validated draft, which derives every release coordinate.
    let Some(draft) = transaction
        .query_opt(
            lock_validated_draft_sql(),
            &[&request.tenant_id, &request.validated_draft_id],
        )
        .await
        .map_err(|error| storage("lock the validated draft", error))?
    else {
        return Err(PublishReleaseError::new(
            PublishReleaseErrorKind::ValidatedDraft,
            format!(
                "this tenant has no validated draft {:?}",
                request.validated_draft_id
            ),
        ));
    };
    verify_validated_draft(
        request,
        &ValidatedDraftFacts {
            catalog_id: draft.get(0),
            catalog_version: draft.get(1),
            environment: draft.get(2),
            flow_id: draft.get(3),
            runtime_flow_version: draft.get(4),
            draft_artifact_hash: draft.get(5),
            execution_bundle_hash: draft.get(6),
        },
    )?;

    // 2 — that draft's own green finalized report. The gate is unconditional.
    let Some(report) = transaction
        .query_opt(
            lock_test_report_sql(),
            &[&request.tenant_id, &request.report_id],
        )
        .await
        .map_err(|error| storage("lock the finalized test report", error))?
    else {
        return Err(PublishReleaseError::new(
            PublishReleaseErrorKind::TestReport,
            format!(
                "this tenant has no finalized test report {:?}",
                request.report_id
            ),
        ));
    };
    let stored_resolution_map: String = report.get(4);
    verify_test_report(
        request,
        &TestReportFacts {
            validated_draft_id: report.get(0),
            catalog_id: report.get(1),
            catalog_version: report.get(2),
            passed: report.get(3),
        },
    )?;

    // 3 — the release coordinate and the environment it belongs to.
    let Some(release) = transaction
        .query_opt(
            lock_release_coordinate_sql(),
            &[
                &request.tenant_id,
                &request.catalog_id,
                &request.catalog_version,
            ],
        )
        .await
        .map_err(|error| storage("lock the target release coordinate", error))?
    else {
        return Err(PublishReleaseError::new(
            PublishReleaseErrorKind::ReleaseCoordinate,
            format!(
                "catalog {:?} version {} has no release manifest to publish into",
                request.catalog_id, request.catalog_version
            ),
        ));
    };
    let release_environment: String = release.get(0);
    if release_environment != request.environment {
        return Err(PublishReleaseError::new(
            PublishReleaseErrorKind::ReleaseCoordinate,
            format!(
                "catalog {:?} version {} is an {release_environment:?} release, not {:?}",
                request.catalog_id, request.catalog_version, request.environment
            ),
        ));
    }

    // 4 — the immutable source artifact the release member names.
    let Some(artifact) = transaction
        .query_opt(
            lock_source_artifact_sql(),
            &[&request.tenant_id, &request.flow_id, &request.flow_version],
        )
        .await
        .map_err(|error| storage("lock the source flow artifact", error))?
    else {
        return Err(PublishReleaseError::new(
            PublishReleaseErrorKind::SourceArtifact,
            format!(
                "flow {:?} version {} has no immutable source artifact",
                request.flow_id, request.flow_version
            ),
        ));
    };
    let artifact_hash: String = artifact.get(0);
    if artifact_hash != request.source_artifact_hash {
        return Err(PublishReleaseError::new(
            PublishReleaseErrorKind::SourceArtifact,
            format!(
                "flow {:?} version {} is artifact {artifact_hash}, not the supplied {}",
                request.flow_id, request.flow_version, request.source_artifact_hash
            ),
        ));
    }

    // 5 — the flow's own plan bytes, hashed before parse, naming that artifact.
    let plan = read_own_plan(transaction, request).await?;
    if plan.header.root_artifact_hash != request.source_artifact_hash {
        return Err(PublishReleaseError::new(
            PublishReleaseErrorKind::PlanBytes,
            format!(
                "plan {} names root artifact {}, not the supplied {}",
                request.execution_bundle_hash,
                plan.header.root_artifact_hash,
                request.source_artifact_hash
            ),
        ));
    }

    // 6 — the callable-contract facts this tested release requires.
    for callee_flow_id in call_flow_callees(&plan)? {
        verify_callee_is_released_and_callable(transaction, request, &plan, &callee_flow_id)
            .await?;
    }

    // 7 — the tested resolution map, as exact RFC 8785 bytes.
    let (map_bytes, map_hash) = canonical_tested_resolution_map(&stored_resolution_map)?;

    // Every conflicting reuse refuses here, before either append.
    let member = transaction
        .query_opt(
            lock_release_member_sql(),
            &[
                &request.tenant_id,
                &request.catalog_id,
                &request.catalog_version,
                &request.flow_id,
            ],
        )
        .await
        .map_err(|error| storage("lock the existing release member", error))?;
    if let Some(row) = &member {
        verify_release_member(
            request,
            &ReleaseMemberFacts {
                flow_version: row.get(0),
                execution_bundle_hash: row.get(1),
            },
        )?;
    }
    if let Some(row) = transaction
        .query_opt(
            lock_release_evidence_sql(),
            &[
                &request.tenant_id,
                &request.catalog_id,
                &request.catalog_version,
                &request.flow_id,
            ],
        )
        .await
        .map_err(|error| storage("lock the existing release evidence", error))?
    {
        verify_release_evidence(
            request,
            &map_bytes,
            &map_hash,
            &ReleaseEvidenceFacts {
                validated_draft_id: row.get(0),
                report_id: row.get(1),
                source_artifact_hash: row.get(2),
                execution_bundle_hash: row.get(3),
                tested_resolution_map_bytes: row.get(4),
                tested_resolution_map_hash: row.get(5),
            },
        )?;
    }

    // Append one — the release member. `ON CONFLICT DO NOTHING` is what makes a
    // retry converge instead of reminting.
    let inserted = transaction
        .execute(
            wamn_schema_control::sql::insert_release_flow_sql(),
            &[
                &request.tenant_id,
                &request.catalog_id,
                &request.catalog_version,
                &request.flow_id,
                &request.flow_version,
                &request.execution_bundle_hash,
            ],
        )
        .await
        .map_err(|error| storage("mint the tested release member", error))?;
    if inserted == 0 && member.is_none() {
        // READ COMMITTED cannot gap-lock an absent member, so a concurrent publish
        // can still win between the pre-read and this append. Its row is either
        // this exact release — an exact retry — or a conflict.
        let Some(row) = transaction
            .query_opt(
                lock_release_member_sql(),
                &[
                    &request.tenant_id,
                    &request.catalog_id,
                    &request.catalog_version,
                    &request.flow_id,
                ],
            )
            .await
            .map_err(|error| storage("recheck the raced release member", error))?
        else {
            return Err(PublishReleaseError::new(
                PublishReleaseErrorKind::ReleaseConflict,
                "the tested release member disappeared during publish",
            ));
        };
        verify_release_member(
            request,
            &ReleaseMemberFacts {
                flow_version: row.get(0),
                execution_bundle_hash: row.get(1),
            },
        )?;
    }

    // Append two — the tested evidence, through the landed wamn-0h0g.9.9 routine.
    let created_at: DateTime<Utc> = transaction
        .query_one(
            register_release_evidence_sql(),
            &[
                &request.tenant_id,
                &request.catalog_id,
                &request.catalog_version,
                &request.flow_id,
                &request.validated_draft_id,
                &request.report_id,
                &request.source_artifact_hash,
                &request.execution_bundle_hash,
                &map_bytes,
                &map_hash,
            ],
        )
        .await
        .map_err(|error| {
            if is_evidence_content_conflict(&error) {
                PublishReleaseError::with_source(
                    PublishReleaseErrorKind::EvidenceConflict,
                    "a concurrent publish registered different tested evidence first",
                    error,
                )
            } else {
                storage("register the tested release evidence", error)
            }
        })?
        .get(0);

    Ok(MintedTestedRelease {
        created_at,
        tested_resolution_map_hash: map_hash,
        minted: inserted == 1,
    })
}

/// Whether the landed routine refused this append as a content conflict.
///
/// The driver's pre-read already refuses a conflicting reuse before any append, so
/// this is only reachable when a concurrent publisher committed different evidence
/// for the coordinate in between. Its predicate is the same either way.
fn is_evidence_content_conflict(error: &tokio_postgres::Error) -> bool {
    error.as_db_error().is_some_and(|database| {
        database.code() == &tokio_postgres::error::SqlState::UNIQUE_VIOLATION
            && database.message() == "release-flow-test-evidence-content-conflict"
    })
}

/// Read the flow's own stored plan, comparing exact bytes before parsing them.
async fn read_own_plan(
    transaction: &Transaction<'_>,
    request: &PublishTestedRelease<'_>,
) -> Result<ExecutionPlanV2, PublishReleaseError> {
    let Some(row) = transaction
        .query_opt(
            select_plan_bytes_sql(),
            &[&request.tenant_id, &request.execution_bundle_hash],
        )
        .await
        .map_err(|error| storage("read the flow's stored execution plan", error))?
    else {
        return Err(PublishReleaseError::new(
            PublishReleaseErrorKind::PlanBytes,
            format!(
                "this tenant stores no execution bundle {}",
                request.execution_bundle_hash
            ),
        ));
    };
    let exact_bytes: Vec<u8> = row.get(0);
    read_execution_plan(request.execution_bundle_hash, &exact_bytes).map_err(|error| {
        PublishReleaseError::with_source(
            PublishReleaseErrorKind::PlanBytes,
            format!(
                "execution bundle {} is not a valid own-flow plan",
                request.execution_bundle_hash
            ),
            error,
        )
    })
}

/// The unique callee flow ids one own-flow plan calls, in a stable order.
fn call_flow_callees(plan: &ExecutionPlanV2) -> Result<BTreeSet<String>, PublishReleaseError> {
    let mut callees = BTreeSet::new();
    for node in &plan.body.nodes {
        if node.node_type != CALL_FLOW_NODE_TYPE {
            continue;
        }
        let instruction = serde_json::from_value::<CallFlowInstruction>(node.config.clone())
            .map_err(|error| {
                PublishReleaseError::with_source(
                    PublishReleaseErrorKind::PlanBytes,
                    format!(
                        "call-flow node {} carries no callee identity",
                        node.local_node_id
                    ),
                    error,
                )
            })?;
        callees.insert(instruction.flow_id);
    }
    Ok(callees)
}

/// Verify one callee is a member of this exact release and is callable.
///
/// A self-call names the very member this transaction is minting, so its contract
/// is the one already read from the plan under verification. A *mutually*
/// recursive group's first publication stays unresolvable here and is the named
/// deferral wamn-0h0g.13.20.
async fn verify_callee_is_released_and_callable(
    transaction: &Transaction<'_>,
    request: &PublishTestedRelease<'_>,
    own_plan: &ExecutionPlanV2,
    callee_flow_id: &str,
) -> Result<(), PublishReleaseError> {
    let callable = if callee_flow_id == request.flow_id {
        own_plan.body.callable_contract.is_some()
    } else {
        let Some(row) = transaction
            .query_opt(
                select_released_callee_sql(),
                &[
                    &request.tenant_id,
                    &request.catalog_id,
                    &request.catalog_version,
                    &callee_flow_id,
                ],
            )
            .await
            .map_err(|error| storage("resolve a released call-flow callee", error))?
        else {
            return Err(PublishReleaseError::new(
                PublishReleaseErrorKind::CallableContract,
                format!(
                    "call-flow callee {callee_flow_id:?} is not a member of catalog {:?} \
                     version {}",
                    request.catalog_id, request.catalog_version
                ),
            ));
        };
        let callee_bundle_hash: String = row.get(0);
        let callee_bytes: Vec<u8> = row.get(1);
        read_execution_plan(&callee_bundle_hash, &callee_bytes)
            .map_err(|error| {
                PublishReleaseError::with_source(
                    PublishReleaseErrorKind::CallableContract,
                    format!("released callee {callee_flow_id:?} has no valid plan"),
                    error,
                )
            })?
            .body
            .callable_contract
            .is_some()
    };
    if !callable {
        return Err(PublishReleaseError::new(
            PublishReleaseErrorKind::CallableContract,
            format!("call-flow callee {callee_flow_id:?} carries no callable contract"),
        ));
    }
    Ok(())
}

/// Canonicalize the report's stored map into the authoritative evidence bytes.
fn canonical_tested_resolution_map(stored: &str) -> Result<(Vec<u8>, String), PublishReleaseError> {
    let map: serde_json::Value = serde_json::from_str(stored).map_err(|error| {
        PublishReleaseError::with_source(
            PublishReleaseErrorKind::TestedResolutionMap,
            "the finalized report's tested resolution map is not JSON",
            error,
        )
    })?;
    if !map.is_object() {
        return Err(PublishReleaseError::new(
            PublishReleaseErrorKind::TestedResolutionMap,
            "the finalized report's tested resolution map is not a JSON object",
        ));
    }
    Ok((
        wamn_flow::canonical_json_bytes(&map),
        wamn_flow::canonical_json_sha256(&map),
    ))
}

#[cfg(test)]
mod tests {
    use tokio_postgres::NoTls;
    use wamn_catalog::{
        CALLABLE_CONTRACT_VERSION, CallableContract, CallableEffectCeiling, CallableReturnContract,
        ExecutionEffectPolicy, ExecutionNodeId, ExecutionPlanBody, ExecutionPlanEdge,
        ExecutionPlanNode, ExecutionRuntimeRevision, ExecutionSourceMapEntry, RootTerminalBehavior,
        entry_input_schema_hash, execution_bundle_hash,
    };

    use super::*;

    const TENANT: &str = "publish-step-a-tenant";
    const CATALOG_ID: &str = "publish-step-a-catalog";
    const CATALOG_VERSION: i32 = 1;
    const ENVIRONMENT: &str = "dev";
    const FLOW_ID: &str = "orders";
    const FLOW_VERSION: i32 = 2;
    const ROOT_ARTIFACT: &str =
        "sha256:1111111111111111111111111111111111111111111111111111111111111111";
    const COMMAND_HASH: &str =
        "sha256:4444444444444444444444444444444444444444444444444444444444444444";

    /// The wamn-0h0g.9.9 frozen tested-resolution-map vector, keys out of order.
    const STORED_MAP: &str = concat!(
        r#"{"flow-z":"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","#,
        r#""flow-a":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#
    );
    const CANONICAL_MAP: &[u8] = concat!(
        r#"{"flow-a":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","#,
        r#""flow-z":"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}"#
    )
    .as_bytes();
    const CANONICAL_MAP_HASH: &str =
        "sha256:1ce65570349393c45e6c5ab58405b960e24b6d0d8ece076a4e4b0947b52383a2";

    fn node(
        local: &str,
        source: &str,
        node_type: &str,
        config: serde_json::Value,
    ) -> ExecutionPlanNode {
        ExecutionPlanNode {
            local_node_id: ExecutionNodeId::new(local).expect("a slug node id"),
            source_node_id: source.to_string(),
            node_type: node_type.to_string(),
            config,
            effect_policy: ExecutionEffectPolicy::Pure,
            source_connection_requirement: None,
        }
    }

    fn source_map(entries: &[(&str, &str)]) -> Vec<ExecutionSourceMapEntry> {
        entries
            .iter()
            .map(|(local, source)| ExecutionSourceMapEntry {
                local_node_id: ExecutionNodeId::new(*local).expect("a slug node id"),
                source_node_id: (*source).to_string(),
            })
            .collect()
    }

    fn edge(source: &str, destination: &str) -> ExecutionPlanEdge {
        ExecutionPlanEdge {
            source: ExecutionNodeId::new(source).expect("a slug node id"),
            source_port: "out".to_string(),
            destination: ExecutionNodeId::new(destination).expect("a slug node id"),
            destination_port: None,
            fan_out_ordinal: 0,
        }
    }

    fn runtime_revision() -> ExecutionRuntimeRevision {
        ExecutionRuntimeRevision {
            flowrunner_component_digest: format!("sha256:{}", "a".repeat(64)),
            effect_provider_revision: format!("sha256:{}", "b".repeat(64)),
            host_effect_contract_version: wamn_catalog::HOST_EFFECT_CONTRACT_VERSION.to_string(),
        }
    }

    /// A minimal valid own-flow plan: `request → respond`, intrinsically callable.
    ///
    /// `entry_source` only varies the exact bytes, so two fixtures are two distinct
    /// execution bundles for the one source artifact.
    fn own_plan(entry_source: &str) -> ExecutionPlanV2 {
        let guard = serde_json::Value::Bool(true);
        let body = ExecutionPlanBody {
            entry_instruction: ExecutionNodeId::new("request").expect("a slug node id"),
            nodes: vec![
                node(
                    "request",
                    entry_source,
                    "request",
                    serde_json::json!({"input-schema": true}),
                ),
                node(
                    "respond",
                    "respond",
                    "respond",
                    serde_json::json!({"status": 200}),
                ),
            ],
            edges: vec![edge("request", "respond")],
            root_terminal_behavior: RootTerminalBehavior::Respond {
                responders: vec![ExecutionNodeId::new("respond").expect("a slug node id")],
            },
            callable_contract: Some(CallableContract {
                version: CALLABLE_CONTRACT_VERSION.to_string(),
                input_schema_hash: entry_input_schema_hash(&guard),
                return_contract: CallableReturnContract::UntypedJsonBody,
                effect_ceiling: CallableEffectCeiling::Effectful,
            }),
            entry_input_schema_guard: guard,
            source_map: source_map(&[("request", entry_source), ("respond", "respond")]),
        };
        ExecutionPlanV2::new(runtime_revision(), ROOT_ARTIFACT, body)
            .expect("the fixture plan is valid")
    }

    fn plan_bytes(plan: &ExecutionPlanV2) -> (String, Vec<u8>) {
        let bytes = serde_json::to_vec(plan).expect("a plan serializes");
        (execution_bundle_hash(&bytes), bytes)
    }

    #[test]
    fn only_call_flow_nodes_contribute_callees_and_each_appears_once() {
        let mut plan = own_plan("request");
        assert!(
            call_flow_callees(&plan)
                .expect("a call-free plan resolves")
                .is_empty()
        );

        for (site, callee) in [
            ("fan-one", "shipping"),
            ("fan-two", "shipping"),
            ("fan-three", "billing"),
        ] {
            plan.body.nodes.push(node(
                site,
                site,
                CALL_FLOW_NODE_TYPE,
                serde_json::json!({"site": site, "flow-id": callee}),
            ));
        }
        let callees = call_flow_callees(&plan).expect("call-flow configs resolve");
        assert_eq!(
            callees.into_iter().collect::<Vec<_>>(),
            vec!["billing".to_string(), "shipping".to_string()]
        );

        // A call-flow node whose config is not the ratified {site, flow-id}.
        plan.body.nodes.push(node(
            "broken",
            "broken",
            CALL_FLOW_NODE_TYPE,
            serde_json::json!({"site": "broken"}),
        ));
        assert_eq!(
            call_flow_callees(&plan)
                .expect_err("a config without a callee refuses")
                .kind(),
            PublishReleaseErrorKind::PlanBytes
        );
    }

    #[test]
    fn the_evidence_map_is_rfc8785_bytes_not_the_stored_jsonb_rendering() {
        let (bytes, hash) =
            canonical_tested_resolution_map(STORED_MAP).expect("a JSON object canonicalizes");
        assert_eq!(bytes, CANONICAL_MAP);
        assert_eq!(hash, CANONICAL_MAP_HASH);

        // PostgreSQL renders `jsonb::text` with spaces, so hashing that rendering
        // cannot establish evidence identity: same value, different bytes.
        let jsonb_rendering = String::from_utf8(CANONICAL_MAP.to_vec())
            .expect("the canonical map is UTF-8")
            .replace("\":", "\": ")
            .replace(",\"", ", \"");
        assert_ne!(jsonb_rendering.as_bytes(), CANONICAL_MAP);
        let (reparsed, reparsed_hash) = canonical_tested_resolution_map(&jsonb_rendering)
            .expect("the spaced rendering canonicalizes back");
        assert_eq!(reparsed, CANONICAL_MAP);
        assert_eq!(reparsed_hash, CANONICAL_MAP_HASH);

        for malformed in ["not json", "[]", "7", "null"] {
            assert_eq!(
                canonical_tested_resolution_map(malformed)
                    .expect_err("a non-object map refuses")
                    .kind(),
                PublishReleaseErrorKind::TestedResolutionMap
            );
        }
    }

    /// Step A is control-only: nothing in this module names a project relation, an
    /// attestation, a binding, a generation, or an activation, and its only writes
    /// are the two named appends.
    #[test]
    fn no_statement_in_this_module_reaches_a_project_or_deployment_record() {
        let source = include_str!("publish_release.rs");
        let implementation = source
            .split("#[cfg(test)]")
            .next()
            .expect("the module has an implementation");
        for forbidden in [
            "deployment_attestations",
            "register_deployment_attestation",
            "deployed_resolution_map",
            "connection_bindings",
            "connection_instances",
            "connection_generations",
            "connection_generation_retention",
            "attachment_activation",
            "attachment_tombstones",
            "FROM runs",
            "JOIN runs",
            "UPDATE ",
            "DELETE FROM",
            "TRUNCATE",
        ] {
            assert!(
                !implementation.contains(forbidden),
                "publish step A reaches {forbidden}"
            );
        }
        assert_eq!(
            implementation.matches("insert_release_flow_sql()").count(),
            1
        );
        assert_eq!(
            implementation
                .matches("register_release_evidence_sql()")
                .count(),
            1
        );
    }

    async fn connect(url: &str) -> (Client, tokio::task::JoinHandle<()>) {
        let (client, connection) = tokio_postgres::connect(url, NoTls)
            .await
            .expect("connect to the control database");
        let task = tokio::spawn(async move {
            let _ = connection.await;
        });
        (client, task)
    }

    async fn provision_control_store(admin: &Client) {
        admin
            .batch_execute(
                "DROP SCHEMA IF EXISTS catalog CASCADE; \
                 DROP SCHEMA IF EXISTS wamn_run CASCADE; \
                 DROP SCHEMA IF EXISTS wamn_authority CASCADE; \
                 DROP SCHEMA IF EXISTS registry CASCADE; \
                 DROP SCHEMA IF EXISTS provisioning CASCADE; \
                 DROP SCHEMA IF EXISTS identity CASCADE; \
                 DO $$ BEGIN IF NOT EXISTS \
                   (SELECT FROM pg_roles WHERE rolname = 'wamn_system') THEN \
                   CREATE ROLE wamn_system NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                     NOINHERIT NOREPLICATION NOBYPASSRLS; END IF; END $$; \
                 DO $$ BEGIN EXECUTE format( \
                   'GRANT CREATE ON DATABASE %I TO wamn_system', current_database()); END $$;",
            )
            .await
            .expect("reset the control schemas");
        admin
            .batch_execute(wamn_control_provision::sql::ensure_control_author_acl_role_sql())
            .await
            .expect("mint the stable control-author ACL role");
        admin
            .batch_execute(&format!(
                "SET ROLE wamn_system;\n{}\n{}\nRESET ROLE;",
                wamn_control_provision::SYSTEM_SCHEMA_SQL,
                wamn_control_provision::CONTROL_PORTABLE_STORE_SQL,
            ))
            .await
            .expect("apply the fresh control bootstrap");
    }

    async fn seed_release_base(admin: &Client) {
        admin
            .execute("SELECT set_config('app.tenant', $1, false)", &[&TENANT])
            .await
            .expect("scope the seeding session");
        admin
            .execute(
                "INSERT INTO catalog.catalogs \
                   (tenant_id, catalog_id, version, environment, schema_version, state) \
                 VALUES ($1, $2, $3, $4, '0.1', 'applied')",
                &[&TENANT, &CATALOG_ID, &CATALOG_VERSION, &ENVIRONMENT],
            )
            .await
            .expect("seed the catalog release");
        admin
            .execute(
                "INSERT INTO catalog.release_manifests \
                   (tenant_id, catalog_id, catalog_version) \
                 VALUES ($1, $2, $3)",
                &[&TENANT, &CATALOG_ID, &CATALOG_VERSION],
            )
            .await
            .expect("seed the release manifest");
        admin
            .execute(
                "INSERT INTO catalog.flow_artifacts \
                   (tenant_id, flow_id, flow_version, schema_version, graph_json, graph_hash, \
                    artifact_hash) \
                 VALUES ($1, $2, $3, '0.1', '{}'::jsonb, $4, $4)",
                &[&TENANT, &FLOW_ID, &FLOW_VERSION, &ROOT_ARTIFACT],
            )
            .await
            .expect("seed the immutable source artifact");
    }

    async fn seed_bundle(admin: &Client, hash: &str, bytes: &[u8]) {
        let byte_length = i32::try_from(bytes.len()).expect("a fixture plan fits i32");
        admin
            .execute(
                "INSERT INTO catalog.execution_bundles \
                   (tenant_id, execution_bundle_hash, format_version, exact_bytes, byte_length) \
                 VALUES ($1, $2, '0.1', $3, $4)",
                &[&TENANT, &hash, &bytes, &byte_length],
            )
            .await
            .expect("seed an execution bundle");
    }

    async fn seed_validated_draft(admin: &Client, draft_id: &str, bundle_hash: &str) {
        admin
            .execute(
                "INSERT INTO catalog.validated_flow_drafts \
                   (tenant_id, draft_id, draft_revision, draft_edited_at, draft_content_hash, \
                    catalog_id, catalog_version, environment, flow_id, runtime_flow_version, \
                    graph_json, graph_hash, draft_artifact_hash, execution_bundle_hash, \
                    binding_base_artifact_hash, validated_draft_hash) \
                 VALUES ($1, $2, 1, now(), $3, $4, $5, $6, $7, $8, '{}'::jsonb, $3, $3, $9, \
                         $3, $10)",
                &[
                    &TENANT,
                    &format!("draft-of-{draft_id}"),
                    &ROOT_ARTIFACT,
                    &CATALOG_ID,
                    &CATALOG_VERSION,
                    &ENVIRONMENT,
                    &FLOW_ID,
                    &FLOW_VERSION,
                    &bundle_hash,
                    &draft_id,
                ],
            )
            .await
            .expect("seed a validated draft");
    }

    async fn seed_report(admin: &Client, report_id: &str, draft_id: &str, passed: bool) {
        admin
            .execute(
                "INSERT INTO wamn_run.authoring_test_run_reservations \
                   (tenant_id, report_id, command_hash, validated_draft_id, catalog_id, \
                    catalog_version, case_count, state, created_at, whole_deadline_at, \
                    finalized_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, 1, 'finalized', now(), \
                         now() + interval '1 hour', now())",
                &[
                    &TENANT,
                    &report_id,
                    &COMMAND_HASH,
                    &draft_id,
                    &CATALOG_ID,
                    &CATALOG_VERSION,
                ],
            )
            .await
            .expect("seed a finalized reservation");
        admin
            .execute(
                "INSERT INTO wamn_run.authoring_test_reports \
                   (tenant_id, report_id, validated_draft_id, catalog_id, catalog_version, \
                    resolution_map, resolution_map_hash, passed, summary) \
                 SELECT $1, $2, $3, $4, $5, stored.map, \
                        'sha256:' || encode(sha256(convert_to(stored.map::text, 'UTF8')), 'hex'), \
                        $7, '{}'::jsonb \
                   FROM (SELECT $6::text::jsonb AS map) AS stored",
                &[
                    &TENANT,
                    &report_id,
                    &draft_id,
                    &CATALOG_ID,
                    &CATALOG_VERSION,
                    &STORED_MAP,
                    &passed,
                ],
            )
            .await
            .expect("seed a finalized report");
    }

    async fn control_counts(admin: &Client) -> (i64, i64, i64, i64) {
        let row = admin
            .query_one(
                "SELECT \
                   (SELECT count(*) FROM catalog.release_flows WHERE tenant_id = $1), \
                   (SELECT count(*) FROM catalog.release_flow_test_evidence \
                     WHERE tenant_id = $1), \
                   (SELECT count(*) FROM catalog.deployment_attestations WHERE tenant_id = $1), \
                   (SELECT count(*) FROM catalog.flow_drafts WHERE tenant_id = $1)",
                &[&TENANT],
            )
            .await
            .expect("read the control record counts");
        (row.get(0), row.get(1), row.get(2), row.get(3))
    }

    fn request<'a>(
        validated_draft_id: &'a str,
        report_id: &'a str,
        execution_bundle_hash: &'a str,
    ) -> PublishTestedRelease<'a> {
        PublishTestedRelease {
            tenant_id: TENANT,
            catalog_id: CATALOG_ID,
            catalog_version: CATALOG_VERSION,
            environment: ENVIRONMENT,
            flow_id: FLOW_ID,
            flow_version: FLOW_VERSION,
            validated_draft_id,
            report_id,
            source_artifact_hash: ROOT_ARTIFACT,
            execution_bundle_hash,
        }
    }

    /// The control-PostgreSQL-18 transaction proof: one mint, an exact retry, and
    /// every conflicting reuse refused before any mutation.
    #[tokio::test]
    async fn publish_step_a_mints_once_and_refuses_every_conflicting_reuse() {
        let Ok(url) = std::env::var("WAMN_PUBLISH_STEP_A_PG_URL") else {
            eprintln!(
                "skipping publish_step_a_mints_once_and_refuses_every_conflicting_reuse \
                 (set WAMN_PUBLISH_STEP_A_PG_URL)"
            );
            return;
        };
        let (mut admin, admin_task) = connect(&url).await;
        provision_control_store(&admin).await;
        seed_release_base(&admin).await;

        let (tested_hash, tested_bytes) = plan_bytes(&own_plan("request"));
        let (other_hash, other_bytes) = plan_bytes(&own_plan("request-v2"));
        assert_ne!(tested_hash, other_hash);
        seed_bundle(&admin, &tested_hash, &tested_bytes).await;
        seed_bundle(&admin, &other_hash, &other_bytes).await;
        seed_validated_draft(&admin, "validated-a", &tested_hash).await;
        seed_validated_draft(&admin, "validated-b", &other_hash).await;
        seed_report(&admin, "report-a", "validated-a", true).await;
        seed_report(&admin, "report-a2", "validated-a", true).await;
        seed_report(&admin, "report-b", "validated-b", true).await;
        seed_report(&admin, "report-red", "validated-a", false).await;
        assert_eq!(control_counts(&admin).await, (0, 0, 0, 0));

        // One mint.
        let tested = request("validated-a", "report-a", &tested_hash);
        let minted = mint_tested_release(&mut admin, &tested)
            .await
            .expect("the tested release mints");
        assert!(minted.minted);
        assert_eq!(minted.tested_resolution_map_hash, CANONICAL_MAP_HASH);
        assert_eq!(control_counts(&admin).await, (1, 1, 0, 0));

        // The evidence carries the exact canonical bytes, and the report's own
        // `jsonb::text` hash is deliberately a different value.
        let evidence = admin
            .query_one(
                "SELECT evidence.tested_resolution_map_bytes, \
                        evidence.tested_resolution_map_hash, report.resolution_map_hash \
                   FROM catalog.release_flow_test_evidence AS evidence \
                   JOIN wamn_run.authoring_test_reports AS report \
                     ON report.tenant_id = evidence.tenant_id \
                    AND report.report_id = evidence.report_id \
                  WHERE evidence.tenant_id = $1",
                &[&TENANT],
            )
            .await
            .expect("read the minted evidence");
        assert_eq!(evidence.get::<_, Vec<u8>>(0), CANONICAL_MAP);
        assert_eq!(evidence.get::<_, String>(1), CANONICAL_MAP_HASH);
        assert_ne!(evidence.get::<_, String>(2), CANONICAL_MAP_HASH);

        // An exact retry returns the same release and never remints.
        let retried = mint_tested_release(&mut admin, &tested)
            .await
            .expect("an exact retry converges");
        assert!(!retried.minted);
        assert_eq!(retried.created_at, minted.created_at);
        assert_eq!(
            retried.tested_resolution_map_hash,
            minted.tested_resolution_map_hash
        );
        assert_eq!(control_counts(&admin).await, (1, 1, 0, 0));

        // Every conflicting reuse refuses, by predicate, before any mutation.
        for (expected, attempt) in [
            (
                PublishReleaseErrorKind::ValidatedDraft,
                request("validated-missing", "report-a", &tested_hash),
            ),
            (
                PublishReleaseErrorKind::ValidatedDraft,
                request("validated-a", "report-a", &other_hash),
            ),
            (
                PublishReleaseErrorKind::TestReport,
                request("validated-a", "report-red", &tested_hash),
            ),
            (
                PublishReleaseErrorKind::TestReport,
                request("validated-a", "report-b", &tested_hash),
            ),
            (
                PublishReleaseErrorKind::ReleaseConflict,
                request("validated-b", "report-b", &other_hash),
            ),
            (
                PublishReleaseErrorKind::EvidenceConflict,
                request("validated-a", "report-a2", &tested_hash),
            ),
        ] {
            let refusal = mint_tested_release(&mut admin, &attempt)
                .await
                .expect_err("a conflicting reuse refuses");
            assert_eq!(refusal.kind(), expected, "{refusal}");
            assert_eq!(control_counts(&admin).await, (1, 1, 0, 0));
        }

        // Step A sealed no release membership: that belongs to wamn-0h0g.15.14.
        // Membership is row-per-member now, so an unsealed release is an empty
        // catalog.release_flows rather than an empty snapshot column.
        let members: i64 = admin
            .query_one(
                "SELECT count(*) FROM catalog.release_flows \
                  WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = $3",
                &[&TENANT, &CATALOG_ID, &CATALOG_VERSION],
            )
            .await
            .expect("read the untouched release membership")
            .get(0);
        assert_eq!(members, 0);

        drop(admin);
        let _ = admin_task.await;
    }
}
