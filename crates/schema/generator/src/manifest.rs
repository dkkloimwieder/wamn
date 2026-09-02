use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{GenerateError, GenerateErrorKind};

const CONTROL_OWNED_RELATION_TABLES: [&str; 2] = ["wamn_entities", "wamn_cdc_exclusions"];

/// Strict package-owned behavior declaration parsed from `wamn.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageManifest {
    pub package: PackageIdentity,
    #[serde(default)]
    pub base_dependencies: BTreeMap<String, BaseDependencyRequirement>,
    pub required_platform_policy_contract: PolicyContractRequirement,
    pub models: BTreeMap<String, ModelDeclaration>,
    #[serde(default)]
    pub internal_relations: BTreeMap<String, InternalRelationDeclaration>,
    #[serde(default)]
    pub custom_operations: BTreeMap<String, CustomOperationDeclaration>,
    pub connections: BTreeMap<String, ConnectionDeclaration>,
    pub components: BTreeMap<String, ComponentDeclaration>,
}

/// One strict package-local operation backed only by declared static SQL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomOperationDeclaration {
    pub kind: CustomOperationKind,
    pub visibility: OperationVisibility,
    #[serde(default)]
    pub permission: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<String>,
    pub input: CustomOperationInputDeclaration,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<CustomOperationResultDeclaration>,
    pub errors: Vec<String>,
    pub error_details: BTreeMap<String, OperationErrorDetailDeclaration>,
    #[serde(default)]
    pub constraint_errors: BTreeMap<String, String>,
    #[serde(default)]
    pub relations: Vec<StaticSqlRelationDeclaration>,
    #[serde(default)]
    pub statements: BTreeMap<String, StaticSqlStatementDeclaration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transaction: Option<CommandTransaction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automatic_retry: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonicalization: Option<CommandCanonicalization>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registration: Option<EventRegistrationDeclaration>,
}

/// Closed non-CRUD operation kinds admitted by the package manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CustomOperationKind {
    Projection,
    Command,
    EventHandler,
}

impl CustomOperationDeclaration {
    /// Closed authored operation kind.
    pub const fn kind(&self) -> &'static str {
        match self.kind {
            CustomOperationKind::Projection => "projection",
            CustomOperationKind::Command => "command",
            CustomOperationKind::EventHandler => "event_handler",
        }
    }

    /// Route visibility declared for this operation.
    pub const fn visibility(&self) -> OperationVisibility {
        self.visibility
    }

    /// Exact public permission, absent for a private operation.
    pub fn permission(&self) -> Option<&str> {
        self.permission.as_deref()
    }

    /// Optional package-local component group; omission selects the sole group.
    pub fn component(&self) -> Option<&str> {
        self.component.as_deref()
    }

    /// Inline source registration, present only for an event handler.
    pub const fn registration(&self) -> Option<&EventRegistrationDeclaration> {
        self.registration.as_ref()
    }
}

/// One package-emitted entity observed by an event handler.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventRegistrationDeclaration {
    pub source_package: String,
    pub entity: String,
    pub ops: Vec<wamn_event_wire::Op>,
}

/// Transaction boundary admitted for custom commands in the POC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandTransaction {
    ExplicitPerInput,
}

/// Typed custom-operation input, with optional command-envelope bounds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomOperationInputDeclaration {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_body_maximum: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub envelope: Option<CountLimitDeclaration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_semantics: Option<ItemSemantics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<CountLimitDeclaration>,
    pub fields: Vec<ContractFieldDeclaration>,
}

/// Independent result semantics for each array-envelope item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemSemantics {
    PerInput,
}

/// One explicit count bound whose refusal stays at the operation layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CountLimitDeclaration {
    pub minimum: u32,
    pub maximum: u32,
    pub invalid: InputRefusal,
}

/// One typed leaf in an input or result contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractFieldDeclaration {
    pub path: String,
    #[serde(rename = "type")]
    pub ty: wamn_schema_introspection::ir::ColumnType,
    pub nullable: bool,
    #[serde(default)]
    pub values: Vec<String>,
}

/// Typed custom-operation result contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomOperationResultDeclaration {
    pub class: ResultClass,
    pub fields: Vec<ContractFieldDeclaration>,
}

/// Byte-stable command identity rules consumed by the runtime implementation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandCanonicalization {
    pub payload: CursorPayload,
    pub excluded_fields: Vec<String>,
    pub line_order: CommandLineOrder,
    pub duplicate_line: InputRefusal,
    pub uuid: UuidSpelling,
    pub timestamptz: TimestamptzSpelling,
    pub numeric: NumericSpelling,
}

/// Closed canonicalized line profile implemented by command generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandLineOrder {
    PurchaseOrderLineIdAscending,
}

/// Frozen UUID spelling at durable JSON boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UuidSpelling {
    LowercaseHyphenated,
}

/// Frozen timestamp spelling at durable JSON boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimestamptzSpelling {
    UtcRfc3339SixFractionalDigits,
}

/// Frozen numeric identity rule; scale is semantic command input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NumericSpelling {
    PostgresqlLexicalScalePreserved,
}

/// Closed operation-error detail keys serialized on per-item refusals.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationErrorDetailKey {
    Field,
    Id,
    ExpectedRowVersion,
    ObservedRowVersion,
    Minimum,
    Maximum,
    Observed,
    Constraint,
    Operation,
}

/// Required and optional keys for one exact error code.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OperationErrorDetailDeclaration {
    #[serde(default)]
    pub required: Vec<OperationErrorDetailKey>,
    #[serde(default)]
    pub optional: Vec<OperationErrorDetailKey>,
}

/// Closed generated-operation refusal vocabulary.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessOperationErrorLiteral {
    InvalidInput,
    NotFound,
    ConcurrencyConflict,
    UniqueViolation,
    ForeignKeyViolation,
    CheckViolation,
    Retry,
    Timeout,
    PermissionDenied,
    InternalError,
}

