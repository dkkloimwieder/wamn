//! Promotion between environments (3.4).
//!
//! Promotion moves a catalog's **applied** schema from one environment to
//! another of the **same application** along the `dev → canary → prod` order.
//! [`promote`] refuses a cross-application move (a different `(org, project)`)
//! and flags a non-forward environment order. The catalog *content* is a [`Catalog`],
//! whose JSON is already the import/export format (`Catalog::from_json` /
//! `to_json`, owned by 3.1) — this module does not re-invent serialization. What
//! it adds is the *workflow*: diff the source's applied catalog against the
//! target's current applied catalog and compile the migration, **reusing the
//! 3.2 DDL compiler and its additive/destructive confirmation gate verbatim**.
//!
//! The result is a [`PromotionPlan`] — the imported catalog plus a
//! [`MigrationPlan`]. Applying it in the target environment mints a new version
//! there (a storage/control-plane concern); this crate stays pure and emits no
//! DDL of its own.

use wamn_catalog::Catalog;
use wamn_ddl::{CompileError, Confirmation, Migration, MigrationPlan, RequiresConfirmation};
use wamn_registry::Env;

use crate::environment::Environment;

/// Why a promotion could not be planned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromoteError {
    /// The source or target catalog failed to compile to DDL (invalid model or a
    /// reserved-column collision). See [`CompileError`].
    Compile(CompileError),
    /// The source environment has no applied version to promote.
    NothingToPromote,
    /// Source and target are different applications (different `(org, project)`)
    /// — promotion moves a schema between environments of the *same* application.
    DifferentApplication { source: String, target: String },
    /// Source and target track different catalogs.
    CatalogIdMismatch { source: String, target: String },
}

impl std::fmt::Display for PromoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PromoteError::Compile(e) => write!(f, "cannot compile promotion: {e}"),
            PromoteError::NothingToPromote => {
                write!(f, "source environment has no applied version to promote")
            }
            PromoteError::DifferentApplication { source, target } => write!(
                f,
                "cannot promote across applications: source {source:?} vs target {target:?}"
            ),
            PromoteError::CatalogIdMismatch { source, target } => write!(
                f,
                "cannot promote across catalogs: source {source:?} vs target {target:?}"
            ),
        }
    }
}

impl std::error::Error for PromoteError {}

impl From<CompileError> for PromoteError {
    fn from(e: CompileError) -> Self {
        PromoteError::Compile(e)
    }
}

/// A planned promotion: the migration to bring the target environment to the
/// source's schema, plus advisory warnings. Wraps a [`MigrationPlan`] and
/// delegates the additive/destructive safety gate to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotionPlan {
    /// The catalog being promoted (the source's applied version).
    pub catalog_id: String,
    /// The source's applied version number (the schema being promoted in).
    pub source_version: u32,
    /// The target's current applied version, or `None` for a first promotion
    /// (the target gets a fresh `CREATE`).
    pub target_version: Option<u32>,
    /// The compiled migration (may be destructive — gated on [`PromotionPlan::sql`]).
    pub plan: MigrationPlan,
    /// Non-fatal advisories surfaced for review (schema-version skew, a version
    /// regression, …).
    pub warnings: Vec<String>,
}

impl PromotionPlan {
    /// `true` if the migration changes nothing (target already matches source).
    pub fn is_empty(&self) -> bool {
        self.plan.is_empty()
    }

    /// `true` if every operation is additive.
    pub fn is_additive(&self) -> bool {
        self.plan.is_additive()
    }

    /// `true` if the migration contains a destructive operation (so applying it
    /// needs confirmation + a backup checkpoint).
    pub fn requires_confirmation(&self) -> bool {
        self.plan.requires_confirmation()
    }

    /// The DDL to apply the promotion, honoring the 3.2 safety gate.
    pub fn sql(&self, confirm: Confirmation) -> Result<String, RequiresConfirmation> {
        self.plan.sql(confirm)
    }

