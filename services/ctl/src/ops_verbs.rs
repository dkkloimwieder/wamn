//! Arguments and output of the operational verbs of the `wamn-ctl-ops` binary.
//!
//! The work runs in `wamn-control`. This module holds the clap surface and the
//! printed lines.

use std::path::PathBuf;

use anyhow::Context as _;
use clap::Args;
use wamn_control::copy_project_env::{CopyProjectEnvRequest, plan_project_env_copy};
use wamn_control::dump_project_env::DumpProjectEnvRequest;
use wamn_control::event_advisories::{EventAdvisoriesRequest, read_retained_advisories};
use wamn_control::prune_record_history::{
    PruneRecordHistoryRequest, PrunedHistory, prune_expired_record_history,
};
use wamn_control::prune_run_history::{PruneRunHistoryRequest, prune_terminal_run_history};
use wamn_control::restore_project_env::{
    DumpSelection, RestoreOutcome, RestoreProjectEnvRequest,
};
use wamn_control_provision::{DEFAULT_BUCKET, DEFAULT_DUMP_SCHEDULE};
use wamn_control_registry::Triple;

/// Retained broker advisory reporting arguments.
#[derive(Debug, Args)]
pub struct EventAdvisoriesArgs {
    /// Event-plane NATS with the retained advisory stream.
    #[arg(long, env = "WAMN_EVT_NATS_URL")]
    pub nats_url: String,

    /// Event-broker username; requires its password file.
    #[arg(long, env = "WAMN_EVT_NATS_USERNAME", requires = "nats_password_file")]
    pub nats_username: Option<String>,

    /// File containing the event-broker password; requires its username.
    #[arg(long, env = "WAMN_EVT_NATS_PASSWORD_FILE", requires = "nats_username")]
    pub nats_password_file: Option<PathBuf>,

    /// Exact source stream from the broker advisory.
    #[arg(long)]
    pub stream: String,

    /// Exact durable consumer from the broker advisory.
    #[arg(long)]
    pub consumer: String,

    /// Maximum advisory records to print.
    #[arg(long, default_value_t = 100)]
    pub limit: usize,
}

/// Read the retained broker advisories of one consumer and print one JSON
/// record per line.
pub async fn event_advisories(args: EventAdvisoriesArgs) -> anyhow::Result<()> {
    let records = read_retained_advisories(EventAdvisoriesRequest {
        nats_url: args.nats_url,
        nats_username: args.nats_username,
        nats_password_file: args.nats_password_file,
        stream: args.stream,
        consumer: args.consumer,
        limit: args.limit,
    })
    .await?;
    for record in &records {
        println!("{}", serde_json::to_string(record)?);
    }
    Ok(())
}

/// Record history retention arguments.
#[derive(Debug, Args)]
pub struct PruneRecordHistoryArgs {
    /// Postgres URL for this tenant's `wamn_audit_retention` credential
    /// generation. The verb refuses any other login. Env `WAMN_PG_URL`.
    #[arg(long, env = "WAMN_PG_URL")]
    pub database_url: String,

    /// The tenant that the mounted audit retention credential was minted for.
    /// A different tenant refuses before any statement runs.
    #[arg(long)]
    pub tenant: String,
}

/// Terminal run history retention arguments.
#[derive(Debug, Args)]
pub struct PruneRunHistoryArgs {
    /// Postgres URL for this tenant's `wamn_run_retention` credential generation
    /// — the NOSUPERUSER/NOBYPASSRLS role whose whole authority is the terminal
    /// run delete. The verb refuses any other identity. Env `WAMN_PG_URL`.
    #[arg(long, env = "WAMN_PG_URL")]
    pub database_url: String,

    /// The run-plane schema the `runs` table lives in (set as the session
    /// `search_path`). Bare identifier, and REQUIRED: the statement this
    /// verb drives is `DELETE FROM runs` — UNQUALIFIED — so it resolves through
    /// that session `search_path`. A default would let an invocation that omits
    /// the flag prune a relation the operator never named and still report
    /// success.
    #[arg(long)]
    pub schema: String,

    /// The tenant whose run history to prune. It must be the tenant the mounted
    /// retention credential was minted for; a mismatch refuses loudly rather
    /// than pruning nothing and reporting success.
    #[arg(long)]
    pub tenant: String,

