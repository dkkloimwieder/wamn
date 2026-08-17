//! The `copy-project-env` subcommand (wamn-8df.5): the unified env-symmetric
//! **copy** between two `(org, project, env)` triples — deploy / promote /
//! clone / move in one operation (`docs/archive/platform/deployment-model.md` §4).
//!
//! The plan comes from the pure [`wamn_control_provision::plan_copy`]; this driver holds
//! the connections and executes each [`CopyStep`] by composing the shipped
//! machinery:
//!
//! * `include: definition` — the src env's **applied catalogs** promote into the
//!   dst through the 2.5 migrate engine (the same one-transaction apply
//!   `migrate-catalog` runs), plus the immutable **flow artifacts and release
//!   membership**, the **RLS
//!   policy rows** (re-compiled and applied on the dst), the **event
//!   registration rows** (EVT-REG — copied verbatim, no re-apply). Config has no
//!   defined artifact yet — deferred, noted at runtime.
//! * `include: data` — `pg_restore --data-only --disable-triggers` of the data
//!   schema from a fresh `pg_dump -Fd` snapshot (the q3n.10 artifact, recorded
//!   in `provisioning.dumps`).
//! * `include: both` — a full-fidelity `pg_restore` of the snapshot (schema +
//!   rows + ownership/ACLs; the dump carries the definition tables too).
//!
//! **The cutover gate (fixes cjv.7):** a `--cutover` copy is a *move* — the
//! pipeline `Quiesce → Snapshot → Restore → Verify → Cutover` is mandatory,
//! every step advances the dedicated `copy` saga in T1 operations state
//! (`provisioning.copy_sagas`), and the `Cutover` executor **re-reads the saga and
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
//! copy the dst carries the catalog storage schema (`deploy/sql/catalog-schema.sql`).
//! Mutable `flows.active` rows are not copied; immutable catalog releases are
//! the authoritative definition source.

use std::path::PathBuf;

use anyhow::{Context as _, bail};
use clap::{Args, ValueEnum};
use tokio_postgres::NoTls;
use tokio_postgres::error::SqlState;

use wamn_control_provision::{
    COPY_SAGA_KIND, CopyInclude, CopyMode, CopyRequest, CopyScope, CopyStep, count_rows_sql,
    dump_object_key, list_schema_tables_sql, pg_dump_argv, pg_restore_data_only_argv, plan_copy,
    project_env_database_name, quiesce_database_sql, sql as provision_sql,
    terminate_database_backends_sql, unquiesce_database_sql, validate_project_env,
};
use wamn_control_registry::Triple;

