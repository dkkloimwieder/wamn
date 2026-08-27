//! Mint the immutable serving-manifest v2 release closure.
//!
//! A release is built only from current, admitted catalog facts. The caller
//! names exact wiring versions; this boundary reads those immutable wiring
//! documents, loads their scoped `catalog.component_library` facts, runs the
//! production semantic wiring gate, and records one
//! `catalog.release_components` row per `(wiring, component digest)`.
//!
//! The membership rows preserve relational promotion coverage; the exact
//! canonical v2 document is the complete release freeze. An exact retry
//! converges, while changing any component, wiring, attachment, or registration
//! fact at the same release coordinate refuses. The serving document is admitted
//! through [`ServingManifest::from_canonical_bytes`] before its digest leaves
//! this module.
//!
//! No flow artifact, execution bundle, plan, callable edge, or legacy serving
//! manifest is read here. A pre-component release therefore cannot be converted
//! into v2: it must be published from current wiring and component records or it
//! has no v2 manifest.
//!
//! The `publish-release` verb is an INTERIM operator surface. Attachment and
//! registration facts arrive as hand-authored JSON documents because no table
//! projects them: the flow-era attachment records were retired whole by
//! wamn-0h0g.26.21, and `catalog.event_registrations`
//! is absent from the control portable store. A ratified ruling moves
//! registrations into that store — owed by whoever lands that move — and the
//! projection replaces these two arguments then. Both stay REQUIRED until it
//! does, because an empty registration set is a valid manifest with a real
//! digest and must be chosen, never defaulted.
//!
//! # Stated precondition: reconcile the run plane BEFORE publishing
//!
//! `publish-release` requires that `reconcile-run-plane` has already converged
//! this tenant's row in `<--run-schema>.environment_policies` in the project
//! database being published into. That row is the provisioned environment fact
//! `verify_provisioned_environment` checks the release's carried environment
//! against, and the check refuses when the row is ABSENT
//! (`environment-policy-not-converged`) exactly as it refuses when the row
//! DISAGREES (`environment-policy-environment-mismatch`). Publishing into a
//! project database whose run plane was never reconciled therefore fails, where
//! before the check existed it succeeded unchecked.
//!
//! The ordering is stated here rather than enforced: nothing in the schema
//! sequences the two verbs, and this module does not run the reconcile itself.
//! What makes the failure diagnosable rather than surprising is that the absent
//! refusal names `reconcile-run-plane` in its own message text — recorded as the
//! publish surface's precondition by wamn-0h0g.8.26, after wamn-xkgp mounted the
//! check. `push-release-manifest` carries no such check and no such precondition:
//! it publishes bytes a mint already verified.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use clap::Args;
use serde::de::DeserializeOwned;
use tokio_postgres::{Client, NoTls, Transaction};
use wamn_catalog::{
    AdmittedComponent, AdmittedComponentEffect, AdmittedComponentParameter, AdmittedComponentPort,
    ArtifactHash, ComponentCatalogScope, ManifestDigest, SERVING_MANIFEST_FORMAT_VERSION,
    ServingAttachment, ServingComponent, ServingManifest, ServingRegistration, ServingRelease,
    ServingWiring, WiringDocument, validate_wiring_compatibility,
};
use wamn_control_registry::Triple;
use wamn_schema_control::BareSchemaName;

const CLAIM_TENANT_SQL: &str = "SELECT set_config('app.tenant', $1, true)";

const LOCK_RELEASE_SQL: &str = "\
SELECT catalog.environment, catalog.schema_version \
  FROM catalog.releases AS release \
  JOIN catalog.catalogs AS catalog \
    ON catalog.tenant_id = release.tenant_id \
   AND catalog.catalog_id = release.catalog_id \
   AND catalog.version = release.catalog_version \
 WHERE release.tenant_id = $1 AND release.catalog_id = $2 \
   AND release.catalog_version = $3 \
 FOR UPDATE OF release";

const SELECT_COMPONENT_FACTS_SQL: &str = "\
SELECT component, interface_version, operation, component_digest, \
       imports::text, imports_fingerprint, input_ports::text, \
       output_ports::text, parameters::text, effects::text \
  FROM catalog.component_library \
 WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = $3 \
 ORDER BY component COLLATE \"C\", interface_version COLLATE \"C\" \
 FOR SHARE";

const SELECT_WIRING_SQL: &str = "\
SELECT gated_catalog_version, wiring_hash, graph_json::text \
  FROM catalog.wirings \
 WHERE tenant_id = $1 AND catalog_id = $2 \
   AND wiring_id = $3 AND version = $4 \
 FOR SHARE";

const SELECT_RELEASE_COMPONENTS_SQL: &str = "\
SELECT wiring_id, wiring_version, component_digest \
  FROM catalog.release_components \
 WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = $3 \
 ORDER BY wiring_id COLLATE \"C\", wiring_version, component_digest COLLATE \"C\" \
 FOR SHARE";

const INSERT_RELEASE_COMPONENT_SQL: &str = "\
INSERT INTO catalog.release_components \
       (tenant_id, catalog_id, catalog_version, wiring_id, wiring_version, component_digest) \
VALUES ($1, $2, $3, $4, $5, $6)";

const SELECT_RELEASE_SNAPSHOT_SQL: &str = "\
SELECT manifest_digest, canonical_bytes \
  FROM catalog.release_manifest_v2_snapshots \
 WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = $3 \
 FOR SHARE";

const INSERT_RELEASE_SNAPSHOT_SQL: &str = "\
INSERT INTO catalog.release_manifest_v2_snapshots \
       (tenant_id, catalog_id, catalog_version, manifest_digest, canonical_bytes) \
VALUES ($1, $2, $3, $4, $5)";

// A publisher reads a frozen row outside any freeze, so it must not carry the
// mint's FOR SHARE: row locking needs UPDATE, DELETE, or TRUNCATE on the table,
// and the schema grants the serving role SELECT alone.
const READ_RELEASE_SNAPSHOT_SQL: &str = "\
SELECT canonical_bytes \
  FROM catalog.release_manifest_v2_snapshots \
 WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = $3";

/// Read the provisioned environment fact out of the operator-named run-plane schema.
///
/// The relation is `environment_policies` (`deploy/sql/run-state.sql`), whose one
/// row per tenant `reconcile-run-plane` projects from the control registry. The
/// schema is composed rather than fixed because a project database's run plane is
/// installed under an operator-chosen bare schema name, not always `wamn_run`.
fn expected_environment_sql(run_schema: &BareSchemaName) -> String {
    format!(
        "SELECT expected_environment FROM {}.environment_policies WHERE tenant_id = $1",
        run_schema.quoted()
    )
}

/// One exact wiring version included in a release closure.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReleaseWiringTarget {
    pub wiring_id: String,
    pub wiring_version: u32,
}

impl std::str::FromStr for ReleaseWiringTarget {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (wiring_id, version) = value
            .split_once('=')
            .ok_or_else(|| "expected WIRING_ID=VERSION".to_owned())?;
        if wiring_id.is_empty() || wiring_id.chars().any(char::is_whitespace) {
            return Err("wiring id must be non-empty and free of whitespace".to_owned());
        }
        let wiring_version = version
            .parse::<u32>()
            .map_err(|_| format!("wiring version {version:?} is not a whole number"))?;
        if wiring_version == 0 {
            return Err("wiring version must be greater than zero".to_owned());
        }
        Ok(Self {
            wiring_id: wiring_id.to_owned(),
            wiring_version,
        })
    }
}

