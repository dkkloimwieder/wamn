//! Publish step A — the control-release statement set and its refusals
//! (wamn-0h0g.8.7).
//!
//! Step A is the whole publish gate that lives in the control database. It locks
//! and verifies the exact immutable validated draft and its green finalized
//! report, verifies that draft's own plan bytes, source artifact, release
//! coordinate and environment, and then mints exactly one release member plus one
//! tested-evidence row. Its idempotency key is the `(validated draft, report)`
//! pair, so an exact retry returns the release it already minted and a
//! conflicting reuse refuses before any mutation.
//!
//! Everything here is pure, following this crate's contract: the statements are
//! text and the decisions are comparisons over already-read rows. The
//! `publish_release` driver in `wamn-ctl` holds the one control transaction, the
//! `wamn-catalog` plan reader, and the RFC 8785 canonicalizer.
//!
//! # The release coordinate is derived, never chosen
//!
//! Every coordinate a caller supplies is an assertion checked against
//! `catalog.validated_flow_drafts`: the catalog, its version, the environment,
//! the flow, its runtime version, the source artifact, and the plan. That is what
//! makes `(draft, report)` a real key — if the target release version were the
//! caller's free choice, one draft and report could mint two releases and "a
//! release is never reminted" would be unenforceable.
//!
//! # What step A deliberately does not touch
//!
//! No statement here names a project relation, a deployment attestation, a
//! project binding or connection generation, an attachment activation, or any
//! lifecycle state, and none of them is an `UPDATE` or a `DELETE`. Publish A is a
//! control-only append: the deployment attestation is wamn-0h0g.8.21's
//! transaction C, and the release-manifest mint is wamn-0h0g.15.14's.
//!
//! # Principal
//!
//! These statements run as the portable store's OWNER.
//! `deploy/sql/control-portable-store.sql` asserts at apply time that
//! `wamn_control_author` holds no privilege on
//! `catalog.release_flow_test_evidence` and no `EXECUTE` on
//! `catalog.register_release_flow_test_evidence`, so the author cannot be the
//! publisher. Row locking says the same thing independently: PostgreSQL requires
//! `UPDATE` on at least one column for `SELECT … FOR SHARE`, and the author holds
//! only `SELECT`/`INSERT` on the append-only facts step A locks.
//!
//! ## SR12 — what these pure tests cannot cover
//!
//! The tests below exercise which statement runs and what it compares. They
//! cannot exercise the lock manager: `FOR SHARE` cannot gap-lock an absent row at
//! READ COMMITTED, so a concurrent publish can still win between the driver's
//! pre-read and its insert. The driver rechecks after the insert for exactly that
//! reason, and only the live control-PostgreSQL transaction test observes it.

use std::fmt;

/// Everything one publish-step-A call asserts about the release it mints.
///
/// Each field is compared against the immutable control record rather than
/// trusted, so a stale or hand-edited request refuses instead of publishing a
/// release nothing tested.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PublishTestedRelease<'a> {
    /// Tenant owning the draft, the report, and the release.
    pub tenant_id: &'a str,
    /// Catalog the tested draft was validated against.
    pub catalog_id: &'a str,
    /// Catalog version the tested draft was validated against.
    pub catalog_version: i32,
    /// Environment that catalog version belongs to.
    pub environment: &'a str,
    /// Flow the release member publishes.
    pub flow_id: &'a str,
    /// Runtime flow version the release member publishes.
    pub flow_version: i32,
    /// `catalog.validated_flow_drafts.validated_draft_hash` being published.
    pub validated_draft_id: &'a str,
    /// Green finalized `wamn_run.authoring_test_reports.report_id`.
    pub report_id: &'a str,
    /// Immutable source-artifact hash of the flow being released.
    pub source_artifact_hash: &'a str,
    /// Byte identity of the flow's own execution plan.
    pub execution_bundle_hash: &'a str,
}

/// The immutable validated-draft facts step A locks and compares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedDraftFacts {
    /// Catalog the draft was validated against.
    pub catalog_id: String,
    /// Catalog version the draft was validated against.
    pub catalog_version: i32,
    /// Environment recorded on the validation.
    pub environment: String,
    /// Flow the draft validates.
    pub flow_id: String,
    /// Runtime flow version the validation pinned.
    pub runtime_flow_version: i32,
    /// Source-artifact hash the validation pinned.
    pub draft_artifact_hash: String,
    /// Own-plan byte identity the validation pinned.
    pub execution_bundle_hash: String,
}

