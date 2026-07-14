//! Rendering per-project-env logical **dumps** (wamn-q3n.10 / plan 10.3).
//!
//! The second backup mechanism in the four-tier topology
//! (docs/postgres-topology.md §Backup architecture): a scheduled `pg_dump -Fd`
//! of one project-env database to object storage. **One artifact serves both**
//! tenant-scoped restore-to-last-dump *and* the 10.3 project export — the RPO for
//! dump-based restore is the dump interval, and the interval is a **tier knob**
//! ([`dump_schedule`]). WAL/PITR (whole-cluster disaster recovery) is the
//! *other* mechanism — wamn-e1g's Barman Cloud — not this.
//!
//! This module is **pure** (SR3 / house rule 1): pure builders (the `pg_dump`
//! argv, the object-store key naming, the tier→schedule map, the upload argv) and
//! K8s manifest renderers (`serde_json::Value` — `kubectl apply -f` accepts JSON,
//! the [`crate::database`] / [`crate::org`] precedent). No DB, no clock, no K8s
//! client, no `pg_dump` invocation — the effects (running the dump, recording the
//! [`provisioning.dumps`](crate::sql) row) live in the `dump-project-env`
//! subcommand (`wamn-host`) and the CronJob container at runtime.
//!
//! **Object store (wamn-q3n.10 scope call, Q2):** the upload command is *rendered*
//! (its argv shape + object-key naming are unit-asserted) but the **live** S3
//! upload is deferred to when the shared object store lands — the rendered CronJob
//! runs `pg_dump` unconditionally and uploads only when the object-store CLI is
//! present, so no store exists yet is not a runtime failure. The shared MinIO/S3
//! is introduced with wamn-e1g (whose Barman WAL/PITR needs the same store); the
//! `.10` round-trip gate proves the artifact is valid + restorable
//! substrate-agnostically (`pg_dump -Fd` → `pg_restore` into a scratch DB).

use serde_json::{Value, json};
use wamn_registry::{Tier, Triple};

use crate::name::project_env_secret_name;

/// The container image the dump runs in — it ships `pg_dump`/`pg_restore`
/// (matches the org/pool/T1 cluster Postgres image). The object-store upload CLI
/// (`aws`/`mc`) is bundled when the shared store lands (wamn-e1g); until then the
/// rendered upload step is a no-op (guarded on the CLI being present).
const DUMP_IMAGE: &str = "ghcr.io/cloudnative-pg/postgresql:18";
/// The namespace dump CronJobs/Jobs live in (alongside the clusters + Secrets).
const NAMESPACE: &str = "wamn-system";
/// The default object-store bucket dumps are written under.
pub const DEFAULT_BUCKET: &str = "wamn-dumps";
/// The `pg_dump` format the artifact uses: directory (`-Fd`). Load-bearing —
/// directory format enables parallel and **selective** restore (the .11 carve-out
/// and .13 tier-move), and is the one artifact 10.3 export reuses.
pub const DUMP_FORMAT: &str = "directory";
/// Max length (bytes) of a dump CronJob / Job resource name. A CronJob appends a
/// `-<timestamp>` (≈11 chars) to the Jobs it creates, which must stay within the
/// 63-byte Job-name limit — so the CronJob name is bounded tighter than a plain
/// resource (the pattern behind [`crate::name::validate_project_env`]).
pub const MAX_DUMP_RESOURCE_NAME_LEN: usize = 52;

/// The dump CronJob / Job resource name for a project-env: `wamn-dump-<org>--
/// <project>--<env>`. Under the platform-reserved `wamn` prefix (wamn-66x); the
/// `--` separator matches the db/Secret naming ([`crate::name`]). Validate its
/// length with [`validate_dump_resource_name`] before rendering a CronJob.
pub fn dump_resource_name(triple: &Triple) -> String {
    format!(
        "wamn-dump-{}--{}--{}",
        triple.org,
        triple.project,
        triple.env.as_str()
    )
}

/// The object-store KEY **prefix** for a project-env's dumps:
/// `dumps/<org>/<project>/<env>`. A `-Fd` dump is a directory, so a single dump
/// lands under `<prefix>/<timestamp>/`. Derivable from the triple, so restore
/// tooling (.11) can list/find dumps without a registry read — the
/// `provisioning.dumps` row is bookkeeping, not the source of the key.
pub fn dump_key_prefix(triple: &Triple) -> String {
    format!(
        "dumps/{}/{}/{}",
        triple.org,
        triple.project,
        triple.env.as_str()
    )
}