/// Inputs owned by the release publisher.
#[derive(Clone, Copy, Debug)]
pub struct MintReleaseManifest<'a> {
    pub tenant_id: &'a str,
    pub catalog_id: &'a str,
    pub catalog_version: i32,
    /// Exact current wiring records selected for this release.
    pub wirings: &'a BTreeSet<ReleaseWiringTarget>,
    /// Exact route facts supplied by the attachment owner.
    pub attachments: &'a BTreeMap<String, ServingAttachment>,
    /// Exact stream facts supplied by the registration owner.
    pub registrations: &'a BTreeMap<String, ServingRegistration>,
}

/// The six-part key `catalog.register_deployment_attestation` writes under.
///
/// Only the control-plane `(org, project)` comes from the operator. Tenant,
/// catalog, version and environment are read back off the manifest the release
/// actually froze, so an attestation cannot name an environment or a catalog
/// version the deployed bytes were never projected for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeploymentCoordinate {
    /// Control-plane placement `(org, project, env)` the release is deployed into.
    pub triple: Triple,
    pub tenant_id: String,
    pub catalog_id: String,
    pub catalog_version: u32,
}

impl DeploymentCoordinate {
    /// Key one deployment by its operator-named placement and its own manifest.
    pub fn new(org: &str, project: &str, release: &ServingRelease) -> Self {
        Self {
            triple: Triple::new(org, project, release.environment.as_str()),
            tenant_id: release.tenant_id.clone(),
            catalog_id: release.catalog_id.clone(),
            catalog_version: release.catalog_version,
        }
    }
}

/// One minted serving manifest and the bytes its digest names.
#[derive(Clone, Debug, PartialEq)]
pub struct MintedReleaseManifest {
    pub manifest: ServingManifest,
    pub digest: ManifestDigest,
    pub canonical_bytes: Vec<u8>,
    /// `catalog.catalogs.schema_version` of the row this release was frozen from
    /// — the catalog-MODEL format version, not the catalog version. Read under
    /// the same `FOR UPDATE` lock as the environment so the identity projected
    /// onto the control plane cannot be assembled from two different reads.
    pub catalog_schema_version: String,
}

/// Stable prefix every release-manifest mint refusal renders with.
pub const RELEASE_MANIFEST_MINT_REFUSAL: &str = "release-manifest-mint-refused";

/// Stable predicate that refused a v2 release mint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MintManifestErrorKind {
    Storage,
    Release,
    Wiring,
    Component,
    ClosureConflict,
    Document,
    /// The project database carries no provisioned environment fact for this
    /// tenant, so the environment the release names cannot be checked at all.
    /// Distinct from a mismatch: the remedy is to converge the policy, not to
    /// republish somewhere else.
    EnvironmentPolicyAbsent,
    /// The release names an environment this project database was not
    /// provisioned for.
    EnvironmentPolicyMismatch,
}

impl MintManifestErrorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Storage => "storage",
            Self::Release => "release",
            Self::Wiring => "wiring",
            Self::Component => "component",
            Self::ClosureConflict => "closure-conflict",
            Self::Document => "document",
            // wamn-xkgp, owner ruling 2026-08-26: ONE CONDITION, ONE LITERAL.
            // These two strings are ADMISSION's, not ours. `pin_run_durability_class`
            // (deploy/sql/run-state.sql, pinned in crates/schema/control/src/run_plane.rs)
            // already refuses on exactly these conditions against exactly this relation,
            // and it named them first. Publish reuses them so one condition never reports
            // under two names depending on which door an operator hit.
            //
            // DO NOT "TIDY" THE STUTTER IN THE SECOND ONE. It reads oddly on its own and
            // it is the price of a shared vocabulary; renaming it here forks the dialect
            // this ruling exists to prevent. If it ever changes, it changes in BOTH
            // places, in one commit.
            Self::EnvironmentPolicyAbsent => "environment-policy-not-converged",
            Self::EnvironmentPolicyMismatch => "environment-policy-environment-mismatch",
        }
    }
}

/// Contextual refusal from the release-manifest mint boundary.
#[derive(Debug)]
pub struct MintManifestError {
    kind: MintManifestErrorKind,
    detail: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl MintManifestError {
    pub fn new(kind: MintManifestErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
            source: None,
        }
    }

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

    pub const fn kind(&self) -> MintManifestErrorKind {
        self.kind
    }

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
        self.source.as_deref().map(|source| source as _)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ReleaseComponentMembership {
    wiring_id: String,
    wiring_version: u32,
    component_digest: String,
}

/// INTERIM arguments for the first-release mint (see the module documentation).
#[derive(Debug, Args)]
pub struct PublishReleaseArgs {
    /// Owner URL to the project-environment database carrying the catalog facts.
    #[arg(long)]
    pub database_url: String,

    /// Registry organization the release is deployed into.
    #[arg(long)]
    pub org: String,

    /// Registry project the release is deployed into.
    #[arg(long)]
    pub project: String,

    /// Tenant claim carried by the release. Never inferred from the placement:
    /// one `(org, project, environment)` maps to exactly one tenant, and that
    /// mapping is stored, not derived.
    #[arg(long)]
    pub tenant: String,

    /// Catalog identity of the release.
    #[arg(long)]
    pub catalog_id: String,

    /// Exact catalog version whose release identity is frozen.
    #[arg(long)]
    pub catalog_version: u32,

    /// The run-plane schema in this project database holding the provisioned
    /// environment fact — the same `--schema` `reconcile-run-plane` converged
    /// (`wamn_run` in the schema of record). Bare identifier, and REQUIRED: a
    /// default would let an invocation that omits the flag check the release
    /// against a relation the operator never named, which is the trusted-carry
    /// this verb exists to stop.
    ///
    /// PRECONDITION: run `reconcile-run-plane` for this tenant and this schema
    /// FIRST. Publishing before it has converged the tenant's
    /// `environment_policies` row refuses on `environment-policy-not-converged`;
    /// a row naming another environment refuses on
    /// `environment-policy-environment-mismatch`.
    #[arg(long)]
    pub run_schema: String,

    /// Exact wiring version in the closure. Repeat once per released wiring.
    #[arg(long = "wiring", value_name = "WIRING_ID=VERSION", required = true)]
    pub wirings: Vec<ReleaseWiringTarget>,

    /// INTERIM: JSON object of attachment id to serving attachment; `{}` for none.
    #[arg(long)]
    pub attachments: PathBuf,

    /// INTERIM: JSON object of registration id to serving registration; `{}` for none.
    #[arg(long)]
    pub registrations: PathBuf,

    /// Owner URL to the CONTROL database this release's identity is projected
    /// into (wamn-0h0g.8.27).
    ///
    /// REQUIRED, and never defaulted to `--database-url`: the two are separate
    /// databases, and a mint whose identity never reaches the control plane
    /// leaves `catalog.deployment_attestations` unable to reference the release
    /// it froze, so nothing could ever mark that digest deployed.
    #[arg(long)]
    pub control_database_url: String,
}

impl PublishReleaseArgs {
    /// Key the minted release for attestation under this invocation's placement.
    fn deployment_coordinate(&self, release: &ServingRelease) -> DeploymentCoordinate {
        DeploymentCoordinate::new(&self.org, &self.project, release)
    }

