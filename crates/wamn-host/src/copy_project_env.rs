//! The `copy-project-env` subcommand (wamn-8df.5): the unified env-symmetric
//! **copy** between two `(org, project, env)` triples — deploy / promote /
//! clone / move in one operation (`docs/deployment-model.md` §4).
//!
//! The plan comes from the pure [`wamn_provision::plan_copy`]; this driver holds
//! the connections and executes each [`CopyStep`] by composing the shipped
//! machinery:
//!
//! * `include: definition` — the src env's **applied catalogs** promote into the
//!   dst through the 2.5 migrate engine (the same one-transaction apply
//!   `migrate-catalog` runs), plus the **flow registrations** and the **RLS
//!   policy rows** (re-compiled and applied on the dst). Config has no defined
//!   artifact yet — deferred, noted at runtime.
//! * `include: data` — `pg_restore --data-only --disable-triggers` of the data
//!   schema from a fresh `pg_dump -Fd` snapshot (the q3n.10 artifact, recorded
//!   in `provisioning.dumps`).
//! * `include: both` — a full-fidelity `pg_restore` of the snapshot (schema +
//!   rows + ownership/ACLs; the dump carries the definition tables too).
//!
//! **The cutover gate (fixes cjv.7):** a `--cutover` copy is a *move* — the
//! pipeline `Quiesce → Snapshot → Restore → Verify → Cutover` is mandatory,
//! every step advances the `copy` saga in the T1 registry
//! (`provisioning.sagas`), and the `Cutover` executor **re-reads the saga and
//! refuses** unless every prior step — quiesce and verify included — is durably
//! recorded. The old dump→flip write-loss window cannot be skipped silently.
//!
//! Quiesce = `ALTER DATABASE … SET default_transaction_read_only = on` +
//! terminating existing backends (pooled connections re-dial under the new
//! default), proven by a write probe that must fail `read_only_sql_transaction`.
//! Reads stay live through the copy window. After a successful cutover the src
//! stays quiesced (it is retired); on failure the un-quiesce statement is
//! printed for the operator.
//!
//! Preconditions (this tool copies, it does not provision): the dst database
//! exists (`provision-project-env` + its Database CR), and for a definition
//! copy the dst carries the catalog storage schema (`deploy/catalog-schema.sql`).
//! The flow registry is ensured on demand (`publish-catalog`'s idempotent DDL).

use std::path::PathBuf;

use anyhow::{Context as _, bail};
use clap::{Args, ValueEnum};
use tokio_postgres::NoTls;
use tokio_postgres::error::SqlState;

use wamn_provision::{
    COPY_SAGA_KIND, CopyInclude, CopyMode, CopyRequest, CopyScope, CopyStep, count_rows_sql,
    dump_object_key, list_schema_tables_sql, pg_dump_argv, pg_restore_data_only_argv, plan_copy,
    project_env_database_name, quiesce_database_sql, sql as provision_sql,
    terminate_database_backends_sql, unquiesce_database_sql, validate_project_env,
};
use wamn_registry::Triple;

use crate::migrate_catalog::{ApplyOutcome, apply_catalog_target, is_bare_ident};
use crate::publish_catalog::{ensure_flow_registry, ensure_runstate};
use crate::restore_project_env::swap_db;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum IncludeArg {
    /// Structure only: catalog + flows + RLS policies.
    Definition,
    /// Rows only: `pg_restore --data-only` of the data schema.
    Data,
    /// Everything: a full-fidelity dump/restore.
    Both,
}

impl From<IncludeArg> for CopyInclude {
    fn from(a: IncludeArg) -> Self {
        match a {
            IncludeArg::Definition => CopyInclude::Definition,
            IncludeArg::Data => CopyInclude::Data,
            IncludeArg::Both => CopyInclude::Both,
        }
    }
}

#[derive(Debug, Args)]
pub struct CopyProjectEnvArgs {
    /// Source org id.
    #[arg(long)]
    pub src_org: String,
    /// Source project id.
    #[arg(long)]
    pub src_project: String,
    /// Source environment slug.
    #[arg(long)]
    pub src_env: String,

    /// Destination org id (may differ from the source — cross-org deploy).
    #[arg(long)]
    pub dst_org: String,
    /// Destination project id.
    #[arg(long)]
    pub dst_project: String,
    /// Destination environment slug.
    #[arg(long)]
    pub dst_env: String,

    /// What the copy carries.
    #[arg(long, value_enum, default_value = "both")]
    pub include: IncludeArg,