/// The object-store KEY for one dump: `dumps/<org>/<project>/<env>/<timestamp>`.
/// The timestamp is **caller-supplied** — the clock lives in the driver (SR6 rule
/// 1: this builder is pure). The `dump-project-env --run-now` path passes the
/// dump's start time; the CronJob computes it in-container (`date`).
pub fn dump_object_key(triple: &Triple, timestamp: &str) -> String {
    format!("{}/{}", dump_key_prefix(triple), timestamp)
}

/// The scheduled-dump cadence for a tier, as a cron expression. **Frequency is a
/// tier knob** (topology §Backup architecture) — the RPO for dump-based restore is
/// this interval, stated in contracts:
///
/// * `trials` → **daily** (03:00) — loosest RPO, pre-contract;
/// * `standard` → **every 6 hours**;
/// * `dedicated` → **hourly** — the regulated tier, tightest RPO.
pub fn dump_schedule(tier: Tier) -> &'static str {
    match tier {
        Tier::Trials => "0 3 * * *",
        Tier::Standard => "0 */6 * * *",
        Tier::Dedicated => "0 * * * *",
    }
}

/// The `pg_dump` argv for a project-env database. `-Fd` (**directory format**) is
/// load-bearing — it produces the one artifact that serves restore-to-last-dump,
/// the 10.3 export, and the .13 tier-move. `conninfo` is a full connection URL or
/// a bare db name (`pg_dump -d` accepts either), so the same argv serves the gate
/// (a URL) and the CronJob (a URL from the credential Secret). `--no-password`
/// fails rather than prompting — credentials come from the URL. Ownership/ACL are
/// **kept** (a complete dump); the *restore* side (.11) decides `--no-owner`.
pub fn pg_dump_argv(conninfo: &str, out_dir: &str) -> Vec<String> {
    vec![
        "pg_dump".into(),
        "-Fd".into(),
        "--no-password".into(),
        "-f".into(),
        out_dir.into(),
        "-d".into(),
        conninfo.into(),
    ]
}

/// The object-store upload argv (**rendered** into the CronJob/Job; the live
/// upload is deferred to when the shared store lands — wamn-e1g, Q2). A `-Fd` dump
/// is a directory → a recursive copy under the dump's object key.
pub fn upload_argv(local_dir: &str, bucket: &str, object_key: &str) -> Vec<String> {
    vec![
        "aws".into(),
        "s3".into(),
        "cp".into(),
        "--recursive".into(),
        local_dir.into(),
        format!("s3://{bucket}/{object_key}/"),
    ]
}

/// Labels stamped on a dump CronJob/Job — platform ownership + the identity
/// triple (so tooling never parses the name), the [`crate::database`] precedent.
fn dump_labels(triple: &Triple) -> Value {
    json!({
        "app.kubernetes.io/managed-by": "wamn",
        "app.kubernetes.io/component": "project-env-dump",
        "wamn.org": triple.org,
        "wamn.project": triple.project,
        "wamn.env": triple.env.as_str(),
    })
}

/// The container command a dump pod runs: `pg_dump -Fd` of the project-env
/// database (connection from the `DATABASE_URL` env, sourced from the credential
/// Secret's `url` key) into an ephemeral volume, then the object-store upload —
/// **guarded** on the upload CLI being present, so the dump succeeds whether or
/// not the shared store is wired yet (Q2). The timestamp is computed in-container
/// (`date -u +%s`, matching [`dump_object_key`]'s caller-supplied form).
fn dump_command(triple: &Triple, bucket: &str) -> String {
    let prefix = dump_key_prefix(triple);
    // pg_dump into /dump/<ts>; upload that directory under <prefix>/<ts>/.
    format!(
        "set -eu\n\
         TS=\"$(date -u +%s)\"\n\
         OUT=\"/dump/$TS\"\n\
         rm -rf \"$OUT\"\n\
         pg_dump -Fd --no-password -f \"$OUT\" -d \"$DATABASE_URL\"\n\
         # object store upload (wamn-q3n.10 Q2): rendered now, live when the shared\n\
         # store lands (wamn-e1g). The pg_dump artifact is complete regardless.\n\
         if command -v aws >/dev/null 2>&1; then\n\
         \x20 aws s3 cp --recursive \"$OUT\" \"s3://{bucket}/{prefix}/$TS/\"\n\
         else echo \"object store not configured (wamn-e1g); dump kept at $OUT\"; fi\n"
    )
}

