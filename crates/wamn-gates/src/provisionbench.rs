//! `provisionbench` — the provisioning gate of record (2.3 + the four-tier
//! wamn-q3n.8 extension).
//!
//! Modes (`--mode`, default `all`):
//!
//! * **legacy** — the 2.3 flow, kept as regression: provision TWO projects through
//!   the **real** `provision-project` path
//!   ([`wamn_host::provision::provision_project`]), then prove routing/resolution
//!   (a marker witness resolved through the plugin's own `StaticCredentialProvider`),
//!   database-level isolation (no cross-database queries), least privilege
//!   (`wamn_app` is `NOSUPERUSER NOCREATEDB`), and the emitted `Secret` layout.
//! * **orgpair** — a **dedicated** org with two project-envs (`prod` + `dev`) as
//!   two per-project-env databases (`wamn-db-<org>--<project>--<env>`, provisioned
//!   via the REAL wamn-q3n.7 role/create/grant builders as a plain-SQL stand-in for
//!   the CNPG `Database` CRD, which needs the operator). Proves per-database
//!   routing / isolation / least-priv / the per-project-env Secret layout, records
//!   the D18 `registry.orgs`/`projects`/`project_envs` rows (placement + env-FK), lands
//!   a provisioning **saga** in the (ephemeral) system DB.
//! * **t3** — a **pooled** org (every env collapses onto the shared pool) with one
//!   project-env, the same per-placement assertions.
//! * **saga** — a focused proof of the wamn-q3n.8 saga builders: exactly-once
//!   create, durable step advance, terminal complete + fail.
//! * **all** — legacy, then (over one ephemeral registry schema) saga, orgpair, t3.
//!
//! A pure host-side `tokio_postgres` gate (no wasm guest) — the queuebench /
//! dispatchbench shape. Substrate-agnostic (a superuser URL — locally a throwaway
//! `postgres:18`, in-cluster the shared CloudNativePG pool): the placement modes
//! **simulate** per-project-env DB creation with plain SQL and keep the registry /
//! saga in an ephemeral `wamn_system`-shaped schema, since the CNPG `Database` CRD
//! and the physical cross-CLUSTER isolation of a real dedicated org need the operator.
//! That physical-isolation gate of record is the live org-pair standup runbook
//! (docs/provisioning.md §provisionbench / CLAUDE.md).

use anyhow::{Context as _, bail};
use clap::{Args, ValueEnum};
use tokio_postgres::{Client, NoTls};

use wamn_host::plugins::wamn_postgres::{
    CredentialProvider, StaticCredentialProvider, WamnPostgresConfig,
};
use wamn_provision::{
    APP_ROLE, compose_url, database_name, project_env_database_name,
    render_project_env_secret_manifest, secret, sql,
};
use wamn_registry::sql as reg_sql;
use wamn_registry::{Org, Triple};

/// The canonical T1 registry DDL (registry + provisioning schemas). Applied into
/// an ephemeral schema on the throwaway/pool PG for the tier modes' registry +
/// saga assertions — the standalone `deploy/system-schema.sql`, embedded so the
/// gate writes into the SAME shape the T1 wamn_system DB carries.
const SYSTEM_SCHEMA_SQL: &str = include_str!("../../../deploy/system-schema.sql");

#[derive(Debug, Args)]
pub struct ProvisionBenchArgs {
    /// Superuser Postgres URL — provisions the project databases + role (the
    /// runtime `wamn_app` role is NOSUPERUSER/NOCREATEDB); env `WAMN_PG_ADMIN_URL`.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: Option<String>,

    /// Which gate mode(s) to run.
    #[arg(long, value_enum, default_value_t = Mode::All)]
    pub mode: Mode,
}