    /// This copy is a MOVE: the src's traffic cuts over to the dst. Runs the
    /// mandatory quiesce → verify → gated-cutover pipeline, recorded step by
    /// step in the T1 registry (requires --system-database-url).
    #[arg(long)]
    pub cutover: bool,

    /// After a verified cutover, drop the retained src database (requires
    /// --confirm; default keeps it through a hold window).
    #[arg(long)]
    pub deprovision_old: bool,

    /// Confirm the destructive --deprovision-old drop.
    #[arg(long)]
    pub confirm: bool,

    /// Superuser Postgres URL to the SOURCE cluster (a maintenance DB, e.g.
    /// `.../postgres`) — quiesce, dump, and reads run through it.
    #[arg(long)]
    pub src_admin_url: Option<String>,

    /// Superuser Postgres URL to the DESTINATION cluster. Defaults to
    /// --src-admin-url (a same-cluster copy).
    #[arg(long)]
    pub dst_admin_url: Option<String>,

    /// Superuser Postgres URL to the T1 system DB (`wamn_system`): the copy
    /// saga (`provisioning.sagas`) + the dump record. Env
    /// `WAMN_SYSTEM_ADMIN_URL`. REQUIRED for --cutover (the gate lives there).
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL")]
    pub system_database_url: Option<String>,

    /// Tenant claim the definition rows are scoped to (`app.tenant`). Required
    /// for a definition copy (catalogs / flows / RLS policies are per-tenant).
    #[arg(long)]
    pub tenant: Option<String>,

    /// The data schema the entity tables live in (verify counts it; a
    /// data-only restore is scoped to it).
    #[arg(long, default_value = "public")]
    pub data_schema: String,

    /// The schema the flow registry (`flows`) lives in.
    #[arg(long, default_value = "wamn_run")]
    pub flow_schema: String,

    /// Directory snapshots are staged under (a per-timestamp subdirectory —
    /// the `dump-project-env --run-now` layout).
    #[arg(long, default_value = "/tmp/wamn-dump")]
    pub dump_root: PathBuf,

    /// Acknowledge a destructive definition promotion (the 3.2 gate) — required
    /// when the dst's applied catalog diverges destructively from the src's.
    #[arg(long)]
    pub confirm_with_backup: bool,

    /// Print the step plan and exit without connecting anywhere.
    #[arg(long)]
    pub plan: bool,

    /// Saga id the pipeline records under. Default:
    /// `copy-<src-db>-to-<dst-db>-<unix-seconds>`.
    #[arg(long)]
    pub saga_id: Option<String>,
}

