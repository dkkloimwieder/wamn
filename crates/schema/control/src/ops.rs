//! Operations-only catalog planning.
//!
//! The default migration boundary is additive-only. This module exposes the
//! destructive compiler capability only when the crate's `ops` feature is
//! selected, for target reconciliation and read-only impact planning.

use wamn_schema_compiler::{CompileError, Migration, MigrationPlan};
use wamn_schema_model::Catalog;

use crate::{ApplyPlan, MigrationError, MigrationRequest};

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

/// Plan target reconciliation after the operations caller has verified its
/// durable backup attestation against the locked current-applied version and
/// the requested target version.
pub fn plan_target_reconciliation(
    request: &MigrationRequest<'_>,
) -> Result<ApplyPlan, MigrationError> {
    crate::engine::plan_target_reconciliation(request)
}
