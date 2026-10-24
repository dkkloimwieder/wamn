//! Environment workflow admission reads and run resolution
//! (docs/plan/workflows.md R2, R3, R8, epic 1).
//!
//! A workflow is a row of `catalog.workflows`, not a member of the release.
//! The host reads the enabled version of a workflow when `workflow.start`
//! admits a run, and the queue resolves a claimed run from its row: the graph
//! from `catalog.workflows` under the run's workflow id and version, and each
//! node from the component digest admission froze on the run, checked against
//! the descriptors of the release the run pins. Nodes come from that release
//! only in this epic.
//!
//! A workflow run resolves on every claim. The router's wiring cache is keyed
//! by a released wiring of a package, and a workflow is not one, so it is not
//! used here.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Context as _;
use wamn_catalog::{
    ComponentPackageScope, ServingManifest, WiringDocument, validate_resolved_wiring_compatibility,
};
use wamn_run_state::AuthorityClass;

use super::WamnPostgres;
use super::wiring_resolution::{DecodedWiring, ResolvedActiveWiring, resolved_wiring};

/// The enabled version of one manual workflow. A schedule workflow is not
/// started through `workflow.start`. Binds: `$1` tenant, `$2` environment,
/// `$3` application, `$4` workflow id.
const ENABLED_WORKFLOW_SQL: &str = "\
SELECT version, graph_json::text, wiring_hash, execution_type, execution_role \
  FROM catalog.workflows \
 WHERE tenant_id = $1 AND environment = $2 AND application = $3 \
   AND workflow_id = $4 AND enabled AND trigger = 'manual'";

/// One workflow version, enabled or not: a run finishes under the version it
/// pinned. Binds: `$1` tenant, `$2` environment, `$3` application, `$4`
/// workflow id, `$5` version.
const WORKFLOW_VERSION_SQL: &str = "\
SELECT graph_json::text, wiring_hash \
  FROM catalog.workflows \
 WHERE tenant_id = $1 AND environment = $2 AND application = $3 \
   AND workflow_id = $4 AND version = $5";

/// The verified manifest bytes of the release a run pins. Binds: `$1`
/// tenant, `$2` manifest digest.
pub(super) const PINNED_RELEASE_SQL: &str = "\
SELECT canonical_bytes FROM catalog.releases \
 WHERE tenant_id = $1 AND manifest_digest = $2";

/// The enabled version of one workflow, as the host admits a run of it.
#[derive(Debug, Clone)]
pub struct EnabledWorkflow {
    pub version: i32,
    pub graph: WiringDocument,
    pub wiring_hash: String,
    /// `role` or `invoker`.
    pub execution_type: String,
    /// The active role of a `role` workflow.
    pub execution_role: Option<String>,
}

/// The facts a manual run row carries, which key its resolution.
#[derive(Debug, Clone, Copy)]
pub struct WorkflowRunKey<'a> {
    pub tenant_id: &'a str,
    pub environment: &'a str,
    /// The application, by the name the environment declares: the run's
    /// package id.
    pub application: &'a str,
    /// The packages the environment declares for `application`.
    pub application_packages: &'a BTreeSet<String>,
    pub workflow_id: &'a str,
    pub version: u32,
    /// The release the run pins.
    pub manifest_digest: &'a str,
    pub wiring_hash: &'a str,
    /// The component digest admission froze for each node.
    pub node_digests: &'a BTreeMap<String, String>,
}

/// Stable predicate that refused one workflow resolution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkflowResolutionErrorType {
    /// No version of the workflow, or no pinned release, is stored.
    NotFound,
    /// The stored graph does not hash to the run's hash.
    HashMismatch,
    /// A node has no frozen digest, or its digest is not a component of the
    /// pinned release.
    NodeNotInRelease,
    /// The graph does not judge compatible over its resolved nodes.
    Incompatible,
    /// The stored graph or release is not valid.
    Definition,
}