pub async fn run(args: CopyProjectEnvArgs) -> anyhow::Result<()> {
    validate_project_env(&args.src_org, &args.src_project, &args.src_env)
        .map_err(|e| anyhow::anyhow!("src names: {e}"))?;
    validate_project_env(&args.dst_org, &args.dst_project, &args.dst_env)
        .map_err(|e| anyhow::anyhow!("dst names: {e}"))?;
    for (label, s) in [
        ("--data-schema", &args.data_schema),
        ("--flow-schema", &args.flow_schema),
    ] {
        if !is_bare_ident(s) {
            bail!("{label} must be a bare identifier [a-z_][a-z0-9_]*: {s:?}");
        }
    }

    let src = Triple::new(&args.src_org, &args.src_project, args.src_env.as_str());
    let dst = Triple::new(&args.dst_org, &args.dst_project, args.dst_env.as_str());
    let include: CopyInclude = args.include.into();
    let request = CopyRequest {
        src: src.clone(),
        dst: dst.clone(),
        include,
        scope: CopyScope::Whole,
        mode: CopyMode::Snapshot,
        cutover: args.cutover,
        deprovision_old: args.deprovision_old,
    };
    let steps = plan_copy(&request).map_err(|e| anyhow::anyhow!("{e}"))?;

    println!(
        "copy {src} -> {dst} (include: {}, {}):",
        include.as_str(),
        if args.cutover {
            "MOVE with cutover"
        } else {
            "clone"
        }
    );
    for (i, step) in steps.iter().enumerate() {
        println!("  {}. {}", i + 1, step.label());
    }
    if args.plan {
        return Ok(());
    }

    if include.wants_definition() && args.tenant.is_none() {
        bail!("a definition copy needs --tenant (catalogs / flows / RLS policies are per-tenant)");
    }
    let src_admin = args
        .src_admin_url
        .as_deref()
        .context("copy needs --src-admin-url (a superuser URL to the SOURCE cluster)")?;
    let dst_admin = args.dst_admin_url.as_deref().unwrap_or(src_admin);
    if args.cutover && args.system_database_url.is_none() {
        bail!(
            "--cutover requires --system-database-url: the pipeline records quiesce + verify \
             in the T1 registry, and the cutover gate refuses without that durable record"
        );
    }

    let src_db = project_env_database_name(&src.org, &src.project, src.env.as_str());
    let dst_db = project_env_database_name(&dst.org, &dst.project, dst.env.as_str());
    let saga_id = args.saga_id.clone().unwrap_or_else(|| {
        format!(
            "copy-{src_db}-to-{dst_db}-{}",
            crate::dump_project_env::unix_seconds()
        )
    });

    let recorder = match &args.system_database_url {
        Some(url) => {
            let r = SagaRecorder::connect(url, &saga_id)
                .await
                .context("system db connect (saga recording)")?;
            r.create(&format!("{src} -> {dst}"), steps.len() as i32)
                .await?;
            println!("recording saga {saga_id:?} ({} steps)", steps.len());
            Some(r)
        }
        None => {
            println!("(no --system-database-url: steps run unrecorded — clone only)");
            None
        }
    };

    let mut ctx = ExecCtx {
        args: &args,
        src_admin,
        dst_admin,
        src_db: &src_db,
        dst_db: &dst_db,
        dump_dir: None,
        quiesced: false,
    };

    let mut executed = 0usize;
    let result = execute_steps(&mut ctx, &steps, &recorder, &mut executed).await;
    match result {
        Ok(()) => {
            if let Some(r) = &recorder {
                r.complete().await?;
            }
            println!(
                "copy {src} -> {dst} complete ({} step(s){})",
                steps.len(),
                if recorder.is_some() {
                    format!("; saga {saga_id} completed")
                } else {
                    String::new()
                }
            );
            Ok(())
        }
        Err(e) => {
            if let Some(r) = &recorder {
                // Best-effort terminal record; the original error wins.
                let _ = r.fail(&format!("step {}: {e:#}", executed + 1)).await;
            }
            if ctx.quiesced {
                eprintln!(
                    "src {src_db:?} is still QUIESCED. To resume writes on the src:\n  \
                     psql <src-admin-url> -c '{}'\n  \
                     then terminate its backends so sessions re-dial.",
                    unquiesce_database_sql(&src_db)
                );
            }
            Err(e)
        }
    }
}

/// Execute the planned steps in order, advancing the saga after each.
async fn execute_steps(
    ctx: &mut ExecCtx<'_>,
    steps: &[CopyStep],
    recorder: &Option<SagaRecorder>,
    executed: &mut usize,
) -> anyhow::Result<()> {
    for (i, step) in steps.iter().enumerate() {
        println!("[{}/{}] {}", i + 1, steps.len(), step.label());
        match step {
            CopyStep::Quiesce { .. } => exec_quiesce(ctx).await?,
            CopyStep::Snapshot { src } => exec_snapshot(ctx, src, recorder).await?,
            CopyStep::CopyDefinition { src, dst } => exec_copy_definition(ctx, src, dst).await?,
            CopyStep::RestoreData { data_only, .. } => exec_restore_data(ctx, *data_only).await?,
            CopyStep::Verify { src, dst, include } => exec_verify(ctx, src, dst, *include).await?,
            CopyStep::Cutover { src, dst } => {
                // THE GATE (cjv.7): refuse unless every prior step — quiesce and
                // verify included — is durably recorded in the saga.
                let r = recorder
                    .as_ref()
                    .context("cutover without a saga recorder (unreachable: gated upfront)")?;
                let (status, step_no, total) = r.state().await?;
                if step_no < i as i32 {
                    bail!(
                        "refusing cutover: saga {:?} records {step_no}/{} steps (status {status}) \
                         — quiesce and verify are not durably recorded",
                        r.saga_id,
                        total.map_or_else(|| "?".into(), |t| t.to_string()),
                    );
                }
                exec_cutover(ctx, src, dst)?;
            }
            CopyStep::DeprovisionOld { .. } => exec_deprovision_old(ctx).await?,
        }
        if let Some(r) = recorder {
            r.advance().await?;
        }
        *executed = i + 1;
    }
    Ok(())
}

struct ExecCtx<'a> {
    args: &'a CopyProjectEnvArgs,
    src_admin: &'a str,
    dst_admin: &'a str,
    src_db: &'a str,
    dst_db: &'a str,
    /// Set by the Snapshot step; consumed by RestoreData.
    dump_dir: Option<PathBuf>,
    quiesced: bool,
}

