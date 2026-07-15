//! The pure engine: guards, DDL compilation (reusing wamn-ddl), the lifecycle
//! validation oracle (reusing wamn-schema), and the three outputs — an
//! executable [`ApplyPlan`], a [`MigrationReport`] dry run, and a generated
//! [`RollbackPlan`].

use wamn_catalog::Catalog;
use wamn_ddl::{Migration, MigrationPlan};
use wamn_schema::{Env as SchemaEnv, Environment, LifecycleError, Triple};

use crate::model::{
    ApplyPlan, MigrationError, MigrationReport, MigrationRequest, RollbackPlan, SqlStatement, Value,
};
use crate::sql;

/// The shared guard + compile step behind [`plan_migration`] and [`dry_run`]:
/// run the forward-only / catalog-id / stale-base guards, compile the wamn-ddl
/// plan, and collect advisory warnings.
struct Compiled {
    plan: MigrationPlan,
    destructive: bool,
    from_version: Option<u32>,
    warnings: Vec<String>,
}

fn compile(req: &MigrationRequest) -> Result<Compiled, MigrationError> {
    // Catalog-id + forward-only guards (2.5 concerns the 3.4 lifecycle is
    // version-agnostic about).
    let from_version = match req.current {
        Some(cur) => {
            if cur.catalog_id != req.target.catalog_id {
                return Err(MigrationError::CatalogIdMismatch {
                    current: cur.catalog_id.clone(),
                    target: req.target.catalog_id.clone(),
                });
            }
            if req.target.version == cur.version {
                return Err(MigrationError::AlreadyApplied {
                    version: cur.version,
                });
            }
            if req.target.version < cur.version {
                return Err(MigrationError::NotForward {
                    target: req.target.version,
                    current: cur.version,
                });
            }
            Some(cur.version)
        }
        None => None,
    };

    // Reuse the 3.4 single-applied + stale-base guards as the oracle.
    validate_lifecycle(req.current, req.target, req.expected_base)?;

    // Compile the DDL: a fresh CREATE, or the diff.
    let plan = match req.current {
        None => Migration::create(req.target)?,
        Some(cur) => Migration::migrate(cur, req.target)?,
    };
    let destructive = plan.requires_confirmation();

    let mut warnings = Vec::new();
    if plan.is_empty() {
        warnings.push(format!(
            "version {} has no structural changes — a metadata-only version bump",
            req.target.version
        ));
    }
    if let Some(cur) = req.current
        && cur.schema_version != req.target.schema_version
    {
        warnings.push(format!(
            "catalog-model version differs: current {:?}, target {:?}",
            cur.schema_version, req.target.schema_version
        ));
    }

    Ok(Compiled {
        plan,
        destructive,
        from_version,
        warnings,
    })
}

/// Validate the target-over-current transition against the 3.4 lifecycle model
/// (single-applied + stale-base), reusing [`wamn_schema::Environment`] as the
/// oracle rather than re-deriving the guards here. The `(org, project, env)`
/// triple is irrelevant to these catalog-scoped guards, so a well-formed
/// placeholder is used.
fn validate_lifecycle(
    current: Option<&Catalog>,
    target: &Catalog,
    expected_base: Option<u32>,
) -> Result<(), MigrationError> {
    let triple = Triple::new("wamn", "migrate", SchemaEnv::Dev);
    let mut env = Environment::new(triple, &target.catalog_id);

    // Replay the DB state: the current applied version, if any.
    if let Some(cur) = current {
        env.add_draft(cur.clone(), None)?;
        env.stage(cur.version)?;
        env.apply(cur.version)?;
    }

    // The target as a staged candidate branched from `expected_base` (defaulting
    // to the current applied version). apply() runs the stale-base + single-
    // applied guards.
    let base = expected_base.or_else(|| current.map(|c| c.version));
    env.add_draft(target.clone(), base)?;
    env.stage(target.version)?;
    env.apply(target.version).map_err(|e| match e {
        LifecycleError::StaleBase {
            base,
            current_applied,
            ..
        } => MigrationError::StaleBase {
            expected_base: base,
            current_applied,
        },
        other => MigrationError::Lifecycle(other),
    })
}