    /// The validated run-plane schema this invocation verifies its environment in.
    fn verified_run_schema(&self) -> anyhow::Result<BareSchemaName> {
        BareSchemaName::new(self.run_schema.clone())
            .map_err(|error| anyhow::anyhow!("invalid --run-schema {:?}: {error}", self.run_schema))
    }
}

/// Mint one v2 release from explicit wiring, attachment, and registration facts.
pub async fn run(args: PublishReleaseArgs) -> anyhow::Result<()> {
    let attachments: BTreeMap<String, ServingAttachment> =
        read_document(&args.attachments, "attachments")?;
    let registrations: BTreeMap<String, ServingRegistration> =
        read_document(&args.registrations, "registrations")?;
    let catalog_version = i32::try_from(args.catalog_version)
        .context("catalog-version exceeds the PostgreSQL integer carrier")?;
    let run_schema = args.verified_run_schema()?;
    let wirings = args.wirings.iter().cloned().collect::<BTreeSet<_>>();
    let request = MintReleaseManifest {
        tenant_id: &args.tenant,
        catalog_id: &args.catalog_id,
        catalog_version,
        wirings: &wirings,
        attachments: &attachments,
        registrations: &registrations,
    };

    let (mut client, connection) = tokio_postgres::connect(&args.database_url, NoTls)
        .await
        .context("connect to the release project environment")?;
    let connection_task = tokio::spawn(connection);
    let minted = mint_in_transaction(&mut client, &request, &run_schema).await;
    match minted {
        Ok(minted) => {
            drop(client);
            connection_task
                .await
                .context("join the release mint connection")?
                .context("drive the release mint connection")?;
            let coordinate = args.deployment_coordinate(&minted.manifest.release);
            report_deployment_coordinate(&coordinate, &minted.digest);
            // The mint's own cross-plane write: identity, NOT attestation. A
            // digest is RELEASED iff an attestation references it
            // (wamn-0h0g.13.54), so a minted-and-unpushed release must be
            // reachable by that foreign key while still carrying no attestation
            // — which is exactly what makes it a candidate.
            project_release_identity(
                &args.control_database_url,
                &coordinate,
                &minted.catalog_schema_version,
            )
            .await?;
            println!("{}", minted.digest);
            Ok(())
        }
        Err(error) => {
            connection_task.abort();
            Err(error)
        }
    }
}

/// Mint one release and prove its carried environment before committing it.
///
/// The verify is the `publish-release` VERB's own precondition, not the shared
/// mint's: `promote` mints into an environment it was told to target and resolves
/// its placement differently, so the check is owned where the operator publishes
/// rather than pushed into [`mint_release_manifest`]. Running inside the mint's
/// own transaction makes the refusal fail-closed — nothing is committed, and the
/// tenant claim [`mint_release_manifest`] set is still in force, so the policy
/// read is row-security scoped to exactly the release's tenant.
async fn mint_in_transaction(
    client: &mut Client,
    request: &MintReleaseManifest<'_>,
    run_schema: &BareSchemaName,
) -> anyhow::Result<MintedReleaseManifest> {
    let transaction = client
        .transaction()
        .await
        .context("begin the release mint")?;
    let minted = mint_release_manifest(&transaction, request).await?;
    let expected_environment =
        read_expected_environment(&transaction, run_schema, request.tenant_id).await?;
    verify_provisioned_environment(
        expected_environment.as_deref(),
        &minted.manifest.release,
        run_schema,
    )?;
    transaction
        .commit()
        .await
        .context("commit the release mint")?;
    Ok(minted)
}

/// Read the one provisioned environment fact this project database holds for a tenant.
pub(crate) async fn read_expected_environment(
    transaction: &Transaction<'_>,
    run_schema: &BareSchemaName,
    tenant_id: &str,
) -> Result<Option<String>, MintManifestError> {
    let policy = transaction
        .query_opt(&expected_environment_sql(run_schema), &[&tenant_id])
        .await
        .map_err(|error| storage("read the provisioned environment policy", error))?;
    Ok(policy.map(|row| row.get(0)))
}

/// Refuse unless the environment a release carries is the one this database is.
///
/// The carried value is `catalog.catalogs.environment`: a free per-catalog-row
/// label that no constraint ties to any provisioned identity (`D18`,
/// `deploy/sql/catalog-schema.sql`). The provisioned fact is
/// `<run-schema>.environment_policies.expected_environment`, which
/// `reconcile-run-plane` projects out of the control registry for exactly the
/// `(org, project, env)` this database was created for. Carrying the label into
/// the attestation key without checking it against the fact would key the write
/// on whatever the catalog row happens to say.
///
/// The two refusals are deliberately distinct literals: an absent policy and a
/// disagreeing one are different operator situations with different remedies, so
/// the absent refusal names `reconcile-run-plane` — the verb that converges it.
pub(crate) fn verify_provisioned_environment(
    expected_environment: Option<&str>,
    release: &ServingRelease,
    run_schema: &BareSchemaName,
) -> Result<(), MintManifestError> {
    let Some(expected_environment) = expected_environment else {
        return Err(MintManifestError::new(
            MintManifestErrorKind::EnvironmentPolicyAbsent,
            format!(
                "tenant {:?} has no row in {}.environment_policies, so the environment {:?} \
                 this release carries cannot be checked against the environment this project \
                 database was provisioned for: run `reconcile-run-plane` for this tenant to \
                 project its environment policy, then publish again",
                release.tenant_id,
                run_schema.as_str(),
                release.environment,
            ),
        ));
    };
    if expected_environment != release.environment {
        return Err(MintManifestError::new(
            MintManifestErrorKind::EnvironmentPolicyMismatch,
            format!(
                "release names environment {:?}, but {}.environment_policies provisioned tenant \
                 {:?} for environment {:?}: publishing would key the deployment attestation to \
                 an environment this database is not",
                release.environment,
                run_schema.as_str(),
                release.tenant_id,
                expected_environment,
            ),
        ));
    }
    Ok(())
}

/// Report the complete attestation key one published manifest is deployable under.
///
/// Shared by both publish verbs so the two surfaces cannot drift on which parts
/// of the key they resolve or on where each part comes from.
pub(crate) fn report_deployment_coordinate(
    coordinate: &DeploymentCoordinate,
    manifest_hash: &ManifestDigest,
) {
    tracing::info!(
        org = %coordinate.triple.org,
        project = %coordinate.triple.project,
        environment = %coordinate.triple.env,
        tenant = %coordinate.tenant_id,
        catalog = %coordinate.catalog_id,
        catalog_version = coordinate.catalog_version,
        manifest_hash = %manifest_hash,
        "release carries a complete deployment attestation coordinate"
    );
}

// ---------------------------------------------------------------------------
// The cross-plane writes (wamn-0h0g.8.27, owner ruling 2026-08-27).
//
// `catalog.deployment_attestations` and the `catalog.releases` its foreign key
// resolves against are CONTROL-plane relations; both publish verbs mint into,
// and read from, a PROJECT environment database. `catalog.releases` exists as a
// SEPARATE relation in each plane, so the key resolves only once release
// identity has been PROJECTED across — which is what the two functions below do,
// in that order and from two different verbs.
//
// No statement spans the two databases. Each write is its own transaction on its
// own connection, opened for the write and closed after it.
// ---------------------------------------------------------------------------

/// Open the control-plane connection, run one write on it, and close it.
///
/// Neither publish verb held a control connection before this bead: both connect
/// to the project environment database alone.
async fn on_control_plane<F, T>(control_database_url: &str, write: F) -> anyhow::Result<T>
where
    F: AsyncFnOnce(&mut Client) -> anyhow::Result<T>,
{
    let (mut control, connection) = tokio_postgres::connect(control_database_url, NoTls)
        .await
        .context("connect to the control database")?;
    let connection_task = tokio::spawn(connection);
    let outcome = write(&mut control).await;
    drop(control);
    match outcome {
        Ok(outcome) => {
            connection_task
                .await
                .context("join the control-plane connection")?
                .context("drive the control-plane connection")?;
            Ok(outcome)
        }
        Err(error) => {
            connection_task.abort();
            Err(error)
        }
    }
}

/// Render a driver failure so the server's own message survives into the translation.
///
/// `tokio_postgres::Error` DISPLAYS as the bare string `db error`: the `DbError`
/// carrying a routine's `RAISE` message is its SOURCE, not part of its own
/// rendering. Both translations in `wamn_schema_control::attestation` classify on
/// the SQLSTATE *and* that message, so a caller handing them `error.to_string()`
/// alone would report every routine refusal as `storage`.
fn render_driver_failure(error: &tokio_postgres::Error) -> String {
    match error.as_db_error() {
        Some(db_error) => format!("{error}: {db_error}"),
        None => error.to_string(),
    }
}

/// Run one bound control-plane statement under the coordinate's own tenant claim.
///
/// The claim is not decoration: every relation this touches is under FORCE ROW
/// LEVEL SECURITY with an `app.tenant` policy, so an unclaimed session writing as
/// the store's owner would be filtered to nothing rather than refused.
async fn execute_claimed(
    control: &mut Client,
    tenant_id: &str,
    statement: &wamn_schema_control::SqlStatement,
) -> Result<(), tokio_postgres::Error> {
    let transaction = control.transaction().await?;
    transaction
        .query_one(CLAIM_TENANT_SQL, &[&tenant_id])
        .await?;
    let params = crate::migrate_catalog::to_sql_params(&statement.params);
    transaction.execute(statement.sql.as_str(), &params).await?;
    transaction.commit().await
}

/// Project one release's identity onto the control plane, as a provenance fact.
///
/// This is what makes the attestation's foreign key resolvable at all. It records
/// nothing about deployment: an identity with no attestation beside it is exactly
/// a CANDIDATE (`wamn-0h0g.13.54`).
pub async fn project_release_identity(
    control_database_url: &str,
    coordinate: &DeploymentCoordinate,
    catalog_schema_version: &str,
) -> anyhow::Result<()> {
    let catalog_version = i32::try_from(coordinate.catalog_version)
        .context("catalog-version exceeds the PostgreSQL integer carrier")?;
    let identity = wamn_schema_control::attestation::ReleaseIdentity {
        tenant_id: &coordinate.tenant_id,
        catalog_id: &coordinate.catalog_id,
        catalog_version,
        environment: coordinate.triple.env.as_str(),
        schema_version: catalog_schema_version,
    };
    let statement = wamn_schema_control::attestation::project_release_identity(&identity);
    on_control_plane(control_database_url, async |control| {
        execute_claimed(control, identity.tenant_id, &statement)
            .await
            .map_err(|error| {
                anyhow::Error::new(
                    wamn_schema_control::attestation::translate_projection_failure(
                        &identity,
                        error.code().map(tokio_postgres::error::SqlState::code),
                        &render_driver_failure(&error),
                    ),
                )
            })
    })
    .await
}

/// Record that one release really reached its control-plane placement.
///
/// The attestation is what MAKES a manifest a release (`wamn-0h0g.13.54`), so it
/// is written where the deployment happens — after the OCI push — and nowhere
/// else. Its foreign key refuses a coordinate whose identity was never projected,
/// which is the referential guarantee that definition rests on.
pub async fn attest_deployment(
    control_database_url: &str,
    coordinate: &DeploymentCoordinate,
    manifest_hash: &ManifestDigest,
) -> anyhow::Result<()> {
    let catalog_version = i32::try_from(coordinate.catalog_version)
        .context("catalog-version exceeds the PostgreSQL integer carrier")?;
    on_control_plane(control_database_url, async |control| {
        // The instant is the CONTROL database's own clock, read on the connection
        // that records it. `register_deployment_attestation` treats `attested_at`
        // as attested content, so a retry must present the value already
        // recorded rather than a second reading of some other clock; the write
        // below reuses the stored instant when the coordinate is already
        // attested, and lets the routine refuse when the content differs.
        let recorded: Option<String> = control
            .query_opt(
                "SELECT attested_at::text FROM catalog.deployment_attestations \
                  WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = $3 \
                    AND org_id = $4 AND project_id = $5 AND environment = $6",
                &[
                    &coordinate.tenant_id,
                    &coordinate.catalog_id,
                    &catalog_version,
                    &coordinate.triple.org,
                    &coordinate.triple.project,
                    &coordinate.triple.env.as_str(),
                ],
            )
            .await
            .context("read the recorded attestation instant")?
            .map(|row| row.get(0));
        let attested_at = match recorded {
            Some(recorded) => recorded,
            None => control
                .query_one("SELECT now()::text", &[])
                .await
                .context("read the control database's attestation instant")?
                .get(0),
        };
        let attestation = wamn_schema_control::attestation::Attestation {
            tenant_id: &coordinate.tenant_id,
            catalog_id: &coordinate.catalog_id,
            catalog_version,
            org_id: &coordinate.triple.org,
            project_id: &coordinate.triple.project,
            environment: coordinate.triple.env.as_str(),
            deployed_manifest_hash: manifest_hash.as_str(),
            attested_at: &attested_at,
        };
        let statement = wamn_schema_control::attestation::register_attestation(&attestation);
        execute_claimed(control, attestation.tenant_id, &statement)
            .await
            .map_err(|error| {
                anyhow::Error::new(wamn_schema_control::attestation::translate_failure(
                    &attestation,
                    error.code().map(tokio_postgres::error::SqlState::code),
                    &render_driver_failure(&error),
                ))
            })
    })
    .await
}

fn read_document<T: DeserializeOwned>(path: &Path, field: &'static str) -> anyhow::Result<T> {
    let bytes =
        std::fs::read(path).with_context(|| format!("read release {field} {}", path.display()))?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("parse release {field} {}", path.display()))
}