/// Quiesce the src database: read-only default for new sessions + terminate
/// existing backends, then PROVE it — a probe write must fail
/// `read_only_sql_transaction` (25006).
async fn exec_quiesce(ctx: &mut ExecCtx<'_>) -> anyhow::Result<()> {
    let (client, task) = connect(ctx.src_admin).await?;
    client
        .batch_execute(&quiesce_database_sql(ctx.src_db))
        .await
        .context("set the read-only default on the src database")?;
    ctx.quiesced = true;
    let terminated: i64 = client
        .query_one(terminate_database_backends_sql(), &[&ctx.src_db])
        .await
        .context("terminate src backends")?
        .get(0);
    drop(client);
    let _ = task.await;

    // The probe: a fresh session must see the read-only default and a write
    // must fail 25006 — quiesce is *proven*, not assumed.
    let src_url = swap_db(ctx.src_admin, ctx.src_db);
    let (probe, task) = connect(&src_url).await?;
    let mode: String = probe
        .query_one("SHOW default_transaction_read_only", &[])
        .await?
        .get(0);
    anyhow::ensure!(
        mode == "on",
        "quiesce probe: default_transaction_read_only is {mode:?}, expected \"on\""
    );
    match probe
        .batch_execute("CREATE TABLE wamn_quiesce_probe_8df5 ()")
        .await
    {
        Ok(()) => {
            let _ = probe
                .batch_execute("DROP TABLE wamn_quiesce_probe_8df5")
                .await;
            bail!("quiesce probe WROTE to the src database — quiesce is not effective");
        }
        Err(e) if e.code() == Some(&SqlState::READ_ONLY_SQL_TRANSACTION) => {}
        Err(e) => return Err(e).context("quiesce probe write failed unexpectedly"),
    }
    drop(probe);
    let _ = task.await;
    println!(
        "  src {:?} quiesced (read-only; {terminated} backend(s) terminated; probe write \
         refused 25006)",
        ctx.src_db
    );
    Ok(())
}

/// `pg_dump -Fd` the src database into `<dump-root>/<ts>` and record it in
/// `provisioning.dumps` (it IS a dump of the src env — one artifact, q3n.10).
async fn exec_snapshot(
    ctx: &mut ExecCtx<'_>,
    src: &Triple,
    recorder: &Option<SagaRecorder>,
) -> anyhow::Result<()> {
    let timestamp = crate::dump_project_env::unix_seconds().to_string();
    let out = ctx.args.dump_root.join(&timestamp);
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let _ = std::fs::remove_dir_all(&out);

    let src_url = swap_db(ctx.src_admin, ctx.src_db);
    run_argv(&pg_dump_argv(&src_url, &out.to_string_lossy()))?;

    let object_key = dump_object_key(src, &timestamp);
    if let Some(r) = recorder {
        let byte_size: Option<i64> = crate::dump_project_env::dir_size(&out)
            .map(|b| b as i64)
            .ok();
        let env = src.env.as_str();
        r.client
            .execute(
                wamn_registry::sql::record_dump_sql(),
                &[
                    &src.org,
                    &src.project,
                    &env,
                    &object_key,
                    &wamn_provision::dump::DUMP_FORMAT,
                    &byte_size,
                ],
            )
            .await
            .context("record the snapshot in provisioning.dumps")?;
    }
    println!("  snapshot {} (object key {object_key})", out.display());
    ctx.dump_dir = Some(out);
    Ok(())
}

