use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use wamn_record_history::{
    HISTORY_TABLE_SUFFIX, NO_LOG_RETENTION, history_object_names_fit, is_history_table_name,
    is_log_retention,
};

use crate::{GenerateError, GenerateErrorKind};

pub(crate) const CONTROL_OWNED_RELATION_TABLES: [&str; 2] =
    ["wamn_entities", "wamn_cdc_exclusions"];

/// Strict package-owned behavior declaration parsed from `wamn.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageManifest {
    pub package: PackageIdentity,
    /// Explicit npm distribution identity for the generated TypeScript.
    ///
    /// A package that omits it generates no TypeScript at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub npm_distribution: Option<NpmDistribution>,
    #[serde(default)]
    pub base_dependencies: BTreeMap<String, BaseDependencyRequirement>,
    pub required_platform_policy_contract: PolicyContractRequirement,
    pub models: BTreeMap<String, ModelDeclaration>,
    #[serde(default)]
    pub internal_relations: BTreeMap<String, InternalRelationDeclaration>,
    #[serde(default)]
    pub custom_operations: BTreeMap<String, CustomOperationDeclaration>,
    pub connections: BTreeSet<String>,
    pub components: BTreeMap<String, ComponentDeclaration>,
}

/// One strict package-local operation backed only by declared static SQL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomOperationDeclaration {
    pub kind: CustomOperationKind,
    pub visibility: OperationVisibility,
    /// Legacy operation metadata; all sessions require current admission authority.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fresh_only: bool,
    #[serde(default)]
    pub permission: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<String>,
    pub input: CustomOperationInputDeclaration,
    /// Base-owned typed input passed through before the command runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_commit: Option<CustomOperationInputDeclaration>,
    /// Package-local execution-only operation selected while composing this command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub participant: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<CustomOperationResultDeclaration>,
    pub errors: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
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
    /// How this command survives a repeat. Required for a command and refused
    /// for every other kind. Nothing about it is inferred.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotent_by: Option<CommandIdempotence>,
    /// The claim relation this command hands out its identities from.
    /// Required under `idempotent_by: claim` and refused under the other two.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim: Option<CustomClaimDeclaration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonicalization: Option<CommandCanonicalization>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registration: Option<EventRegistrationDeclaration>,
    /// Authored screen text for the operation itself. A component exports the
    /// label, and the page that places the component decides where it goes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
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
    Participant,
}

/// How one command survives a repeat, in exactly three declared shapes.
///
/// A command declares this. Generation infers none of it, not even for the
/// composed case, because an inference is what let the pilot command ship with
/// no idempotence at all.
///
/// `Claim` mints identity under an idempotency key, so a replay returns the ids
/// the claim already generated. `State` mints no identity and names the row
/// versions that guard it, so a repeat after success sees a version moved and
/// gets a typed conflict. `Inherited` rides the claim of a base operation it
/// names, so a second claim over that identity never exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandIdempotence {
    Claim,
    State(StateGuardDeclaration),
    Inherited(InheritedClaimDeclaration),
}

/// The row versions that make one command idempotent by state.
///
/// The guard is named, never inferred from a field spelling. A command guarding
/// two rows with one input field passes an inferred check and emits a test that
/// cannot say which row it protects. That is a test asserting less than it
/// appears to.
///
/// `guards` maps each guarded relation to the input field carrying the caller's
/// expected version for it. One version field per relation, by construction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateGuardDeclaration {
    /// Guarded relation to the input field carrying its expected version.
    pub guards: BTreeMap<String, String>,
}

/// The base operation whose claim one composing command rides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InheritedClaimDeclaration {
    /// Base dependency alias declared by this package.
    pub base: String,
    /// Operation under that dependency, whose claim mints the identity.
    pub operation: String,
}

