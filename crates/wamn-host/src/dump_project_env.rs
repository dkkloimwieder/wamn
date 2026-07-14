//! The `dump-project-env` subcommand (wamn-q3n.10): the per-project-env logical
//! **dump** producer — the second backup mechanism in the four-tier topology
//! (docs/postgres-topology.md §Backup architecture).
//!
//! `pg_dump -Fd` of one project-env database → object storage. **One artifact**
//! serves tenant-scoped restore-to-last-dump *and* the 10.3 project export; the
//! RPO is the dump interval, and the interval is a **tier knob**
//! ([`wamn_provision::dump_schedule`]). Two surfaces:
//!
//! * a scheduled **CronJob** (`--emit-cronjob`) at the tier cadence, and a
//!   **one-shot Job** (`--emit-job`) for on-demand exports — rendered here, applied
//!   by the runbook (no K8s client, the `provision-*` precedent);
//! * an imperative **`--run-now`** dump (against `--database-url`) — the on-demand
//!   export / .13 pre-move snapshot path — which runs `pg_dump -Fd` and records the
//!   dump in the T1 registry (`provisioning.dumps`).
//!
//! The dump connects via the project-env credential Secret (its `url`), so the
//! target cluster is not named here. The **object-store upload** is rendered into
//! the CronJob/Job but its live execution is deferred to when the shared store
//! lands (wamn-e1g) — the `pg_dump -Fd` artifact is complete regardless (Q2).
//!
//! **Scope (wamn-q3n.10):** producing the dump + its schedule + the metadata
//! record. The operator-facing RESTORE runbook + the audit-rewind caveat +
//! backup/restore gates are wamn-q3n.11; the tier-move cutover that consumes a
//! dump is wamn-q3n.13; WAL/PITR is wamn-e1g.

use std::path::PathBuf;
use std::process::Command as Proc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use clap::Args;
use tokio_postgres::NoTls;

use wamn_provision::{
    DEFAULT_BUCKET, dump_object_key, dump_schedule, pg_dump_argv, render_project_env_dump_cronjob,
    render_project_env_dump_job, validate_dump_resource_name, validate_project_env,
};
use wamn_registry::{Env, Tier, Triple};

use crate::provision_org::TierArg;
use crate::provision_project_env::EnvArg;

#[derive(Debug, Args)]
pub struct DumpProjectEnvArgs {
    /// Org id (must already be registered — `provision-org` / the T3 pool).
    #[arg(long)]
    pub org: String,

    /// Project id: a lowercase slug `[a-z0-9-]` (start/end alphanumeric).
    #[arg(long)]
    pub project: String,

    /// Environment: `dev`, `canary`, or `prod`.
    #[arg(long, value_enum)]
    pub env: EnvArg,

    /// Superuser Postgres URL to the T1 system DB (`wamn_system`): read the org's
    /// tier (dump cadence) and record `--run-now` dumps. Env `WAMN_SYSTEM_ADMIN_URL`.
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL")]
    pub system_database_url: Option<String>,

