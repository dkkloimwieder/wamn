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
//! 7. the tested resolution map, derived from the release manifest this publish
//!    mints and canonicalized to exact RFC 8785 bytes.
//!
//! It appends exactly two rows: one `catalog.release_flows` member and one
//! `catalog.release_flow_test_evidence` row through the landed wamn-0h0g.9.9
//! routine. Every conflicting reuse is refused before the append it conflicts
//! with, nothing is committed on any refusal, and an exact retry returns the
//! release already minted with its original server-minted `created_at`.
//!
//! # `tested_resolution_map` is manifest-derived bytes, never the report's JSONB
//!
//! The map is `flow_id -> plan_hash` over the flows reachable from the flow being
//! published, read off [`ServingManifest`] — the same document a pod mounts. Its
//! authoritative form is the exact RFC 8785 bytes [`wamn_flow::canonical_json_bytes`]
//! produces, and identity is byte equality, never a reserialized projection
//! (wamn-0h0g.9.9, owner amendment 2026-08-15).
//!
//! Step A does **not** read a report-level resolution map (wamn-0h0g.15.29).
//! That producer was dead — wamn-0h0g.15.7 deleted the last writer of
//! `authoring_test_case_runs.resolution_map`, so the report-level map minted
//! `'{}'` on every report and reading it welded an empty evidence map to every
//! release — and wamn-0h0g.15.170 deleted the columns themselves. The map's
//! semantics survived: [`ServingManifest::reachable_flows`] is by its own doc the
//! replacement for the walk that used to build it, so the manifest is where it is
//! now sourced.
//!
//! # Why the reachable set, and not the whole release
//!
//! Evidence is per flow, and the gate claim it records is *this* flow exercised
//! against *its* callees. Scoping to [`ServingManifest::reachable_flows`] is also
//! what keeps an exact retry converging: a member is append-only at its coordinate
//! and every reachable callee was already released when step 6 verified it, so the
//! reachable set and its plan hashes cannot move. The whole-release map could —
//! publishing any unrelated flow would change it, and the next retry of *this*
//! publish would refuse itself as an `EvidenceConflict`.
//!
//! # Out of scope, by construction
//!
//! Step A touches no deployed map, no project binding or connection generation, no
//! project retention, no activation, no deployment attestation, no lifecycle
//! state, and no project relation at all. The attestation is wamn-0h0g.8.21's
//! transaction C. Both [`wamn_control_provision::publish_release`] and the source
//! audit below assert that rather than assuming it.
//!
//! # The release-manifest mint (wamn-0h0g.15.14)
//!
//! [`mint_release_manifest`] projects a release coordinate into the mounted
//! serving document ([`ServingManifest`]) and derives its `sha256:` identity from
//! the RFC 8785 canonical bytes it will actually ship. It is a pure read: it
//! appends nothing, so step A runs it inside its own transaction — after the
//! member append, so the flow just published is in the projection, and before the
//! commit, so no concurrent publisher can slip a member between the two. Since
//! wamn-0h0g.15.29 the tested-evidence map is derived from that same projection,
//! which is the other reason it has to run before the evidence append.
//!
//! The mint is the whole of wamn-0h0g.15.14. Pushing those bytes to OCI is
//! wamn-0h0g.15.97, writing them into the GitOps desired state is wamn-0h0g.15.98,
//! and the attestation that makes the digest a *release* rather than a candidate
//! is wamn-0h0g.8.21 (ruling wamn-0h0g.13.54: a digest is released iff a
//! deployment attestation references it, distinguishable by attestation absence,
//! with zero schema).

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio_postgres::{Client, Transaction};
use wamn_catalog::{
    AttachmentKind, CallFlowInstruction, ExecutionPlanV2, ManifestDigest,
    SERVING_MANIFEST_FORMAT_VERSION, ServingAttachment, ServingFlow, ServingManifest,
    ServingRegistration, ServingRelease, read_execution_plan,
};
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
///
/// `PartialEq` without `Eq`: the minted manifest carries attachment definition
/// documents as `serde_json::Value`, which is only partially equal.
#[derive(Clone, Debug, PartialEq)]
pub struct MintedTestedRelease {
    /// Server-minted immutable evidence timestamp; an exact retry returns it again.
    pub created_at: DateTime<Utc>,
    /// SHA-256 over the exact RFC 8785 tested-resolution-map bytes persisted.
    pub tested_resolution_map_hash: String,
    /// Whether THIS call minted the release member. `false` is an exact retry.
    pub minted: bool,
    /// The serving manifest this publish minted, projected from the release as it
    /// stands with this member appended (wamn-0h0g.15.14). Nothing here persists
    /// it: the OCI push, the GitOps write, and the attestation that turns the
    /// digest into a release are wamn-0h0g.15.97, .15.98 and .8.21.
    pub serving_manifest: MintedReleaseManifest,
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

    // A conflicting member refuses here, before either append.
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