impl AccessOperationErrorLiteral {
    const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::NotFound => "not_found",
            Self::ConcurrencyConflict => "concurrency_conflict",
            Self::UniqueViolation => "unique_violation",
            Self::ForeignKeyViolation => "foreign_key_violation",
            Self::CheckViolation => "check_violation",
            Self::Retry => "retry",
            Self::Timeout => "timeout",
            Self::PermissionDenied => "permission_denied",
            Self::InternalError => "internal_error",
        }
    }
}

/// One migration-derived relation consumed by static custom-operation SQL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StaticSqlRelationDeclaration {
    pub schema: String,
    pub table: String,
    pub select_fields: Vec<String>,
    pub insert_fields: Vec<String>,
    pub update_fields: Vec<String>,
    /// Whether verified SQL takes a row lock; generation owns any PostgreSQL
    /// UPDATE carrier needed for the lock and does not treat it as DML intent.
    pub lock: bool,
    #[serde(default)]
    pub constraints: Vec<String>,
}

/// One static SQL accessor signature shared by native and Wamn projections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StaticSqlStatementDeclaration {
    pub path: String,
    pub fetch: StaticSqlFetch,
    #[serde(default)]
    pub parameters: Vec<StaticSqlValueDeclaration>,
    pub row: Vec<StaticSqlValueDeclaration>,
}

/// One named SQL parameter or result member.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StaticSqlValueDeclaration {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: wamn_schema_introspection::ir::ColumnType,
    pub nullable: bool,
}

/// Static SQL result cardinality used by generated accessors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StaticSqlFetch {
    One,
    OptionalOne,
    BoundedList,
}

impl PackageManifest {
    /// Parse one complete manifest, refusing unknown fields at every level.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, GenerateError> {
        let manifest: Self = serde_json::from_slice(bytes).map_err(|source| {
            GenerateError::with_source(
                GenerateErrorKind::InvalidManifest,
                "wamn.json does not match the closed manifest vocabulary",
                source,
            )
        })?;
        if manifest
            .package
            .predecessor_version
            .as_deref()
            .is_some_and(str::is_empty)
        {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidManifest,
                "package predecessor version must not be empty",
            ));
        }
        if manifest.package.predecessor_version.as_deref()
            == Some(manifest.package.version.as_str())
        {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidManifest,
                "package predecessor version must differ from the package version",
            ));
        }
        Ok(manifest)
    }
}

/// Validate and return the manifest's exact package-local operation vocabulary.
///
/// This is the shared semantic authority for generation and production grant
/// reconciliation. The package and local operation identities obey the naming
/// law, every operation names itself as its permission token, and every
/// declared operation belongs to exactly one component artifact. A sole
/// component is the implicit package default; multi-component manifests name
/// every membership explicitly. Empty, unknown, unused, or requirement-
/// identical component groups refuse.
pub fn validate_operation_vocabulary(
    manifest: &PackageManifest,
) -> Result<BTreeSet<String>, GenerateError> {
    validate_package_identity(&manifest.package)?;
    validate_base_dependencies(manifest)?;
    validate_internal_relation_vocabulary(manifest)?;

    let mut declared = BTreeSet::new();
    let mut component_by_operation = BTreeMap::new();
    let mut artifact_owners = BTreeMap::new();
    for (model_name, model) in &manifest.models {
        validate_identifier(model_name, "operation module")?;
        artifact_owners.insert(model_name.clone(), format!("model {model_name}"));
        for (action, operation) in &model.operations {
            let identity = format!("{model_name}.{}", action.as_str());
            if operation.permission != identity {
                return Err(GenerateError::new(
                    GenerateErrorKind::InvalidOperation,
                    format!(
                        "{identity} permission must equal its package-local operation identity"
                    ),
                ));
            }
            if !declared.insert(identity.clone()) {
                return Err(GenerateError::new(
                    GenerateErrorKind::InvalidOperation,
                    format!("manifest repeats declared operation {identity}"),
                ));
            }
            component_by_operation.insert(identity.clone(), operation.component.as_deref());
            validate_access_error_details(&identity, *action, &operation.error_details)?;
        }
    }
    for (operation_name, operation) in &manifest.custom_operations {
        validate_operation_identity(operation_name)?;
        let artifact = custom_artifact_stem(operation_name);
        if let Some(existing) = artifact_owners.insert(
            artifact.clone(),
            format!("custom operation {operation_name}"),
        ) {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidOperation,
                format!(
                    "{existing} and custom operation {operation_name} collide at generated/source-map/{artifact}.json"
                ),
            ));
        }
        validate_custom_operation(manifest, operation_name, operation)?;
        if !declared.insert(operation_name.clone()) {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidOperation,
                format!("manifest repeats declared operation {operation_name}"),
            ));
        }
        component_by_operation.insert(operation_name.clone(), operation.component());
    }

    validate_component_groups(manifest, &component_by_operation)?;
    Ok(declared)
}

fn validate_internal_relation_vocabulary(manifest: &PackageManifest) -> Result<(), GenerateError> {
    let mut coordinates = BTreeMap::<(String, String), String>::new();
    for (model_id, model) in &manifest.models {
        if CONTROL_OWNED_RELATION_TABLES.contains(&model.table.as_str()) {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidManifest,
                format!(
                    "model {model_id} uses reserved control relation {}.{}",
                    model.schema, model.table
                ),
            ));
        }
        if let Some(existing) = coordinates.insert(
            (model.schema.clone(), model.table.clone()),
            format!("model {model_id}"),
        ) {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidManifest,
                format!(
                    "{existing} and model {model_id} classify the same relation {}.{}",
                    model.schema, model.table
                ),
            ));
        }
    }
    for (relation_id, relation) in &manifest.internal_relations {
        validate_identifier(relation_id, "internal relation")?;
        validate_identifier(&relation.schema, "internal relation schema")?;
        validate_identifier(&relation.table, "internal relation table")?;
        if CONTROL_OWNED_RELATION_TABLES.contains(&relation.table.as_str()) {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidManifest,
                format!(
                    "internal relation {relation_id} uses reserved control relation {}.{}",
                    relation.schema, relation.table
                ),
            ));
        }
        if manifest.models.contains_key(relation_id) {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidManifest,
                format!("relation {relation_id} cannot be both a model and CDC-excluded"),
            ));
        }
        if let Some(existing) = coordinates.insert(
            (relation.schema.clone(), relation.table.clone()),
            format!("CDC-excluded relation {relation_id}"),
        ) {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidManifest,
                format!(
                    "{existing} and CDC-excluded relation {relation_id} classify the same relation {}.{}",
                    relation.schema, relation.table
                ),
            ));
        }
    }
    Ok(())
}