/// The command-claim relation of one authored command.
///
/// Ratified platform law, command-identity-from-claim: any identity a command
/// creates comes from the CLAIM, not from the work. The law was ratified on an
/// authored command, so an authored command carries it exactly as a generated
/// create does.
///
/// `identities` maps every result field the command hands out as a new id to
/// the claim column that pre-generated it. Generation refuses the command
/// unless the `claim` statement returns exactly those claim columns. A replay
/// then finds the claim row and returns the same ids BY CONSTRUCTION.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomClaimDeclaration {
    /// Claim relation, named by one of the operation's declared relations.
    pub table: String,
    /// Result field to the claim column that pre-generates it.
    pub identities: BTreeMap<String, String>,
    /// Statement that mints the claim row and returns every identity.
    pub claim: String,
    /// Statement that reads the durable original back through the claim.
    pub replay: String,
    /// Statement that finishes the claim in the first call's transaction.
    pub finalize: String,
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
    pub line: Option<CountLimitDeclaration>,
    pub fields: Vec<ContractFieldDeclaration>,
}

/// One explicit count bound whose refusal stays at the operation layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CountLimitDeclaration {
    pub minimum: u32,
    pub maximum: u32,
}

/// One typed leaf in an input or result contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractFieldDeclaration {
    pub path: String,
    #[serde(rename = "type")]
    pub ty: wamn_schema_introspection::ir::ColumnType,
    pub nullable: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub revision: bool,
    #[serde(default)]
    pub values: Vec<String>,
    /// Authored screen text. Flattening [`FieldText`] here is not available,
    /// because serde refuses `flatten` beside `deny_unknown_fields`, and the
    /// refusal of a misspelled key is the rule this epic keeps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Authored text that a screen shows, and that an author reads.
///
/// Text for a person, never a name: it changes no path, no wire spelling and
/// no generated identifier. An absent member writes nothing, so a package that
/// authors none keeps the bytes it has today.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldText {
    /// What a control, a column header or a term reads. The default is the
    /// field name with its underscores as spaces, applied by the emitter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// One sentence for an author. It reaches a comment, never a screen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl FieldText {
    /// Whether the author stated neither member.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.label.is_none() && self.description.is_none()
    }

    /// The text of one contract field, as the field object states it.
    #[must_use]
    pub fn of_field(field: &ContractFieldDeclaration) -> Self {
        Self {
            label: field.label.clone(),
            description: field.description.clone(),
        }
    }
}

/// Typed custom-operation result contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomOperationResultDeclaration {
    pub class: ResultClass,
    pub fields: Vec<ContractFieldDeclaration>,
}

/// Application choices for canonical command identity.
///
/// Generated codecs normalize JSON and primitive values before hashing.
/// Exclusions and line ordering retain the command's own identity semantics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandCanonicalization {
    pub excluded_fields: Vec<String>,
    /// How a LINE SET is ordered before hashing, when the command has one.
    ///
    /// Absent means the command carries no lines, and canonicalization covers
    /// the top-level fields alone under the shared canonical-JSON authority.
    /// There is deliberately no enum value meaning "no ordering": that would
    /// be a null wearing a name, and every command would have to pick one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_order: Option<CommandLineOrder>,
}

/// Closed canonicalized line profile implemented by command generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandLineOrder {
    PurchaseOrderLineIdAscending,
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
    IdempotencyConflict,
    UniqueViolation,
    ForeignKeyViolation,
    CheckViolation,
    ExclusionViolation,
    Retry,
    Timeout,
    PermissionDenied,
    InternalError,
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
    /// Whether verified SQL deletes rows from this relation.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub delete: bool,
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
        validate_audit_log(manifest, model_name, model)?;
        validate_delete_mode(manifest, model_name, model)?;
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