/// The finalized test-report facts step A locks and compares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestReportFacts {
    /// Validated draft the report ran against.
    pub validated_draft_id: String,
    /// Catalog the report pinned.
    pub catalog_id: String,
    /// Catalog version the report pinned.
    pub catalog_version: i32,
    /// Whether every case in the report passed.
    pub passed: bool,
}

/// The release-member facts step A compares before it mints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseMemberFacts {
    /// Flow version the coordinate already publishes.
    pub flow_version: i32,
    /// Plan the coordinate already publishes.
    pub execution_bundle_hash: String,
}

/// The tested-evidence facts step A compares before it registers.
///
/// `tested_resolution_map_bytes` is the authoritative RFC 8785 byte sequence
/// (wamn-0h0g.9.9). It is compared byte for byte; the derived hash is compared as
/// well so a stored pair that disagrees with itself can never read as an exact
/// retry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseEvidenceFacts {
    /// Validated draft the stored evidence names.
    pub validated_draft_id: String,
    /// Report the stored evidence names.
    pub report_id: String,
    /// Source artifact the stored evidence names.
    pub source_artifact_hash: String,
    /// Plan the stored evidence names.
    pub execution_bundle_hash: String,
    /// Exact canonical map bytes the stored evidence carries.
    pub tested_resolution_map_bytes: Vec<u8>,
    /// SHA-256 over those exact bytes.
    pub tested_resolution_map_hash: String,
}

/// Which predicate refused one publish step A.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublishReleaseErrorKind {
    /// The control transaction itself failed; nothing was decided.
    Storage,
    /// The supplied validated draft is absent or is not the exact draft named.
    ValidatedDraft,
    /// The report is absent, not this draft's, or not green.
    TestReport,
    /// The target release coordinate does not exist, or is another environment's.
    ReleaseCoordinate,
    /// The immutable source artifact is absent or is not the one supplied.
    SourceArtifact,
    /// The plan bytes are absent, fail their hash, or are not this artifact's.
    PlanBytes,
    /// A `call-flow` callee is not a released member, or carries no contract.
    CallableContract,
    /// The report's tested resolution map is not a canonicalizable JSON object.
    TestedResolutionMap,
    /// The coordinate already publishes a DIFFERENT release member.
    ReleaseConflict,
    /// The coordinate already carries DIFFERENT tested evidence.
    EvidenceConflict,
}

impl PublishReleaseErrorKind {
    /// Stable label for logs, refusal literals, and tests.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Storage => "storage",
            Self::ValidatedDraft => "validated-draft",
            Self::TestReport => "test-report",
            Self::ReleaseCoordinate => "release-coordinate",
            Self::SourceArtifact => "source-artifact",
            Self::PlanBytes => "plan-bytes",
            Self::CallableContract => "callable-contract",
            Self::TestedResolutionMap => "tested-resolution-map",
            Self::ReleaseConflict => "release-conflict",
            Self::EvidenceConflict => "evidence-conflict",
        }
    }
}

/// Stable prefix every publish step A refusal renders with.
pub const PUBLISH_RELEASE_REFUSAL: &str = "publish-release-a-refused";

/// One refused publish step A, carrying the predicate and its exact context.
#[derive(Debug)]
pub struct PublishReleaseError {
    kind: PublishReleaseErrorKind,
    detail: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl PublishReleaseError {
    /// Refuse with a stable predicate and the context that decided it.
    pub fn new(kind: PublishReleaseErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
            source: None,
        }
    }

    /// Refuse with an upstream cause retained as this error's source.
    pub fn with_source(
        kind: PublishReleaseErrorKind,
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
    pub const fn kind(&self) -> PublishReleaseErrorKind {
        self.kind
    }

    /// The exact context the predicate refused on.
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for PublishReleaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{PUBLISH_RELEASE_REFUSAL} ({}): {}",
            self.kind.as_str(),
            self.detail
        )
    }
}