    // 7 — the release-manifest mint (wamn-0h0g.15.14) and the tested resolution
    // map derived from it (wamn-0h0g.15.29). Both run here, after the member
    // append and inside the same transaction, because a projection taken before
    // the append would omit the flow this call publishes and one taken after the
    // commit would race the next publisher.
    //
    // The registration set is EMPTY here, and that is a stated limit rather than a
    // measurement: `catalog.event_registrations` exists only in the project
    // database and step A holds no project connection, so this transaction has no
    // way to observe one. See [`MintReleaseManifest::registrations`]. A caller that
    // ships these bytes must resolve that first — the mint is scoped to wamn-0h0g
    // .15.14 and the push, the desired-state write and the attestation are
    // wamn-0h0g.15.97, .15.98 and .8.21.
    let no_control_side_registrations = BTreeMap::new();
    let serving_manifest = mint_release_manifest(
        transaction,
        &MintReleaseManifest {
            tenant_id: request.tenant_id,
            catalog_id: request.catalog_id,
            catalog_version: request.catalog_version,
            candidate_draft_id: None,
            registrations: &no_control_side_registrations,
        },
    )
    .await?;
    let (map_bytes, map_hash) = tested_resolution_map(&serving_manifest.manifest, request.flow_id)?;

    // A conflicting evidence row refuses here, before the evidence append. It
    // cannot be checked any earlier: the map it is compared against is the one the
    // mint above just derived. Nothing is lost by the later position — step A is
    // one transaction, so a refusal here rolls the member append back with it.
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
        serving_manifest,
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

/// Derive the tested resolution map from the manifest this publish just minted.
///
/// The map is `flow_id -> plan_hash` over [`ServingManifest::reachable_flows`]
/// from the flow being published — the exact `flow_id -> execution_bundle_hash`
/// object, sourced from the manifest rather than from the report
/// (wamn-0h0g.15.29).
///
/// An empty reachable set is a refusal, not an empty map. That is the whole point
/// of the bead: the map this replaced could only ever be `'{}'` once
/// wamn-0h0g.15.7 deleted its last writer, and an empty map welded to a release is
/// evidence of nothing.
fn tested_resolution_map(
    manifest: &ServingManifest,
    root_flow_id: &str,
) -> Result<(Vec<u8>, String), PublishReleaseError> {
    let resolved = manifest.reachable_flows(root_flow_id);
    if resolved.is_empty() {
        return Err(PublishReleaseError::new(
            PublishReleaseErrorKind::TestedResolutionMap,
            format!(
                "the minted release manifest resolves no plan for flow {root_flow_id:?}, so this \
                 publish would record an empty tested resolution map"
            ),
        ));
    }
    // Driven off `manifest.flows` rather than off `resolved` so the map is built by
    // lookup-free iteration: every key it carries is a member that exists.
    let map = Value::Object(
        manifest
            .flows
            .iter()
            .filter(|(flow_id, _)| resolved.contains(flow_id.as_str()))
            .map(|(flow_id, flow)| (flow_id.clone(), Value::String(flow.plan_hash.clone())))
            .collect(),
    );
    Ok((
        wamn_flow::canonical_json_bytes(&map),
        wamn_flow::canonical_json_sha256(&map),
    ))
}

// ---------------------------------------------------------------------------
// The release-manifest mint (wamn-0h0g.15.14).
// ---------------------------------------------------------------------------

/// Which release one manifest mint projects, and the candidate it overlays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MintReleaseManifest<'a> {
    /// Tenant owning the release.
    pub tenant_id: &'a str,
    /// Catalog the release belongs to.
    pub catalog_id: &'a str,
    /// Release version being projected.
    pub catalog_version: i32,
    /// The `catalog.validated_flow_drafts.validated_draft_hash` whose flow
    /// overlays its released member, or `None` for a plain release mint. A
    /// candidate manifest is the same document minted from draft inputs — ruling
    /// wamn-0h0g.13.54 gives it no marker, so this is the *only* place the two
    /// cases differ.
    pub candidate_draft_id: Option<&'a str>,
    /// The event registrations this release serves, supplied rather than read.
    ///
    /// # This is a parameter because the relation is in the other database
    ///
    /// Every other input the mint needs is in the control database:
    /// `catalog.release_flows`, `flow_artifacts`, `execution_bundles`,
    /// `validated_flow_drafts`, `release_attachments` and `release_sources` are
    /// all in `deploy/sql/control-portable-store.sql`. `catalog.event_registrations`
    /// is **not** — it exists only in `deploy/sql/catalog-schema.sql`, the project
    /// database's copy. Measured on PostgreSQL 18.6 by applying the control
    /// bootstrap and listing `information_schema.tables`.
    ///
    /// Publish step A holds no project connection and claims no atomicity across
    /// the two databases, by its own stated contract, so it cannot read them.
    /// Making the set an argument keeps that fact at every call site instead of
    /// letting an absent read look like an empty catalog — which is precisely the
    /// dual-representation under-report wamn-0h0g.15.159 was spent deleting.
    /// Where the projection is finally sourced is an owner decision, reported at
    /// this bead's close.
    pub registrations: &'a BTreeMap<String, ServingRegistration>,
}

/// One minted serving manifest: the document, its exact bytes, and their name.
///
/// The bytes are what a `release-manifest-<digest>` ConfigMap carries verbatim and
/// what an OCI push uploads; the digest is derived from those same bytes rather
/// than asserted against them.
#[derive(Clone, Debug, PartialEq)]
pub struct MintedReleaseManifest {
    /// The admitted document.
    pub manifest: ServingManifest,
    /// `sha256:` over [`Self::canonical_bytes`].
    pub digest: ManifestDigest,
    /// The exact RFC 8785 canonical bytes the digest names.
    pub canonical_bytes: Vec<u8>,
}