/// Refuse a record-history declaration whose shape is invalid without a catalog.
fn validate_audit_log(
    manifest: &PackageManifest,
    model_name: &str,
    model: &ModelDeclaration,
) -> Result<(), GenerateError> {
    let refuse =
        |message: String| Err(GenerateError::new(GenerateErrorKind::InvalidModel, message));
    let Some(audit_log) = &model.audit_log else {
        if model.owner == manifest.package.id {
            return refuse(format!(
                "{model_name} owns its relation and must declare audit_log"
            ));
        }
        return Ok(());
    };
    if model.owner != manifest.package.id {
        return refuse(format!(
            "{model_name} overlays a {} relation and must not declare audit_log",
            model.owner
        ));
    }
    let selected = audit_log.columns.iter().copied().collect::<BTreeSet<_>>();
    if selected.len() != audit_log.columns.len() {
        return refuse(format!("{model_name} audit_log repeats a column"));
    }
    for (actor, time) in [
        (
            RecordHistoryColumn::CreatedBy,
            RecordHistoryColumn::CreatedAt,
        ),
        (
            RecordHistoryColumn::UpdatedBy,
            RecordHistoryColumn::UpdatedAt,
        ),
    ] {
        if selected.contains(&actor) && !selected.contains(&time) {
            return refuse(format!(
                "{model_name} audit_log selects {} without {}",
                actor.as_str(),
                time.as_str()
            ));
        }
    }
    if audit_log.retention != NO_LOG_RETENTION && !is_log_retention(&audit_log.retention) {
        return refuse(format!(
            "{model_name} audit_log retention must be none, unlimited, or P<n>D with a positive whole number of days"
        ));
    }
    if model.log_retention().is_some() && !history_object_names_fit(&model.table) {
        return Err(GenerateError::for_object(
            GenerateErrorKind::InvalidModel,
            format!(
                "{model_name} logs {}.{}, and a history object name of that relation has 64 bytes or more",
                model.schema, model.table
            ),
            format!("{}.{}", model.schema, model.table),
        ));
    }
    Ok(())
}