/// Contextual refusal of one workflow resolution.
#[derive(Debug)]
pub struct WorkflowResolutionError {
    type_: WorkflowResolutionErrorType,
    detail: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl WorkflowResolutionError {
    fn new(type_: WorkflowResolutionErrorType, detail: impl Into<String>) -> Self {
        Self {
            type_,
            detail: detail.into(),
            source: None,
        }
    }

    fn with_source(
        type_: WorkflowResolutionErrorType,
        detail: impl Into<String>,
        source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Self {
        Self {
            type_,
            detail: detail.into(),
            source: Some(source.into()),
        }
    }

    /// Stable refusal class for callers that must not match display text.
    pub const fn error_type(&self) -> WorkflowResolutionErrorType {
        self.type_
    }
}

impl std::fmt::Display for WorkflowResolutionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "workflow-resolution-refused: {}", self.detail)
    }
}

impl std::error::Error for WorkflowResolutionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_deref().map(|source| source as _)
    }
}

/// Resolve one workflow run over `manifest`, the release it pins: the stored
/// graph `document` under `stored_hash`, each node to the component of the
/// release whose descriptor carries the node's frozen digest, component,
/// interface version and operation.
///
/// The stored hash must be the run's hash, and the graph must hash to it. A
/// node resolves to a component of the application's packages first, then to
/// any package's. A workflow belongs to an application, not to a package
/// version (docs/plan/workflows.md R8), so the resolved wiring names none.
pub fn resolve_workflow_in_release(
    run: &WorkflowRunKey<'_>,
    manifest: &ServingManifest,
    document: WiringDocument,
    stored_hash: &str,
) -> Result<ResolvedActiveWiring, WorkflowResolutionError> {
    if stored_hash != run.wiring_hash || document.wiring_hash().as_str() != stored_hash {
        return Err(WorkflowResolutionError::new(
            WorkflowResolutionErrorType::HashMismatch,
            format!(
                "workflow {:?} version {} stores hash {stored_hash}, and the run pins {}",
                run.workflow_id, run.version, run.wiring_hash
            ),
        ));
    }
    if document.wiring_id != run.workflow_id || document.version != run.version {
        return Err(WorkflowResolutionError::new(
            WorkflowResolutionErrorType::Definition,
            format!(
                "the stored graph names workflow {:?} version {}",
                document.wiring_id, document.version
            ),
        ));
    }
    let versions: BTreeMap<&str, &str> = manifest
        .release
        .packages
        .iter()
        .map(|package| (package.package_id(), package.package_version()))
        .collect();
    let admitted = |component: &wamn_catalog::ServingComponent| {
        component
            .descriptor
            .clone()
            .into_admitted(ComponentPackageScope {
                tenant_id: run.tenant_id.to_owned(),
                package_id: component.package_id.clone(),
                package_version: versions
                    .get(component.package_id.as_str())
                    .copied()
                    .unwrap_or_default()
                    .to_owned(),
            })
            .component
    };
    if let Some(extra) = run
        .node_digests
        .keys()
        .find(|node_id| !document.nodes.contains_key(*node_id))
    {
        return Err(WorkflowResolutionError::new(
            WorkflowResolutionErrorType::NodeNotInRelease,
            format!("the run froze a digest for node {extra:?}, which the graph does not name"),
        ));
    }
    let mut node_components = BTreeMap::new();
    for (node_id, node) in &document.nodes {
        let digest = run.node_digests.get(node_id).ok_or_else(|| {
            WorkflowResolutionError::new(
                WorkflowResolutionErrorType::NodeNotInRelease,
                format!("the run froze no digest for node {node_id:?}"),
            )
        })?;
        let serving: Vec<_> = manifest
            .components
            .iter()
            .filter(|component| {
                &component.descriptor.component_digest == digest
                    && component.descriptor.component == node.component
                    && component.descriptor.interface_version == node.interface_version
                    && component
                        .descriptor
                        .operations
                        .contains_key(&node.operation)
            })
            .collect();
        let chosen = serving
            .iter()
            .find(|component| run.application_packages.contains(&component.package_id))
            .or_else(|| serving.first())
            .ok_or_else(|| {
                WorkflowResolutionError::new(
                    WorkflowResolutionErrorType::NodeNotInRelease,
                    format!(
                        "node {node_id:?} froze digest {digest}, which is not a component of \
                         release {} serving operation {:?}",
                        run.manifest_digest, node.operation
                    ),
                )
            })?;
        node_components.insert(node_id.clone(), admitted(chosen));
    }
    validate_resolved_wiring_compatibility(&document, &node_components).map_err(|error| {
        WorkflowResolutionError::with_source(
            WorkflowResolutionErrorType::Incompatible,
            format!(
                "workflow {:?} version {} is not compatible with its resolved nodes",
                run.workflow_id, run.version
            ),
            error,
        )
    })?;
    let components = manifest.components.iter().map(admitted).collect();
    resolved_wiring(
        DecodedWiring {
            version: run.version,
            package_version: String::new(),
            graph_hash: stored_hash.to_owned(),
            document,
        },
        run.manifest_digest,
        node_components,
        components,
    )
    .map_err(|error| {
        WorkflowResolutionError::with_source(
            WorkflowResolutionErrorType::Definition,
            "the resolved workflow is not executable",
            error,
        )
    })
}

