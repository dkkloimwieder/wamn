//! The pure engine: guards, DDL compilation (reusing wamn-schema-compiler), the lifecycle
//! validation oracle (reusing wamn-schema-control), and an executable
//! [`ApplyPlan`].

use crate::lifecycle::{Environment, LifecycleError, Triple};
use wamn_schema_compiler::{Migration, MigrationPlan};
use wamn_schema_model::Catalog;

use crate::model::{
    ApplyPlan, DestructiveMigration, MigrationError, MigrationRequest, SqlStatement, Value,
};
use crate::sql;

/// The guard + compile step behind the additive planner: run the forward-only /
/// catalog-id / stale-base guards, compile the wamn-schema-compiler plan, and
/// collect advisory warnings.
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
    let destructive = !plan.is_additive();

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
/// (single-applied + stale-base), reusing [`crate::lifecycle::Environment`] as the
/// oracle rather than re-deriving the guards here. The `(org, project, env)`
/// triple is irrelevant to these catalog-scoped guards, so a well-formed
/// placeholder is used.
fn validate_lifecycle(
    current: Option<&Catalog>,
    target: &Catalog,
    expected_base: Option<u32>,
) -> Result<(), MigrationError> {
    let triple = Triple::new("wamn", "migrate", "dev");
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

/// Plan an additive executable migration: the ordered one-transaction
/// statements (DDL + lifecycle advance + history row). The default boundary
/// always refuses destructive operations.
pub fn plan_migration(req: &MigrationRequest) -> Result<ApplyPlan, MigrationError> {
    let c = compile(req)?;
    if c.destructive {
        return Err(MigrationError::Destructive(DestructiveMigration {
            operations: c
                .plan
                .destructive()
                .map(|operation| operation.summary.clone())
                .collect(),
        }));
    }
    let ddl_sql = c
        .plan
        .sql()
        .expect("the additive boundary rejected destructive operations above");
    Ok(build_apply_plan(req, c, ddl_sql))
}

/// Narrow a catalog version onto the `int4` the schema plane stores it in.
///
/// `as` would wrap silently, and a wrapped version names a DIFFERENT release in
/// the promoted and history rows — the exact truncation `wamn-0h0g.15.65` exists
/// to close. This is the last `as` on this value in the tree; every other
/// crossing already uses a checked conversion. A version past `i32::MAX` is a
/// data-integrity bug rather than a runtime condition (versions increment by one
/// per migration), so it stops the program instead of becoming an error variant.
fn storable_version(version: u32) -> i32 {
    i32::try_from(version)
        .expect("a catalog version must fit the int4 the schema plane stores it in")
}

fn build_apply_plan(req: &MigrationRequest, c: Compiled, ddl_sql: String) -> ApplyPlan {
    let env_str = req.environment.as_str().to_string();
    let catalog_id = req.target.catalog_id.clone();
    let base_param = Value::NullableInt(c.from_version.map(storable_version));

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
            Value::Int(storable_version(req.target.version)),
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
            Value::Int(storable_version(req.target.version)),
            Value::Int(c.plan.operations.len() as i32),
            Value::Bool(c.destructive),
            Value::Text(sql::ddl_checksum(&ddl_sql)),
        ],
    });

    ApplyPlan {
        catalog_id,
        environment: env_str,
        from_version: c.from_version,
        to_version: req.target.version,
        destructive: c.destructive,
        warnings: c.warnings,
        statements,
    }
}
