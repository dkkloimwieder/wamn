//! Author one immutable version of an environment workflow
//! (docs/plan/workflows.md R8, epic 1).
//!
//! A workflow is environment data: its graph is a [`WiringDocument`], and the
//! row lives in `catalog.workflows` of the environment's project database.
//! The graph is gated against the release the environment serves, not against
//! one package: each node resolves to the component of that release whose
//! descriptor carries the node's component, interface version and operation,
//! and [`validate_resolved_wiring_compatibility`] judges the graph over those
//! per-node facts. An operation node resolves only to a component of the
//! application. A platform node (an operation with no registered operation)
//! resolves to the component of any package of the release.
//!
//! The hash is bound exactly as [`crate::author_wiring`] binds it: the gate
//! produces it, and the control store must hold a green gate report under it.
//! An exact resubmission converges. Other bytes under the same version refuse.
//! A version is authored disabled; [`crate::workflow_lifecycle`] enables it.

use std::collections::BTreeMap;

use anyhow::Context as _;
use tokio_postgres::{Client, NoTls, Transaction};
use wamn_catalog::{
    AdmittedComponent, ComponentPackageScope, DefinitionHash, ServingManifest, WiringDocument,
    validate_resolved_wiring_compatibility,
};

use crate::author_wiring::{AuthorWiringError, read_gate_verdict, require_green_report};

const CLAIM_TENANT_SQL: &str = "SELECT set_config('app.tenant', $1, true)";

/// The one trigger this epic admits.
const MANUAL_TRIGGER: &str = "manual";

/// Params: `$1` tenant, `$2` environment, `$3` application, `$4` workflow id,
/// `$5` version, `$6` graph, `$7` hash, `$8` trigger, `$9` execution type,
/// `$10` execution role, `$11` authoring principal.
const INSERT_WORKFLOW_SQL: &str = "\
INSERT INTO catalog.workflows (\
       tenant_id, environment, application, workflow_id, version, graph_json, wiring_hash, \
       trigger, execution_type, execution_role, created_by\
     ) VALUES ($1, $2, $3, $4, $5, $6::text::jsonb, $7, $8, $9, $10, $11::text::uuid) \
ON CONFLICT DO NOTHING";

/// The stored version carries exactly this definition. The author is not part
/// of the definition, so a second author of the same bytes converges.
const EXACT_WORKFLOW_SQL: &str = "\
SELECT EXISTS (\
    SELECT 1 FROM catalog.workflows \
     WHERE tenant_id = $1 AND environment = $2 AND application = $3 \
       AND workflow_id = $4 AND version = $5 AND graph_json = $6::text::jsonb \
       AND wiring_hash = $7 AND trigger = $8 AND execution_type = $9 \
       AND execution_role IS NOT DISTINCT FROM $10 \
       AND $11::text <> ''\
    )";

const SELECT_RELEASE_SQL: &str = "\
SELECT canonical_bytes FROM catalog.releases \
 WHERE tenant_id = $1 AND manifest_digest = $2";

/// Stable prefix every workflow-authorship refusal renders with.
pub const WORKFLOW_AUTHORSHIP_REFUSAL: &str = "workflow-authorship-refused";

/// Stable predicate that refused one workflow authorship.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthorWorkflowErrorType {
    Storage,
    Document,
    /// The graph does not resolve against, or is not compatible with, the
    /// serving release.
    Gate,
    /// No GREEN gate report covers this document's own hash.
    Report,
    Conflict,
}

impl AuthorWorkflowErrorType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Storage => "storage",
            Self::Document => "document",
            Self::Gate => "gate",
            Self::Report => "report",
            Self::Conflict => "conflict",
        }
    }
}

/// Contextual refusal from the workflow-authorship boundary.
#[derive(Debug)]
pub struct AuthorWorkflowError {
    type_: AuthorWorkflowErrorType,
    detail: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl AuthorWorkflowError {
    fn new(type_: AuthorWorkflowErrorType, detail: impl Into<String>) -> Self {
        Self {
            type_,
            detail: detail.into(),
            source: None,
        }
    }

    fn with_source(
        type_: AuthorWorkflowErrorType,
        detail: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            type_,
            detail: detail.into(),
            source: Some(Box::new(source)),
        }
    }