fn validate_component_groups(
    manifest: &PackageManifest,
    component_by_operation: &BTreeMap<String, Option<&str>>,
) -> Result<(), GenerateError> {
    if manifest.components.is_empty() {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidComponent,
            "manifest declares no component requirements",
        ));
    }

    let mut requirement_sets = BTreeMap::<BTreeSet<&str>, &str>::new();
    for (name, component) in &manifest.components {
        if name.is_empty() {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidComponent,
                "component name must not be empty",
            ));
        }
        validate_identifier(name, "component")?;
        if component.connections.is_empty() {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidComponent,
                format!("{name} must declare connections"),
            ));
        }
        let requirements = component
            .connections
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        if requirements.len() != component.connections.len() {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidComponent,
                format!("{name} repeats a connection requirement"),
            ));
        }
        for connection in &requirements {
            if !manifest.connections.contains_key(*connection) {
                return Err(GenerateError::new(
                    GenerateErrorKind::InvalidComponent,
                    format!("{name} references unknown connection {connection}"),
                ));
            }
        }
        if let Some(existing) = requirement_sets.insert(requirements, name) {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidComponent,
                format!("components {existing} and {name} have identical requirement sets"),
            ));
        }
    }

    let implicit = (manifest.components.len() == 1)
        .then(|| manifest.components.keys().next().expect("one component"));
    let mut grouped = manifest
        .components
        .keys()
        .map(|name| (name.as_str(), 0_usize))
        .collect::<BTreeMap<_, _>>();
    for (operation, requested) in component_by_operation {
        let component = match requested {
            Some("") => {
                return Err(GenerateError::new(
                    GenerateErrorKind::InvalidComponent,
                    format!("operation {operation} component must not be empty"),
                ));
            }
            Some(name) => {
                validate_identifier(name, "component")?;
                *name
            }
            None => implicit.map(String::as_str).ok_or_else(|| {
                GenerateError::new(
                    GenerateErrorKind::InvalidComponent,
                    format!(
                        "operation {operation} must name a component when the manifest declares multiple components"
                    ),
                )
            })?,
        };
        let Some(count) = grouped.get_mut(component) else {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidComponent,
                format!("operation {operation} references unknown component {component}"),
            ));
        };
        *count += 1;
    }
    if let Some((component, _)) = grouped.iter().find(|(_, count)| **count == 0) {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidComponent,
            format!("component {component} groups no operations"),
        ));
    }
    Ok(())
}

fn validate_custom_operation(
    manifest: &PackageManifest,
    operation_name: &str,
    operation: &CustomOperationDeclaration,
) -> Result<(), GenerateError> {
    match (operation.visibility(), operation.permission()) {
        (OperationVisibility::Public, Some(permission)) if permission == operation_name => {}
        (OperationVisibility::Public, _) => {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidOperation,
                format!(
                    "public operation {operation_name} permission must equal its package-local identity"
                ),
            ));
        }
        (OperationVisibility::Private, None) => {}
        (OperationVisibility::Private, Some(_)) => {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidOperation,
                format!("private operation {operation_name} must not declare a permission"),
            ));
        }
    }

    validate_custom_operation_kind(manifest, operation_name, operation)?;
    validate_custom_operation_input(operation_name, &operation.input)?;
    if let Some(result) = &operation.result {
        validate_contract_fields(operation_name, "result", &result.fields)?;
    }
    validate_custom_operation_errors(operation_name, operation)?;
    validate_static_sql_declarations(manifest, operation_name, operation)
}

fn validate_custom_operation_kind(
    manifest: &PackageManifest,
    operation_name: &str,
    operation: &CustomOperationDeclaration,
) -> Result<(), GenerateError> {
    match operation.kind {
        CustomOperationKind::Projection => {
            if operation.result.is_none() {
                return Err(GenerateError::new(
                    GenerateErrorKind::InvalidOperation,
                    format!("projection {operation_name} must declare a typed result"),
                ));
            }
            refuse_command_only_fields(operation_name, operation)?;
            if operation.registration.is_some() {
                return Err(GenerateError::new(
                    GenerateErrorKind::InvalidOperation,
                    format!("projection {operation_name} must not declare a registration"),
                ));
            }
            if operation.relations.iter().any(|relation| {
                !relation.insert_fields.is_empty()
                    || !relation.update_fields.is_empty()
                    || relation.lock
            }) {
                return Err(GenerateError::new(
                    GenerateErrorKind::InvalidOperation,
                    format!("projection {operation_name} must be read-only"),
                ));
            }
        }
        CustomOperationKind::Command => {
            if operation.result.is_none() || operation.registration.is_some() {
                return Err(GenerateError::new(
                    GenerateErrorKind::InvalidOperation,
                    format!("command {operation_name} must declare a result and no registration"),
                ));
            }
            let has_local_sql = operation.connection.is_some()
                || !operation.relations.is_empty()
                || !operation.statements.is_empty();
            match (
                has_local_sql,
                operation.transaction,
                operation.automatic_retry,
            ) {
                (true, Some(CommandTransaction::ExplicitPerInput), Some(false))
                | (false, None, None) => {}
                (true, Some(_), Some(true)) => {
                    return Err(GenerateError::new(
                        GenerateErrorKind::InvalidOperation,
                        format!("command {operation_name} must not retry automatically"),
                    ));
                }
                _ => {
                    return Err(GenerateError::new(
                        GenerateErrorKind::InvalidOperation,
                        format!(
                            "command {operation_name} transaction and automatic_retry must be declared together"
                        ),
                    ));
                }
            }
            if !has_local_sql && operation.canonicalization.is_some() {
                return Err(GenerateError::new(
                    GenerateErrorKind::InvalidOperation,
                    format!(
                        "composition-only command {operation_name} must not declare local canonicalization"
                    ),
                ));
            }
            if let Some(canonicalization) = &operation.canonicalization {
                validate_command_canonicalization(operation_name, operation, canonicalization)?;
            }
        }
        CustomOperationKind::EventHandler => {
            if operation.visibility != OperationVisibility::Private || operation.result.is_some() {
                return Err(GenerateError::new(
                    GenerateErrorKind::InvalidOperation,
                    format!(
                        "event handler {operation_name} must be private and have no outward result"
                    ),
                ));
            }
            refuse_command_only_fields(operation_name, operation)?;
            let registration = operation.registration.as_ref().ok_or_else(|| {
                GenerateError::new(
                    GenerateErrorKind::InvalidOperation,
                    format!("event handler {operation_name} must declare a registration"),
                )
            })?;
            validate_registration(manifest, operation_name, registration)?;
        }
    }
    Ok(())
}