/// The gate modes (see the module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    /// The 2.3 two-project regression.
    Legacy,
    /// A T2-shaped org pair (two project-envs).
    Orgpair,
    /// A T3 trials org (one project-env on the shared pool).
    T3,
    /// The saga builders in isolation.
    Saga,
    /// legacy → saga → orgpair → t3.
    All,
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

    let run_legacy = matches!(args.mode, Mode::Legacy | Mode::All);
    let run_saga = matches!(args.mode, Mode::Saga | Mode::All);
    let run_orgpair = matches!(args.mode, Mode::Orgpair | Mode::All);
    let run_t3 = matches!(args.mode, Mode::T3 | Mode::All);

    if run_legacy {
        legacy(admin_url).await?;
    }

    // The tier / saga modes assert registry + saga rows, so they need the T1
    // registry schema. Set it up once (ephemeral), run them, tear it down.
    if run_saga || run_orgpair || run_t3 {
        setup_registry(admin_url)
            .await
            .context("set up the ephemeral registry schema")?;
        if run_saga {
            saga_mode(admin_url).await?;
        }
        if run_orgpair {
            orgpair_mode(admin_url).await?;
        }
        if run_t3 {
            t3_mode(admin_url).await?;
        }
        teardown_registry(admin_url).await?;
    }

    println!("provisionbench: overall PASS");
    Ok(())
}

// ============================================================================
// legacy (2.3) — provision two projects, prove routing / isolation / least-priv
// ============================================================================

