//! The blobstore host plugin: registration, invocation facts, and the live
//! binding lookup.
//!
//! Mirrors `ConnectionHttp`'s shape deliberately. It keeps its OWN invocation
//! registry rather than reading the HTTP plugin's, because a second reader on
//! one registry is the defect class `wamn-0h0g.21.9` records, and two sibling
//! plugins should not couple over something neither owns. Extracting a shared
//! registry is the right end state and is deferred to capability three
//! (`wamn-jpxo`, and inherited by the seam template) — two consumers is the
//! wrong count to extract at.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

use object_store::ObjectStore;
use object_store::aws::AmazonS3Builder;
use wash_runtime::engine::ctx::{SharedCtx, extract_active_ctx};
use wash_runtime::plugin::HostPlugin;
use wash_runtime::wasmtime::component::Linker;
use wash_runtime::wit::{WitInterface, WitWorld};

use crate::plugins::effect_span::{EffectIdentity, EffectWiring, effect_span, record_wiring};

use super::binding::{self, BindingError};
use super::store::BoundContainer;
use wamn_catalog::ServingManifest;

use crate::plugins::connection_http::{
    ConnectionExecutionClosure, ConnectionInvocation, authorize_candidate_closure,
    authorize_release_closure,
};
use crate::plugins::wamn_credentials::WamnCredentials;
use crate::plugins::wamn_postgres::{
    CandidateConnectionBinding, ConnectionEffectLookup, ConnectionEffectSnapshot, WamnPostgres,
};
use crate::release_manifest::LoadedRelease;

/// Plugin id, as the host registry knows it.
pub const WAMN_BLOBSTORE_ID: &str = "wamn-blobstore";

/// The WAMN blobstore capability.
pub struct WamnBlobstore {
    postgres: Arc<WamnPostgres>,
    vault: Arc<WamnCredentials>,
    pub(super) tenant: Box<str>,
    pub(super) project: Box<str>,
    /// The mounted, digest-verified serving manifest, when this host serves a
    /// release. A CANDIDATE closure carries its effective release and
    /// environment in the invocation; a RELEASED one does not, and this is the
    /// only thing that can supply them without guessing at the coordinates
    /// that decide which binding authorizes.
    release: Option<Arc<LoadedRelease>>,
    /// Component-store owner id to the invocation currently using that pooled
    /// instance. The driver binds before `handler.run` and revokes before
    /// returning the instance to the pool.
    invocations: RwLock<HashMap<String, ConnectionInvocation>>,
}

/// The effective release and environment one invocation authorizes under.
///
/// A CANDIDATE closure states them itself. A RELEASED one takes them from the
/// mounted serving manifest, after TWO CHECKS — because a mounted manifest is
/// an input like any other:
///
/// - it must belong to THIS tenant, or a manifest served for another one would
///   hand a guest an effective release under which some other tenant's binding
///   authorizes;
/// - it must actually CONTAIN the package the invocation claims, or a release
///   that never shipped this package would still supply coordinates for it.
///
/// Pure and separate from the plugin, so the decision gating every released
/// effect can be asserted without a database or an object store.
fn release_coordinates(
    invocation: &ConnectionInvocation,
    manifest: Option<&ServingManifest>,
    tenant: &str,
) -> Result<(i32, String), BindingError> {
    match (&invocation.closure, manifest) {
        (
            ConnectionExecutionClosure::Candidate {
                effective_release_id,
                environment,
                ..
            },
            None,
        ) => Ok((
            i32::try_from(*effective_release_id).map_err(|_| BindingError::Unauthorized)?,
            environment.clone(),
        )),
        (ConnectionExecutionClosure::Released, Some(manifest)) => {
            if manifest.release.tenant_id != tenant
                || !manifest
                    .release
                    .packages
                    .iter()
                    .any(|package| package.package_id() == invocation.package_id.as_str())
            {
                return Err(BindingError::Unauthorized);
            }
            Ok((
                i32::try_from(manifest.release.effective_release_id.get())
                    .map_err(|_| BindingError::Unauthorized)?,
                manifest.release.environment.clone(),
            ))
        }
        // A released closure with no mounted manifest, or a candidate handed
        // one, is a caller mismatch rather than a policy question. It refuses
        // instead of picking whichever arm looks closer, because guessing here
        // decides which binding authorizes.
        _ => Err(BindingError::Unauthorized),
    }
}