    /// The gate-report refusal of [`crate::author_wiring`], under this
    /// boundary's prefix.
    fn report(error: AuthorWiringError) -> Self {
        let type_ = match error.error_type() {
            crate::author_wiring::AuthorWiringErrorType::Storage => {
                AuthorWorkflowErrorType::Storage
            }
            _ => AuthorWorkflowErrorType::Report,
        };
        Self::with_source(
            type_,
            "the control store holds no green gate report for this graph",
            error,
        )
    }

    /// Stable refusal class for callers that must not match display text.
    pub const fn error_type(&self) -> AuthorWorkflowErrorType {
        self.type_
    }
}

impl std::fmt::Display for AuthorWorkflowError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{WORKFLOW_AUTHORSHIP_REFUSAL} ({}): {}",
            self.type_.as_str(),
            self.detail
        )
    }
}

impl std::error::Error for AuthorWorkflowError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_deref().map(|source| source as _)
    }
}

/// The identity a run of the workflow executes as (docs/plan/workflows.md R3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkflowExecution<'a> {
    /// The `workflow` user, with this role active.
    Role(&'a str),
    /// The caller, with its active role.
    Invoker,
}

impl<'a> WorkflowExecution<'a> {
    /// The stored `execution_type`.
    pub const fn type_str(self) -> &'static str {
        match self {
            Self::Role(_) => "role",
            Self::Invoker => "invoker",
        }
    }

    /// The stored `execution_role`.
    pub const fn role(self) -> Option<&'a str> {
        match self {
            Self::Role(role) => Some(role),
            Self::Invoker => None,
        }
    }
}

/// One workflow version to author. The workflow id and the version are the
/// document's own `wiring-id` and `version`.
#[derive(Clone, Copy, Debug)]
pub struct AuthorWorkflowRequest<'a> {
    pub tenant_id: &'a str,
    pub environment: &'a str,
    /// The application package id of the release, whose operations the graph
    /// calls.
    pub application: &'a str,
    pub document: &'a WiringDocument,
    pub execution: WorkflowExecution<'a>,
    /// The authoring principal, as the text of its `platform."user"` id.
    pub created_by: &'a str,
    /// The manifest digest of the release the environment serves.
    pub manifest_digest: &'a str,
}

/// Resolve each node of `document` to the component of `manifest` that serves
/// it, as the facts [`validate_resolved_wiring_compatibility`] reads.
///
/// A node resolves to a component whose descriptor carries the node's
/// component, interface version and operation. An operation node (one whose
/// operation has a registered operation) resolves only within `application`.
/// A platform node resolves within the application first, then within any
/// package of the release; two different digests there refuse as ambiguous.
pub fn resolve_workflow_components(
    tenant_id: &str,
    application: &str,
    manifest: &ServingManifest,
    document: &WiringDocument,
) -> Result<BTreeMap<String, AdmittedComponent>, AuthorWorkflowError> {
    let versions: BTreeMap<&str, &str> = manifest
        .release
        .packages
        .iter()
        .map(|package| (package.package_id(), package.package_version()))
        .collect();
    if !versions.contains_key(application) {
        return Err(AuthorWorkflowError::new(
            AuthorWorkflowErrorType::Gate,
            format!("the serving release carries no application {application:?}"),
        ));
    }
    let mut resolved = BTreeMap::new();
    for (node_id, node) in &document.nodes {
        let serving: Vec<_> = manifest
            .components
            .iter()
            .filter(|component| {
                component.descriptor.component == node.component
                    && component.descriptor.interface_version == node.interface_version
                    && component
                        .descriptor
                        .operations
                        .contains_key(&node.operation)
            })
            .collect();
        let in_application: Vec<_> = serving
            .iter()
            .filter(|component| component.package_id == application)
            .collect();
        let chosen = match (in_application.as_slice(), serving.as_slice()) {
            ([component], _) => **component,
            ([], []) => {
                return Err(AuthorWorkflowError::new(
                    AuthorWorkflowErrorType::Gate,
                    format!(
                        "workflow node {node_id:?} names component {:?} interface {:?} \
                         operation {:?}, which the serving release does not carry",
                        node.component, node.interface_version, node.operation
                    ),
                ));
            }
            ([], [first, rest @ ..]) => {
                let operation = &first.descriptor.operations[&node.operation];
                if operation.registered_operation.is_some() {
                    return Err(AuthorWorkflowError::new(
                        AuthorWorkflowErrorType::Gate,
                        format!(
                            "workflow node {node_id:?} names operation {:?} of package {:?}, \
                             which application {application:?} does not serve",
                            node.operation, first.package_id
                        ),
                    ));
                }
                if rest.iter().any(|other| {
                    other.descriptor.component_digest != first.descriptor.component_digest
                }) {
                    return Err(ambiguous(node_id));
                }
                *first
            }
            _ => return Err(ambiguous(node_id)),
        };
        let scope = ComponentPackageScope {
            tenant_id: tenant_id.to_owned(),
            package_id: chosen.package_id.clone(),
            package_version: versions[chosen.package_id.as_str()].to_owned(),
        };
        resolved.insert(
            node_id.clone(),
            chosen.descriptor.clone().into_admitted(scope).component,
        );
    }
    Ok(resolved)
}

