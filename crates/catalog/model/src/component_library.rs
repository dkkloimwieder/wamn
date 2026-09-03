//! Pure component-library facts stored after byte admission.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Component as PathComponent, Path};

use boon::{Compiler, Draft, Schemas};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use crate::package::validate_canonical_operation_for_package;
use crate::{CatalogIdentityError, PackageCoordinate, validate_text};

const JSON_SCHEMA_2020_12: &str = "https://json-schema.org/draft/2020-12/schema";
const SCHEMA_URI: &str = "mem://wamn-component-schema.json";

/// Package coordinate that owns an admitted component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ComponentPackageScope {
    pub tenant_id: String,
    pub package_id: String,
    pub package_version: String,
}

impl ComponentPackageScope {
    pub fn new(
        tenant_id: impl Into<String>,
        package_id: impl Into<String>,
        package_version: impl Into<String>,
    ) -> Result<Self, CatalogIdentityError> {
        let tenant_id = tenant_id.into();
        let coordinate = PackageCoordinate::new(package_id, package_version)?;
        validate_text(&tenant_id, "tenant-id")?;
        Ok(Self {
            tenant_id,
            package_id: coordinate.package_id().to_owned(),
            package_version: coordinate.package_version().to_owned(),
        })
    }
}

impl<'de> Deserialize<'de> for ComponentPackageScope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "kebab-case", deny_unknown_fields)]
        struct Wire {
            tenant_id: String,
            package_id: String,
            package_version: String,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.tenant_id, wire.package_id, wire.package_version)
            .map_err(serde::de::Error::custom)
    }
}

/// One author-declared input or output port before admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ComponentPortDeclaration {
    pub name: String,
    pub schema: Value,
}

/// One author-declared component parameter before admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ComponentParameterDeclaration {
    pub name: String,
    pub schema: Value,
    pub required: bool,
}

/// The closed set of connection types an admitted component may require.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ComponentConnectionType {
    Http,
    Blobstore,
}

/// Every connection type. A new variant does not compile until it is listed.
const CONNECTION_TYPES: [ComponentConnectionType; 2] = [
    ComponentConnectionType::Http,
    ComponentConnectionType::Blobstore,
];

impl ComponentConnectionType {
    /// Exact WIT `namespace:package` whose import this connection type needs.
    pub fn import_package(self) -> &'static str {
        match self {
            Self::Http => "wamn:connection",
            Self::Blobstore => "wasmcloud:blobstore",
        }
    }
}

/// One portable connection the component reaches under an author-chosen alias.
///
/// The alias is the author-owned half: it is the `requirement` string the guest
/// names at the `wamn:connection` boundary, and the coordinate an environment
/// binds to a concrete instance. Connection SEMANTICS are platform-owned and
/// selected whole by `requirement-type`, never authored field by field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ComponentConnection {
    pub store_alias: String,
    pub requirement_type: ComponentConnectionType,
}

/// One exact cross-package operation imported by component bytes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ComponentOperationDependency {
    pub package: String,
    pub version: String,
    pub digest: String,
    pub operation: String,
}

/// One operation exported by component bytes before admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ComponentOperationDeclaration {
    /// Explicit application permission identity. Palette operations carry none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registered_operation: Option<String>,
    /// Closed exact operation imports assigned to this exported operation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<ComponentOperationDependency>,
    pub input_ports: Vec<ComponentPortDeclaration>,
    pub output_ports: Vec<ComponentPortDeclaration>,
    pub parameters: Vec<ComponentParameterDeclaration>,
}

/// Component-owned facts presented with exact component bytes for admission.
///
/// Each map key is the exact exported `wamn:node/handler` instance name. A
/// package operation repeats that key in `registered-operation`; palette
/// operations leave the authorization identity absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ComponentDeclaration {
    pub scope: ComponentPackageScope,
    pub component: String,
    pub interface_version: String,
    pub operations: BTreeMap<String, ComponentOperationDeclaration>,
    pub connections: Vec<ComponentConnection>,
}

/// Canonical JSON Schema document and its exact RFC 8785 digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ComponentSchema {
    pub schema: Value,
    pub schema_digest: String,
}

/// One normalized admitted input or output port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct AdmittedComponentPort {
    pub name: String,
    pub schema: ComponentSchema,
}

/// One normalized admitted component parameter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct AdmittedComponentParameter {
    pub name: String,
    pub schema: ComponentSchema,
    pub required: bool,
}

/// One authority leaving the host, proved by the component's audited imports.
///
/// Effects are a projection of `imports`, never a second declaration: an author
/// cannot claim fewer effects than the bytes import, and an empty list is the
/// positive statement that the occurrence is pure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct AdmittedComponentEffect {
    pub package: String,
    pub interfaces: Vec<String>,
}

/// Closed PostgreSQL value vocabulary carried by one admitted SQL statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentSqlValueType {
    Boolean,
    Int32,
    Int64,
    Float64,
    Text,
    Bytes,
    Numeric,
    Timestamptz,
    Json,
    Uuid,
}

/// One ordered bind or result column in an admitted SQL statement.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ComponentSqlField {
    pub name: String,
    #[serde(rename = "type")]
    pub value_type: ComponentSqlValueType,
    pub nullable: bool,
}

/// Exact SQL bytes and typed shape admitted under their SHA-256 map key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ComponentSqlStatement {
    pub name: String,
    pub path: String,
    pub sql: String,
    pub binds: Vec<ComponentSqlField>,
    pub columns: Vec<ComponentSqlField>,
}

/// One normalized operation exported by an admitted component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct AdmittedComponentOperation {
    /// Explicit application permission identity. Never inferred from the key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registered_operation: Option<String>,
    /// Imports assigned to this export and byte-verified in the component inventory.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<ComponentOperationDependency>,
    pub input_ports: Vec<AdmittedComponentPort>,
    pub output_ports: Vec<AdmittedComponentPort>,
    pub parameters: Vec<AdmittedComponentParameter>,
    /// Exact SQL available while this export is active, keyed by raw-byte digest.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub statements: BTreeMap<String, ComponentSqlStatement>,
}

impl AdmittedComponentOperation {
    /// Resolve one statement only inside this operation's admitted authority.
    pub fn statement(&self, digest: &str) -> Option<&ComponentSqlStatement> {
        self.statements.get(digest)
    }
}

/// Complete package-owned fact persisted in `catalog.component_library`.
///
/// Environment is deliberately absent: an environment selects a wiring, while
/// component admission is owned by an exact package version. `admitted_at` is likewise
/// a storage timestamp, not validator output, so admission stays clock-free.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct AdmittedComponent {
    pub scope: ComponentPackageScope,
    pub component: String,
    pub interface_version: String,
    pub operations: BTreeMap<String, AdmittedComponentOperation>,
    pub component_digest: String,
    pub imports: Vec<String>,
    pub imports_fingerprint: String,
    pub effects: Vec<AdmittedComponentEffect>,
}