/// Refuse a delete declaration whose shape is invalid without a catalog.
///
/// The mode and the action travel together, and only the relation owner
/// declares either one. An overlay package cannot delete a base row.
fn validate_delete_mode(
    manifest: &PackageManifest,
    model_name: &str,
    model: &ModelDeclaration,
) -> Result<(), GenerateError> {
    let refuse =
        |message: String| Err(GenerateError::new(GenerateErrorKind::InvalidModel, message));
    let declares_delete = model.operations.contains_key(&CrudAction::Delete);
    if model.owner != manifest.package.id {
        if model.delete_mode.is_some() {
            return refuse(format!(
                "{model_name} overlays a {} relation and must not declare delete_mode",
                model.owner
            ));
        }
        if declares_delete {
            return refuse(format!(
                "{model_name} overlays a {} relation and must not declare delete",
                model.owner
            ));
        }
        return Ok(());
    }
    match (declares_delete, model.delete_mode) {
        (true, None) => refuse(format!("{model_name} declares delete without delete_mode")),
        (false, Some(_)) => refuse(format!("{model_name} declares delete_mode without delete")),
        _ => Ok(()),
    }
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
        if is_history_table_name(&model.table) {
            return Err(reserved_history_name(
                &format!("model {model_id}"),
                &model.schema,
                &model.table,
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
        // A history table takes its table name as its CDC exclusion relation id.
        if is_history_table_name(relation_id) || is_history_table_name(&relation.table) {
            return Err(reserved_history_name(
                &format!("internal relation {relation_id}"),
                &relation.schema,
                &relation.table,
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

/// Refuse an authored name that ends with the reserved history suffix.
fn reserved_history_name(subject: &str, schema: &str, table: &str) -> GenerateError {
    GenerateError::for_object(
        GenerateErrorKind::InvalidManifest,
        format!(
            "{subject} uses {schema}.{table}, but the {HISTORY_TABLE_SUFFIX} suffix is reserved for history tables"
        ),
        format!("{schema}.{table}"),
    )
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
            if !manifest.connections.contains(*connection) {
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
    if operation.fresh_only && operation.visibility != OperationVisibility::Public {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("private operation {operation_name} must not require a fresh credential"),
        ));
    }
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
    if let Some(pre_commit) = &operation.pre_commit {
        if operation.kind != CustomOperationKind::Command
            || operation.claim.is_none()
            || pre_commit.raw_body_maximum.is_some()
            || pre_commit.envelope.is_some()
            || pre_commit.line.is_some()
        {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidOperation,
                format!(
                    "command {operation_name} pre_commit requires a claim and typed fields without envelope or line bounds"
                ),
            ));
        }
        validate_custom_operation_input(operation_name, pre_commit)?;
    }
    if let Some(result) = &operation.result {
        // No paging contract exists for a custom operation. Page belongs to
        // the generated query, which checks its cursor, limit and envelope.
        if result.class == ResultClass::Page {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidOperation,
                format!(
                    "{} {operation_name} must not declare result class page; page belongs to the generated query",
                    operation.kind()
                ),
            ));
        }
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
                    || relation.delete
                    || relation.lock
            }) {
                return Err(GenerateError::new(
                    GenerateErrorKind::InvalidOperation,
                    format!("projection {operation_name} must be read-only"),
                ));
            }
        }
        CustomOperationKind::Command => {
            let participant = operation.transaction == Some(CommandTransaction::Participant);
            if (!participant && operation.result.is_none())
                || (participant && operation.result.is_some())
                || operation.registration.is_some()
            {
                return Err(GenerateError::new(
                    GenerateErrorKind::InvalidOperation,
                    format!(
                        "command {operation_name} must declare a result unless it is an execution-only participant, and must declare no registration"
                    ),
                ));
            }
            validate_command_idempotence(manifest, operation_name, operation)?;
            if participant
                && (!matches!(
                    operation.idempotent_by,
                    Some(CommandIdempotence::Inherited(_))
                ) || operation.claim.is_some()
                    || operation.automatic_retry.is_some()
                    || operation.visibility != OperationVisibility::Public
                    || operation.input.raw_body_maximum.is_some()
                    || operation.input.envelope.is_some()
                    || operation.input.line.is_some())
            {
                return Err(GenerateError::new(
                    GenerateErrorKind::InvalidOperation,
                    format!(
                        "participant command {operation_name} must be public with its own permission, plain typed input, inherited idempotence, and no result, claim or automatic_retry"
                    ),
                ));
            }
            if operation.claim.is_some()
                && operation.transaction != Some(CommandTransaction::ExplicitPerInput)
            {
                return Err(GenerateError::new(
                    GenerateErrorKind::InvalidOperation,
                    format!(
                        "command {operation_name} has a claim; declare transaction: explicit_per_input and automatic_retry: false so the claim and its work commit together"
                    ),
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
                | (true, Some(CommandTransaction::Participant), None)
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
            validate_participant_reference(manifest, operation_name, operation)?;
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

fn validate_participant_reference(
    manifest: &PackageManifest,
    operation_name: &str,
    operation: &CustomOperationDeclaration,
) -> Result<(), GenerateError> {
    let Some(participant_name) = &operation.participant else {
        return Ok(());
    };
    if operation.transaction.is_some()
        || operation.connection.is_some()
        || !operation.relations.is_empty()
        || !operation.statements.is_empty()
    {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("composing command {operation_name} with a participant must not own local SQL"),
        ));
    }
    let Some(CommandIdempotence::Inherited(wrapper_inherited)) = &operation.idempotent_by else {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!(
                "composing command {operation_name} with a participant must inherit its base claim"
            ),
        ));
    };
    let participant = manifest
        .custom_operations
        .get(participant_name)
        .ok_or_else(|| {
            GenerateError::new(
                GenerateErrorKind::InvalidOperation,
                format!("command {operation_name} names unknown participant {participant_name}"),
            )
        })?;
    if participant.transaction != Some(CommandTransaction::Participant) {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!(
                "command {operation_name} participant {participant_name} is not execution-only"
            ),
        ));
    }
    if participant.idempotent_by.as_ref()
        != Some(&CommandIdempotence::Inherited(wrapper_inherited.clone()))
    {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!(
                "command {operation_name} and participant {participant_name} must inherit the same base operation"
            ),
        ));
    }
    Ok(())
}