fn ambiguous(node_id: &str) -> AuthorWorkflowError {
    AuthorWorkflowError::new(
        AuthorWorkflowErrorType::Gate,
        format!("workflow node {node_id:?} resolves to more than one component of the release"),
    )
}

/// Gate `document` against the per-node facts of `manifest` and derive its
/// hash. The hash is produced only on the far side of the gate.
pub fn gate_workflow_document(
    tenant_id: &str,
    application: &str,
    manifest: &ServingManifest,
    document: &WiringDocument,
) -> Result<DefinitionHash, AuthorWorkflowError> {
    let components = resolve_workflow_components(tenant_id, application, manifest, document)?;
    validate_resolved_wiring_compatibility(document, &components).map_err(|error| {
        AuthorWorkflowError::with_source(
            AuthorWorkflowErrorType::Gate,
            format!(
                "workflow {:?} version {} is not compatible with the serving release",
                document.wiring_id, document.version
            ),
            error,
        )
    })?;
    Ok(document.wiring_hash())
}

/// Read the installed release `manifest_digest` and gate `document` against it.
pub(crate) async fn gate_against_release(
    transaction: &Transaction<'_>,
    tenant_id: &str,
    application: &str,
    manifest_digest: &str,
    document: &WiringDocument,
) -> Result<DefinitionHash, AuthorWorkflowError> {
    let bytes: Vec<u8> = transaction
        .query_opt(SELECT_RELEASE_SQL, &[&tenant_id, &manifest_digest])
        .await
        .map_err(|error| storage("read the serving release", error))?
        .ok_or_else(|| {
            AuthorWorkflowError::new(
                AuthorWorkflowErrorType::Gate,
                format!("release {manifest_digest} is not installed in this environment"),
            )
        })?
        .get(0);
    let (manifest, _) = ServingManifest::from_canonical_bytes(&bytes).map_err(|error| {
        AuthorWorkflowError::with_source(
            AuthorWorkflowErrorType::Storage,
            format!("release {manifest_digest} holds no valid manifest"),
            error,
        )
    })?;
    gate_workflow_document(tenant_id, application, &manifest, document)
}