async fn legacy(admin_url: &str) -> anyhow::Result<()> {
    println!(
        "== [2.3] provisionbench legacy: per-project provisioning + credential resolution + isolation =="
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
    let (is_super, can_createdb) = role_attrs(&client_a).await?;
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

    println!("  legacy: PASS");
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

// ============================================================================
// tier modes (wamn-q3n.8) — org pair (T2) + trials pool (T3)
// ============================================================================

/// One env of a placement scenario: which environment slug, and the distinct
/// routing marker its per-project-env database carries.
struct EnvSpec {
    env: &'static str,
    marker: i32,
}

/// A **dedicated** org (owns per-recovery-domain clusters) with two project-envs —
/// `prod` and `dev` — as two per-project-env databases.
async fn orgpair_mode(admin_url: &str) -> anyhow::Result<()> {
    let org = Org::dedicated("gate-t2");
    tier_scenario(
        admin_url,
        "dedicated (prod + dev)",
        &org,
        "app",
        &[
            EnvSpec {
                env: "prod",
                marker: 201,
            },
            EnvSpec {
                env: "dev",
                marker: 202,
            },
        ],
        "gate-t2:provision-org",
        "provision-org",
    )
    .await
}

/// A **pooled** org: every env collapses onto the shared pool cluster.
async fn t3_mode(admin_url: &str) -> anyhow::Result<()> {
    let org = Org::pooled("gate-t3", "wamn-pg");
    tier_scenario(
        admin_url,
        "pooled (shared pool)",
        &org,
        "demo",
        &[EnvSpec {
            env: "dev",
            marker: 301,
        }],
        "gate-t3:provision-project-env",
        "provision-project-env",
    )
    .await
}

/// The shared per-tier scenario: provision each project-env database, prove
/// routing / per-DB isolation / least-priv / Secret layout, record the registry
/// rows, and land a provisioning saga (create → advance → complete).
async fn tier_scenario(
    admin_url: &str,
    label: &str,
    org: &Org,
    project: &str,
    envs: &[EnvSpec],
    saga_id: &str,
    saga_kind: &str,
) -> anyhow::Result<()> {
    println!(
        "== [wamn-q3n.8] provisionbench {label}: org {}, {} project-env db(s) ==",
        org.id,
        envs.len()
    );

    // Clean slate for the per-project-env databases (a prior failed run).
    drop_env_dbs(admin_url, org, project, envs).await?;

    // 1. Provision each project-env database (the plain-SQL stand-in for the CNPG
    //    Database CRD: ensure the shared role, CREATE DATABASE with the
    //    per-project-env name, confine CONNECT), seed a routing + isolation
    //    witness, and compose the app-role URL — collected into the projects-file
    //    JSON the plugin resolves.
    let mut entries = serde_json::Map::new();
    for spec in envs {
        wamn_provision::validate_project_env(&org.id, project, spec.env)
            .map_err(|e| anyhow::anyhow!("project-env name: {e}"))?;
        let db = project_env_database_name(&org.id, project, spec.env);
        provision_env_scaffold(admin_url, &db).await?;
        seed_env_witness(admin_url, &db, project, spec.env, spec.marker).await?;
        let url = app_url_for(admin_url, &db)?;
        entries.insert(db.clone(), secret::projects_file_entry(&url));
    }

    // 2. Resolve each per-project-env credential through the plugin's
    //    StaticCredentialProvider (the from_env parse path), keyed by the
    //    per-project-env db / Secret name.
    let projects_json = serde_json::to_string(&serde_json::Value::Object(entries))?;
    let base = WamnPostgresConfig::from_env();
    let projects = StaticCredentialProvider::projects_from_json(&projects_json, &base)
        .context("parse projects-file json")?;
    let provider = StaticCredentialProvider::new(projects, None);

    // 2a. Routing witness: each resolved URL reaches its own project-env database.
    let mut conns: Vec<(&'static str, Client, Conn)> = Vec::new();
    for spec in envs {
        let db = project_env_database_name(&org.id, project, spec.env);
        let cfg = provider
            .resolve(&db)?
            .with_context(|| format!("resolve {db}"))?;
        let (client, task) = connect(&cfg.database_url)
            .await
            .with_context(|| format!("connect {db}"))?;
        let got = query_marker(&client)
            .await
            .with_context(|| format!("marker {db}"))?;
        if got != spec.marker {
            bail!(
                "routing witness FAIL for {}: saw {got} (want {})",
                spec.env,
                spec.marker
            );
        }
        conns.push((spec.env, client, task));
    }
    println!(
        "  routing: {} project-env db(s) each resolve to their own marker",
        conns.len()
    );

    // 2b. Per-database isolation: no project-env's connection sees a sibling's
    //     private table (a different database, no cross-database queries).
    if conns.len() > 1 {
        for (env, client, _) in &conns {
            for (other, _, _) in &conns {
                if env != other {
                    assert_invisible(client, &private_table(project, other)).await?;
                }
            }
        }
        println!("  isolation: each project-env database is isolated from its siblings");
    } else {
        println!("  isolation: single project-env database (per-DB isolation is the orgpair mode)");
    }

    // 2c. Least privilege (read from the app connection itself).
    let (is_super, can_createdb) = role_attrs(&conns[0].1).await?;
    if is_super || can_createdb {
        bail!("least-privilege FAIL: wamn_app super={is_super} createdb={can_createdb}");
    }
    println!("  least privilege: wamn_app is NOSUPERUSER NOCREATEDB");

    for (_, client, task) in conns {
        drop(client);
        let _ = task.await;
    }

    // 2d. Credential layout: the per-project-env Secret carries the db name, URL,
    //     and the identity triple (labels).
    let triple0 = Triple::new(&org.id, project, envs[0].env);
    let db0 = project_env_database_name(&org.id, project, envs[0].env);
    let url0 = app_url_for(admin_url, &db0)?;
    let sec = render_project_env_secret_manifest(&triple0, "wamn-system", &url0);
    let ok_name = sec["metadata"]["name"] == db0;
    let ok_url = sec["stringData"]["url"] == url0;
    let ok_labels = sec["metadata"]["labels"]["wamn.org"] == org.id.as_str()
        && sec["metadata"]["labels"]["wamn.env"] == envs[0].env;
    if !ok_name || !ok_url || !ok_labels {
        bail!("secret layout FAIL: name={ok_name} url={ok_url} labels={ok_labels} ({sec})");
    }
    println!("  secret layout: {db0} carries the app-role url + identity triple");

    // 3. Registry rows: the org, its project, and a project-env row per env land
    //    in the (ephemeral) system DB.
    let (admin, admin_task) = connect(admin_url).await.context("registry connect")?;
    let org_id = org.id.as_str();
    let placement_kind = org.placement.kind_str();
    let pool = org.placement.pool();
    admin
        .execute(
            reg_sql::upsert_org_sql(),
            &[&org_id, &placement_kind, &pool],
        )
        .await
        .context("upsert org")?;
    admin
        .execute(reg_sql::upsert_project_sql(), &[&org_id, &project])
        .await
        .context("upsert project")?;
    for spec in envs {
        let db = project_env_database_name(&org.id, project, spec.env);
        let env_s = spec.env;
        let ns: Option<&str> = None;
        admin
            .execute(
                reg_sql::upsert_project_env_sql(),
                &[&org_id, &project, &env_s, &db, &ns],
            )
            .await
            .context("upsert project_env")?;
    }
    let org_rows: i64 = admin
        .query_one(
            "SELECT count(*) FROM registry.orgs WHERE id = $1",
            &[&org_id],
        )
        .await?
        .get(0);
    let env_rows: i64 = admin
        .query_one(
            "SELECT count(*) FROM registry.project_envs WHERE org = $1 AND project = $2",
            &[&org_id, &project],
        )
        .await?
        .get(0);
    if org_rows != 1 || env_rows != envs.len() as i64 {
        bail!(
            "registry rows FAIL: orgs={org_rows} project_envs={env_rows} (want 1 / {})",
            envs.len()
        );
    }
    println!("  registry: org + project + {env_rows} project-env row(s) recorded in the system DB");

    // 4. Saga: a provisioning saga for this tier lands (exactly-once), advances one
    //    durable step per env, and reaches `completed`.
    let total = envs.len() as i32;
    let total_opt = Some(total);
    admin
        .execute(
            reg_sql::create_saga_sql(),
            &[&saga_id, &saga_kind, &org_id, &total_opt],
        )
        .await
        .context("create saga")?;
    // A redelivered create is a no-op (exactly-once via the saga_id PK).
    admin
        .execute(
            reg_sql::create_saga_sql(),
            &[&saga_id, &saga_kind, &org_id, &total_opt],
        )
        .await?;
    for _ in envs {
        admin
            .execute(reg_sql::advance_saga_step_sql(), &[&saga_id])
            .await
            .context("advance saga")?;
    }
    admin
        .execute(reg_sql::complete_saga_sql(), &[&saga_id])
        .await
        .context("complete saga")?;
    let saga_count: i64 = admin
        .query_one(
            "SELECT count(*) FROM provisioning.sagas WHERE saga_id = $1",
            &[&saga_id],
        )
        .await?
        .get(0);
    let (step, status): (i32, String) = {
        let r = admin
            .query_one(
                "SELECT step, status FROM provisioning.sagas WHERE saga_id = $1",
                &[&saga_id],
            )
            .await?;
        (r.get(0), r.get(1))
    };
    if saga_count != 1 {
        bail!("saga exactly-once FAIL: {saga_count} rows for {saga_id}");
    }
    if step != total {
        bail!("saga step FAIL: {step} (want {total})");
    }
    if status != "completed" {
        bail!("saga status FAIL: {status} (want completed)");
    }
    println!("  saga: {saga_id} landed once, {step} step(s), completed");

    drop(admin);
    let _ = admin_task.await;

    // Teardown the per-project-env databases (registry/saga rows go with the
    // ephemeral schema at teardown_registry).
    drop_env_dbs(admin_url, org, project, envs).await?;
    println!("  {label}: PASS");
    Ok(())
}

/// Provision one per-project-env database as superuser scaffolding: ensure the
/// shared role, `CREATE DATABASE` (the per-project-env name), confine CONNECT.
async fn provision_env_scaffold(admin_url: &str, db: &str) -> anyhow::Result<()> {
    let (client, task) = connect(admin_url).await.context("admin connect")?;
    client
        .batch_execute(&sql::ensure_app_role_sql(APP_PASSWORD))
        .await
        .context("ensure wamn_app role")?;
    client
        .batch_execute(&sql::create_database_named_sql(db))
        .await
        .with_context(|| format!("create database {db}"))?;
    client
        .batch_execute(&sql::grant_connect_on_database_sql(db))
        .await
        .with_context(|| format!("confine CONNECT on {db}"))?;
    drop(client);
    let _ = task.await;
    Ok(())
}

/// Seed a routing marker + a per-env private table into a project-env database.
async fn seed_env_witness(
    admin_url: &str,
    db: &str,
    project: &str,
    env: &str,
    marker: i32,
) -> anyhow::Result<()> {
    let db_url = swap_db(admin_url, db)?;
    let (client, task) = connect(&db_url)
        .await
        .with_context(|| format!("connect {db}"))?;
    let private = private_table(project, env);
    client
        .batch_execute(&format!(
            "CREATE TABLE marker (n int NOT NULL); \
             INSERT INTO marker VALUES ({marker}); \
             GRANT SELECT ON marker TO wamn_app; \
             CREATE TABLE {private} (id int); \
             GRANT SELECT ON {private} TO wamn_app;"
        ))
        .await
        .with_context(|| format!("seed witness {db}"))?;
    drop(client);
    let _ = task.await;
    Ok(())
}

/// Drop each project-env database (teardown / clean slate). Autocommit.
async fn drop_env_dbs(
    admin_url: &str,
    org: &Org,
    project: &str,
    envs: &[EnvSpec],
) -> anyhow::Result<()> {
    let (client, task) = connect(admin_url).await.context("admin connect")?;
    for spec in envs {
        let db = project_env_database_name(&org.id, project, spec.env);
        client
            .batch_execute(&sql::drop_database_named_sql(&db))
            .await
            .with_context(|| format!("drop {db}"))?;
    }
    drop(client);
    let _ = task.await;
    Ok(())
}

/// The per-env private table name (distinct per env so a sibling's connection can
/// prove it invisible across databases).
fn private_table(project: &str, env: &str) -> String {
    format!("only_in_{}_{}", project.replace('-', "_"), env)
}

/// Compose the app-role connection URL for a project-env database, reusing the
/// superuser URL's host/port (the app role reaches the same cluster).
fn app_url_for(admin_url: &str, db: &str) -> anyhow::Result<String> {
    let config: tokio_postgres::Config = admin_url.parse().context("parse admin database url")?;
    let host = config
        .get_hosts()
        .iter()
        .find_map(|h| match h {
            tokio_postgres::config::Host::Tcp(h) => Some(h.clone()),
            _ => None,
        })
        .context("admin url has no TCP host")?;
    let port = config.get_ports().first().copied().unwrap_or(5432);
    Ok(compose_url(APP_ROLE, APP_PASSWORD, &host, port, db))
}

// ============================================================================
// saga mode (wamn-q3n.8) — the builders in isolation
// ============================================================================

async fn saga_mode(admin_url: &str) -> anyhow::Result<()> {
    println!("== [wamn-q3n.8] provisionbench saga: exactly-once / step / complete / fail ==");
    let (client, task) = connect(admin_url).await.context("saga connect")?;

    // Exactly-once create: a redelivered create of the same saga_id is a no-op.
    let sid = "gate-saga-1";
    let kind = "provision-org";
    let target = "gate-org";
    let total = Some(2i32);
    client
        .execute(reg_sql::create_saga_sql(), &[&sid, &kind, &target, &total])
        .await?;
    client
        .execute(reg_sql::create_saga_sql(), &[&sid, &kind, &target, &total])
        .await?;
    let n: i64 = client
        .query_one(
            "SELECT count(*) FROM provisioning.sagas WHERE saga_id = $1",
            &[&sid],
        )
        .await?
        .get(0);
    if n != 1 {
        bail!("saga exactly-once FAIL: {n} rows for {sid}");
    }

    // Durable step checkpoint: two advances → 2.
    client
        .execute(reg_sql::advance_saga_step_sql(), &[&sid])
        .await?;
    client
        .execute(reg_sql::advance_saga_step_sql(), &[&sid])
        .await?;
    let step: i32 = client
        .query_one(
            "SELECT step FROM provisioning.sagas WHERE saga_id = $1",
            &[&sid],
        )
        .await?
        .get(0);
    if step != 2 {
        bail!("saga step FAIL: {step} (want 2)");
    }

    // Terminal complete.
    client
        .execute(reg_sql::complete_saga_sql(), &[&sid])
        .await?;
    let status: String = client
        .query_one(
            "SELECT status FROM provisioning.sagas WHERE saga_id = $1",
            &[&sid],
        )
        .await?
        .get(0);
    if status != "completed" {
        bail!("saga complete FAIL: {status} (want completed)");
    }

    // Terminal fail (a second saga) records the error.
    let sid2 = "gate-saga-2";
    let none: Option<i32> = None;
    client
        .execute(
            reg_sql::create_saga_sql(),
            &[&sid2, &"provision-project-env", &"gate-org/app/dev", &none],
        )
        .await?;
    client
        .execute(reg_sql::fail_saga_sql(), &[&sid2, &"boom"])
        .await?;
    let (fstatus, ferr): (String, Option<String>) = {
        let r = client
            .query_one(
                "SELECT status, last_error FROM provisioning.sagas WHERE saga_id = $1",
                &[&sid2],
            )
            .await?;
        (r.get(0), r.get(1))
    };
    if fstatus != "failed" || ferr.as_deref() != Some("boom") {
        bail!("saga fail FAIL: status={fstatus} err={ferr:?}");
    }

    println!("  saga: exactly-once create, step→2, complete, fail(+error) all hold");
    drop(client);
    let _ = task.await;
    Ok(())
}

// ============================================================================
// ephemeral registry schema (wamn-q3n.8) — the T1 DDL on the throwaway/pool PG
// ============================================================================

/// Apply the canonical registry DDL into an ephemeral pair of schemas (owned by a
/// `wamn_system` role via `AUTHORIZATION`, the way the T1 wamn_system DB carries
/// it), dropping any prior copy first. Applied as superuser (the admin URL).
async fn setup_registry(admin_url: &str) -> anyhow::Result<()> {
    let (client, task) = connect(admin_url).await.context("registry admin connect")?;
    client
        .batch_execute(
            "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_system') THEN \
               CREATE ROLE wamn_system LOGIN PASSWORD 'wamn_system' NOSUPERUSER; \
             END IF; END $$; \
             DROP SCHEMA IF EXISTS registry CASCADE; \
             DROP SCHEMA IF EXISTS provisioning CASCADE;",
        )
        .await
        .context("prepare wamn_system role + drop prior registry schemas")?;
    client
        .batch_execute(SYSTEM_SCHEMA_SQL)
        .await
        .context("apply system-schema.sql")?;
    drop(client);
    let _ = task.await;
    Ok(())
}

/// Drop the ephemeral registry schemas (self-contained teardown).
async fn teardown_registry(admin_url: &str) -> anyhow::Result<()> {
    let (client, task) = connect(admin_url).await.context("registry admin connect")?;
    client
        .batch_execute(
            "DROP SCHEMA IF EXISTS registry CASCADE; DROP SCHEMA IF EXISTS provisioning CASCADE;",
        )
        .await
        .context("drop ephemeral registry schemas")?;
    drop(client);
    let _ = task.await;
    Ok(())
}

// ============================================================================
// shared helpers
// ============================================================================

async fn query_marker(client: &Client) -> anyhow::Result<i32> {
    Ok(client.query_one("SELECT n FROM marker", &[]).await?.get(0))
}

/// Read the current role's `rolsuper` / `rolcreatedb` from the app connection.
async fn role_attrs(client: &Client) -> anyhow::Result<(bool, bool)> {
    let row = client
        .query_one(
            "SELECT rolsuper, rolcreatedb FROM pg_roles WHERE rolname = current_user",
            &[],
        )
        .await
        .context("read role attributes")?;
    Ok((row.get(0), row.get(1)))
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
