//! Operations-only catalog planning.
//!
//! The default migration boundary is additive-only. This module exposes the
//! destructive compiler classification only when the crate's `ops` feature is
//! selected, for read-only impact planning.

use wamn_schema_compiler::{CompileError, Migration, MigrationPlan};
use wamn_schema_model::Catalog;

/// Compile a classified catalog diff for read-only operations impact analysis.
pub fn compile_migration(
    current: Option<&Catalog>,
    target: &Catalog,
) -> Result<MigrationPlan, CompileError> {
    match current {
        Some(current) => Migration::migrate(current, target),
        None => Migration::create(target),
    }
}
