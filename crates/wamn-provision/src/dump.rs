//! Rendering per-project-env logical **dumps** (wamn-q3n.10 / plan 10.3).
//!
//! The second backup mechanism in the four-tier topology
//! (docs/postgres-topology.md §Backup architecture): a scheduled `pg_dump -Fd`
//! of one project-env database to object storage. **One artifact serves both**
//! tenant-scoped restore-to-last-dump *and* the 10.3 project export — the RPO for
//! dump-based restore is the dump interval ([`DEFAULT_DUMP_SCHEDULE`]; a per-env
//! cadence knob is a future additive column, D18). WAL/PITR (whole-cluster
//! disaster recovery) is the *other* mechanism — wamn-e1g's Barman Cloud — not this.
//!
//! This module is **pure** (SR3 / house rule 1): pure builders (the `pg_dump`
//! argv, the object-store key naming, the default schedule, the upload argv) and
//! K8s manifest renderers (`serde_json::Value` — `kubectl apply -f` accepts JSON,
//! the [`crate::database`] / [`crate::org`] precedent). No DB, no clock, no K8s
//! client, no `pg_dump` invocation — the effects (running the dump, recording the
//! [`provisioning.dumps`](crate::sql) row) live in the `dump-project-env`
//! subcommand (`wamn-ctl`) and the CronJob container at runtime.
//!
//! **Object store (wamn-q3n.10 rendered the upload; wamn-e1g makes it live):** the
//! dump pod is now `initContainer`(`pg_dump -Fd` into a shared volume) +
//! `container`(the MinIO client `mc` uploads that directory to the shared MinIO —
//! [`crate::backup::MINIO_ENDPOINT`], credentials from
//! [`crate::backup::OBJECT_STORE_SECRET`]). The upload stays **guarded** on the S3
//! endpoint env, so no store configured is not a runtime failure (the `pg_dump`
//! init step still runs). The `.10` round-trip gate proves the artifact valid +
//! restorable substrate-agnostically (`pg_dump -Fd` → `pg_restore` into a scratch
//! DB) — that path uses the pure [`pg_dump_argv`] builder, unaffected by the pod
//! topology.

use serde_json::{Value, json};
use wamn_registry::Triple;

use crate::backup::{MINIO_ENDPOINT, OBJECT_STORE_SECRET};
use crate::name::project_env_secret_name;

/// The container image the `pg_dump` step runs in — it ships `pg_dump`/`pg_restore`
/// (matches the org/pool/T1 cluster Postgres image).
const DUMP_IMAGE: &str = "ghcr.io/cloudnative-pg/postgresql:18";
/// The image the object-store **upload** step runs in — the MinIO client (`mc`),
/// which speaks S3 to the shared MinIO ([`MINIO_ENDPOINT`]). Pinned. wamn-e1g
/// makes the upload live (wamn-q3n.10 rendered it, deferring the live upload to
/// when the shared store landed).
const MC_IMAGE: &str = "minio/mc:RELEASE.2025-08-13T08-35-41Z";
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