impl AdmittedComponent {
    /// Resolve one exact operation export without inferring authorization.
    pub fn operation(&self, operation: &str) -> Option<&AdmittedComponentOperation> {
        self.operations.get(operation)
    }
}

/// Everything one byte-verified component admission mints.
///
/// The two halves land in two relations — the library row and the portable
/// connection requirements keyed by `(component-digest, store-alias)` — so they
/// are returned together rather than persisted from two independent decisions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedComponentFacts {
    pub component: AdmittedComponent,
    pub connections: Vec<ComponentConnection>,
}

/// Stable classification for component-fact normalization refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentFactErrorKind {
    EmptyIdentity,
    EmptyOperationSet,
    NonCanonicalIdentity,
    RegisteredOperationMismatch,
    InvalidComponentDigest,
    InvalidOperationDependency,
    DuplicateOperationDependency,
    ConflictingOperationDependency,
    OperationDependencyMismatch,
    DuplicateInputPort,
    DuplicateOutputPort,
    DuplicateParameter,
    InvalidSchema,
    RemoteSchemaReference,
    UnimportedEffect,
    UnprojectedEffect,
    DuplicateConnection,
    UnimportedConnection,
    UndeclaredConnection,
    UnexpectedOperationStatements,
    InvalidStatementFact,
    DuplicateStatementField,
}

/// Refusal to mint a complete normalized component-library fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentFactError {
    kind: ComponentFactErrorKind,
    detail: Box<str>,
}

impl ComponentFactError {
    /// Stable refusal class for callers that must not match display text.
    pub fn kind(&self) -> ComponentFactErrorKind {
        self.kind
    }

