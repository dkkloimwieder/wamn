//! Generates deterministic package artifacts from migration-derived schema IR.
//!
//! Core generation is a pure transformation: callers provide the normalized
//! [`CatalogIr`], exact manifest bytes, authored SQL bytes, and explicit
//! provenance. That transformation performs no filesystem, database, clock,
//! or environment access.
//!
//! Generated identifiers follow
//! `docs/architecture/application-naming.md`. Migration introspection owns
//! schema-to-IR normalization; that IR is generation's only structural input.
//!
//! Migrations author PostgreSQL schema selection. Generated and authored query
//! corpus files therefore use unqualified relations and inherit the host-owned
//! search path frozen by
//! `components/data/postgres-statements/wit/deps/wamn-postgres/package.wit`.

pub mod client_ir;
pub mod client_rust;
mod cursor;
mod data_access;
mod error;
mod generate;
mod manifest;
mod materialize;
mod parity;
mod sql;
mod sql_lex;

pub use cursor::{
    CursorError, CursorErrorKind, CursorV1, CursorValue, decode_cursor, encode_cursor,
};
pub use data_access::{
    DATA_ACCESS_OVERLAY_PATH, DATA_ACCESS_ROLE, DataAccessOverlay, DataAccessRelation,
    DataAccessRelationInventory, EffectiveDataAccess, EffectiveDataAccessRelation,
    data_access_schemas, derive_data_access_overlay_from_inventory, derive_effective_data_access,
    render_effective_data_access_sql, validate_data_access_contribution,
};
pub use error::{GenerateError, GenerateErrorKind};
pub use generate::{
    StatementTransactionality,
    AuthoredSql, GeneratedFile, GeneratedPackage, GenerationInput, GenerationProvenance,
    PackageWeld, corpus_sha256, generate,
};
pub use manifest::{
    AccessOperationErrorLiteral, AuthoredSqlDeclaration, AuthoredSqlVariant,
    BaseDependencyRequirement, CdcDisposition, ClaimDeclaration, CommandCanonicalization,
    CommandLineOrder, CommandTransaction, ComponentDeclaration, ConnectionDeclaration,
    ContractFieldDeclaration, CountLimitDeclaration, CrudAction, CursorDeclaration,
    CursorDirection, CursorEncoding, CursorPayload, CustomOperationDeclaration,
    CustomOperationInputDeclaration, CustomOperationKind, CustomOperationResultDeclaration,
    EventRegistrationDeclaration, FilterBinding, FilterDeclaration, InputRefusal,
    InternalRelationDeclaration, ItemSemantics, LimitDeclaration, ModelDeclaration,
    NumericSpelling, OperationDeclaration, OperationErrorDetailDeclaration,
    OperationErrorDetailKey, OperationVisibility, PackageIdentity, PackageManifest,
    PaginationDeclaration, PaginationKind, PolicyContractRequirement, PolicyContractState,
    ResultClass, SortDeclaration, SortKey, StaticSqlFetch, StaticSqlRelationDeclaration,
    StaticSqlStatementDeclaration, StaticSqlValueDeclaration, TieBreakerDeclaration,
    TimestamptzSpelling, UuidSpelling, canonical_operation_identity, canonical_operation_prefix,
    validate_operation_vocabulary,
};
pub use materialize::{
    materialize_package_verified,
    MaterializeMode, introspect_package, materialize_package, materialize_package_from_catalog,
};
pub use parity::{ParityError, ParityErrorKind, validate_parity_json};
pub use wamn_schema_introspection::ir::CatalogIr;
