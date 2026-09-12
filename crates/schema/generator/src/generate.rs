mod contracts;
mod rust;
mod validation;

use contracts::{
    emit_cursor_contract, emit_custom_operation, emit_model, required_schema_contract,
};
use validation::{authored_sql_map, validate};

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use wamn_execution_contract::canonical_json_bytes;
use wamn_schema_introspection::ir::{
    CatalogIr, Column, ColumnDefault, ColumnType, Constraint, ConstraintKind, Exclusion, Table,
};

use crate::manifest::{
    AccessOperationErrorLiteral, AuthoredSqlDeclaration, CommandIdempotence,
    ContractFieldDeclaration, CrudAction, CursorDirection, CustomOperationDeclaration,
    CustomOperationKind, CustomOperationResultDeclaration, InheritedClaimDeclaration,
    ModelDeclaration, OperationDeclaration, OperationErrorDetailDeclaration, PackageManifest,
    PolicyContractRequirement, PolicyContractState, ResultClass, SortDeclaration,
    StateGuardDeclaration, StaticSqlFetch, canonical_operation_identity, custom_artifact_stem,
    rust_identifier, rust_type_identifier, validate_identifier, validate_operation_vocabulary,
};
use crate::sql;
use crate::sql_lex::contains_schema_qualified_reference;
use crate::{GenerateError, GenerateErrorKind};

const POSTGRES_INTERFACE: &str = "wamn:postgres@0.1.0";
const QUERY_LIMIT: u32 = 100;
const CURSOR_VERSION: u8 = 1;
/// The claim column carrying the caller's idempotency key, under the claim's
/// primary key. One spelling, so a replay of one command can never look for
/// its claim under another name.
pub(crate) const CLAIM_KEY_COLUMN: &str = "idempotency_key";
/// The claim column carrying the canonical request bytes the key is bound to.
/// A second call with the same key and different bytes is refused against it.
pub(crate) const CLAIM_COMMAND_COLUMN: &str = "canonical_command";
/// Mint the claim, or yield nothing because the key already has one.
const CREATE_CLAIM_STATEMENT: &str = "create_claim";
/// Read the immutable original for one key. Writes nothing.
const CREATE_REPLAY_STATEMENT: &str = "create_replay";
/// Insert the row under the identities the claim already minted.
const CREATE_STATEMENT: &str = "create";
/// The three statements of one generated create, in emission order.
const CREATE_STATEMENTS: [&str; 3] = [
    CREATE_CLAIM_STATEMENT,
    CREATE_REPLAY_STATEMENT,
    CREATE_STATEMENT,
];

/// One package-owned authored SQL source supplied without filesystem access.
#[derive(Debug, Clone, Copy)]
pub struct AuthoredSql<'a> {
    path: &'a str,
    bytes: &'a [u8],
}

impl<'a> AuthoredSql<'a> {
    /// Pair a package-relative path with its exact source bytes.
    pub const fn new(path: &'a str, bytes: &'a [u8]) -> Self {
        Self { path, bytes }
    }

    /// Package-relative corpus path.
    pub const fn path(&self) -> &'a str {
        self.path
    }

    /// Exact authored bytes included in the SQL corpus.
    pub const fn bytes(&self) -> &'a [u8] {
        self.bytes
    }
}

/// Generator and toolchain facts in each generated package.
#[derive(Debug, Clone, Copy)]
pub struct GenerationProvenance<'a> {
    generator: &'a str,
    toolchain: &'a str,
}

impl<'a> GenerationProvenance<'a> {
    /// Construct generator provenance without consulting git, environment, or a clock.
    pub const fn new(generator: &'a str, toolchain: &'a str) -> Self {
        Self {
            generator,
            toolchain,
        }
    }
}