/// The optimistic-concurrency guard one state-idempotent command declares.
const EXPECTED_REVISION_FIELD: &str = "expected_row_version";

/// Refuse a command that never says how it survives a repeat.
///
/// Ratified platform law, command-identity-from-claim: every id a command
/// returns comes from the claim row its idempotency key pins. A command that
/// says nothing reruns its work on a repeat. It then returns whatever the rows
/// hold at that moment, not what the original call returned.
///
/// Three shapes carry that law, and a command names the one it has. Nothing
/// here is inferred from the operation's other declarations. An inference is
/// what let a pilot command ship with no idempotence and no test.
fn validate_command_idempotence(
    manifest: &PackageManifest,
    operation_name: &str,
    operation: &CustomOperationDeclaration,
) -> Result<(), GenerateError> {
    let Some(idempotent_by) = &operation.idempotent_by else {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!(
                "command {operation_name} must declare idempotent_by as exactly one of three. \
                 \"claim\": add a CDC-excluded internal relation keyed by idempotency_key text \
                 with a canonical_command bytea beside it and one unique non-null uuid column \
                 defaulting to gen_random_uuid() for each identity the command returns, declare \
                 that relation under the operation, then declare \"claim\": {{\"table\", \
                 \"identities\", \"claim\", \"replay\", \"finalize\"}} naming it, the result field \
                 each claim column pre-generates, and the three statements that mint, replay and \
                 finish the claim. {{\"state\": {{\"guards\"}}}}: for a command that mints no \
                 identity, so declare no claim, insert no row, and map each guarded relation to \
                 the input field carrying its expected version, such as \
                 {EXPECTED_REVISION_FIELD}. {{\"inherited\": {{\"base\", \"operation\"}}}}: for a \
                 command that rides the claim of a base operation, naming the base dependency \
                 alias and the operation under it"
            ),
        ));
    };
    match idempotent_by {
        CommandIdempotence::Claim => {
            if operation.claim.is_none() {
                return Err(GenerateError::new(
                    GenerateErrorKind::InvalidOperation,
                    format!("command {operation_name} is idempotent by claim and declares none"),
                ));
            }
        }
        CommandIdempotence::State(state) => {
            refuse_minted_identity(operation_name, operation, "state")?;
            validate_state_guards(operation_name, operation, state)?;
        }
        CommandIdempotence::Inherited(inherited) => {
            if operation.transaction != Some(CommandTransaction::Participant) {
                refuse_minted_identity(operation_name, operation, "inherited")?;
            } else if operation.claim.is_some() {
                return Err(GenerateError::new(
                    GenerateErrorKind::InvalidOperation,
                    format!("participant command {operation_name} must not declare a claim"),
                ));
            }
            let dependency = manifest.base_dependencies.get(&inherited.base).ok_or_else(|| {
                GenerateError::new(
                    GenerateErrorKind::InvalidOperation,
                    format!(
                        "command {operation_name} inherits a claim from undeclared base dependency {}",
                        inherited.base
                    ),
                )
            })?;
            if !dependency.operations.contains(&inherited.operation) {
                return Err(GenerateError::new(
                    GenerateErrorKind::InvalidOperation,
                    format!(
                        "command {operation_name} inherits a claim from {}, which base dependency {} does not declare",
                        inherited.operation, inherited.base
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// Check that every row version a state-idempotent command relies on is named.
///
/// The guarded relation is one the operation already declares, and the version
/// field is one its input already takes. Both are named here, so the emitted
/// test says which row the guard protects instead of implying it.
fn validate_state_guards(
    operation_name: &str,
    operation: &CustomOperationDeclaration,
    state: &StateGuardDeclaration,
) -> Result<(), GenerateError> {
    if state.guards.is_empty() {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!("command {operation_name} is idempotent by state and guards no relation"),
        ));
    }
    for (relation, field) in &state.guards {
        if !operation
            .relations
            .iter()
            .any(|candidate| candidate.table == *relation)
        {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidOperation,
                format!(
                    "command {operation_name} guards {relation}, which is not one of its declared relations"
                ),
            ));
        }
        if !operation
            .input
            .fields
            .iter()
            .any(|candidate| candidate.path == *field)
        {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidOperation,
                format!(
                    "command {operation_name} guards {relation} with {field}, which is not one of its input fields"
                ),
            ));
        }
    }
    Ok(())
}