/// The definition pass: applied catalogs (via the 2.5 migrate engine), flow
/// registrations, RLS policy rows + their compiled application. Config has no
/// defined artifact yet — deferred.
async fn exec_copy_definition(
    ctx: &mut ExecCtx<'_>,
    _src: &Triple,
    _dst: &Triple,
) -> anyhow::Result<()> {
    let tenant = ctx.args.tenant.as_deref().expect("checked upfront");
    let (src_client, src_task) = connect(&swap_db(ctx.src_admin, ctx.src_db)).await?;
    let (mut dst_client, dst_task) = connect(&swap_db(ctx.dst_admin, ctx.dst_db)).await?;

    for (side, client, hint) in [
        ("src", &src_client, "the src holds no catalog storage"),
        (
            "dst",
            &dst_client,
            "provision the dst first (apply deploy/catalog-schema.sql)",
        ),
    ] {
        let has: Option<String> = client
            .query_one("SELECT to_regclass('catalog.catalogs')::text", &[])
            .await?
            .get(0);
        anyhow::ensure!(
            has.is_some(),
            "{side} database has no catalog.catalogs — {hint}"
        );
    }

    // 1. Applied catalogs: promote each of the src env's applied catalogs into
    //    the dst env through the migrate engine (one-transaction apply each).
    let src_env = ctx.args.src_env.as_str();
    let dst_env = ctx.args.dst_env.as_str();
    let rows = src_client
        .query(
            &wamn_migrate::sql::select_applied_catalogs_sql(),
            &[&tenant, &src_env],
        )
        .await
        .context("enumerate the src env's applied catalogs")?;
    let mut catalogs = Vec::new();
    for row in &rows {
        let catalog_id: String = row.get(0);
        let doc: Option<String> = row.get(2);
        let doc = doc.with_context(|| {
            format!("applied catalog {catalog_id:?} has no stored document (a pre-2.5 row?)")
        })?;
        let cat = wamn_migrate::Catalog::from_json(&doc)
            .with_context(|| format!("parse applied catalog {catalog_id:?}"))?;
        catalogs.push(cat);
    }
    if catalogs.is_empty() {
        println!("  no applied catalogs for tenant {tenant:?} in the src env");
    }
    let confirm = if ctx.args.confirm_with_backup {
        wamn_migrate::Confirmation::ConfirmedWithBackup
    } else {
        wamn_migrate::Confirmation::None
    };
    for cat in &catalogs {
        match apply_catalog_target(
            &mut dst_client,
            tenant,
            dst_env,
            &ctx.args.data_schema,
            cat,
            None,
            confirm,
        )
        .await
        .with_context(|| format!("promote catalog {:?} into the dst", cat.catalog_id))?
        {
            ApplyOutcome::Applied(plan) => println!(
                "  catalog {:?}: applied {} -> {}{}",
                plan.catalog_id,
                plan.from_version
                    .map_or_else(|| "(none)".into(), |v| v.to_string()),
                plan.to_version,
                if plan.destructive {
                    " (DESTRUCTIVE)"
                } else {
                    ""
                },
            ),
            ApplyOutcome::AlreadyApplied { version } => println!(
                "  catalog {:?}: version {version} already applied (skip)",
                cat.catalog_id
            ),
        }
    }

    // 2. Flow registrations: copy the tenant's flows rows verbatim (versions +
    //    active flags — a copy, not a re-registration).
    let fs = &ctx.args.flow_schema;
    let src_has_flows: Option<String> = src_client
        .query_one(&format!("SELECT to_regclass('{fs}.flows')::text"), &[])
        .await?
        .get(0);
    if src_has_flows.is_some() {
        ensure_runstate(&dst_client, fs).await?;
        ensure_flow_registry(&dst_client, fs).await?;
        let flows = src_client
            .query(
                &format!(
                    "SELECT flow_id, version, active, graph_json::text \
                     FROM {fs}.flows WHERE tenant_id = $1"
                ),
                &[&tenant],
            )
            .await
            .context("read src flows")?;
        for row in &flows {
            let flow_id: String = row.get(0);
            let version: i32 = row.get(1);
            let active: bool = row.get(2);
            let graph: String = row.get(3);
            dst_client
                .execute(
                    &format!(
                        "INSERT INTO {fs}.flows (tenant_id, flow_id, version, active, graph_json) \
                         VALUES ($1, $2, $3, $4, $5::text::jsonb) \
                         ON CONFLICT (tenant_id, flow_id, version) DO UPDATE SET \
                           active = EXCLUDED.active, graph_json = EXCLUDED.graph_json, \
                           updated_at = now()"
                    ),
                    &[&tenant, &flow_id, &version, &active, &graph],
                )
                .await
                .with_context(|| format!("copy flow {flow_id} v{version}"))?;
        }
        println!("  flows: {} registration(s) copied", flows.len());
    } else {
        println!("  flows: src has no {fs}.flows registry — skipped");
    }

    // 3. RLS policies: copy the definition rows, then re-compile and apply them
    //    on the dst so its tables actually carry the policies.
    let policies = src_client
        .query(
            "SELECT catalog_id, policy_id, entity_id, rule::text \
             FROM catalog.rls_policies WHERE tenant_id = $1",
            &[&tenant],
        )
        .await
        .context("read src RLS policies")?;
    for row in &policies {
        let (catalog_id, policy_id, entity_id): (String, String, String) =
            (row.get(0), row.get(1), row.get(2));
        let rule: String = row.get(3);
        dst_client
            .execute(
                "INSERT INTO catalog.rls_policies (tenant_id, catalog_id, policy_id, entity_id, rule) \
                 VALUES ($1, $2, $3, $4, $5::text::jsonb) \
                 ON CONFLICT (tenant_id, catalog_id, policy_id) DO UPDATE SET \
                   entity_id = EXCLUDED.entity_id, rule = EXCLUDED.rule",
                &[&tenant, &catalog_id, &policy_id, &entity_id, &rule],
            )
            .await
            .with_context(|| format!("copy RLS policy {policy_id}"))?;
    }
    if !policies.is_empty() {
        apply_rls_policies(&dst_client, &ctx.args.data_schema, &catalogs, &policies).await?;
    }
    println!("  rls: {} policy row(s) copied + applied", policies.len());
    println!("  config: no defined artifact yet — deferred");

    drop(src_client);
    drop(dst_client);
    let _ = src_task.await;
    let _ = dst_task.await;
    Ok(())
}