/// Stable prefix every release-manifest mint refusal renders with.
pub const RELEASE_MANIFEST_MINT_REFUSAL: &str = "release-manifest-mint-refused";

/// Which predicate refused a release-manifest mint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MintManifestErrorKind {
    /// The control read itself failed; nothing was projected.
    Storage,
    /// No release identity row exists at the coordinate, so there is no release
    /// to project. This is also what a mint with no claimed tenant refuses as,
    /// because every relation it reads forces row-level security.
    Release,
    /// A member's stored plan is absent, fails its hash, or does not parse.
    PlanBytes,
    /// The named validated draft is absent, or belongs to another release.
    CandidateDraft,
    /// A stored attachment or registration row does not project into the frozen
    /// serving shape.
    Projection,
    /// The projected document is not an admissible serving manifest — a dangling
    /// call edge, a zero version, a malformed digest, or bytes no delivery path
    /// can carry.
    Document,
}

impl MintManifestErrorKind {
    /// Stable label for logs, refusal literals, and tests.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Storage => "storage",
            Self::Release => "release",
            Self::PlanBytes => "plan-bytes",
            Self::CandidateDraft => "candidate-draft",
            Self::Projection => "projection",
            Self::Document => "document",
        }
    }
}

/// One refused release-manifest mint, carrying the predicate and its context.
#[derive(Debug)]
pub struct MintManifestError {
    kind: MintManifestErrorKind,
    detail: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl MintManifestError {
    /// Refuse with a stable predicate and the context that decided it.
    pub fn new(kind: MintManifestErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
            source: None,
        }
    }

    /// Refuse with an upstream cause retained as this error's source.
    pub fn with_source(
        kind: MintManifestErrorKind,
        detail: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind,
            detail: detail.into(),
            source: Some(Box::new(source)),
        }
    }

    /// Which predicate refused.
    pub const fn kind(&self) -> MintManifestErrorKind {
        self.kind
    }

    /// The exact context the predicate refused on.
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl std::fmt::Display for MintManifestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{RELEASE_MANIFEST_MINT_REFUSAL} ({}): {}",
            self.kind.as_str(),
            self.detail
        )
    }
}

impl std::error::Error for MintManifestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.source {
            Some(source) => Some(&**source),
            None => None,
        }
    }
}

/// Translate a mint refusal once, at step A's boundary.
///
/// Four predicates map onto the step-A predicate that means the same thing.
/// `Projection` and `Document` have no counterpart: they say the release at this
/// coordinate does not project into a deliverable serving manifest, which is why
/// they land on `ReleaseCoordinate`. That coarsening is deliberate and reported —
/// the honest fix is two more [`PublishReleaseErrorKind`] variants, and that enum
/// lives in `wamn-control-provision`, outside this bead's file domain. Nothing is
/// lost in the meantime: the exact predicate survives on the retained source.
impl From<MintManifestError> for PublishReleaseError {
    fn from(error: MintManifestError) -> Self {
        let kind = match error.kind() {
            MintManifestErrorKind::Storage => PublishReleaseErrorKind::Storage,
            MintManifestErrorKind::PlanBytes => PublishReleaseErrorKind::PlanBytes,
            MintManifestErrorKind::CandidateDraft => PublishReleaseErrorKind::ValidatedDraft,
            MintManifestErrorKind::Release
            | MintManifestErrorKind::Projection
            | MintManifestErrorKind::Document => PublishReleaseErrorKind::ReleaseCoordinate,
        };
        let detail = format!("the release manifest mint refused: {error}");
        Self::with_source(kind, detail, error)
    }
}

/// Every release member's identity, its source artifact, and its exact plan bytes.
const SELECT_MANIFEST_MEMBERS_SQL: &str = "\
     SELECT member.flow_id, member.flow_version, member.execution_bundle_hash, \
            artifact.artifact_hash, bundle.exact_bytes \
       FROM catalog.release_flows AS member \
       JOIN catalog.flow_artifacts AS artifact \
         ON artifact.tenant_id = member.tenant_id AND artifact.flow_id = member.flow_id \
        AND artifact.flow_version = member.flow_version \
       JOIN catalog.execution_bundles AS bundle \
         ON bundle.tenant_id = member.tenant_id \
        AND bundle.execution_bundle_hash = member.execution_bundle_hash \
      WHERE member.tenant_id = $1 AND member.catalog_id = $2 \
        AND member.catalog_version = $3 \
      ORDER BY member.flow_id COLLATE \"C\"";

/// Every release attachment with the source document it resolved against.
///
/// The join to `catalog.release_sources` is 1:1 and total: the attachment's
/// `(tenant, catalog, version, source_id)` is a foreign key onto that table's
/// primary key, so an inner join can neither drop nor duplicate a row.
const SELECT_MANIFEST_ATTACHMENTS_SQL: &str = "\
     SELECT exposed.attachment_id, exposed.attachment_kind, exposed.flow_id, \
            exposed.definition_hash, exposed.definition_json::text, \
            source.definition_json::text \
       FROM catalog.release_attachments AS exposed \
       JOIN catalog.release_sources AS source \
         ON source.tenant_id = exposed.tenant_id \
        AND source.catalog_id = exposed.catalog_id \
        AND source.catalog_version = exposed.catalog_version \
        AND source.source_id = exposed.source_id \
      WHERE exposed.tenant_id = $1 AND exposed.catalog_id = $2 \
        AND exposed.catalog_version = $3 \
      ORDER BY exposed.attachment_id COLLATE \"C\"";