fn refuse_command_only_fields(
    operation_name: &str,
    operation: &CustomOperationDeclaration,
) -> Result<(), GenerateError> {
    if operation.transaction.is_some()
        || operation.automatic_retry.is_some()
        || operation.canonicalization.is_some()
    {
        Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("{operation_name} declares command-only transaction or canonicalization"),
        ))
    } else {
        Ok(())
    }
}

fn validate_registration(
    manifest: &PackageManifest,
    operation_name: &str,
    registration: &EventRegistrationDeclaration,
) -> Result<(), GenerateError> {
    validate_identifier(&registration.source_package, "event source package")?;
    validate_identifier(&registration.entity, "event entity")?;
    if registration.ops.is_empty() {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("event handler {operation_name} registration must declare at least one op"),
        ));
    }
    for (index, op) in registration.ops.iter().enumerate() {
        if registration.ops[..index].contains(op) {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidOperation,
                format!(
                    "event handler {operation_name} registration repeats op {:?}",
                    op.as_str()
                ),
            ));
        }
    }
    let source_is_declared = registration.source_package == manifest.package.id
        || manifest
            .base_dependencies
            .values()
            .any(|dependency| dependency.package == registration.source_package);
    if source_is_declared {
        Ok(())
    } else {
        Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!(
                "event handler {operation_name} source package {} is not installed by the manifest",
                registration.source_package
            ),
        ))
    }
}

fn validate_custom_operation_input(
    operation_name: &str,
    input: &CustomOperationInputDeclaration,
) -> Result<(), GenerateError> {
    validate_contract_fields(operation_name, "input", &input.fields)?;
    let envelope_fields = [
        input.raw_body_maximum.is_some(),
        input.envelope.is_some(),
        input.item_semantics.is_some(),
    ];
    if envelope_fields.iter().any(|present| *present)
        && !envelope_fields.iter().all(|present| *present)
    {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("{operation_name} public-envelope bounds must be declared as one complete set"),
        ));
    }
    if input.raw_body_maximum == Some(0) {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("{operation_name} raw body maximum must be positive"),
        ));
    }
    for (name, limit) in [("envelope", &input.envelope), ("line", &input.line)] {
        if let Some(limit) = limit {
            if limit.minimum == 0 || limit.maximum < limit.minimum {
                return Err(GenerateError::new(
                    GenerateErrorKind::InvalidOperation,
                    format!("{operation_name} {name} bounds must be a positive closed interval"),
                ));
            }
        }
    }
    Ok(())
}

fn validate_contract_fields(
    operation_name: &str,
    contract: &str,
    fields: &[ContractFieldDeclaration],
) -> Result<(), GenerateError> {
    if fields.is_empty() {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("{operation_name} {contract} must declare at least one typed field"),
        ));
    }
    let mut paths = BTreeSet::new();
    for field in fields {
        validate_contract_path(&field.path, operation_name)?;
        if !paths.insert(field.path.as_str()) {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidOperation,
                format!("{operation_name} {contract} repeats field {}", field.path),
            ));
        }
        if !field.values.is_empty() {
            if field.ty != wamn_schema_introspection::ir::ColumnType::Text
                || field.values.iter().any(String::is_empty)
                || field.values.iter().collect::<BTreeSet<_>>().len() != field.values.len()
            {
                return Err(GenerateError::new(
                    GenerateErrorKind::InvalidOperation,
                    format!(
                        "{operation_name} {contract} field {} has invalid closed text values",
                        field.path
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn validate_contract_path(path: &str, operation_name: &str) -> Result<(), GenerateError> {
    let valid = !path.is_empty()
        && path.split('.').all(|segment| {
            let name = segment.strip_suffix("[]").unwrap_or(segment);
            !name.is_empty() && validate_identifier(name, "contract field").is_ok()
        });
    if valid {
        Ok(())
    } else {
        Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("{operation_name} has invalid contract field path {path}"),
        ))
    }
}

fn validate_command_canonicalization(
    operation_name: &str,
    operation: &CustomOperationDeclaration,
    canonicalization: &CommandCanonicalization,
) -> Result<(), GenerateError> {
    let has_ordering_key = operation.input.fields.iter().any(|field| {
        field.path.ends_with("line[].purchase_order_line_id")
            && field.ty == wamn_schema_introspection::ir::ColumnType::Uuid
            && !field.nullable
    });
    let has_positive_quantity = operation.input.fields.iter().any(|field| {
        field.path.ends_with("line[].quantity")
            && field.ty == wamn_schema_introspection::ir::ColumnType::Numeric
            && !field.nullable
    });
    if operation.input.line.is_none() || !has_ordering_key || !has_positive_quantity {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!(
                "{operation_name} canonical line profile requires non-null UUID purchase_order_line_id and numeric quantity inputs"
            ),
        ));
    }
    let input_paths = operation
        .input
        .fields
        .iter()
        .map(|field| field.path.as_str())
        .collect::<BTreeSet<_>>();
    let excluded = canonicalization
        .excluded_fields
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if excluded.len() != canonicalization.excluded_fields.len()
        || excluded.is_empty()
        || !excluded.is_subset(&input_paths)
    {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("{operation_name} canonicalization must name unique declared input exclusions"),
        ));
    }
    Ok(())
}

