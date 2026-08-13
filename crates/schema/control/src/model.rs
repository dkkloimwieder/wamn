//! The engine's value types: the request, the executable apply plan (its
//! `$n`-parameterized statements), and the error taxonomy.

use crate::lifecycle::LifecycleError;
use wamn_schema_compiler::CompileError;
use wamn_schema_model::Catalog;

pub use crate::lifecycle::Env;

/// A migration to plan: bring `target` live in `(tenant, environment)`, diffing
/// it against the `current` applied catalog (`None` = a first materialization).
/// `expected_base` is the applied version the caller asserts the target was
/// branched from — the 3.4 stale-base guard checks it against the actual current
/// applied version. The default planner accepts additive changes only.
#[derive(Debug, Clone)]
pub struct MigrationRequest<'a> {
    pub tenant: &'a str,
    pub environment: Env,
    pub current: Option<&'a Catalog>,
    pub target: &'a Catalog,
    pub expected_base: Option<u32>,
}

/// A positional bind value for an [`SqlStatement`]. The engine emits
/// `$n`-parameterized SQL (SR3) and hands the driver the values to bind in order,
/// so identifiers stay pinned and values never interpolate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Text(String),
    NullableText(Option<String>),
    Int(i32),
    NullableInt(Option<i32>),
    Bool(bool),
}

/// One statement in an [`ApplyPlan`]: `sql` (with `$n` placeholders) and the
/// positional `params` to bind. A `params`-free statement is the DDL script (a
/// multi-statement batch); a parameterized one is a metadata write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlStatement {
    pub summary: String,
    pub sql: String,
    pub params: Vec<Value>,
}

/// The executable migration: the ordered statements to run inside **one
/// transaction**. The whole plan applies atomically, so a mid-plan failure rolls
/// the wamn-schema-compiler name-freeing aside-renames back with zero `wamn_mig_drop_*`
/// residue (the R9c one-transaction invariant — see the crate docs; it holds
/// while the compiler emits no non-transactional step such as
/// `CREATE INDEX CONCURRENTLY`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyPlan {
    pub catalog_id: String,
    pub environment: String,
    /// `None` for a first materialization (a fresh `CREATE`).
    pub from_version: Option<u32>,
    pub to_version: u32,
    /// `true` if the operations-only target-reconciliation plan contains a
    /// destructive operation. Default plans are always `false`.
    pub destructive: bool,
    /// Advisory notes surfaced for review (a version bump with no structural
    /// change, a catalog-model version skew, …).
    pub warnings: Vec<String>,
    /// The ordered statements to execute in one transaction.
    pub statements: Vec<SqlStatement>,
}

/// A destructive diff reached the additive-only public migration boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestructiveMigration {
    /// Summaries of the destructive operations, in execution order.
    pub operations: Vec<String>,
}

impl std::fmt::Display for DestructiveMigration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "migration has {} destructive operation(s); default catalog migration is additive-only: {}",
            self.operations.len(),
            self.operations.join("; ")
        )
    }
}

impl std::error::Error for DestructiveMigration {}

/// Why a migration could not be planned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationError {
    /// The DDL compiler rejected the model — an invalid catalog, a reserved
    /// managed-column collision, or a rename/drop cycle. See [`CompileError`].
    Compile(CompileError),
    /// The current applied catalog and the target track different catalogs.
    CatalogIdMismatch { current: String, target: String },
    /// The target version is older than the current applied version — apply only
    /// moves **forward**.
    NotForward { target: u32, current: u32 },
    /// The target version equals the current applied version — already applied
    /// (migrations are versioned; re-applying a version is refused).
    AlreadyApplied { version: u32 },
    /// The target was branched from a version that is no longer the current
    /// applied one — rebase before applying (the 3.4 stale-base guard).
    StaleBase {
        expected_base: Option<u32>,
        current_applied: Option<u32>,
    },
    /// The public migration path accepts additive changes only.
    Destructive(DestructiveMigration),
    /// A lifecycle transition the 3.4 model rejects (surfaced via
    /// [`crate::lifecycle::Environment`] as the validation oracle).
    Lifecycle(LifecycleError),
}

impl std::fmt::Display for MigrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MigrationError::Compile(e) => write!(f, "cannot compile migration: {e}"),
            MigrationError::CatalogIdMismatch { current, target } => write!(
                f,
                "catalog id mismatch: current applied is {current:?}, target is {target:?}"
            ),
            MigrationError::NotForward { target, current } => write!(
                f,
                "target version {target} is not newer than the current applied version {current} — migrations only move forward"
            ),
            MigrationError::AlreadyApplied { version } => {
                write!(f, "version {version} is already the applied version")
            }
            MigrationError::StaleBase {
                expected_base,
                current_applied,
            } => write!(
                f,
                "the target's base ({expected_base:?}) is not the current applied version ({current_applied:?}) — rebase before applying"
            ),
            MigrationError::Destructive(e) => write!(f, "{e}"),
            MigrationError::Lifecycle(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for MigrationError {}

impl From<CompileError> for MigrationError {
    fn from(e: CompileError) -> Self {
        MigrationError::Compile(e)
    }
}

impl From<LifecycleError> for MigrationError {
    fn from(e: LifecycleError) -> Self {
        MigrationError::Lifecycle(e)
    }
}
