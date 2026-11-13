//! Generates deterministic package artifacts from migration-derived schema IR.
//!
//! Core generation is a deterministic transformation: callers provide the
//! normalized [`CatalogIr`](wamn_schema_introspection::ir::CatalogIr), exact manifest bytes, and authored SQL bytes. That transformation performs no filesystem, database,
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
//! `wit/wamn-postgres-0.4/package.wit`.

mod authoring;
/// `wamn build`: the two-pass package build (docs/operations/building.md#wamn-build).
pub mod build;
/// `build.json`, the build record of one package build.
pub mod build_record;
pub mod client_component;
mod client_fields;
pub mod client_ir;
/// The UI-neutral screen plan. Every client emitter reads its rules.
pub mod client_plan;
mod client_route;
/// The TypeScript client emitter. Browser bindings from the same contract IR.
pub mod client_ts;
mod components;
mod cursor;
mod describe;
/// Catalog facts for authored query feeds.
pub mod feed;
mod generate;
mod inheritance;
mod key;
pub mod manifest;
mod manifest_error;
pub use manifest_error::{ManifestError, ManifestErrorType};
mod materialize;
mod operation_reference;
mod output;
mod package_catalog;
pub mod publication;
mod rustfmt;
mod sql;
mod sql_analysis;
mod sqlx_metadata;
mod tofu;

pub use authoring::{
    AUTHORED_MANIFEST, COMPILED_MANIFEST, KCL_ENV, compile_environment, compile_manifest,
    compile_schema_file, is_authored, is_package_root, manifest_package_root,
    package_manifest_path,
};
/// The write log's three fixed statements.
pub use cursor::{
    CursorError, CursorErrorType, CursorV1, CursorValue, decode_cursor, encode_cursor,
};
pub use generate::{
    AuthoredSql, GeneratedFile, GeneratedPackage, GenerationInput, OperationDescription,
    PHYSICAL_SCHEMA_PATH, REQUIRED_SCHEMA_CONTRACT_PATH, StatementTransactionality,
    derive_manifest, generate,
};
pub use materialize::{
    introspect_package, materialize_client_consumers, materialize_host_route_client,
    materialize_package, materialize_package_from_catalog, materialize_package_verified,
    materialize_package_verified_with_catalog,
};
pub use output::{host_route_client_output_root, output_root};
pub use package_catalog::project_package_catalog;
pub use sqlx_metadata::{
    SqlxVerifier, package_database_url, prepare_sqlx_metadata, stage_sqlx_verifier,
};
#[doc(inline)]
pub use tofu::render_project_tofu;
