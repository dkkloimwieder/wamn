//! The `migrate-catalog` subcommand (2.5): the **effect shell** for the
//! `wamn-schema-control` engine — it reads the current applied catalog from a project
//! database, calls the pure planner, and executes the resulting one-transaction
//! [`ApplyPlan`] (DDL + the lifecycle advance + the history row).
//!
//! The engine ([`wamn_schema_control`]) is pure (guards, DDL via wamn-schema-compiler, the
//! lifecycle via wamn-schema-control, `$n`-parameterized SQL); this shell holds the
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

use wamn_schema_control::{
    BareSchemaName, Catalog, Confirmation, Env, MigrationError, MigrationRequest, Value, dry_run,
    plan_migration, sql,
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

    /// Path to the target catalog JSON (crates/schema/model `Catalog`).
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
    let schema = BareSchemaName::new(args.schema.clone())
        .with_context(|| format!("invalid --schema {:?}", args.schema))?;
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
            "SET LOCAL search_path = {}, catalog",
            schema.quoted()
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
            schema.as_str(),
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
    crate::publish_catalog::ensure_wamn_app_role(&client).await?;
    crate::publish_catalog::ensure_catalog_storage(&client).await?;

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
            "SET LOCAL search_path = {}, catalog",
            schema.quoted()
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
            schema.as_str(),
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
        &schema,
        &target,
        args.base,
        confirm,
        true,
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
        crate::reconcile_replica_identity::reconcile_after_apply(&client, &target, schema.as_str())
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
    Applied(wamn_schema_control::ApplyPlan),
    /// The target version is already the applied version — nothing to do. The
    /// caller decides whether that is an error (`migrate-catalog`) or an
    /// idempotent skip (the copy driver's re-copy).
    AlreadyApplied { version: u32 },
}

/// Ensure the data schema exists (idempotent; the tenant floor DDL grants the
/// tables to wamn_app, and the schema needs USAGE too). Outside the migration
/// transaction — it is provisioning, not part of the atomic apply.
pub(crate) async fn ensure_data_schema(
    client: &impl tokio_postgres::GenericClient,
    schema: &BareSchemaName,
) -> anyhow::Result<()> {
    client
        .batch_execute(&format!(
            "CREATE SCHEMA IF NOT EXISTS {schema} AUTHORIZATION CURRENT_USER; \
             GRANT USAGE ON SCHEMA {schema} TO wamn_app;",
            schema = schema.quoted(),
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
// Its parameters are exactly the in-transaction verb's; bundling them would churn
// both signatures and every caller for no behaviour change.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn apply_catalog_target(
    client: &mut tokio_postgres::Client,
    tenant: &str,
    environment: &str,
    schema: &BareSchemaName,
    target: &Catalog,
    expected_base: Option<u32>,
    confirm: Confirmation,
    advance_release_head: bool,
) -> anyhow::Result<ApplyOutcome> {
    let tx = client.transaction().await.context("begin")?;
    let outcome = apply_catalog_target_in_transaction(
        &tx,
        tenant,
        environment,
        schema,
        target,
        expected_base,
        confirm,
        advance_release_head,
    )
    .await?;
    tx.commit().await.context("commit migration")?;
    Ok(outcome)
}

// Bundling these would churn the `copy-project-env` caller too, for no behaviour change.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn apply_catalog_target_in_transaction(
    tx: &tokio_postgres::Transaction<'_>,
    tenant: &str,
    environment: &str,
    schema: &BareSchemaName,
    target: &Catalog,
    expected_base: Option<u32>,
    confirm: Confirmation,
    advance_release_head: bool,
) -> anyhow::Result<ApplyOutcome> {
    ensure_data_schema(tx, schema).await?;
    tx.batch_execute(&format!(
        "SET LOCAL search_path = {}, catalog",
        schema.quoted()
    ))
    .await
    .context("set search_path")?;

    let current = read_current_applied(tx, tenant, &target.catalog_id, environment).await?;
    let head_version = crate::publish_catalog::lock_or_initialize_catalog_head(
        tx,
        tenant,
        &target.catalog_id,
        environment,
    )
    .await?;
    let current_version = current
        .as_ref()
        .map(|catalog| i32::try_from(catalog.version).context("current catalog version"))
        .transpose()?;
    anyhow::ensure!(
        head_version.is_none() || head_version == current_version,
        "catalog-head-state-conflict: head {head_version:?}, applied catalog {current_version:?}"
    );
    let runs_present: bool = tx
        .query_one(
            &format!("SELECT to_regclass('{}.runs') IS NOT NULL", schema.as_str()),
            &[],
        )
        .await?
        .get(0);
    let nonterminal_runs = if runs_present {
        if let Some(applied) = head_version.or(current_version) {
            tx.query_one(
                &wamn_schema_control::sql::count_nonterminal_release_runs_sql(schema.as_str()),
                &[&tenant, &target.catalog_id, &applied],
            )
            .await
            .context("check release-pinned runs")?
            .get(0)
        } else {
            0_i64
        }
    } else {
        0_i64
    };
    wamn_schema_control::guard_publication(&wamn_schema_control::PublicationGuard {
        expected_base: current_version,
        applied_version: head_version.or(current_version),
        nonterminal_runs,
        unresolved_sources: &[],
    })
    .map_err(anyhow::Error::new)?;
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
            if advance_release_head {
                let version_i32 = i32::try_from(version).context("catalog version")?;
                ensure_existing_release_manifest(tx, tenant, &target.catalog_id, version_i32)
                    .await?;
                tx.execute(
                    wamn_schema_control::sql::advance_catalog_head_sql(),
                    &[&tenant, &target.catalog_id, &environment, &version_i32],
                )
                .await
                .context("initialize catalog head")?;
            }
            return Ok(ApplyOutcome::AlreadyApplied { version });
        }
        other => plan_error(other)?,
    };
    for stmt in &plan.statements {
        if !should_execute_migration_statement(advance_release_head, stmt) {
            continue;
        }
        execute_migration_statement(tx, stmt).await?;
    }
    // Refresh the decode-time entity map (wamn-l5i9.11) IN the apply
    // transaction: the OID-keyed rows commit atomically with the DDL that
    // created/renamed the tables, so a CDC reader's lookup never sees one
    // without the other.
    crate::publish_catalog::upsert_entity_map(tx, target, schema).await?;
    if advance_release_head {
        let target_version = i32::try_from(target.version).context("catalog version")?;
        carry_forward_release(
            tx,
            tenant,
            &target.catalog_id,
            current_version,
            target_version,
            environment,
            schema,
        )
        .await?;
        tx.execute(
            wamn_schema_control::sql::advance_catalog_head_sql(),
            &[&tenant, &target.catalog_id, &environment, &target_version],
        )
        .await
        .context("advance catalog head")?;
    }
    Ok(ApplyOutcome::Applied(plan))
}

