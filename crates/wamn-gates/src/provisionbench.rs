//! `provisionbench` — the 2.3 provisioning gate of record.
//!
//! Provisions TWO projects through the **real** `provision-project` path
//! ([`wamn_host::provision::provision_project`]) against a cluster (a superuser
//! URL — locally a throwaway `postgres:18`, in-cluster the CloudNativePG
//! cluster, the gate of record), then proves the full runtime chain:
//!
//! * **routing / resolution** — the credential each project emits, parsed
//!   through the plugin's own `StaticCredentialProvider` (the `from_env` path),
//!   resolves to that project's database (a distinct marker witness, 111 / 222);
//! * **database-level isolation** — a project's connection cannot see another
//!   project's tables (Postgres has no cross-database queries);
//! * **least privilege** — the shared `wamn_app` role is `NOSUPERUSER
//!   NOCREATEDB`;
//! * **credential layout** — the emitted `Secret` carries the name + URL the
//!   future `K8sSecretProvider` (5x0.1) reads.
//!
//! A pure host-side `tokio_postgres` gate (no wasm guest) — the queuebench /
//! dispatchbench shape. The isolation model is per-database + per-DB CONNECT +
//! RLS-within with a single shared role (see docs/provisioning.md); dropping the
//! per-DB `GRANT CONNECT` fails the resolve→connect step here, dropping the
//! `REVOKE … FROM PUBLIC` fails the wamn-provision live-apply gate.

use anyhow::{Context as _, bail};
use clap::Args;
use tokio_postgres::{Client, NoTls};

use wamn_host::plugins::wamn_postgres::{
    CredentialProvider, StaticCredentialProvider, WamnPostgresConfig,
};
use wamn_provision::{database_name, secret, sql};

#[derive(Debug, Args)]
pub struct ProvisionBenchArgs {
    /// Superuser Postgres URL — provisions the project databases + role (the
    /// runtime `wamn_app` role is NOSUPERUSER/NOCREATEDB); env `WAMN_PG_ADMIN_URL`.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: Option<String>,
}

const PROJECT_A: &str = "provbench-a";
const PROJECT_B: &str = "provbench-b";
const MARKER_A: i32 = 111;
const MARKER_B: i32 = 222;
const APP_PASSWORD: &str = "wamn_app";

pub async fn run(args: ProvisionBenchArgs) -> anyhow::Result<()> {
    let admin_url = args.admin_database_url.as_deref().context(
        "provisionbench needs a superuser url (--admin-database-url / WAMN_PG_ADMIN_URL)",
    )?;

    println!(
        "== [2.3] provisionbench: per-project provisioning + credential resolution + isolation =="
    );

    // Clean slate (a prior failed run may have left the databases). Additive to
    // the shared cluster otherwise — these are gate-owned project databases.
    drop_projects(admin_url).await?;

    // 1. Provision both projects through the production path; capture the
    //    emitted app-role URLs.
    let url_a =
        wamn_host::provision::provision_project(admin_url, PROJECT_A, APP_PASSWORD, None, None)
            .await
            .context("provision project a")?;
    let url_b =
        wamn_host::provision::provision_project(admin_url, PROJECT_B, APP_PASSWORD, None, None)
            .await
            .context("provision project b")?;

    // 2. Seed each database with a distinct marker (routing witness) and a
    //    project-private table (isolation witness). provision-project delivers
    //    an EMPTY database, so this is gate scaffolding done as superuser.
    seed_witnesses(admin_url, PROJECT_A, MARKER_A).await?;
    seed_witnesses(admin_url, PROJECT_B, MARKER_B).await?;

    // 3. Resolve through the plugin's StaticCredentialProvider, fed by the SAME
    //    projects-file JSON provision-project emits (the from_env parse path).
    let mut obj = serde_json::Map::new();
    obj.insert(PROJECT_A.into(), secret::projects_file_entry(&url_a));
    obj.insert(PROJECT_B.into(), secret::projects_file_entry(&url_b));
    let projects_json = serde_json::to_string(&serde_json::Value::Object(obj))?;
    let base = WamnPostgresConfig::from_env();
    let projects = StaticCredentialProvider::projects_from_json(&projects_json, &base)
        .context("parse emitted projects-file json")?;
    let provider = StaticCredentialProvider::new(projects, None);

    let cfg_a = provider
        .resolve(PROJECT_A)?
        .with_context(|| format!("resolve {PROJECT_A}"))?;
    let cfg_b = provider
        .resolve(PROJECT_B)?
        .with_context(|| format!("resolve {PROJECT_B}"))?;

    // 4a. Routing witness: each resolved URL reaches its own project's database.
    let (client_a, task_a) = connect(&cfg_a.database_url).await.context("connect a")?;
    let (client_b, task_b) = connect(&cfg_b.database_url).await.context("connect b")?;
    let marker_a = query_marker(&client_a).await.context("marker a")?;
    let marker_b = query_marker(&client_b).await.context("marker b")?;
    if marker_a != MARKER_A || marker_b != MARKER_B {
        bail!(
            "routing witness FAIL: a saw {marker_a} (want {MARKER_A}), b saw {marker_b} (want {MARKER_B})"
        );
    }
    println!("  routing: project a -> marker {marker_a}, project b -> marker {marker_b}");

    // 4b. Database-level isolation: a's connection cannot see b's private table
    //     (and vice versa) — a different database, no cross-database queries.
    assert_invisible(&client_a, "only_in_b").await?;
    assert_invisible(&client_b, "only_in_a").await?;
    println!("  isolation: each project's connection cannot see the other's tables");

    // 4c. Least privilege: the resolved connection is the NOSUPERUSER/NOCREATEDB
    //     runtime role (read from the app connection itself).
    let (is_super, can_createdb): (bool, bool) = {
        let row = client_a
            .query_one(
                "SELECT rolsuper, rolcreatedb FROM pg_roles WHERE rolname = current_user",
                &[],
            )
            .await
            .context("read role attributes")?;
        (row.get(0), row.get(1))
    };
    if is_super || can_createdb {
        bail!("least-privilege FAIL: wamn_app super={is_super} createdb={can_createdb}");
    }
    println!("  least privilege: wamn_app is NOSUPERUSER NOCREATEDB");

    drop(client_a);
    drop(client_b);
    let _ = task_a.await;
    let _ = task_b.await;

    // 5. Credential layout: the emitted Secret carries the name + URL 5x0.1 reads.
    let sec = secret::render_secret_manifest(PROJECT_A, "wamn-system", &url_a);
    let ok_name = sec["metadata"]["name"] == format!("wamn-db-{PROJECT_A}");
    let ok_url = sec["stringData"]["url"] == url_a;
    if !ok_name || !ok_url {
        bail!("secret layout FAIL: name_ok={ok_name} url_ok={ok_url} ({sec})");
    }
    println!(
        "  secret layout: {} carries the app-role url",
        sec["metadata"]["name"]
    );

    // Teardown (self-contained — never touches shared databases).
    drop_projects(admin_url).await?;

    println!("provisionbench: overall PASS");
    Ok(())
}

