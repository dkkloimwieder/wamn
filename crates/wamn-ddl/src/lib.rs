//! wamn DDL compiler (3.2).
//!
//! Turns the canonical catalog model (3.1, [`wamn_catalog`]) into Postgres DDL:
//! a whole catalog into `CREATE` statements, or a catalog *diff* into an ordered
//! [`MigrationPlan`] of `ALTER`s. Every operation is classified
//! [`Safety::Additive`] or [`Safety::Destructive`]; the plan **applies additive
//! changes freely but refuses destructive ones** unless the caller confirms them
//! and asserts a backup checkpoint (the "additive by default; destructive needs
//! explicit confirmation + backup" rule).
//!
//! Scope: this crate *emits and classifies* DDL. It does not execute it — the
//! live transactional apply, versioned migration history, and rollback are the
//! migration engine (2.5); the backup/PITR mechanism is hosting (2.3 / 10.3);
//! the draft→staged→applied lifecycle is 3.4; per-role RLS rules are 3.5. It
//! *does* emit the platform multi-tenancy floor (tenant column + FORCE RLS +
//! the `app.tenant` policy) so generated tables are tenant-safe by default.
//!
//! ```
//! use wamn_catalog::Catalog;
//! use wamn_ddl::{Migration, Confirmation};
//!
//! # fn go(catalog: &Catalog) -> Result<(), Box<dyn std::error::Error>> {
//! let plan = Migration::create(catalog)?;
//! let sql = plan.sql(Confirmation::None)?; // a fresh CREATE is all-additive
//! # let _ = sql;
//! # Ok(())
//! # }
//! ```

mod emit;
mod plan;
pub mod sql;

pub use plan::{Confirmation, MigrationPlan, Operation, RequiresConfirmation, Safety};

use wamn_catalog::{Catalog, Issue};

/// Why a catalog could not be compiled to DDL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompileError {
    /// The catalog failed structural validation (3.1). Fix the model first.
    InvalidCatalog(Vec<Issue>),
    /// A user field reuses a reserved managed column name (`id` / `tenant_id`).
    ReservedColumn { entity: String, field: String },
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompileError::InvalidCatalog(issues) => {
                write!(f, "catalog is invalid ({} error(s)): ", issues.len())?;
                for (i, issue) in issues.iter().enumerate() {
                    if i > 0 {
                        write!(f, "; ")?;
                    }
                    write!(f, "{issue}")?;
                }
                Ok(())
            }
            CompileError::ReservedColumn { entity, field } => write!(
                f,
                "entity {entity:?} field {field:?} reuses a reserved managed column name (id / tenant_id)"
            ),
        }
    }
}

impl std::error::Error for CompileError {}

/// The DDL compiler entry point.
pub struct Migration;

impl Migration {
    /// Compile a whole catalog into a `CREATE` plan (all additive) — the initial
    /// materialization (POC-DM1).
    pub fn create(catalog: &Catalog) -> Result<MigrationPlan, CompileError> {
        check(catalog)?;
        Ok(emit::create_plan(catalog))
    }

    /// Compile the migration from `old` to `new` (driven by the catalog diff).
    /// Both versions must be valid; the resulting plan may contain destructive
    /// operations (gated by [`MigrationPlan::sql`]).
    pub fn migrate(old: &Catalog, new: &Catalog) -> Result<MigrationPlan, CompileError> {
        check(old)?;
        check(new)?;
        Ok(emit::migrate_plan(old, new))
    }
}

/// Validate the catalog and reject reserved managed-column collisions.
fn check(catalog: &Catalog) -> Result<(), CompileError> {
    if let Err(issues) = catalog.validate() {
        return Err(CompileError::InvalidCatalog(issues));
    }
    for e in &catalog.entities {
        for f in &e.fields {
            if emit::RESERVED_COLUMNS.contains(&f.name.to_ascii_lowercase().as_str()) {
                return Err(CompileError::ReservedColumn {
                    entity: e.id.clone(),
                    field: f.name.clone(),
                });
            }
        }
    }
    Ok(())
}