pub(crate) fn is_migration_journal_statement(
    statement: &wamn_schema_control::SqlStatement,
) -> bool {
    statement.summary == "record the migration in schema_migrations"
}

fn should_execute_migration_statement(
    advance_release_head: bool,
    statement: &wamn_schema_control::SqlStatement,
) -> bool {
    advance_release_head || !is_migration_journal_statement(statement)
}

pub(crate) async fn execute_migration_statement(
    tx: &tokio_postgres::Transaction<'_>,
    statement: &wamn_schema_control::SqlStatement,
) -> anyhow::Result<()> {
    if statement.params.is_empty() {
        tx.batch_execute(&statement.sql)
            .await
            .with_context(|| format!("apply: {}", statement.summary))?;
    } else {
        let params = to_sql_params(&statement.params);
        tx.execute(statement.sql.as_str(), &params)
            .await
            .with_context(|| format!("apply: {}", statement.summary))?;
    }
    Ok(())
}

async fn ensure_existing_release_manifest(
    tx: &tokio_postgres::Transaction<'_>,
    tenant: &str,
    catalog_id: &str,
    catalog_version: i32,
) -> anyhow::Result<()> {
    let present: bool = tx
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM catalog.release_manifests \
             WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = $3) \
             AND EXISTS (SELECT 1 FROM catalog.release_exposure_manifests \
             WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = $3)",
            &[&tenant, &catalog_id, &catalog_version],
        )
        .await
        .context("verify existing release manifest")?
        .get(0);
    anyhow::ensure!(
        present,
        "catalog-release-manifest-missing: applied release {catalog_id:?} v{catalog_version}"
    );
    Ok(())
}