/// Drop both gate project databases (pure builder), if present. Autocommit.
async fn drop_projects(admin_url: &str) -> anyhow::Result<()> {
    let (client, task) = connect(admin_url).await.context("admin connect")?;
    for project in [PROJECT_A, PROJECT_B] {
        client
            .batch_execute(&sql::drop_database_sql(project))
            .await
            .with_context(|| format!("drop {}", database_name(project)))?;
    }
    drop(client);
    let _ = task.await;
    Ok(())
}

/// Seed the routing marker (`marker`) and a project-private table
/// (`only_in_<project>`) into the project database, as superuser, granting
/// `wamn_app` SELECT (mirroring how the 3.2 floor grants per-table).
async fn seed_witnesses(admin_url: &str, project: &str, marker: i32) -> anyhow::Result<()> {
    let db_url = swap_db(admin_url, &database_name(project))?;
    let (client, task) = connect(&db_url)
        .await
        .with_context(|| format!("connect {}", database_name(project)))?;
    let private = format!("only_in_{}", project.replace('-', "_"));
    client
        .batch_execute(&format!(
            "CREATE TABLE marker (n int NOT NULL); \
             INSERT INTO marker VALUES ({marker}); \
             GRANT SELECT ON marker TO wamn_app; \
             CREATE TABLE {private} (id int); \
             GRANT SELECT ON {private} TO wamn_app;"
        ))
        .await
        .context("seed witnesses")?;
    drop(client);
    let _ = task.await;
    Ok(())
}

async fn query_marker(client: &Client) -> anyhow::Result<i32> {
    Ok(client.query_one("SELECT n FROM marker", &[]).await?.get(0))
}

/// Assert `table` is invisible from this connection (undefined_table, 42P01) —
/// the cross-database isolation witness. A different error (or success) fails.
async fn assert_invisible(client: &Client, table: &str) -> anyhow::Result<()> {
    match client
        .query_one(&format!("SELECT 1 FROM {table}"), &[])
        .await
    {
        Ok(_) => bail!("isolation FAIL: {table} is visible across databases"),
        Err(e) => {
            let code = e.as_db_error().map(|d| d.code().code().to_string());
            if code.as_deref() == Some("42P01") {
                Ok(())
            } else {
                bail!("isolation check for {table} got an unexpected error: {e} (code={code:?})")
            }
        }
    }
}

type Conn = tokio::task::JoinHandle<()>;

async fn connect(url: &str) -> anyhow::Result<(Client, Conn)> {
    let (client, conn) = tokio_postgres::connect(url, NoTls).await?;
    let task = tokio::spawn(async move {
        let _ = conn.await;
    });
    Ok((client, task))
}

/// Replace the database name (URL path) while preserving any `?params`.
fn swap_db(url: &str, db: &str) -> anyhow::Result<String> {
    let (base, query) = match url.split_once('?') {
        Some((b, q)) => (b, Some(q)),
        None => (url, None),
    };
    let slash = base.rfind('/').context("connection url has no path")?;
    let mut out = format!("{}/{db}", &base[..slash]);
    if let Some(q) = query {
        out.push('?');
        out.push_str(q);
    }
    Ok(out)
}