/// The candidate draft's own plan, artifact, and the released base its
/// connection requirements were resolved under.
const SELECT_CANDIDATE_DRAFT_SQL: &str = "\
     SELECT draft.catalog_id, draft.catalog_version, draft.flow_id, \
            draft.runtime_flow_version, draft.draft_artifact_hash, \
            draft.execution_bundle_hash, draft.binding_base_artifact_hash, \
            bundle.exact_bytes \
       FROM catalog.validated_flow_drafts AS draft \
       JOIN catalog.execution_bundles AS bundle \
         ON bundle.tenant_id = draft.tenant_id \
        AND bundle.execution_bundle_hash = draft.execution_bundle_hash \
      WHERE draft.tenant_id = $1 AND draft.validated_draft_hash = $2 \
        FOR SHARE OF draft, bundle";

/// Project one release coordinate into its serving manifest and name the bytes.
///
/// # Why this takes a transaction
///
/// Every relation read here forces row-level security, and the publishing tenant
/// is claimed with a **transaction-local** `set_config`
/// ([`claim_publishing_tenant_sql`]). On an autocommit connection that claim
/// expires with the statement that set it, so each following read would see zero
/// rows and the mint would emit a structurally valid manifest with no members and
/// a real digest. Taking a transaction is what makes the claim outlive the claim
/// statement; probing the release identity row first is what makes an unclaimed
/// mint refuse instead of under-reporting.
///
/// # What it deliberately does not read
///
/// Attachment *activation* and attachment *tombstones* are environment-owned
/// operational state, and neither reaches these bytes. Activation is absent by
/// the frozen schema's own rule — a disabled attachment leaves
/// `catalog.active_attachments` and refuses at admission by row absence, so an
/// emergency off stays one statement rather than a mint plus a rollout. Tombstones
/// are absent because they cannot apply: `catalog.apply_release_exposure` raises
/// `tombstoned-attachment-id` when a version being applied exposes an
/// already-tombstoned id, so a tombstoned id is never a member of a release the
/// gate admitted. Filtering on them anyway would be worse than redundant — a
/// tombstone is written by a *later* publication, so re-minting an older release
/// would then name different content than the first mint did, and the digest
/// would stop being a function of the release.
///
/// It reads no event registration either, for a different and blunter reason:
/// that relation is not in this database. See
/// [`MintReleaseManifest::registrations`].
pub async fn mint_release_manifest(
    transaction: &Transaction<'_>,
    request: &MintReleaseManifest<'_>,
) -> Result<MintedReleaseManifest, MintManifestError> {
    transaction
        .query_one(claim_publishing_tenant_sql(), &[&request.tenant_id])
        .await
        .map_err(|error| mint_storage("claim the projecting tenant", error))?;

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
        .map_err(|error| mint_storage("lock the projected release coordinate", error))?
    else {
        return Err(MintManifestError::new(
            MintManifestErrorKind::Release,
            format!(
                "catalog {:?} version {} has no release to project",
                request.catalog_id, request.catalog_version
            ),
        ));
    };
    let catalog_version = u32::try_from(request.catalog_version).map_err(|error| {
        MintManifestError::with_source(
            MintManifestErrorKind::Release,
            format!(
                "catalog version {} is not a serving-manifest version",
                request.catalog_version
            ),
            error,
        )
    })?;

    let mut flows = project_release_members(transaction, request).await?;
    if let Some(candidate_draft_id) = request.candidate_draft_id {
        let (flow_id, overlay) =
            project_candidate_overlay(transaction, request, candidate_draft_id).await?;
        flows.insert(flow_id, overlay);
    }

    let projected = ServingManifest {
        format_version: SERVING_MANIFEST_FORMAT_VERSION.to_string(),
        release: ServingRelease {
            tenant_id: request.tenant_id.to_string(),
            catalog_id: request.catalog_id.to_string(),
            catalog_version,
            environment: release.get(0),
        },
        flows,
        attachments: project_release_attachments(transaction, request).await?,
        registrations: request.registrations.clone(),
    };

    // The mint admits its own output through the reader's one entry point, so the
    // bytes it hands on are exactly the bytes a pod will accept and the digest is
    // derived from them rather than asserted about them.
    let canonical_bytes = projected.canonical_bytes();
    let (manifest, digest) =
        ServingManifest::from_canonical_bytes(&canonical_bytes).map_err(|error| {
            MintManifestError::with_source(
                MintManifestErrorKind::Document,
                format!(
                    "catalog {:?} version {} does not project a deliverable serving manifest",
                    request.catalog_id, request.catalog_version
                ),
                error,
            )
        })?;
    Ok(MintedReleaseManifest {
        manifest,
        digest,
        canonical_bytes,
    })
}

fn mint_storage(context: &'static str, error: tokio_postgres::Error) -> MintManifestError {
    MintManifestError::with_source(MintManifestErrorKind::Storage, context, error)
}

