//! The `restore-project-env` subcommand (wamn-q3n.11): restore a per-project-env
//! logical **dump** back into a database — the restore counterpart of
//! `dump-project-env` (docs/postgres-topology.md §Backup architecture).
//!
//! `pg_restore` of a `pg_dump -Fd` directory artifact into one of two targets:
//!
//! * **scratch database** (default, non-destructive): restore into a fresh
//!   `wamn-restore-<org>--<project>--<env>` database so the dump can be inspected or
//!   a single table carved out without touching the live project-env DB — the
//!   sub-cluster carve-out path (T3 arbitrary-instant / intra-cluster T2). The
//!   scratch DB is left standing for inspection (drop it when done);
//! * **in place** (`--in-place --confirm`, destructive): `pg_restore --clean
//!   --if-exists` over the live project-env database — restore-to-last-dump.
//!   `--confirm` is required because it drops and replaces the live data.
//!
//! **Which dump:** an explicit `--dump-dir` (a local `-Fd` directory) wins;
//! otherwise the dump **catalog** (`provisioning.dumps` in the T1 registry) is read
//! for the latest recorded dump (or `--object-key`), and the dump directory is
//! `--dump-root/<timestamp>` (the timestamp is the object key's last segment — the
//! `dump-project-env --run-now --out-dir` layout). So restore-to-last-dump needs no
//! manual key. The dump **bytes** are local until the shared object store lands
//! (wamn-e1g); the catalog says *which* dump, staged under `--dump-root`.
//!
//! **Scope (wamn-q3n.11):** logical-dump restore (this) + the operator restore
//! runbook + the audit-rewind caveat (docs). Whole-cluster **PITR** (restore an org
//! cluster to an arbitrary instant, then carve one DB out) needs WAL/PITR and is
//! wamn-e1g — cross-referenced from the runbook, not implemented here.

use std::path::{Path, PathBuf};
use std::process::Command as Proc;

use anyhow::Context as _;
use clap::Args;
use tokio_postgres::NoTls;

use wamn_provision::{
    pg_restore_argv, project_env_database_name, restore_scratch_db_name, sql, validate_project_env,
    validate_restore_scratch_name,
};
use wamn_registry::Triple;

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

    /// Local root the dumps are staged under (the object-store mirror until
    /// wamn-e1g). When `--dump-dir` is absent, the dump directory is
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

pub async fn run(args: RestoreProjectEnvArgs) -> anyhow::Result<()> {
    let triple = Triple::new(&args.org, &args.project, args.env.as_str());
    validate_project_env(&args.org, &args.project, &args.env)
        .map_err(|e| anyhow::anyhow!("project-env names: {e}"))?;

    // Resolve which dump directory to restore (explicit dir, or the catalog).
    let (dump_dir, object_key) = resolve_dump_dir(&args, &triple).await?;
    anyhow::ensure!(
        dump_dir.join("toc.dat").exists(),
        "dump directory {} is not a pg_dump -Fd artifact (no toc.dat) — stage the dump there \
         (dump-project-env --run-now --out-dir) or pass --dump-dir",
        dump_dir.display()
    );
    match &object_key {
        Some(key) => println!(
            "restoring {triple} from dump {key} ({})",
            dump_dir.display()
        ),
        None => println!("restoring {triple} from {}", dump_dir.display()),
    }

    let admin_url = args
        .database_url
        .as_deref()
        .context("restore needs --database-url (a superuser URL to the TARGET cluster)")?;
    let dump_dir_str = dump_dir.to_string_lossy().to_string();

    if args.in_place {
        restore_in_place(&args, &triple, admin_url, &dump_dir_str).await
    } else {
        restore_into_scratch(&args, &triple, admin_url, &dump_dir_str).await
    }
}