/// From a **listing** of object keys under a project-env's dump [`dump_key_prefix`],
/// pick the key of the LATEST dump — the one whose embedded timestamp (the first path
/// segment after the prefix, per the [`dump_object_key`] layout `<prefix>/<timestamp>`)
/// is numerically greatest. Returns the bare dump key `<prefix>/<timestamp>`, or `None`
/// for an empty / all-foreign listing.
///
/// This powers **restore-to-last-dump's fallback** (wamn-cjv.19): the scheduled dump
/// CronJob uploads to object storage but records NO `provisioning.dumps` catalog row
/// (it holds only the project-env DB URL + object-store creds, not the `wamn_system`
/// connection), so those dumps are invisible to a catalog-only read. Restore lists the
/// deterministic prefix directly and picks the newest from the store's own key layout —
/// no new credential surface. The caller may fold the catalog's own latest recorded key
/// into `keys`, so the newest across catalog + listing is chosen (the genuinely last
/// dump, not merely the last *recorded* one).
///
/// Robust against a real (recursive) store listing: keys that don't sit under
/// `prefix`/ (foreign orgs/projects/envs), or whose timestamp segment isn't a positive
/// integer (malformed / stray objects), are ignored — they never mask or outrank a real
/// dump. The timestamp is compared NUMERICALLY (`u64`), independently of listing order
/// (a lexical or listing-order max would pick the wrong dump). A `<prefix>/<ts>/toc.dat`
/// and a `<prefix>/<ts>/3.dat` collapse to the one dump `<prefix>/<ts>`.
///
/// **Completeness:** the `-Fd` layout carries no terminal completion marker (`pg_dump`
/// writes `toc.dat` + data files; `mc mirror` copies them in no guaranteed order), so
/// this picks the latest WELL-FORMED dump key per the layout — it does not redesign the
/// dump format. The caller applies whatever completeness signal it has (the local
/// mirror gates on `toc.dat` presence; restore's own `toc.dat` check backstops the
/// chosen directory). Torn-dump detection for a live recursive store listing wants a
/// real completion marker — a follow-up, not this fix.
pub fn select_latest_dump_key(prefix: &str, keys: &[String]) -> Option<String> {
    let want = format!("{prefix}/");
    keys.iter()
        .filter_map(|k| k.strip_prefix(&want))
        .filter_map(|rest| rest.split('/').next())
        .filter_map(|seg| seg.parse::<u64>().ok())
        .max()
        .map(|ts| format!("{prefix}/{ts}"))
}

/// The default scheduled-dump cadence, a 5-field cron: **daily** (03:00). Under
/// D18 the dump cadence is no longer a closed-tier knob — a per-env `dump_cadence`
/// policy field is a future additive column (`docs/deployment-model.md` §Region:
/// "the next placement axis is data"). Callers that want a per-env RPO pass their
/// own schedule to [`render_project_env_dump_cronjob`].
pub const DEFAULT_DUMP_SCHEDULE: &str = "0 3 * * *";

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

/// The **init**-container command: `pg_dump -Fd` of the project-env database
/// (connection from `DATABASE_URL`, sourced from the credential Secret's `url`
/// key) into `/dump/out`, recording the dump timestamp in `/dump/TS` so the
/// upload container writes to the same object key (`date -u +%s`, matching
/// [`dump_object_key`]'s caller-supplied form). Runs to completion before the
/// upload container starts.
fn dump_command() -> String {
    "set -eu\n\
     date -u +%s > /dump/TS\n\
     rm -rf /dump/out\n\
     pg_dump -Fd --no-password -f /dump/out -d \"$DATABASE_URL\"\n"
        .to_string()
}

/// The **upload**-container command: `mc` copies the `pg_dump` directory
/// (`/dump/out`) to the shared MinIO under the derivable object key
/// [`dump_object_key`] = `<prefix>/<TS>`. `mc mirror` puts the dump directory's
/// **contents** (`toc.dat`, the data files) directly under `<prefix>/<TS>/`, so
/// the object key matches what `.11` restore derives (a `cp --recursive` would
/// nest them one level deeper under `out/`). **Guarded** on the S3 endpoint env,
/// so no store configured is not a runtime failure (the init `pg_dump` still
/// ran). Credentials come from the [`OBJECT_STORE_SECRET`] env.
fn upload_command(triple: &Triple, bucket: &str) -> String {
    let prefix = dump_key_prefix(triple);
    format!(
        "set -eu\n\
         TS=\"$(cat /dump/TS)\"\n\
         if [ -n \"${{S3_ENDPOINT:-}}\" ]; then\n\
         \x20 mc alias set store \"$S3_ENDPOINT\" \"$ACCESS_KEY_ID\" \"$ACCESS_SECRET_KEY\"\n\
         \x20 mc mirror /dump/out \"store/{bucket}/{prefix}/$TS\"\n\
         else echo \"object store not configured; dump kept in the pod\"; fi\n"
    )
}