/// Project every released member. `binding-base-artifact` is the member's own
/// `source-artifact` here, and only here: a released flow's connection
/// requirements are resolved under the artifact it was released as.
async fn project_release_members(
    transaction: &Transaction<'_>,
    request: &MintReleaseManifest<'_>,
) -> Result<BTreeMap<String, ServingFlow>, MintManifestError> {
    let rows = transaction
        .query(
            SELECT_MANIFEST_MEMBERS_SQL,
            &[
                &request.tenant_id,
                &request.catalog_id,
                &request.catalog_version,
            ],
        )
        .await
        .map_err(|error| mint_storage("read the release members", error))?;

    let mut flows = BTreeMap::new();
    for row in rows {
        let flow_id: String = row.get(0);
        let flow_version: i32 = row.get(1);
        let plan_hash: String = row.get(2);
        let source_artifact: String = row.get(3);
        let exact_bytes: Vec<u8> = row.get(4);
        let plan = parse_member_plan(&flow_id, &plan_hash, &exact_bytes)?;
        flows.insert(
            flow_id.clone(),
            ServingFlow {
                flow_version: member_version(&flow_id, flow_version)?,
                plan_hash,
                binding_base_artifact: source_artifact.clone(),
                source_artifact,
                callable_contract: plan.body.callable_contract.clone(),
                calls: member_calls(&flow_id, &plan)?,
            },
        );
    }
    Ok(flows)
}

/// Project the candidate overlay this mint carries.
///
/// The overlay is the whole reason `binding-base-artifact` exists (ruling
/// wamn-0h0g.15.62). A candidate's plan is compiled from the draft's own artifact,
/// so `source-artifact` must be the draft hash or the plan verifiers refuse the
/// snapshot; but a draft artifact can never own a connection-binding row, so
/// readiness and effect authority have to resolve under the released base the
/// draft was validated against. Both values travel, and they differ.
async fn project_candidate_overlay(
    transaction: &Transaction<'_>,
    request: &MintReleaseManifest<'_>,
    candidate_draft_id: &str,
) -> Result<(String, ServingFlow), MintManifestError> {
    let Some(row) = transaction
        .query_opt(
            SELECT_CANDIDATE_DRAFT_SQL,
            &[&request.tenant_id, &candidate_draft_id],
        )
        .await
        .map_err(|error| mint_storage("lock the candidate validated draft", error))?
    else {
        return Err(MintManifestError::new(
            MintManifestErrorKind::CandidateDraft,
            format!("this tenant has no validated draft {candidate_draft_id:?}"),
        ));
    };
    let draft_catalog_id: String = row.get(0);
    let draft_catalog_version: i32 = row.get(1);
    if draft_catalog_id != request.catalog_id || draft_catalog_version != request.catalog_version {
        return Err(MintManifestError::new(
            MintManifestErrorKind::CandidateDraft,
            format!(
                "validated draft {candidate_draft_id:?} was validated against catalog \
                 {draft_catalog_id:?} version {draft_catalog_version}, not {:?} version {}",
                request.catalog_id, request.catalog_version
            ),
        ));
    }
    let flow_id: String = row.get(2);
    let flow_version: i32 = row.get(3);
    let source_artifact: String = row.get(4);
    let plan_hash: String = row.get(5);
    let binding_base_artifact: String = row.get(6);
    let exact_bytes: Vec<u8> = row.get(7);
    let plan = parse_member_plan(&flow_id, &plan_hash, &exact_bytes)?;
    let overlay = ServingFlow {
        flow_version: member_version(&flow_id, flow_version)?,
        plan_hash,
        source_artifact,
        binding_base_artifact,
        callable_contract: plan.body.callable_contract.clone(),
        calls: member_calls(&flow_id, &plan)?,
    };
    Ok((flow_id, overlay))
}

/// Read one member's stored plan, comparing exact bytes before parsing them.
fn parse_member_plan(
    flow_id: &str,
    plan_hash: &str,
    exact_bytes: &[u8],
) -> Result<ExecutionPlanV2, MintManifestError> {
    read_execution_plan(plan_hash, exact_bytes).map_err(|error| {
        MintManifestError::with_source(
            MintManifestErrorKind::PlanBytes,
            format!("release member {flow_id:?} stores no valid plan at {plan_hash}"),
            error,
        )
    })
}

/// The call-edge adjacency one member contributes.
fn member_calls(
    flow_id: &str,
    plan: &ExecutionPlanV2,
) -> Result<BTreeSet<String>, MintManifestError> {
    call_flow_callees(plan).map_err(|error| {
        MintManifestError::with_source(
            MintManifestErrorKind::PlanBytes,
            format!("release member {flow_id:?} has a call-flow node naming no callee"),
            error,
        )
    })
}

/// Narrow a stored flow version onto the serving shape's unsigned one.
fn member_version(flow_id: &str, flow_version: i32) -> Result<u32, MintManifestError> {
    u32::try_from(flow_version).map_err(|error| {
        MintManifestError::with_source(
            MintManifestErrorKind::Projection,
            format!("release member {flow_id:?} stores flow version {flow_version}"),
            error,
        )
    })
}