/// Resolve the dump directory: an explicit `--dump-dir` wins; otherwise read the
/// catalog (latest, or `--object-key`) and derive `<dump-root>/<timestamp>`, where
/// the timestamp is the object key's last path segment (the `--run-now` layout).
async fn resolve_dump_dir(
    args: &RestoreProjectEnvArgs,
    triple: &Triple,
) -> anyhow::Result<(PathBuf, Option<String>)> {
    if let Some(dir) = &args.dump_dir {
        return Ok((dir.clone(), None));
    }
    let system_url = args.system_database_url.as_deref().context(
        "pass --dump-dir, or --system-database-url to read the dump catalog (restore-to-last-dump)",
    )?;
    let key = match &args.object_key {
        Some(k) => k.clone(),
        None => latest_dump_key(system_url, triple, &args.dump_root).await?,
    };
    // The dump is staged locally under <dump-root>/<timestamp> (the object key's
    // last segment — the dump-project-env --run-now --out-dir layout).
    let timestamp = key
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .with_context(|| format!("malformed dump object key {key:?}"))?;
    Ok((args.dump_root.join(timestamp), Some(key)))
}

/// Resolve restore-to-last-dump's dump key: the genuinely NEWEST dump across the
/// `provisioning.dumps` **catalog** (imperative `--run-now` dumps record there) AND a
/// direct **listing** of the dump key prefix staged under `--dump-root`. The listing is
/// the fallback for scheduled CronJob dumps, which upload to object storage but record
/// NO catalog row — that pod holds only the project-env DB URL + object-store creds, not
/// the `wamn_system` connection (wamn-cjv.19). Without it those dumps are invisible and
/// restore-to-last-dump errors "no dump recorded" though dumps exist. Folding the
/// catalog's own latest key into the candidate set also lets a newer STAGED dump beat a
/// stale recorded one — the last dump, not merely the last *recorded* one. Prints WHICH
/// path found the chosen dump; errors only if neither offers one.
async fn latest_dump_key(
    system_url: &str,
    triple: &Triple,
    dump_root: &Path,
) -> anyhow::Result<String> {
    let recorded = recorded_latest_dump_key(system_url, triple).await?;

    let prefix = wamn_provision::dump::dump_key_prefix(triple);
    let mut candidates = staged_dump_keys(dump_root, &prefix);
    if let Some(key) = &recorded {
        candidates.push(key.clone());
    }
    let key =
        wamn_provision::dump::select_latest_dump_key(&prefix, &candidates).with_context(|| {
            format!(
                "no dump recorded for {triple} in provisioning.dumps, and none staged under {} — \
                 run dump-project-env --run-now first, or stage a scheduled dump under --dump-root",
                dump_root.display()
            )
        })?;

    if recorded.as_deref() == Some(key.as_str()) {
        println!("restore-to-last-dump: newest dump {key} (from the provisioning.dumps catalog)");
    } else {
        println!(
            "restore-to-last-dump: newest dump {key} (found by listing the dump prefix {prefix:?} \
             staged under --dump-root — NOT in the provisioning.dumps catalog, e.g. a scheduled \
             CronJob dump)"
        );
    }
    Ok(key)
}

/// The latest recorded dump's object key for a project-env from the catalog (as the
/// `wamn_system` owner), or `None` when no dump is recorded. Unlike a hard error,
/// `None` lets [`latest_dump_key`] fall back to the prefix listing (wamn-cjv.19).
async fn recorded_latest_dump_key(
    system_url: &str,
    triple: &Triple,
) -> anyhow::Result<Option<String>> {
    let (client, conn) = tokio_postgres::connect(system_url, NoTls)
        .await
        .context("system db connect")?;
    let conn_task = tokio::spawn(conn);
    let result = do_recorded_latest_dump_key(&client, triple).await;
    drop(client);
    let _ = conn_task.await;
    result
}

async fn do_recorded_latest_dump_key(
    client: &tokio_postgres::Client,
    triple: &Triple,
) -> anyhow::Result<Option<String>> {
    client
        .batch_execute("SET ROLE wamn_system")
        .await
        .context("SET ROLE wamn_system")?;
    let env = triple.env.as_str();
    let row = client
        .query_opt(
            wamn_registry::sql::select_latest_dump_sql(),
            &[&triple.org, &triple.project, &env],
        )
        .await
        .context("read latest dump from provisioning.dumps")?;
    Ok(row.map(|r| r.get("object_key")))
}