/// Mint and freeze one v2 release closure in the caller's transaction.
pub async fn mint_release_manifest(
    transaction: &Transaction<'_>,
    request: &MintReleaseManifest<'_>,
) -> Result<MintedReleaseManifest, MintManifestError> {
    transaction
        .query_one(CLAIM_TENANT_SQL, &[&request.tenant_id])
        .await
        .map_err(|error| storage("claim the release tenant", error))?;

    let catalog_version = serving_version(request.catalog_version, "catalog-version")?;
    let Some(release) = transaction
        .query_opt(
            LOCK_RELEASE_SQL,
            &[
                &request.tenant_id,
                &request.catalog_id,
                &request.catalog_version,
            ],
        )
        .await
        .map_err(|error| storage("lock the release coordinate", error))?
    else {
        return Err(MintManifestError::new(
            MintManifestErrorKind::Release,
            format!(
                "catalog {:?} version {} has no release identity",
                request.catalog_id, request.catalog_version
            ),
        ));
    };
    if request.wirings.is_empty() {
        return Err(MintManifestError::new(
            MintManifestErrorKind::Wiring,
            "a release with no wiring has no executable closure",
        ));
    }

    let scope = ComponentCatalogScope {
        tenant_id: request.tenant_id.to_string(),
        catalog_id: request.catalog_id.to_string(),
        catalog_version,
    };
    let component_facts = load_component_facts(transaction, &scope).await?;
    let (components, wirings, expected_membership) =
        resolve_current_closure(transaction, request, &scope, &component_facts).await?;

    let projected = ServingManifest {
        format_version: SERVING_MANIFEST_FORMAT_VERSION,
        release: ServingRelease {
            tenant_id: request.tenant_id.to_string(),
            catalog_id: request.catalog_id.to_string(),
            catalog_version,
            environment: release.get(0),
        },
        components,
        wirings,
        attachments: request.attachments.clone(),
        registrations: request.registrations.clone(),
    };
    let canonical_bytes = projected.canonical_bytes();
    let (manifest, digest) =
        ServingManifest::from_canonical_bytes(&canonical_bytes).map_err(|error| {
            MintManifestError::with_source(
                MintManifestErrorKind::Document,
                format!(
                    "catalog {:?} version {} does not project a deliverable v2 manifest",
                    request.catalog_id, request.catalog_version
                ),
                error,
            )
        })?;
    freeze_release(
        transaction,
        request,
        &expected_membership,
        &digest,
        &canonical_bytes,
    )
    .await?;
    Ok(MintedReleaseManifest {
        manifest,
        digest,
        canonical_bytes,
        catalog_schema_version: release.get(1),
    })
}