/// Project every release attachment and the source document it resolved against.
async fn project_release_attachments(
    transaction: &Transaction<'_>,
    request: &MintReleaseManifest<'_>,
) -> Result<BTreeMap<String, ServingAttachment>, MintManifestError> {
    let rows = transaction
        .query(
            SELECT_MANIFEST_ATTACHMENTS_SQL,
            &[
                &request.tenant_id,
                &request.catalog_id,
                &request.catalog_version,
            ],
        )
        .await
        .map_err(|error| mint_storage("read the release attachments", error))?;

    let mut attachments = BTreeMap::new();
    for row in rows {
        let attachment_id: String = row.get(0);
        let stored_kind: String = row.get(1);
        let kind = serde_json::from_value::<AttachmentKind>(Value::String(stored_kind.clone()))
            .map_err(|error| {
                MintManifestError::with_source(
                    MintManifestErrorKind::Projection,
                    format!("attachment {attachment_id:?} carries unknown kind {stored_kind:?}"),
                    error,
                )
            })?;
        let definition = stored_document(&attachment_id, "definition", row.get(4))?;
        let auth_policy = stored_document(&attachment_id, "resolved source", row.get(5))?;
        attachments.insert(
            attachment_id,
            ServingAttachment {
                kind,
                flow_id: row.get(2),
                definition_hash: row.get(3),
                definition,
                auth_policy,
            },
        );
    }
    Ok(attachments)
}