pub(super) fn parse_graph(graph_json: &str) -> anyhow::Result<WiringDocument> {
    let value = serde_json::from_str(graph_json).context("parse the stored workflow graph")?;
    WiringDocument::parse(&value).context("validate the stored workflow graph")
}

impl WamnPostgres {
    /// The enabled version of manual workflow `workflow_id` of `application`
    /// in `environment`, or none.
    pub async fn enabled_workflow(
        &self,
        project: &str,
        tenant_id: &str,
        environment: &str,
        application: &str,
        workflow_id: &str,
    ) -> anyhow::Result<Option<EnabledWorkflow>> {
        self.executor_read(project, tenant_id, async |connection| {
            let Some(row) = connection
                .query_opt(
                    ENABLED_WORKFLOW_SQL,
                    &[&tenant_id, &environment, &application, &workflow_id],
                )
                .await
                .context("read the enabled workflow version")?
            else {
                return Ok(None);
            };
            let graph_json: String = row.try_get(1)?;
            Ok(Some(EnabledWorkflow {
                version: row.try_get(0)?,
                graph: parse_graph(&graph_json)?,
                wiring_hash: row.try_get(2)?,
                execution_type: row.try_get(3)?,
                execution_role: row.try_get(4)?,
            }))
        })
        .await
    }