use crate::migrate_catalog::{
    ApplyOutcome, TargetReconciliationWindow, reconcile_catalog_target_in_transaction,
};
use crate::restore_project_env::swap_db;
use wamn_schema_control::BareSchemaName;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum IncludeArg {
    /// Structure only: catalog + flows + RLS policies + event registrations.
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
    /// saga (`provisioning.copy_sagas`) + the dump/confirmation records. Env
    /// `WAMN_SYSTEM_ADMIN_URL`. Required for destructive definition
    /// reconciliation attestations and for --cutover.
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

    /// Record a backup-checkpoint attestation for each destructive definition
    /// reconciliation. Requires --system-database-url; an exact durable record
    /// may also be reused by a retry.
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
    let data_schema = BareSchemaName::new(args.data_schema.clone())
        .with_context(|| format!("invalid --data-schema {:?}", args.data_schema))?;
    let flow_schema = BareSchemaName::new(args.flow_schema.clone())
        .with_context(|| format!("invalid --flow-schema {:?}", args.flow_schema))?;

    let src = Triple::new(&args.src_org, &args.src_project, args.src_env.as_str());
    let dst = Triple::new(&args.dst_org, &args.dst_project, args.dst_env.as_str());
    let include: CopyInclude = args.include.into();
    // wamn-0h0g.8.18 cutover: a definition copy writes the catalog, flow, and
    // release records whose durable owner is now the control database, so it would
    // promote a definition nothing reads. Refused for `definition` AND `both`
    // before ANY I/O — everything above this line is pure name validation, so no
    // plan has been printed, no dump directory touched, no cluster dialled.
    //
    // Matched explicitly rather than through `CopyInclude::wants_definition`,
    // which is FALSE for `Both`: `both` carries the definition implicitly through
    // a full `pg_restore` instead of a separate definition pass, so the helper
    // would have let exactly the widest case through.
    if matches!(include, CopyInclude::Definition | CopyInclude::Both) {
        bail!(crate::CONTROL_DEFINITION_PUBLISH_REFUSAL);
    }
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
    if (args.cutover || args.confirm_with_backup) && args.system_database_url.is_none() {
        bail!(
            "this copy requires --system-database-url: destructive reconciliation records its \
             attestation and cutover records its durable pipeline in the T1 registry"
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
        data_schema,
        flow_schema,
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
    data_schema: BareSchemaName,
    flow_schema: BareSchemaName,
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
                wamn_control_provision::state::record_dump_sql(),
                &[
                    &src.org,
                    &src.project,
                    &env,
                    &object_key,
                    &wamn_control_provision::dump::DUMP_FORMAT,
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
/// registrations, RLS policy rows + their compiled application, and event
/// registration rows. Config has no defined artifact yet — deferred.
async fn exec_copy_definition(
    ctx: &mut ExecCtx<'_>,
    _src: &Triple,
    dst: &Triple,
) -> anyhow::Result<()> {
    let tenant = ctx.args.tenant.as_deref().expect("checked upfront");
    let (src_client, src_task) = connect(&swap_db(ctx.src_admin, ctx.src_db)).await?;
    let (mut dst_client, dst_task) = connect(&swap_db(ctx.dst_admin, ctx.dst_db)).await?;

    for (side, client, hint) in [
        ("src", &src_client, "the src holds no catalog storage"),
        (
            "dst",
            &dst_client,
            "provision the dst first (apply deploy/sql/catalog-schema.sql)",
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
            &wamn_schema_control::sql::select_applied_catalogs_sql(),
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
        let cat = wamn_schema_control::Catalog::from_json(&doc)
            .with_context(|| format!("parse applied catalog {catalog_id:?}"))?;
        catalogs.push(cat);
    }
    let mut destination_bases = std::collections::BTreeMap::new();
    let mut destination_catalogs = std::collections::BTreeMap::new();
    for cat in &catalogs {
        let base = dst_client
            .query_opt(
                "SELECT applied_catalog_version FROM catalog.catalog_heads \
                 WHERE tenant_id = $1 AND catalog_id = $2 AND environment = $3",
                &[&tenant, &cat.catalog_id, &dst_env],
            )
            .await
            .with_context(|| format!("read destination base for {:?}", cat.catalog_id))?
            .map(|row| row.get::<_, i32>(0));
        destination_bases.insert(cat.catalog_id.clone(), base);
        let current = dst_client
            .query_opt(
                "SELECT document::text FROM catalog.catalogs \
                 WHERE tenant_id = $1 AND catalog_id = $2 AND environment = $3 \
                   AND state = 'applied'",
                &[&tenant, &cat.catalog_id, &dst_env],
            )
            .await
            .with_context(|| format!("read destination catalog {:?}", cat.catalog_id))?
            .map(|row| {
                let document: Option<String> = row.get(0);
                document
                    .context("destination applied catalog has no stored document")
                    .and_then(|document| {
                        wamn_schema_control::Catalog::from_json(&document)
                            .context("parse destination applied catalog")
                    })
            })
            .transpose()?;
        destination_catalogs.insert(cat.catalog_id.clone(), current);
    }
    if catalogs.is_empty() {
        println!("  no applied catalogs for tenant {tenant:?} in the src env");
    }
    let source_heads = src_client
        .query(
            "SELECT catalog_id, applied_catalog_version \
             FROM catalog.catalog_heads \
             WHERE tenant_id = $1 AND environment = $2 \
             ORDER BY catalog_id",
            &[&tenant, &src_env],
        )
        .await
        .context("read source catalog heads")?;

    // Destructive target reconciliation is authorized by a durable operations
    // record, not by an invocation-local flag. The flag mints that record; a
    // retry may consume an existing exact-window record. Planning here is
    // read-only, and the destination transaction below rechecks the locked
    // current-applied head against the recorded window immediately before DDL.
    let mut destructive_windows = Vec::new();
    for cat in &catalogs {
        let current = destination_catalogs
            .get(&cat.catalog_id)
            .and_then(Option::as_ref);
        let plan = wamn_schema_control::ops::compile_migration(current, cat)
            .with_context(|| format!("compile destination diff for {:?}", cat.catalog_id))?;
        if !plan.is_additive() {
            destructive_windows.push((
                cat.catalog_id.clone(),
                current.map(|catalog| catalog.version),
                cat.version,
            ));
        }
    }
    let mut authorizations = std::collections::BTreeMap::new();
    let confirmation_connection = if destructive_windows.is_empty() {
        None
    } else {
        let system_url = ctx.args.system_database_url.as_deref().context(
            "destructive definition reconciliation needs --system-database-url to read its \
             backup-checkpoint-attested operations record",
        )?;
        let (client, task) = connect(system_url)
            .await
            .context("system db connect (migration confirmation)")?;
        crate::ops_schema::install_and_enter(&client).await?;
        for (catalog_id, from_version, to_version) in &destructive_windows {
            let from_i32 = from_version
                .map(|version| i32::try_from(version).context("confirmation from version"))
                .transpose()?;
            let to_i32 = i32::try_from(*to_version).context("confirmation target version")?;
            if ctx.args.confirm_with_backup {
                client
                    .execute(
                        wamn_control_provision::state::record_migration_confirmation_sql(),
                        &[
                            &dst.org,
                            &dst.project,
                            &dst_env,
                            &tenant,
                            &catalog_id,
                            &from_i32,
                            &to_i32,
                        ],
                    )
                    .await
                    .with_context(|| format!("record migration confirmation for {catalog_id:?}"))?;
            }
            let row = client
                .query_opt(
                    wamn_control_provision::state::select_migration_confirmation_sql(),
                    &[
                        &dst.org,
                        &dst.project,
                        &dst_env,
                        &tenant,
                        &catalog_id,
                        &to_i32,
                    ],
                )
                .await
                .with_context(|| format!("read migration confirmation for {catalog_id:?}"))?
                .with_context(|| {
                    format!(
                        "destructive target reconciliation {from_version:?} -> {to_version} for \
                         catalog {catalog_id:?} has no backup-checkpoint-attested operations record"
                    )
                })?;
            let recorded_from: Option<i32> = row.get(0);
            let kind: String = row.get(1);
            let _confirmed_at: chrono::DateTime<chrono::Utc> = row.get(2);
            let confirmed_by: String = row.get(3);
            validate_migration_confirmation(
                catalog_id,
                recorded_from,
                &kind,
                &confirmed_by,
                from_i32,
                to_i32,
            )?;
            authorizations.insert(
                catalog_id.clone(),
                TargetReconciliationWindow {
                    from_version: *from_version,
                    to_version: *to_version,
                },
            );
        }
        Some((client, task))
    };

    let tx = dst_client
        .transaction()
        .await
        .context("begin atomic definition copy")?;
    let mut deferred_journals = std::collections::BTreeMap::new();
    for cat in &catalogs {
        match reconcile_catalog_target_in_transaction(
            &tx,
            tenant,
            dst_env,
            &ctx.data_schema,
            cat,
            None,
            authorizations.remove(&cat.catalog_id),
            false,
        )
        .await
        .with_context(|| format!("promote catalog {:?} into the dst", cat.catalog_id))?
        {
            ApplyOutcome::Applied(plan) => {
                let journal = plan
                    .statements
                    .iter()
                    .find(|statement| {
                        crate::migrate_catalog::is_migration_journal_statement(statement)
                    })
                    .cloned()
                    .context("applied catalog plan has no migration journal")?;
                deferred_journals.insert(plan.catalog_id.clone(), journal);
                println!(
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
                );
            }
            ApplyOutcome::AlreadyApplied { version } => println!(
                "  catalog {:?}: version {version} already applied (skip)",
                cat.catalog_id
            ),
        }
    }

    // 2a. Immutable release artifacts + membership. The complete release slice
    //     is copied in one destination transaction; a missing artifact or
    //     different-content retry rolls the entire slice back.
    if !source_heads.is_empty() {
        for head in &source_heads {
            let catalog_id: String = head.get(0);
            let catalog_version: i32 = head.get(1);
            let expected_base = destination_bases
                .get(&catalog_id)
                .copied()
                .context("source head has no copied catalog")?;
            let locked_head = tx
                .query_opt(
                    wamn_schema_control::sql::lock_catalog_head_sql(),
                    &[&tenant, &catalog_id, &dst_env],
                )
                .await
                .with_context(|| format!("lock destination head for {catalog_id:?}"))?;
            let applied_version = locked_head.map(|row| row.get::<_, i32>(0));
            let runs_present: bool = tx
                .query_one(
                    &format!(
                        "SELECT to_regclass('{}.runs') IS NOT NULL",
                        ctx.flow_schema.as_str()
                    ),
                    &[],
                )
                .await?
                .get(0);
            let nonterminal_runs = if runs_present {
                if let Some(applied) = applied_version {
                    tx.query_one(
                        &wamn_schema_control::sql::count_nonterminal_release_runs_sql(
                            ctx.flow_schema.as_str(),
                        ),
                        &[&tenant, &catalog_id, &applied],
                    )
                    .await
                    .context("check destination release-pinned runs")?
                    .get(0)
                } else {
                    0_i64
                }
            } else {
                0_i64
            };
            guard_copy_destination(expected_base, applied_version, nonterminal_runs)?;
            let source_manifest: String = src_client
                .query_one(
                    "SELECT members_json::text FROM catalog.release_manifests \
                     WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = $3",
                    &[&tenant, &catalog_id, &catalog_version],
                )
                .await
                .with_context(|| format!("read release manifest for {catalog_id:?}"))?
                .get(0);
            if let Some(existing) = tx
                .query_opt(
                    "SELECT members_json::text FROM catalog.release_manifests \
                     WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = $3",
                    &[&tenant, &catalog_id, &catalog_version],
                )
                .await
                .context("preflight destination release membership")?
            {
                let existing: String = existing.get(0);
                let existing: serde_json::Value =
                    serde_json::from_str(&existing).context("parse destination manifest")?;
                let source: serde_json::Value =
                    serde_json::from_str(&source_manifest).context("parse source manifest")?;
                anyhow::ensure!(existing == source, "catalog-release-content-conflict");
            }
            let artifacts = src_client
                .query(
                    wamn_schema_control::sql::select_release_artifacts_sql(),
                    &[&tenant, &catalog_id, &catalog_version],
                )
                .await
                .with_context(|| format!("read release artifacts for {catalog_id:?}"))?;
            for artifact in &artifacts {
                let flow_id: String = artifact.get(0);
                let flow_version: i32 = artifact.get(1);
                let schema_version: String = artifact.get(2);
                let graph_json: String = artifact.get(3);
                let graph_hash: String = artifact.get(4);
                let artifact_hash: String = artifact.get(5);
                let execution_bundle_hash: String = artifact.get(6);
                let execution_bundle_bytes: Vec<u8> = artifact.get(7);
                let flow_version_u32 =
                    u32::try_from(flow_version).context("copied flow version must be positive")?;
                wamn_catalog::PinnedArtifact::from_storage(
                    tenant,
                    &flow_id,
                    flow_version_u32,
                    &graph_json,
                    &graph_hash,
                    &artifact_hash,
                )
                .with_context(|| {
                    format!("verify copied immutable flow {flow_id} v{flow_version}")
                })?;
                tx.execute(
                    wamn_schema_control::sql::register_flow_artifact_sql(),
                    &[
                        &tenant,
                        &flow_id,
                        &flow_version,
                        &schema_version,
                        &graph_json,
                        &graph_hash,
                        &artifact_hash,
                    ],
                )
                .await
                .with_context(|| format!("copy immutable flow {flow_id} v{flow_version}"))?;
                tx.execute(
                    wamn_schema_control::sql::insert_execution_bundle_sql(),
                    &[&tenant, &execution_bundle_hash, &execution_bundle_bytes],
                )
                .await
                .with_context(|| format!("copy execution bundle for {flow_id} v{flow_version}"))?;
            }
            tx.execute(
                wamn_schema_control::sql::publication_boundary_sql(),
                &[&"after-artifacts"],
            )
            .await
            .context("copy boundary after artifacts")?;
            tx.execute(
                wamn_schema_control::sql::register_release_manifest_sql(),
                &[&tenant, &catalog_id, &catalog_version, &source_manifest],
            )
            .await
            .with_context(|| format!("seal copied release {catalog_id:?}"))?;
            for artifact in &artifacts {
                let flow_id: String = artifact.get(0);
                let flow_version: i32 = artifact.get(1);
                let execution_bundle_hash: String = artifact.get(6);
                tx.execute(
                    wamn_schema_control::sql::insert_release_flow_sql(),
                    &[
                        &tenant,
                        &catalog_id,
                        &catalog_version,
                        &flow_id,
                        &flow_version,
                        &execution_bundle_hash,
                    ],
                )
                .await
                .with_context(|| format!("copy release member {flow_id} v{flow_version}"))?;
                let exact: bool = tx
                    .query_one(
                        "SELECT EXISTS (SELECT 1 FROM catalog.release_flows \
                         WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = $3 \
                           AND flow_id = $4 AND flow_version = $5 \
                           AND execution_bundle_hash = $6)",
                        &[
                            &tenant,
                            &catalog_id,
                            &catalog_version,
                            &flow_id,
                            &flow_version,
                            &execution_bundle_hash,
                        ],
                    )
                    .await?
                    .get(0);
                anyhow::ensure!(exact, "catalog-release-content-conflict");
            }
            let exposure_manifest: String = src_client
                .query_one(
                    "SELECT definitions_json::text \
                     FROM catalog.release_exposure_manifests \
                     WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = $3",
                    &[&tenant, &catalog_id, &catalog_version],
                )
                .await
                .with_context(|| format!("read exposure manifest for {catalog_id:?}"))?
                .get(0);
            tx.execute(
                wamn_schema_control::sql::register_release_exposure_manifest_sql(),
                &[&tenant, &catalog_id, &catalog_version, &exposure_manifest],
            )
            .await
            .with_context(|| format!("seal copied exposure {catalog_id:?}"))?;
            let sources = src_client
                .query(
                    wamn_schema_control::sql::select_release_sources_sql(),
                    &[&tenant, &catalog_id, &catalog_version],
                )
                .await
                .with_context(|| format!("read release sources for {catalog_id:?}"))?;
            for source in &sources {
                let source_id: String = source.get(0);
                let source_kind: String = source.get(1);
                let definition_json: String = source.get(2);
                let source_hash: String = source.get(3);
                tx.execute(
                    wamn_schema_control::sql::insert_release_source_sql(),
                    &[
                        &tenant,
                        &catalog_id,
                        &catalog_version,
                        &source_id,
                        &source_kind,
                        &definition_json,
                        &source_hash,
                    ],
                )
                .await
                .with_context(|| format!("copy release source {source_id:?}"))?;
            }
            let attachments = src_client
                .query(
                    wamn_schema_control::sql::select_release_attachments_sql(),
                    &[&tenant, &catalog_id, &catalog_version],
                )
                .await
                .with_context(|| format!("read release attachments for {catalog_id:?}"))?;
            for attachment in &attachments {
                let attachment_id: String = attachment.get(0);
                let attachment_kind: String = attachment.get(1);
                let flow_id: String = attachment.get(2);
                let source_id: String = attachment.get(3);
                let definition_hash: String = attachment.get(4);
                let definition_json: String = attachment.get(5);
                let route_host: Option<String> = attachment.get(6);
                let route_path: Option<String> = attachment.get(7);
                let route_template: Option<String> = attachment.get(8);
                let route_method: Option<String> = attachment.get(9);
                tx.execute(
                    wamn_schema_control::sql::insert_release_attachment_sql(),
                    &[
                        &tenant,
                        &catalog_id,
                        &catalog_version,
                        &attachment_id,
                        &attachment_kind,
                        &flow_id,
                        &source_id,
                        &definition_hash,
                        &definition_json,
                        &route_host,
                        &route_path,
                        &route_template,
                        &route_method,
                    ],
                )
                .await
                .with_context(|| format!("copy release attachment {attachment_id:?}"))?;
            }
            tx.execute(
                wamn_schema_control::sql::apply_release_exposure_sql(),
                &[
                    &tenant,
                    &catalog_id,
                    &dst_env,
                    &catalog_version,
                    &"copy-project-env",
                ],
            )
            .await
            .context("carry copied attachment activation")?;
            let copied: i64 = tx
                .query_one(
                    "SELECT count(*) FROM catalog.release_flows \
                     WHERE tenant_id = $1 AND catalog_id = $2 AND catalog_version = $3",
                    &[&tenant, &catalog_id, &catalog_version],
                )
                .await?
                .get(0);
            anyhow::ensure!(
                copied == i64::try_from(artifacts.len()).unwrap_or(i64::MAX),
                "catalog-release-content-conflict"
            );
            tx.execute(
                wamn_schema_control::sql::publication_boundary_sql(),
                &[&"after-members"],
            )
            .await
            .context("copy boundary after-members")?;
            if let Some(journal) = deferred_journals.remove(&catalog_id) {
                crate::migrate_catalog::execute_migration_statement(&tx, &journal)
                    .await
                    .with_context(|| format!("record copied catalog journal for {catalog_id:?}"))?;
            }
            tx.execute(
                wamn_schema_control::sql::publication_boundary_sql(),
                &[&"after-journal"],
            )
            .await
            .context("copy boundary after-journal")?;
            tx.execute(
                wamn_schema_control::sql::publication_boundary_sql(),
                &[&"before-head"],
            )
            .await
            .context("copy boundary before-head")?;
            tx.execute(
                wamn_schema_control::sql::advance_catalog_head_sql(),
                &[&tenant, &catalog_id, &dst_env, &catalog_version],
            )
            .await
            .with_context(|| format!("advance destination head for {catalog_id:?}"))?;
        }
        println!(
            "  immutable releases: {} catalog release(s) copied atomically",
            source_heads.len()
        );
    }
    anyhow::ensure!(
        deferred_journals.is_empty(),
        "copied catalog plan has no matching source release head: {:?}",
        deferred_journals.keys().collect::<Vec<_>>()
    );
    tx.commit().await.context("commit atomic definition copy")?;

    println!("  mutable flows.active registry: not copied (immutable release is authoritative)");

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
        apply_rls_policies(&dst_client, &ctx.data_schema, &catalogs, &policies).await?;
    }
    println!("  rls: {} policy row(s) copied + applied", policies.len());

    // 4. Event registrations (EVT-REG): copy the tenant's registration rows
    //    verbatim (the jsonb document + its denormalized flow_id/entity_id).
    //    Like flows — and unlike RLS policies — a registration is pure data with
    //    no compiled Postgres artifact, so there is no re-apply step; the copy
    //    mirrors the rls_policies row copy (superuser bypasses the tenant-FORCE
    //    RLS; the tenant filter scopes the read).
    let registrations = src_client
        .query(
            "SELECT catalog_id, registration_id, flow_id, entity_id, registration::text \
             FROM catalog.event_registrations WHERE tenant_id = $1",
            &[&tenant],
        )
        .await
        .context("read src event registrations")?;
    for row in &registrations {
        let (catalog_id, registration_id, flow_id, entity_id): (String, String, String, String) =
            (row.get(0), row.get(1), row.get(2), row.get(3));
        let registration: String = row.get(4);
        dst_client
            .execute(
                "INSERT INTO catalog.event_registrations \
                   (tenant_id, catalog_id, registration_id, flow_id, entity_id, registration) \
                 VALUES ($1, $2, $3, $4, $5, $6::text::jsonb) \
                 ON CONFLICT (tenant_id, catalog_id, registration_id) DO UPDATE SET \
                   flow_id = EXCLUDED.flow_id, entity_id = EXCLUDED.entity_id, \
                   registration = EXCLUDED.registration",
                &[
                    &tenant,
                    &catalog_id,
                    &registration_id,
                    &flow_id,
                    &entity_id,
                    &registration,
                ],
            )
            .await
            .with_context(|| format!("copy event registration {registration_id}"))?;
    }
    println!(
        "  event registrations: {} row(s) copied",
        registrations.len()
    );

    println!("  config: no defined artifact yet — deferred");

    drop(src_client);
    drop(dst_client);
    if let Some((client, task)) = confirmation_connection {
        drop(client);
        let _ = task.await;
    }
    let _ = src_task.await;
    let _ = dst_task.await;
    Ok(())
}

fn validate_migration_confirmation(
    catalog_id: &str,
    recorded_from: Option<i32>,
    kind: &str,
    confirmed_by: &str,
    expected_from: Option<i32>,
    expected_to: i32,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        recorded_from == expected_from
            && kind == wamn_control_provision::state::MIGRATION_CONFIRMATION_KIND,
        "migration confirmation for {catalog_id:?} does not authorize the current \
         window: recorded {recorded_from:?} -> {expected_to} kind {kind:?}, expected \
         {expected_from:?} -> {expected_to} kind {:?}",
        wamn_control_provision::state::MIGRATION_CONFIRMATION_KIND,
    );
    anyhow::ensure!(
        !confirmed_by.is_empty(),
        "migration confirmation for {catalog_id:?} has no confirming principal"
    );
    Ok(())
}

fn guard_copy_destination(
    expected_base: Option<i32>,
    applied_version: Option<i32>,
    nonterminal_runs: i64,
) -> anyhow::Result<()> {
    wamn_schema_control::guard_publication(&wamn_schema_control::PublicationGuard {
        expected_base,
        applied_version,
        nonterminal_runs,
        unresolved_sources: &[],
    })
    .map_err(anyhow::Error::new)
}

/// Re-compile the copied RLS policy rows per catalog and apply the compiled
/// plans on the dst. Each `CREATE POLICY` runs autocommit with the data schema
/// on the search path; a policy that already exists (`duplicate_object`) is an
/// idempotent skip — the re-copy case.
async fn apply_rls_policies(
    dst: &tokio_postgres::Client,
    data_schema: &BareSchemaName,
    catalogs: &[wamn_schema_control::Catalog],
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
            "schema-version": wamn_schema_compiler::rls::SCHEMA_VERSION,
            "catalog-id": catalog_id,
            "rules": rules,
        });
        let policy = wamn_schema_compiler::rls::AccessPolicy::from_json(&policy_json.to_string())
            .with_context(|| format!("assemble the RLS policy set for {catalog_id:?}"))?;
        let plan = wamn_schema_compiler::rls::compile(&policy, cat)
            .map_err(|e| anyhow::anyhow!("compile RLS policies for {catalog_id:?}: {e}"))?;
        for op in &plan.operations {
            let sql = op
                .additive_sql()
                .context("RLS compilation emitted a destructive operation")?;
            match dst.batch_execute(sql).await {
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
/// schema (`--data-only --disable-triggers` — no trigger may fire per restored
/// row); a full restore keeps ownership + ACLs (the dst cluster
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
        pg_restore_data_only_argv(&dst_url, &dump_dir, ctx.data_schema.as_str())
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
/// document is byte-equal on the dst, and immutable releases / RLS rows match.
async fn exec_verify(
    ctx: &mut ExecCtx<'_>,
    _src: &Triple,
    _dst: &Triple,
    include: CopyInclude,
) -> anyhow::Result<()> {
    let (src_client, src_task) = connect(&swap_db(ctx.src_admin, ctx.src_db)).await?;
    let (dst_client, dst_task) = connect(&swap_db(ctx.dst_admin, ctx.dst_db)).await?;

    if include.wants_data() {
        let schema = &ctx.data_schema;
        let src_tables = list_tables(&src_client, schema.as_str())
            .await
            .context("list src tables")?;
        let dst_tables = list_tables(&dst_client, schema.as_str())
            .await
            .context("list dst tables")?;
        anyhow::ensure!(
            src_tables == dst_tables,
            "verify FAILED: table sets differ in schema {schema:?} (src {src_tables:?}, \
             dst {dst_tables:?})"
        );
        for table in &src_tables {
            let sql = count_rows_sql(schema.as_str(), table);
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
        let applied = wamn_schema_control::sql::select_applied_catalogs_sql();
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
        for (label, sql) in [
            (
                "rls policies",
                "SELECT count(*) FROM catalog.rls_policies WHERE tenant_id = $1".to_string(),
            ),
            (
                "event registrations",
                "SELECT count(*) FROM catalog.event_registrations WHERE tenant_id = $1".to_string(),
            ),
        ] {
            let s: i64 = src_client
                .query_one(sql.as_str(), &[&tenant])
                .await
                .context("verify src counts")?
                .get(0);
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
            "  verified: {} applied catalog(s) byte-equal; flows + RLS + \
             event-registration counts match",
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

/// The copy saga recorder: a superuser connection installs the idempotent ops
/// artifact as `wamn_system`, then executes through the bounded `wamn_ops` role.
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
        crate::ops_schema::install_and_enter(&client).await?;
        Ok(Self {
            client,
            saga_id: saga_id.to_string(),
            conn_task,
        })
    }

    async fn create(&self, target: &str, total_steps: i32) -> anyhow::Result<()> {
        self.client
            .execute(
                wamn_control_provision::state::create_saga_sql(),
                &[&self.saga_id, &COPY_SAGA_KIND, &target, &Some(total_steps)],
            )
            .await
            .context("create the copy saga")?;
        Ok(())
    }

    async fn advance(&self) -> anyhow::Result<()> {
        self.client
            .execute(
                wamn_control_provision::state::advance_saga_step_sql(),
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
            .query_one(
                wamn_control_provision::state::select_saga_sql(),
                &[&self.saga_id],
            )
            .await
            .context("read the saga state")?;
        Ok((row.get(0), row.get(1), row.get(2)))
    }

    async fn fail(&self, err: &str) -> anyhow::Result<()> {
        self.client
            .execute(
                wamn_control_provision::state::fail_saga_sql(),
                &[&self.saga_id, &err],
            )
            .await
            .context("record the saga failure")?;
        Ok(())
    }

    async fn complete(&self) -> anyhow::Result<()> {
        self.client
            .execute(
                wamn_control_provision::state::complete_saga_sql(),
                &[&self.saga_id],
            )
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

    /// The one axis value that still copies, and the two that no longer do.
    ///
    /// `Both` is the trap: `wants_definition()` is FALSE for it, so a guard
    /// written on that helper would have let the widest case through.
    #[test]
    fn only_the_data_axis_asks_for_a_definition_this_plane_no_longer_owns() {
        assert!(CopyInclude::Definition.wants_definition());
        assert!(
            !CopyInclude::Both.wants_definition(),
            "the refusal must not be written on wants_definition()"
        );
        assert!(CopyInclude::Both.wants_data() && CopyInclude::Data.wants_data());
        assert!(!CopyInclude::Definition.wants_data());
        // Flag closure: every representable axis is classified, and exactly one is
        // still available.
        let closed = |include: CopyInclude| {
            matches!(include, CopyInclude::Definition | CopyInclude::Both)
        };
        assert!(closed(CopyInclude::from(IncludeArg::Definition)));
        assert!(closed(CopyInclude::from(IncludeArg::Both)));
        assert!(!closed(CopyInclude::from(IncludeArg::Data)));
        // `--include` defaults to `both`, so a bare invocation refuses too.
        let source = include_str!("copy_project_env.rs");
        assert!(source.contains(r#"#[arg(long, value_enum, default_value = "both")]"#));
    }

    /// wamn-0h0g.8.18: the closed axes refuse before ANY I/O, and the open one
    /// still gets past the refusal.
    ///
    /// Every URL is unroutable and the dump root does not exist, so any I/O would
    /// surface as its own context literal instead of the refusal — and a
    /// connection attempt would stall rather than return promptly.
    #[tokio::test]
    async fn definition_and_both_refuse_before_any_connection_or_file() {
        let args = |include: IncludeArg| CopyProjectEnvArgs {
            src_org: "acme".to_owned(),
            src_project: "receiving".to_owned(),
            src_env: "dev".to_owned(),
            dst_org: "acme".to_owned(),
            dst_project: "receiving".to_owned(),
            dst_env: "prod".to_owned(),
            include,
            cutover: false,
            deprovision_old: false,
            confirm: false,
            src_admin_url: Some("postgresql://invalid.invalid/never".to_owned()),
            dst_admin_url: None,
            system_database_url: Some("postgresql://invalid.invalid/never".to_owned()),
            tenant: Some("tenant-a".to_owned()),
            data_schema: "public".to_owned(),
            flow_schema: "wamn_run".to_owned(),
            dump_root: std::env::temp_dir().join("control-copy-closed-8-18"),
            confirm_with_backup: false,
            plan: false,
            saga_id: None,
        };
        for closed in [IncludeArg::Definition, IncludeArg::Both] {
            let error = run(args(closed))
                .await
                .expect_err("a definition-bearing copy must refuse");
            let message = format!("{error:#}");
            assert_eq!(message, crate::CONTROL_DEFINITION_PUBLISH_REFUSAL);
            for io in [
                "postgres connect",
                "system db connect",
                "create the copy saga",
                "spawn ",
                "pg_dump",
            ] {
                assert!(!message.contains(io), "the copy reached {io}: {message}");
            }
        }
        // Data-only still works: it must fail LATER, on the connection, not here.
        let error = run(args(IncludeArg::Data))
            .await
            .expect_err("an unroutable data copy fails at its connection");
        let message = format!("{error:#}");
        assert_ne!(message, crate::CONTROL_DEFINITION_PUBLISH_REFUSAL);
        assert!(
            !message.contains("control-definition-publish-requires"),
            "data-only copy was refused: {message}"
        );
    }

    /// The refusal precedes every effect in `run`, measured on the source itself:
    /// no plan is printed, no clock read, no connection opened.
    #[test]
    fn the_copy_refusal_precedes_every_effect_in_run() {
        let source = include_str!("copy_project_env.rs");
        let run_body = source
            .split("pub async fn run(args: CopyProjectEnvArgs)")
            .nth(1)
            .expect("the copy verb exists")
            .split("\n/// ")
            .next()
            .expect("the verb body ends");
        let refusal = run_body
            .find("CONTROL_DEFINITION_PUBLISH_REFUSAL")
            .expect("the verb refuses");
        for effect in [
            "plan_copy(",
            "println!(",
            "unix_seconds()",
            "SagaRecorder::connect(",
            "execute_steps(",
        ] {
            let at = run_body
                .find(effect)
                .unwrap_or_else(|| panic!("the verb performs {effect}"));
            assert!(refusal < at, "{effect} runs before the refusal");
        }
    }

    #[test]
    fn destination_promotion_refuses_head_drift_and_nonterminal_runs() {
        let drift = guard_copy_destination(Some(3), Some(4), 0).unwrap_err();
        assert!(format!("{drift:#}").contains("catalog-release-stale-base"));
        let pinned = guard_copy_destination(Some(3), Some(3), 1).unwrap_err();
        assert!(format!("{pinned:#}").contains("catalog-release-has-nonterminal-runs"));
        assert!(wamn_schema_control::sql::advance_catalog_head_sql().contains("DO UPDATE"));
    }

    #[test]
    fn migration_confirmation_requires_the_exact_window_kind_and_actor() {
        let kind = wamn_control_provision::state::MIGRATION_CONFIRMATION_KIND;
        validate_migration_confirmation("orders", Some(2), kind, "operator", Some(2), 3).unwrap();
        assert!(
            validate_migration_confirmation("orders", Some(1), kind, "operator", Some(2), 3)
                .is_err()
        );
        assert!(
            validate_migration_confirmation("orders", Some(2), "other", "operator", Some(2), 3)
                .is_err()
        );
        assert!(validate_migration_confirmation("orders", Some(2), kind, "", Some(2), 3).is_err());
    }

    #[test]
    fn definition_copy_orders_members_then_journal_then_head_fault_boundaries() {
        let source = include_str!("copy_project_env.rs");
        let after_artifacts = source.find("copy boundary after artifacts").unwrap();
        let after_members = source.find("copy boundary after-members").unwrap();
        let deferred_journal = source.find("record copied catalog journal for").unwrap();
        let after_journal = source.find("copy boundary after-journal").unwrap();
        let before_head = source.find("copy boundary before-head").unwrap();
        let head = source.find("advance destination head for").unwrap();
        assert!(
            after_artifacts < after_members
                && after_members < deferred_journal
                && deferred_journal < after_journal
                && after_journal < before_head
                && before_head < head
        );
        assert_ne!(after_members, after_journal);
    }
}