/// Gate and append one disabled workflow version in the caller's transaction.
///
/// `control` is the CONTROL-database connection the gate report is read on,
/// as for [`crate::author_wiring::author_wiring`].
pub async fn author_workflow(
    control: &Client,
    transaction: &Transaction<'_>,
    request: &AuthorWorkflowRequest<'_>,
) -> Result<DefinitionHash, AuthorWorkflowError> {
    transaction
        .query_one(CLAIM_TENANT_SQL, &[&request.tenant_id])
        .await
        .map_err(|error| storage("claim the authoring tenant", error))?;
    let document = request.document;
    let hash = gate_against_release(
        transaction,
        request.tenant_id,
        request.application,
        request.manifest_digest,
        document,
    )
    .await?;
    let verdict = read_gate_verdict(control, request.tenant_id, &hash)
        .await
        .map_err(AuthorWorkflowError::report)?;
    require_green_report(verdict, document, &hash).map_err(AuthorWorkflowError::report)?;

    let version = i32::try_from(document.version).map_err(|error| {
        AuthorWorkflowError::with_source(
            AuthorWorkflowErrorType::Document,
            format!(
                "workflow {:?} version {} exceeds the catalog storage width",
                document.wiring_id, document.version
            ),
            error,
        )
    })?;
    let graph_json = serde_json::to_string(document).expect("a wiring document serializes");
    let stored_hash = hash.as_str();
    let execution_type = request.execution.type_str();
    let execution_role = request.execution.role();
    let parameters: [&(dyn tokio_postgres::types::ToSql + Sync); 11] = [
        &request.tenant_id,
        &request.environment,
        &request.application,
        &document.wiring_id,
        &version,
        &graph_json,
        &stored_hash,
        &MANUAL_TRIGGER,
        &execution_type,
        &execution_role,
        &request.created_by,
    ];
    transaction
        .execute(INSERT_WORKFLOW_SQL, &parameters)
        .await
        .map_err(|error| storage("append the workflow version", error))?;
    let exact: bool = transaction
        .query_one(EXACT_WORKFLOW_SQL, &parameters)
        .await
        .map_err(|error| storage("verify the stored workflow version", error))?
        .get(0);
    if !exact {
        return Err(AuthorWorkflowError::new(
            AuthorWorkflowErrorType::Conflict,
            format!(
                "workflow {:?} version {} is already authored with another definition",
                document.wiring_id, document.version
            ),
        ));
    }
    Ok(hash)
}

/// Author one workflow version over its own project and control connections.
pub async fn author_workflow_in_databases(
    database_url: &str,
    control_database_url: &str,
    request: &AuthorWorkflowRequest<'_>,
) -> anyhow::Result<DefinitionHash> {
    let (control, control_connection) = tokio_postgres::connect(control_database_url, NoTls)
        .await
        .context("connect to the control store holding the gate reports")?;
    let control_task = tokio::spawn(control_connection);
    let (mut client, connection) = match tokio_postgres::connect(database_url, NoTls).await {
        Ok(opened) => opened,
        Err(error) => {
            control_task.abort();
            return Err(error).context("connect to the project database");
        }
    };
    let connection_task = tokio::spawn(connection);
    let authored = async {
        let transaction = client
            .transaction()
            .await
            .context("begin the workflow authorship")?;
        let hash = author_workflow(&control, &transaction, request).await?;
        transaction
            .commit()
            .await
            .context("commit the workflow authorship")?;
        anyhow::Ok(hash)
    }
    .await;
    connection_task.abort();
    control_task.abort();
    authored
}