fn validate_custom_operation_errors(
    operation_name: &str,
    operation: &CustomOperationDeclaration,
) -> Result<(), GenerateError> {
    let errors = operation
        .errors
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if errors.is_empty() || errors.len() != operation.errors.len() {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("{operation_name} errors must be nonempty and unique"),
        ));
    }
    let has_permission_refusal = errors.contains("permission_denied");
    if has_permission_refusal != (operation.visibility == OperationVisibility::Public) {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!(
                "{operation_name} permission_denied must be present exactly for public visibility"
            ),
        ));
    }
    for error in &operation.errors {
        validate_identifier(error, "custom-operation error")?;
    }
    if operation
        .error_details
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>()
        != errors
    {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("{operation_name} error details must match its exact error set"),
        ));
    }
    for (error, detail) in &operation.error_details {
        if operation
            .constraint_errors
            .values()
            .any(|mapped| mapped == error)
        {
            validate_detail_keys(
                operation_name,
                error,
                detail,
                &[OperationErrorDetailKey::Constraint],
                &[],
            )?;
        } else if let Some((required, optional)) = shared_custom_error_detail(operation, error) {
            validate_detail_keys(operation_name, error, detail, required, optional)?;
        } else {
            validate_unconstrained_detail_keys(operation_name, error, detail)?;
        }
    }
    let mut mapped_errors = BTreeSet::new();
    for (constraint, error) in &operation.constraint_errors {
        validate_identifier(constraint, "custom-operation constraint")?;
        if !errors.contains(error.as_str()) {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidOperation,
                format!(
                    "{operation_name} constraint {constraint} maps to undeclared error {error}"
                ),
            ));
        }
        if matches!(
            error.as_str(),
            "invalid_input"
                | "not_found"
                | "concurrency_conflict"
                | "idempotency_conflict"
                | "retry"
                | "timeout"
                | "permission_denied"
                | "internal_error"
        ) {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidOperation,
                format!(
                    "{operation_name} constraint {constraint} must not redefine reserved error {error}"
                ),
            ));
        }
        if !mapped_errors.insert(error.as_str()) {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidOperation,
                format!("{operation_name} maps more than one constraint to error {error}"),
            ));
        }
    }
    Ok(())
}

fn shared_custom_error_detail(
    operation: &CustomOperationDeclaration,
    error: &str,
) -> Option<(
    &'static [OperationErrorDetailKey],
    &'static [OperationErrorDetailKey],
)> {
    use OperationErrorDetailKey as Key;

    const NONE: &[Key] = &[];
    const FIELD: &[Key] = &[Key::Field];
    const FIELD_ID: &[Key] = &[Key::Field, Key::Id];
    const BOUNDS: &[Key] = &[Key::Minimum, Key::Maximum, Key::Observed];
    const CONCURRENCY: &[Key] = &[Key::ExpectedRowVersion, Key::ObservedRowVersion];
    const OPERATION: &[Key] = &[Key::Operation];

    match error {
        "invalid_input" => Some((
            FIELD,
            if operation.input.envelope.is_some() || operation.input.line.is_some() {
                BOUNDS
            } else {
                NONE
            },
        )),
        "not_found" => Some((FIELD_ID, NONE)),
        "concurrency_conflict" => Some((CONCURRENCY, NONE)),
        "permission_denied" => Some((OPERATION, NONE)),
        "retry" | "timeout" | "internal_error" => Some((NONE, NONE)),
        _ => None,
    }
}

fn validate_unconstrained_detail_keys(
    operation: &str,
    code: &str,
    detail: &OperationErrorDetailDeclaration,
) -> Result<(), GenerateError> {
    let required = detail.required.iter().copied().collect::<BTreeSet<_>>();
    let optional = detail.optional.iter().copied().collect::<BTreeSet<_>>();
    if required.len() == detail.required.len()
        && optional.len() == detail.optional.len()
        && required.is_disjoint(&optional)
    {
        Ok(())
    } else {
        Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("{operation} error {code} repeats or conflicts structured-detail keys"),
        ))
    }
}

