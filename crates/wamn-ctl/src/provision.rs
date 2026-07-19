//! The `provision-project` subcommand (2.3): stand up a per-project Postgres
//! **database** on the shared cluster (D6: CloudNativePG) and emit the
//! credential the runtime + the future `K8sSecretProvider` (5x0.1) consume.
//!
//! An imperative CLI, run as a Job (the `publish-catalog` precedent — not a
//! Project CRD + controller, which is the 10.1 control plane). It connects as
//! the cluster **superuser** (only the operator/superuser can create databases
//! and roles — the runtime `wamn_app` role is `NOSUPERUSER NOCREATEDB`), runs
//! the pure [`wamn_provision`] builders, and produces:
//!
//! * a per-project database `wamn-db-<project>`, empty and RLS-ready — the input
//!   2.4 (system schema) consumes;
//! * the shared, least-privilege `wamn_app` role (idempotently ensured), granted
//!   `CONNECT` on the project database with `PUBLIC` revoked;
//! * the app-role connection URL, optionally as a `WAMN_PG_PROJECTS_FILE` entry
//!   (`--emit-projects-file`) and/or a Kubernetes `Secret` manifest
//!   (`--emit-secret`, JSON — `kubectl apply -f` accepts it).
//!
//! Everything is **additive** and idempotent (create-if-absent; the shared-
//! cluster guardrail): re-running against an already-provisioned project
//! refreshes the grants and re-emits the credential, never dropping anything.
//! Backups / WAL archiving / PITR are deferred to a fast-follow bead; per-project
//! **distinct** roles are an 8.2 hardening (see docs/provisioning.md).

use std::path::PathBuf;

use anyhow::Context as _;
use clap::Args;
use tokio_postgres::{Config as PgConfig, NoTls};

use wamn_provision::{APP_ROLE, compose_url, database_name, secret, sql, validate_project_id};

#[derive(Debug, Args)]
pub struct ProvisionProjectArgs {
    /// Project id: a lowercase slug `[a-z0-9-]` (start/end alphanumeric); maps to
    /// database + Secret `wamn-db-<project>`. The reserved `wamn` prefix is rejected.
    #[arg(long)]
    pub project: String,

    /// Superuser Postgres URL to the cluster's maintenance database (creates the
    /// database + role); env `WAMN_PG_ADMIN_URL`.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: Option<String>,

    /// Password for the shared `wamn_app` role (used when the role is first
    /// created and embedded in the emitted URL); env `WAMN_APP_PASSWORD`.
    #[arg(long, env = "WAMN_APP_PASSWORD", default_value = "wamn_app")]
    pub app_password: String,

    /// Host the runtime reaches the project database at (the cluster's `-rw`
    /// service). Defaults to the admin URL's host.
    #[arg(long)]
    pub app_host: Option<String>,

    /// Port the runtime reaches the project database at. Defaults to the admin
    /// URL's port (or 5432).
    #[arg(long)]
    pub app_port: Option<u16>,

    /// Namespace for the emitted `Secret` manifest.
    #[arg(long, env = "WAMN_NAMESPACE", default_value = "wamn-system")]
    pub namespace: String,

    /// Write the credential `Secret` (JSON manifest) here; `-` = stdout. The
    /// provisioning Job can pipe it to `kubectl apply -f -`.
    #[arg(long)]
    pub emit_secret: Option<PathBuf>,

    /// Write the `WAMN_PG_PROJECTS_FILE` entry (`{ <project>: { "url": … } }`)
    /// here; `-` = stdout. This is the shape the plugin's StaticCredentialProvider
    /// and the dispatcher `--projects-file` parse.
    #[arg(long)]
    pub emit_projects_file: Option<PathBuf>,
}

pub async fn run(args: ProvisionProjectArgs) -> anyhow::Result<()> {
    let admin_url = args
        .admin_database_url
        .clone()
        .context("no admin database url: pass --admin-database-url or set WAMN_PG_ADMIN_URL")?;

    let app_url = provision_project(
        &admin_url,
        &args.project,
        &args.app_password,
        args.app_host.as_deref(),
        args.app_port,
    )
    .await?;

    // Emit the credential in whichever shapes were requested; always print a
    // human summary (the URL) so a Job's logs record what was provisioned.
    println!(
        "provisioned project {project:?}: database {db:?}, {role} CONNECT granted (PUBLIC revoked)",
        project = args.project,
        db = database_name(&args.project),
        role = APP_ROLE,
    );
    println!("app url: {app_url}");

    if let Some(path) = &args.emit_projects_file {
        let doc = secret::projects_file(&args.project, &app_url);
        write_json(path, &doc).context("emit projects file")?;
    }
    if let Some(path) = &args.emit_secret {
        let doc = secret::render_secret_manifest(&args.project, &args.namespace, &app_url);
        write_json(path, &doc).context("emit secret")?;
    }

    Ok(())
}