/// The shared pod `spec` a dump CronJob's jobTemplate and a one-shot dump Job both
/// wrap: a single `postgres:18` container running [`dump_command`], the
/// project-env credential Secret's `url` mounted as `DATABASE_URL`, and an
/// ephemeral `/dump` scratch volume. `restartPolicy: OnFailure` (a transient dump
/// failure retries; the CronJob/Job caps overall retries).
fn dump_pod_spec(triple: &Triple, bucket: &str) -> Value {
    let secret = project_env_secret_name(&triple.org, &triple.project, triple.env);
    json!({
        "spec": {
            "restartPolicy": "OnFailure",
            "containers": [{
                "name": "dump",
                "image": DUMP_IMAGE,
                "command": ["/bin/sh", "-c", dump_command(triple, bucket)],
                "env": [{
                    "name": "DATABASE_URL",
                    "valueFrom": { "secretKeyRef": { "name": secret, "key": "url" } }
                }],
                "volumeMounts": [{ "name": "dump", "mountPath": "/dump" }],
            }],
            "volumes": [{ "name": "dump", "emptyDir": {} }],
        }
    })
}

/// Render the scheduled-dump **CronJob** for a project-env. `schedule` is the
/// tier cadence ([`dump_schedule`]); `bucket` is the object-store bucket. The
/// dump connects via the project-env credential Secret, so the target cluster is
/// not named here (it is embedded in the Secret's URL host). `concurrencyPolicy:
/// Forbid` — dumps never overlap.
///
/// Validate the resource name with [`validate_dump_resource_name`] first (a
/// CronJob name is length-bounded — [`MAX_DUMP_RESOURCE_NAME_LEN`]).
pub fn render_project_env_dump_cronjob(triple: &Triple, schedule: &str, bucket: &str) -> Value {
    json!({
        "apiVersion": "batch/v1",
        "kind": "CronJob",
        "metadata": {
            "name": dump_resource_name(triple),
            "namespace": NAMESPACE,
            "labels": dump_labels(triple),
        },
        "spec": {
            "schedule": schedule,
            "concurrencyPolicy": "Forbid",
            "successfulJobsHistoryLimit": 3,
            "failedJobsHistoryLimit": 3,
            "jobTemplate": { "spec": { "template": dump_pod_spec(triple, bucket) } },
        },
    })
}

/// Render a **one-shot** dump Job for a project-env — the on-demand path the 10.3
/// project export and the .13 pre-move snapshot use (`dump-project-env
/// --emit-job`). Uses `generateName` (the operator `kubectl create`s it) so a
/// re-run never collides on a name, without needing a clock. `backoffLimit: 2`.
pub fn render_project_env_dump_job(triple: &Triple, bucket: &str) -> Value {
    json!({
        "apiVersion": "batch/v1",
        "kind": "Job",
        "metadata": {
            "generateName": format!("{}-", dump_resource_name(triple)),
            "namespace": NAMESPACE,
            "labels": dump_labels(triple),
        },
        "spec": {
            "backoffLimit": 2,
            "template": dump_pod_spec(triple, bucket),
        },
    })
}