fn validate_static_sql_declarations(
    manifest: &PackageManifest,
    operation_name: &str,
    operation: &CustomOperationDeclaration,
) -> Result<(), GenerateError> {
    let has_connection = operation.connection.is_some();
    let has_relations = !operation.relations.is_empty();
    let has_statements = !operation.statements.is_empty();
    let matching_dependencies = manifest
        .base_dependencies
        .values()
        .filter(|dependency| {
            dependency
                .operations
                .iter()
                .any(|candidate| candidate == operation_name)
        })
        .count();
    if matching_dependencies > 1 {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!(
                "{operation_name} is ambiguous across {matching_dependencies} base dependencies"
            ),
        ));
    }
    if !has_connection && !has_relations && !has_statements {
        if operation.kind == CustomOperationKind::Command && matching_dependencies == 1 {
            return Ok(());
        }
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!(
                "{operation_name} must declare local static SQL or an exact same-operation dependency composition"
            ),
        ));
    }
    if !(has_connection && has_relations && has_statements) {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!(
                "{operation_name} local SQL connection, relations, and statements must be declared together"
            ),
        ));
    }
    let connection = operation
        .connection
        .as_deref()
        .expect("complete local SQL shape has a connection");
    validate_identifier(connection, "custom-operation connection")?;
    if !manifest.connections.contains_key(connection) {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("{operation_name} references unknown connection {connection}"),
        ));
    }
    let mut relations = BTreeSet::new();
    for relation in &operation.relations {
        validate_identifier(&relation.schema, "static SQL relation schema")?;
        validate_identifier(&relation.table, "static SQL relation table")?;
        if CONTROL_OWNED_RELATION_TABLES.contains(&relation.table.as_str()) {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidOperation,
                format!(
                    "{operation_name} references reserved control relation {}.{}",
                    relation.schema, relation.table
                ),
            ));
        }
        if !relations.insert((relation.schema.as_str(), relation.table.as_str())) {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidOperation,
                format!(
                    "{operation_name} repeats relation {}.{}",
                    relation.schema, relation.table
                ),
            ));
        }
        for (access, fields) in [
            ("select", &relation.select_fields),
            ("insert", &relation.insert_fields),
            ("update", &relation.update_fields),
        ] {
            validate_named_values(operation_name, access, fields)?;
        }
        validate_named_values(operation_name, "constraint", &relation.constraints)?;
        if relation.select_fields.is_empty()
            && relation.insert_fields.is_empty()
            && relation.update_fields.is_empty()
            && !relation.lock
        {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidOperation,
                format!(
                    "{operation_name} relation {}.{} declares no SQL access",
                    relation.schema, relation.table
                ),
            ));
        }
    }
    let mut statement_paths = BTreeSet::new();
    let mut row_symbols = BTreeMap::new();
    let mut fixture_symbols = BTreeMap::new();
    for (name, statement) in &operation.statements {
        validate_identifier(name, "static SQL statement")?;
        if rust_identifier(name).is_none() {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidIdentity,
                format!("static SQL statement `{name}` is not a usable Rust identifier"),
            ));
        }
        let row_symbol = format!("{}Row", rust_type_identifier(name));
        if let Some(existing) = row_symbols.insert(row_symbol.clone(), name.as_str()) {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidOperation,
                format!(
                    "{operation_name} statements {existing} and {name} collide at Rust row {row_symbol}"
                ),
            ));
        }
        if !statement_paths.insert(statement.path.as_str()) {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidOperation,
                format!(
                    "{operation_name} repeats static SQL path {}",
                    statement.path
                ),
            ));
        }
        validate_static_sql_values(operation_name, name, "parameter", &statement.parameters)?;
        validate_static_sql_values(operation_name, name, "row", &statement.row)?;
        for parameter in &statement.parameters {
            let fixture = format!("{name}_{}_bind_fixture", parameter.name);
            if let Some(existing) =
                fixture_symbols.insert(fixture.clone(), format!("{name}.{}", parameter.name))
            {
                return Err(GenerateError::new(
                    GenerateErrorKind::InvalidOperation,
                    format!(
                        "{operation_name} parameter pairs {existing} and {name}.{} collide at Rust fixture {fixture}",
                        parameter.name
                    ),
                ));
            }
        }
        if statement.row.is_empty() {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidOperation,
                format!("{operation_name}.{name} must declare a typed result row"),
            ));
        }
    }
    Ok(())
}

fn validate_named_values(
    operation: &str,
    kind: &str,
    values: &[String],
) -> Result<(), GenerateError> {
    let unique = values.iter().collect::<BTreeSet<_>>();
    if unique.len() != values.len() {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("{operation} repeats a declared {kind} name"),
        ));
    }
    for value in values {
        validate_identifier(value, kind)?;
    }
    Ok(())
}

fn validate_static_sql_values(
    operation: &str,
    statement: &str,
    kind: &str,
    values: &[StaticSqlValueDeclaration],
) -> Result<(), GenerateError> {
    let mut names = BTreeSet::new();
    for value in values {
        validate_identifier(&value.name, kind)?;
        if rust_identifier(&value.name).is_none() {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidIdentity,
                format!(
                    "{operation}.{statement} {kind} `{}` is not a usable Rust identifier",
                    value.name
                ),
            ));
        }
        if !names.insert(value.name.as_str()) {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidOperation,
                format!("{operation}.{statement} repeats {kind} {}", value.name),
            ));
        }
    }
    Ok(())
}

fn validate_access_error_details(
    operation: &str,
    action: CrudAction,
    details: &BTreeMap<AccessOperationErrorLiteral, OperationErrorDetailDeclaration>,
) -> Result<(), GenerateError> {
    use AccessOperationErrorLiteral as Code;
    use OperationErrorDetailKey as Key;

    let mut expected = BTreeSet::from([
        Code::InvalidInput,
        Code::Retry,
        Code::Timeout,
        Code::PermissionDenied,
        Code::InternalError,
    ]);
    if matches!(
        action,
        CrudAction::Get | CrudAction::Update | CrudAction::Delete
    ) {
        expected.insert(Code::NotFound);
    }
    if matches!(action, CrudAction::Update | CrudAction::Delete) {
        expected.insert(Code::ConcurrencyConflict);
    }
    for constraint in [
        Code::UniqueViolation,
        Code::ForeignKeyViolation,
        Code::CheckViolation,
    ] {
        if details.contains_key(&constraint) {
            expected.insert(constraint);
        }
    }
    if details.keys().copied().collect::<BTreeSet<_>>() != expected {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("{operation} must declare its exact closed error-detail code set"),
        ));
    }

    for (code, detail) in details {
        let (required, optional): (&[Key], &[Key]) = match code {
            Code::InvalidInput if action == CrudAction::Query => {
                (&[Key::Field], &[Key::Minimum, Key::Maximum, Key::Observed])
            }
            Code::InvalidInput => (&[Key::Field], &[]),
            Code::NotFound => (&[Key::Field, Key::Id], &[]),
            Code::ConcurrencyConflict => (&[Key::ExpectedRowVersion, Key::ObservedRowVersion], &[]),
            Code::UniqueViolation | Code::ForeignKeyViolation | Code::CheckViolation => {
                (&[Key::Constraint], &[])
            }
            Code::PermissionDenied => (&[Key::Operation], &[]),
            Code::Retry | Code::Timeout | Code::InternalError => (&[], &[]),
        };
        validate_detail_keys(operation, code.as_str(), detail, required, optional)?;
    }
    Ok(())
}