/// Re-compile the copied RLS policy rows per catalog and apply the compiled
/// plans on the dst. Each `CREATE POLICY` runs autocommit with the data schema
/// on the search path; a policy that already exists (`duplicate_object`) is an
/// idempotent skip — the re-copy case.
async fn apply_rls_policies(
    dst: &tokio_postgres::Client,
    data_schema: &str,
    catalogs: &[wamn_migrate::Catalog],
    rows: &[tokio_postgres::Row],
) -> anyhow::Result<()> {
    use std::collections::BTreeMap;
    let mut by_catalog: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();
    for row in rows {
        let catalog_id: String = row.get(0);
        let rule: String = row.get(3);
        let rule: serde_json::Value =
            serde_json::from_str(&rule).context("parse a stored RLS rule")?;
        by_catalog.entry(catalog_id).or_default().push(rule);
    }
    dst.batch_execute(&format!("SET search_path = {data_schema}, catalog"))
        .await
        .context("set search_path for the RLS apply")?;
    for (catalog_id, rules) in by_catalog {
        let cat = catalogs
            .iter()
            .find(|c| c.catalog_id == catalog_id)
            .with_context(|| {
                format!("RLS policies reference catalog {catalog_id:?}, which is not applied")
            })?;
        let policy_json = serde_json::json!({
            "schema-version": wamn_rls::SCHEMA_VERSION,
            "catalog-id": catalog_id,
            "rules": rules,
        });
        let policy = wamn_rls::AccessPolicy::from_json(&policy_json.to_string())
            .with_context(|| format!("assemble the RLS policy set for {catalog_id:?}"))?;
        let plan = wamn_rls::compile(&policy, cat)
            .map_err(|e| anyhow::anyhow!("compile RLS policies for {catalog_id:?}: {e}"))?;
        for op in &plan.operations {
            match dst.batch_execute(&op.sql).await {
                Ok(()) => {}
                // Already applied (a re-copy) — idempotent skip.
                Err(e) if e.code() == Some(&SqlState::DUPLICATE_OBJECT) => {}
                Err(e) => return Err(e).with_context(|| format!("apply RLS: {}", op.summary)),
            }
        }
    }
    Ok(())
}

/// `pg_restore` the snapshot into the dst. `data_only` scopes to the data
/// schema (`--data-only --disable-triggers` — the outbox triggers must not fire
/// per restored row); a full restore keeps ownership + ACLs (the dst cluster
/// carries `wamn_app` — the provision-project-env precondition).
async fn exec_restore_data(ctx: &mut ExecCtx<'_>, data_only: bool) -> anyhow::Result<()> {
    let dump_dir = ctx
        .dump_dir
        .as_ref()
        .context("restore without a snapshot (unreachable: the plan orders Snapshot first)")?
        .to_string_lossy()
        .to_string();
    let dst_url = swap_db(ctx.dst_admin, ctx.dst_db);
    let argv = if data_only {
        pg_restore_data_only_argv(&dst_url, &dump_dir, &ctx.args.data_schema)
    } else {
        // Full fidelity: schema + rows + ownership/ACLs (no --no-owner).
        vec![
            "pg_restore".to_string(),
            "-d".to_string(),
            dst_url,
            dump_dir,
        ]
    };
    run_argv(&argv)?;
    println!(
        "  restored into {:?} ({})",
        ctx.dst_db,
        if data_only { "data only" } else { "full" }
    );
    Ok(())
}