/// Plan an executable migration: the ordered one-transaction statements (DDL +
/// the lifecycle advance + the history row). Refuses a destructive plan unless
/// `confirm` is [`Confirmation::ConfirmedWithBackup`] (the 3.2 gate). The driver
/// executes the returned statements inside a single transaction.
pub fn plan_migration(req: &MigrationRequest) -> Result<ApplyPlan, MigrationError> {
    let c = compile(req)?;
    // Honor the 3.2 backup gate: a destructive plan needs ConfirmedWithBackup,
    // and the emitted DDL then carries the backup-checkpoint marker.
    let ddl_sql = c.plan.sql(req.confirm)?;

    let env_str = req.environment.as_str().to_string();
    let catalog_id = req.target.catalog_id.clone();
    let base_param = Value::NullableInt(c.from_version.map(|v| v as i32));

    let mut statements = Vec::new();

    // 1. The DDL — a param-free multi-statement batch. Skipped when there is no
    //    structural change (a metadata-only version bump still advances the
    //    lifecycle + records history).
    if !c.plan.is_empty() {
        statements.push(SqlStatement {
            summary: format!("apply DDL ({} operation(s))", c.plan.operations.len()),
            sql: ddl_sql.clone(),
            params: vec![],
        });
    }

    // 2. Demote the current applied version (no-op when nothing is applied).
    statements.push(SqlStatement {
        summary: "demote the current applied version to superseded".into(),
        sql: sql::demote_current_applied_sql(),
        params: vec![
            Value::Text(req.tenant.to_string()),
            Value::Text(catalog_id.clone()),
            Value::Text(env_str.clone()),
        ],
    });

    // 3. Promote the target to applied, storing its catalog document.
    statements.push(SqlStatement {
        summary: format!("record version {} as applied", req.target.version),
        sql: sql::upsert_applied_version_sql(),
        params: vec![
            Value::Text(req.tenant.to_string()),
            Value::Text(catalog_id.clone()),
            Value::Int(req.target.version as i32),
            Value::Text(env_str.clone()),
            Value::Text(req.target.schema_version.clone()),
            Value::NullableText(req.target.name.clone()),
            base_param.clone(),
            Value::Text(req.target.to_json()),
        ],
    });

    // 4. Append the immutable history row.
    statements.push(SqlStatement {
        summary: "record the migration in schema_migrations".into(),
        sql: sql::record_migration_sql(),
        params: vec![
            Value::Text(req.tenant.to_string()),
            Value::Text(catalog_id.clone()),
            Value::Text(env_str.clone()),
            base_param,
            Value::Int(req.target.version as i32),
            Value::Text(sql::confirmation_sql(req.confirm).to_string()),
            Value::Int(c.plan.operations.len() as i32),
            Value::Bool(c.destructive),
            Value::Text(sql::ddl_checksum(&ddl_sql)),
        ],
    });

    Ok(ApplyPlan {
        catalog_id,
        environment: env_str,
        from_version: c.from_version,
        to_version: req.target.version,
        destructive: c.destructive,
        warnings: c.warnings,
        statements,
    })
}

/// Report what the migration would do without touching the database. Unlike
/// [`plan_migration`], a dry run does **not** gate on the confirmation — it
/// reports the destructiveness so an operator can decide. Includes the generated
/// rollback plan.
pub fn dry_run(req: &MigrationRequest) -> Result<MigrationReport, MigrationError> {
    let c = compile(req)?;
    let rollback = rollback_plan(req)?;
    Ok(MigrationReport {
        catalog_id: req.target.catalog_id.clone(),
        environment: req.environment.as_str().to_string(),
        from_version: c.from_version,
        to_version: req.target.version,
        destructive: c.destructive,
        warnings: c.warnings,
        ddl_report: c.plan.report(),
        rollback,
    })
}

/// Generate the rollback for `req`'s migration: an inverse forward-migration back
/// to the current applied version. For a first materialization there is no prior
/// version, so the plan is empty and the note points at drop / restore. The
/// inverse drops the migration's additions and so is destructive — apply it with
/// [`RollbackPlan::sql`] under `ConfirmedWithBackup`.
pub fn rollback_plan(req: &MigrationRequest) -> Result<RollbackPlan, MigrationError> {
    match req.current {
        None => Ok(RollbackPlan {
            plan: MigrationPlan::default(),
            note: "first materialization: there is no prior version to roll back to. \
                   Rollback = drop the created objects, or restore-to-last-dump (wamn-q3n.11)."
                .into(),
        }),
        Some(cur) => {
            // The inverse: migrate the target back to the current applied catalog.
            let plan = Migration::migrate(req.target, cur)?;
            Ok(RollbackPlan {
                plan,
                note: "rollback = this inverse forward-migration back to the prior version \
                       (destructive: apply with a confirmed backup). Data written under \
                       columns/tables this drops is NOT recoverable by the forward rollback \
                       — use restore-to-last-dump (wamn-q3n.11) for that."
                    .into(),
            })
        }
    }
}