impl std::error::Error for PublishReleaseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.source {
            Some(source) => Some(&**source),
            None => None,
        }
    }
}

/// Bind the publishing tenant for this transaction only.
///
/// Every relation step A reads carries `FORCE ROW LEVEL SECURITY`, and FORCE
/// applies to the owner too, so a publisher that claims no tenant reads zero rows
/// and would mistake a populated release for an absent one
/// (`deploy/sql/control-portable-store.sql`). `true` keeps the claim
/// transaction-local, so it cannot outlive the statement set it scopes.
///
/// Params: `$1` tenant.
pub fn claim_publishing_tenant_sql() -> &'static str {
    "SELECT set_config('app.tenant', $1, true)"
}

/// Lock and read the exact immutable validated draft being published.
///
/// Params: `$1` tenant, `$2` validated-draft hash.
pub fn lock_validated_draft_sql() -> &'static str {
    "SELECT draft.catalog_id, draft.catalog_version, draft.environment, draft.flow_id, \
            draft.runtime_flow_version, draft.draft_artifact_hash, draft.execution_bundle_hash \
       FROM catalog.validated_flow_drafts AS draft \
      WHERE draft.tenant_id = $1 AND draft.validated_draft_hash = $2 \
        FOR SHARE"
}

/// Lock and read the finalized test report that gates this publish.
///
/// `resolution_map::text` is read as text because the authoritative evidence form
/// is RFC 8785 canonical bytes the driver derives from this value; the stored
/// `jsonb` is the report's own record, never the evidence identity.
///
/// Params: `$1` tenant, `$2` report id.
pub fn lock_test_report_sql() -> &'static str {
    "SELECT report.validated_draft_id, report.catalog_id, report.catalog_version, \
            report.passed, report.resolution_map::text \
       FROM wamn_run.authoring_test_reports AS report \
      WHERE report.tenant_id = $1 AND report.report_id = $2 \
        FOR SHARE"
}

/// Lock the target release coordinate and read the environment it belongs to.
///
/// The release member's foreign key is to `catalog.release_manifests`, so an
/// absent manifest is a refusal here rather than a foreign-key violation halfway
/// through the mint. Sealing that manifest is wamn-0h0g.15.14's.
///
/// Params: `$1` tenant, `$2` catalog id, `$3` catalog version.
pub fn lock_release_coordinate_sql() -> &'static str {
    "SELECT release.environment \
       FROM catalog.release_manifests AS manifest \
       JOIN catalog.catalogs AS release \
         ON release.tenant_id = manifest.tenant_id \
        AND release.catalog_id = manifest.catalog_id \
        AND release.version = manifest.catalog_version \
      WHERE manifest.tenant_id = $1 AND manifest.catalog_id = $2 \
        AND manifest.catalog_version = $3 \
        FOR SHARE OF manifest, release"
}

/// Lock and read the immutable source artifact the release member names.
///
/// Params: `$1` tenant, `$2` flow id, `$3` flow version.
pub fn lock_source_artifact_sql() -> &'static str {
    "SELECT artifact.artifact_hash \
       FROM catalog.flow_artifacts AS artifact \
      WHERE artifact.tenant_id = $1 AND artifact.flow_id = $2 \
        AND artifact.flow_version = $3 \
        FOR SHARE"
}

/// Read one execution plan's exact stored bytes for hash-before-parse.
///
/// Params: `$1` tenant, `$2` execution-bundle hash.
pub fn select_plan_bytes_sql() -> &'static str {
    "SELECT bundle.exact_bytes \
       FROM catalog.execution_bundles AS bundle \
      WHERE bundle.tenant_id = $1 AND bundle.execution_bundle_hash = $2 \
        FOR SHARE"
}

/// Resolve one `call-flow` callee inside the exact target release.
///
/// Callability is intrinsic to the callee's own plan, so the plan bytes come back
/// with the member and the driver reads them through the same hash-before-parse
/// boundary the root plan uses.
///
/// Params: `$1` tenant, `$2` catalog id, `$3` catalog version, `$4` callee flow.
pub fn select_released_callee_sql() -> &'static str {
    "SELECT member.execution_bundle_hash, bundle.exact_bytes \
       FROM catalog.release_flows AS member \
       JOIN catalog.execution_bundles AS bundle \
         ON bundle.tenant_id = member.tenant_id \
        AND bundle.execution_bundle_hash = member.execution_bundle_hash \
      WHERE member.tenant_id = $1 AND member.catalog_id = $2 \
        AND member.catalog_version = $3 AND member.flow_id = $4 \
        FOR SHARE OF member, bundle"
}