/// Authorize one invocation's execution closure against the snapshot the
/// connection authority returned.
///
/// A RELEASED closure must be carried by the mounted manifest. A CANDIDATE
/// closure must agree with the wiring hash, component and interface version
/// frozen at admission, and the snapshot must equal the frozen binding row.
///
/// Both rules come from the HTTP capability and are called, not copied. The
/// blobstore applied the released rule alone and let every candidate closure
/// through with no closure check at all (`wamn-b2m6.6`). One spelling of each
/// rule now authorizes both surfaces.
///
/// Pure and separate from the plugin, so the decision gating every effect can
/// be asserted without a database or an object store. It returns the reason it
/// refused and the caller logs it, which keeps every refusal named.
fn authorize_closure(
    invocation: &ConnectionInvocation,
    released_manifest: Option<&ServingManifest>,
    candidate_binding: Option<&CandidateConnectionBinding>,
    snapshot: &ConnectionEffectSnapshot,
) -> Result<(), &'static str> {
    match (released_manifest, candidate_binding) {
        (Some(manifest), None) => authorize_release_closure(manifest, invocation, snapshot)
            .map_err(|_| "the release closure does not carry this component and wiring"),
        (None, Some(binding)) => authorize_candidate_closure(invocation, snapshot, binding)
            .map_err(|_| "the candidate closure disagrees with the frozen wiring or binding"),
        // A closure with neither authorization input, or with both, is a
        // caller mismatch. It refuses rather than authorizing under whichever
        // input is present.
        _ => Err("closure kind disagrees with the authorization inputs"),
    }
}

impl std::fmt::Debug for WamnBlobstore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The vault is never rendered.
        formatter
            .debug_struct("WamnBlobstore")
            .field("tenant", &self.tenant)
            .field("project", &self.project)
            .finish_non_exhaustive()
    }
}

impl WamnBlobstore {
    /// Build the plugin over the connection-authority reader and the vault.
    pub fn new(
        postgres: Arc<WamnPostgres>,
        vault: Arc<WamnCredentials>,
        tenant: impl Into<Box<str>>,
        project: impl Into<Box<str>>,
        release: Option<Arc<LoadedRelease>>,
    ) -> Self {
        Self {
            postgres,
            vault,
            tenant: tenant.into(),
            project: project.into(),
            release,
            invocations: RwLock::new(HashMap::new()),
        }
    }

    /// Bind the exact invocation facts before entering one pooled component.
    ///
    /// A still-bound owner refuses rather than silently replacing leaked
    /// state — the same rule the HTTP plugin holds, for the same reason: a
    /// stale invocation would authorize the next guest against the previous
    /// one's wiring position.
    pub fn bind_invocation(
        &self,
        component_id: &str,
        invocation: ConnectionInvocation,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(!component_id.is_empty(), "component id must be present");
        let mut invocations = self
            .invocations
            .write()
            .map_err(|_| anyhow::anyhow!("blobstore invocation registry is poisoned"))?;
        anyhow::ensure!(
            !invocations.contains_key(component_id),
            "component {component_id} still holds a bound blobstore invocation"
        );
        invocations.insert(component_id.to_owned(), invocation);
        Ok(())
    }

    /// Release the invocation when the instance returns to the pool.
    pub fn revoke_invocation(&self, component_id: &str) {
        if let Ok(mut invocations) = self.invocations.write() {
            invocations.remove(component_id);
        }
    }

    /// The invocation currently bound to one pooled instance.
    #[must_use]
    pub fn invocation(&self, component_id: &str) -> Option<ConnectionInvocation> {
        self.invocations
            .read()
            .ok()
            .and_then(|invocations| invocations.get(component_id).cloned())
    }