/// Read the exact canonical bytes one minted release froze, if it is minted.
///
/// Only the bytes are returned: the frozen digest is `sha256(canonical_bytes)`
/// by table constraint, and the publisher re-derives release identity from the
/// bytes it is about to push rather than trusting a second carrier.
pub async fn read_release_snapshot(
    transaction: &Transaction<'_>,
    tenant_id: &str,
    catalog_id: &str,
    catalog_version: i32,
) -> Result<Option<Vec<u8>>, MintManifestError> {
    transaction
        .query_one(CLAIM_TENANT_SQL, &[&tenant_id])
        .await
        .map_err(|error| storage("claim the release tenant", error))?;
    let snapshot = transaction
        .query_opt(
            READ_RELEASE_SNAPSHOT_SQL,
            &[&tenant_id, &catalog_id, &catalog_version],
        )
        .await
        .map_err(|error| storage("read the frozen v2 manifest snapshot", error))?;
    Ok(snapshot.map(|row| row.get(0)))
}

/// Read every admitted component fact of one gate scope.
///
/// Shared with `author-wiring` (wamn-1xb5) so authoring and minting gate a
/// wiring against the same fact set rather than two readers of one table.
pub async fn load_component_facts(
    transaction: &Transaction<'_>,
    scope: &ComponentCatalogScope,
) -> Result<Vec<AdmittedComponent>, MintManifestError> {
    let catalog_version = i32::try_from(scope.catalog_version)
        .expect("a gate scope carries a stored catalog version");
    let rows = transaction
        .query(
            SELECT_COMPONENT_FACTS_SQL,
            &[&scope.tenant_id, &scope.catalog_id, &catalog_version],
        )
        .await
        .map_err(|error| storage("read admitted component facts", error))?;
    rows.into_iter()
        .map(|row| {
            let component: String = row.get(0);
            let decoded = AdmittedComponent {
                scope: scope.clone(),
                component: component.clone(),
                interface_version: row.get(1),
                operation: row.get(2),
                component_digest: row.get(3),
                imports: decode_json(row.get(4), &component, "imports")?,
                imports_fingerprint: row.get(5),
                input_ports: decode_json::<Vec<AdmittedComponentPort>>(
                    row.get(6),
                    &component,
                    "input-ports",
                )?,
                output_ports: decode_json::<Vec<AdmittedComponentPort>>(
                    row.get(7),
                    &component,
                    "output-ports",
                )?,
                parameters: decode_json::<Vec<AdmittedComponentParameter>>(
                    row.get(8),
                    &component,
                    "parameters",
                )?,
                effects: decode_json::<Vec<AdmittedComponentEffect>>(
                    row.get(9),
                    &component,
                    "effects",
                )?,
            };
            // wamn-0h0g.21.10: a row whose effects its own audited imports do
            // not derive was never produced by the validator, so no release may
            // be minted over it until the component is re-admitted.
            wamn_catalog::verify_stored_effect_projection(&decoded).map_err(|error| {
                MintManifestError::with_source(
                    MintManifestErrorKind::Component,
                    format!(
                        "component {component:?} stores an effect projection its audited imports \
                         do not derive; re-admit it through the validator"
                    ),
                    error,
                )
            })?;
            Ok(decoded)
        })
        .collect()
}

fn decode_json<T: DeserializeOwned>(
    stored: String,
    component: &str,
    field: &'static str,
) -> Result<T, MintManifestError> {
    serde_json::from_str(&stored).map_err(|error| {
        MintManifestError::with_source(
            MintManifestErrorKind::Component,
            format!("component {component:?} stores unreadable {field}"),
            error,
        )
    })
}

async fn resolve_current_closure(
    transaction: &Transaction<'_>,
    request: &MintReleaseManifest<'_>,
    scope: &ComponentCatalogScope,
    component_facts: &[AdmittedComponent],
) -> Result<
    (
        BTreeSet<ServingComponent>,
        BTreeSet<ServingWiring>,
        BTreeSet<ReleaseComponentMembership>,
    ),
    MintManifestError,
> {
    let mut components = BTreeSet::new();
    let mut wirings = BTreeSet::new();
    let mut membership = BTreeSet::new();
    for target in request.wirings {
        let version = i32::try_from(target.wiring_version).map_err(|error| {
            MintManifestError::with_source(
                MintManifestErrorKind::Wiring,
                format!(
                    "wiring {:?} version {} exceeds the catalog storage width",
                    target.wiring_id, target.wiring_version
                ),
                error,
            )
        })?;
        let Some(row) = transaction
            .query_opt(
                SELECT_WIRING_SQL,
                &[
                    &request.tenant_id,
                    &request.catalog_id,
                    &target.wiring_id,
                    &version,
                ],
            )
            .await
            .map_err(|error| storage("read a release wiring", error))?
        else {
            return Err(MintManifestError::new(
                MintManifestErrorKind::Wiring,
                format!(
                    "catalog {:?} has no wiring {:?} version {}",
                    request.catalog_id, target.wiring_id, target.wiring_version
                ),
            ));
        };
        let gated_catalog_version: i32 = row.get(0);
        if gated_catalog_version != request.catalog_version {
            return Err(MintManifestError::new(
                MintManifestErrorKind::Wiring,
                format!(
                    "wiring {:?} version {} was gated against catalog version {}, not {}",
                    target.wiring_id,
                    target.wiring_version,
                    gated_catalog_version,
                    request.catalog_version
                ),
            ));
        }
        let stored_hash: String = row.get(1);
        let stored_document: String = row.get(2);
        let document_value = serde_json::from_str(&stored_document).map_err(|error| {
            MintManifestError::with_source(
                MintManifestErrorKind::Wiring,
                format!("wiring {:?} stores unreadable graph JSON", target.wiring_id),
                error,
            )
        })?;
        let document = WiringDocument::parse(&document_value).map_err(|error| {
            MintManifestError::with_source(
                MintManifestErrorKind::Wiring,
                format!("wiring {:?} stores an invalid document", target.wiring_id),
                error,
            )
        })?;
        if document.wiring_id != target.wiring_id || document.version != target.wiring_version {
            return Err(MintManifestError::new(
                MintManifestErrorKind::Wiring,
                format!(
                    "wiring row {:?} version {} stores document {:?} version {}",
                    target.wiring_id, target.wiring_version, document.wiring_id, document.version
                ),
            ));
        }
        let derived_hash = document.wiring_hash();
        if stored_hash != derived_hash.as_str() {
            return Err(MintManifestError::new(
                MintManifestErrorKind::Wiring,
                format!(
                    "wiring {:?} version {} stores hash {:?}, not its canonical document hash {:?}",
                    target.wiring_id,
                    target.wiring_version,
                    stored_hash,
                    derived_hash.as_str()
                ),
            ));
        }
        validate_wiring_compatibility(&document, scope, component_facts).map_err(|error| {
            MintManifestError::with_source(
                MintManifestErrorKind::Component,
                format!(
                    "wiring {:?} version {} is not compatible with its admitted component facts",
                    target.wiring_id, target.wiring_version
                ),
                error,
            )
        })?;

        wirings.insert(ServingWiring {
            wiring_id: target.wiring_id.clone(),
            wiring_version: target.wiring_version,
            graph_hash: derived_hash,
        });
        for node in document.nodes.values() {
            let fact = component_facts
                .iter()
                .find(|fact| {
                    fact.component == node.component
                        && fact.interface_version == node.interface_version
                        && fact.operation == node.operation
                })
                .expect("the semantic gate resolved every wiring node");
            let digest = ArtifactHash::parse(fact.component_digest.clone()).map_err(|error| {
                MintManifestError::with_source(
                    MintManifestErrorKind::Component,
                    format!(
                        "component {:?} interface {:?} stores a non-canonical artifact hash",
                        fact.component, fact.interface_version
                    ),
                    error,
                )
            })?;
            components.insert(ServingComponent {
                component: fact.component.clone(),
                interface_version: fact.interface_version.clone(),
                digest,
            });
            membership.insert(ReleaseComponentMembership {
                wiring_id: target.wiring_id.clone(),
                wiring_version: target.wiring_version,
                component_digest: fact.component_digest.clone(),
            });
        }
    }
    Ok((components, wirings, membership))
}