/// Lock and read whatever release member this coordinate already publishes.
///
/// Params: `$1` tenant, `$2` catalog id, `$3` catalog version, `$4` flow id.
pub fn lock_release_member_sql() -> &'static str {
    "SELECT member.flow_version, member.execution_bundle_hash \
       FROM catalog.release_flows AS member \
      WHERE member.tenant_id = $1 AND member.catalog_id = $2 \
        AND member.catalog_version = $3 AND member.flow_id = $4 \
        FOR SHARE"
}

/// Lock and read whatever tested evidence this coordinate already carries.
///
/// Params: `$1` tenant, `$2` catalog id, `$3` catalog version, `$4` flow id.
pub fn lock_release_evidence_sql() -> &'static str {
    "SELECT evidence.validated_draft_id, evidence.report_id, \
            evidence.source_artifact_hash, evidence.execution_bundle_hash, \
            evidence.tested_resolution_map_bytes, evidence.tested_resolution_map_hash \
       FROM catalog.release_flow_test_evidence AS evidence \
      WHERE evidence.tenant_id = $1 AND evidence.catalog_id = $2 \
        AND evidence.catalog_version = $3 AND evidence.flow_id = $4 \
        FOR SHARE"
}

/// Register the one tested-evidence row through the landed wamn-0h0g.9.9 routine.
///
/// The routine — not this crate — owns the durable append-or-exact-verify rule and
/// the byte-for-byte map comparison, and it has no `UPDATE` or `DELETE` path. The
/// driver's own pre-read exists so a conflicting reuse refuses *before* reaching
/// it; this call is the backstop a concurrent writer cannot slip past.
///
/// Params: `$1` tenant, `$2` catalog id, `$3` catalog version, `$4` flow id,
/// `$5` validated-draft hash, `$6` report id, `$7` source-artifact hash,
/// `$8` execution-bundle hash, `$9` exact canonical map bytes, `$10` map hash.
pub fn register_release_evidence_sql() -> &'static str {
    "SELECT catalog.register_release_flow_test_evidence(\
       $1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"
}

/// Verify the locked validated draft is exactly the release the caller asserted.
pub fn verify_validated_draft(
    request: &PublishTestedRelease<'_>,
    observed: &ValidatedDraftFacts,
) -> Result<(), PublishReleaseError> {
    for (field, supplied, stored) in [
        (
            "catalog-id",
            request.catalog_id,
            observed.catalog_id.as_str(),
        ),
        (
            "environment",
            request.environment,
            observed.environment.as_str(),
        ),
        ("flow-id", request.flow_id, observed.flow_id.as_str()),
        (
            "source-artifact-hash",
            request.source_artifact_hash,
            observed.draft_artifact_hash.as_str(),
        ),
        (
            "execution-bundle-hash",
            request.execution_bundle_hash,
            observed.execution_bundle_hash.as_str(),
        ),
    ] {
        if supplied != stored {
            return Err(PublishReleaseError::new(
                PublishReleaseErrorKind::ValidatedDraft,
                format!(
                    "validated draft {:?} pins {field} {stored:?}, not the supplied {supplied:?}",
                    request.validated_draft_id
                ),
            ));
        }
    }
    for (field, supplied, stored) in [
        (
            "catalog-version",
            request.catalog_version,
            observed.catalog_version,
        ),
        (
            "flow-version",
            request.flow_version,
            observed.runtime_flow_version,
        ),
    ] {
        if supplied != stored {
            return Err(PublishReleaseError::new(
                PublishReleaseErrorKind::ValidatedDraft,
                format!(
                    "validated draft {:?} pins {field} {stored}, not the supplied {supplied}",
                    request.validated_draft_id
                ),
            ));
        }
    }
    Ok(())
}