    /// Resolve one store alias into a confined container.
    ///
    /// The guest names its own declared STORE ALIAS, never a container: the
    /// container is environment-owned, so a guest that had to name it would
    /// need the coordinate the confinement exists to keep from it.
    pub(super) async fn container_for(
        &self,
        component_id: &str,
        store_alias: &str,
    ) -> Result<BoundContainer, BindingError> {
        // EVERY REFUSAL NAMES ITSELF. The guest sees one opaque error and the
        // router logs one context line, so a refusal that stays silent here
        // is undiagnosable from any evidence -- six cluster runs' worth
        // (wamn-362o.45). The warn carries the alias and component, never
        // the credential.
        let refused = |reason: &'static str| {
            tracing::warn!(
                store_alias,
                component_id,
                reason,
                "blobstore binding refused"
            );
            BindingError::Unauthorized
        };
        let invocation = self
            .invocation(component_id)
            .ok_or_else(|| refused("no invocation is registered for the component"))?;
        let wiring_version = i32::try_from(invocation.wiring_version)
            .map_err(|_| refused("wiring version does not fit the authority's column"))?;
        let released_manifest = match &invocation.closure {
            ConnectionExecutionClosure::Released => Some(
                self.release
                    .as_deref()
                    .ok_or_else(|| refused("a released closure with no release manifest mounted"))?
                    .manifest(),
            ),
            ConnectionExecutionClosure::Candidate { .. } => None,
        };
        // The binding frozen at candidate admission for exactly this component
        // and alias. The authority query is narrowed to it, and the snapshot is
        // compared against it once the row comes back. A candidate closure
        // refuses here when the frozen world holds no row for it, before any
        // query runs.
        let candidate_binding = match &invocation.closure {
            ConnectionExecutionClosure::Released => None,
            ConnectionExecutionClosure::Candidate { binding_world, .. } => Some(
                binding_world
                    .binding(&invocation.component_digest, store_alias)
                    .ok_or_else(|| refused("the frozen world holds no binding for this alias"))?,
            ),
        };
        let (effective_release_id, environment) = release_coordinates(
            &invocation,
            released_manifest,
            &self.tenant,
        )
        .map_err(|_| {
            refused(
                "release coordinates: tenant, package or closure kind disagree with the manifest",
            )
        })?;
        let snapshot = self
            .postgres
            .connection_effect_snapshot(
                component_id,
                &self.project,
                &self.tenant,
                &ConnectionEffectLookup {
                    package_id: &invocation.package_id,
                    wiring_package_id: &invocation.origin.wiring_package_id,
                    origin_package_id: &invocation.origin.package_id,
                    origin_component_digest: &invocation.origin.component_digest,
                    origin_component: &invocation.origin.component,
                    origin_interface_version: &invocation.origin.interface_version,
                    origin_operation: &invocation.origin.operation,
                    operation: &invocation.operation,
                    effective_release_id,
                    environment: &environment,
                    wiring_id: &invocation.wiring_id,
                    wiring_version,
                    node_id: &invocation.node_id,
                    component_digest: &invocation.component_digest,
                    store_alias,
                    candidate_binding,
                },
            )
            .await
            .map_err(|error| {
                tracing::warn!(error = %error, "blobstore connection authority snapshot failed");
                BindingError::Unauthorized
            })?
            .ok_or_else(|| {
                tracing::warn!(
                    store_alias,
                    component_id,
                    package_id = %invocation.package_id,
                    effective_release_id,
                    environment = %environment,
                    wiring_id = %invocation.wiring_id,
                    wiring_version,
                    node_id = %invocation.node_id,
                    component_digest = %invocation.component_digest,
                    "blobstore binding refused: the connection authority holds no binding for this closure"
                );
                BindingError::Unauthorized
            })?;
        authorize_closure(&invocation, released_manifest, candidate_binding, &snapshot)
            .map_err(refused)?;
        let bound = binding::resolve(&snapshot).map_err(|error| {
            tracing::warn!(store_alias, component_id, error = %error, "blobstore binding refused: the binding does not resolve");
            error
        })?;
        let secret = self
            .vault
            .lookup(&self.project, &bound.credential_handle)
            .ok_or_else(|| {
                tracing::warn!(
                    store_alias,
                    component_id,
                    credential_handle = %bound.credential_handle,
                    project = %self.project,
                    "blobstore binding refused: no credential under this project for the handle"
                );
                BindingError::NoCredential
            })?;
        let store = build_store(&bound, &secret).map_err(|error| {
            tracing::warn!(error = %error, "blobstore client construction failed");
            BindingError::Unauthorized
        })?;
        Ok(BoundContainer::new(store, bound.container, bound.prefix))
    }
}

