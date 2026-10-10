//! Generates deterministic package artifacts from migration-derived schema IR.
//!
//! Core generation is a deterministic transformation: callers provide the
//! normalized [`CatalogIr`], exact manifest bytes, authored SQL bytes, and
//! explicit provenance. That transformation performs no filesystem, database,
//! clock, or environment access; its one child process is `rustfmt`, which
//! formats the Rust it emits so that the committed artifacts equal what
//! `cargo fmt` produces.
//!
//! Generated identifiers follow
//! `docs/architecture/naming.md`. Migration introspection owns
//! schema-to-IR normalization; that IR is generation's only structural input.
//!
//! Migrations author PostgreSQL schema selection. Generated and authored query
//! corpus files therefore use unqualified relations and inherit the host-owned
//! search path frozen by
//! `crates/platform/runtime/wit/deps/wamn-postgres-0.3/package.wit`.

mod authoring;
/// `wamn build`: the two-pass package build (docs/plan/platform-deploy.md §7.1).
pub mod build;
pub mod client_component;
mod client_fields;
pub mod client_ir;
/// The UI-neutral screen plan. Every client emitter reads its rules.
pub mod client_plan;
mod client_route;
pub mod client_rust;
/// The TypeScript client emitter. Browser bindings from the same contract IR.
pub mod client_ts;
mod cursor;
mod data_access;
mod describe;
mod error;
mod generate;
mod manifest;
mod materialize;
pub mod operation_reference;
mod output;
mod package_catalog;
/// `build.json`, the content receipt of one package build.
pub mod receipt;
pub mod route_schema;
mod rustfmt;
mod sql;
mod sql_lex;
mod sqlx_metadata;
mod upgrade_stage;

pub use authoring::{
    AUTHORED_MANIFEST, COMPILED_MANIFEST, KCL_ENV, compile_environment, compile_manifest,
    is_authored, is_package_root, manifest_package_root, package_manifest_path,
    write_compiled_manifest,
};
/// The write log's three fixed statements.
pub use cursor::{
    CursorError, CursorErrorType, CursorV1, CursorValue, decode_cursor, encode_cursor,
};
pub use data_access::{
    DATA_ACCESS_OVERLAY_PATH, DATA_ACCESS_ROLE, DataAccessOverlay, DataAccessRelation,
    DataAccessRelationFields, EffectiveDataAccess, EffectiveDataAccessRelation,
    data_access_schemas, derive_data_access_overlay_from_relation_fields,
    derive_effective_data_access, render_effective_data_access_sql,
    validate_data_access_contribution,
};
pub use error::{GenerateError, GenerateErrorType};
pub use generate::{
    AuthoredSql, GeneratedFile, GeneratedPackage, GeneratedPackageMetadata, GenerationInput,
    GenerationProvenance, StatementTransactionality, corpus_sha256, derive_manifest, generate,
};
pub use manifest::{
    AccessOperationErrorLiteral, AuditLogDeclaration, AuthoredSqlDeclaration, AuthoredSqlVariant,
    BaseDependencyRequirement, CdcDisposition, ClientPackage, CommandCanonicalization,
    CommandIdempotence, CommandLineOrder, CommandTransaction, ComponentDeclaration,
    ContractFieldDeclaration, CountLimitDeclaration, CrudAction, CursorDirection,
    CustomOperationDeclaration, CustomOperationInputDeclaration, CustomOperationResultDeclaration,
    CustomOperationType, EventRegistrationDeclaration, FieldText, FilterDeclaration, FilterDefault,
    FilterMatch, InheritedClaimDeclaration, InternalRelationDeclaration, LimitDeclaration,
    ModelDeclaration, OperationDeclaration, OperationErrorDetailDeclaration,
    OperationErrorDetailKey, OperationOwners, OperationVisibility, PackageIdentity,
    PackageManifest, PaginationDeclaration, PolicyContractRequirement, PolicyContractState,
    RecordHistoryColumn, ResultClass, Revision, SearchDeclaration, SortDeclaration, SortKey,
    StateGuardDeclaration, StaticSqlFetch, StaticSqlRelationDeclaration,
    StaticSqlStatementDeclaration, StaticSqlValueDeclaration, TieBreakerDeclaration,
    TombstoneColumn, WorkflowDeclaration, canonical_operation_identity, canonical_operation_prefix,
    resolve_operation_reference, sealed_operation_reference, validate_operation_vocabulary,
};
pub use materialize::{
    classify_statements_with_existing_grants,
    classify_statements_with_existing_grants_in_transaction, introspect_package,
    materialize_host_route_client, materialize_package, materialize_package_from_catalog,
    materialize_package_verified, materialize_package_verified_with_catalog,
    materialize_package_verified_with_existing_grants,
};
pub use output::output_root;
pub use package_catalog::project_package_catalog;
pub use sqlx_metadata::{
    SqlxVerifier, package_database_url, prepare_sqlx_metadata, stage_sqlx_verifier,
};
#[doc(inline)]
pub use upgrade_stage::{
    BackfillStage, UpgradeStage, UpgradeStageException, UpgradeStagePhase, validate_upgrade_stage,
};
pub use wamn_schema_introspection::ir::CatalogIr;