    fn new(kind: ComponentFactErrorKind, detail: impl Into<Box<str>>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for ComponentFactError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for ComponentFactError {}

/// Normalize one byte-verified component declaration for catalog persistence.
///
/// `imports` and `effects` are both derived from the exact bytes by the caller
/// that audited them; this function re-checks that every effect interface is
/// one of those imports, and that declared connections and effect-bearing
/// imports account for each other in both directions.
pub fn normalize_component_fact(
    declaration: ComponentDeclaration,
    component_digest: String,
    imports: impl IntoIterator<Item = String>,
    effects: Vec<AdmittedComponentEffect>,
) -> Result<AdmittedComponentFacts, ComponentFactError> {
    ComponentPackageScope::new(
        &declaration.scope.tenant_id,
        &declaration.scope.package_id,
        &declaration.scope.package_version,
    )
    .map_err(|error| {
        ComponentFactError::new(
            ComponentFactErrorKind::NonCanonicalIdentity,
            error.to_string(),
        )
    })?;
    validate_identity(&declaration.component, "component")?;
    validate_identity(&declaration.interface_version, "interface-version")?;
    if declaration.operations.is_empty() {
        return Err(ComponentFactError::new(
            ComponentFactErrorKind::EmptyOperationSet,
            "component operations must not be empty",
        ));
    }
    if !valid_digest(&component_digest) {
        return Err(ComponentFactError::new(
            ComponentFactErrorKind::InvalidComponentDigest,
            "component-digest must be sha256:<64 lowercase hex digits>",
        ));
    }

    let mut operations = BTreeMap::new();
    for (export, operation) in declaration.operations {
        validate_identity(&export, "operation export")?;
        validate_registered_operation_scope(
            &declaration.scope,
            &export,
            operation.registered_operation.as_deref(),
        )?;
        let dependencies = normalize_operation_dependencies(operation.dependencies)?;
        let input_ports = normalize_ports(
            operation.input_ports,
            ComponentFactErrorKind::DuplicateInputPort,
            "input-port",
        )?;
        let output_ports = normalize_ports(
            operation.output_ports,
            ComponentFactErrorKind::DuplicateOutputPort,
            "output-port",
        )?;
        let parameters = normalize_parameters(operation.parameters)?;
        operations.insert(
            export,
            AdmittedComponentOperation {
                registered_operation: operation.registered_operation,
                dependencies,
                input_ports,
                output_ports,
                parameters,
                statements: BTreeMap::new(),
            },
        );
    }

    let mut imports: Vec<_> = imports.into_iter().collect();
    imports.sort();
    imports.dedup();
    let imports_fingerprint = wamn_execution_contract::canonical_json_sha256(
        &serde_json::to_value(&imports).expect("a string list serializes"),
    );
    let dependency_imports = operation_dependency_imports(&operations)?;
    validate_operation_dependency_imports(&imports, &dependency_imports)?;
    let effects = normalize_effects(effects, &imports, &dependency_imports)?;
    let connections = normalize_connections(declaration.connections, &effects)?;

    Ok(AdmittedComponentFacts {
        component: AdmittedComponent {
            scope: declaration.scope,
            component: declaration.component,
            interface_version: declaration.interface_version,
            operations,
            component_digest,
            imports,
            imports_fingerprint,
            effects,
        },
        connections,
    })
}

/// MVP wiring compatibility: exact canonical schema-digest equality only.
///
/// This intentionally makes no structural-subtyping promise. Relaxing exact
/// equality later is compatible; callers cannot depend on an unshipped
/// widening today.
pub fn schema_digests_match(left: &ComponentSchema, right: &ComponentSchema) -> bool {
    left.schema_digest == right.schema_digest
}

/// Refuse a stored component fact whose effects its own imports do not derive.
///
/// `effects` is a total function of the audited imports, and `imports` is the
/// half the OCI artifact config digest attests, so re-running the admission
/// rules over a stored pair decides whether a validator ever derived that
/// projection — without the component bytes. A row that fails here was written
/// by something other than the validator (wamn-0h0g.21.10: the converge ALTER
/// that defaulted pre-existing rows to `'[]'`, the positive claim of purity)
/// and its component is unpublishable until it is re-admitted.
pub fn verify_stored_effect_projection(
    component: &AdmittedComponent,
) -> Result<(), ComponentFactError> {
    ComponentPackageScope::new(
        &component.scope.tenant_id,
        &component.scope.package_id,
        &component.scope.package_version,
    )
    .map_err(|error| {
        ComponentFactError::new(
            ComponentFactErrorKind::NonCanonicalIdentity,
            error.to_string(),
        )
    })?;
    if component.operations.is_empty() {
        return Err(ComponentFactError::new(
            ComponentFactErrorKind::EmptyOperationSet,
            "component operations must not be empty",
        ));
    }
    for (export, operation) in &component.operations {
        validate_identity(export, "operation export")?;
        validate_registered_operation_scope(
            &component.scope,
            export,
            operation.registered_operation.as_deref(),
        )?;
        let dependencies = normalize_operation_dependencies(operation.dependencies.clone())?;
        if dependencies != operation.dependencies {
            return Err(ComponentFactError::new(
                ComponentFactErrorKind::InvalidOperationDependency,
                format!("operation {export:?} dependencies are not normalized"),
            ));
        }
        validate_operation_statement_facts(export, &operation.statements)?;
    }
    let dependency_imports = operation_dependency_imports(&component.operations)?;
    validate_operation_dependency_imports(&component.imports, &dependency_imports)?;
    normalize_effects(
        component.effects.clone(),
        &component.imports,
        &dependency_imports,
    )
    .map(|_| ())
}

/// Attach generated SQL facts to manifest-backed exports atomically.
///
/// The publisher supplies only manifest-backed exports. Public package
/// operations are necessarily present through `registered-operation`; private
/// package operations are admitted by their manifest match at the publisher
/// and deliberately carry no authorization token. Unmentioned exports remain
/// palette operations with no package SQL.
pub fn bind_component_statement_facts(
    component: &mut AdmittedComponent,
    statements: BTreeMap<String, BTreeMap<String, ComponentSqlStatement>>,
) -> Result<(), ComponentFactError> {
    let registered = component
        .operations
        .iter()
        .filter_map(|(export, operation)| {
            operation
                .registered_operation
                .as_ref()
                .map(|_| export.clone())
        })
        .collect::<BTreeSet<_>>();
    let exported = component
        .operations
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    let supplied = statements.keys().cloned().collect::<BTreeSet<_>>();
    if !registered.is_subset(&supplied) || !supplied.is_subset(&exported) {
        let missing = registered.difference(&supplied).collect::<Vec<_>>();
        let extra = supplied.difference(&exported).collect::<Vec<_>>();
        return Err(ComponentFactError::new(
            ComponentFactErrorKind::UnexpectedOperationStatements,
            format!(
                "statement operation set differs from component exports: missing-public={missing:?}, extra={extra:?}"
            ),
        ));
    }
    for (export, operation_statements) in &statements {
        validate_operation_statement_facts(export, operation_statements)?;
    }
    for (export, operation_statements) in statements {
        component
            .operations
            .get_mut(&export)
            .expect("the exact operation-set comparison proved this export exists")
            .statements = operation_statements;
    }
    Ok(())
}

pub(crate) fn validate_operation_statement_facts(
    export: &str,
    statements: &BTreeMap<String, ComponentSqlStatement>,
) -> Result<(), ComponentFactError> {
    let mut names = BTreeSet::new();
    let mut paths = BTreeSet::new();
    for (digest, statement) in statements {
        if !valid_digest(digest) || component_sql_digest(statement.sql.as_bytes()) != *digest {
            return Err(ComponentFactError::new(
                ComponentFactErrorKind::InvalidStatementFact,
                format!(
                    "operation {export:?} statement {:?} digest disagrees with its SQL bytes",
                    statement.name
                ),
            ));
        }
        validate_identity(&statement.name, "statement name")?;
        if !is_safe_statement_path(&statement.path) {
            return Err(ComponentFactError::new(
                ComponentFactErrorKind::InvalidStatementFact,
                format!(
                    "operation {export:?} statement {:?} path is not a canonical package-relative SQL path",
                    statement.name
                ),
            ));
        }
        if !names.insert(&statement.name) || !paths.insert(&statement.path) {
            return Err(ComponentFactError::new(
                ComponentFactErrorKind::InvalidStatementFact,
                format!("operation {export:?} repeats a statement name or path"),
            ));
        }
        validate_statement_fields(export, &statement.name, "bind", &statement.binds)?;
        validate_statement_fields(export, &statement.name, "column", &statement.columns)?;
    }
    Ok(())
}

fn validate_statement_fields(
    export: &str,
    statement: &str,
    field_kind: &str,
    fields: &[ComponentSqlField],
) -> Result<(), ComponentFactError> {
    let mut names = BTreeSet::new();
    for field in fields {
        validate_identity(&field.name, field_kind)?;
        if !names.insert(&field.name) {
            return Err(ComponentFactError::new(
                ComponentFactErrorKind::DuplicateStatementField,
                format!(
                    "operation {export:?} statement {statement:?} repeats {field_kind} {:?}",
                    field.name
                ),
            ));
        }
    }
    Ok(())
}

fn is_safe_statement_path(path: &str) -> bool {
    let parsed = Path::new(path);
    !parsed.as_os_str().is_empty()
        && parsed
            .extension()
            .is_some_and(|extension| extension == "sql")
        && !path.contains('\\')
        && parsed
            .components()
            .all(|part| matches!(part, PathComponent::Normal(_)))
        && parsed
            .components()
            .filter_map(|part| match part {
                PathComponent::Normal(part) => part.to_str(),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("/")
            == path
}

/// SHA-256 identity of exact SQL source bytes.
pub fn component_sql_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity("sha256:".len() + 64);
    output.push_str("sha256:");
    for byte in digest {
        use fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to a string is infallible");
    }
    output
}

fn validate_registered_operation_scope(
    scope: &ComponentPackageScope,
    export: &str,
    operation: Option<&str>,
) -> Result<(), ComponentFactError> {
    let Some(operation) = operation else {
        return Ok(());
    };
    validate_canonical_operation_for_package(operation, &scope.package_id, &scope.package_version)
        .map_err(|error| {
            ComponentFactError::new(
                ComponentFactErrorKind::NonCanonicalIdentity,
                error.to_string(),
            )
        })?;
    if operation != export {
        return Err(ComponentFactError::new(
            ComponentFactErrorKind::RegisteredOperationMismatch,
            format!("registered operation {operation:?} must equal exported operation {export:?}"),
        ));
    }
    Ok(())
}

fn normalize_operation_dependencies(
    declarations: Vec<ComponentOperationDependency>,
) -> Result<Vec<ComponentOperationDependency>, ComponentFactError> {
    let mut seen = BTreeSet::new();
    let mut dependencies = Vec::with_capacity(declarations.len());
    for declaration in declarations {
        let coordinate = PackageCoordinate::new(&declaration.package, &declaration.version)
            .map_err(|error| {
                ComponentFactError::new(
                    ComponentFactErrorKind::InvalidOperationDependency,
                    error.to_string(),
                )
            })?;
        if !valid_digest(&declaration.digest) {
            return Err(ComponentFactError::new(
                ComponentFactErrorKind::InvalidOperationDependency,
                "operation dependency digest must be sha256:<64 lowercase hex digits>",
            ));
        }
        validate_canonical_operation_for_package(
            &declaration.operation,
            coordinate.package_id(),
            coordinate.package_version(),
        )
        .map_err(|error| {
            ComponentFactError::new(
                ComponentFactErrorKind::InvalidOperationDependency,
                error.to_string(),
            )
        })?;
        if !seen.insert(declaration.operation.clone()) {
            return Err(ComponentFactError::new(
                ComponentFactErrorKind::DuplicateOperationDependency,
                format!(
                    "operation dependency {:?} is duplicated",
                    declaration.operation
                ),
            ));
        }
        dependencies.push(declaration);
    }
    dependencies.sort_by(|left, right| left.operation.cmp(&right.operation));
    Ok(dependencies)
}

fn validate_operation_dependency_imports(
    imports: &[String],
    dependencies: &BTreeSet<&str>,
) -> Result<(), ComponentFactError> {
    let imported_dependencies = imports
        .iter()
        .map(String::as_str)
        .filter(|import| is_application_operation_import(import))
        .collect::<BTreeSet<_>>();
    if &imported_dependencies != dependencies {
        let missing = dependencies
            .difference(&imported_dependencies)
            .copied()
            .collect::<Vec<_>>();
        let extra = imported_dependencies
            .difference(dependencies)
            .copied()
            .collect::<Vec<_>>();
        return Err(ComponentFactError::new(
            ComponentFactErrorKind::OperationDependencyMismatch,
            format!(
                "declared operation dependencies differ from audited imports: missing={missing:?}, extra={extra:?}"
            ),
        ));
    }
    Ok(())
}

fn operation_dependency_imports(
    operations: &BTreeMap<String, AdmittedComponentOperation>,
) -> Result<BTreeSet<&str>, ComponentFactError> {
    let mut pins = BTreeMap::new();
    for dependency in operations
        .values()
        .flat_map(|operation| operation.dependencies.iter())
    {
        if let Some(existing) = pins.insert(dependency.operation.as_str(), dependency)
            && existing != dependency
        {
            return Err(ComponentFactError::new(
                ComponentFactErrorKind::ConflictingOperationDependency,
                format!(
                    "operation dependency {:?} carries conflicting package, version, or digest pins",
                    dependency.operation
                ),
            ));
        }
    }
    Ok(pins.into_keys().collect())
}

/// Whether an import is a cross-package APPLICATION call rather than a
/// platform capability.
///
/// Keyed on the capability registry, not on a namespace. This was the LAST
/// namespace heuristic in the tree, and §2a missed it while deleting
/// `is_effect_package`: it read "not `wasi` and not `wamn` ⇒ an application
/// call", which silently misclassified `wasmcloud:blobstore` as a cross-package
/// operation dependency and made every blobstore-effect component fail its
/// stored projection with `OperationDependencyMismatch`. §2a's claim that
/// interfaces carry name, shape AND posture with no second classifier was not
/// true until this moved.
///
/// Matching is on the registered PACKAGE, at any version, because this asks
/// what KIND of import it is and kind does not change with version. A
/// `wasi:clocks` import at an unregistered version is still a platform
/// capability; refusing the version is admission's job, not this classifier's.
fn is_application_operation_import(name: &str) -> bool {
    !wamn_component_policy::is_registered_package(name)
}

/// Sort and deduplicate derived effects, pinning them to platform imports.
///
/// Both directions are checked after excluding exact cross-package operation
/// dependencies: those remain in the audited import inventory but are resolved
/// as component calls, not host capability effects.
fn normalize_effects(
    effects: Vec<AdmittedComponentEffect>,
    imports: &[String],
    dependency_imports: &BTreeSet<&str>,
) -> Result<Vec<AdmittedComponentEffect>, ComponentFactError> {
    let mut normalized = Vec::with_capacity(effects.len());
    for effect in effects {
        validate_identity(&effect.package, "effect-package")?;
        let mut interfaces = effect.interfaces;
        interfaces.sort();
        interfaces.dedup();
        for interface in &interfaces {
            if !imports.iter().any(|import| import == interface)
                || dependency_imports.contains(interface.as_str())
            {
                return Err(ComponentFactError::new(
                    ComponentFactErrorKind::UnimportedEffect,
                    format!(
                        "effect interface {interface:?} is not an audited platform-capability import"
                    ),
                ));
            }
        }
        normalized.push(AdmittedComponentEffect {
            package: effect.package,
            interfaces,
        });
    }
    normalized.sort_by(|left, right| left.package.cmp(&right.package));
    normalized.dedup_by(|left, right| left.package == right.package);

    for import in imports {
        if dependency_imports.contains(import.as_str()) {
            continue;
        }
        let package = wamn_component_policy::import_pkg(import);
        if wamn_component_policy::import_posture(import)
            != Some(wamn_component_policy::Posture::Effect)
        {
            continue;
        }
        if !normalized.iter().any(|effect| {
            effect.package == package
                && effect
                    .interfaces
                    .iter()
                    .any(|interface| interface == import)
        }) {
            return Err(ComponentFactError::new(
                ComponentFactErrorKind::UnprojectedEffect,
                format!("audited import {import:?} is absent from the effect projection"),
            ));
        }
    }
    Ok(normalized)
}

/// Normalize declared connections against the effects the bytes actually prove.
fn normalize_connections(
    declarations: Vec<ComponentConnection>,
    effects: &[AdmittedComponentEffect],
) -> Result<Vec<ComponentConnection>, ComponentFactError> {
    let mut seen = BTreeSet::new();
    let mut connections = Vec::with_capacity(declarations.len());
    for declaration in declarations {
        validate_identity(&declaration.store_alias, "store-alias")?;
        if !seen.insert(declaration.store_alias.clone()) {
            return Err(ComponentFactError::new(
                ComponentFactErrorKind::DuplicateConnection,
                format!("store-alias {:?} is duplicated", declaration.store_alias),
            ));
        }
        let package = declaration.requirement_type.import_package();
        if !effects.iter().any(|effect| effect.package == package) {
            return Err(ComponentFactError::new(
                ComponentFactErrorKind::UnimportedConnection,
                format!(
                    "store-alias {:?} requires package {package:?}, which these bytes do not import",
                    declaration.store_alias
                ),
            ));
        }
        connections.push(declaration);
    }
    connections.sort_by(|left, right| left.store_alias.cmp(&right.store_alias));

    // The reverse direction: connection authority nothing binds is authority
    // the environment can never satisfy, so it is refused at admission rather
    // than surfacing as an unresolvable effect at delivery time.
    for connection_type in CONNECTION_TYPES {
        let package = connection_type.import_package();
        if effects.iter().any(|effect| effect.package == package)
            && !connections
                .iter()
                .any(|connection| connection.requirement_type == connection_type)
        {
            return Err(ComponentFactError::new(
                ComponentFactErrorKind::UndeclaredConnection,
                format!("package {package:?} is imported but no store-alias declares it"),
            ));
        }
    }
    Ok(connections)
}

fn normalize_ports(
    declarations: Vec<ComponentPortDeclaration>,
    duplicate_kind: ComponentFactErrorKind,
    field: &'static str,
) -> Result<Vec<AdmittedComponentPort>, ComponentFactError> {
    let mut seen = BTreeSet::new();
    let mut ports = Vec::with_capacity(declarations.len());
    for declaration in declarations {
        validate_identity(&declaration.name, field)?;
        if !seen.insert(declaration.name.clone()) {
            return Err(ComponentFactError::new(
                duplicate_kind,
                format!("{field} {:?} is duplicated", declaration.name),
            ));
        }
        ports.push(AdmittedComponentPort {
            name: declaration.name,
            schema: normalize_schema(declaration.schema, field)?,
        });
    }
    ports.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(ports)
}

fn normalize_parameters(
    declarations: Vec<ComponentParameterDeclaration>,
) -> Result<Vec<AdmittedComponentParameter>, ComponentFactError> {
    let mut seen = BTreeSet::new();
    let mut parameters = Vec::with_capacity(declarations.len());
    for declaration in declarations {
        validate_identity(&declaration.name, "parameter")?;
        if !seen.insert(declaration.name.clone()) {
            return Err(ComponentFactError::new(
                ComponentFactErrorKind::DuplicateParameter,
                format!("parameter {:?} is duplicated", declaration.name),
            ));
        }
        parameters.push(AdmittedComponentParameter {
            name: declaration.name,
            schema: normalize_schema(declaration.schema, "parameter")?,
            required: declaration.required,
        });
    }
    parameters.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(parameters)
}

fn normalize_schema(schema: Value, field: &str) -> Result<ComponentSchema, ComponentFactError> {
    if let Some(declared) = schema.get("$schema")
        && declared.as_str() != Some(JSON_SCHEMA_2020_12)
    {
        return Err(ComponentFactError::new(
            ComponentFactErrorKind::InvalidSchema,
            format!("{field} schema must use JSON Schema draft 2020-12"),
        ));
    }
    if has_remote_ref(&schema) {
        return Err(ComponentFactError::new(
            ComponentFactErrorKind::RemoteSchemaReference,
            format!("{field} schema may only use document-local references"),
        ));
    }

    let mut compiler = Compiler::new();
    compiler.set_default_draft(Draft::V2020_12);
    compiler
        .add_resource(SCHEMA_URI, schema.clone())
        .map_err(|error| {
            ComponentFactError::new(
                ComponentFactErrorKind::InvalidSchema,
                format!("{field} schema is invalid: {error}"),
            )
        })?;
    let mut schemas = Schemas::new();
    compiler
        .compile(SCHEMA_URI, &mut schemas)
        .map_err(|error| {
            ComponentFactError::new(
                ComponentFactErrorKind::InvalidSchema,
                format!("{field} schema is invalid: {error}"),
            )
        })?;

    let schema_digest = wamn_execution_contract::canonical_json_sha256(&schema);
    Ok(ComponentSchema {
        schema,
        schema_digest,
    })
}

fn has_remote_ref(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(has_remote_ref),
        Value::Object(values) => values.iter().any(|(key, value)| {
            (key == "$ref"
                && value
                    .as_str()
                    .is_none_or(|reference| !reference.starts_with('#')))
                || has_remote_ref(value)
        }),
        _ => false,
    }
}

fn validate_identity(value: &str, field: &str) -> Result<(), ComponentFactError> {
    if value.is_empty() {
        return Err(ComponentFactError::new(
            ComponentFactErrorKind::EmptyIdentity,
            format!("{field} is empty"),
        ));
    }
    if value.trim() != value || value.as_bytes().contains(&0) {
        return Err(ComponentFactError::new(
            ComponentFactErrorKind::NonCanonicalIdentity,
            format!("{field} is not in canonical form"),
        ));
    }
    Ok(())
}

fn valid_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn declaration() -> ComponentDeclaration {
        ComponentDeclaration {
            scope: ComponentPackageScope {
                tenant_id: "tenant-a".to_string(),
                package_id: "orders".to_string(),
                package_version: "1.2.0".to_string(),
            },
            component: "transform".to_string(),
            interface_version: "0.1.0".to_string(),
            operations: BTreeMap::from([(
                "map".to_string(),
                ComponentOperationDeclaration {
                    registered_operation: None,
                    dependencies: Vec::new(),
                    input_ports: vec![ComponentPortDeclaration {
                        name: "input".to_string(),
                        schema: json!({
                            "$schema": JSON_SCHEMA_2020_12,
                            "type": "object",
                            "required": ["id"],
                            "properties": {"id": {"type": "string"}}
                        }),
                    }],
                    output_ports: vec![ComponentPortDeclaration {
                        name: "main".to_string(),
                        schema: json!({"type": "object"}),
                    }],
                    parameters: vec![ComponentParameterDeclaration {
                        name: "mapping".to_string(),
                        schema: json!({"type": "object"}),
                        required: true,
                    }],
                },
            )]),
            connections: Vec::new(),
        }
    }

    fn operation_mut(declaration: &mut ComponentDeclaration) -> &mut ComponentOperationDeclaration {
        declaration
            .operations
            .get_mut("map")
            .expect("map operation")
    }

    fn postgres_effect() -> AdmittedComponentEffect {
        AdmittedComponentEffect {
            package: "wamn:postgres".to_string(),
            interfaces: vec!["wamn:postgres/client@0.1.0".to_string()],
        }
    }

    fn operation_dependency() -> ComponentOperationDependency {
        ComponentOperationDependency {
            package: "wamn_receiving".to_string(),
            version: "1.0.0".to_string(),
            digest: format!("sha256:{}", "c".repeat(64)),
            operation: "wamn-receiving:receiving/record-receipt@1.0.0".to_string(),
        }
    }

    #[test]
    fn normalization_is_sorted_and_carries_every_component_owned_fact() {
        let facts = normalize_component_fact(
            declaration(),
            format!("sha256:{}", "a".repeat(64)),
            [
                "wasi:io/streams@0.2.3".to_string(),
                "wamn:postgres/client@0.1.0".to_string(),
            ],
            vec![postgres_effect()],
        )
        .expect("component fact normalizes");
        let fact = facts.component;

        assert_eq!(fact.scope.package_version, "1.2.0");
        let operation = fact.operation("map").expect("map operation is admitted");
        assert_eq!(operation.registered_operation, None);
        assert_eq!(operation.input_ports[0].name, "input");
        assert_eq!(operation.output_ports[0].name, "main");
        assert!(operation.parameters[0].required);
        assert_eq!(fact.component_digest, format!("sha256:{}", "a".repeat(64)));
        assert_eq!(fact.imports.len(), 2);
        assert!(fact.imports_fingerprint.starts_with("sha256:"));
        assert_eq!(fact.effects, vec![postgres_effect()]);
        assert!(facts.connections.is_empty());
    }

    #[test]
    fn persisted_component_scope_uses_the_package_coordinate_guard() {
        assert!(
            serde_json::from_value::<ComponentPackageScope>(json!({
                "tenant-id": "tenant-a",
                "package-id": "not-snake",
                "package-version": "1.0.0"
            }))
            .is_err()
        );
    }

    /// A component with no effect-bearing import records the POSITIVE fact that
    /// it is pure, rather than an absent one — this is what a caller reads to
    /// decide an occurrence writes no effect-ledger row.
    #[test]
    fn a_component_importing_no_authority_admits_as_pure() {
        let facts = normalize_component_fact(
            declaration(),
            format!("sha256:{}", "a".repeat(64)),
            ["wasi:clocks/monotonic-clock@0.2.3".to_string()],
            Vec::new(),
        )
        .expect("a pure component normalizes");

        assert!(facts.component.effects.is_empty());
        assert_eq!(facts.component.imports.len(), 1);
    }

    #[test]
    fn exact_operation_dependency_is_preserved_without_becoming_an_effect() {
        let dependency = operation_dependency();
        let mut declared = declaration();
        operation_mut(&mut declared).dependencies = vec![dependency.clone()];

        let facts = normalize_component_fact(
            declared,
            format!("sha256:{}", "a".repeat(64)),
            [dependency.operation.clone()],
            Vec::new(),
        )
        .expect("an exact audited operation dependency admits");

        assert_eq!(facts.component.operations["map"].dependencies, [dependency]);
        assert!(facts.component.effects.is_empty());
        verify_stored_effect_projection(&facts.component)
            .expect("the stored dependency projection remains verifiable");
    }

    #[test]
    fn operation_dependency_must_be_exact_and_audited() {
        let dependency = operation_dependency();
        let mut missing = declaration();
        operation_mut(&mut missing).dependencies = vec![dependency.clone()];
        assert_eq!(
            normalize_component_fact(
                missing,
                format!("sha256:{}", "a".repeat(64)),
                Vec::new(),
                Vec::new(),
            )
            .unwrap_err()
            .kind(),
            ComponentFactErrorKind::OperationDependencyMismatch
        );

        let mut invalid_digest = declaration();
        let mut malformed = dependency.clone();
        malformed.digest = "latest".to_string();
        operation_mut(&mut invalid_digest).dependencies = vec![malformed];
        assert_eq!(
            normalize_component_fact(
                invalid_digest,
                format!("sha256:{}", "a".repeat(64)),
                [dependency.operation.clone()],
                Vec::new(),
            )
            .unwrap_err()
            .kind(),
            ComponentFactErrorKind::InvalidOperationDependency
        );

        let mut mismatched_coordinate = declaration();
        let mut mismatched = dependency.clone();
        mismatched.version = "2.0.0".to_string();
        operation_mut(&mut mismatched_coordinate).dependencies = vec![mismatched];
        assert_eq!(
            normalize_component_fact(
                mismatched_coordinate,
                format!("sha256:{}", "a".repeat(64)),
                [dependency.operation.clone()],
                Vec::new(),
            )
            .unwrap_err()
            .kind(),
            ComponentFactErrorKind::InvalidOperationDependency
        );

        let mut duplicate = declaration();
        operation_mut(&mut duplicate).dependencies = vec![dependency.clone(), dependency.clone()];
        assert_eq!(
            normalize_component_fact(
                duplicate,
                format!("sha256:{}", "a".repeat(64)),
                [dependency.operation.clone()],
                Vec::new(),
            )
            .unwrap_err()
            .kind(),
            ComponentFactErrorKind::DuplicateOperationDependency
        );

        let mut conflicting = declaration();
        let operation = operation_mut(&mut conflicting);
        operation.dependencies = vec![dependency.clone()];
        let mut second = operation.clone();
        second.dependencies[0].digest = format!("sha256:{}", "d".repeat(64));
        conflicting.operations.insert("map-two".to_string(), second);
        assert_eq!(
            normalize_component_fact(
                conflicting,
                format!("sha256:{}", "a".repeat(64)),
                [dependency.operation],
                Vec::new(),
            )
            .unwrap_err()
            .kind(),
            ComponentFactErrorKind::ConflictingOperationDependency
        );
    }

    #[test]
    fn registered_operation_is_explicit_and_equals_the_export_token() {
        let mut declared = declaration();
        let registered = "orders:purchase-order/get@1.2.0";
        let mut operation = declared.operations.remove("map").expect("map operation");
        operation.registered_operation = Some(registered.to_string());
        declared
            .operations
            .insert(registered.to_string(), operation);
        let facts = normalize_component_fact(
            declared,
            format!("sha256:{}", "a".repeat(64)),
            ["wasi:clocks/monotonic-clock@0.2.3".to_string()],
            Vec::new(),
        )
        .expect("canonical registered operation is admitted");
        assert_eq!(
            facts.component.operations[registered]
                .registered_operation
                .as_deref(),
            Some(registered)
        );

        let mut malformed = declaration();
        operation_mut(&mut malformed).registered_operation = Some("purchase_order.get".to_string());
        assert_eq!(
            normalize_component_fact(
                malformed,
                format!("sha256:{}", "a".repeat(64)),
                ["wasi:clocks/monotonic-clock@0.2.3".to_string()],
                Vec::new(),
            )
            .unwrap_err()
            .kind(),
            ComponentFactErrorKind::NonCanonicalIdentity
        );

        for operation in [
            "other:purchase-order/get@1.2.0",
            "orders:purchase-order/get@2.0.0",
        ] {
            let mut mismatched = declaration();
            operation_mut(&mut mismatched).registered_operation = Some(operation.to_string());
            assert_eq!(
                normalize_component_fact(
                    mismatched,
                    format!("sha256:{}", "a".repeat(64)),
                    ["wasi:clocks/monotonic-clock@0.2.3".to_string()],
                    Vec::new(),
                )
                .unwrap_err()
                .kind(),
                ComponentFactErrorKind::NonCanonicalIdentity
            );
        }

        let mut unequal = declaration();
        operation_mut(&mut unequal).registered_operation =
            Some("orders:purchase-order/get@1.2.0".to_string());
        assert_eq!(
            normalize_component_fact(
                unequal,
                format!("sha256:{}", "a".repeat(64)),
                Vec::new(),
                Vec::new(),
            )
            .unwrap_err()
            .kind(),
            ComponentFactErrorKind::RegisteredOperationMismatch
        );

        let mut stored = migration_defaulted_fact(&[]);
        stored
            .operations
            .get_mut("map")
            .expect("map operation")
            .registered_operation = Some("other:purchase-order/get@1.2.0".into());
        assert_eq!(
            verify_stored_effect_projection(&stored).unwrap_err().kind(),
            ComponentFactErrorKind::NonCanonicalIdentity
        );
    }

    /// A stored row exactly as the wamn-0h0g.21.9 converge ALTER leaves one:
    /// the audited imports it was admitted with, and the `'[]'` the DEFAULT
    /// wrote over them.
    fn migration_defaulted_fact(imports: &[&str]) -> AdmittedComponent {
        AdmittedComponent {
            scope: ComponentPackageScope {
                tenant_id: "tenant-a".to_string(),
                package_id: "orders".to_string(),
                package_version: "1.2.0".to_string(),
            },
            component: "transform".to_string(),
            interface_version: "0.1.0".to_string(),
            operations: BTreeMap::from([(
                "map".to_string(),
                AdmittedComponentOperation {
                    registered_operation: None,
                    dependencies: Vec::new(),
                    input_ports: Vec::new(),
                    output_ports: Vec::new(),
                    parameters: Vec::new(),
                    statements: BTreeMap::new(),
                },
            )]),
            component_digest: format!("sha256:{}", "a".repeat(64)),
            imports: imports.iter().map(|name| (*name).to_string()).collect(),
            imports_fingerprint: format!("sha256:{}", "b".repeat(64)),
            effects: Vec::new(),
        }
    }

    /// wamn-0h0g.21.10. The converge ALTER defaulted every pre-existing row to
    /// `'[]'` — the positive claim of purity — for a component whose own
    /// audited imports prove it reaches Postgres. No validator derived that, so
    /// reading the row must refuse rather than trust it.
    #[test]
    fn a_migration_defaulted_effect_projection_is_refused() {
        let stored = migration_defaulted_fact(&["wamn:postgres/client@0.1.0"]);

        let error = verify_stored_effect_projection(&stored)
            .expect_err("an underived purity claim is refused");

        assert_eq!(error.kind(), ComponentFactErrorKind::UnprojectedEffect);
    }

    /// The other half of the same guard: a row whose `'[]'` happens to be the
    /// value its imports do derive is not a fabricated claim, and stays
    /// readable. That is what keeps the refusal scoped to exactly the rows a
    /// validator never produced.
    #[test]
    fn a_stored_pure_projection_its_imports_derive_stays_readable() {
        let stored = migration_defaulted_fact(&["wasi:clocks/monotonic-clock@0.2.3"]);

        verify_stored_effect_projection(&stored).expect("a derived pure projection verifies");
    }

    /// Admission itself must never mint the shape the migration fabricated.
    #[test]
    fn admission_refuses_an_effect_projection_narrower_than_the_imports() {
        let error = normalize_component_fact(
            declaration(),
            format!("sha256:{}", "a".repeat(64)),
            ["wamn:postgres/client@0.1.0".to_string()],
            Vec::new(),
        )
        .expect_err("an under-claimed projection is refused at admission");

        assert_eq!(error.kind(), ComponentFactErrorKind::UnprojectedEffect);
    }

    #[test]
    fn connections_and_effect_imports_must_account_for_each_other() {
        let http_effect = AdmittedComponentEffect {
            package: "wamn:connection".to_string(),
            interfaces: vec!["wamn:connection/http@0.1.0".to_string()],
        };
        let imports = ["wamn:connection/http@0.1.0".to_string()];
        let connection = ComponentConnection {
            store_alias: "erp".to_string(),
            requirement_type: ComponentConnectionType::Http,
        };

        let mut declared = declaration();
        declared.connections = vec![connection.clone()];
        let facts = normalize_component_fact(
            declared.clone(),
            format!("sha256:{}", "a".repeat(64)),
            imports.clone(),
            vec![http_effect.clone()],
        )
        .expect("a declared connection backed by its import admits");
        assert_eq!(facts.connections, vec![connection.clone()]);

        // Imported authority nothing declares.
        let mut undeclared = declaration();
        undeclared.connections = Vec::new();
        assert_eq!(
            normalize_component_fact(
                undeclared,
                format!("sha256:{}", "b".repeat(64)),
                imports.clone(),
                vec![http_effect],
            )
            .unwrap_err()
            .kind(),
            ComponentFactErrorKind::UndeclaredConnection
        );

        // Declared authority the bytes never import.
        assert_eq!(
            normalize_component_fact(
                declared.clone(),
                format!("sha256:{}", "c".repeat(64)),
                Vec::new(),
                Vec::new(),
            )
            .unwrap_err()
            .kind(),
            ComponentFactErrorKind::UnimportedConnection
        );

        let mut duplicate = declared;
        duplicate.connections.push(connection);
        assert_eq!(
            normalize_component_fact(
                duplicate,
                format!("sha256:{}", "d".repeat(64)),
                imports,
                vec![AdmittedComponentEffect {
                    package: "wamn:connection".to_string(),
                    interfaces: vec!["wamn:connection/http@0.1.0".to_string()],
                }],
            )
            .unwrap_err()
            .kind(),
            ComponentFactErrorKind::DuplicateConnection
        );
    }

    /// Effects are a projection of the audited imports and can never widen
    /// beyond them, whatever the caller that derived them passes in.
    #[test]
    fn an_effect_interface_absent_from_the_audited_imports_refuses() {
        assert_eq!(
            normalize_component_fact(
                declaration(),
                format!("sha256:{}", "a".repeat(64)),
                ["wasi:io/streams@0.2.3".to_string()],
                vec![postgres_effect()],
            )
            .unwrap_err()
            .kind(),
            ComponentFactErrorKind::UnimportedEffect
        );
    }

    #[test]
    fn declaration_document_is_exact_kebab_case_json() {
        let document = serde_json::to_value(declaration()).expect("declaration serializes");
        assert!(document["operations"]["map"].get("input-ports").is_some());
        assert!(document.get("interface-version").is_some());
        assert_eq!(document["scope"]["tenant-id"], "tenant-a");
        assert_eq!(
            serde_json::from_value::<ComponentDeclaration>(document.clone())
                .expect("exact declaration parses"),
            declaration()
        );

        let mut unknown = document;
        unknown
            .as_object_mut()
            .expect("declaration is an object")
            .insert("environment".to_owned(), json!("dev"));
        assert!(serde_json::from_value::<ComponentDeclaration>(unknown).is_err());
    }

    #[test]
    fn schema_identity_is_rfc_8785_and_compatibility_is_exact() {
        let mut left = declaration();
        let mut right = declaration();
        operation_mut(&mut left).input_ports[0].schema =
            json!({"type":"object","properties":{"b":{"type":"number"},"a":{"type":"string"}}});
        operation_mut(&mut right).input_ports[0].schema =
            json!({"properties":{"a":{"type":"string"},"b":{"type":"number"}},"type":"object"});

        let left = normalize_component_fact(
            left,
            format!("sha256:{}", "b".repeat(64)),
            Vec::new(),
            Vec::new(),
        )
        .unwrap()
        .component;
        let right = normalize_component_fact(
            right,
            format!("sha256:{}", "c".repeat(64)),
            Vec::new(),
            Vec::new(),
        )
        .unwrap()
        .component;
        assert!(schema_digests_match(
            &left.operations["map"].input_ports[0].schema,
            &right.operations["map"].input_ports[0].schema
        ));

        let different = normalize_schema(json!({"type": "string"}), "input-port").unwrap();
        assert!(!schema_digests_match(
            &left.operations["map"].input_ports[0].schema,
            &different
        ));
    }

    #[test]
    fn partial_or_ambiguous_component_facts_refuse() {
        let mut empty = declaration();
        empty.operations.clear();
        assert_eq!(
            normalize_component_fact(
                empty,
                format!("sha256:{}", "c".repeat(64)),
                Vec::new(),
                Vec::new(),
            )
            .unwrap_err()
            .kind(),
            ComponentFactErrorKind::EmptyOperationSet
        );

        let mut duplicate = declaration();
        let duplicate_port = duplicate.operations["map"].input_ports[0].clone();
        operation_mut(&mut duplicate)
            .input_ports
            .push(duplicate_port);
        assert_eq!(
            normalize_component_fact(
                duplicate,
                format!("sha256:{}", "d".repeat(64)),
                Vec::new(),
                Vec::new(),
            )
            .unwrap_err()
            .kind(),
            ComponentFactErrorKind::DuplicateInputPort
        );

        let mut remote = declaration();
        operation_mut(&mut remote).parameters[0].schema =
            json!({"$ref": "https://example.invalid/schema"});
        assert_eq!(
            normalize_component_fact(
                remote,
                format!("sha256:{}", "e".repeat(64)),
                Vec::new(),
                Vec::new(),
            )
            .unwrap_err()
            .kind(),
            ComponentFactErrorKind::RemoteSchemaReference
        );
    }

    fn sql_statement(sql: &str) -> (String, ComponentSqlStatement) {
        (
            component_sql_digest(sql.as_bytes()),
            ComponentSqlStatement {
                name: "select_order".to_owned(),
                path: "generated/sql/purchase_order/get.sql".to_owned(),
                sql: sql.to_owned(),
                binds: vec![ComponentSqlField {
                    name: "id".to_owned(),
                    value_type: ComponentSqlValueType::Uuid,
                    nullable: false,
                }],
                columns: vec![ComponentSqlField {
                    name: "row_version".to_owned(),
                    value_type: ComponentSqlValueType::Int64,
                    nullable: false,
                }],
            },
        )
    }

    #[test]
    fn generated_statement_facts_bind_to_exact_registered_export() {
        let registered = "orders:purchase-order/get@1.2.0";
        let mut declared = declaration();
        let mut operation = declared.operations.remove("map").expect("map operation");
        operation.registered_operation = Some(registered.to_owned());
        declared.operations.insert(registered.to_owned(), operation);
        let mut component = normalize_component_fact(
            declared,
            format!("sha256:{}", "a".repeat(64)),
            Vec::new(),
            Vec::new(),
        )
        .expect("declaration normalizes before package evidence is attached")
        .component;
        let statement = sql_statement("SELECT row_version FROM purchase_order WHERE id = $1");

        bind_component_statement_facts(
            &mut component,
            BTreeMap::from([(registered.to_owned(), BTreeMap::from([statement.clone()]))]),
        )
        .expect("exact generated statement facts bind");

        assert_eq!(
            component.operations[registered].statement(&statement.0),
            Some(&statement.1)
        );
        verify_stored_effect_projection(&component)
            .expect("a stored component re-verifies its exact SQL bytes");
    }

    #[test]
    fn manifest_matched_private_operation_can_carry_sql_without_an_auth_token() {
        let mut component = migration_defaulted_fact(&[]);
        let statement = sql_statement("SELECT row_version FROM purchase_order WHERE id = $1");

        bind_component_statement_facts(
            &mut component,
            BTreeMap::from([("map".to_owned(), BTreeMap::from([statement.clone()]))]),
        )
        .expect("the publisher may bind a manifest-matched private operation");

        assert_eq!(
            component.operations["map"].statement(&statement.0),
            Some(&statement.1)
        );
    }

    #[test]
    fn statement_authority_refuses_wrong_operation_digest_and_shape() {
        let mut component = migration_defaulted_fact(&[]);
        let statement = sql_statement("SELECT row_version FROM purchase_order WHERE id = $1");
        assert_eq!(
            bind_component_statement_facts(
                &mut component,
                BTreeMap::from([("unknown".to_owned(), BTreeMap::new())]),
            )
            .unwrap_err()
            .kind(),
            ComponentFactErrorKind::UnexpectedOperationStatements
        );

        let operation = component.operations.get_mut("map").expect("map operation");
        operation.registered_operation = Some("orders:purchase-order/get@1.2.0".to_owned());
        operation.statements =
            BTreeMap::from([(format!("sha256:{}", "f".repeat(64)), statement.1.clone())]);
        assert_eq!(
            verify_stored_effect_projection(&component)
                .unwrap_err()
                .kind(),
            ComponentFactErrorKind::RegisteredOperationMismatch
        );

        let operation = component.operations.remove("map").expect("map operation");
        component
            .operations
            .insert("orders:purchase-order/get@1.2.0".to_owned(), operation);
        assert_eq!(
            verify_stored_effect_projection(&component)
                .unwrap_err()
                .kind(),
            ComponentFactErrorKind::InvalidStatementFact
        );

        let operation = component
            .operations
            .get_mut("orders:purchase-order/get@1.2.0")
            .expect("registered operation");
        let (digest, mut malformed) = statement;
        malformed.binds.push(malformed.binds[0].clone());
        operation.statements = BTreeMap::from([(digest, malformed)]);
        assert_eq!(
            verify_stored_effect_projection(&component)
                .unwrap_err()
                .kind(),
            ComponentFactErrorKind::DuplicateStatementField
        );
    }
}