/// The dump keys currently STAGED under `--dump-root` for a project-env, in the
/// object-key form [`select_latest_dump_key`](wamn_provision::dump::select_latest_dump_key)
/// ranks over. Until the shared object store lands (wamn-e1g), `--dump-root` is the local
/// mirror restore reads dump bytes from, so each timestamp subdirectory holding a
/// complete `-Fd` dump (a `toc.dat`) IS a listed dump. The `toc.dat` gate is the local
/// mirror's completeness signal (a torn/partial directory is skipped). A missing or
/// unreadable root ⇒ no staged dumps.
fn staged_dump_keys(dump_root: &Path, prefix: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dump_root) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|e| e.path().join("toc.dat").is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .map(|name| format!("{prefix}/{name}"))
        .collect()
}

/// Restore into a fresh scratch database (non-destructive). The scratch DB is left
/// standing for inspection / carve-out; the drop command is printed.
async fn restore_into_scratch(
    args: &RestoreProjectEnvArgs,
    triple: &Triple,
    admin_url: &str,
    dump_dir: &str,
) -> anyhow::Result<()> {
    let scratch = match &args.scratch_db {
        Some(s) => s.clone(),
        None => {
            validate_restore_scratch_name(triple)
                .map_err(|e| anyhow::anyhow!("scratch db name: {e}"))?;
            restore_scratch_db_name(triple)
        }
    };

    // Fresh scratch database (CREATE/DROP DATABASE are autocommit — batch_execute).
    recreate_database(admin_url, &scratch).await?;

    let conninfo = swap_db(admin_url, &scratch);
    run_pg_restore(&conninfo, dump_dir, false)?;

    println!(
        "restored into scratch database {scratch:?} (non-destructive). Inspect it, then drop:\n  \
         psql {admin_url:?} -c 'DROP DATABASE IF EXISTS \"{scratch}\" WITH (FORCE)'"
    );
    Ok(())
}

/// Whether a destructive in-place restore may proceed. In place `pg_restore
/// --clean` drops and replaces the LIVE project-env database, so it requires
/// explicit `--confirm` — the destructive gate.
fn in_place_confirmed(confirm: bool) -> bool {
    confirm
}

/// Restore IN PLACE over the live project-env database (destructive; `--confirm`
/// gated). `pg_restore --clean --if-exists` drops each object before recreating it.
async fn restore_in_place(
    args: &RestoreProjectEnvArgs,
    triple: &Triple,
    admin_url: &str,
    dump_dir: &str,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        in_place_confirmed(args.confirm),
        "--in-place drops and replaces the LIVE {triple} database — re-run with --confirm to proceed"
    );
    let db_name = project_env_database_name(&args.org, &args.project, triple.env.as_str());
    let conninfo = swap_db(admin_url, &db_name);
    run_pg_restore(&conninfo, dump_dir, true)?;
    println!("restored {triple} in place over the live database {db_name:?} (--clean)");
    Ok(())
}

/// Run `pg_restore` with the pure argv builder; fail on a non-zero exit.
fn run_pg_restore(conninfo: &str, dump_dir: &str, clean: bool) -> anyhow::Result<()> {
    let argv = pg_restore_argv(conninfo, dump_dir, clean);
    let status = Proc::new(&argv[0])
        .args(&argv[1..])
        .status()
        .with_context(|| format!("spawn {} (is pg_restore installed?)", argv[0]))?;
    anyhow::ensure!(status.success(), "pg_restore failed ({status})");
    Ok(())
}