    /// A human-readable review of the migration, each op tagged additive /
    /// DESTRUCTIVE — plus any promotion warnings.
    pub fn report(&self) -> String {
        let mut out = String::new();
        for w in &self.warnings {
            out.push_str(&format!("[warning] {w}\n"));
        }
        out.push_str(&self.plan.report());
        out
    }
}

/// Plan a promotion of `source` (an applied catalog) into an environment whose
/// current applied catalog is `target_applied` (`None` = a first promotion, so
/// the target gets a fresh `CREATE`). This is the environment-independent core
/// of [`promote`].
///
/// Reuses the 3.2 DDL compiler: [`Migration::create`] for a first promotion or
/// [`Migration::migrate`] for a diff-based one. Both validate the model first,
/// so an invalid catalog is rejected rather than compiled to unsafe DDL.
pub fn promote_catalog(
    source: &Catalog,
    target_applied: Option<&Catalog>,
) -> Result<PromotionPlan, PromoteError> {
    let mut warnings = Vec::new();
    let plan = match target_applied {
        None => Migration::create(source)?,
        Some(target) => {
            if target.catalog_id != source.catalog_id {
                return Err(PromoteError::CatalogIdMismatch {
                    source: source.catalog_id.clone(),
                    target: target.catalog_id.clone(),
                });
            }
            if target.schema_version != source.schema_version {
                warnings.push(format!(
                    "catalog-model version differs: source {:?}, target {:?}",
                    source.schema_version, target.schema_version
                ));
            }
            if source.version <= target.version {
                warnings.push(format!(
                    "source version {} is not newer than the target's applied version {}",
                    source.version, target.version
                ));
            }
            Migration::migrate(target, source)?
        }
    };
    Ok(PromotionPlan {
        catalog_id: source.catalog_id.clone(),
        source_version: source.version,
        target_version: target_applied.map(|t| t.version),
        plan,
        warnings,
    })
}

/// The position of `env` in the canonical `dev → canary → prod` promotion order.
fn env_rank(env: Env) -> usize {
    Env::ALL
        .iter()
        .position(|&e| e == env)
        .expect("env is a member of Env::ALL")
}

/// Plan a promotion of `source`'s applied schema into `target`. Both must be the
/// **same application** (same `(org, project)`) and track the same catalog; the
/// source must have an applied version; the target may be empty (a first
/// promotion).
///
/// This is the first-class-environment entry point: `promote(dev, prod)` diffs
/// `prod`'s applied catalog against `dev`'s applied catalog and compiles the
/// migration. A non-forward environment order (e.g. `prod → dev`) is not an
/// error but adds a warning, since promotion normally runs `dev → canary → prod`.
/// Applying the returned plan and recording the new version in `target` is the
/// caller's step (see [`Environment::add_draft`] / [`Environment::apply`]).
pub fn promote(source: &Environment, target: &Environment) -> Result<PromotionPlan, PromoteError> {
    // Same application: promotion moves a schema between a single application's
    // environments, never across `(org, project)` boundaries.
    if source.org() != target.org() || source.project() != target.project() {
        return Err(PromoteError::DifferentApplication {
            source: format!("{}/{}", source.org(), source.project()),
            target: format!("{}/{}", target.org(), target.project()),
        });
    }
    if source.catalog_id() != target.catalog_id() {
        return Err(PromoteError::CatalogIdMismatch {
            source: source.catalog_id().to_string(),
            target: target.catalog_id().to_string(),
        });
    }
    let src = source
        .applied()
        .ok_or(PromoteError::NothingToPromote)?
        .catalog
        .clone();
    let tgt = target.applied().map(|r| &r.catalog);
    let mut plan = promote_catalog(&src, tgt)?;
    // Environment-order advisory: promotion normally runs dev -> canary -> prod.
    if env_rank(target.env()) <= env_rank(source.env()) {
        plan.warnings.push(format!(
            "promoting {} -> {} is not a forward environment promotion (dev -> canary -> prod)",
            source.env(),
            target.env()
        ));
    }
    Ok(plan)
}