/// PostgreSQL's verdict on which statements need a transaction, keyed by the
/// statement's corpus path.
///
/// A statement NEEDS A TRANSACTION when its generic plan carries a
/// `ModifyTable` node (it writes -- data-modifying CTEs included) or a
/// `LockRows` node (it takes a row lock, which autocommit would drop the
/// instant the statement returned). The verdict is the server's, taken at
/// generation time against the already-migrated database; nothing here reads
/// SQL text.
///
/// Generation runs TWICE: once to obtain the corpus with every verdict absent,
/// then again with the verdicts the server gave for that corpus. The bit is
/// contract-only and never reaches SQL bytes, so the corpus is identical across
/// both passes -- which `application_sql_corpus_identity` would catch if it
/// were not.
#[derive(Debug, Clone, Default)]
pub struct StatementTransactionality {
    paths: BTreeMap<String, bool>,
}

impl StatementTransactionality {
    /// Build from the server's verdicts, keyed by corpus path.
    #[must_use]
    pub const fn from_paths(paths: BTreeMap<String, bool>) -> Self {
        Self { paths }
    }

    /// Absent verdicts, for the corpus-discovery pass.
    #[must_use]
    pub fn unclassified() -> Self {
        Self::from_paths(BTreeMap::new())
    }

    /// The server's verdict for one corpus path. An unclassified path reads
    /// `false` ONLY during the discovery pass, whose contracts are discarded.
    #[must_use]
    pub fn needs_transaction(&self, path: &str) -> bool {
        self.paths.get(path).copied().unwrap_or(false)
    }
}

/// Complete pure input to [`generate`].
#[derive(Debug)]
pub struct GenerationInput<'a> {
    catalog: &'a CatalogIr,
    manifest_json: &'a [u8],
    authored_sql: &'a [AuthoredSql<'a>],
    provenance: GenerationProvenance<'a>,
    transactional: &'a StatementTransactionality,
}

impl<'a> GenerationInput<'a> {
    /// Construct a generation input from exact in-memory artifacts.
    pub const fn new(
        catalog: &'a CatalogIr,
        manifest_json: &'a [u8],
        authored_sql: &'a [AuthoredSql<'a>],
        provenance: GenerationProvenance<'a>,
        transactional: &'a StatementTransactionality,
    ) -> Self {
        Self {
            catalog,
            manifest_json,
            authored_sql,
            provenance,
            transactional,
        }
    }
}

/// One generated package-relative artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedFile {
    path: Box<str>,
    bytes: Box<[u8]>,
}

impl GeneratedFile {
    /// One generated artifact. Sibling modules emit their own files; the
    /// fields stay private so a path and its bytes are always set together.
    pub(crate) fn new(path: Box<str>, bytes: Box<[u8]>) -> Self {
        Self { path, bytes }
    }

    /// Package-relative artifact path.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Exact deterministic artifact bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Canonical immutable package metadata emitted as `generated/package-weld.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    rename = "PackageWeld",
    expecting = "struct PackageWeld",
    deny_unknown_fields
)]
pub struct GeneratedPackageMetadata {
    verified_schema_state_id: Box<str>,
    required_schema_contract: RequiredSchemaContract,
    required_platform_policy_contract: PolicyContractRequirement,
    application_sql_corpus_identity: Box<str>,
    provenance: OwnedProvenance,
    promotion_state: PromotionState,
}