/// Drop + create a database via the admin URL (autocommit — `CREATE DATABASE`
/// cannot run in a transaction block). Reuses the pure `wamn-provision` builders.
async fn recreate_database(admin_url: &str, database: &str) -> anyhow::Result<()> {
    let (client, conn) = tokio_postgres::connect(admin_url, NoTls)
        .await
        .context("target cluster connect")?;
    let conn_task = tokio::spawn(conn);
    let result = async {
        client
            .batch_execute(&sql::drop_database_named_sql(database))
            .await
            .context("drop stale scratch database")?;
        client
            .batch_execute(&sql::create_database_named_sql(database))
            .await
            .context("create scratch database")?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    drop(client);
    let _ = conn_task.await;
    result
}

/// Swap the database path segment of a libpq URL, preserving any query string
/// (the connection driver's concern — the builders stay pure). Mirrors the dump
/// round-trip gate's helper.
pub(crate) fn swap_db(url: &str, db: &str) -> String {
    let (no_q, query) = match url.split_once('?') {
        Some((a, b)) => (a, Some(b)),
        None => (url, None),
    };
    let (base, _old) = no_q.rsplit_once('/').unwrap_or((url, ""));
    match query {
        Some(q) => format!("{base}/{db}?{q}"),
        None => format!("{base}/{db}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swap_db_replaces_the_database_segment_keeping_the_query() {
        assert_eq!(
            swap_db(
                "postgres://u:p@h:5432/postgres",
                "wamn-restore-acme--app--dev"
            ),
            "postgres://u:p@h:5432/wamn-restore-acme--app--dev"
        );
        // A query string (e.g. sslmode) is preserved across the swap.
        assert_eq!(
            swap_db("postgres://u:p@h:5432/postgres?sslmode=disable", "scratch"),
            "postgres://u:p@h:5432/scratch?sslmode=disable"
        );
    }

    #[test]
    fn a_timestamp_is_the_object_keys_last_segment() {
        // The catalog-selected dump directory is <dump-root>/<timestamp>, and the
        // timestamp is the object key's trailing segment (the --run-now layout).
        let key = "dumps/acme/billing/dev/1720000000";
        assert_eq!(key.rsplit('/').next(), Some("1720000000"));
    }

    #[test]
    fn staged_dump_keys_lists_only_complete_dumps_and_the_fallback_picks_the_newest() {
        // The restore-to-last-dump fallback over the LOCAL --dump-root mirror
        // (wamn-cjv.19): a scheduled CronJob dump staged here but absent from the
        // catalog is still found. Only complete dumps (a `toc.dat`) are candidates —
        // a torn newest directory is skipped in favour of the complete older one.
        let root = std::env::temp_dir().join(format!("wamn-cjv19-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (ts, complete) in [("100", true), ("300", true), ("400", false)] {
            let dir = root.join(ts);
            std::fs::create_dir_all(&dir).unwrap();
            if complete {
                std::fs::write(dir.join("toc.dat"), b"x").unwrap();
            }
        }
        std::fs::write(root.join("readme"), b"not a dump").unwrap(); // a stray non-dir

        let triple = Triple::new("acme", "billing", "dev");
        let prefix = wamn_provision::dump::dump_key_prefix(&triple);
        let mut keys = staged_dump_keys(&root, &prefix);
        keys.sort();
        // Only the complete (toc.dat) timestamp dirs, in <prefix>/<ts> object-key form;
        // the torn `400` and the stray `readme` are excluded.
        assert_eq!(keys, vec![format!("{prefix}/100"), format!("{prefix}/300")]);
        // The fallback picks the newest COMPLETE staged dump (300), not the torn 400.
        assert_eq!(
            wamn_provision::dump::select_latest_dump_key(&prefix, &keys),
            Some(format!("{prefix}/300"))
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn in_place_requires_confirmation() {
        // The destructive in-place restore refuses without --confirm and proceeds
        // only with it (the destructive gate).
        assert!(
            !in_place_confirmed(false),
            "in-place must refuse without --confirm"
        );
        assert!(in_place_confirmed(true), "in-place proceeds with --confirm");
    }
}