    /// Resolve one claimed workflow run from its row
    /// ([`resolve_workflow_in_release`]).
    pub async fn resolve_workflow(
        &self,
        project: &str,
        run: &WorkflowRunKey<'_>,
    ) -> anyhow::Result<ResolvedActiveWiring> {
        let version = i32::try_from(run.version).context("workflow version exceeds int")?;
        let local = self.local_application.as_ref();
        let (graph_json, stored_hash, manifest) = self
            .executor_read(project, run.tenant_id, async |connection| {
                let row = connection
                    .query_opt(
                        WORKFLOW_VERSION_SQL,
                        &[
                            &run.tenant_id,
                            &run.environment,
                            &run.application,
                            &run.workflow_id,
                            &version,
                        ],
                    )
                    .await
                    .context("read the workflow version")?
                    .ok_or_else(|| {
                        WorkflowResolutionError::new(
                            WorkflowResolutionErrorType::NotFound,
                            format!(
                                "workflow {:?} version {} is not stored",
                                run.workflow_id, run.version
                            ),
                        )
                    })?;
                let graph_json: String = row.try_get(0)?;
                let stored_hash: String = row.try_get(1)?;
                let manifest = if let Some(local) = local {
                    local.require_instance(connection).await?;
                    anyhow::ensure!(
                        local.scope.tenant_id == run.tenant_id
                            && local.scope.environment == run.environment
                            && local.facts.manifest_digest.as_str() == run.manifest_digest,
                        "local release scope mismatch"
                    );
                    local.manifest.clone()
                } else {
                    let bytes: Vec<u8> = connection
                        .query_opt(PINNED_RELEASE_SQL, &[&run.tenant_id, &run.manifest_digest])
                        .await
                        .context("read the pinned release")?
                        .ok_or_else(|| {
                            WorkflowResolutionError::new(
                                WorkflowResolutionErrorType::NotFound,
                                format!("release {} is not installed", run.manifest_digest),
                            )
                        })?
                        .try_get(0)?;
                    let (manifest, digest) = ServingManifest::from_canonical_bytes(&bytes)
                        .context("decode the pinned release")?;
                    anyhow::ensure!(
                        digest.as_str() == run.manifest_digest,
                        "the pinned release bytes do not hash to the run's digest"
                    );
                    manifest
                };
                Ok((graph_json, stored_hash, manifest))
            })
            .await?;
        let document = parse_graph(&graph_json)?;
        Ok(resolve_workflow_in_release(
            run,
            &manifest,
            document,
            &stored_hash,
        )?)
    }

