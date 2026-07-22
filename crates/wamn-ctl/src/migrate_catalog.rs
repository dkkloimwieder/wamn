//! The `migrate-catalog` subcommand (2.5): the **effect shell** for the
//! `wamn-migrate` engine — it reads the current applied catalog from a project
//! database, calls the pure planner, and executes the resulting one-transaction
//! [`ApplyPlan`] (DDL + the lifecycle advance + the history row).
//!
//! The engine ([`wamn_migrate`]) is pure (guards, DDL via wamn-ddl, the
//! lifecycle via wamn-schema, `$n`-parameterized SQL); this shell holds the
//! connection. Two modes:
//!
//! * `--dry-run` — read + plan + print the report (DDL + rollback) and run the
//!   read-only D24 registration-orphan probe (surfacing + failing on an
//!   orphaning target, exactly as the apply path would refuse), touching nothing;
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
    Catalog, Confirmation, Env, MigrationError, MigrationRequest, Value, dry_run, plan_migration,
    sql,
};

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

    /// Environment slug the catalog version is tagged with (any slug; default `dev`).
    #[arg(long, default_value = "dev")]
    pub environment: String,

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

    /// Acknowledge the schema-change impact (11.8). Required to APPLY a destructive
    /// plan whose affected entities carry dependent flows or suites — the report is
    /// always rendered; this asserts the operator has reviewed the blast radius.
    /// Orthogonal to `--confirm-with-backup` (that gate is about data loss; this is
    /// about downstream flows/suites). No effect on an additive or no-dependent plan.
    #[arg(long)]
    pub acknowledge_impact: bool,

    /// Skip the post-migrate REPLICA IDENTITY reconcile (EVT-RI-ORCH, l5i9.61).
    /// By default a successful migration reconciles RI for the data schema so an
    /// entity that needs the old image is never left on DEFAULT; pass this to run
    /// `reconcile-replica-identity` separately instead. No effect with `--dry-run`.
    #[arg(long)]
    pub skip_reconcile_replica_identity: bool,
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

    let env = Env::new(&args.environment);
    let env_str = env.as_str().to_string();
    let confirm = if args.confirm_with_backup {
        Confirmation::ConfirmedWithBackup
    } else {
        Confirmation::None
    };

    let (mut client, conn) = tokio_postgres::connect(&args.admin_database_url, NoTls)
        .await
        .context("admin connect")?;
    let conn_task = tokio::spawn(conn);

    if args.dry_run {
        // STRICTLY read-only (the 1wdq reconcile-run-plane standard): NO
        // ensure_data_schema — a dry run must not CREATE SCHEMA. Planning against
        // a not-yet-existing data schema is coherent: the pure planner consumes
        // only `current` (read below, fully catalog-qualified) and `target`, never
        // the live data schema; and `SET search_path = <absent>, catalog` is
        // tolerated by Postgres (a missing schema is skipped in name resolution),
        // so the hypothetical env still yields a plan, not an error. The real
        // (non-dry) apply path creates the schema (see apply_catalog_target).
        let tx = client.transaction().await.context("begin")?;
        tx.batch_execute(&format!(
            "SET LOCAL search_path = {schema}, catalog",
            schema = args.schema
        ))
        .await
        .context("set search_path")?;
        let current = read_current_applied(&tx, &args.tenant, &target.catalog_id, &env_str).await?;
        let request = MigrationRequest {
            tenant: &args.tenant,
            environment: env,
            current: current.as_ref(),
            target: &target,
            expected_base: args.base,
            confirm,
        };
        let report = plan_error(dry_run(&request))?;
        // Nothing is executed — drop the transaction (rolls back the lock).
        drop(tx);
        println!("{}", report.render());

        // [11.8] (wamn-wvb): render the schema-change impact report — the SAME
        // read-only dependency edges the apply path gates on, so a dry run previews
        // the blast radius (affected flows via registration + node config, their
        // suites, the generated-API resources). The --acknowledge-impact gate is
        // OVERRIDABLE (like --confirm-with-backup), so a dry run SURFACES it rather
        // than failing on it (unlike the unconditional D24 orphan refusal below).
        let impact_plan = crate::impact_report::compile_plan(current.as_ref(), &target)?;
        let impact = crate::impact_report::gather_impact(
            &client,
            &impact_plan,
            current.as_ref(),
            &target,
            &args.schema,
        )
        .await?;
        println!("{}", impact.render());
        if impact.requires_acknowledgement() && !args.acknowledge_impact {
            println!(
                "[dry-run] apply would REFUSE without --acknowledge-impact \
                 (destructive change with dependent flows/suites)"
            );
        }

        // D24 (EVT-REG, wamn-1bfe): run the SAME read-only registration-orphan
        // probe the apply path runs (guard_registration_orphans), so a dry run
        // cannot report clean while the real migrate-catalog would REFUSE before
        // the apply transaction. The orphan refusal is UNCONDITIONAL — unlike the
        // destructive gate (which dry-run merely surfaces, because
        // --confirm-with-backup overrides it), there is no override — so it joins
        // the stale-base / not-forward preconditions dry-run already exits nonzero
        // on: the verdict is surfaced as a marked dry-run finding AND fails the
        // dry run. Read-only: mutates nothing (matches --dry-run's contract).
        let orphan_check =
            crate::publish_catalog::guard_registration_orphans(&client, &target).await;
        conn_task.abort();
        if let Err(e) = orphan_check {
            bail!("[dry-run] would REFUSE at apply — {e}");
        }
        return Ok(());
    }

    // D24 (EVT-REG, wamn-rmxa): refuse a migration that would remove an entity
    // still referenced by an event registration — across ALL tenants, since the
    // entity table is shared. Read-only pre-check on the same superuser
    // connection, BEFORE the apply transaction opens, so a refusal mutates
    // nothing and fires independently of the destructive-backup gate. Shared
    // with publish-catalog (the bead's carrier verb).
    crate::publish_catalog::guard_registration_orphans(&client, &target).await?;

    // [11.8] (wamn-wvb): render the schema-change impact report and enforce the
    // acknowledge gate BEFORE the apply transaction (a refusal mutates nothing,
    // mirroring the D24 guard). The current-applied snapshot is read read-only and
    // dropped; the authoritative apply below re-reads it under FOR UPDATE.
    {
        let snap = client
            .transaction()
            .await
            .context("begin impact snapshot")?;
        snap.batch_execute(&format!(
            "SET LOCAL search_path = {schema}, catalog",
            schema = args.schema
        ))
        .await
        .context("set search_path")?;
        let current =
            read_current_applied(&snap, &args.tenant, &target.catalog_id, &env_str).await?;
        drop(snap);
        let impact_plan = crate::impact_report::compile_plan(current.as_ref(), &target)?;
        let impact = crate::impact_report::gather_impact(
            &client,
            &impact_plan,
            current.as_ref(),
            &target,
            &args.schema,
        )
        .await?;
        println!("{}", impact.render());
        if impact.requires_acknowledgement() && !args.acknowledge_impact {
            // Typed refusal (non-zero exit), mirroring OrphaningPublish /
            // RequiresConfirmation — the operator reviews the report + re-runs.
            return Err(impact.acknowledgement_refusal().into());
        }
    }

    let plan = match apply_catalog_target(
        &mut client,
        &args.tenant,
        &env_str,
        &args.schema,
        &target,
        args.base,
        confirm,
    )
    .await?
    {
        ApplyOutcome::Applied(plan) => plan,
        // migrate-catalog keeps re-applying a version an ERROR (the copy driver
        // treats it as "already current" — its call site decides).
        ApplyOutcome::AlreadyApplied { version } => {
            bail!("{}", MigrationError::AlreadyApplied { version })
        }
    };

    // EVT-RI-ORCH (wamn-l5i9.61): reconcile REPLICA IDENTITY as the automatic
    // operational caller now the migration committed — the table set and the
    // registration set may both have changed, so an entity that needs the old
    // image is flipped to FULL here rather than waiting for a manual verb run (the
    // flip is non-retroactive; the gap would be permanent for events captured
    // meanwhile). Runs on the same superuser connection AFTER commit (reads the
    // post-migration table set), scoped strictly to the data schema. Idempotent.
    if !args.skip_reconcile_replica_identity {
        crate::reconcile_replica_identity::reconcile_after_apply(&client, &target, &args.schema)
            .await?;
    }

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

