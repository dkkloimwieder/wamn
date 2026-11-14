//! Resolve workflow graphs against their frozen release and component identities.
use anyhow::Context as _;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use wamn_catalog::{AdmittedComponent, WiringDocument};
use wamn_postgres_capability::WamnPostgres;

/// An immutable wiring and its resolved catalog facts.
///
/// The workflow layer indexes `document` directly.
#[derive(Debug, Clone)]
pub struct ResolvedActiveWiring {
    pub version: u32,
    /// The release the wiring resolved under, as its manifest digest.
    pub manifest_digest: String,
    pub graph_hash: Arc<str>,
    pub package_version: String,
    pub document: WiringDocument,
    /// Complete admitted facts selected by each exact wiring node.
    pub node_components: Arc<BTreeMap<String, AdmittedComponent>>,
    pub components: Arc<[AdmittedComponent]>,
}

/// The facts a manual run row carries, which key its resolution.
#[derive(Debug, Clone, Copy)]
pub struct WorkflowRunKey<'a> {
    pub project_env_key: &'a str,
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
}

impl WorkflowResolutionError {
    fn new(type_: WorkflowResolutionErrorType, detail: impl Into<String>) -> Self {
        Self {
            type_,
            detail: detail.into(),
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

impl std::error::Error for WorkflowResolutionError {}

/// Resolve one workflow run over the admitted components of its loaded release: the stored
/// graph `document` under `stored_hash`, each node to the component of the
/// release whose descriptor carries the node's frozen digest, component,
/// interface version and operation.
///
/// The stored hash must be the run's hash, and the graph must hash to it. A
/// node resolves to a component of the application's packages first, then to
/// any package's. A workflow belongs to an application, not to a package
/// version (docs/architecture/execution.md#environment-workflows), so the
/// resolved wiring names none.
pub fn resolve_workflow_in_release(
    run: &WorkflowRunKey<'_>,
    components: &Arc<[AdmittedComponent]>,
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
        let serving: Vec<_> = components
            .iter()
            .filter(|component| {
                &component.component_digest == digest
                    && component.component == node.component
                    && component.interface_version == node.interface_version
                    && component.operations.contains_key(&node.operation)
            })
            .collect();
        let chosen = serving
            .iter()
            .find(|component| {
                run.application_packages
                    .contains(&component.target.package_id)
            })
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
        node_components.insert(node_id.clone(), (*chosen).clone());
    }
    Ok(ResolvedActiveWiring {
        version: run.version,
        manifest_digest: run.manifest_digest.to_owned(),
        graph_hash: std::sync::Arc::from(stored_hash),
        package_version: String::new(),
        document,
        node_components: std::sync::Arc::new(node_components),
        components: Arc::clone(components),
    })
}

/// Resolve the pinned graph under the release already loaded by this host.
pub async fn resolve_workflow(
    postgres: &WamnPostgres,
    project: &str,
    release: &wamn_engine::release_manifest::LoadedRelease,
    run: &WorkflowRunKey<'_>,
) -> anyhow::Result<ResolvedActiveWiring> {
    anyhow::ensure!(
        release.target().project_env_key == run.project_env_key
            && release.target().environment == run.environment
            && release.release().manifest_digest.as_str() == run.manifest_digest,
        "workflow run release differs from the loaded release"
    );
    let version = i32::try_from(run.version).context("workflow version exceeds int")?;
    let (graph_json, hash) = postgres
        .executor_read(project, run.project_env_key, async |connection| {
            wamn_run_state::admission::workflow_version(
                connection,
                run.project_env_key,
                run.environment,
                run.application,
                run.workflow_id,
                version,
            )
            .await?
            .ok_or_else(|| {
                WorkflowResolutionError::new(
                    WorkflowResolutionErrorType::NotFound,
                    "the pinned workflow version is not stored",
                )
                .into()
            })
        })
        .await?;
    let graph = crate::admission::parse_graph(&graph_json)?;
    Ok(resolve_workflow_in_release(
        run,
        release.components(),
        graph,
        &hash,
    )?)
}

#[cfg(test)]
mod tests {
    use wamn_catalog::{
        AdmittedComponentOperation, PackageCoordinate, ServingComponent, ServingRelease,
        WiringNode, WiringTerminal,
    };

    use wamn_catalog::{ComponentPackageCoordinate, ServingManifest};

    use super::*;

    const APPLICATION: &str = "orders";
    const ORDERS_GET: &str = "orders:order/get@1.0.0";
    const DIGEST: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
    const OTHER: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";
    const RELEASE: &str = "sha256:3333333333333333333333333333333333333333333333333333333333333333";

    fn manifest() -> ServingManifest {
        let fact = AdmittedComponent {
            target: ComponentPackageCoordinate {
                project_env_key: "t1".to_owned(),
                package_id: APPLICATION.to_owned(),
                package_version: "1.0.0".to_owned(),
            },
            component: "data".to_owned(),
            interface_version: "0.1".to_owned(),
            operations: BTreeMap::from([(
                ORDERS_GET.to_owned(),
                AdmittedComponentOperation {
                    connections: Vec::new(),
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
        )
        .expect("the fixture workflow is structurally valid")
    }

    fn run<'a>(hash: &'a str, node_digests: &'a BTreeMap<String, String>) -> WorkflowRunKey<'a> {
        static PACKAGES: std::sync::LazyLock<BTreeSet<String>> =
            std::sync::LazyLock::new(|| BTreeSet::from([APPLICATION.to_owned()]));
        WorkflowRunKey {
            project_env_key: "t1",
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
        let resolved = resolve_workflow_in_release(
            &run(&hash, &digests),
            &manifest().admitted_components("t1").unwrap().into(),
            document,
            &hash,
        )
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
                &manifest().admitted_components("t1").unwrap().into(),
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
            &manifest().admitted_components("t1").unwrap().into(),
            document.clone(),
            &hash,
        )
        .expect_err("a stored hash other than the run's refuses");
        assert_eq!(
            refusal.error_type(),
            WorkflowResolutionErrorType::HashMismatch
        );
        // The stored graph does not hash to its row.
        let refusal = resolve_workflow_in_release(
            &run(OTHER, &digests),
            &manifest().admitted_components("t1").unwrap().into(),
            document,
            OTHER,
        )
        .expect_err("a graph that does not hash to its row refuses");
        assert_eq!(
            refusal.error_type(),
            WorkflowResolutionErrorType::HashMismatch
        );
    }
}