/// Refuse the identity a command outside the claim shape must not mint.
///
/// A command that is idempotent by state or by an inherited claim owns no
/// claim, so it has nowhere to pre-generate an id. It therefore declares no
/// claim and inserts no row of its own.
///
/// The insert rule is not narrowed to the id column. An inherited command
/// decorates a base result, and the moment it writes its own row it writes
/// state under an identity it does not own. That command is not inherited. It
/// is a command with a claim that also composes, and `idempotent_by: claim` is
/// its honest shape.
fn refuse_minted_identity(
    operation_name: &str,
    operation: &CustomOperationDeclaration,
    declared: &str,
) -> Result<(), GenerateError> {
    if operation.claim.is_some() {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!(
                "command {operation_name} is idempotent by {declared} and declares a claim. Declare idempotent_by claim, which is the shape that owns one"
            ),
        ));
    }
    if let Some(relation) = operation
        .relations
        .iter()
        .find(|relation| !relation.insert_fields.is_empty())
    {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!(
                "command {operation_name} is idempotent by {declared} and inserts into {}.{}. A command that writes its own row writes state under an identity it owns, so declare idempotent_by claim and a claim relation that pre-generates that identity",
                relation.schema, relation.table
            ),
        ));
    }
    Ok(())
}

fn refuse_command_only_fields(
    operation_name: &str,
    operation: &CustomOperationDeclaration,
) -> Result<(), GenerateError> {
    if operation.transaction.is_some()
        || operation.automatic_retry.is_some()
        || operation.idempotent_by.is_some()
        || operation.claim.is_some()
        || operation.canonicalization.is_some()
        || operation.pre_commit.is_some()
        || operation.participant.is_some()
    {
        Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!(
                "{operation_name} declares command-only transaction, idempotence or canonicalization"
            ),
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
    let envelope_fields = [input.raw_body_maximum.is_some(), input.envelope.is_some()];
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
        if let Some(limit) = limit
            && (limit.minimum == 0 || limit.maximum < limit.minimum)
        {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidOperation,
                format!("{operation_name} {name} bounds must be a positive closed interval"),
            ));
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
        if !field.values.is_empty()
            && (field.ty != wamn_schema_introspection::ir::ColumnType::Text
                || field.values.iter().any(String::is_empty)
                || field.values.iter().collect::<BTreeSet<_>>().len() != field.values.len())
        {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidOperation,
                format!(
                    "{operation_name} {contract} field {} has invalid closed text values",
                    field.path
                ),
            ));
        }
        // An application integer is int32 by default, and int64 is opt-in, so a
        // revision carries either width.
        if field.revision
            && !matches!(
                field.ty,
                wamn_schema_introspection::ir::ColumnType::Int32
                    | wamn_schema_introspection::ir::ColumnType::Int64
            )
        {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidOperation,
                format!(
                    "{operation_name} {contract} field {} marks a revision that is neither int32 nor int64",
                    field.path
                ),
            ));
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
    // THE INPUT DECIDES, not a separate flag: a command with a line set
    // declares how its lines are ordered,
    // and a command without one declares no ordering. Tying the two together this
    // way is what keeps a lineless command from having to name an ordering
    // over lines it does not have.
    let declares_lines = canonicalization.line_order.is_some();
    if declares_lines != operation.input.line.is_some() {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!(
                "{operation_name} declares a canonical line profile without a line input, or a line input without one"
            ),
        ));
    }
    if declares_lines {
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
        if !has_ordering_key || !has_positive_quantity {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidOperation,
                format!(
                    "{operation_name} canonical line profile requires non-null UUID purchase_order_line_id and numeric quantity inputs"
                ),
            ));
        }
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
    let authored_errors = errors
        .iter()
        .copied()
        .filter(|error| shared_custom_error_detail(operation, error).is_none())
        .collect::<BTreeSet<_>>();
    if operation
        .error_details
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>()
        != authored_errors
    {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidOperation,
            format!(
                "{operation_name} error details must contain exactly its business error set; platform error details are derived"
            ),
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

pub(crate) fn custom_operation_error_detail(
    operation: &CustomOperationDeclaration,
    error: &str,
) -> OperationErrorDetailDeclaration {
    if let Some((required, optional)) = shared_custom_error_detail(operation, error) {
        return OperationErrorDetailDeclaration {
            required: required.to_vec(),
            optional: optional.to_vec(),
        };
    }
    operation
        .error_details
        .get(error)
        .expect("manifest validation closed custom-operation error details")
        .clone()
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
    if !manifest.connections.contains(connection) {
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
        // A history table admits a declared read. Only the log trigger writes it.
        if is_history_table_name(&relation.table)
            && (!relation.insert_fields.is_empty()
                || !relation.update_fields.is_empty()
                || relation.delete
                || relation.lock)
        {
            return Err(GenerateError::for_object(
                GenerateErrorKind::InvalidOperation,
                format!(
                    "{operation_name} declares a write or a row lock on {}.{}, but the {HISTORY_TABLE_SUFFIX} suffix is reserved for history tables, which only the log trigger writes",
                    relation.schema, relation.table
                ),
                format!("{}.{}", relation.schema, relation.table),
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
            && !relation.delete
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

pub(crate) fn access_operation_error_detail(
    action: CrudAction,
    code: AccessOperationErrorLiteral,
) -> OperationErrorDetailDeclaration {
    use AccessOperationErrorLiteral as Code;
    use OperationErrorDetailKey as Key;

    let (required, optional): (&[Key], &[Key]) = match code {
        Code::InvalidInput if action == CrudAction::Query => {
            (&[Key::Field], &[Key::Minimum, Key::Maximum, Key::Observed])
        }
        // A key rebound to a different request names the field that
        // carried it, exactly as any other refused input does.
        Code::InvalidInput | Code::IdempotencyConflict => (&[Key::Field], &[]),
        Code::NotFound => (&[Key::Field, Key::Id], &[]),
        Code::ConcurrencyConflict => (&[Key::ExpectedRowVersion, Key::ObservedRowVersion], &[]),
        Code::UniqueViolation
        | Code::ForeignKeyViolation
        | Code::CheckViolation
        | Code::ExclusionViolation => (&[Key::Constraint], &[]),
        Code::PermissionDenied => (&[Key::Operation], &[]),
        Code::Retry | Code::Timeout | Code::InternalError => (&[], &[]),
    };
    OperationErrorDetailDeclaration {
        required: required.to_vec(),
        optional: optional.to_vec(),
    }
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
    // The platform owns the `wamn:` operation-token namespace.
    if package.id == "wamn" {
        return Err(GenerateError::new(
            GenerateErrorKind::InvalidIdentity,
            "package id `wamn` is reserved for the platform",
        ));
    }
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

/// Explicit npm distribution identity for generated TypeScript source.
///
/// The name is authored, never inferred from an application path or a package
/// id, because the distribution is what a browser application imports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NpmDistribution {
    /// The npm package name, optionally scoped.
    pub name: String,
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
    /// Authored screen text for a column, keyed by column name.
    ///
    /// A model has no per-field object, because its fields come from
    /// introspection rather than from `wamn.json`. It addresses a column
    /// through a map, the way `enum_fields` does, and the text follows.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub field_text: BTreeMap<String, FieldText>,
    /// Record-history declaration. A relation-owning model requires it, and
    /// an overlay model inherits the declaration of the relation owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit_log: Option<AuditLogDeclaration>,
    /// How a declared `delete` removes a row. Only a relation-owning model
    /// that declares the action declares the mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete_mode: Option<DeleteMode>,
    pub operations: BTreeMap<CrudAction, OperationDeclaration>,
}

/// Closed delete mode of a model that declares the `delete` action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeleteMode {
    /// The row is removed. Its contents survive only in the record history.
    Hard,
    /// The row stays and carries the tombstone marker. Every generated read
    /// hides it.
    Tombstone,
}

/// The two reserved tombstone column names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TombstoneColumn {
    DeletedAt,
    DeletedBy,
}

impl TombstoneColumn {
    /// Both reserved names, time before actor.
    pub const ALL: [Self; 2] = [Self::DeletedAt, Self::DeletedBy];

    /// Reserved column spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DeletedAt => "deleted_at",
            Self::DeletedBy => "deleted_by",
        }
    }
}

/// Stamp columns and log retention of one relation-owning model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditLogDeclaration {
    pub columns: Vec<RecordHistoryColumn>,
    /// `none`, `unlimited`, or `P<n>D` with a positive whole number of days.
    pub retention: String,
}

/// The four reserved record-history column names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordHistoryColumn {
    CreatedAt,
    CreatedBy,
    UpdatedAt,
    UpdatedBy,
}