/// Outcome of applying a target catalog against a live database.
pub(crate) enum ApplyOutcome {
    /// The migration ran (the executed plan, with its versions/warnings).
    Applied(wamn_migrate::ApplyPlan),
    /// The target version is already the applied version — nothing to do. The
    /// caller decides whether that is an error (`migrate-catalog`) or an
    /// idempotent skip (the copy driver's re-copy).
    AlreadyApplied { version: u32 },
}

/// Ensure the data schema exists (idempotent; the tenant floor DDL grants the
/// tables to wamn_app, and the schema needs USAGE too). Outside the migration
/// transaction — it is provisioning, not part of the atomic apply.
pub(crate) async fn ensure_data_schema(
    client: &tokio_postgres::Client,
    schema: &str,
) -> anyhow::Result<()> {
    client
        .batch_execute(&format!(
            "CREATE SCHEMA IF NOT EXISTS {schema} AUTHORIZATION CURRENT_USER; \
             GRANT USAGE ON SCHEMA {schema} TO wamn_app;"
        ))
        .await
        .context("ensure data schema")?;
    Ok(())
}

/// Read the current applied version for `(tenant, catalog, environment)`,
/// locked `FOR UPDATE` (the apply transaction holds it). `pub(crate)` so the
/// read-only `impact-report` verb (11.8) reads the same current-applied snapshot.
pub(crate) async fn read_current_applied(
    tx: &tokio_postgres::Transaction<'_>,
    tenant: &str,
    catalog_id: &str,
    environment: &str,
) -> anyhow::Result<Option<Catalog>> {
    let current_row = tx
        .query_opt(
            &sql::select_current_applied_sql(),
            &[&tenant, &catalog_id, &environment],
        )
        .await
        .context("read current applied version")?;
    match current_row {
        Some(row) => {
            let doc: Option<String> = row.get(1);
            let doc = doc.context(
                "current applied version has no stored document — cannot diff (a pre-2.5 row?)",
            )?;
            Ok(Some(
                Catalog::from_json(&doc).context("parse current applied catalog document")?,
            ))
        }
        None => Ok(None),
    }
}

