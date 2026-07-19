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
//!
//! ## SR12 — what the pure tests cover, and what they cannot
//!
//! This crate's tests exercise the **decision** (which statement, what shape,
//! which binds); they cannot exercise the **statement** — the pure model has no
//! planner, isolation level, lock manager, or RLS. A statement can be modelled
//! correctly here and still misbehave live: `wamn-run-queue`'s `claim_batch_sql`
//! passed every pure test while the real statement over-claimed on a
//! plan-dependent `SKIP LOCKED` re-scan — the `AS MATERIALIZED` fix is a
//! property of the emitted SQL no pure test can observe. Convention (SR12a):
//! every composed or plan-sensitive statement carries a comment naming what the
//! pure tests do NOT cover; the live half is the throwaway-PG gates over the
//! real prepared-statement path (SR12b).

mod emit;
mod outbox;
mod plan;
pub mod sql;

pub use outbox::OutboxOptions;
pub use plan::{Confirmation, MigrationPlan, Operation, RequiresConfirmation, Safety};

use wamn_catalog::{Catalog, Issue};

/// Why a catalog could not be compiled to DDL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompileError {
    /// The catalog failed structural validation (3.1). Fix the model first.
    InvalidCatalog(Vec<Issue>),
    /// A user field reuses a reserved managed column name (`id` / `tenant_id`).
    ReservedColumn { entity: String, field: String },
    /// The outbox schema option is not a bare identifier. It is embedded inside
    /// the trigger function's dollar-quoted body, so anything beyond
    /// `[A-Za-z_][A-Za-z0-9_]*` is refused rather than quoted.
    InvalidOutboxSchema { schema: String },
    /// The migration's table renames form a cycle (a swap: A -> B and B -> A
    /// in one version bump). No order of plain renames can apply it — split
    /// the evolution into two version bumps (rename one table aside first).
    TableRenameCycle { names: Vec<String> },
    /// One entity's column renames form a cycle (a swap) — the column-level
    /// analog of [`CompileError::TableRenameCycle`]; split it into two
    /// version bumps.
    ColumnRenameCycle { entity: String, names: Vec<String> },
    /// The tables being dropped in this migration form a foreign-key cycle
    /// (mutual `Reference`s among the removed set), so no `DROP TABLE` order
    /// unwinds the FKs without dropping a constraint first. Rejected in v1 (as
    /// the rename cycles are) — break the cycle by dropping one side's
    /// reference field in an earlier version bump.
    DropCycle { entities: Vec<String> },
    /// A dropped table's name is reclaimed by this migration, and the
    /// transient aside-name the plan needs (`wamn_mig_drop_<name>`) is itself
    /// a real table in the old or new catalog.
    TempNameCollision { name: String },
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
            CompileError::InvalidOutboxSchema { schema } => write!(
                f,
                "outbox schema {schema:?} is not a bare identifier ([A-Za-z_][A-Za-z0-9_]*)"
            ),
            CompileError::TableRenameCycle { names } => write!(
                f,
                "table renames form a cycle ({}): split the evolution into two version bumps",
                names.join(" -> ")
            ),
            CompileError::ColumnRenameCycle { entity, names } => write!(
                f,
                "column renames on entity {entity:?} form a cycle ({}): split the evolution into two version bumps",
                names.join(" -> ")
            ),
            CompileError::DropCycle { entities } => write!(
                f,
                "dropped tables form a foreign-key cycle ({}): drop one side's reference field in an earlier version bump",
                entities.join(" <-> ")
            ),
            CompileError::TempNameCollision { name } => write!(
                f,
                "cannot rename a dropped table aside: transient name {name:?} is already a table in the catalog"
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
    ///
    /// Operations are additive-first / destructive-last, EXCEPT a name-freeing
    /// preamble: tables, indexes, and unique-constraint backing indexes share
    /// one Postgres relation namespace, so when this migration both frees a
    /// name (rename / drop) and reclaims it (create / add), the freeing side
    /// is hoisted ahead of the adds — a dropped-and-reclaimed table is renamed
    /// aside (its `DROP TABLE` stays last, keeping FK unwind order), and ALL
    /// table renames run first, dependency-ordered. A rename cycle (a swap)
    /// is rejected with [`CompileError::TableRenameCycle`].
    pub fn migrate(old: &Catalog, new: &Catalog) -> Result<MigrationPlan, CompileError> {
        check(old)?;
        check(new)?;
        emit::migrate_plan(old, new)
    }

    /// The outbox row-event trigger plan (5.14 / D4 producer side): one shared
    /// trigger function + one `AFTER INSERT OR UPDATE OR DELETE` trigger per
    /// entity table, inserting the event row into `<options.schema>.outbox`
    /// inside the user's transaction. Opt-in and uniform — a separate plan the
    /// provisioning path composes with [`Migration::create`] for projects whose
    /// database carries the run schema (deploy/sql/run-state.sql + run-queue.sql);
    /// deliberately not part of `create`/`migrate`, whose consumers' schemas
    /// have no outbox. All additive and idempotent (`CREATE OR REPLACE` +
    /// constant trigger name), so re-apply it on every catalog version: added
    /// entities gain their trigger, renamed tables keep exactly one, and
    /// dropped tables take theirs with them. The function-create operation is
    /// catalog-scoped and carries an empty `entity` attribution.
    pub fn outbox_triggers(
        catalog: &Catalog,
        options: &OutboxOptions,
    ) -> Result<MigrationPlan, CompileError> {
        check(catalog)?;
        if !outbox::valid_bare_ident(&options.schema) {
            return Err(CompileError::InvalidOutboxSchema {
                schema: options.schema.clone(),
            });
        }
        Ok(outbox::outbox_triggers_plan(catalog, options))
    }

    /// The opt-out counterpart of [`Migration::outbox_triggers`]: drop every
    /// entity table's row-event trigger, then the shared function. Destructive
    /// (row-event flows registered on these tables silently stop firing), so
    /// the plan is gated behind [`Confirmation::ConfirmedWithBackup`].
    pub fn drop_outbox_triggers(catalog: &Catalog) -> Result<MigrationPlan, CompileError> {
        check(catalog)?;
        Ok(outbox::drop_outbox_triggers_plan(catalog))
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
                    entity: e.id.to_string(),
                    field: f.name.clone(),
                });
            }
        }
    }
    Ok(())
}