/// Build the S3 client for one binding.
///
/// The secret enters HERE and nowhere else: it is handed straight to the
/// signer and is never stored on [`BoundContainer`], never logged, and never
/// reachable from a guest-visible structure.
fn build_store(
    bound: &binding::BlobstoreBinding,
    secret: &str,
) -> anyhow::Result<Arc<dyn ObjectStore>> {
    let credential: serde_json::Value = serde_json::from_str(secret)
        .map_err(|_| anyhow::anyhow!("object-store credential is not JSON"))?;
    let access_key = credential
        .get("ACCESS_KEY_ID")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("object-store credential lacks ACCESS_KEY_ID"))?;
    let secret_key = credential
        .get("ACCESS_SECRET_KEY")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("object-store credential lacks ACCESS_SECRET_KEY"))?;

    let store = AmazonS3Builder::new()
        .with_endpoint(&bound.endpoint)
        .with_bucket_name(&bound.container)
        .with_access_key_id(access_key)
        .with_secret_access_key(secret_key)
        .with_region("us-east-1")
        .with_allow_http(bound.endpoint.starts_with("http://"))
        .with_virtual_hosted_style_request(false)
        .build()?;
    Ok(Arc::new(store))
}

/// The `wamn.blobstore` span over one guest object-store effect.
///
/// Carries the shared identity vocabulary plus the invocation this component
/// was entered under: the package, the wiring position, the occurrence, the
/// admitted component name and the node operation, copied from the
/// host-attested invocation — never anything the guest sent. A
/// pooled instance with no invocation bound records those keys empty, which says
/// "about to be refused" where a missing field would look like lost
/// instrumentation.
///
/// The object KEY is deliberately absent: keys are guest-authored and can carry
/// tenant data, and a span is a wider audience than the effect itself.
pub(super) fn blobstore_span(
    plugin: &WamnBlobstore,
    component_id: &str,
    operation: &'static str,
) -> tracing::Span {
    let span = effect_span!(
        "wamn.blobstore",
        EffectIdentity {
            tenant: &plugin.tenant,
            project: &plugin.project,
            component: component_id,
        },
        None,
        effect.operation = operation,
    );
    let invocation = plugin.invocation(component_id);
    record_wiring(
        &span,
        invocation.as_ref().map(|invocation| EffectWiring {
            package_id: &invocation.package_id,
            wiring_id: &invocation.wiring_id,
            wiring_version: invocation.wiring_version,
            node_id: &invocation.node_id,
            occurrence: invocation.occurrence,
            component_digest: &invocation.component_digest,
            component_name: &invocation.component,
            operation: &invocation.operation,
        }),
    );
    span
}

impl HostPlugin for WamnBlobstore {
    fn id(&self) -> &'static str {
        WAMN_BLOBSTORE_ID
    }

    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::from([
                WitInterface::from("wasmcloud:blobstore/types@0.1.0"),
                WitInterface::from("wasmcloud:blobstore/container@0.1.0"),
                WitInterface::from("wasmcloud:blobstore/blobstore@0.1.0"),
            ]),
            exports: HashSet::new(),
        }
    }
}