fn validate_detail_keys(
    operation: &str,
    code: &str,
    detail: &OperationErrorDetailDeclaration,
    required: &[OperationErrorDetailKey],
    optional: &[OperationErrorDetailKey],
) -> Result<(), GenerateError> {
    let actual_required = detail.required.iter().copied().collect::<BTreeSet<_>>();
    let actual_optional = detail.optional.iter().copied().collect::<BTreeSet<_>>();
    let valid = actual_required.len() == detail.required.len()
        && actual_optional.len() == detail.optional.len()
        && actual_required.is_disjoint(&actual_optional)
        && actual_required == required.iter().copied().collect()
        && actual_optional == optional.iter().copied().collect();
    if valid {
        Ok(())
    } else {
        Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("{operation} error {code} must declare its exact structured-detail keys"),
        ))
    }
}

/// Construct the one canonical package-qualified identity for a local operation.
pub fn canonical_operation_identity(
    package: &PackageIdentity,
    local_operation: &str,
) -> Result<String, GenerateError> {
    validate_operation_identity(local_operation)?;
    let (module, operation) = local_operation
        .split_once('.')
        .expect("validated operation identity has one separator");
    Ok(format!(
        "{}{}/{}@{}",
        canonical_operation_prefix(package)?,
        module.replace('_', "-"),
        operation.replace('_', "-"),
        package.version,
    ))
}

/// Construct the native extern-name prefix owned by one package.
pub fn canonical_operation_prefix(package: &PackageIdentity) -> Result<String, GenerateError> {
    validate_package_identity(package)?;
    Ok(format!("{}:", package.id.replace('_', "-")))
}

fn validate_package_identity(package: &PackageIdentity) -> Result<(), GenerateError> {
    validate_package_coordinate(&package.id, &package.version)
}

fn validate_package_coordinate(package: &str, version: &str) -> Result<(), GenerateError> {
    validate_identifier(package, "package id")?;
    if version.is_empty()
        || version.trim() != version
        || version.as_bytes().contains(&0)
        || version.contains('@')
        || version.contains(':')
        || version.contains('/')
    {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidIdentity,
            "package version must be canonical text without native operation-token separators",
        ));
    }
    Ok(())
}

fn validate_base_dependencies(manifest: &PackageManifest) -> Result<(), GenerateError> {
    let mut packages = BTreeSet::new();
    for (alias, requirement) in &manifest.base_dependencies {
        validate_identifier(alias, "base dependency alias")?;
        validate_package_coordinate(&requirement.package, &requirement.version)?;
        if requirement.package == manifest.package.id {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidIdentity,
                format!("base dependency {alias} must not name the owning package"),
            ));
        }
        if !packages.insert(requirement.package.as_str()) {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidIdentity,
                format!(
                    "base package {} has more than one alias",
                    requirement.package
                ),
            ));
        }
        if requirement.version.bytes().any(|byte| {
            byte.is_ascii_whitespace()
                || matches!(byte, b'*' | b'^' | b'~' | b'<' | b'>' | b'=' | b',' | b'|')
        }) {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidIdentity,
                format!("base dependency {alias} version must be exact, not a range"),
            ));
        }
        let Some(digest) = requirement.digest.strip_prefix("sha256:") else {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidIdentity,
                format!("base dependency {alias} digest must be lowercase sha256"),
            ));
        };
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidIdentity,
                format!("base dependency {alias} digest must be lowercase sha256"),
            ));
        }
        if requirement.operations.is_empty() {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidOperation,
                format!("base dependency {alias} must require at least one operation"),
            ));
        }
        let mut operations = BTreeSet::new();
        for operation in &requirement.operations {
            validate_operation_identity(operation)?;
            if !operations.insert(operation) {
                return Err(GenerateError::new(
                    GenerateErrorKind::InvalidOperation,
                    format!("base dependency {alias} repeats operation {operation}"),
                ));
            }
        }
    }
    Ok(())
}

fn validate_operation_identity(value: &str) -> Result<(), GenerateError> {
    let Some((module, operation)) = value.split_once('.') else {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidIdentity,
            format!("operation `{value}` must have canonical module.operation form"),
        ));
    };
    if operation.contains('.') {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidIdentity,
            format!("operation `{value}` must contain exactly one module separator"),
        ));
    }
    validate_identifier(module, "operation module")?;
    validate_identifier(operation, "operation name")
}

pub(crate) fn validate_identifier(value: &str, object: &str) -> Result<(), GenerateError> {
    let mut bytes = value.bytes();
    let valid_start = bytes.next().is_some_and(|byte| byte.is_ascii_lowercase());
    let valid_tail =
        bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
    if valid_start && valid_tail && !value.ends_with('_') && !value.contains("__") {
        Ok(())
    } else {
        Err(GenerateError::new(
            GenerateErrorKind::InvalidIdentity,
            format!("{object} `{value}` must be singular snake_case"),
        ))
    }
}

/// Spell one singular snake_case name as a Rust 2024 identifier.
///
/// Raw-ineligible path keywords have no lossless field spelling and refuse at
/// manifest or catalog validation instead of producing uncompilable source.
pub(crate) fn rust_identifier(value: &str) -> Option<String> {
    const RAW_INELIGIBLE: [&str; 3] = ["crate", "self", "super"];
    const KEYWORDS: [&str; 49] = [
        "abstract", "as", "async", "await", "become", "box", "break", "const", "continue", "do",
        "dyn", "else", "enum", "extern", "false", "final", "fn", "for", "gen", "if", "impl", "in",
        "let", "loop", "macro", "match", "mod", "move", "mut", "override", "priv", "pub", "ref",
        "return", "static", "struct", "trait", "true", "try", "type", "typeof", "union", "unsafe",
        "unsized", "use", "virtual", "where", "while", "yield",
    ];
    if RAW_INELIGIBLE.contains(&value) {
        None
    } else if KEYWORDS.contains(&value) {
        Some(format!("r#{value}"))
    } else {
        Some(value.to_owned())
    }
}

pub(crate) fn custom_artifact_stem(operation: &str) -> String {
    operation.replace('.', "_")
}