    /// Run `read` in one read transaction under the executor credential.
    async fn executor_read<T>(
        &self,
        project: &str,
        tenant_id: &str,
        read: impl AsyncFnOnce(&tokio_postgres::Client) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let (connection, policy) = self
            .checkout_platform(project, AuthorityClass::ExecutorPlatform)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        if let Err(error) = self
            .begin_with_claims(
                &connection,
                AuthorityClass::ExecutorPlatform,
                true,
                tenant_id,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                policy.statement_timeout_ms,
            )
            .await
        {
            self.destroy(connection);
            return Err(anyhow::anyhow!(error.to_string()));
        }
        let result = read(&**connection).await;
        let end = if result.is_ok() { "COMMIT" } else { "ROLLBACK" };
        if connection.batch_execute(end).await.is_err() {
            self.destroy(connection);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use wamn_catalog::{
        AdmittedComponentOperation, PackageCoordinate, ServingComponent, ServingRelease,
        WiringNode, WiringTerminal,
    };

    use wamn_catalog::AdmittedComponent;

    use super::*;

    const APPLICATION: &str = "orders";
    const ORDERS_GET: &str = "orders:order/get@1.0.0";
    const DIGEST: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
    const OTHER: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";
    const RELEASE: &str = "sha256:3333333333333333333333333333333333333333333333333333333333333333";

    fn manifest() -> ServingManifest {
        let fact = AdmittedComponent {
            scope: ComponentPackageScope {
                tenant_id: "t1".to_owned(),
                package_id: APPLICATION.to_owned(),
                package_version: "1.0.0".to_owned(),
            },
            component: "data".to_owned(),
            interface_version: "0.1".to_owned(),
            operations: BTreeMap::from([(
                ORDERS_GET.to_owned(),
                AdmittedComponentOperation {
                    committed_result_schema: None,
                    registered_operation: Some(ORDERS_GET.to_owned()),
                    host_transaction: false,
                    input_ports: Vec::new(),
                    output_ports: Vec::new(),
                    parameters: Vec::new(),
                    statements: BTreeMap::new(),
                },
            )]),
            component_digest: DIGEST.to_owned(),
            imports: Vec::new(),
            imports_fingerprint: format!("sha256:{}", "6".repeat(64)),
            effects: Vec::new(),
        };
        ServingManifest::new(
            ServingRelease {
                packages: BTreeSet::from([PackageCoordinate::new(APPLICATION, "1.0.0").unwrap()]),
            },
            BTreeSet::from([ServingComponent::project(&fact, Vec::new()).unwrap()]),
            BTreeSet::new(),
            BTreeSet::new(),
            BTreeMap::new(),
            BTreeMap::new(),
        )
        .expect("the fixture manifest is valid")
    }

    fn document() -> WiringDocument {
        WiringDocument::new(
            "nightly",
            1,
            "node",
            BTreeMap::from([(
                "node".to_owned(),
                WiringNode {
                    component: "data".to_owned(),
                    interface_version: "0.1".to_owned(),
                    operation: ORDERS_GET.to_owned(),
                    operation_dependency: None,
                    params: BTreeMap::new(),
                    terminal: Some(WiringTerminal::Respond),
                },
            )]),
            Vec::new(),
            Vec::new(),
        )
        .expect("the fixture workflow is structurally valid")
    }

    fn run<'a>(hash: &'a str, node_digests: &'a BTreeMap<String, String>) -> WorkflowRunKey<'a> {
        static PACKAGES: std::sync::LazyLock<BTreeSet<String>> =
            std::sync::LazyLock::new(|| BTreeSet::from([APPLICATION.to_owned()]));
        WorkflowRunKey {
            tenant_id: "t1",
            environment: "dev",
            application: "erp",
            application_packages: &PACKAGES,
            workflow_id: "nightly",
            version: 1,
            manifest_digest: RELEASE,
            wiring_hash: hash,
            node_digests,
        }
    }

    #[test]
    fn a_run_resolves_each_node_to_its_frozen_digest() {
        let document = document();
        let hash = document.wiring_hash().as_str().to_owned();
        let digests = BTreeMap::from([("node".to_owned(), DIGEST.to_owned())]);
        let resolved =
            resolve_workflow_in_release(&run(&hash, &digests), &manifest(), document, &hash)
                .expect("the frozen digest is a component of the release");
        assert_eq!(resolved.node_components["node"].component_digest, DIGEST);
        assert_eq!(resolved.manifest_digest, RELEASE);
        assert_eq!(resolved.package_version, "");
        assert_eq!(resolved.graph_hash.as_ref(), hash);
    }

    #[test]
    fn a_node_digest_the_release_does_not_carry_refuses() {
        let document = document();
        let hash = document.wiring_hash().as_str().to_owned();
        for digests in [
            BTreeMap::from([("node".to_owned(), OTHER.to_owned())]),
            BTreeMap::new(),
            BTreeMap::from([
                ("node".to_owned(), DIGEST.to_owned()),
                ("extra".to_owned(), DIGEST.to_owned()),
            ]),
        ] {
            let refusal = resolve_workflow_in_release(
                &run(&hash, &digests),
                &manifest(),
                document.clone(),
                &hash,
            )
            .expect_err("a digest outside the release refuses");
            assert_eq!(
                refusal.error_type(),
                WorkflowResolutionErrorType::NodeNotInRelease
            );
        }
    }

    #[test]
    fn a_hash_mismatch_refuses() {
        let document = document();
        let hash = document.wiring_hash().as_str().to_owned();
        let digests = BTreeMap::from([("node".to_owned(), DIGEST.to_owned())]);
        // The stored row carries another hash than the run.
        let refusal = resolve_workflow_in_release(
            &run(OTHER, &digests),
            &manifest(),
            document.clone(),
            &hash,
        )
        .expect_err("a stored hash other than the run's refuses");
        assert_eq!(
            refusal.error_type(),
            WorkflowResolutionErrorType::HashMismatch
        );
        // The stored graph does not hash to its row.
        let refusal =
            resolve_workflow_in_release(&run(OTHER, &digests), &manifest(), document, OTHER)
                .expect_err("a graph that does not hash to its row refuses");
        assert_eq!(
            refusal.error_type(),
            WorkflowResolutionErrorType::HashMismatch
        );
    }
}