/// Wire the blobstore host functions into a linker.
pub fn add_to_linker(linker: &mut Linker<SharedCtx>) -> wash_runtime::wasmtime::Result<()> {
    super::bindings::wasmcloud::blobstore::types::add_to_linker::<SharedCtx, SharedCtx>(
        linker,
        extract_active_ctx,
    )?;
    super::bindings::wasmcloud::blobstore::container::add_to_linker::<SharedCtx, SharedCtx>(
        linker,
        extract_active_ctx,
    )?;
    super::bindings::wasmcloud::blobstore::blobstore::add_to_linker::<SharedCtx, SharedCtx>(
        linker,
        extract_active_ctx,
    )
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use wamn_catalog::{
        ArtifactHash, DefinitionHash, EffectiveReleaseId, PackageCoordinate,
        SERVING_MANIFEST_FORMAT_VERSION, ServingComponent, ServingComponentOperation,
        ServingRelease, ServingWiring,
    };

    use super::*;
    use crate::plugins::connection_http::ConnectionOrigin;
    use crate::plugins::effect_span::span_proof::{SpanHarness, expected_attributes};
    use crate::plugins::wamn_postgres::{CandidateBindingWorld, WamnPostgresConfig};

    fn released() -> ConnectionInvocation {
        ConnectionInvocation {
            origin: ConnectionOrigin {
                wiring_package_id: "package_a".to_string(),
                package_id: "package_a".to_string(),
                component_digest: format!("sha256:{}", "a".repeat(64)),
                component: "archiver".to_string(),
                interface_version: "0.1.0".to_string(),
                operation: "orders:archive/store@1.0.0".to_string(),
            },
            package_id: "package_a".to_string(),
            wiring_id: "orders".to_string(),
            wiring_version: 3,
            node_id: "archive".to_string(),
            occurrence: 1,
            component_digest: format!("sha256:{}", "a".repeat(64)),
            component: "archiver".to_string(),
            operation: "orders:archive/store@1.0.0".to_string(),
            closure: ConnectionExecutionClosure::Released,
            effects: None,
        }
    }

    fn candidate_with(binding_world: Arc<CandidateBindingWorld>) -> ConnectionInvocation {
        ConnectionInvocation {
            closure: ConnectionExecutionClosure::Candidate {
                effective_release_id: 9,
                environment: "staging".to_string(),
                wiring_hash: format!("sha256:{}", "b".repeat(64)),
                component: "archiver".to_string(),
                interface_version: "0.1.0".to_string(),
                binding_world,
            },
            ..released()
        }
    }

    fn candidate() -> ConnectionInvocation {
        candidate_with(Arc::new(
            CandidateBindingWorld::from_json(serde_json::json!([]))
                .expect("an empty candidate binding world decodes"),
        ))
    }

    fn manifest(tenant: &str, package: &str) -> ServingManifest {
        ServingManifest {
            format_version: SERVING_MANIFEST_FORMAT_VERSION,
            release: ServingRelease {
                tenant_id: tenant.to_string(),
                effective_release_id: EffectiveReleaseId::new(4).expect("a positive release id"),
                environment: "warehouse-eu-3".to_string(),
                packages: BTreeSet::from([
                    PackageCoordinate::new(package, "1.0.0").expect("a canonical coordinate")
                ]),
            },
            components: BTreeSet::new(),
            wirings: BTreeSet::new(),
            attachments: BTreeMap::new(),
            registrations: BTreeMap::new(),
        }
    }

    /// A candidate closure states its own coordinates and needs no manifest.
    #[test]
    fn a_candidate_closure_carries_its_own_coordinates() {
        let coordinates =
            release_coordinates(&candidate(), None, "tenant-a").expect("a candidate resolves");
        assert_eq!(coordinates, (9, "staging".to_string()));
    }

    /// EXIT GATE: a released closure resolves through the mounted manifest,
    /// where before it refused outright and the capability was unusable in any
    /// released deployment.
    ///
    /// The fixture environment is deliberately UNGUESSABLE. A hardcoded
    /// "prod" reads the same as a manifest lookup when the fixture itself says
    /// "prod", and a mutant that hardcoded it survived this test until the
    /// value was changed — the distinguishing-step law in miniature.
    #[test]
    fn a_released_closure_resolves_through_the_mounted_manifest() {
        let manifest = manifest("tenant-a", "package_a");
        let coordinates = release_coordinates(&released(), Some(&manifest), "tenant-a")
            .expect("a released closure resolves");
        assert_eq!(coordinates, (4, "warehouse-eu-3".to_string()));
    }

    /// A manifest for ANOTHER tenant must not supply coordinates: it would
    /// hand this guest an effective release under which some other tenant's
    /// binding authorizes — a cross-tenant reach wearing a mounted file.
    #[test]
    fn a_manifest_for_another_tenant_refuses() {
        let manifest = manifest("tenant-b", "package_a");
        assert_eq!(
            release_coordinates(&released(), Some(&manifest), "tenant-a"),
            Err(BindingError::Unauthorized)
        );
    }

    /// A release that never shipped this package must not supply coordinates
    /// for it.
    #[test]
    fn a_manifest_without_the_invoked_package_refuses() {
        let manifest = manifest("tenant-a", "package_b");
        assert_eq!(
            release_coordinates(&released(), Some(&manifest), "tenant-a"),
            Err(BindingError::Unauthorized)
        );
    }

    /// No mounted manifest means no released coordinates. This is the
    /// fail-closed arm the capability had for EVERY release before the loaded release
    /// was wired, and it stays correct when the loaded release is absent.
    #[test]
    fn a_released_closure_without_a_manifest_refuses() {
        assert_eq!(
            release_coordinates(&released(), None, "tenant-a"),
            Err(BindingError::Unauthorized)
        );
    }

    /// A candidate closure handed a manifest is a caller mismatch, not a
    /// policy question — it refuses rather than picking whichever arm looks
    /// closer, because guessing here decides which binding authorizes.
    #[test]
    fn a_candidate_closure_handed_a_manifest_refuses() {
        let manifest = manifest("tenant-a", "package_a");
        assert_eq!(
            release_coordinates(&candidate(), Some(&manifest), "tenant-a"),
            Err(BindingError::Unauthorized)
        );
    }

    /// The store alias the candidate fixtures freeze a binding for.
    const CANDIDATE_ALIAS: &str = "cold-store";

    /// The binding world private admission froze, in its persisted JSON shape,
    /// carrying one row for this component and alias.
    fn frozen_world() -> CandidateBindingWorld {
        CandidateBindingWorld::from_json(serde_json::json!([{
            "component-digest": format!("sha256:{}", "a".repeat(64)),
            "store-alias": CANDIDATE_ALIAS,
            "requirement-hash": format!("sha256:{}", "c".repeat(64)),
            "instance-id": "cold-store-instance",
            "instance-revision": 2,
            "requirement-type": "blobstore",
            "contract": "wasmcloud:blobstore",
            "validation-hash": format!("sha256:{}", "d".repeat(64)),
            "generation": 7,
            "definition-hash": format!("sha256:{}", "e".repeat(64)),
            "credential-set-handle": "cold-store-v7",
        }]))
        .expect("the frozen binding row is complete and canonical")
    }

    /// The one row the frozen world carries, found the way `container_for`
    /// finds it.
    fn frozen_binding(world: &CandidateBindingWorld) -> &CandidateConnectionBinding {
        world
            .binding(&released().component_digest, CANDIDATE_ALIAS)
            .expect("the frozen world carries a binding for this component and alias")
    }

    /// An authority snapshot that agrees with the candidate closure and equals
    /// the frozen row in every field the frozen row names.
    fn matching_snapshot(binding: &CandidateConnectionBinding) -> ConnectionEffectSnapshot {
        ConnectionEffectSnapshot {
            wiring_hash: format!("sha256:{}", "b".repeat(64)),
            component: Some("archiver".to_string()),
            interface_version: Some("0.1.0".to_string()),
            operation: Some("orders:archive/store@1.0.0".to_string()),
            registered_operation: Some("package-a:orders/archive@1.0.0".to_string()),
            requirement_json: None,
            requirement_hash: Some(binding.requirement_hash.clone()),
            node_permitted: true,
            binding_active: true,
            binding_valid: true,
            instance_id: Some(binding.instance_id.clone()),
            validation_hash: Some(binding.validation_hash.clone()),
            requirement_type: Some(binding.requirement_type.clone()),
            contract: Some(binding.contract.clone()),
            instance_enabled: true,
            active_generation: Some(binding.generation),
            instance_revision: Some(binding.instance_revision),
            generation: Some(binding.generation),
            definition: None,
            definition_hash: Some(binding.definition_hash.clone()),
            credential_handle: Some(binding.credential_set_handle.clone()),
        }
    }

    /// A manifest that carries the released fixture's component and wiring, so
    /// the released arm can be proven to ADMIT as well as to refuse.
    fn carrying_manifest(snapshot: &ConnectionEffectSnapshot) -> ServingManifest {
        let invocation = released();
        let mut manifest = manifest("tenant-a", "package_a");
        manifest.components = BTreeSet::from([ServingComponent {
            package_id: invocation.package_id.clone(),
            component: snapshot
                .component
                .clone()
                .expect("the fixture snapshot names a component"),
            interface_version: snapshot
                .interface_version
                .clone()
                .expect("the fixture snapshot names an interface version"),
            digest: ArtifactHash::parse(invocation.component_digest)
                .expect("the fixture digest is canonical"),
            operations: BTreeMap::from([(
                snapshot
                    .operation
                    .clone()
                    .expect("the fixture snapshot names an operation"),
                ServingComponentOperation {
                    fresh_only: false,
                    committed_result_schema: None,
                    registered_operation: snapshot.registered_operation.clone(),
                    dependencies: Vec::new(),
                    statements: BTreeMap::new(),
                },
            )]),
        }]);
        manifest.wirings = BTreeSet::from([ServingWiring {
            package_id: invocation.package_id,
            wiring_id: invocation.wiring_id,
            wiring_version: invocation.wiring_version,
            graph_hash: DefinitionHash::parse(snapshot.wiring_hash.clone())
                .expect("the fixture wiring hash is canonical"),
        }]);
        manifest
    }

    /// THE DEFECT (`wamn-b2m6.6`). A candidate closure whose authority snapshot
    /// disagrees with the binding frozen at admission is refused.
    ///
    /// The blobstore ran NO closure check on a candidate. It passed
    /// `candidate_binding: None` to the authority and authorized only when a
    /// released manifest was mounted, so a snapshot naming another credential,
    /// another instance or another generation reached `binding::resolve` and
    /// the effect went out. The HTTP surface refused the same snapshot.
    #[test]
    fn a_candidate_snapshot_that_disagrees_with_the_frozen_binding_refuses() {
        let world = Arc::new(frozen_world());
        let binding = frozen_binding(&world);
        let invocation = candidate_with(Arc::clone(&world));
        let mut snapshot = matching_snapshot(binding);
        snapshot.credential_handle = Some("cold-store-v8".to_string());

        assert_eq!(
            authorize_closure(&invocation, None, Some(binding), &snapshot),
            Err("the candidate closure disagrees with the frozen wiring or binding"),
        );
    }

    /// A snapshot taken from another wiring is refused even when it equals the
    /// frozen binding row, because the frozen closure names the wiring too.
    #[test]
    fn a_candidate_snapshot_from_another_wiring_refuses() {
        let world = Arc::new(frozen_world());
        let binding = frozen_binding(&world);
        let invocation = candidate_with(Arc::clone(&world));
        let mut snapshot = matching_snapshot(binding);
        snapshot.wiring_hash = format!("sha256:{}", "f".repeat(64));

        assert_eq!(
            authorize_closure(&invocation, None, Some(binding), &snapshot),
            Err("the candidate closure disagrees with the frozen wiring or binding"),
        );
    }

    /// The rule admits the run it was frozen for. A check that refuses every
    /// candidate satisfies the two tests above and breaks the surface.
    #[test]
    fn a_candidate_snapshot_that_matches_the_frozen_binding_is_admitted() {
        let world = Arc::new(frozen_world());
        let binding = frozen_binding(&world);
        let invocation = candidate_with(Arc::clone(&world));
        let snapshot = matching_snapshot(binding);

        assert_eq!(
            authorize_closure(&invocation, None, Some(binding), &snapshot),
            Ok(())
        );
    }

    /// A candidate closure whose frozen world holds no binding for the alias
    /// has nothing to authorize against, so it refuses. `container_for` refuses
    /// on the same missing row before it queries the authority, which is what
    /// the HTTP surface does.
    #[test]
    fn a_candidate_closure_with_no_frozen_binding_refuses() {
        let world = frozen_world();
        assert!(
            world
                .binding(&released().component_digest, "hot-store")
                .is_none()
        );
        let snapshot = matching_snapshot(frozen_binding(&world));

        assert_eq!(
            authorize_closure(&candidate(), None, None, &snapshot),
            Err("closure kind disagrees with the authorization inputs"),
        );
    }

    /// The released path is unchanged: a manifest that carries the component
    /// and the wiring admits.
    #[test]
    fn a_released_closure_the_manifest_carries_is_admitted() {
        let world = frozen_world();
        let snapshot = matching_snapshot(frozen_binding(&world));
        let manifest = carrying_manifest(&snapshot);

        assert_eq!(
            authorize_closure(&released(), Some(&manifest), None, &snapshot),
            Ok(())
        );
    }

    #[test]
    fn a_nested_blobstore_effect_requires_its_roots_dependency_grant() {
        let world = frozen_world();
        let mut snapshot = matching_snapshot(frozen_binding(&world));
        let mut manifest = carrying_manifest(&snapshot);
        let mut invocation = released();
        let mut root = manifest.components.pop_first().expect("root component");
        let mut child = root.clone();
        child.package_id = "package_b".to_string();
        child.component = "child-archiver".to_string();
        child.digest =
            ArtifactHash::parse(format!("sha256:{}", "c".repeat(64))).expect("child digest");
        root.operations
            .get_mut(&invocation.operation)
            .expect("root operation")
            .dependencies
            .push(wamn_catalog::ComponentOperationDependency {
                package: child.package_id.clone(),
                version: "1.0.0".to_string(),
                digest: child.digest.to_string(),
                operation: invocation.operation.clone(),
            });
        invocation.package_id.clone_from(&child.package_id);
        invocation.component.clone_from(&child.component);
        invocation.component_digest = child.digest.to_string();
        snapshot.component = Some(child.component.clone());
        manifest
            .release
            .packages
            .insert(PackageCoordinate::new("package_b", "1.0.0").expect("child package"));
        manifest.components.extend([root.clone(), child]);
        assert_eq!(
            authorize_closure(&invocation, Some(&manifest), None, &snapshot),
            Ok(())
        );

        manifest.components.remove(&root);
        root.operations
            .get_mut(&invocation.origin.operation)
            .expect("root operation")
            .dependencies
            .clear();
        manifest.components.insert(root);
        assert_eq!(
            authorize_closure(&invocation, Some(&manifest), None, &snapshot),
            Err("the release closure does not carry this component and wiring"),
        );
    }

    #[test]
    fn a_candidate_cannot_retarget_its_frozen_origin() {
        let world = Arc::new(frozen_world());
        let binding = frozen_binding(&world);
        let mut invocation = candidate_with(Arc::clone(&world));
        let snapshot = matching_snapshot(binding);
        invocation.origin.operation = "other-operation".to_string();
        assert_eq!(
            authorize_closure(&invocation, None, Some(binding), &snapshot),
            Err("the candidate closure disagrees with the frozen wiring or binding"),
        );
    }

    /// And a manifest that carries neither still refuses, so the released arm
    /// applies the released rule rather than passing everything through.
    #[test]
    fn a_released_closure_the_manifest_does_not_carry_refuses() {
        let world = frozen_world();
        let snapshot = matching_snapshot(frozen_binding(&world));
        let manifest = manifest("tenant-a", "package_a");

        assert_eq!(
            authorize_closure(&released(), Some(&manifest), None, &snapshot),
            Err("the release closure does not carry this component and wiring"),
        );
    }

    /// A plugin that opens no connection, so a span assertion needs no database
    /// and no object store.
    fn offline_plugin() -> WamnBlobstore {
        let postgres = WamnPostgres::new(WamnPostgresConfig {
            credentials: None,
            guest_pool_max_size: 1,
            platform_pool_max_size: 1,
            wait_timeout_ms: 1,
            statement_timeout_ms: 1,
            row_limit: 1,
        })
        .expect("an offline postgres plugin does not open a connection");
        WamnBlobstore::new(
            Arc::new(postgres),
            Arc::new(WamnCredentials::empty()),
            "tenant-a",
            "project-a",
            None,
        )
    }

    /// The blobstore surface fills the SAME vocabulary the HTTP surface fills,
    /// so one object-store effect names the call that raised it by the same
    /// keys.
    #[test]
    fn a_blobstore_effect_span_names_the_invocation_it_was_raised_under() {
        let plugin = offline_plugin();
        let component_id = "component-store-3";
        plugin
            .bind_invocation(component_id, released())
            .expect("the fresh store accepts its invocation");
        let component_digest = released().component_digest;

        let harness = SpanHarness::install("blobstore-span-test");
        drop(blobstore_span(&plugin, component_id, "get-data"));

        assert_eq!(
            harness.attributes("wamn.blobstore"),
            expected_attributes(&[
                ("effect.operation", "get-data"),
                ("wamn.tenant", "tenant-a"),
                ("wamn.project", "project-a"),
                ("wamn.component", component_id),
                ("wamn.package_id", "package_a"),
                ("wamn.wiring_id", "orders"),
                ("wamn.wiring_version", "3"),
                ("wamn.node_id", "archive"),
                ("wamn.occurrence", "1"),
                ("wamn.component_digest", component_digest.as_str()),
                ("wamn.component_name", "archiver"),
                ("wamn.operation", "orders:archive/store@1.0.0"),
            ]),
        );
    }
}
