//! The `migrate-catalog` subcommand (2.5): the **effect shell** for the
//! `wamn-migrate` engine — it reads the current applied catalog from a project
//! database, calls the pure planner, and executes the resulting one-transaction
//! [`ApplyPlan`] (DDL + the lifecycle advance + the history row).
//!
//! The engine ([`wamn_migrate`]) is pure (guards, DDL via wamn-ddl, the
//! lifecycle via wamn-schema, `$n`-parameterized SQL); this shell holds the
//! connection. Two modes:
//!
//! * `--dry-run` — read + plan + print the report (DDL + rollback), touching
//!   nothing;
//! * apply — read the current applied version (locked `FOR UPDATE`), plan, and
//!   run the whole plan in **one transaction** so a mid-plan failure rolls back
//!   with zero residue (the R9c invariant).
//!
//! A destructive migration is refused unless `--confirm-with-backup` is passed
//! (the 3.2 gate, honored by the engine). Connects as a **superuser** (the DDL
//! creates tables + policies + grants, like `publish-catalog --provision`).

use std::path::PathBuf;

use anyhow::{Context as _, bail};
use clap::Args;
use tokio_postgres::NoTls;
use tokio_postgres::types::ToSql;

use wamn_migrate::{
    Catalog, Confirmation, MigrationError, MigrationRequest, Value, dry_run, plan_migration, sql,
};

use crate::provision_project_env::EnvArg;

#[derive(Debug, Args)]
pub struct MigrateCatalogArgs {
    /// Superuser Postgres URL to the PROJECT database (holds the `catalog` schema
    /// and the data schema). The DDL creates tables/policies/grants, so a
    /// superuser (or the schema owner) is required. Env `WAMN_PG_ADMIN_URL`.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: String,

    /// Tenant claim the catalog + data rows are scoped to (`app.tenant`).
    #[arg(long)]
    pub tenant: String,

    /// Environment: `dev`, `canary`, or `prod`.
    #[arg(long, value_enum, default_value = "dev")]
    pub environment: EnvArg,

    /// The data schema the generated tables live in (unqualified DDL resolves
    /// here; the `catalog` metadata schema is fixed).
    #[arg(long, default_value = "public")]
    pub schema: String,

    /// Path to the target catalog JSON (crates/wamn-catalog `Catalog`).
    #[arg(long)]
    pub target: PathBuf,

    /// The applied version the target was branched from — the 3.4 stale-base
    /// guard checks it against the actual current applied version. Omit to
    /// default to "branched from the current applied version".
    #[arg(long)]
    pub base: Option<u32>,

    /// Print the plan (DDL + rollback) without applying it.
    #[arg(long)]
    pub dry_run: bool,

    /// Acknowledge a destructive migration + assert a backup checkpoint was taken
    /// (the 3.2 gate). Required to apply a plan that drops/retypes.
    #[arg(long)]
    pub confirm_with_backup: bool,
}