/// Verify the dst against the src. Data: the data schema's table sets match and
/// every table's exact row count matches. Definition: each applied catalog's
/// document is byte-equal on the dst, and the flows / RLS row counts match.
async fn exec_verify(
    ctx: &mut ExecCtx<'_>,
    _src: &Triple,
    _dst: &Triple,
    include: CopyInclude,
) -> anyhow::Result<()> {
    let (src_client, src_task) = connect(&swap_db(ctx.src_admin, ctx.src_db)).await?;
    let (dst_client, dst_task) = connect(&swap_db(ctx.dst_admin, ctx.dst_db)).await?;

    if include.wants_data() {
        let schema = &ctx.args.data_schema;
        let src_tables = list_tables(&src_client, schema)
            .await
            .context("list src tables")?;
        let dst_tables = list_tables(&dst_client, schema)
            .await
            .context("list dst tables")?;
        anyhow::ensure!(
            src_tables == dst_tables,
            "verify FAILED: table sets differ in schema {schema:?} (src {src_tables:?}, \
             dst {dst_tables:?})"
        );
        for table in &src_tables {
            let sql = count_rows_sql(schema, table);
            let s: i64 = src_client.query_one(sql.as_str(), &[]).await?.get(0);
            let d: i64 = dst_client.query_one(sql.as_str(), &[]).await?.get(0);
            anyhow::ensure!(
                s == d,
                "verify FAILED: {schema}.{table} row counts differ (src {s}, dst {d})"
            );
        }
        println!(
            "  verified: {} table(s) in {schema:?}, all row counts match",
            src_tables.len()
        );
    }

    if include == CopyInclude::Definition {
        let tenant = ctx.args.tenant.as_deref().expect("checked upfront");
        let src_env = ctx.args.src_env.as_str();
        let dst_env = ctx.args.dst_env.as_str();
        let applied = wamn_migrate::sql::select_applied_catalogs_sql();
        let src_rows = src_client.query(&applied, &[&tenant, &src_env]).await?;
        let dst_rows = dst_client.query(&applied, &[&tenant, &dst_env]).await?;
        anyhow::ensure!(
            src_rows.len() == dst_rows.len(),
            "verify FAILED: applied-catalog counts differ (src {}, dst {})",
            src_rows.len(),
            dst_rows.len()
        );
        for (s, d) in src_rows.iter().zip(dst_rows.iter()) {
            let (sid, did): (String, String) = (s.get(0), d.get(0));
            let (sdoc, ddoc): (Option<String>, Option<String>) = (s.get(2), d.get(2));
            anyhow::ensure!(
                sid == did && sdoc == ddoc,
                "verify FAILED: applied catalog {sid:?} differs on the dst"
            );
        }
        let fs = &ctx.args.flow_schema;
        for (label, sql) in [
            (
                "flows",
                format!("SELECT count(*) FROM {fs}.flows WHERE tenant_id = $1"),
            ),
            (
                "rls policies",
                "SELECT count(*) FROM catalog.rls_policies WHERE tenant_id = $1".to_string(),
            ),
        ] {
            let s: i64 = match src_client.query_one(sql.as_str(), &[&tenant]).await {
                Ok(row) => row.get(0),
                // The src may have no flow registry at all — nothing to compare.
                Err(_) if label == "flows" => continue,
                Err(e) => return Err(e).context("verify src counts"),
            };
            let d: i64 = dst_client
                .query_one(sql.as_str(), &[&tenant])
                .await
                .with_context(|| format!("verify dst {label}"))?
                .get(0);
            anyhow::ensure!(
                s == d,
                "verify FAILED: {label} row counts differ (src {s}, dst {d})"
            );
        }
        println!(
            "  verified: {} applied catalog(s) byte-equal; flows + RLS counts match",
            src_rows.len()
        );
    }

    drop(src_client);
    drop(dst_client);
    let _ = src_task.await;
    let _ = dst_task.await;
    Ok(())
}

/// The repoint: gated upstream (the saga check), here the operator-facing
/// runbook — the credential seam is a K8s Secret only `kubectl` can apply.
fn exec_cutover(ctx: &mut ExecCtx<'_>, src: &Triple, dst: &Triple) -> anyhow::Result<()> {
    println!(
        "  cutover recorded: repoint the serving identity {src} -> {dst}:\n    \
         1. apply the dst credential Secret (provision-project-env --emit-secret) / update \
         the workload's project config;\n    \
         2. the src {:?} stays quiesced (retired);\n    \
         3. keep the src database through a hold window, then deprovision \
         (--deprovision-old --confirm, or the printed DROP).",
        ctx.src_db
    );
    Ok(())
}

