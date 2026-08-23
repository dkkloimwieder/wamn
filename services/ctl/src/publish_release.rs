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
//! projects them: `catalog.release_attachments` is flow-keyed and carries
//! neither a wiring version nor an auth policy, and `catalog.event_registrations`
//! is absent from the control portable store. Ruling `wamn-0h0g.15.164` moves
//! registrations into that store; the projection replaces these two arguments
//! then. Both stay REQUIRED until it does, because an empty registration set is
//! a valid manifest with a real digest and must be chosen, never defaulted.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use clap::Args;
use serde::de::DeserializeOwned;
use tokio_postgres::{Client, NoTls, Transaction};
use wamn_catalog::{
    AdmittedComponent, AdmittedComponentParameter, AdmittedComponentPort, ArtifactHash,
    ComponentCatalogScope, ManifestDigest, SERVING_MANIFEST_FORMAT_VERSION, ServingAttachment,
    ServingComponent, ServingManifest, ServingRegistration, ServingRelease, ServingWiring,
    WiringDocument, validate_wiring_compatibility,
};

const CLAIM_TENANT_SQL: &str = "SELECT set_config('app.tenant', $1, true)";

const LOCK_RELEASE_SQL: &str = "\
SELECT catalog.environment \
  FROM catalog.release_manifests AS release \
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
       output_ports::text, parameters::text \
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

/// One minted serving manifest and the bytes its digest names.
#[derive(Clone, Debug, PartialEq)]
pub struct MintedReleaseManifest {
    pub manifest: ServingManifest,
    pub digest: ManifestDigest,
    pub canonical_bytes: Vec<u8>,
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

    /// Tenant claim carried by the release.
    #[arg(long)]
    pub tenant: String,

    /// Catalog identity of the release.
    #[arg(long)]
    pub catalog_id: String,

    /// Exact catalog version whose release identity is frozen.
    #[arg(long)]
    pub catalog_version: u32,

    /// Exact wiring version in the closure. Repeat once per released wiring.
    #[arg(long = "wiring", value_name = "WIRING_ID=VERSION", required = true)]
    pub wirings: Vec<ReleaseWiringTarget>,

    /// INTERIM: JSON object of attachment id to serving attachment; `{}` for none.
    #[arg(long)]
    pub attachments: PathBuf,

    /// INTERIM: JSON object of registration id to serving registration; `{}` for none.
    #[arg(long)]
    pub registrations: PathBuf,
}

/// Mint one v2 release from explicit wiring, attachment, and registration facts.
pub async fn run(args: PublishReleaseArgs) -> anyhow::Result<()> {
    let attachments: BTreeMap<String, ServingAttachment> =
        read_document(&args.attachments, "attachments")?;
    let registrations: BTreeMap<String, ServingRegistration> =
        read_document(&args.registrations, "registrations")?;
    let catalog_version = i32::try_from(args.catalog_version)
        .context("catalog-version exceeds the PostgreSQL integer carrier")?;
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
    let minted = mint_in_transaction(&mut client, &request).await;
    match minted {
        Ok(digest) => {
            drop(client);
            connection_task
                .await
                .context("join the release mint connection")?
                .context("drive the release mint connection")?;
            println!("{digest}");
            Ok(())
        }
        Err(error) => {
            connection_task.abort();
            Err(error)
        }
    }
}

async fn mint_in_transaction(
    client: &mut Client,
    request: &MintReleaseManifest<'_>,
) -> anyhow::Result<ManifestDigest> {
    let transaction = client
        .transaction()
        .await
        .context("begin the release mint")?;
    let minted = mint_release_manifest(&transaction, request).await?;
    transaction
        .commit()
        .await
        .context("commit the release mint")?;
    Ok(minted.digest)
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
    let component_facts = load_component_facts(transaction, request, &scope).await?;
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

async fn load_component_facts(
    transaction: &Transaction<'_>,
    request: &MintReleaseManifest<'_>,
    scope: &ComponentCatalogScope,
) -> Result<Vec<AdmittedComponent>, MintManifestError> {
    let rows = transaction
        .query(
            SELECT_COMPONENT_FACTS_SQL,
            &[
                &request.tenant_id,
                &request.catalog_id,
                &request.catalog_version,
            ],
        )
        .await
        .map_err(|error| storage("read admitted component facts", error))?;
    rows.into_iter()
        .map(|row| {
            let component: String = row.get(0);
            Ok(AdmittedComponent {
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
            })
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

    const COORDINATE: [&str; 8] = [
        "--database-url",
        "postgres://release.invalid/env",
        "--tenant",
        "tenant-a",
        "--catalog-id",
        "orders",
        "--catalog-version",
        "3",
    ];

    fn parse(closure: &[&str]) -> Result<PublishReleaseArgs, clap::Error> {
        let mut argv = vec!["publish-release"];
        argv.extend_from_slice(&COORDINATE);
        argv.extend_from_slice(closure);
        PublishProbe::try_parse_from(argv).map(|probe| probe.args)
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
