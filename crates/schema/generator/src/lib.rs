//! Generates deterministic package artifacts from migration-derived schema IR.
//!
//! Generation is a pure transformation: callers provide the normalized
//! [`CatalogIr`], exact manifest bytes, authored SQL bytes, and explicit
//! provenance. The generator performs no filesystem, database, clock, or
//! environment access.
//!
//! Generated identifiers follow
//! `docs/architecture/application-naming.md`. The legacy schema model and DDL
//! compiler are deliberately outside this crate: migration introspection is the
//! only structural input.
//!
//! Migrations author PostgreSQL schema selection. Generated and authored query
//! corpus files therefore use unqualified relations and inherit the host-owned
//! search path frozen by
//! `components/data/postgres-sqlx/wit/deps/wamn-postgres/package.wit`.

mod cursor;
mod error;
mod generate;
mod manifest;
mod parity;
mod sql;
mod sql_lex;

pub use cursor::{
    CursorError, CursorErrorKind, CursorV1, CursorValue, decode_cursor, encode_cursor,
};
pub use error::{GenerateError, GenerateErrorKind};
pub use generate::{
    AuthoredSql, GeneratedFile, GeneratedPackage, GenerationInput, GenerationProvenance,
    PackageWeld, corpus_sha256, generate,
};
pub use manifest::{
    AuthoredSqlDeclaration, AuthoredSqlVariant, CommandCanonicalization, CommandDeclaration,
    CommandErrorLiteral, CommandFetch, CommandInputDeclaration, CommandLineOrder,
    CommandRelationDeclaration, CommandResultDeclaration, CommandStatementDeclaration,
    CommandTransaction, CommandValueDeclaration, ComponentDeclaration, ConnectionDeclaration,
    ContractFieldDeclaration, CountLimitDeclaration, CrudAction, CursorDeclaration,
    CursorDirection, CursorEncoding, CursorPayload, FilterBinding, FilterDeclaration, InputRefusal,
    ItemSemantics, LimitDeclaration, ModelDeclaration, NumericSpelling, OperationDeclaration,
    PackageIdentity, PackageManifest, PaginationDeclaration, PaginationKind,
    PolicyContractRequirement, PolicyContractState, ResultClass, SortDeclaration, SortKey,
    TieBreakerDeclaration, TimestamptzSpelling, UuidSpelling,
};
pub use parity::{ParityError, ParityErrorKind, validate_parity_json};
pub use wamn_schema_introspection::ir::CatalogIr;
