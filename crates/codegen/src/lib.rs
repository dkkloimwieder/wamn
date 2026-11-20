//! Generates deterministic package artifacts from migration-derived schema IR.
//!
//! Core generation is a deterministic transformation: callers provide the
//! normalized [`CatalogIr`](wamn_schema_introspection::ir::CatalogIr), exact manifest bytes, and authored SQL bytes.
//!
//! Generated identifiers follow
//! `docs/architecture/naming.md`. Migration introspection owns
//! schema-to-IR normalization; that IR is generation's only structural input.
//!
//! Migrations author PostgreSQL schema selection. Generated and authored query
//! corpus files therefore use unqualified relations and inherit the host-owned
//! search path frozen by
//! `wit/wamn-postgres-0.4/package.wit`.

mod authored_wit;
mod authoring;
/// `wamn build`: the two-pass package build (docs/operations/building.md#wamn-build).
pub mod build;
mod client_fields;
pub mod client_ir;
pub mod client_plan;
mod client_route;
/// The TypeScript client emitter. Browser bindings from the same contract IR.
pub mod client_ts;
mod describe;
mod describe_cache;
pub mod fingerprint;
/// Catalog facts for authored query feeds.
pub mod feed;
mod generate;
mod inheritance;
pub mod kcl;
mod key;
mod materialize;
mod output;
mod package_catalog;
pub mod publication;
mod sql;
mod sql_analysis;

pub use authoring::{
    AUTHORED_MANIFEST, COMPILED_MANIFEST, compile_manifest, is_authored, is_package_root,
    manifest_package_root, package_manifest_path,
};
pub use generate::{
    AuthoredSql, GeneratedFile, GeneratedPackage, GenerationInput, OperationDescription,
    StatementTransactionality, derive_manifest, generate,
};
pub use materialize::introspect_package;
pub use output::{is_input_path, output_root, package_workspace, source_root};
pub use package_catalog::project_package_catalog;

mod history;

pub mod migrations;
