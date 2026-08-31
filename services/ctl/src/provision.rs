//! The `provision-project` subcommand (2.3): stand up a per-project Postgres
//! **database** on the shared cluster (D6: CloudNativePG) and emit the
//! credential the runtime + the future `K8sSecretProvider` (5x0.1) consume.
//!
//! An imperative CLI, run as a Job (the management-verb precedent — not a
//! Project CRD + controller, which is the 10.1 control plane). It connects as
//! the cluster **superuser** (only the operator/superuser can create databases
//! and roles — the runtime `wamn_app` role is `NOSUPERUSER NOCREATEDB`), runs
//! the pure [`wamn_control_provision`] builders, and produces:
//!
//! * a per-project database `wamn-db-<project>`, owned by the stable NOLOGIN
//!   `wamn_db_owner` title role, empty and RLS-ready — the input 2.4 (system
//!   schema) consumes;
//! * the shared, passwordless NOLOGIN `wamn_app` ACL role (idempotently
//!   ensured), granted `CONNECT` on the project database with `PUBLIC` revoked;
//! * the legacy app-role connection URL, optionally as a
//!   `WAMN_PG_PROJECTS_FILE` entry (`--emit-projects-file`) and/or a Kubernetes
//!   `Secret` manifest (`--emit-secret`, JSON — `kubectl apply -f` accepts it).
//!   `wamn-0h0g.12.185` owns removing this now-non-authenticating surface.
//!
//! Re-runs are idempotent at the intended boundary (create-if-absent; the
//! shared-cluster guardrail): an already-provisioned project converges title
//! ownership, refreshes the grants and re-emits the credential, never dropping
//! an object.
//! Backups / WAL archiving / PITR are deferred to a fast-follow bead; per-project
//! **distinct** roles are an 8.2 hardening (see docs/archive/platform/provisioning.md).

use std::path::PathBuf;

use anyhow::Context as _;
use clap::Args;
use tokio_postgres::{Config as PgConfig, NoTls};

use wamn_control_provision::{
    APP_ROLE, compose_url, database_name, secret, sql, validate_project_id,
};

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

    /// Password embedded in the legacy shared-app URL output. Supply it with
    /// `--app-password` or the env var `WAMN_APP_PASSWORD`.
    ///
    /// `wamn-0h0g.12.140` removes it from role SQL: `wamn_app` is a stable
    /// passwordless NOLOGIN ACL role. The argument and URL output remain until
    /// `wamn-0h0g.12.185` retires that legacy surface.
    ///
    /// **Deliberately has no `default_value`** — the same shape
    /// `--dispatch-reader-password` takes in `provision-project-env`
    /// (wamn-0h0g.12.122). A default here provisioned every environment with a
    /// publicly known password on a `LOGIN` role that guest-authored SQL
    /// executes as; a 2026-08-19 verifier read measured it live on every
    /// cluster the role existed on, because nothing ever overrode it.
    /// Provisioning refuses instead (wamn-0h0g.12.129).
    #[arg(
        long,
        env = "WAMN_APP_PASSWORD",
        value_name = "PASSWORD ($WAMN_APP_PASSWORD)"
    )]
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
/// validate the project id, connect as superuser, ensure the shared app and
/// title roles, create the database when absent, converge its title owner,
/// confine `CONNECT` to `wamn_app`, and return the composed app-role connection
/// URL. Re-runs are idempotent at the intended boundary (the shared-cluster
/// guardrail) and never drop an existing object.
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

/// Ensure the roles, create the database when absent, converge its owner, then
/// confine CONNECT.
async fn do_provision(
    client: &tokio_postgres::Client,
    project: &str,
    app_password: &str,
) -> anyhow::Result<()> {
    let db = database_name(project);

    // 1. The shared passwordless NOLOGIN app ACL role (idempotent; pre-created
    //    in production). `app_password` is a legacy URL input and the builder
    //    deliberately does not emit it.
    client
        .batch_execute(&sql::ensure_app_role_sql(app_password))
        .await
        .context("ensure wamn_app role")?;
    // Session drain is a cluster-wide cutover finalizer, not a per-database
    // provisioning side effect. The operator applies the emitted role.sql only
    // after every replacement carrier has been verified.
    client
        .batch_execute(sql::ensure_db_owner_role_sql())
        .await
        .context("ensure wamn_db_owner role")?;

    // 2. The project database, when absent. CREATE DATABASE is autocommit and
    //    cannot run in a transaction block — a single-statement batch is fine.
    let exists: bool = client
        .query_one(sql::database_exists_sql(), &[&db])
        .await
        .context("probe database")?
        .get(0);
    if exists {
        println!("database {db:?} already present; converging ownership and grants");
    } else {
        client
            .batch_execute(&sql::create_database_sql(project))
            .await
            .with_context(|| format!("create database {db:?}"))?;
        println!("created database {db:?}");
    }

    // 3. Converge legacy databases before granting CONNECT. The order is
    //    load-bearing when a login role is the outgoing owner: changing owner
    //    after its self-grant destroys the CONNECT ACL entry.
    client
        .batch_execute(&sql::set_database_owner_sql(&db))
        .await
        .context("converge database owner")?;

    // 4. Confine CONNECT to wamn_app (revoke PUBLIC). Idempotent.
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
    use clap::{CommandFactory as _, FromArgMatches as _, Parser};

    use super::{ProvisionProjectArgs, parse_host_port};

    #[derive(Debug, Parser)]
    struct TestCli {
        #[command(flatten)]
        args: ProvisionProjectArgs,
    }

    fn parse_without_app_password_env<const N: usize>(
        argv: [&str; N],
    ) -> Result<ProvisionProjectArgs, clap::Error> {
        let matches = TestCli::command()
            .mut_arg("app_password", |arg| arg.env(None::<&str>))
            .try_get_matches_from(argv)?;
        TestCli::from_arg_matches(&matches).map(|cli| cli.args)
    }

    /// No `default_value` on `--app-password` (wamn-0h0g.12.129).
    ///
    /// The argument remains for the legacy URL output until
    /// `wamn-0h0g.12.185`, but it cannot quietly reacquire the default whose
    /// verifier a 2026-08-19 read measured on every old shared LOGIN.
    #[test]
    fn the_app_password_has_no_default() {
        let error = parse_without_app_password_env(["test", "--project", "billing"])
            .expect_err("provisioning accepted a missing --app-password");
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
        assert!(
            error.to_string().contains("--app-password"),
            "unexpected missing-argument error: {error}"
        );
        assert_eq!(
            parse_without_app_password_env([
                "test",
                "--project",
                "billing",
                "--app-password",
                "probe",
            ])
            .unwrap()
            .app_password,
            "probe"
        );
    }

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