impl RecordHistoryColumn {
    /// Every reserved name, in trigger argument order.
    pub const ALL: [Self; 4] = [
        Self::CreatedAt,
        Self::CreatedBy,
        Self::UpdatedAt,
        Self::UpdatedBy,
    ];

    /// Reserved column spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CreatedAt => "created_at",
            Self::CreatedBy => "created_by",
            Self::UpdatedAt => "updated_at",
            Self::UpdatedBy => "updated_by",
        }
    }
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
    /// The retention of a model whose declaration keeps a log.
    ///
    /// Only a relation-owning model declares `audit_log`, so an overlay model
    /// and a model with the retention `none` return `None`.
    pub fn log_retention(&self) -> Option<&str> {
        self.audit_log
            .as_ref()
            .map(|audit_log| audit_log.retention.as_str())
            .filter(|retention| *retention != NO_LOG_RETENTION)
    }

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
    /// Legacy operation metadata retained in published component contracts.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fresh_only: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<String>,
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
    /// The claim relation a generated `create` mints its identity from.
    /// Required for `create` and refused for every other action.
    #[serde(default)]
    pub claim: Option<ClaimDeclaration>,
    /// Authored screen text for the operation itself. A component exports the
    /// label, and the page that places the component decides where it goes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub result: ResultClass,
}