async fn freeze_release(
    transaction: &Transaction<'_>,
    request: &MintReleaseManifest<'_>,
    expected: &BTreeSet<ReleaseComponentMembership>,
    digest: &ManifestDigest,
    canonical_bytes: &[u8],
) -> Result<(), MintManifestError> {
    let rows = transaction
        .query(
            SELECT_RELEASE_COMPONENTS_SQL,
            &[
                &request.tenant_id,
                &request.catalog_id,
                &request.catalog_version,
            ],
        )
        .await
        .map_err(|error| storage("read the frozen release component closure", error))?;
    let observed = rows
        .into_iter()
        .map(|row| {
            let version: i32 = row.get(1);
            Ok(ReleaseComponentMembership {
                wiring_id: row.get(0),
                wiring_version: serving_version(version, "wiring-version")?,
                component_digest: row.get(2),
            })
        })
        .collect::<Result<BTreeSet<_>, MintManifestError>>()?;
    let snapshot = transaction
        .query_opt(
            SELECT_RELEASE_SNAPSHOT_SQL,
            &[
                &request.tenant_id,
                &request.catalog_id,
                &request.catalog_version,
            ],
        )
        .await
        .map_err(|error| storage("read the frozen v2 manifest snapshot", error))?;

    match (observed.is_empty(), snapshot) {
        (false, Some(snapshot)) => {
            if &observed != expected {
                return Err(MintManifestError::new(
                    MintManifestErrorKind::ClosureConflict,
                    format!(
                        "release component closure is already frozen as {} rows, not the requested {}",
                        observed.len(),
                        expected.len()
                    ),
                ));
            }
            let frozen_digest: String = snapshot.get(0);
            let frozen_bytes: Vec<u8> = snapshot.get(1);
            if frozen_digest != digest.as_str() || frozen_bytes != canonical_bytes {
                return Err(MintManifestError::new(
                    MintManifestErrorKind::ClosureConflict,
                    format!(
                        "release serving facts are already frozen under digest {frozen_digest:?}, not {:?}",
                        digest.as_str()
                    ),
                ));
            }
            return Ok(());
        }
        (true, None) => {}
        _ => {
            return Err(MintManifestError::new(
                MintManifestErrorKind::ClosureConflict,
                "release component membership and v2 manifest snapshot are only partially frozen",
            ));
        }
    }

    for member in expected {
        let version = i32::try_from(member.wiring_version).expect("resolved storage width");
        transaction
            .execute(
                INSERT_RELEASE_COMPONENT_SQL,
                &[
                    &request.tenant_id,
                    &request.catalog_id,
                    &request.catalog_version,
                    &member.wiring_id,
                    &version,
                    &member.component_digest,
                ],
            )
            .await
            .map_err(|error| storage("freeze one release component member", error))?;
    }
    transaction
        .execute(
            INSERT_RELEASE_SNAPSHOT_SQL,
            &[
                &request.tenant_id,
                &request.catalog_id,
                &request.catalog_version,
                &digest.as_str(),
                &canonical_bytes,
            ],
        )
        .await
        .map_err(|error| storage("freeze the complete v2 manifest source facts", error))?;
    Ok(())
}

fn serving_version(version: i32, field: &'static str) -> Result<u32, MintManifestError> {
    let version = u32::try_from(version).map_err(|error| {
        MintManifestError::with_source(
            MintManifestErrorKind::Release,
            format!("{field} {version} is outside the serving-manifest width"),
            error,
        )
    })?;
    if version == 0 {
        return Err(MintManifestError::new(
            MintManifestErrorKind::Release,
            format!("{field} must be greater than zero"),
        ));
    }
    Ok(version)
}

