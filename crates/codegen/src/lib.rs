//! Generates deterministic package artifacts from migration-derived schema IR.
//!
//! Core generation is a deterministic transformation: callers provide the
//! normalized [`CatalogIr`](wamn_schema::ir::CatalogIr), exact manifest bytes, and authored SQL bytes.
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
pub mod build_database;
pub mod catalog;
mod describe;
mod describe_cache;
/// Catalog facts for authored query feeds.
pub mod feed;
pub mod fingerprint;
mod generate;
mod inheritance;
pub mod kcl;
mod key;
mod materialize;
mod openapi;
mod output;
mod package_catalog;
pub mod publication;
mod sql;
mod sql_analysis;
mod sql_quote;

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

/// Component directory in a package build.
pub const COMPONENTS_DIRECTORY: &str = "components";

/// Relative component artifact path shared by builds and their consumers.
pub fn component_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(COMPONENTS_DIRECTORY).join(format!("{name}.wasm"))
}
