//! The engine's value types: the request, the executable apply plan (its
//! `$n`-parameterized statements), the dry-run report, the generated rollback
//! plan, and the error taxonomy.

use wamn_catalog::Catalog;
use wamn_ddl::{CompileError, MigrationPlan, RequiresConfirmation};
use wamn_schema::LifecycleError;

pub use wamn_ddl::Confirmation;
pub use wamn_schema::Env;

/// A migration to plan: bring `target` live in `(tenant, environment)`, diffing
/// it against the `current` applied catalog (`None` = a first materialization).
/// `expected_base` is the applied version the caller asserts the target was
/// branched from — the 3.4 stale-base guard checks it against the actual current
/// applied version. `confirm` is the 3.2 backup gate, honored verbatim.
#[derive(Debug, Clone, Copy)]
pub struct MigrationRequest<'a> {
    pub tenant: &'a str,
    pub environment: Env,
    pub current: Option<&'a Catalog>,
    pub target: &'a Catalog,
    pub expected_base: Option<u32>,
    pub confirm: Confirmation,
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
/// the wamn-ddl name-freeing aside-renames back with zero `wamn_mig_drop_*`
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
    /// `true` if the DDL contains a destructive operation (it then carries the
    /// backup-checkpoint marker and was gated behind `ConfirmedWithBackup`).
    pub destructive: bool,
    /// Advisory notes surfaced for review (a version bump with no structural
    /// change, a catalog-model version skew, …).
    pub warnings: Vec<String>,
    /// The ordered statements to execute in one transaction.
    pub statements: Vec<SqlStatement>,
}

/// A dry run: what the migration **would** do, computed without touching the
/// database. Unlike [`crate::plan_migration`], a dry run does not gate on the
/// confirmation — it *reports* that a destructive plan would require one.
#[derive(Debug, Clone)]
pub struct MigrationReport {
    pub catalog_id: String,
    pub environment: String,
    pub from_version: Option<u32>,
    pub to_version: u32,
    pub destructive: bool,
    pub warnings: Vec<String>,
    /// The wamn-ddl operation report — each op tagged additive / DESTRUCTIVE.
    pub ddl_report: String,
    /// The generated rollback for this migration.
    pub rollback: RollbackPlan,
}

impl MigrationReport {
    /// A human-readable rendering of the whole dry run.
    pub fn render(&self) -> String {
        let from = self
            .from_version
            .map_or_else(|| "(none)".to_string(), |v| v.to_string());
        let mut out = format!(
            "migration {from} -> {} for catalog {:?} in environment {}\n",
            self.to_version, self.catalog_id, self.environment
        );
        out.push_str(if self.destructive {
            "  DESTRUCTIVE — requires --confirm-with-backup\n"
        } else {
            "  additive\n"
        });
        for w in &self.warnings {
            out.push_str(&format!("  [warning] {w}\n"));
        }
        out.push_str("\n-- DDL --\n");
        out.push_str(&self.ddl_report);
        out.push_str("\n-- rollback --\n");
        out.push_str(&self.rollback.report());
        out
    }
}

/// The generated rollback for a migration: an **inverse forward-migration** back
/// to the prior version (dropping the migration's new additions is destructive,
/// so applying it carries the 3.2 confirmation gate), plus a restore-to-last-dump
/// pointer for data a forward rollback cannot recover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackPlan {
    /// The inverse migration (`target -> current_applied`). Empty for a first
    /// materialization — there is no prior version, so rollback is a drop /
    /// restore (see [`RollbackPlan::note`]).
    pub plan: MigrationPlan,
    /// Human-readable guidance, including the restore-to-last-dump (wamn-q3n.11)
    /// pointer for data the forward rollback drops.
    pub note: String,
}

impl RollbackPlan {
    /// `true` if the inverse migration has no operations (a first
    /// materialization, or a metadata-only version bump).
    pub fn is_empty(&self) -> bool {
        self.plan.is_empty()
    }

    /// The DDL to run the rollback, honoring the 3.2 gate (the inverse is
    /// destructive, so it needs `ConfirmedWithBackup`).
    pub fn sql(&self, confirm: Confirmation) -> Result<String, RequiresConfirmation> {
        self.plan.sql(confirm)
    }

    /// A human-readable review: the inverse operations, then the guidance note.
    pub fn report(&self) -> String {
        let mut out = self.plan.report();
        out.push_str(&self.note);
        out.push('\n');
        out
    }
}

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
    /// The migration is destructive and was not confirmed with a backup
    /// checkpoint (the 3.2 gate, honored verbatim).
    RequiresConfirmation(RequiresConfirmation),
    /// A lifecycle transition the 3.4 model rejects (surfaced via
    /// [`wamn_schema::Environment`] as the validation oracle).
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
            MigrationError::RequiresConfirmation(e) => write!(f, "{e}"),
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

impl From<RequiresConfirmation> for MigrationError {
    fn from(e: RequiresConfirmation) -> Self {
        MigrationError::RequiresConfirmation(e)
    }
}

impl From<LifecycleError> for MigrationError {
    fn from(e: LifecycleError) -> Self {
        MigrationError::Lifecycle(e)
    }
}