impl GeneratedPackageMetadata {
    /// Parse the canonical generated metadata and refuse alternate spellings.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, GenerateError> {
        let metadata: Self = serde_json::from_slice(bytes).map_err(|source| {
            GenerateError::with_source(
                GenerateErrorKind::InvalidManifest,
                "package-weld.json does not match the package contract",
                source,
            )
        })?;
        let canonical = canonical_json_bytes(
            &serde_json::to_value(&metadata).expect("generated package metadata always serializes"),
        );
        if canonical != bytes {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidManifest,
                "package-weld.json is not canonical compact JSON",
            ));
        }
        for (field, value) in [
            (
                "verified_schema_state_id",
                metadata.verified_schema_state_id(),
            ),
            (
                "application_sql_corpus_identity",
                metadata.application_sql_corpus_identity(),
            ),
        ] {
            if !valid_sha256(value) {
                return Err(GenerateError::new(
                    GenerateErrorKind::InvalidManifest,
                    format!("package-weld.json {field} is not sha256:<64 lowercase hex>"),
                ));
            }
        }
        let expected_promotion_state = match metadata.required_platform_policy_contract.state {
            PolicyContractState::Unsatisfied => PromotionState::BlockedUnsatisfiedPolicyContract,
            PolicyContractState::Satisfied => PromotionState::Eligible,
        };
        if metadata.promotion_state != expected_promotion_state {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidManifest,
                "package-weld.json promotion_state disagrees with required_platform_policy_contract.state",
            ));
        }
        Ok(metadata)
    }

    /// Digest of the complete normalized catalog IR used for generation.
    pub fn verified_schema_state_id(&self) -> &str {
        &self.verified_schema_state_id
    }

    /// Digest of the exact authored and generated SQL files.
    pub fn application_sql_corpus_identity(&self) -> &str {
        &self.application_sql_corpus_identity
    }

    /// Required opaque platform policy contract and current satisfaction state.
    pub const fn required_platform_policy_contract(&self) -> &PolicyContractRequirement {
        &self.required_platform_policy_contract
    }

    /// Whether the typed policy requirement permits package promotion.
    pub const fn promotion_eligible(&self) -> bool {
        matches!(self.promotion_state, PromotionState::Eligible)
    }
}

/// Generated package files and their metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedPackage {
    files: Box<[GeneratedFile]>,
    metadata: GeneratedPackageMetadata,
}

impl GeneratedPackage {
    /// Artifacts ordered by package-relative path.
    pub fn files(&self) -> &[GeneratedFile] {
        &self.files
    }

    /// Find one generated artifact without touching the filesystem.
    pub fn file(&self, path: &str) -> Option<&GeneratedFile> {
        self.files
            .binary_search_by_key(&path, |file| file.path())
            .ok()
            .map(|index| &self.files[index])
    }

