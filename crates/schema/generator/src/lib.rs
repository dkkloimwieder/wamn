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
//! `crates/platform/runtime/wit/deps/wamn-postgres/package.wit`.

pub mod client_component;
mod client_fields;
pub mod client_ir;
/// The UI-neutral screen plan. Every client emitter reads its rules.
pub mod client_plan;
mod client_route;
pub mod client_rust;
/// The TypeScript client emitter. Browser bindings from the same contract IR.
pub mod client_ts;
pub mod client_tui;
mod cursor;
mod data_access;
mod error;
mod generate;
mod manifest;
mod materialize;
mod rustfmt;
mod sql;
mod sql_lex;
mod sqlx_metadata;

pub use cursor::{
    CursorError, CursorErrorKind, CursorV1, CursorValue, decode_cursor, encode_cursor,
};
pub use data_access::{
    DATA_ACCESS_OVERLAY_PATH, DATA_ACCESS_ROLE, DataAccessOverlay, DataAccessRelation,
    DataAccessRelationFields, EffectiveDataAccess, EffectiveDataAccessRelation,
    data_access_schemas, derive_data_access_overlay_from_relation_fields,
    derive_effective_data_access, render_effective_data_access_sql,
    validate_data_access_contribution,
};
pub use error::{GenerateError, GenerateErrorKind};
pub use generate::{
    AuthoredSql, GeneratedFile, GeneratedPackage, GeneratedPackageMetadata, GenerationInput,
    GenerationProvenance, StatementTransactionality, corpus_sha256, generate,
};
pub use manifest::{
    AccessOperationErrorLiteral, AuditLogDeclaration, AuthoredSqlDeclaration, AuthoredSqlVariant,
    BaseDependencyRequirement, CdcDisposition, ClaimDeclaration, CommandCanonicalization,
    CommandIdempotence, CommandLineOrder, CommandTransaction, ComponentDeclaration,
    ContractFieldDeclaration, CountLimitDeclaration, CrudAction, CursorDirection,
    CustomClaimDeclaration, CustomOperationDeclaration, CustomOperationInputDeclaration,
    CustomOperationKind, CustomOperationResultDeclaration, EventRegistrationDeclaration,
    FilterDeclaration, InheritedClaimDeclaration, InternalRelationDeclaration, LimitDeclaration,
    ModelDeclaration, NpmDistribution, OperationDeclaration, OperationErrorDetailDeclaration,
    OperationErrorDetailKey, OperationVisibility, PackageIdentity, PackageManifest,
    PaginationDeclaration, PolicyContractRequirement, PolicyContractState, RecordHistoryColumn,
    ResultClass, SortDeclaration, SortKey, StateGuardDeclaration, StaticSqlFetch,
    StaticSqlRelationDeclaration, StaticSqlStatementDeclaration, StaticSqlValueDeclaration,
    TieBreakerDeclaration, canonical_operation_identity, canonical_operation_prefix,
    validate_operation_vocabulary,
};
pub use materialize::{
    MaterializeMode, introspect_package, materialize_package, materialize_package_from_catalog,
    materialize_package_verified, materialize_package_verified_with_catalog,
};
pub use sqlx_metadata::{
    SqlxMetadataMode, SqlxVerifier, package_database_url, stage_sqlx_verifier, verify_sqlx_metadata,
};
pub use wamn_schema_introspection::ir::CatalogIr;
