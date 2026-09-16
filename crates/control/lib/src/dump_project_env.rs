//! The `dump-project-env` subcommand (wamn-q3n.10): the per-project-env logical
//! **dump** producer — the second backup mechanism in the four-tier topology
//! (docs/archive/platform/postgres-topology.md §Backup architecture).
//!
//! `pg_dump -Fd` of one project-env database → object storage. **One artifact**
//! serves tenant-scoped restore-to-last-dump *and* the 10.3 project export; the
//! RPO is the dump interval (`--schedule`, default
//! [`wamn_control_provision::DEFAULT_DUMP_SCHEDULE`] — under D18 the cadence is no longer a
//! closed-tier knob). Two surfaces:
//!
//! * a scheduled **CronJob** (`--emit-cronjob`) at the `--schedule` cadence, and a
//!   **one-shot Job** (`--emit-job`) for on-demand exports — rendered here, applied
//!   by the runbook (no K8s client, the `provision-*` precedent);
//! * an imperative **`--run-now`** dump (against `--database-url`) — the on-demand
//!   export / .13 pre-move snapshot path — which runs `pg_dump -Fd` and records the
//!   dump in the T1 registry (`provisioning.dumps`).
//!
//! The dump connects via the project-env credential Secret (its `url`), so the
//! target cluster is not named here. The **object-store upload** is rendered into
//! the CronJob/Job and runs live against the shared store, guarded on the S3
//! endpoint env — the `pg_dump -Fd` artifact is complete regardless (Q2).
//!
//! **Scope (wamn-q3n.10):** producing the dump + its schedule + the metadata
//! record. The operator-facing RESTORE runbook + the audit-rewind caveat +
//! backup/restore gates are wamn-q3n.11; the tier-move cutover that consumes a
//! dump is wamn-q3n.13; WAL/PITR is wamn-e1g.

use std::path::PathBuf;
use std::process::Command as Proc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use tokio_postgres::NoTls;

use wamn_control_provision::{
    dump_object_key, pg_dump_argv, render_project_env_dump_cronjob, render_project_env_dump_job,
    validate_dump_resource_name, validate_project_env,
};
use wamn_control_registry::Triple;

/// Inputs of one project-env dump rendering and, when `run_now`, one dump.
#[derive(Debug)]
pub struct DumpProjectEnvRequest {
    /// Org id (must already be registered — `provision-org` / the pool).
    pub org: String,

    /// Project id: a lowercase slug `[a-z0-9-]` (start/end alphanumeric).
    pub project: String,

    /// Environment slug (any `registry.env_policies` name; default set `dev`/`prod`).
    pub env: String,

    /// Superuser Postgres URL to the T1 system DB (`wamn_system`): record
    /// `run_now` dumps. Absent leaves the dump unrecorded.
    pub system_database_url: Option<String>,

    /// The scheduled-dump cron (D18: the cadence is no longer a closed-tier knob —
    /// a per-env `dump_cadence` policy field is a future additive column).
    pub schedule: String,

    /// Object-store bucket dumps are written under.
    pub bucket: String,

    /// Run a dump NOW: `pg_dump -Fd` of `database_url` into `out_dir`, then
    /// record it in the registry. The on-demand export / .13 pre-move snapshot
    /// path.
    pub run_now: bool,

    /// The project-env database connection URL to dump (required by `run_now`).
    pub database_url: Option<String>,

    /// Directory `run_now` writes the dump into (a per-timestamp subdirectory).
    pub out_dir: PathBuf,
}

/// What one dump-project-env call rendered and, when asked, produced.
#[derive(Debug)]
pub struct DumpProjectEnvReport {
    /// The project-env the dump belongs to.
    pub triple: Triple,
    /// The scheduled-dump cron the CronJob carries.
    pub schedule: String,
    /// The object-store bucket both manifests write under.
    pub bucket: String,
    /// The rendered scheduled dump CronJob.
    pub cronjob: serde_json::Value,
    /// The rendered one-shot dump Job.
    pub job: serde_json::Value,
    /// The dump this call ran, when `run_now` asked for one.
    pub run: Option<DumpRun>,
}

/// The dump one `run_now` request produced.
#[derive(Debug)]
pub struct DumpRun {
    /// The per-timestamp directory holding the `-Fd` artifact.
    pub directory: PathBuf,
    /// The object key the dump belongs under in the shared store.
    pub object_key: String,
    /// The dump's on-disk size, when it could be measured.
    pub byte_size: Option<i64>,
    /// Whether the dump reached the `provisioning.dumps` catalog.
    pub recorded: bool,
}

/// Render the dump manifests of one project-env and, when the request asks for
/// it, run `pg_dump -Fd` now and record the dump.
pub async fn dump_project_env(
    request: DumpProjectEnvRequest,
) -> anyhow::Result<DumpProjectEnvReport> {
    let triple = Triple::new(&request.org, &request.project, request.env.as_str());

    // Name sanity: the db/Secret name (its length) and the CronJob resource name.
    validate_project_env(&request.org, &request.project, &request.env)
        .map_err(|e| anyhow::anyhow!("project-env names: {e}"))?;
    validate_dump_resource_name(&triple).map_err(|e| anyhow::anyhow!("dump resource name: {e}"))?;

    let cronjob = render_project_env_dump_cronjob(&triple, &request.schedule, &request.bucket);
    let job = render_project_env_dump_job(&triple, &request.bucket);

    let run = if request.run_now {
        Some(run_now(&request, &triple).await?)
    } else {
        None
    };

    Ok(DumpProjectEnvReport {
        triple,
        schedule: request.schedule,
        bucket: request.bucket,
        cronjob,
        job,
        run,
    })
}

/// Run `pg_dump -Fd` now and record the dump in the registry.
async fn run_now(request: &DumpProjectEnvRequest, triple: &Triple) -> anyhow::Result<DumpRun> {
    let db_url = request
        .database_url
        .as_deref()
        .context("a dump run needs the project-env database URL to dump")?;

    let timestamp = unix_seconds().to_string();
    let object_key = dump_object_key(triple, &timestamp);
    let out = request.out_dir.join(&timestamp);
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
    let recorded = match &request.system_database_url {
        Some(url) => {
            record_dump(url, triple, &object_key, byte_size).await?;
            true
        }
        None => false,
    };
    Ok(DumpRun {
        directory: out,
        object_key,
        byte_size,
        recorded,
    })
}

/// Record a completed dump in operations state (idempotent — refreshes
/// `byte_size` on a re-record).
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
    crate::ops_schema::install_and_enter(client).await?;
    let env = triple.env.as_str();
    let format = wamn_control_provision::dump::DUMP_FORMAT;
    client
        .execute(
            wamn_control_provision::state::record_dump_sql(),
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
pub(crate) fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Total byte size of a directory tree (the dump's on-disk size).
pub(crate) fn dir_size(path: &std::path::Path) -> std::io::Result<u64> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_schedule_is_the_provision_default() {
        // The dump cadence is the fixed D18 default (no closed-tier knob).
        assert_eq!(wamn_control_provision::DEFAULT_DUMP_SCHEDULE, "0 3 * * *");
    }

    #[test]
    fn a_dump_records_the_env_slug_verbatim() {
        // The dump keys off the env slug (open D18 env), not a closed enum.
        let t = Triple::new("acme", "billing", "staging");
        assert_eq!(dump_object_key(&t, "1"), "dumps/acme/billing/staging/1");
    }
}
