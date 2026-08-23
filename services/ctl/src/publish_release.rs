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

use std::collections::{BTreeMap, BTreeSet};

use serde::de::DeserializeOwned;
use tokio_postgres::Transaction;
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

/// One exact wiring version included in a release closure.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReleaseWiringTarget {
    pub wiring_id: String,
    pub wiring_version: u32,
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