/// Verify the locked report is this draft's own green finalized report.
pub fn verify_test_report(
    request: &PublishTestedRelease<'_>,
    observed: &TestReportFacts,
) -> Result<(), PublishReleaseError> {
    if observed.validated_draft_id != request.validated_draft_id {
        return Err(PublishReleaseError::new(
            PublishReleaseErrorKind::TestReport,
            format!(
                "report {:?} tested validated draft {:?}, not the supplied {:?}",
                request.report_id, observed.validated_draft_id, request.validated_draft_id
            ),
        ));
    }
    if observed.catalog_id != request.catalog_id
        || observed.catalog_version != request.catalog_version
    {
        return Err(PublishReleaseError::new(
            PublishReleaseErrorKind::TestReport,
            format!(
                "report {:?} pinned catalog {:?} version {}, not {:?} version {}",
                request.report_id,
                observed.catalog_id,
                observed.catalog_version,
                request.catalog_id,
                request.catalog_version
            ),
        ));
    }
    if !observed.passed {
        return Err(PublishReleaseError::new(
            PublishReleaseErrorKind::TestReport,
            format!(
                "report {:?} finalized red; the publish gate is unconditional",
                request.report_id
            ),
        ));
    }
    Ok(())
}

/// Verify an already-published member is this exact release, not another one.
pub fn verify_release_member(
    request: &PublishTestedRelease<'_>,
    observed: &ReleaseMemberFacts,
) -> Result<(), PublishReleaseError> {
    if observed.flow_version != request.flow_version
        || observed.execution_bundle_hash != request.execution_bundle_hash
    {
        return Err(PublishReleaseError::new(
            PublishReleaseErrorKind::ReleaseConflict,
            format!(
                "catalog {:?} version {} already publishes flow {:?} at version {} with plan {}, \
                 not version {} with plan {}",
                request.catalog_id,
                request.catalog_version,
                request.flow_id,
                observed.flow_version,
                observed.execution_bundle_hash,
                request.flow_version,
                request.execution_bundle_hash
            ),
        ));
    }
    Ok(())
}