    /// Canonical metadata included in the generated file set.
    pub const fn metadata(&self) -> &GeneratedPackageMetadata {
        &self.metadata
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequiredSchemaContract {
    tables: Box<[RequiredTable]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequiredTable {
    schema: Box<str>,
    table: Box<str>,
    fields: Box<[RequiredField]>,
    constraints: Box<[RequiredConstraint]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequiredField {
    name: Box<str>,
    #[serde(rename = "type")]
    ty: Box<str>,
    nullable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequiredConstraint {
    name: Box<str>,
    definition: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnedProvenance {
    generator: Box<str>,
    toolchain: Box<str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PromotionState {
    BlockedUnsatisfiedPolicyContract,
    Eligible,
}

fn valid_sha256(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

/// Generate a package without filesystem, database, clock, or environment I/O.
pub fn generate(input: &GenerationInput<'_>) -> Result<GeneratedPackage, GenerateError> {
    let manifest = PackageManifest::from_slice(input.manifest_json)?;
    validate(input, &manifest)?;

    let mut files = BTreeMap::<String, Vec<u8>>::new();
    let mut sql_corpus = authored_sql_map(input.authored_sql)?;
    emit_cursor_contract(&mut files)?;

    for (model_name, model) in &manifest.models {
        let table = relation(input.catalog, model).expect("validation resolved every relation");
        emit_model(
            &mut files,
            &mut sql_corpus,
            input.transactional,
            input.catalog,
            &manifest,
            model_name,
            model,
            table,
        )?;
    }
    for (operation_name, operation) in &manifest.custom_operations {
        emit_custom_operation(
            &mut files,
            &sql_corpus,
            input.transactional,
            input.catalog,
            &manifest,
            operation_name,
            operation,
        )?;
    }
    let data_access = crate::data_access::derive_data_access_overlay(
        input.catalog,
        input.manifest_json,
        &manifest,
    )?;
    insert_canonical_json(
        &mut files,
        crate::data_access::DATA_ACCESS_OVERLAY_PATH,
        &data_access,
    )?;

    let metadata = GeneratedPackageMetadata {
        verified_schema_state_id: sha256(&canonical_json_bytes(
            &serde_json::to_value(input.catalog).expect("schema IR always serializes"),
        ))
        .into(),
        required_schema_contract: required_schema_contract(input.catalog, &manifest),
        required_platform_policy_contract: manifest.required_platform_policy_contract.clone(),
        application_sql_corpus_identity: corpus_sha256(
            sql_corpus
                .iter()
                .map(|(path, bytes)| (path.as_str(), bytes.as_slice())),
        )
        .into(),
        provenance: OwnedProvenance {
            generator: input.provenance.generator.into(),
            toolchain: input.provenance.toolchain.into(),
        },
        promotion_state: match manifest.required_platform_policy_contract.state {
            PolicyContractState::Unsatisfied => PromotionState::BlockedUnsatisfiedPolicyContract,
            PolicyContractState::Satisfied => PromotionState::Eligible,
        },
    };
    insert_canonical_json(&mut files, "generated/package-weld.json", &metadata)?;

    let files = files
        .into_iter()
        .map(|(path, bytes)| GeneratedFile {
            path: path.into_boxed_str(),
            bytes: bytes.into_boxed_slice(),
        })
        .collect::<Vec<_>>()
        .into_boxed_slice();

    Ok(GeneratedPackage { files, metadata })
}

/// Hash sorted path/byte entries with unambiguous big-endian length framing.
pub fn corpus_sha256<'a>(entries: impl IntoIterator<Item = (&'a str, &'a [u8])>) -> String {
    let mut entries = entries.into_iter().collect::<Vec<_>>();
    entries.sort_by_key(|(path, _)| *path);

    let mut hasher = Sha256::new();
    for (path, bytes) in entries {
        let path = path.as_bytes();
        hasher.update(
            u64::try_from(path.len())
                .expect("path length fits u64")
                .to_be_bytes(),
        );
        hasher.update(path);
        hasher.update(
            u64::try_from(bytes.len())
                .expect("artifact length fits u64")
                .to_be_bytes(),
        );
        hasher.update(bytes);
    }
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

/// The `from` literal for the constraint a custom operation maps, whether the
/// database enforces it as a [`Constraint`] or as an exclusion constraint.
fn custom_operation_constraint_origin(
    catalog: &CatalogIr,
    operation: &CustomOperationDeclaration,
    name: &str,
) -> Option<&'static str> {
    for relation in &operation.relations {
        if !relation
            .constraints
            .iter()
            .any(|constraint| constraint == name)
        {
            continue;
        }
        let table = catalog
            .tables()
            .iter()
            .find(|table| table.schema() == relation.schema && table.name() == relation.table)?;
        if let Some(constraint) = table
            .constraints()
            .iter()
            .find(|constraint| constraint.name() == name)
        {
            return Some(constraint_error(constraint.kind()));
        }
        if table
            .exclusions()
            .iter()
            .any(|exclusion| exclusion.name() == name)
        {
            return Some("exclusion_violation");
        }
    }
    None
}

fn query_variants(operation: &OperationDeclaration) -> Vec<(&str, CursorDirection)> {
    if let Some(authored) = &operation.authored_sql {
        return authored
            .variants
            .iter()
            .map(|variant| (variant.field.as_str(), variant.direction))
            .collect();
    }
    operation.sort.as_ref().map_or_else(
        || {
            let pagination = operation
                .pagination
                .as_ref()
                .expect("query validation requires pagination");
            vec![(
                pagination.default_sort.field.as_str(),
                pagination.default_sort.direction,
            )]
        },
        |sort| {
            sort.fields
                .iter()
                .flat_map(|field| {
                    sort.directions
                        .iter()
                        .map(move |direction| (field.as_str(), *direction))
                })
                .collect()
        },
    )
}

fn constraint_error_code(kind: &ConstraintKind) -> AccessOperationErrorLiteral {
    match kind {
        ConstraintKind::PrimaryKey { .. } | ConstraintKind::Unique { .. } => {
            AccessOperationErrorLiteral::UniqueViolation
        }
        ConstraintKind::ForeignKey { .. } => AccessOperationErrorLiteral::ForeignKeyViolation,
        ConstraintKind::Check { .. } => AccessOperationErrorLiteral::CheckViolation,
    }
}

fn operation_constraints<'a>(
    table: &'a Table,
    action: CrudAction,
    operation: &OperationDeclaration,
) -> Vec<&'a Constraint> {
    if !matches!(
        action,
        CrudAction::Create | CrudAction::Update | CrudAction::Delete
    ) {
        return Vec::new();
    }
    table
        .constraints()
        .iter()
        .filter(|constraint| {
            action != CrudAction::Update
                || update_can_violate(constraint.kind(), &operation.writable_fields)
        })
        .collect()
}

/// Exclusions the operation can violate.
///
/// An INSERT can always collide with a row already stored. An UPDATE can only
/// collide when it writes a column the constraint depends on -- including a
/// column an expression key reads, which PostgreSQL records as a dependency and
/// [`Exclusion::columns`] carries. A DELETE never can: removing a row cannot
/// create an overlap.
fn operation_exclusions<'a>(
    table: &'a Table,
    action: CrudAction,
    operation: &OperationDeclaration,
) -> Vec<&'a Exclusion> {
    if !matches!(action, CrudAction::Create | CrudAction::Update) {
        return Vec::new();
    }
    table
        .exclusions()
        .iter()
        .filter(|exclusion| {
            action != CrudAction::Update
                || exclusion.columns().iter().any(|column| {
                    operation
                        .writable_fields
                        .iter()
                        .any(|field| field.as_str() == column.as_ref())
                })
        })
        .collect()
}

fn update_can_violate(kind: &ConstraintKind, writable_fields: &[String]) -> bool {
    match kind {
        // Opaque CHECK expressions expose no structural field set to intersect.
        ConstraintKind::Check { .. } => false,
        ConstraintKind::PrimaryKey { columns } | ConstraintKind::Unique { columns } => {
            columns.iter().any(|column| {
                writable_fields
                    .iter()
                    .any(|field| field.as_str() == column.as_ref())
            })
        }
        ConstraintKind::ForeignKey { columns, .. } => columns.iter().any(|column| {
            writable_fields
                .iter()
                .any(|field| field.as_str() == column.column())
        }),
    }
}

#[derive(Debug, Clone, Copy)]
enum Projection {
    Native,
    Wamn,
}

#[derive(Debug, Clone, Copy)]
enum ProjectionContents<'a> {
    Native {
        operation_rows: &'a [RustRow],
        bind_fixtures: &'a [NativeBindFixture],
    },
    Wamn(&'a WamnApi),
}

#[derive(Debug, Serialize)]
struct WamnApi {
    statement_digest_visibility: RustVisibility,
    mutation_constraints: Vec<MutationConstraintNames>,
    operation_rows: Vec<RustRow>,
    accessors: Vec<WamnAccessor>,
}

#[derive(Debug, Serialize)]
struct MutationConstraintNames {
    operation: CrudAction,
    unique: ConstraintNameSlice,
    foreign_key: ConstraintNameSlice,
    check: ConstraintNameSlice,
    exclusion: ConstraintNameSlice,
}

#[derive(Debug, Serialize)]
struct ConstraintNameSlice {
    constant: String,
    visibility: RustVisibility,
    names: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct RustRow {
    name: String,
    visibility: RustVisibility,
    fields: Vec<RustMember>,
}

#[derive(Debug, Clone, Serialize)]
struct RustMember {
    name: String,
    #[serde(rename = "type")]
    rust_type: String,
    #[serde(skip)]
    statement_type: ColumnType,
    #[serde(skip)]
    nullable: bool,
}

#[derive(Debug, Clone, Serialize)]
struct AccessorBind {
    parameter: String,
    postgres: String,
    nullable: bool,
    native_rust: String,
    wamn_rust: String,
    #[serde(skip)]
    statement_type: ColumnType,
}

#[derive(Debug, Serialize)]
struct WamnAccessor {
    name: String,
    visibility: RustVisibility,
    operation: CrudAction,
    statement_digest_constant: String,
    #[serde(skip)]
    sql_path: String,
    row: String,
    fetch: AccessorFetch,
    binds: Vec<AccessorBind>,
}

#[derive(Debug, Serialize)]
struct StaticSqlAccessor {
    name: String,
    statement_digest_constant: String,
    row: String,
    fetch: StaticSqlFetch,
    binds: Vec<AccessorBind>,
}

#[derive(Debug, Serialize)]
struct NativeBindFixture {
    accessor: String,
    parameter: String,
    function: String,
    visibility: RustVisibility,
    #[serde(rename = "type")]
    rust_type: String,
    #[serde(skip)]
    value: String,
}

#[derive(Debug, Serialize)]
struct StatementContract {
    name: String,
    path: String,
    digest: String,
    binds: Vec<StatementValueContract>,
    columns: Vec<StatementValueContract>,
    /// PostgreSQL's own verdict, from the statement's generic plan against the
    /// migrated database: a `ModifyTable` node (it writes, data-modifying CTEs
    /// included) or a `LockRows` node (it takes a row lock, which autocommit
    /// would drop the instant the statement returned). Never read off the text.
    transactional: bool,
}

#[derive(Debug, Clone, Serialize)]
struct StatementValueContract {
    name: String,
    #[serde(rename = "type")]
    ty: ColumnType,
    nullable: bool,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum AccessorFetch {
    Optional,
    All,
    One,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum RustVisibility {
    Public,
    Crate,
}

impl RustVisibility {
    const fn source(self) -> &'static str {
        match self {
            Self::Public => "pub",
            Self::Crate => "pub(crate)",
        }
    }
}

fn relation<'a>(catalog: &'a CatalogIr, model: &ModelDeclaration) -> Option<&'a Table> {
    catalog
        .tables()
        .iter()
        .find(|table| table.schema() == model.schema && table.name() == model.table)
}

fn column<'a>(table: &'a Table, name: &str) -> Option<&'a Column> {
    table.columns().iter().find(|column| column.name() == name)
}

fn insert_json(
    files: &mut BTreeMap<String, Vec<u8>>,
    path: &str,
    value: &impl Serialize,
) -> Result<(), GenerateError> {
    insert_bytes(
        files,
        path,
        serde_json::to_vec(value).expect("generated JSON values always serialize"),
    )
}

fn insert_json_line(
    files: &mut BTreeMap<String, Vec<u8>>,
    path: &str,
    value: &impl Serialize,
) -> Result<(), GenerateError> {
    let mut bytes = serde_json::to_vec(value).expect("generated JSON values always serialize");
    bytes.push(b'\n');
    insert_bytes(files, path, bytes)
}

fn insert_canonical_json(
    files: &mut BTreeMap<String, Vec<u8>>,
    path: &str,
    value: &impl Serialize,
) -> Result<(), GenerateError> {
    insert_bytes(
        files,
        path,
        canonical_json_bytes(
            &serde_json::to_value(value).expect("generated JSON values always serialize"),
        ),
    )
}

fn insert_bytes(
    files: &mut BTreeMap<String, Vec<u8>>,
    path: &str,
    bytes: Vec<u8>,
) -> Result<(), GenerateError> {
    if files.insert(path.to_owned(), bytes).is_some() {
        Err(GenerateError::for_path(
            GenerateErrorKind::DuplicatePath,
            "generated artifact path is repeated",
            path,
        ))
    } else {
        Ok(())
    }
}

fn sha256(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

fn constraint_error(kind: &ConstraintKind) -> &'static str {
    match kind {
        ConstraintKind::PrimaryKey { .. } | ConstraintKind::Unique { .. } => "unique_violation",
        ConstraintKind::ForeignKey { .. } => "foreign_key_violation",
        ConstraintKind::Check { .. } => "check_violation",
    }
}