/// Decode one stored jsonb document the projection carries verbatim.
fn stored_document(
    attachment_id: &str,
    field: &'static str,
    stored: String,
) -> Result<Value, MintManifestError> {
    serde_json::from_str(&stored).map_err(|error| {
        MintManifestError::with_source(
            MintManifestErrorKind::Projection,
            format!("attachment {attachment_id:?} stores an unreadable {field}"),
            error,
        )
    })
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

    const CURRENT_DATABASE_PUBLIC_CONNECT_SQL: &str =
        include_str!("../../../test-support/fixtures/sql/current-database-public-connect.sql");

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

    /// The wamn-0h0g.9.9 frozen tested-resolution-map vector.
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

    /// Build a manifest carrying exactly these `(flow_id, plan_hash, callees)`.
    fn manifest_of(members: &[(&str, &str, &[&str])]) -> ServingManifest {
        ServingManifest {
            format_version: SERVING_MANIFEST_FORMAT_VERSION.to_string(),
            release: ServingRelease {
                tenant_id: TENANT.to_string(),
                catalog_id: CATALOG_ID.to_string(),
                catalog_version: 1,
                environment: ENVIRONMENT.to_string(),
            },
            flows: members
                .iter()
                .map(|(flow_id, plan_hash, calls)| {
                    (
                        (*flow_id).to_string(),
                        ServingFlow {
                            flow_version: 1,
                            plan_hash: (*plan_hash).to_string(),
                            source_artifact: ROOT_ARTIFACT.to_string(),
                            binding_base_artifact: ROOT_ARTIFACT.to_string(),
                            callable_contract: None,
                            calls: calls.iter().map(|call| (*call).to_string()).collect(),
                        },
                    )
                })
                .collect(),
            attachments: BTreeMap::new(),
            registrations: BTreeMap::new(),
        }
    }

    const PLAN_A: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const PLAN_Z: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const PLAN_OFF_PATH: &str =
        "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

    /// wamn-0h0g.15.29: the evidence map is the manifest's reachable
    /// `flow_id -> plan_hash` projection, in exact RFC 8785 bytes.
    ///
    /// The expected value is the frozen wamn-0h0g.9.9 vector unchanged — the map's
    /// *source* moved, its bytes did not. `flow-off-path` is a member of the same
    /// release that nothing reaches from the root, and its absence is what makes
    /// this a per-flow gate claim rather than a snapshot of the whole release.
    #[test]
    fn the_evidence_map_is_the_reachable_flow_to_plan_hash_projection() {
        let manifest = manifest_of(&[
            ("flow-a", PLAN_A, &["flow-z"]),
            ("flow-z", PLAN_Z, &[]),
            ("flow-off-path", PLAN_OFF_PATH, &[]),
        ]);
        let (bytes, hash) =
            tested_resolution_map(&manifest, "flow-a").expect("the root resolves a plan");
        assert_eq!(bytes, CANONICAL_MAP);
        assert_eq!(hash, CANONICAL_MAP_HASH);
        assert!(
            !String::from_utf8(bytes)
                .expect("the map is UTF-8")
                .contains("flow-off-path")
        );

        // PostgreSQL renders `jsonb::text` with spaces, so the report's own hash
        // over that rendering could never have been this evidence hash anyway.
        let jsonb_rendering = String::from_utf8(CANONICAL_MAP.to_vec())
            .expect("the canonical map is UTF-8")
            .replace("\":", "\": ")
            .replace(",\"", ", \"");
        assert_ne!(jsonb_rendering.as_bytes(), CANONICAL_MAP);
        assert_eq!(
            wamn_flow::canonical_json_sha256(
                &serde_json::from_str::<Value>(&jsonb_rendering).expect("the rendering is JSON")
            ),
            CANONICAL_MAP_HASH
        );

        // A root with no callees resolves exactly itself, never nothing.
        let (solo, _) = tested_resolution_map(&manifest, "flow-off-path")
            .expect("a member with no callees still resolves itself");
        assert_eq!(
            solo,
            format!(r#"{{"flow-off-path":"{PLAN_OFF_PATH}"}}"#).into_bytes()
        );

        // A root the manifest does not carry refuses. This is the whole bead: the
        // empty map is a refusal, never evidence.
        let refusal = tested_resolution_map(&manifest, "absent-from-the-release")
            .expect_err("an unresolvable root refuses");
        assert_eq!(refusal.kind(), PublishReleaseErrorKind::TestedResolutionMap);
        assert!(refusal.detail().contains("empty tested resolution map"));
        assert_eq!(
            tested_resolution_map(&manifest_of(&[]), "flow-a")
                .expect_err("an empty release refuses")
                .kind(),
            PublishReleaseErrorKind::TestedResolutionMap
        );
    }

    /// wamn-0h0g.15.29: step A must never re-source the map from the report.
    #[test]
    fn step_a_never_reads_the_reports_own_resolution_map() {
        let source = include_str!("publish_release.rs");
        let implementation = source
            .split("#[cfg(test)]")
            .next()
            .expect("the module has an implementation");
        // wamn-0h0g.15.7 deleted the last writer of the case-level map, so the
        // report-level map is `'{}'` on every report. Reading it welds an empty
        // evidence map to every release, which is the defect this bead closed.
        for resurrected in [
            "resolution_map::text",
            "report.resolution_map",
            "stored_resolution_map",
            "canonical_tested_resolution_map",
        ] {
            assert!(
                !implementation.contains(resurrected),
                "step A re-sourced the tested map from the report via {resurrected}"
            );
        }
        assert!(!lock_test_report_sql().contains("resolution_map"));
        // The map comes from the manifest, and the derivation is the only producer.
        assert_eq!(
            implementation
                .matches("tested_resolution_map(&serving_manifest")
                .count(),
            1
        );
        assert!(implementation.contains("manifest.reachable_flows(root_flow_id)"));
        // The mint has to precede the evidence append, or there is no map to append.
        let publish = implementation
            .split("async fn publish(")
            .nth(1)
            .expect("the module defines step A");
        let mint = publish
            .find("mint_release_manifest(")
            .expect("step A mints the manifest");
        let evidence = publish
            .find("register_release_evidence_sql()")
            .expect("step A appends evidence");
        let member = publish
            .find("insert_release_flow_sql()")
            .expect("step A appends the member");
        assert!(member < mint, "the mint must follow the member append");
        assert!(mint < evidence, "the mint must precede the evidence append");
    }

    /// The mint reads exactly four relations, and every one of them has to be in
    /// the database step A connects to.
    ///
    /// `catalog.event_registrations` is not, which is why the registration set is
    /// an argument rather than a read — measured on PostgreSQL 18.6, pinned here
    /// so a later lane cannot quietly add the read back and get an empty map on
    /// every control database instead of a refusal.
    #[test]
    fn every_relation_the_mint_reads_is_in_the_control_portable_store() {
        const CONTROL_STORE: &str = include_str!("../../../deploy/sql/control-portable-store.sql");
        let source = include_str!("publish_release.rs");
        let implementation = source
            .split("#[cfg(test)]")
            .next()
            .expect("the module has an implementation");

        for relation in [
            "catalog.release_flows",
            "catalog.flow_artifacts",
            "catalog.execution_bundles",
            "catalog.release_attachments",
            "catalog.release_sources",
            "catalog.validated_flow_drafts",
        ] {
            assert!(
                CONTROL_STORE.contains(&format!("CREATE TABLE IF NOT EXISTS {relation} (")),
                "the mint reads {relation}, which the control store does not carry"
            );
        }
        assert!(
            !CONTROL_STORE.contains("catalog.event_registrations"),
            "catalog.event_registrations reached the control store: the registration \
             projection can stop being an argument and become a read"
        );
        // Prose may name the relation; a statement may not.
        assert!(
            !implementation.contains("FROM catalog.event_registrations"),
            "the mint reads event_registrations from a database that does not have it, \
             so every control-side release would project an empty registration set"
        );

        // The member read INNER JOINs each member to its artifact and to its plan
        // bytes. Both joins are total only because `catalog.release_flows` carries
        // a foreign key onto each one's primary key; lose either and the join
        // silently DROPS that member instead of refusing — the same under-report
        // wamn-0h0g.15.159 spent a bead deleting, except invisible, because a
        // manifest missing a member is still a valid manifest with a real digest.
        for target in [
            "REFERENCES catalog.flow_artifacts (tenant_id, flow_id, flow_version)",
            "REFERENCES catalog.execution_bundles (tenant_id, execution_bundle_hash)",
        ] {
            assert!(
                CONTROL_STORE.contains(target),
                "release_flows lost its {target}: the member read's inner join can now \
                 drop a member instead of refusing"
            );
        }
    }

    /// Every mint predicate translates exactly once, at step A's boundary, and the
    /// precise predicate survives the two that have no step-A counterpart.
    #[test]
    fn every_mint_refusal_translates_once_at_step_a() {
        for (mint, step_a) in [
            (
                MintManifestErrorKind::Storage,
                PublishReleaseErrorKind::Storage,
            ),
            (
                MintManifestErrorKind::Release,
                PublishReleaseErrorKind::ReleaseCoordinate,
            ),
            (
                MintManifestErrorKind::PlanBytes,
                PublishReleaseErrorKind::PlanBytes,
            ),
            (
                MintManifestErrorKind::CandidateDraft,
                PublishReleaseErrorKind::ValidatedDraft,
            ),
            (
                MintManifestErrorKind::Projection,
                PublishReleaseErrorKind::ReleaseCoordinate,
            ),
            (
                MintManifestErrorKind::Document,
                PublishReleaseErrorKind::ReleaseCoordinate,
            ),
        ] {
            let translated =
                PublishReleaseError::from(MintManifestError::new(mint, "why it refused"));
            assert_eq!(translated.kind(), step_a);
            // The coarsened pair keeps its exact predicate in the detail, which is
            // the only reason the coarsening is admissible.
            assert!(
                translated.detail().contains(mint.as_str()),
                "{} lost its predicate: {translated}",
                mint.as_str()
            );
            assert!(translated.detail().contains("why it refused"));
        }
        assert_eq!(
            RELEASE_MANIFEST_MINT_REFUSAL,
            "release-manifest-mint-refused"
        );
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
            .batch_execute(&format!(
                "{CURRENT_DATABASE_PUBLIC_CONNECT_SQL} \
                 DROP SCHEMA IF EXISTS catalog CASCADE; \
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
                   'GRANT CREATE ON DATABASE %I TO wamn_system', current_database()); END $$;"
            ))
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
                    passed, summary) \
                 VALUES ($1, $2, $3, $4, $5, $6, '{}'::jsonb)",
                &[
                    &TENANT,
                    &report_id,
                    &draft_id,
                    &CATALOG_ID,
                    &CATALOG_VERSION,
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
        assert_eq!(control_counts(&admin).await, (1, 1, 0, 0));

        // wamn-0h0g.15.29, the regression this bead closed. The evidence map is
        // the manifest's reachable `flow_id -> plan_hash` projection — here the one
        // member this publish appended — and NOT the `'{}'` the report carries.
        let expected_map = {
            let mut object = serde_json::Map::new();
            object.insert(FLOW_ID.to_string(), Value::String(tested_hash.clone()));
            Value::Object(object)
        };
        let expected_bytes = wamn_flow::canonical_json_bytes(&expected_map);
        assert_eq!(
            expected_bytes,
            format!(r#"{{"{FLOW_ID}":"{tested_hash}"}}"#).into_bytes()
        );
        assert_eq!(
            minted.tested_resolution_map_hash,
            wamn_flow::canonical_json_sha256(&expected_map)
        );

        let evidence = admin
            .query_one(
                "SELECT evidence.tested_resolution_map_bytes, \
                        evidence.tested_resolution_map_hash \
                   FROM catalog.release_flow_test_evidence AS evidence \
                  WHERE evidence.tenant_id = $1",
                &[&TENANT],
            )
            .await
            .expect("read the minted evidence");
        // wamn-0h0g.15.186: the report-level map was retired by .15.170. The
        // evidence row's exact producer-derived bytes are the remaining proof
        // that step A never substitutes an artificial empty map.
        assert_ne!(evidence.get::<_, Vec<u8>>(0), b"{}".to_vec());
        assert_eq!(evidence.get::<_, Vec<u8>>(0), expected_bytes);
        assert_eq!(
            evidence.get::<_, String>(1),
            minted.tested_resolution_map_hash
        );

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

        // The release-manifest mint this publish performed (wamn-0h0g.15.14).
        // Membership is row-per-member since wamn-0h0g.15.159, so the release the
        // manifest projects is exactly the one member step A just appended.
        let manifest = &minted.serving_manifest;
        assert_eq!(
            manifest.manifest.release,
            ServingRelease {
                tenant_id: TENANT.to_string(),
                catalog_id: CATALOG_ID.to_string(),
                catalog_version: 1,
                environment: ENVIRONMENT.to_string(),
            }
        );
        assert_eq!(
            manifest.manifest.flows.keys().collect::<Vec<_>>(),
            vec![FLOW_ID]
        );
        let member = &manifest.manifest.flows[FLOW_ID];
        assert_eq!(member.flow_version, 2);
        assert_eq!(member.plan_hash, tested_hash);
        assert_eq!(member.source_artifact, ROOT_ARTIFACT);
        // A released member resolves its bindings under its own artifact.
        assert_eq!(member.binding_base_artifact, ROOT_ARTIFACT);
        assert!(member.callable_contract.is_some());
        assert!(member.calls.is_empty());
        assert!(manifest.manifest.attachments.is_empty());
        assert!(manifest.manifest.registrations.is_empty());

        // The bytes round-trip through the one reader entry point, and the digest
        // they derive is the one the mint handed back.
        assert_eq!(
            ServingManifest::from_canonical_bytes(&manifest.canonical_bytes),
            Ok((manifest.manifest.clone(), manifest.digest.clone()))
        );

        // A repeated mint over identical content is byte-identical, so the
        // exact retry above already re-minted the same name.
        assert_eq!(
            retried.serving_manifest.canonical_bytes,
            manifest.canonical_bytes
        );
        assert_eq!(retried.serving_manifest.digest, manifest.digest);

        drop(admin);
        let _ = admin_task.await;
    }
}