/// Verify already-stored evidence is byte-for-byte this publish's evidence.
///
/// The map is compared as exact bytes, never as a reserialized projection: two
/// different byte sequences can share one `jsonb` value, so only the bytes can
/// establish identity (wamn-0h0g.9.9).
pub fn verify_release_evidence(
    request: &PublishTestedRelease<'_>,
    tested_resolution_map_bytes: &[u8],
    tested_resolution_map_hash: &str,
    observed: &ReleaseEvidenceFacts,
) -> Result<(), PublishReleaseError> {
    for (field, supplied, stored) in [
        (
            "validated-draft-id",
            request.validated_draft_id,
            observed.validated_draft_id.as_str(),
        ),
        ("report-id", request.report_id, observed.report_id.as_str()),
        (
            "source-artifact-hash",
            request.source_artifact_hash,
            observed.source_artifact_hash.as_str(),
        ),
        (
            "execution-bundle-hash",
            request.execution_bundle_hash,
            observed.execution_bundle_hash.as_str(),
        ),
        (
            "tested-resolution-map-hash",
            tested_resolution_map_hash,
            observed.tested_resolution_map_hash.as_str(),
        ),
    ] {
        if supplied != stored {
            return Err(PublishReleaseError::new(
                PublishReleaseErrorKind::EvidenceConflict,
                format!(
                    "release evidence for flow {:?} in catalog {:?} version {} records {field} \
                     {stored:?}, not {supplied:?}",
                    request.flow_id, request.catalog_id, request.catalog_version
                ),
            ));
        }
    }
    if observed.tested_resolution_map_bytes.as_slice() != tested_resolution_map_bytes {
        return Err(PublishReleaseError::new(
            PublishReleaseErrorKind::EvidenceConflict,
            format!(
                "release evidence for flow {:?} in catalog {:?} version {} records {} tested \
                 resolution-map bytes that differ from this publish's {}",
                request.flow_id,
                request.catalog_id,
                request.catalog_version,
                observed.tested_resolution_map_bytes.len(),
                tested_resolution_map_bytes.len()
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DRAFT: &str = "validated-draft-a";
    const REPORT: &str = "report-a";
    const ARTIFACT: &str =
        "sha256:1111111111111111111111111111111111111111111111111111111111111111";
    const BUNDLE: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";
    const MAP_HASH: &str =
        "sha256:3333333333333333333333333333333333333333333333333333333333333333";

    fn request() -> PublishTestedRelease<'static> {
        PublishTestedRelease {
            tenant_id: "tenant-a",
            catalog_id: "catalog-a",
            catalog_version: 7,
            environment: "dev",
            flow_id: "orders",
            flow_version: 3,
            validated_draft_id: DRAFT,
            report_id: REPORT,
            source_artifact_hash: ARTIFACT,
            execution_bundle_hash: BUNDLE,
        }
    }

    fn draft_facts() -> ValidatedDraftFacts {
        ValidatedDraftFacts {
            catalog_id: "catalog-a".into(),
            catalog_version: 7,
            environment: "dev".into(),
            flow_id: "orders".into(),
            runtime_flow_version: 3,
            draft_artifact_hash: ARTIFACT.into(),
            execution_bundle_hash: BUNDLE.into(),
        }
    }

    fn report_facts() -> TestReportFacts {
        TestReportFacts {
            validated_draft_id: DRAFT.into(),
            catalog_id: "catalog-a".into(),
            catalog_version: 7,
            passed: true,
        }
    }

    fn evidence_facts() -> ReleaseEvidenceFacts {
        ReleaseEvidenceFacts {
            validated_draft_id: DRAFT.into(),
            report_id: REPORT.into(),
            source_artifact_hash: ARTIFACT.into(),
            execution_bundle_hash: BUNDLE.into(),
            tested_resolution_map_bytes: b"{}".to_vec(),
            tested_resolution_map_hash: MAP_HASH.into(),
        }
    }

    fn statements() -> [&'static str; 10] {
        [
            claim_publishing_tenant_sql(),
            lock_validated_draft_sql(),
            lock_test_report_sql(),
            lock_release_coordinate_sql(),
            lock_source_artifact_sql(),
            select_plan_bytes_sql(),
            select_released_callee_sql(),
            lock_release_member_sql(),
            lock_release_evidence_sql(),
            register_release_evidence_sql(),
        ]
    }

    fn drifted<T: Clone>(base: &T, mutate: fn(&mut T)) -> T {
        let mut copy = base.clone();
        mutate(&mut copy);
        copy
    }

    /// The whole statement set is a control-only read plus one append routine.
    #[test]
    fn every_statement_reads_or_appends_the_control_release_record_only() {
        for statement in statements() {
            for forbidden in [
                "UPDATE ",
                "DELETE ",
                "TRUNCATE",
                "deployment_attestations",
                "deployed_resolution_map",
                "connection_bindings",
                "connection_instances",
                "connection_generations",
                "connection_generation_retention",
                "attachment_activation",
                "attachment_tombstones",
                "authoring_test_case_runs",
                "FROM runs",
                "JOIN runs",
                ".runs",
                "lifecycle",
            ] {
                assert!(
                    !statement.contains(forbidden),
                    "step A statement reaches {forbidden}: {statement}"
                );
            }
        }
        // Exactly one statement writes, and it writes through the landed routine.
        let writers = statements()
            .into_iter()
            .filter(|statement| statement.contains("register_release_flow_test_evidence"))
            .count();
        assert_eq!(writers, 1);
        assert!(
            !statements()
                .iter()
                .any(|statement| statement.contains("INSERT INTO"))
        );
    }

    /// Each read names its exact relation, coordinates, and lock.
    #[test]
    fn each_read_locks_the_exact_immutable_fact_it_verifies() {
        let draft = lock_validated_draft_sql();
        assert!(draft.contains("FROM catalog.validated_flow_drafts AS draft"));
        assert!(draft.contains("draft.validated_draft_hash = $2"));
        assert!(draft.contains("FOR SHARE"));

        let report = lock_test_report_sql();
        assert!(report.contains("FROM wamn_run.authoring_test_reports AS report"));
        assert!(report.contains("report.report_id = $2"));
        assert!(report.contains("report.resolution_map::text"));
        assert!(report.contains("FOR SHARE"));

        let coordinate = lock_release_coordinate_sql();
        assert!(coordinate.contains("FROM catalog.release_manifests AS manifest"));
        assert!(coordinate.contains("JOIN catalog.catalogs AS release"));
        assert!(coordinate.contains("FOR SHARE OF manifest, release"));

        let artifact = lock_source_artifact_sql();
        assert!(artifact.contains("FROM catalog.flow_artifacts AS artifact"));
        assert!(artifact.contains("artifact.flow_version = $3"));

        let plan = select_plan_bytes_sql();
        assert!(plan.contains("FROM catalog.execution_bundles AS bundle"));
        assert!(plan.contains("bundle.exact_bytes"));

        let callee = select_released_callee_sql();
        assert!(callee.contains("FROM catalog.release_flows AS member"));
        assert!(callee.contains("JOIN catalog.execution_bundles AS bundle"));
        assert!(callee.contains("member.flow_id = $4"));

        let member = lock_release_member_sql();
        assert!(member.contains("FROM catalog.release_flows AS member"));
        assert!(member.contains("member.flow_id = $4"));
        assert!(member.contains("FOR SHARE"));

        let evidence = lock_release_evidence_sql();
        assert!(evidence.contains("FROM catalog.release_flow_test_evidence AS evidence"));
        assert!(evidence.contains("evidence.tested_resolution_map_bytes"));
        assert!(evidence.contains("evidence.flow_id = $4"));
        assert!(evidence.contains("FOR SHARE"));

        // The tenant claim is transaction-local, so it cannot outlive step A.
        assert_eq!(
            claim_publishing_tenant_sql(),
            "SELECT set_config('app.tenant', $1, true)"
        );
    }

    /// The evidence append binds the landed wamn-0h0g.9.9 routine, exactly.
    #[test]
    fn the_evidence_append_binds_the_landed_control_store_routine() {
        let ddl = crate::CONTROL_PORTABLE_STORE_SQL;
        let register = register_release_evidence_sql();
        assert!(
            ddl.contains("CREATE OR REPLACE FUNCTION catalog.register_release_flow_test_evidence(")
        );
        assert!(ddl.contains("text, text, int, text, text, text, text, text, bytea, text"));
        assert_eq!(register.matches('$').count(), 10);

        // The routine, not this crate, owns byte-for-byte retry and the checked
        // hash — and it has no UPDATE or DELETE path.
        assert!(ddl.contains("tested_resolution_map_bytes = p_tested_resolution_map_bytes"));
        assert!(ddl.contains("sha256(tested_resolution_map_bytes)"));
        assert!(ddl.contains("'release-flow-test-evidence-content-conflict'"));
        assert!(!ddl.contains("UPDATE catalog.release_flow_test_evidence"));
        assert!(!ddl.contains("DELETE FROM catalog.release_flow_test_evidence"));

        // Every relation the statement set names is owned by that same store.
        for relation in [
            "catalog.validated_flow_drafts",
            "catalog.release_manifests",
            "catalog.catalogs",
            "catalog.flow_artifacts",
            "catalog.execution_bundles",
            "catalog.release_flows",
            "catalog.release_flow_test_evidence",
            "wamn_run.authoring_test_reports",
        ] {
            assert!(
                ddl.contains(&format!("CREATE TABLE IF NOT EXISTS {relation}")),
                "the control store does not own {relation}"
            );
        }
    }

    #[test]
    fn the_validated_draft_pins_every_supplied_release_coordinate() {
        verify_validated_draft(&request(), &draft_facts()).expect("the exact draft is admitted");

        let mutations: [fn(&mut ValidatedDraftFacts); 7] = [
            |facts| facts.catalog_id = "catalog-b".into(),
            |facts| facts.environment = "prod".into(),
            |facts| facts.flow_id = "payments".into(),
            |facts| facts.draft_artifact_hash = BUNDLE.into(),
            |facts| facts.execution_bundle_hash = ARTIFACT.into(),
            |facts| facts.catalog_version = 8,
            |facts| facts.runtime_flow_version = 4,
        ];
        for mutate in mutations {
            let facts = drifted(&draft_facts(), mutate);
            assert_eq!(
                verify_validated_draft(&request(), &facts)
                    .expect_err("a drifted draft pin refuses")
                    .kind(),
                PublishReleaseErrorKind::ValidatedDraft
            );
        }
    }

    #[test]
    fn only_this_drafts_green_finalized_report_admits_a_publish() {
        verify_test_report(&request(), &report_facts()).expect("the green report is admitted");

        let mutations: [fn(&mut TestReportFacts); 4] = [
            |facts| facts.validated_draft_id = "validated-draft-b".into(),
            |facts| facts.catalog_id = "catalog-b".into(),
            |facts| facts.catalog_version = 8,
            |facts| facts.passed = false,
        ];
        for mutate in mutations {
            let facts = drifted(&report_facts(), mutate);
            assert_eq!(
                verify_test_report(&request(), &facts)
                    .expect_err("an ungating report refuses")
                    .kind(),
                PublishReleaseErrorKind::TestReport
            );
        }
    }

    #[test]
    fn an_exact_retry_is_admitted_and_a_conflicting_reuse_refuses() {
        let request = request();
        verify_release_member(
            &request,
            &ReleaseMemberFacts {
                flow_version: 3,
                execution_bundle_hash: BUNDLE.into(),
            },
        )
        .expect("the identical member is an exact retry");
        verify_release_evidence(&request, b"{}", MAP_HASH, &evidence_facts())
            .expect("the identical evidence is an exact retry");

        for member in [
            ReleaseMemberFacts {
                flow_version: 4,
                execution_bundle_hash: BUNDLE.into(),
            },
            ReleaseMemberFacts {
                flow_version: 3,
                execution_bundle_hash: ARTIFACT.into(),
            },
        ] {
            assert_eq!(
                verify_release_member(&request, &member)
                    .expect_err("a different member refuses")
                    .kind(),
                PublishReleaseErrorKind::ReleaseConflict
            );
        }

        // One byte of the canonical map is enough, and a stored hash that disagrees
        // with the stored bytes can never read as an exact retry either.
        let mutations: [fn(&mut ReleaseEvidenceFacts); 6] = [
            |facts| facts.tested_resolution_map_bytes = br#"{"a":"b"}"#.to_vec(),
            |facts| facts.tested_resolution_map_hash = ARTIFACT.into(),
            |facts| facts.validated_draft_id = "other".into(),
            |facts| facts.report_id = "other".into(),
            |facts| facts.source_artifact_hash = BUNDLE.into(),
            |facts| facts.execution_bundle_hash = ARTIFACT.into(),
        ];
        for mutate in mutations {
            let facts = drifted(&evidence_facts(), mutate);
            assert_eq!(
                verify_release_evidence(&request, b"{}", MAP_HASH, &facts)
                    .expect_err("conflicting evidence refuses")
                    .kind(),
                PublishReleaseErrorKind::EvidenceConflict
            );
        }
    }

    #[test]
    fn refusals_render_one_stable_prefix_and_one_stable_predicate() {
        for (kind, label) in [
            (PublishReleaseErrorKind::Storage, "storage"),
            (PublishReleaseErrorKind::ValidatedDraft, "validated-draft"),
            (PublishReleaseErrorKind::TestReport, "test-report"),
            (
                PublishReleaseErrorKind::ReleaseCoordinate,
                "release-coordinate",
            ),
            (PublishReleaseErrorKind::SourceArtifact, "source-artifact"),
            (PublishReleaseErrorKind::PlanBytes, "plan-bytes"),
            (
                PublishReleaseErrorKind::CallableContract,
                "callable-contract",
            ),
            (
                PublishReleaseErrorKind::TestedResolutionMap,
                "tested-resolution-map",
            ),
            (PublishReleaseErrorKind::ReleaseConflict, "release-conflict"),
            (
                PublishReleaseErrorKind::EvidenceConflict,
                "evidence-conflict",
            ),
        ] {
            assert_eq!(kind.as_str(), label);
            let error = PublishReleaseError::new(kind, "why");
            assert_eq!(
                error.to_string(),
                format!("publish-release-a-refused ({label}): why")
            );
            assert_eq!(error.kind(), kind);
            assert_eq!(error.detail(), "why");
        }
        assert_eq!(PUBLISH_RELEASE_REFUSAL, "publish-release-a-refused");
    }
}