/// The reusable provisioning core (also driven by the `provisionbench` gate):
/// validate the project id, connect as superuser, ensure the shared role, create
/// the database when absent, confine `CONNECT` to `wamn_app`, and return the
/// composed app-role connection URL. Idempotent + additive (the shared-cluster
/// guardrail): never drops or alters an existing object.
///
/// `app_host`/`app_port` default to the admin URL's host/port (the app role
/// reaches the same cluster the superuser provisioned it on).
pub async fn provision_project(
    admin_url: &str,
    project: &str,
    app_password: &str,
    app_host: Option<&str>,
    app_port: Option<u16>,
) -> anyhow::Result<String> {
    validate_project_id(project).map_err(|e| anyhow::anyhow!("project id: {e}"))?;

    let (default_host, default_port) = parse_host_port(admin_url)?;
    let host = app_host.map(str::to_string).unwrap_or(default_host);
    let port = app_port.unwrap_or(default_port);
    let db = database_name(project);

    let (client, conn) = tokio_postgres::connect(admin_url, NoTls)
        .await
        .context("admin connect")?;
    let conn_task = tokio::spawn(conn);
    let result = do_provision(&client, project, app_password).await;
    drop(client);
    let _ = conn_task.await;
    result?;

    Ok(compose_url(APP_ROLE, app_password, &host, port, &db))
}

/// Ensure the role, create the database when absent, confine CONNECT.
async fn do_provision(
    client: &tokio_postgres::Client,
    project: &str,
    app_password: &str,
) -> anyhow::Result<()> {
    let db = database_name(project);

    // 1. The shared, least-privilege app role (idempotent; pre-created in prod).
    client
        .batch_execute(&sql::ensure_app_role_sql(app_password))
        .await
        .context("ensure wamn_app role")?;

    // 2. The project database, when absent. CREATE DATABASE is autocommit and
    //    cannot run in a transaction block — a single-statement batch is fine.
    let exists: bool = client
        .query_one(sql::database_exists_sql(), &[&db])
        .await
        .context("probe database")?
        .get(0);
    if exists {
        println!("database {db:?} already present; refreshing grants");
    } else {
        client
            .batch_execute(&sql::create_database_sql(project))
            .await
            .with_context(|| format!("create database {db:?}"))?;
        println!("created database {db:?}");
    }

    // 3. Confine CONNECT to wamn_app (revoke PUBLIC). Idempotent.
    client
        .batch_execute(&sql::grant_connect_sql(project))
        .await
        .context("confine CONNECT")?;

    Ok(())
}

/// Extract the first TCP host + port from a libpq URL, for composing the
/// runtime-facing app URL. Port defaults to 5432 when unspecified.
fn parse_host_port(url: &str) -> anyhow::Result<(String, u16)> {
    let config: PgConfig = url.parse().context("parse admin database url")?;
    let host = config
        .get_hosts()
        .iter()
        .find_map(|h| match h {
            tokio_postgres::config::Host::Tcp(h) => Some(h.clone()),
            _ => None,
        })
        .context("admin url has no TCP host; pass --app-host")?;
    let port = config.get_ports().first().copied().unwrap_or(5432);
    Ok((host, port))
}

fn write_json(path: &PathBuf, doc: &serde_json::Value) -> anyhow::Result<()> {
    let text = serde_json::to_string_pretty(doc)?;
    if path.as_os_str() == "-" {
        println!("{text}");
    } else {
        std::fs::write(path, text).with_context(|| format!("write {}", path.display()))?;
        println!("wrote {}", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_host_port;

    #[test]
    fn host_port_derives_from_the_admin_url() {
        let (h, p) = parse_host_port("postgres://postgres:pw@wamn-pg-rw:5432/postgres").unwrap();
        assert_eq!(h, "wamn-pg-rw");
        assert_eq!(p, 5432);
        // Port defaults to 5432 when unspecified.
        let (h, p) = parse_host_port("postgres://postgres@db.internal/postgres").unwrap();
        assert_eq!(h, "db.internal");
        assert_eq!(p, 5432);
    }
}
