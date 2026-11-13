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
mod describe;
/// Catalog facts for authored query feeds.
pub mod feed;
mod generate;
mod inheritance;
pub mod kcl;
mod key;
pub mod manifest;
mod manifest_error;
pub use manifest_error::{ManifestError, ManifestErrorType};
mod materialize;
mod output;
mod package_catalog;
pub mod publication;
mod rustfmt;
mod sql;
mod sql_analysis;
pub use sql_analysis::admission as declared_sql;

pub use authoring::{
    AUTHORED_MANIFEST, COMPILED_MANIFEST, compile_manifest, is_authored, is_package_root,
    manifest_package_root, package_manifest_path,
};
pub use generate::{
    AuthoredSql, GeneratedFile, GeneratedPackage, GenerationInput, OperationDescription,
    PHYSICAL_SCHEMA_PATH, REQUIRED_SCHEMA_CONTRACT_PATH, StatementTransactionality,
    derive_manifest, generate,
};
pub use materialize::{
    introspect_package, materialize_client_consumers, materialize_host_route_client,
};
pub use output::{host_route_client_output_root, output_root, package_workspace};
pub use package_catalog::project_package_catalog;