pub async fn run(args: MigrateCatalogArgs) -> anyhow::Result<()> {
    // A bare-identifier data schema (it is interpolated into SET search_path).
    if !is_bare_ident(&args.schema) {
        bail!(
            "--schema must be a bare identifier [a-z_][a-z0-9_]*: {:?}",
            args.schema
        );
    }
    let target_json = std::fs::read_to_string(&args.target)
        .with_context(|| format!("read target catalog {}", args.target.display()))?;
    let target = Catalog::from_json(&target_json).context("parse target catalog JSON")?;

    let env: wamn_registry::Env = args.environment.into();
    let env_str = env.as_str();
    let confirm = if args.confirm_with_backup {
        Confirmation::ConfirmedWithBackup
    } else {
        Confirmation::None
    };

    let (mut client, conn) = tokio_postgres::connect(&args.admin_database_url, NoTls)
        .await
        .context("admin connect")?;
    let conn_task = tokio::spawn(conn);

    // Ensure the data schema exists (idempotent; the tenant floor DDL grants the
    // tables to wamn_app, and the schema needs USAGE too). Outside the migration
    // transaction — it is provisioning, not part of the atomic apply.
    client
        .batch_execute(&format!(
            "CREATE SCHEMA IF NOT EXISTS {schema} AUTHORIZATION CURRENT_USER; \
             GRANT USAGE ON SCHEMA {schema} TO wamn_app;",
            schema = args.schema
        ))
        .await
        .context("ensure data schema")?;

    let tx = client.transaction().await.context("begin")?;
    tx.batch_execute(&format!(
        "SET LOCAL search_path = {schema}, catalog",
        schema = args.schema
    ))
    .await
    .context("set search_path")?;

    // Read the current applied version (locked for the apply).
    let current_row = tx
        .query_opt(
            &sql::select_current_applied_sql(),
            &[&args.tenant, &target.catalog_id, &env_str],
        )
        .await
        .context("read current applied version")?;
    let current: Option<Catalog> = match current_row {
        Some(row) => {
            let doc: Option<String> = row.get(1);
            let doc = doc.context(
                "current applied version has no stored document — cannot diff (a pre-2.5 row?)",
            )?;
            Some(Catalog::from_json(&doc).context("parse current applied catalog document")?)
        }
        None => None,
    };

    let request = MigrationRequest {
        tenant: &args.tenant,
        environment: env,
        current: current.as_ref(),
        target: &target,
        expected_base: args.base,
        confirm,
    };

    if args.dry_run {
        let report = plan_error(dry_run(&request))?;
        // Nothing is executed — drop the transaction (rolls back the lock).
        drop(tx);
        println!("{}", report.render());
        conn_task.abort();
        return Ok(());
    }

    let plan = plan_error(plan_migration(&request))?;
    for stmt in &plan.statements {
        if stmt.params.is_empty() {
            tx.batch_execute(&stmt.sql)
                .await
                .with_context(|| format!("apply: {}", stmt.summary))?;
        } else {
            let params = to_sql_params(&stmt.params);
            tx.execute(stmt.sql.as_str(), &params)
                .await
                .with_context(|| format!("apply: {}", stmt.summary))?;
        }
    }
    tx.commit().await.context("commit migration")?;
    conn_task.abort();

    let from = plan
        .from_version
        .map_or_else(|| "(none)".to_string(), |v| v.to_string());
    println!(
        "applied migration {from} -> {} for catalog {:?} in environment {} ({}{} operation(s))",
        plan.to_version,
        plan.catalog_id,
        plan.environment,
        if plan.destructive {
            "DESTRUCTIVE, "
        } else {
            ""
        },
        plan.statements
            .iter()
            .filter(|s| s.params.is_empty())
            .count(),
    );
    for w in &plan.warnings {
        println!("  [warning] {w}");
    }
    Ok(())
}

/// Map a [`MigrationError`] to a clear operator-facing failure (the confirmation
/// gate especially needs a legible message).
fn plan_error<T>(r: Result<T, MigrationError>) -> anyhow::Result<T> {
    r.map_err(|e| match &e {
        MigrationError::RequiresConfirmation(rc) => anyhow::anyhow!(
            "migration is destructive; re-run with --confirm-with-backup after taking a backup \
             checkpoint. Destructive: {}",
            rc.destructive.join("; ")
        ),
        _ => anyhow::anyhow!("{e}"),
    })
}

fn to_sql_params(vals: &[Value]) -> Vec<&(dyn ToSql + Sync)> {
    vals.iter()
        .map(|v| -> &(dyn ToSql + Sync) {
            match v {
                Value::Text(s) => s,
                Value::NullableText(o) => o,
                Value::Int(i) => i,
                Value::NullableInt(o) => o,
                Value::Bool(b) => b,
            }
        })
        .collect()
}

fn is_bare_ident(s: &str) -> bool {
    let mut cs = s.chars();
    matches!(cs.next(), Some(c) if c == '_' || c.is_ascii_lowercase())
        && cs.all(|c| c == '_' || c.is_ascii_lowercase() || c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_ident_rules() {
        assert!(is_bare_ident("public"));
        assert!(is_bare_ident("app_data_2"));
        assert!(!is_bare_ident("2data")); // must not start with a digit
        assert!(!is_bare_ident("Public")); // lowercase only
        assert!(!is_bare_ident("a; drop")); // no punctuation/space
        assert!(!is_bare_ident(""));
    }

    #[test]
    fn to_sql_params_maps_each_variant() {
        let vals = vec![
            Value::Text("t".into()),
            Value::NullableText(None),
            Value::Int(3),
            Value::NullableInt(Some(1)),
            Value::Bool(true),
        ];
        let params = to_sql_params(&vals);
        assert_eq!(params.len(), 5);
    }
}