/// Apply a target catalog to the connected database: read the current applied
/// version (locked), plan with the pure engine, and run the whole [`ApplyPlan`]
/// in **one transaction** (the R9c invariant). Shared by `migrate-catalog` and
/// the copy driver's definition pass (`copy-project-env`, wamn-8df.5).
pub(crate) async fn apply_catalog_target(
    client: &mut tokio_postgres::Client,
    tenant: &str,
    environment: &str,
    schema: &str,
    target: &Catalog,
    expected_base: Option<u32>,
    confirm: Confirmation,
) -> anyhow::Result<ApplyOutcome> {
    ensure_data_schema(client, schema).await?;
    let tx = client.transaction().await.context("begin")?;
    tx.batch_execute(&format!("SET LOCAL search_path = {schema}, catalog"))
        .await
        .context("set search_path")?;

    let current = read_current_applied(&tx, tenant, &target.catalog_id, environment).await?;
    let request = MigrationRequest {
        tenant,
        environment: Env::new(environment),
        current: current.as_ref(),
        target,
        expected_base,
        confirm,
    };
    let plan = match plan_migration(&request) {
        Err(MigrationError::AlreadyApplied { version }) => {
            drop(tx);
            return Ok(ApplyOutcome::AlreadyApplied { version });
        }
        other => plan_error(other)?,
    };
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
    // Refresh the decode-time entity map (wamn-l5i9.11) IN the apply
    // transaction: the OID-keyed rows commit atomically with the DDL that
    // created/renamed the tables, so a CDC reader's lookup never sees one
    // without the other.
    crate::publish_catalog::upsert_entity_map(&tx, target, schema).await?;
    tx.commit().await.context("commit migration")?;
    Ok(ApplyOutcome::Applied(plan))
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

pub(crate) fn is_bare_ident(s: &str) -> bool {
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