fn storage(context: &'static str, error: tokio_postgres::Error) -> AuthorWorkflowError {
    AuthorWorkflowError::with_source(AuthorWorkflowErrorType::Storage, context, error)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use wamn_catalog::{
        AdmittedComponentOperation, PackageCoordinate, ServingComponent, ServingRelease,
        WiringNode, WiringTerminal,
    };

    use super::*;

    const APPLICATION: &str = "orders";
    const ORDERS_GET: &str = "orders:order/get@1.0.0";
    const PALETTE_GET: &str = "palette:data/get@2.0.0";

    fn operation(op: &str, registered: bool) -> AdmittedComponentOperation {
        AdmittedComponentOperation {
            committed_result_schema: None,
            registered_operation: registered.then(|| op.to_owned()),
            host_transaction: false,
            input_ports: Vec::new(),
            output_ports: Vec::new(),
            parameters: Vec::new(),
            statements: BTreeMap::new(),
        }
    }

    fn component(package: &str, name: &str, op: &str, registered: bool) -> ServingComponent {
        let fact = AdmittedComponent {
            scope: ComponentPackageScope {
                tenant_id: "t1".to_owned(),
                package_id: package.to_owned(),
                package_version: "1.0.0".to_owned(),
            },
            component: name.to_owned(),
            interface_version: "0.1".to_owned(),
            operations: BTreeMap::from([(op.to_owned(), operation(op, registered))]),
            component_digest: format!("sha256:{}", "1".repeat(64)),
            imports: Vec::new(),
            imports_fingerprint: format!("sha256:{}", "6".repeat(64)),
            effects: Vec::new(),
        };
        ServingComponent::project(&fact, Vec::new()).expect("the fixture component projects")
    }

    fn manifest(components: Vec<ServingComponent>) -> ServingManifest {
        ServingManifest::new(
            ServingRelease {
                packages: BTreeSet::from([
                    PackageCoordinate::new(APPLICATION, "1.0.0").unwrap(),
                    PackageCoordinate::new("palette", "2.0.0").unwrap(),
                ]),
            },
            components.into_iter().collect(),
            BTreeSet::new(),
            BTreeSet::new(),
            BTreeMap::new(),
            BTreeMap::new(),
        )
        .expect("the fixture manifest is valid")
    }

    fn document(name: &str, op: &str) -> WiringDocument {
        WiringDocument::new(
            "nightly",
            1,
            "node",
            BTreeMap::from([(
                "node".to_owned(),
                WiringNode {
                    component: name.to_owned(),
                    interface_version: "0.1".to_owned(),
                    operation: op.to_owned(),
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

    #[test]
    fn an_application_operation_gates_with_its_scope() {
        let manifest = manifest(vec![component(APPLICATION, "data", ORDERS_GET, true)]);
        let document = document("data", ORDERS_GET);
        let resolved =
            resolve_workflow_components("t1", APPLICATION, &manifest, &document).unwrap();
        assert_eq!(resolved["node"].scope.package_version, "1.0.0");
        assert_eq!(
            gate_workflow_document("t1", APPLICATION, &manifest, &document).unwrap(),
            document.wiring_hash()
        );
    }

    #[test]
    fn a_platform_node_resolves_in_another_package() {
        let manifest = manifest(vec![component("palette", "jsonata", "map", false)]);
        let resolved =
            resolve_workflow_components("t1", APPLICATION, &manifest, &document("jsonata", "map"))
                .unwrap();
        assert_eq!(resolved["node"].scope.package_id, "palette");
    }

    #[test]
    fn an_operation_of_another_package_refuses() {
        let manifest = manifest(vec![component("palette", "data", PALETTE_GET, true)]);
        let refusal = resolve_workflow_components(
            "t1",
            APPLICATION,
            &manifest,
            &document("data", PALETTE_GET),
        )
        .unwrap_err();
        assert_eq!(refusal.error_type(), AuthorWorkflowErrorType::Gate);
        assert!(format!("{refusal}").starts_with(WORKFLOW_AUTHORSHIP_REFUSAL));
    }

    #[test]
    fn a_node_the_release_does_not_carry_refuses() {
        let manifest = manifest(vec![component(APPLICATION, "data", ORDERS_GET, true)]);
        for document in [document("data", "put"), document("other", ORDERS_GET)] {
            let refusal = gate_workflow_document("t1", APPLICATION, &manifest, &document)
                .expect_err("a node absent from the release refuses");
            assert_eq!(refusal.error_type(), AuthorWorkflowErrorType::Gate);
        }
        let refusal =
            gate_workflow_document("t1", "absent", &manifest, &document("data", ORDERS_GET))
                .expect_err("an application absent from the release refuses");
        assert_eq!(refusal.error_type(), AuthorWorkflowErrorType::Gate);
    }

    #[test]
    fn the_execution_maps_to_its_stored_columns() {
        assert_eq!(WorkflowExecution::Role("clerk").type_str(), "role");
        assert_eq!(WorkflowExecution::Role("clerk").role(), Some("clerk"));
        assert_eq!(WorkflowExecution::Invoker.type_str(), "invoker");
        assert_eq!(WorkflowExecution::Invoker.role(), None);
    }
}
