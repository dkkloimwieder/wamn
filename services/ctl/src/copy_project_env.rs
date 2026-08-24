//! The `copy-project-env` subcommand (wamn-8df.5): the env-symmetric
//! **data copy** between two `(org, project, env)` triples.
//!
//! The plan comes from the pure [`wamn_control_provision::plan_copy`]; this driver holds
//! the connections and executes each [`CopyStep`] by composing the shipped
//! machinery.
//!
//! Definition promotion now has one production owner, `wamn-ctl promote`.
//! This operations verb retains only `pg_restore --data-only
//! --disable-triggers` from a fresh `pg_dump -Fd` snapshot (the q3n.10
//! artifact, recorded in `provisioning.dumps`).
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
//! Precondition (this tool copies, it does not provision): the dst database
//! exists (`provision-project-env` + its Database CR).

use std::path::PathBuf;

use anyhow::{Context as _, bail};
use clap::Args;
use tokio_postgres::NoTls;
use tokio_postgres::error::SqlState;

use wamn_control_provision::{
    COPY_SAGA_KIND, CopyRequest, CopyStep, count_rows_sql, dump_object_key, list_schema_tables_sql,
    pg_dump_argv, pg_restore_data_only_argv, plan_copy, project_env_database_name,
    quiesce_database_sql, sql as provision_sql, terminate_database_backends_sql,
    unquiesce_database_sql, validate_project_env,
};
use wamn_control_registry::Triple;

use crate::restore_project_env::swap_db;
use wamn_schema_control::BareSchemaName;

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

    /// The data schema the entity tables live in (verify counts it; a
    /// data-only restore is scoped to it).
    #[arg(long, default_value = "public")]
    pub data_schema: String,

    /// Directory snapshots are staged under (a per-timestamp subdirectory —
    /// the `dump-project-env --run-now` layout).
    #[arg(long, default_value = "/tmp/wamn-dump")]
    pub dump_root: PathBuf,

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

    let src = Triple::new(&args.src_org, &args.src_project, args.src_env.as_str());
    let dst = Triple::new(&args.dst_org, &args.dst_project, args.dst_env.as_str());
    let request = CopyRequest {
        src: src.clone(),
        dst: dst.clone(),
        cutover: args.cutover,
        deprovision_old: args.deprovision_old,
    };
    let steps = plan_copy(&request).map_err(|e| anyhow::anyhow!("{e}"))?;

    println!(
        "data copy {src} -> {dst} ({}):",
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

    let src_admin = args
        .src_admin_url
        .as_deref()
        .context("copy needs --src-admin-url (a superuser URL to the SOURCE cluster)")?;
    let dst_admin = args.dst_admin_url.as_deref().unwrap_or(src_admin);
    let system_url = args
        .system_database_url
        .as_deref()
        .context("copy requires --system-database-url to resolve both stored instance suffixes")?;
    let src_instance =
        crate::provision_project_env::read_project_env_instance(system_url, &src).await?;
    let dst_instance =
        crate::provision_project_env::read_project_env_instance(system_url, &dst).await?;
    let src_db = project_env_database_name(&src.org, &src.project, src.env.as_str(), &src_instance);
    let dst_db = project_env_database_name(&dst.org, &dst.project, dst.env.as_str(), &dst_instance);
    let saga_id = args.saga_id.clone().unwrap_or_else(|| {
        format!(
            "copy-{src_db}-to-{dst_db}-{}",
            crate::dump_project_env::unix_seconds()
        )
    });

    let r = SagaRecorder::connect(system_url, &saga_id)
        .await
        .context("system db connect (saga recording)")?;
    r.create(&format!("{src} -> {dst}"), steps.len() as i32)
        .await?;
    println!("recording saga {saga_id:?} ({} steps)", steps.len());
    let recorder = Some(r);

    let mut ctx = ExecCtx {
        args: &args,
        src_admin,
        dst_admin,
        src_db: &src_db,
        dst_db: &dst_db,
        data_schema,
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
            CopyStep::RestoreData { .. } => exec_restore_data(ctx).await?,
            CopyStep::Verify { src, dst } => exec_verify(ctx, src, dst).await?,
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

/// `pg_restore --data-only --disable-triggers` the snapshot into the dst data
/// schema. A restore replays state; no trigger may fire per restored row.
async fn exec_restore_data(ctx: &mut ExecCtx<'_>) -> anyhow::Result<()> {
    let dump_dir = ctx
        .dump_dir
        .as_ref()
        .context("restore without a snapshot (unreachable: the plan orders Snapshot first)")?
        .to_string_lossy()
        .to_string();
    let dst_url = swap_db(ctx.dst_admin, ctx.dst_db);
    let argv = pg_restore_data_only_argv(&dst_url, &dump_dir, ctx.data_schema.as_str());
    run_argv(&argv)?;
    println!("  restored data into {:?}", ctx.dst_db);
    Ok(())
}

/// Verify the dst data-schema table set and exact row counts against the src.
async fn exec_verify(ctx: &mut ExecCtx<'_>, _src: &Triple, _dst: &Triple) -> anyhow::Result<()> {
    let (src_client, src_task) = connect(&swap_db(ctx.src_admin, ctx.src_db)).await?;
    let (dst_client, dst_task) = connect(&swap_db(ctx.dst_admin, ctx.dst_db)).await?;

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