    /// Prune terminal runs whose `created_at` is older than this many days.
    #[arg(long)]
    pub retention_days: u32,

    /// Count what WOULD be pruned (a rolled-back delete under the same predicate)
    /// without deleting anything.
    #[arg(long)]
    pub dry_run: bool,
}

/// Prune the expired record history of one tenant and print what each history
/// table lost.
pub async fn prune_record_history(args: PruneRecordHistoryArgs) -> anyhow::Result<()> {
    let pruned = prune_expired_record_history(PruneRecordHistoryRequest {
        database_url: args.database_url,
        tenant: args.tenant.clone(),
    })
    .await?;
    print_pruned_record_history(&pruned, &args.tenant);
    Ok(())
}

fn print_pruned_record_history(pruned: &[PrunedHistory], tenant: &str) {
    for history in pruned {
        println!(
            "prune-record-history: removed {} entries from {}.{} (retention P{}D, tenant {})",
            history.removed, history.schema, history.history, history.days, tenant
        );
    }
    println!(
        "prune-record-history: pruned {} history table(s) for tenant {}",
        pruned.len(),
        tenant
    );
}

/// Prune one tenant's expired terminal run history and print the count.
pub async fn prune_run_history(args: PruneRunHistoryArgs) -> anyhow::Result<()> {
    let pruned = prune_terminal_run_history(PruneRunHistoryRequest {
        database_url: args.database_url,
        schema: args.schema.clone(),
        tenant: args.tenant.clone(),
        retention_days: args.retention_days,
        dry_run: args.dry_run,
    })
    .await?;

    if args.dry_run {
        println!(
            "prune-run-history (dry-run): {pruned} terminal run(s) older than {} day(s) WOULD be \
             pruned in schema {} (tenant {})",
            args.retention_days, args.schema, args.tenant
        );
    } else {
        println!(
            "prune-run-history: pruned {pruned} terminal run(s) older than {} day(s) in schema {} \
             (tenant {})",
            args.retention_days, args.schema, args.tenant
        );
    }
    Ok(())
}

/// Project-env dump arguments.
#[derive(Debug, Args)]
pub struct DumpProjectEnvArgs {
    /// Org id (must already be registered — `provision-org` / the pool).
    #[arg(long)]
    pub org: String,

    /// Project id: a lowercase slug `[a-z0-9-]` (start/end alphanumeric).
    #[arg(long)]
    pub project: String,

    /// Environment slug (any `registry.env_policies` name; default set `dev`/`prod`).
    #[arg(long)]
    pub env: String,

    /// Superuser Postgres URL to the T1 system DB (`wamn_system`): record
    /// `--run-now` dumps. Env `WAMN_SYSTEM_ADMIN_URL`.
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL")]
    pub system_database_url: Option<String>,

    /// The scheduled-dump cron (D18: the cadence is no longer a closed-tier knob —
    /// a per-env `dump_cadence` policy field is a future additive column).
    #[arg(long, default_value = DEFAULT_DUMP_SCHEDULE)]
    pub schedule: String,

    /// Object-store bucket dumps are written under.
    #[arg(long, default_value = DEFAULT_BUCKET)]
    pub bucket: String,

    /// Write the scheduled dump CronJob (JSON) here; `-` = stdout. Absent (with no
    /// other emit flag and no `--run-now`) ⇒ the CronJob is printed with a header.
    #[arg(long)]
    pub emit_cronjob: Option<PathBuf>,

    /// Write the one-shot dump Job (JSON) here; `-` = stdout. `kubectl create -f`
    /// it (it uses `generateName`) for an on-demand export.
    #[arg(long)]
    pub emit_job: Option<PathBuf>,

    /// Run a dump NOW: `pg_dump -Fd` of `--database-url` into `--out-dir`, then
    /// record it in the registry (needs `--system-database-url`). The on-demand
    /// export / .13 pre-move snapshot path.
    #[arg(long)]
    pub run_now: bool,

    /// The project-env database connection URL to dump (required by `--run-now`).
    #[arg(long)]
    pub database_url: Option<String>,

    /// Directory `--run-now` writes the dump into (a per-timestamp subdirectory).
    #[arg(long, default_value = "/tmp/wamn-dump")]
    pub out_dir: PathBuf,
}