    /// Override the org tier (the dump cadence). When omitted, it is read from the
    /// registry — which needs `--system-database-url`.
    #[arg(long, value_enum)]
    pub tier: Option<TierArg>,

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

pub async fn run(args: DumpProjectEnvArgs) -> anyhow::Result<()> {
    let env: Env = args.env.into();
    let triple = Triple::new(&args.org, &args.project, env);

    // Name sanity: the db/Secret name (its length) and the CronJob resource name.
    validate_project_env(&args.org, &args.project, env)
        .map_err(|e| anyhow::anyhow!("project-env names: {e}"))?;
    validate_dump_resource_name(&triple).map_err(|e| anyhow::anyhow!("dump resource name: {e}"))?;

    // The dump cadence follows the org tier: an explicit `--tier` wins; otherwise
    // read it from the registry (frequency is a tier knob).
    let tier: Tier = match args.tier {
        Some(t) => t.into(),
        None => {
            let url = args.system_database_url.as_deref().context(
                "pass --tier, or --system-database-url to read the org tier from the registry",
            )?;
            resolve_tier(url, &args.org).await?
        }
    };
    let schedule = dump_schedule(tier);

    let cronjob = render_project_env_dump_cronjob(&triple, schedule, &args.bucket);
    let job = render_project_env_dump_job(&triple, &args.bucket);

    println!(
        "project-env {triple}: dump schedule {schedule:?} (tier {}), bucket {:?}",
        tier.as_str(),
        args.bucket
    );

    let mut emitted = false;
    if args.emit_cronjob.is_some() {
        emit_json(&args.emit_cronjob, "dump CronJob (kubectl apply)", &cronjob)?;
        emitted = true;
    }
    if args.emit_job.is_some() {
        emit_json(&args.emit_job, "one-shot dump Job (kubectl create)", &job)?;
        emitted = true;
    }
    // Default action (no emit flag, no run-now): show the scheduled CronJob.
    if !emitted && !args.run_now {
        emit_json(&None, "dump CronJob (kubectl apply)", &cronjob)?;
    }

    if args.run_now {
        run_now(&args, &triple).await?;
    }

    Ok(())
}

/// Run `pg_dump -Fd` now and record the dump in the registry.
async fn run_now(args: &DumpProjectEnvArgs, triple: &Triple) -> anyhow::Result<()> {
    let db_url = args
        .database_url
        .as_deref()
        .context("--run-now needs --database-url (the project-env database to dump)")?;

    let timestamp = unix_seconds().to_string();
    let object_key = dump_object_key(triple, &timestamp);
    let out = args.out_dir.join(&timestamp);
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    // pg_dump refuses a pre-existing non-empty directory; start clean.
    let _ = std::fs::remove_dir_all(&out);

    let out_str = out.to_string_lossy().to_string();
    let argv = pg_dump_argv(db_url, &out_str);
    let status = Proc::new(&argv[0])
        .args(&argv[1..])
        .status()
        .with_context(|| format!("spawn {} (is pg_dump installed?)", argv[0]))?;
    anyhow::ensure!(status.success(), "pg_dump failed ({status})");

    let byte_size = dir_size(&out).map(|b| b as i64).ok();
    println!(
        "dumped {triple} -> {} ({} bytes); object key {object_key}",
        out.display(),
        byte_size.map_or_else(|| "?".into(), |b| b.to_string()),
    );

    match &args.system_database_url {
        Some(url) => {
            record_dump(url, triple, &object_key, byte_size).await?;
            println!("recorded dump in the registry (provisioning.dumps)");
        }
        None => println!("(no --system-database-url: dump produced but not recorded)"),
    }
    Ok(())
}

/// Read the org's tier from the registry (as the `wamn_system` owner).
async fn resolve_tier(system_url: &str, org: &str) -> anyhow::Result<Tier> {
    let (client, conn) = tokio_postgres::connect(system_url, NoTls)
        .await
        .context("system db connect")?;
    let conn_task = tokio::spawn(conn);
    let result = do_resolve_tier(&client, org).await;
    drop(client);
    let _ = conn_task.await;
    result
}

async fn do_resolve_tier(client: &tokio_postgres::Client, org: &str) -> anyhow::Result<Tier> {
    client
        .batch_execute("SET ROLE wamn_system")
        .await
        .context("SET ROLE wamn_system")?;
    let row = client
        .query_opt(wamn_registry::sql::select_org_tier_sql(), &[&org])
        .await
        .context("read org tier")?
        .with_context(|| format!("org {org:?} is not registered (run provision-org first)"))?;
    let tier: String = row.get("tier");
    tier_from_str(&tier).with_context(|| format!("unknown tier {tier:?} in registry"))
}

fn tier_from_str(s: &str) -> anyhow::Result<Tier> {
    Tier::ALL
        .into_iter()
        .find(|t| t.as_str() == s)
        .ok_or_else(|| anyhow::anyhow!("not a tier: {s:?}"))
}

/// Record a completed dump in the registry (idempotent — refreshes byte_size on a
/// re-record). Connects as superuser and `SET ROLE wamn_system` (the .3 pattern).
async fn record_dump(
    system_url: &str,
    triple: &Triple,
    object_key: &str,
    byte_size: Option<i64>,
) -> anyhow::Result<()> {
    let (client, conn) = tokio_postgres::connect(system_url, NoTls)
        .await
        .context("system db connect")?;
    let conn_task = tokio::spawn(conn);
    let result = do_record_dump(&client, triple, object_key, byte_size).await;
    drop(client);
    let _ = conn_task.await;
    result
}

async fn do_record_dump(
    client: &tokio_postgres::Client,
    triple: &Triple,
    object_key: &str,
    byte_size: Option<i64>,
) -> anyhow::Result<()> {
    client
        .batch_execute("SET ROLE wamn_system")
        .await
        .context("SET ROLE wamn_system")?;
    let env = triple.env.as_str();
    let format = wamn_provision::dump::DUMP_FORMAT;
    client
        .execute(
            wamn_registry::sql::record_dump_sql(),
            &[
                &triple.org,
                &triple.project,
                &env,
                &object_key,
                &format,
                &byte_size,
            ],
        )
        .await
        .context("record dump in provisioning.dumps")?;
    Ok(())
}

/// Seconds since the Unix epoch (a monotonic-enough dump label). The clock lives
/// in this driver, never in the pure renderer/builder (SR6 rule 1).
fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Total byte size of a directory tree (the dump's on-disk size).
fn dir_size(path: &std::path::Path) -> std::io::Result<u64> {
    let mut total = 0;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        total += if meta.is_dir() {
            dir_size(&entry.path())?
        } else {
            meta.len()
        };
    }
    Ok(total)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_from_str_round_trips_the_registry_literals() {
        for t in Tier::ALL {
            assert_eq!(tier_from_str(t.as_str()).unwrap(), t);
        }
        assert!(tier_from_str("platinum").is_err());
    }

    #[test]
    fn env_arg_and_tier_arg_map_to_the_registry_types() {
        // The subcommand's clap enums map to the registry vocabulary the dump
        // cadence + object key key off.
        assert_eq!(Env::from(EnvArg::Prod), Env::Prod);
        assert_eq!(Tier::from(TierArg::Dedicated), Tier::Dedicated);
        // The cadence is derived from the tier (frequency is a tier knob).
        assert_eq!(
            dump_schedule(Tier::from(TierArg::Dedicated)),
            dump_schedule(Tier::Dedicated)
        );
    }
}