fn storage(context: &'static str, error: tokio_postgres::Error) -> MintManifestError {
    MintManifestError::with_source(MintManifestErrorKind::Storage, context, error)
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;
    use wamn_catalog::ServingRegistrationInput;

    use super::*;

    /// Host command for the flattened argument surface under test.
    #[derive(Debug, clap::Parser)]
    struct PublishProbe {
        #[command(flatten)]
        args: PublishReleaseArgs,
    }

    /// The schema of record declaring the fact this verb's precondition reads.
    const RUN_STATE_SCHEMA: &str = include_str!("../../../deploy/sql/run-state.sql");

    /// The control database the mint projects release identity into. A separate
    /// URL on purpose: the two planes are two databases (wamn-0h0g.8.27).
    const CONTROL_PLANE: [&str; 2] = ["--control-database-url", "postgres://control.invalid/store"];

    const COORDINATE: [&str; 16] = [
        "--database-url",
        "postgres://release.invalid/env",
        "--control-database-url",
        "postgres://control.invalid/store",
        "--org",
        "acme",
        "--project",
        "billing",
        "--tenant",
        "tenant-a",
        "--catalog-id",
        "orders",
        "--catalog-version",
        "3",
        "--run-schema",
        "wamn_run",
    ];

    fn run_schema() -> BareSchemaName {
        BareSchemaName::new("wamn_run").expect("the schema of record is a bare identifier")
    }

    /// A minted release whose four manifest-fixed parts are all distinguishable.
    fn release(environment: &str) -> ServingRelease {
        ServingRelease {
            tenant_id: "tenant-a".to_owned(),
            catalog_id: "orders".to_owned(),
            catalog_version: 7,
            environment: environment.to_owned(),
        }
    }

    fn parse(closure: &[&str]) -> Result<PublishReleaseArgs, clap::Error> {
        let mut argv = vec!["publish-release"];
        argv.extend_from_slice(&COORDINATE);
        argv.extend_from_slice(closure);
        PublishProbe::try_parse_from(argv).map(|probe| probe.args)
    }

    #[test]
    fn the_attestation_key_places_each_part_from_its_own_source() {
        let coordinate = DeploymentCoordinate::new("acme", "billing", &release("prod"));

        // The operator names only the placement. Tenant, catalog, version and
        // environment are read back off the manifest the mint actually froze,
        // so no part of the key can disagree with the deployed bytes.
        assert_eq!(coordinate.triple.org, "acme");
        assert_eq!(coordinate.triple.project, "billing");
        assert_eq!(coordinate.triple.env.as_str(), "prod");
        assert_eq!(coordinate.tenant_id, "tenant-a");
        assert_eq!(coordinate.catalog_id, "orders");
        assert_eq!(coordinate.catalog_version, 7);
    }

    #[test]
    fn the_attestation_key_sources_six_parts_independently() {
        // Destructured exhaustively on purpose: `catalog.deployment_attestations`
        // is UNIQUE on exactly (tenant, catalog, version, org, project,
        // environment), so a part added to or dropped from this key must break
        // the build rather than silently re-key every attestation.
        let DeploymentCoordinate {
            triple: Triple { org, project, env },
            tenant_id,
            catalog_id,
            catalog_version,
        } = DeploymentCoordinate::new("acme", "billing", &release("prod"));
        let parts = [
            org,
            project,
            env.as_str().to_owned(),
            tenant_id,
            catalog_id,
            catalog_version.to_string(),
        ];
        assert_eq!(
            parts.iter().collect::<BTreeSet<_>>().len(),
            parts.len(),
            "two key parts collapsed onto one input: {parts:?}"
        );
    }

    #[test]
    fn a_release_cannot_be_minted_without_its_control_plane_placement() {
        // Neither half of the placement defaults. The `dev` literal this surface
        // used to assume is exactly what must not come back: an unnamed org or
        // project would key the attestation to a deployment nobody requested.
        let closure = [
            "--wiring",
            "orders=1",
            "--attachments",
            "a.json",
            "--registrations",
            "r.json",
        ];
        for placement in [vec!["--org", "acme"], vec!["--project", "billing"], vec![]] {
            let mut argv = vec![
                "publish-release",
                "--database-url",
                "postgres://release.invalid/env",
                "--tenant",
                "tenant-a",
                "--catalog-id",
                "orders",
                "--catalog-version",
                "3",
            ];
            argv.extend_from_slice(&["--run-schema", "wamn_run"]);
            argv.extend_from_slice(&CONTROL_PLANE);
            argv.extend_from_slice(&placement);
            argv.extend_from_slice(&closure);
            assert!(
                PublishProbe::try_parse_from(argv).is_err(),
                "minted a release with placement {placement:?}"
            );
        }

        // The control database is required for the same reason and separately
        // (wamn-0h0g.8.27): a placement that names an org and a project but no
        // control plane could never project the release identity the deployment
        // attestation's foreign key resolves against.
        let mut unprojected = vec![
            "publish-release",
            "--database-url",
            "postgres://release.invalid/env",
            "--tenant",
            "tenant-a",
            "--catalog-id",
            "orders",
            "--catalog-version",
            "3",
            "--run-schema",
            "wamn_run",
            "--org",
            "acme",
            "--project",
            "billing",
        ];
        unprojected.extend_from_slice(&closure);
        assert!(
            PublishProbe::try_parse_from(unprojected).is_err(),
            "minted a release with no control database to project its identity into"
        );

        let placed = parse(&closure).expect("the complete placement parses");
        assert_eq!(placed.org, "acme");
        assert_eq!(placed.project, "billing");
        assert_eq!(
            placed.control_database_url,
            "postgres://control.invalid/store"
        );
    }

    #[test]
    fn the_parsed_placement_reaches_the_attestation_key() {
        // The link the surface exists for: what the operator typed on the
        // command line, not some other string in scope, is what keys the write.
        let args = parse(&[
            "--wiring",
            "orders=1",
            "--attachments",
            "a.json",
            "--registrations",
            "r.json",
        ])
        .expect("the complete surface parses");
        let coordinate = args.deployment_coordinate(&release("prod"));

        assert_eq!(coordinate.triple.org, "acme");
        assert_eq!(coordinate.triple.project, "billing");
        assert_eq!(coordinate.triple.env.as_str(), "prod");
        assert_eq!(coordinate.tenant_id, "tenant-a");
    }

    #[test]
    fn a_release_naming_the_provisioned_environment_is_published() {
        // The match arm: the carried label and the provisioned fact agree, so the
        // attestation key is checked rather than trusted and the mint stands.
        verify_provisioned_environment(Some("prod"), &release("prod"), &run_schema())
            .expect("a release published into the environment this database is");
    }

    #[test]
    fn a_release_naming_another_environment_than_the_database_refuses() {
        // The bead's whole point: `catalog.catalogs.environment` is a free label,
        // so a release row saying `prod` in a database provisioned for `staging`
        // must REFUSE — not warn, and not be coerced to either value.
        let refusal =
            verify_provisioned_environment(Some("staging"), &release("prod"), &run_schema())
                .expect_err("a release keyed to an environment this database is not");

        assert_eq!(
            refusal.kind(),
            MintManifestErrorKind::EnvironmentPolicyMismatch
        );
        let rendered = format!("{refusal}");
        assert!(
            rendered.starts_with(RELEASE_MANIFEST_MINT_REFUSAL),
            "refusal is unlabelled: {rendered}"
        );
        assert!(
            rendered.contains("environment-policy-environment-mismatch"),
            "the mismatch refusal does not carry its own literal: {rendered}"
        );
        // Both sides are named, so the operator can tell which one is wrong.
        assert!(
            rendered.contains("\"prod\"") && rendered.contains("\"staging\""),
            "the mismatch refusal hides one of the two environments: {rendered}"
        );
    }

    #[test]
    fn an_absent_environment_policy_refuses_and_names_the_verb_that_converges_it() {
        // Owner ruling 2026-08-26: absent REFUSES, on its own literal, and the
        // remedy is discoverable in the error text. A guard whose defeat is
        // "delete the row" is weaker than the check this bead asks for.
        let refusal = verify_provisioned_environment(None, &release("prod"), &run_schema())
            .expect_err("a release published against an unprojected environment policy");

        assert_eq!(
            refusal.kind(),
            MintManifestErrorKind::EnvironmentPolicyAbsent
        );
        let rendered = format!("{refusal}");
        assert!(
            rendered.starts_with(RELEASE_MANIFEST_MINT_REFUSAL),
            "refusal is unlabelled: {rendered}"
        );
        assert!(
            rendered.contains("environment-policy-not-converged"),
            "the absent refusal does not carry its own literal: {rendered}"
        );
        // The load-bearing rider: a fail-closed refusal whose remedy is
        // discoverable is a guard; one whose remedy is not is a trap.
        assert!(
            rendered.contains("reconcile-run-plane"),
            "the absent refusal does not name the verb that converges the policy: {rendered}"
        );
        assert!(
            rendered.contains("wamn_run.environment_policies"),
            "the absent refusal does not name the relation it looked in: {rendered}"
        );
    }

    #[test]
    fn absent_and_mismatched_environment_policies_never_collapse_onto_one_literal() {
        // Two different operator situations with two different remedies. If these
        // ever render as one code, an operator reading the error learns nothing
        // about which of the two happened.
        let absent = verify_provisioned_environment(None, &release("prod"), &run_schema())
            .expect_err("an absent policy refuses");
        let mismatch =
            verify_provisioned_environment(Some("staging"), &release("prod"), &run_schema())
                .expect_err("a disagreeing policy refuses");

        assert_eq!(
            MintManifestErrorKind::EnvironmentPolicyAbsent.as_str(),
            "environment-policy-not-converged"
        );
        assert_eq!(
            MintManifestErrorKind::EnvironmentPolicyMismatch.as_str(),
            "environment-policy-environment-mismatch"
        );
        assert_ne!(absent.kind(), mismatch.kind());
        assert_ne!(
            absent.kind().as_str(),
            mismatch.kind().as_str(),
            "the two environment refusals rendered as one literal"
        );
        // Only the absent arm carries a remedy verb: the mismatch remedy is to
        // republish elsewhere or fix the catalog row, not to reconcile.
        assert!(!format!("{mismatch}").contains("reconcile-run-plane"));
    }

    #[test]
    fn the_provisioned_environment_fact_is_read_from_the_relation_of_record() {
        let declaration = RUN_STATE_SCHEMA
            .split_once("CREATE TABLE wamn_run.environment_policies (")
            .expect("run-state.sql declares the provisioned environment relation")
            .1
            .split_once("\n);")
            .expect("the provisioned environment declaration terminates")
            .0;
        for column in ["tenant_id", "expected_environment"] {
            assert!(
                declaration.contains(column),
                "environment_policies no longer declares {column:?}: {declaration}"
            );
        }

        // The read names exactly those two columns, in the schema the operator
        // named rather than a `wamn_run` this verb assumed.
        assert_eq!(
            expected_environment_sql(&run_schema()),
            "SELECT expected_environment FROM \"wamn_run\".environment_policies \
             WHERE tenant_id = $1"
        );
        assert_eq!(
            expected_environment_sql(
                &BareSchemaName::new("poc_f1").expect("a deployed run-plane schema")
            ),
            "SELECT expected_environment FROM \"poc_f1\".environment_policies \
             WHERE tenant_id = $1"
        );

        // The read runs under the tenant claim the mint already set, and is safe
        // to do so only because the relation forces row security behind it. Lose
        // the policy and this precondition starts reading another tenant's fact.
        assert!(
            RUN_STATE_SCHEMA
                .contains("ALTER TABLE wamn_run.environment_policies FORCE ROW LEVEL SECURITY"),
            "the provisioned environment relation no longer forces row security"
        );
        assert!(
            RUN_STATE_SCHEMA.contains("CREATE POLICY environment_policies_tenant"),
            "the provisioned environment relation no longer carries its tenant policy"
        );
    }

    #[test]
    fn a_release_cannot_be_minted_without_the_schema_holding_its_environment_fact() {
        // The run-plane schema does not default: `wamn_run` is the schema of
        // record, but deployed project databases install it under operator-chosen
        // names, and a verify against a schema nobody named is not a verify.
        let closure = [
            "--wiring",
            "orders=1",
            "--attachments",
            "a.json",
            "--registrations",
            "r.json",
        ];
        let mut argv = vec![
            "publish-release",
            "--database-url",
            "postgres://release.invalid/env",
            "--org",
            "acme",
            "--project",
            "billing",
            "--tenant",
            "tenant-a",
            "--catalog-id",
            "orders",
            "--catalog-version",
            "3",
        ];
        argv.extend_from_slice(&closure);
        assert!(
            PublishProbe::try_parse_from(argv).is_err(),
            "minted a release without naming the schema holding its environment fact"
        );

        // What the operator typed, not some other string in scope, is the schema
        // the precondition reads: the wave-58 hole was exactly a parsed argument
        // that no test followed to its use.
        let args = parse(&closure).expect("the complete surface parses");
        let named = args
            .verified_run_schema()
            .expect("the schema of record validates");
        assert_eq!(named.as_str(), "wamn_run");
        assert!(expected_environment_sql(&named).contains("\"wamn_run\".environment_policies"));

        let mut argv = vec!["publish-release"];
        argv.extend_from_slice(&COORDINATE);
        argv.extend_from_slice(&closure);
        let typed = argv
            .iter()
            .position(|argument| *argument == "--run-schema")
            .expect("the coordinate names the run-plane schema");
        argv[typed + 1] = "poc_f1";
        let elsewhere = PublishProbe::try_parse_from(argv)
            .expect("another deployed run-plane schema parses")
            .args
            .verified_run_schema()
            .expect("a deployed run-plane schema validates");
        assert_eq!(elsewhere.as_str(), "poc_f1");
        assert!(expected_environment_sql(&elsewhere).contains("\"poc_f1\".environment_policies"));

        // A name that cannot be a bare schema is refused before any connection.
        let mut argv = vec!["publish-release"];
        argv.extend_from_slice(&COORDINATE);
        argv.extend_from_slice(&closure);
        argv[typed + 1] = "Run Plane\"; DROP SCHEMA catalog";
        assert!(
            PublishProbe::try_parse_from(argv)
                .expect("clap takes any string")
                .args
                .verified_run_schema()
                .is_err(),
            "an unvalidated schema name reached the composed statement"
        );
    }

    #[test]
    fn a_wiring_target_names_one_exact_positive_version() {
        let target: ReleaseWiringTarget = "orders=2".parse().expect("an exact coordinate parses");
        assert_eq!(target.wiring_id, "orders");
        assert_eq!(target.wiring_version, 2);
        for refused in [
            "orders",
            "orders=",
            "=2",
            "orders=0",
            "orders=-1",
            "or ders=1",
        ] {
            assert!(
                refused.parse::<ReleaseWiringTarget>().is_err(),
                "accepted {refused:?}"
            );
        }
    }

    #[test]
    fn the_interim_mint_requires_named_wirings_and_both_documents() {
        let complete = parse(&[
            "--wiring",
            "orders=1",
            "--wiring",
            "shipping=2",
            "--attachments",
            "attachments.json",
            "--registrations",
            "registrations.json",
        ])
        .expect("the interim surface parses");
        assert_eq!(
            complete.wirings,
            vec![
                ReleaseWiringTarget {
                    wiring_id: "orders".to_owned(),
                    wiring_version: 1,
                },
                ReleaseWiringTarget {
                    wiring_id: "shipping".to_owned(),
                    wiring_version: 2,
                },
            ]
        );
        assert_eq!(complete.attachments, PathBuf::from("attachments.json"));
        assert_eq!(complete.registrations, PathBuf::from("registrations.json"));

        // Neither document defaults to the empty set: an empty registration set
        // is a valid manifest with a real digest and must be chosen explicitly.
        let refusals: [Vec<&str>; 3] = [
            vec!["--attachments", "a.json", "--registrations", "r.json"],
            vec!["--wiring", "orders=1", "--registrations", "r.json"],
            vec!["--wiring", "orders=1", "--attachments", "a.json"],
        ];
        for refused in refusals {
            assert!(parse(&refused).is_err(), "accepted {refused:?}");
        }
    }

    #[test]
    fn hand_authored_documents_use_the_serving_manifest_spelling() {
        let attachments: BTreeMap<String, ServingAttachment> = serde_json::from_str(
            r#"{"orders-http":{"kind":"http","wiring-id":"orders","wiring-version":1,
                "definition-hash":"sha256:5555555555555555555555555555555555555555555555555555555555555555",
                "definition":{"id":"orders-http","kind":"http","run-deadline-ms":30000},
                "auth-policy":{"mode":"none"}}}"#,
        )
        .expect("a hand-authored attachment document parses");
        let attachment = &attachments["orders-http"];
        assert_eq!(attachment.wiring_version, 1);
        assert_eq!(attachment.auth_policy, serde_json::json!({"mode": "none"}));

        let registrations: BTreeMap<String, ServingRegistration> = serde_json::from_str(
            r#"{"orders-changed":{"wiring-id":"shipping","wiring-version":2,
                "entity":"orders","ops":["insert"],"input":"batch"}}"#,
        )
        .expect("a hand-authored registration document parses");
        let registration = &registrations["orders-changed"];
        assert_eq!(registration.entity, "orders");
        assert_eq!(registration.input, ServingRegistrationInput::Batch);

        let none: BTreeMap<String, ServingRegistration> =
            serde_json::from_str("{}").expect("an explicitly empty document parses");
        assert!(none.is_empty());
    }
}