pub(crate) fn rust_type_identifier(value: &str) -> String {
    value
        .split('_')
        .map(|part| {
            let mut characters = part.chars();
            match characters.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + characters.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::rust_identifier;

    #[test]
    fn rust_2024_names_have_one_lossless_spelling_authority() {
        for keyword in ["type", "async", "await", "move"] {
            assert_eq!(rust_identifier(keyword), Some(format!("r#{keyword}")));
        }
        for raw_ineligible in ["crate", "self", "super"] {
            assert_eq!(rust_identifier(raw_ineligible), None);
        }
        assert_eq!(rust_identifier("receipt_id"), Some("receipt_id".to_owned()));
    }
}

/// Immutable package identity and sole operation-version coordinate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageIdentity {
    pub id: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predecessor_version: Option<String>,
}

/// Exact package artifact and local operation set bound to one source alias.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaseDependencyRequirement {
    pub package: String,
    pub version: String,
    pub digest: String,
    pub operations: Vec<String>,
}

/// Platform policy contract required before package promotion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyContractRequirement {
    pub id: String,
    pub state: PolicyContractState,
}

/// Slice-ii policy requirements remain explicitly unsatisfied until wiring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyContractState {
    Unsatisfied,
    Satisfied,
}

/// Behavior attached to one introspected relation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelDeclaration {
    pub schema: String,
    pub table: String,
    pub owner: String,
    #[serde(default)]
    pub client_field_extensible: bool,
    #[serde(default)]
    pub field_owners: BTreeMap<String, String>,
    #[serde(default)]
    pub constraint_owners: BTreeMap<String, String>,
    #[serde(default)]
    pub server_owned_fields: Vec<String>,
    #[serde(default)]
    pub enum_fields: BTreeMap<String, Vec<String>>,
    pub operations: BTreeMap<CrudAction, OperationDeclaration>,
}

/// Package-owned mechanism state that must never enter the CDC event plane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InternalRelationDeclaration {
    pub schema: String,
    pub table: String,
    pub cdc: CdcDisposition,
}

/// Closed CDC disposition for a package-owned internal relation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CdcDisposition {
    Excluded,
}

impl ModelDeclaration {
    /// Definition owner for one field, inheriting the relation owner when omitted.
    pub fn field_owner(&self, field: &str) -> &str {
        self.field_owners
            .get(field)
            .map_or(self.owner.as_str(), String::as_str)
    }

    /// Definition owner for one constraint, inheriting the relation owner when omitted.
    pub fn constraint_owner(&self, constraint: &str) -> &str {
        self.constraint_owners
            .get(constraint)
            .map_or(self.owner.as_str(), String::as_str)
    }
}

/// Closed generated CRUD action vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrudAction {
    Get,
    Query,
    Create,
    Update,
    Delete,
}

impl CrudAction {
    /// Canonical local action spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "get",
            Self::Query => "query",
            Self::Create => "create",
            Self::Update => "update",
            Self::Delete => "delete",
        }
    }
}

/// One generated operation's behavior and authority declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationDeclaration {
    pub permission: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<String>,
    pub error_details: BTreeMap<AccessOperationErrorLiteral, OperationErrorDetailDeclaration>,
    #[serde(default)]
    pub authored_sql: Option<AuthoredSqlDeclaration>,
    #[serde(default)]
    pub writable_fields: Vec<String>,
    #[serde(default)]
    pub revision_field: Option<String>,
    #[serde(default)]
    pub filters: Vec<FilterDeclaration>,
    #[serde(default)]
    pub sort: Option<SortDeclaration>,
    #[serde(default)]
    pub pagination: Option<PaginationDeclaration>,
    #[serde(default)]
    pub limit: Option<LimitDeclaration>,
    pub result: ResultClass,
}

/// Whether a registered operation may be bound to an external route.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationVisibility {
    Public,
    Private,
}

/// Package-owned static SQL files for every declared query ordering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthoredSqlDeclaration {
    pub default: String,
    pub variants: Vec<AuthoredSqlVariant>,
}

/// One authored query file selected by a finite field/direction pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthoredSqlVariant {
    pub field: String,
    pub direction: CursorDirection,
    pub path: String,
}

/// Closed result cardinality vocabulary from the POC design.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultClass {
    One,
    OptionalOne,
    Page,
    BoundedList,
}

/// Binding strategy for one query filter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilterDeclaration {
    pub field: String,
    pub binding: FilterBinding,
}

/// Frozen filter binding strategies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterBinding {
    JsonArray,
}

/// Finite query sorting vocabulary with at most one requested field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SortDeclaration {
    pub fields: Vec<String>,
    pub directions: Vec<CursorDirection>,
    pub max_fields: u8,
}

/// Keyset pagination and opaque cursor contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaginationDeclaration {
    pub kind: PaginationKind,
    pub cursor: CursorDeclaration,
    pub default_sort: SortKey,
    pub tie_breaker: TieBreakerDeclaration,
}

/// Supported pagination strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaginationKind {
    Keyset,
}

/// Opaque, versioned wire cursor declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CursorDeclaration {
    pub version: u8,
    pub payload: CursorPayload,
    pub encoding: CursorEncoding,
    pub opaque: bool,
    pub invalid: InputRefusal,
}

/// Canonical payload serialized before cursor encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorPayload {
    CanonicalCompactJson,
}

/// Supported wire cursor encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorEncoding {
    Base64urlUnpadded,
}

/// One field and direction used by deterministic keyset ordering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SortKey {
    pub field: String,
    pub direction: CursorDirection,
}

/// Stable secondary key whose direction inherits the selected primary sort.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TieBreakerDeclaration {
    pub field: String,
}

/// Request limit contract enforced before SQL execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitDeclaration {
    pub default: u32,
    pub minimum: u32,
    pub maximum: u32,
    pub invalid: InputRefusal,
}

/// Typed refusal used for malformed operation inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputRefusal {
    InvalidInput,
}

/// Closed sort direction vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorDirection {
    Ascending,
    Descending,
}

/// One package-required connection capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionDeclaration {
    pub interface: String,
}

/// Import requirements for one package-local component group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentDeclaration {
    pub connections: Vec<String>,
}