/// The shared pod `spec` a dump CronJob's jobTemplate and a one-shot dump Job both
/// wrap: an `initContainer` (`postgres:18` running [`dump_command`], the
/// project-env credential Secret's `url` mounted as `DATABASE_URL`) that dumps to
/// a shared `/dump` volume, then a `container` (`mc` running [`upload_command`],
/// the shared object-store credentials + endpoint) that uploads it to MinIO. The
/// init runs to completion first, so the dump exists before the upload. wamn-e1g
/// makes the upload live (wamn-q3n.10 rendered it). `restartPolicy: OnFailure`.
fn dump_pod_spec(triple: &Triple, bucket: &str) -> Value {
    let secret = project_env_secret_name(&triple.org, &triple.project, triple.env.as_str());
    json!({
        "spec": {
            "restartPolicy": "OnFailure",
            "initContainers": [{
                "name": "dump",
                "image": DUMP_IMAGE,
                "command": ["/bin/sh", "-c", dump_command()],
                "env": [{
                    "name": "DATABASE_URL",
                    "valueFrom": { "secretKeyRef": { "name": secret, "key": "url" } }
                }],
                "volumeMounts": [{ "name": "dump", "mountPath": "/dump" }],
            }],
            "containers": [{
                "name": "upload",
                "image": MC_IMAGE,
                "command": ["/bin/sh", "-c", upload_command(triple, bucket)],
                "env": [
                    { "name": "S3_ENDPOINT", "value": MINIO_ENDPOINT },
                    { "name": "ACCESS_KEY_ID", "valueFrom": {
                        "secretKeyRef": { "name": OBJECT_STORE_SECRET, "key": "ACCESS_KEY_ID" } } },
                    { "name": "ACCESS_SECRET_KEY", "valueFrom": {
                        "secretKeyRef": { "name": OBJECT_STORE_SECRET, "key": "ACCESS_SECRET_KEY" } } },
                ],
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

    fn t() -> Triple {
        Triple::new("acme", "billing", "dev")
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
        let prod = Triple::new("acme", "billing", "prod");
        assert_ne!(dump_key_prefix(&t()), dump_key_prefix(&prod));
    }

    #[test]
    fn select_latest_dump_key_returns_the_latest_of_many() {
        // The fallback (wamn-cjv.19): pick the newest dump from a prefix listing when
        // the catalog has no row. Disabling the fallback (always `None`) fails here.
        let prefix = dump_key_prefix(&t());
        let keys = vec![
            format!("{prefix}/100/toc.dat"),
            format!("{prefix}/300/toc.dat"),
            format!("{prefix}/300/3.dat"), // same dump, extra data file — collapses
            format!("{prefix}/200/toc.dat"),
        ];
        assert_eq!(
            select_latest_dump_key(&prefix, &keys),
            Some(format!("{prefix}/300")),
            "the newest dump directory, returned as the bare key <prefix>/<ts>"
        );
    }

    #[test]
    fn select_latest_dump_key_orders_by_embedded_timestamp_not_listing_order() {
        // The timestamp is compared NUMERICALLY, not lexically or in listing order:
        // "100" sorts before "90" lexically, but 100 is the newer dump. Picking the
        // OLDEST (a flipped max→min) returns <prefix>/90 and fails here.
        let prefix = dump_key_prefix(&t());
        let keys = vec![format!("{prefix}/100"), format!("{prefix}/90")];
        assert_eq!(
            select_latest_dump_key(&prefix, &keys),
            Some(format!("{prefix}/100")),
            "newest is the numerically-greatest timestamp, not the lexical/listing max"
        );
    }

    #[test]
    fn select_latest_dump_key_ignores_foreign_and_malformed_keys() {
        // A real (recursive) store listing carries other envs' objects and stray keys;
        // none may mask or outrank a real dump under this project-env's prefix.
        let prefix = dump_key_prefix(&t()); // dumps/acme/billing/dev
        let keys = vec![
            "dumps/acme/billing/prod/999/toc.dat".to_string(), // foreign env, newer ts
            "dumps/other/billing/dev/999".to_string(),         // foreign org
            format!("{prefix}/notanumber/toc.dat"),            // malformed timestamp
            prefix.clone(),                                    // the bare prefix, no ts
            format!("{prefix}/150/toc.dat"),                   // the only real dump
        ];
        assert_eq!(
            select_latest_dump_key(&prefix, &keys),
            Some(format!("{prefix}/150")),
            "foreign-prefix and non-numeric-timestamp keys are ignored"
        );
    }

    #[test]
    fn select_latest_dump_key_is_none_for_empty_or_all_foreign() {
        // No dump under the prefix ⇒ `None`, so restore falls through to the existing
        // "no dump recorded" error path (the fallback never invents a dump).
        let prefix = dump_key_prefix(&t());
        assert_eq!(select_latest_dump_key(&prefix, &[]), None);
        let foreign = vec![
            "dumps/acme/billing/prod/1".to_string(),
            "unrelated/object".to_string(),
        ];
        assert_eq!(select_latest_dump_key(&prefix, &foreign), None);
    }

    #[test]
    fn default_dump_schedule_is_a_daily_cron() {
        // D18: the cadence is no longer a closed-tier knob — a fixed daily default
        // (a per-env dump_cadence policy field is a future additive column).
        assert_eq!(DEFAULT_DUMP_SCHEDULE, "0 3 * * *");
        assert_eq!(
            DEFAULT_DUMP_SCHEDULE.split_whitespace().count(),
            5,
            "a 5-field cron"
        );
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
        let cr = render_project_env_dump_cronjob(&t(), DEFAULT_DUMP_SCHEDULE, DEFAULT_BUCKET);
        assert_eq!(cr["apiVersion"], "batch/v1");
        assert_eq!(cr["kind"], "CronJob");
        assert_eq!(cr["metadata"]["name"], "wamn-dump-acme--billing--dev");
        assert_eq!(cr["metadata"]["namespace"], "wamn-system");
        assert_eq!(cr["metadata"]["labels"]["wamn.env"], "dev");
        // The schedule is the tier cadence; dumps never overlap.
        assert_eq!(cr["spec"]["schedule"], "0 3 * * *");
        assert_eq!(cr["spec"]["concurrencyPolicy"], "Forbid");
        let pod = &cr["spec"]["jobTemplate"]["spec"]["template"]["spec"];
        // The init container runs `pg_dump -Fd`; its connection comes from the
        // project-env credential Secret's `url` key.
        let init = &pod["initContainers"][0];
        let dump_cmd = init["command"][2].as_str().unwrap();
        assert!(
            dump_cmd.contains("pg_dump -Fd"),
            "init runs a directory-format dump"
        );
        let dburl = &init["env"][0];
        assert_eq!(dburl["name"], "DATABASE_URL");
        assert_eq!(
            dburl["valueFrom"]["secretKeyRef"]["name"],
            "wamn-db-acme--billing--dev"
        );
        assert_eq!(dburl["valueFrom"]["secretKeyRef"]["key"], "url");
        // The upload container runs `mc` against the shared MinIO under the
        // derivable object key, with the shared object-store credentials.
        let upload = &pod["containers"][0];
        let up_cmd = upload["command"][2].as_str().unwrap();
        assert!(
            up_cmd.contains("mc mirror /dump/out"),
            "uploads the dump's contents via the MinIO client"
        );
        assert!(
            up_cmd.contains("store/wamn-dumps/dumps/acme/billing/dev/"),
            "uploads under the derivable object key"
        );
        let up_env = &upload["env"];
        assert_eq!(up_env[0]["name"], "S3_ENDPOINT");
        assert_eq!(up_env[0]["value"], "http://minio.wamn-system.svc:9000");
        assert_eq!(
            up_env[1]["valueFrom"]["secretKeyRef"]["name"],
            "wamn-object-store"
        );
        assert_eq!(
            up_env[1]["valueFrom"]["secretKeyRef"]["key"],
            "ACCESS_KEY_ID"
        );
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
        // Same init(pg_dump) + upload(mc) pod as the scheduled path.
        let pod = &job["spec"]["template"]["spec"];
        assert!(
            pod["initContainers"][0]["command"][2]
                .as_str()
                .unwrap()
                .contains("pg_dump -Fd")
        );
        assert!(
            pod["containers"][0]["command"][2]
                .as_str()
                .unwrap()
                .contains("mc mirror /dump/out")
        );
    }

    #[test]
    fn dump_resource_name_length_is_bounded() {
        assert!(validate_dump_resource_name(&t()).is_ok());
        // A pathologically long triple overflows the CronJob-name bound.
        let long = Triple::new("o".repeat(30), "p".repeat(30), "prod");
        assert!(matches!(
            validate_dump_resource_name(&long),
            Err(crate::ProvisionError::NameTooLong { max: 52, .. })
        ));
    }
}