/// Drop the retained src database (confirm-gated; the plan appends this step
/// only when --deprovision-old was passed).
async fn exec_deprovision_old(ctx: &mut ExecCtx<'_>) -> anyhow::Result<()> {
    anyhow::ensure!(
        ctx.args.confirm,
        "--deprovision-old drops the retained src database {:?} — re-run with --confirm",
        ctx.src_db
    );
    let (client, task) = connect(ctx.src_admin).await?;
    client
        .batch_execute(&provision_sql::drop_database_named_sql(ctx.src_db))
        .await
        .context("drop the retained src database")?;
    drop(client);
    let _ = task.await;
    println!(
        "  dropped src database {:?} (delete its Database CR too: kubectl -n wamn-system \
         delete database {:?})",
        ctx.src_db, ctx.src_db
    );
    Ok(())
}

/// The copy saga recorder: a superuser connection to the T1 system DB acting as
/// `wamn_system`, driving the q3n.8 saga builders.
struct SagaRecorder {
    client: tokio_postgres::Client,
    saga_id: String,
    conn_task: tokio::task::JoinHandle<()>,
}

impl SagaRecorder {
    async fn connect(url: &str, saga_id: &str) -> anyhow::Result<Self> {
        let (client, conn) = tokio_postgres::connect(url, NoTls).await?;
        let conn_task = tokio::spawn(async move {
            let _ = conn.await;
        });
        client
            .batch_execute("SET ROLE wamn_system")
            .await
            .context("SET ROLE wamn_system")?;
        Ok(Self {
            client,
            saga_id: saga_id.to_string(),
            conn_task,
        })
    }

    async fn create(&self, target: &str, total_steps: i32) -> anyhow::Result<()> {
        self.client
            .execute(
                wamn_registry::sql::create_saga_sql(),
                &[&self.saga_id, &COPY_SAGA_KIND, &target, &Some(total_steps)],
            )
            .await
            .context("create the copy saga")?;
        Ok(())
    }

    async fn advance(&self) -> anyhow::Result<()> {
        self.client
            .execute(
                wamn_registry::sql::advance_saga_step_sql(),
                &[&self.saga_id],
            )
            .await
            .context("advance the saga step")?;
        Ok(())
    }

    /// `(status, step, total_steps)` — what the cutover gate checks.
    async fn state(&self) -> anyhow::Result<(String, i32, Option<i32>)> {
        let row = self
            .client
            .query_one(wamn_registry::sql::select_saga_sql(), &[&self.saga_id])
            .await
            .context("read the saga state")?;
        Ok((row.get(0), row.get(1), row.get(2)))
    }

    async fn fail(&self, err: &str) -> anyhow::Result<()> {
        self.client
            .execute(wamn_registry::sql::fail_saga_sql(), &[&self.saga_id, &err])
            .await
            .context("record the saga failure")?;
        Ok(())
    }

    async fn complete(&self) -> anyhow::Result<()> {
        self.client
            .execute(wamn_registry::sql::complete_saga_sql(), &[&self.saga_id])
            .await
            .context("complete the saga")?;
        Ok(())
    }
}

impl Drop for SagaRecorder {
    fn drop(&mut self) {
        self.conn_task.abort();
    }
}

async fn connect(
    url: &str,
) -> anyhow::Result<(tokio_postgres::Client, tokio::task::JoinHandle<()>)> {
    let (client, conn) = tokio_postgres::connect(url, NoTls)
        .await
        .with_context(|| "postgres connect".to_string())?;
    let task = tokio::spawn(async move {
        let _ = conn.await;
    });
    Ok((client, task))
}

/// Spawn an argv (built by a pure builder); fail on a non-zero exit.
fn run_argv(argv: &[String]) -> anyhow::Result<()> {
    let status = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .status()
        .with_context(|| format!("spawn {} (is it installed?)", argv[0]))?;
    anyhow::ensure!(status.success(), "{} failed ({status})", argv[0]);
    Ok(())
}

/// The data schema's table names, ordered (the verify step's comparison basis).
async fn list_tables(client: &tokio_postgres::Client, schema: &str) -> anyhow::Result<Vec<String>> {
    let rows = client.query(list_schema_tables_sql(), &[&schema]).await?;
    Ok(rows.iter().map(|r| r.get::<_, String>(0)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn include_arg_maps_onto_the_pure_axis() {
        assert_eq!(
            CopyInclude::from(IncludeArg::Definition).as_str(),
            "definition"
        );
        assert_eq!(CopyInclude::from(IncludeArg::Data).as_str(), "data");
        assert_eq!(CopyInclude::from(IncludeArg::Both).as_str(), "both");
    }
}