/// Render one project-env's dump manifests, run the dump when asked, and print
/// the report.
pub async fn dump_project_env(args: DumpProjectEnvArgs) -> anyhow::Result<()> {
    let report = wamn_control::dump_project_env::dump_project_env(DumpProjectEnvRequest {
        org: args.org,
        project: args.project,
        env: args.env,
        system_database_url: args.system_database_url,
        schedule: args.schedule,
        bucket: args.bucket,
        run_now: args.run_now,
        database_url: args.database_url,
        out_dir: args.out_dir,
    })
    .await?;

    println!(
        "project-env {}: dump schedule {:?}, bucket {:?}",
        report.triple, report.schedule, report.bucket
    );

    let mut emitted = false;
    if args.emit_cronjob.is_some() {
        emit_json(
            &args.emit_cronjob,
            "dump CronJob (kubectl apply)",
            &report.cronjob,
        )?;
        emitted = true;
    }
    if args.emit_job.is_some() {
        emit_json(
            &args.emit_job,
            "one-shot dump Job (kubectl create)",
            &report.job,
        )?;
        emitted = true;
    }
    // Default action (no emit flag, no run-now): show the scheduled CronJob.
    if !emitted && !args.run_now {
        emit_json(&None, "dump CronJob (kubectl apply)", &report.cronjob)?;
    }

    if let Some(run) = &report.run {
        println!(
            "dumped {} -> {} ({} bytes); object key {}",
            report.triple,
            run.directory.display(),
            run.byte_size
                .map_or_else(|| "?".into(), |b| b.to_string()),
            run.object_key
        );
        if run.recorded {
            println!("recorded dump in the registry (provisioning.dumps)");
        } else {
            println!("(no --system-database-url: dump produced but not recorded)");
        }
    }
    Ok(())
}

/// Print a JSON document to a path, or to stdout with a labeled header when the
/// path is absent (`-` also means stdout) — the `provision-*` `emit_json` shape.
fn emit_json(path: &Option<PathBuf>, label: &str, doc: &serde_json::Value) -> anyhow::Result<()> {
    let text = serde_json::to_string_pretty(doc)?;
    match path {
        Some(p) if p.as_os_str() != "-" => {
            std::fs::write(p, &text).with_context(|| format!("write {}", p.display()))?;
            println!("wrote {} ({label})", p.display());
        }
        _ => println!("--- {label} ---\n{text}"),
    }
    Ok(())
}

/// Project-env restore arguments.
#[derive(Debug, Args)]
pub struct RestoreProjectEnvArgs {
    /// Org id (must already be registered — `provision-org` / the pool).
    #[arg(long)]
    pub org: String,

    /// Project id: a lowercase slug `[a-z0-9-]` (start/end alphanumeric).
    #[arg(long)]
    pub project: String,

    /// Environment slug (any `registry.env_policies` name; default set `dev`/`prod`).
    #[arg(long)]
    pub env: String,

    /// Superuser Postgres URL to the T1 system DB (`wamn_system`): read the dump
    /// catalog (`provisioning.dumps`) to pick which dump to restore. Env
    /// `WAMN_SYSTEM_ADMIN_URL`. Not needed when `--dump-dir` is given.
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL")]
    pub system_database_url: Option<String>,

    /// Superuser Postgres URL to the TARGET cluster (a maintenance DB, e.g.
    /// `.../postgres`): create the scratch database + connect to run `pg_restore`.
    /// Required to perform a restore.
    #[arg(long)]
    pub database_url: Option<String>,

    /// Explicit local `pg_dump -Fd` directory to restore from. When given, the
    /// catalog is not read (this exact artifact is restored).
    #[arg(long)]
    pub dump_dir: Option<PathBuf>,

    /// Local root the dumps are staged under (the object-store mirror until the
    /// restore-side fetch is wired). When `--dump-dir` is absent, the dump directory is
    /// `<dump-root>/<timestamp>` for the catalog-selected dump.
    #[arg(long, default_value = "/tmp/wamn-dump")]
    pub dump_root: PathBuf,

    /// Restore a SPECIFIC recorded dump by its object key (from the catalog).
    /// When omitted, the latest recorded dump is restored (restore-to-last-dump).
    #[arg(long)]
    pub object_key: Option<String>,

    /// Override the scratch-restore database name. Default:
    /// `wamn-restore-<org>--<project>--<env>`.
    #[arg(long)]
    pub scratch_db: Option<String>,