/// The command-claim relation of one generated `create`.
///
/// Ratified platform law, command-identity-from-claim: any identity a command
/// creates comes from the CLAIM, not from the work. A create that minted its
/// row id during the work would mint a SECOND id on replay, which is a
/// duplicate identity — real stock on a row nothing points at — and not merely
/// a duplicate row.
///
/// `identities` maps every model field the create would otherwise let
/// PostgreSQL default to a claim column that pre-generated it. Generation
/// refuses any create whose minted-identity set and `identities` keys differ,
/// so a new `gen_random_uuid()` column added to the model breaks the build
/// rather than silently re-minting on replay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimDeclaration {
    /// Claim relation, in the model's own schema.
    pub table: String,
    /// Model field to the claim column that pre-generates it.
    pub identities: BTreeMap<String, String>,
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
}

/// Finite query sorting vocabulary with at most one requested field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SortDeclaration {
    pub fields: Vec<String>,
    pub directions: Vec<CursorDirection>,
}

/// Keyset pagination and opaque cursor contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaginationDeclaration {
    pub default_sort: SortKey,
    pub tie_breaker: TieBreakerDeclaration,
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
}

/// Closed sort direction vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorDirection {
    Ascending,
    Descending,
}

/// Import requirements for one package-local component group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentDeclaration {
    pub connections: Vec<String>,
}