/// Validate that a project-env's dump CronJob/Job resource name fits
/// [`MAX_DUMP_RESOURCE_NAME_LEN`]. Errors with
/// [`ProvisionError::NameTooLong`](crate::ProvisionError::NameTooLong) for a
/// pathologically long triple (the [`crate::name::validate_project_env`] pattern).
pub fn validate_dump_resource_name(triple: &Triple) -> Result<(), crate::ProvisionError> {
    let name = dump_resource_name(triple);
    if name.len() > MAX_DUMP_RESOURCE_NAME_LEN {
        return Err(crate::ProvisionError::NameTooLong {
            name,
            max: MAX_DUMP_RESOURCE_NAME_LEN,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wamn_registry::Env;

    fn t() -> Triple {
        Triple::new("acme", "billing", Env::Dev)
    }

    #[test]
    fn pg_dump_uses_directory_format() {
        // -Fd is the load-bearing flag (the artifact 10.3 export + .11/.13 reuse).
        let argv = pg_dump_argv("postgres://u@h/db", "/dump/out");
        assert_eq!(argv[0], "pg_dump");
        assert!(
            argv.iter().any(|a| a == "-Fd"),
            "must dump directory format"
        );
        // The connection + output dir are passed as separate argv (no shell splice).
        assert!(argv.windows(2).any(|w| w == ["-f", "/dump/out"]));
        assert!(argv.windows(2).any(|w| w == ["-d", "postgres://u@h/db"]));
        // Never prompt for a password.
        assert!(argv.iter().any(|a| a == "--no-password"));
    }

    #[test]
    fn object_key_has_the_stable_derivable_shape() {
        // dumps/<org>/<project>/<env>/<timestamp> — derivable, so restore (.11)
        // needs no registry read to find a dump.
        assert_eq!(dump_key_prefix(&t()), "dumps/acme/billing/dev");
        assert_eq!(
            dump_object_key(&t(), "1720000000"),
            "dumps/acme/billing/dev/1720000000"
        );
        // The prod and dev envs of one project never share a key prefix.
        let prod = Triple::new("acme", "billing", Env::Prod);
        assert_ne!(dump_key_prefix(&t()), dump_key_prefix(&prod));
    }

    #[test]
    fn schedule_is_a_tier_knob() {
        // Frequency rises with the tier (tightest RPO on the regulated tier).
        assert_eq!(dump_schedule(Tier::Trials), "0 3 * * *"); // daily
        assert_eq!(dump_schedule(Tier::Standard), "0 */6 * * *"); // every 6h
        assert_eq!(dump_schedule(Tier::Dedicated), "0 * * * *"); // hourly
        // The three tiers map to three distinct cadences.
        let all: Vec<_> = Tier::ALL.iter().map(|&x| dump_schedule(x)).collect();
        let uniq: std::collections::HashSet<_> = all.iter().collect();
        assert_eq!(uniq.len(), 3, "each tier has its own cadence");
    }

    #[test]
    fn upload_argv_targets_the_object_key_recursively() {
        let argv = upload_argv("/dump/out", "wamn-dumps", "dumps/acme/billing/dev/123");
        assert_eq!(argv[..4], ["aws", "s3", "cp", "--recursive"]);
        assert_eq!(argv[4], "/dump/out");
        // A -Fd dump is a directory → uploaded under the object-key prefix.
        assert_eq!(argv[5], "s3://wamn-dumps/dumps/acme/billing/dev/123/");
    }

    #[test]
    fn cronjob_schedules_a_guarded_pg_dump_of_the_project_env() {
        let cr = render_project_env_dump_cronjob(&t(), dump_schedule(Tier::Trials), DEFAULT_BUCKET);
        assert_eq!(cr["apiVersion"], "batch/v1");
        assert_eq!(cr["kind"], "CronJob");
        assert_eq!(cr["metadata"]["name"], "wamn-dump-acme--billing--dev");
        assert_eq!(cr["metadata"]["namespace"], "wamn-system");
        assert_eq!(cr["metadata"]["labels"]["wamn.env"], "dev");
        // The schedule is the tier cadence; dumps never overlap.
        assert_eq!(cr["spec"]["schedule"], "0 3 * * *");
        assert_eq!(cr["spec"]["concurrencyPolicy"], "Forbid");
        // The container runs `pg_dump -Fd` and the (guarded) object-store upload.
        let cmd = cr["spec"]["jobTemplate"]["spec"]["template"]["spec"]["containers"][0]["command"]
            [2]
        .as_str()
        .unwrap();
        assert!(cmd.contains("pg_dump -Fd"), "runs a directory-format dump");
        assert!(
            cmd.contains("aws s3 cp --recursive"),
            "renders the object-store upload"
        );
        assert!(
            cmd.contains("s3://wamn-dumps/dumps/acme/billing/dev/"),
            "uploads under the derivable object key"
        );
        // The connection comes from the project-env credential Secret's `url` key.
        let env = &cr["spec"]["jobTemplate"]["spec"]["template"]["spec"]["containers"][0]["env"][0];
        assert_eq!(env["name"], "DATABASE_URL");
        assert_eq!(
            env["valueFrom"]["secretKeyRef"]["name"],
            "wamn-db-acme--billing--dev"
        );
        assert_eq!(env["valueFrom"]["secretKeyRef"]["key"], "url");
    }

    #[test]
    fn one_shot_job_uses_generate_name_and_the_same_pod() {
        let job = render_project_env_dump_job(&t(), DEFAULT_BUCKET);
        assert_eq!(job["kind"], "Job");
        // generateName (not name) so a re-run never collides — no clock needed.
        assert_eq!(
            job["metadata"]["generateName"],
            "wamn-dump-acme--billing--dev-"
        );
        assert!(job["metadata"]["name"].is_null());
        // Same dump container as the scheduled path.
        let cmd = job["spec"]["template"]["spec"]["containers"][0]["command"][2]
            .as_str()
            .unwrap();
        assert!(cmd.contains("pg_dump -Fd"));
    }

    #[test]
    fn dump_resource_name_length_is_bounded() {
        assert!(validate_dump_resource_name(&t()).is_ok());
        // A pathologically long triple overflows the CronJob-name bound.
        let long = Triple::new("o".repeat(30), "p".repeat(30), Env::Prod);
        assert!(matches!(
            validate_dump_resource_name(&long),
            Err(crate::ProvisionError::NameTooLong { max: 52, .. })
        ));
    }
}