    /// Restore IN PLACE over the LIVE project-env database (destructive:
    /// `pg_restore --clean` drops and replaces the current data). Requires
    /// `--confirm`. Default is a non-destructive scratch restore.
    #[arg(long)]
    pub in_place: bool,

    /// Confirm a destructive `--in-place` restore. Without it, `--in-place` refuses
    /// to run (it would drop and replace live data).
    #[arg(long)]
    pub confirm: bool,
}

/// Restore one project-env dump and print which dump it chose and what it wrote.
pub async fn restore_project_env(args: RestoreProjectEnvArgs) -> anyhow::Result<()> {
    let admin_url = args.database_url.clone();
    let report =
        wamn_control::restore_project_env::restore_project_env(RestoreProjectEnvRequest {
            org: args.org,
            project: args.project,
            env: args.env,
            system_database_url: args.system_database_url,
            database_url: args.database_url,
            dump_dir: args.dump_dir,
            dump_root: args.dump_root,
            object_key: args.object_key,
            scratch_db: args.scratch_db,
            in_place: args.in_place,
            confirm: args.confirm,
        })
        .await?;

    match &report.selection {
        Some(DumpSelection::Catalog) => println!(
            "restore-to-last-dump: newest dump {} (from the provisioning.dumps catalog)",
            report.object_key.as_deref().unwrap_or_default()
        ),
        Some(DumpSelection::StagedPrefix(prefix)) => println!(
            "restore-to-last-dump: newest dump {} (found by listing the dump prefix {prefix:?} \
             staged under --dump-root — NOT in the provisioning.dumps catalog, e.g. a scheduled \
             CronJob dump)",
            report.object_key.as_deref().unwrap_or_default()
        ),
        None => {}
    }
    match &report.object_key {
        Some(key) => println!(
            "restored {} from dump {key} ({})",
            report.triple,
            report.dump_dir.display()
        ),
        None => println!(
            "restored {} from {}",
            report.triple,
            report.dump_dir.display()
        ),
    }
    match &report.outcome {
        RestoreOutcome::Scratch { database } => println!(
            "restored into scratch database {database:?} (non-destructive). Inspect it, then \
             drop:\n  psql {:?} -c 'DROP DATABASE IF EXISTS \"{database}\" WITH (FORCE)'",
            admin_url.as_deref().unwrap_or_default()
        ),
        RestoreOutcome::InPlace { database } => println!(
            "restored {} in place over the live database {database:?} (--clean)",
            report.triple
        ),
    }
    Ok(())
}

/// Project-env data copy arguments.
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

/// Print the copy plan, then run it unless `--plan` asked for the plan alone.
pub async fn copy_project_env(args: CopyProjectEnvArgs) -> anyhow::Result<()> {
    let plan_only = args.plan;
    let cutover = args.cutover;
    let request = CopyProjectEnvRequest {
        src_org: args.src_org,
        src_project: args.src_project,
        src_env: args.src_env,
        dst_org: args.dst_org,
        dst_project: args.dst_project,
        dst_env: args.dst_env,
        cutover: args.cutover,
        deprovision_old: args.deprovision_old,
        confirm: args.confirm,
        src_admin_url: args.src_admin_url,
        dst_admin_url: args.dst_admin_url,
        system_database_url: args.system_database_url,
        data_schema: args.data_schema,
        dump_root: args.dump_root,
        saga_id: args.saga_id,
    };

    let steps = plan_project_env_copy(&request)?;
    let src = Triple::new(
        &request.src_org,
        &request.src_project,
        request.src_env.as_str(),
    );
    let dst = Triple::new(
        &request.dst_org,
        &request.dst_project,
        request.dst_env.as_str(),
    );
    println!(
        "data copy {src} -> {dst} ({}):",
        if cutover { "MOVE with cutover" } else { "clone" }
    );
    for (i, step) in steps.iter().enumerate() {
        println!("  {}. {}", i + 1, step.label());
    }
    if plan_only {
        return Ok(());
    }

    let report = wamn_control::copy_project_env::copy_project_env(request).await?;
    println!(
        "copy {} -> {} complete ({} step(s); saga {} completed)",
        report.src, report.dst, report.steps, report.saga_id
    );
    Ok(())
}