/// A schema-only migration keeps the prior sealed flow set. A first release may
/// be sealed empty only when the project flow registry is genuinely empty.
async fn carry_forward_release(
    tx: &tokio_postgres::Transaction<'_>,
    tenant: &str,
    catalog_id: &str,
    current_version: Option<i32>,
    target_version: i32,
    environment: &str,
    schema: &BareSchemaName,
) -> anyhow::Result<()> {
    let manifest = if let Some(source_version) = current_version {
        tx.query_opt(
            "SELECT members_json::text FROM catalog.release_manifests \
             WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = $3",
            &[&tenant, &catalog_id, &source_version],
        )
        .await
        .context("read applied release manifest")?
        .with_context(|| {
            format!(
                "catalog-release-manifest-missing: cannot migrate {catalog_id:?} v{source_version}"
            )
        })?
        .get::<_, String>(0)
    } else {
        let flows_present: bool = tx
            .query_one(
                &format!(
                    "SELECT to_regclass('{}.flows') IS NOT NULL",
                    schema.as_str()
                ),
                &[],
            )
            .await?
            .get(0);
        let flow_count = if flows_present {
            tx.query_one(
                &format!(
                    "SELECT count(*) FROM {}.flows WHERE tenant_id = $1",
                    schema.as_str()
                ),
                &[&tenant],
            )
            .await
            .context("check first release flow registry")?
            .get::<_, i64>(0)
        } else {
            0
        };
        anyhow::ensure!(
            flow_count == 0,
            "catalog-release-unresolved-sources: first release has {flow_count} legacy flow(s)"
        );
        "[]".to_string()
    };
    tx.execute(
        wamn_schema_control::sql::register_release_manifest_sql(),
        &[&tenant, &catalog_id, &target_version, &manifest],
    )
    .await
    .context("seal migrated release manifest")?;
    if let Some(source_version) = current_version {
        tx.execute(
            "INSERT INTO catalog.release_flows \
               (tenant_id, catalog_id, catalog_version, flow_id, flow_version) \
             SELECT tenant_id, catalog_id, $4, flow_id, flow_version \
             FROM catalog.release_flows \
             WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = $3 \
             ON CONFLICT (tenant_id, catalog_id, catalog_version, flow_id) DO NOTHING",
            &[&tenant, &catalog_id, &source_version, &target_version],
        )
        .await
        .context("carry migrated release members forward")?;
    }
    let exposure_manifest = if let Some(source_version) = current_version {
        tx.query_one(
            "SELECT definitions_json::text FROM catalog.release_exposure_manifests \
             WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = $3",
            &[&tenant, &catalog_id, &source_version],
        )
        .await
        .context("read applied release exposure")?
        .get::<_, String>(0)
    } else {
        r#"{"attachments":[],"sources":[]}"#.to_string()
    };
    tx.execute(
        wamn_schema_control::sql::register_release_exposure_manifest_sql(),
        &[&tenant, &catalog_id, &target_version, &exposure_manifest],
    )
    .await
    .context("seal migrated release exposure")?;
    if let Some(source_version) = current_version {
        tx.execute(
            "INSERT INTO catalog.release_sources \
               (tenant_id, catalog_id, catalog_version, source_id, source_kind, \
                definition_json, source_hash) \
             SELECT tenant_id, catalog_id, $4, source_id, source_kind, \
                    definition_json, source_hash \
             FROM catalog.release_sources \
             WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = $3",
            &[&tenant, &catalog_id, &source_version, &target_version],
        )
        .await
        .context("carry migrated release sources forward")?;
        tx.execute(
            "INSERT INTO catalog.release_attachments \
               (tenant_id, catalog_id, catalog_version, attachment_id, attachment_kind, \
                flow_id, source_id, definition_hash, definition_json, route_host, \
                route_path, route_template, route_method) \
             SELECT tenant_id, catalog_id, $4, attachment_id, attachment_kind, \
                    flow_id, source_id, definition_hash, definition_json, route_host, \
                    route_path, route_template, route_method \
             FROM catalog.release_attachments \
             WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = $3",
            &[&tenant, &catalog_id, &source_version, &target_version],
        )
        .await
        .context("carry migrated release attachments forward")?;
    }
    tx.execute(
        wamn_schema_control::sql::apply_release_exposure_sql(),
        &[
            &tenant,
            &catalog_id,
            &environment,
            &target_version,
            &"migrate-catalog",
        ],
    )
    .await
    .context("carry migrated attachment activation")?;
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

pub(crate) fn is_bare_ident(s: &str) -> bool {
    BareSchemaName::new(s).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn migration_carries_the_sealed_release_forward_before_head_advance() {
        let Ok(url) = std::env::var("WAMN_MIGRATE_PG_URL") else {
            return;
        };
        let (mut client, connection) = tokio_postgres::connect(&url, NoTls).await.unwrap();
        let connection_task = tokio::spawn(connection);
        crate::publish_catalog::ensure_wamn_app_role(&client)
            .await
            .unwrap();
        crate::publish_catalog::ensure_catalog_storage(&client)
            .await
            .unwrap();
        let suffix = std::process::id();
        let tenant = format!("migrate-release-{suffix}");
        let catalog_id = format!("migrate-release-{suffix}");
        client
            .execute(
                "INSERT INTO catalog.catalogs \
                   (tenant_id, catalog_id, version, environment, schema_version, state, document) \
                 VALUES \
                   ($1, $2, 1, 'dev', '0.1', 'superseded', '{}'::jsonb), \
                   ($1, $2, 2, 'dev', '0.1', 'applied', '{}'::jsonb)",
                &[&tenant, &catalog_id],
            )
            .await
            .unwrap();
        client
            .execute(
                wamn_schema_control::sql::register_flow_artifact_sql(),
                &[
                    &tenant,
                    &"flow",
                    &1_i32,
                    &"0.1",
                    &r#"{"flow-id":"flow"}"#,
                    &"graph",
                    &"artifact",
                ],
            )
            .await
            .unwrap();
        let manifest = r#"[{"flow-id":"flow","flow-version":1,"artifact-hash":"artifact"}]"#;
        client.batch_execute("BEGIN").await.unwrap();
        client
            .execute(
                wamn_schema_control::sql::register_release_manifest_sql(),
                &[&tenant, &catalog_id, &1_i32, &manifest],
            )
            .await
            .unwrap();
        client
            .execute(
                wamn_schema_control::sql::insert_release_flow_sql(),
                &[&tenant, &catalog_id, &1_i32, &"flow", &1_i32],
            )
            .await
            .unwrap();
        client
            .execute(
                wamn_schema_control::sql::register_release_exposure_manifest_sql(),
                &[
                    &tenant,
                    &catalog_id,
                    &1_i32,
                    &r#"{"attachments":[],"sources":[]}"#,
                ],
            )
            .await
            .unwrap();
        client.batch_execute("COMMIT").await.unwrap();

        let tx = client.transaction().await.unwrap();
        carry_forward_release(
            &tx,
            &tenant,
            &catalog_id,
            Some(1),
            2,
            "dev",
            &BareSchemaName::new("pg_temp").unwrap(),
        )
        .await
        .unwrap();
        tx.execute(
            wamn_schema_control::sql::advance_catalog_head_sql(),
            &[&tenant, &catalog_id, &"dev", &2_i32],
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let copied: bool = client
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM catalog.release_flows \
                 WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = 2 \
                   AND flow_id = 'flow' AND flow_version = 1) \
                 AND EXISTS (SELECT 1 FROM catalog.catalog_heads \
                 WHERE tenant_id = $1 AND catalog_id = $2 AND environment = 'dev' \
                   AND applied_catalog_version = 2)",
                &[&tenant, &catalog_id],
            )
            .await
            .unwrap()
            .get(0);
        assert!(copied);
        drop(client);
        let _ = connection_task.await;
    }

    #[test]
    fn bare_ident_rules() {
        assert!(is_bare_ident("public"));
        assert!(is_bare_ident("app_data_2"));
        assert!(!is_bare_ident("2data")); // must not start with a digit
        assert!(!is_bare_ident("Public")); // lowercase only
        assert!(!is_bare_ident("a; drop")); // no punctuation/space
        assert!(!is_bare_ident(""));
        assert!(is_bare_ident(&format!("s{}", "a".repeat(62))));
        assert!(!is_bare_ident(&format!("s{}", "a".repeat(63))));
    }

    #[test]
    fn normal_migrate_executes_journal_while_copy_defers_it() {
        let journal = wamn_schema_control::SqlStatement {
            summary: "record the migration in schema_migrations".into(),
            sql: "INSERT INTO catalog.schema_migrations".into(),
            params: vec![],
        };
        assert!(should_execute_migration_statement(true, &journal));
        assert!(!should_execute_migration_statement(false, &journal));
        let ddl = wamn_schema_control::SqlStatement {
            summary: "apply DDL".into(),
            sql: "SELECT 1".into(),
            params: vec![],
        };
        assert!(should_execute_migration_statement(false, &ddl));
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
