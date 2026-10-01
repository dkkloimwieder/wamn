//! Live test of `upgrade-schema` (`wamn-o8b9`, docs/plan/schema-upgrade.md
//! section 5, issue 2).
//!
//! The test starts its own PostgreSQL 18 server. It installs the control store
//! with `provision-system`'s installer and one project database with
//! `reconcile-run-plane`, and checks that both fresh installs record every
//! migration of the binary. It then runs the verb with its own probe files: the
//! plan, one apply, a second run with nothing pending, an edited applied file, a
//! failing file, the baseline of a database installed before its record table,
//! and the refusals of the wrong database.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use tokio::task::JoinHandle;
use tokio_postgres::{Client, NoTls};

use wamn_control::pat_client::PatIssuerConfig;
use wamn_control::provision_org::{ProvisionOrgRequest, provision_org};
use wamn_control::provision_project_env::{ProvisionProjectEnvRequest, provision_project_env};
use wamn_control::upgrade_schema::{UpgradeSchemaRequest, upgrade_schema_with};
use wamn_control_provision::schema_migrations::{Migration, MigrationTarget};
use wamn_gate_harness::environment::apply_project_database;

const ORG: &str = "us0";
const PROJECT: &str = "app";
const ENV: &str = "dev";
const TENANT: &str = "us0-tenant";
const CLUSTER: &str = "us0-pg18";

struct WorkDirectory(std::path::PathBuf);

impl Drop for WorkDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

async fn connect(url: &str) -> (Client, JoinHandle<()>) {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect the test server");
    let task = tokio::spawn(async move {
        let _ = connection.await;
    });
    (client, task)
}

async fn close((client, task): (Client, JoinHandle<()>)) {
    drop(client);
    task.await.expect("connection task");
}

fn database_url(admin_url: &str, database: &str) -> String {
    let mut url = url::Url::parse(admin_url).expect("admin URL");
    url.set_path(&format!("/{database}"));
    url.into()
}

/// A probe file with the next ordinal after `before`.
fn probe(target: &str, before: usize, name: &str, sql: &str) -> Migration {
    Migration {
        relative_path: Box::leak(
            format!("migrations/{target}/{:04}_{name}.sql", before + 1).into_boxed_str(),
        ),
        sql: Box::leak(sql.to_owned().into_boxed_str()),
    }
}

fn request(
    system_url: &str,
    admin_url: Option<&str>,
    baseline: Option<i32>,
    confirm: bool,
) -> UpgradeSchemaRequest {
    UpgradeSchemaRequest {
        system_database_url: system_url.into(),
        admin_database_url: admin_url.map(str::to_owned),
        baseline,
        confirm,
    }
}

fn refusal(result: anyhow::Result<()>) -> String {
    format!("{:#}", result.expect_err("the verb must refuse"))
}

async fn scalar(url: &str, statement: &str) -> i64 {
    let client = connect(url).await;
    let value = client
        .0
        .query_one(statement, &[])
        .await
        .unwrap_or_else(|error| panic!("{statement}: {error}"))
        .get(0);
    close(client).await;
    value
}

async fn install_control(admin_url: &str, system_url: &str) {
    let admin = connect(admin_url).await;
    for statement in [
        "CREATE ROLE wamn_system NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
         NOREPLICATION NOBYPASSRLS",
        "CREATE DATABASE wamn_system OWNER wamn_system",
    ] {
        admin
            .0
            .batch_execute(statement)
            .await
            .expect("control role and database");
    }
    close(admin).await;
    let system = connect(system_url).await;
    wamn_control::provision_system::install_control_store(&system.0)
        .await
        .expect("install the control store");
    system
        .0
        .execute(
            "UPDATE registry.meta SET platform_domain = $1",
            &[&"example.invalid"],
        )
        .await
        .expect("platform domain");
    close(system).await;
}

/// Provision the triple and install its database the way the operations page
/// does: the catalog, then `reconcile-run-plane`. Returns the database URL.
async fn provision(work: &Path, admin_url: &str, system_url: &str) -> String {
    provision_org(ProvisionOrgRequest {
        org: ORG.into(),
        template: wamn_control_registry::Template::trials(),
        pool: CLUSTER.into(),
        cluster_namespace: "platform".into(),
        owner_email: None,
        system_database_url: Some(system_url.into()),
    })
    .await
    .expect("provision the org");
    let outcome = provision_project_env(&ProvisionProjectEnvRequest {
        org: ORG.into(),
        project: PROJECT.into(),
        env: ENV.into(),
        tenant: Some(TENANT.into()),
        disposable: false,
        system_database_url: Some(system_url.into()),
        cluster: Some(CLUSTER.into()),
        connection_limit: None,
        cluster_namespace: "platform".into(),
        namespace: "hosts".into(),
        secret_namespace: Some("hosts".into()),
        emit_database: Some(work.join("database.json")),
        emit_role_sql: Some(work.join("roles.sql")),
        emit_privilege_sql: Some(work.join("privileges.sql")),
        emit_secret: Some(work.join("project-db.json")),
        pat_issuer: PatIssuerConfig::default(),
        emit_management_author_pat_secret: None,
        emit_operator_pat_secret: None,
    })
    .await
    .expect("provision the project environment");
    let url = database_url(admin_url, &outcome.database);
    let admin = connect(admin_url).await;
    admin
        .0
        .batch_execute(&fs::read_to_string(work.join("roles.sql")).unwrap())
        .await
        .expect("apply the emitted role SQL");
    admin
        .0
        .batch_execute(wamn_schema_control::ensure_scenario_author_role_sql())
        .await
        .expect("ensure the catalog author role");
    apply_project_database(&admin.0, &url, &work.join("privileges.sql"))
        .await
        .expect("create the project database");
    close(admin).await;
    let project = connect(&url).await;
    project
        .0
        .batch_execute(wamn_catalog::CATALOG_SCHEMA_SQL)
        .await
        .expect("install the catalog schema");
    close(project).await;
    wamn_control::reconcile_run_plane::reconcile_run_plane(
        wamn_control::reconcile_run_plane::ReconcileRunPlaneRequest {
            system_database_url: system_url.into(),
            admin_database_url: url.clone(),
            org: ORG.into(),
            project: PROJECT.into(),
            tenant: TENANT.into(),
            env: ENV.into(),
            schema: "wamn_run".into(),
            dry_run: false,
        },
    )
    .await
    .expect("reconcile the run plane");
    url
}

#[tokio::test]
async fn upgrade_schema_applies_each_pending_file_once() {
    let postgres = wamn_test_postgres::start(&[]).expect("start a PostgreSQL 18 server");
    let admin_url = postgres.database("postgres").unwrap().url().to_owned();
    let system_url = postgres.database("wamn_system").unwrap().url().to_owned();
    let guard =
        WorkDirectory(std::env::temp_dir().join(format!("wamn-o8b9-live-{}", std::process::id())));
    let work = guard.0.clone();
    fs::create_dir(&work).expect("work directory");
    fs::set_permissions(&work, fs::Permissions::from_mode(0o700)).unwrap();

    // Fresh installs record every file of the binary.
    install_control(&admin_url, &system_url).await;
    let project_url = provision(&work, &admin_url, &system_url).await;
    let system_files = MigrationTarget::System.migrations();
    let project_files = MigrationTarget::Project.migrations();
    assert_eq!(
        scalar(
            &system_url,
            "SELECT count(*) FROM registry.schema_migrations"
        )
        .await,
        i64::try_from(system_files.len()).unwrap()
    );
    assert_eq!(
        scalar(
            &project_url,
            "SELECT count(*) FROM app_system.schema_migrations"
        )
        .await,
        i64::try_from(project_files.len()).unwrap()
    );

    // The system database: plan, apply, then nothing pending.
    let one = probe(
        "system",
        system_files.len(),
        "probe_one",
        "CREATE TABLE registry.upgrade_probe_one (x integer);",
    );
    let mut files = system_files.to_vec();
    files.push(one);
    upgrade_schema_with(&request(&system_url, None, None, false), &files)
        .await
        .expect("plan");
    assert_eq!(
        scalar(
            &system_url,
            "SELECT count(*) FROM pg_class WHERE relname = 'upgrade_probe_one'"
        )
        .await,
        0,
        "the plan changes nothing"
    );
    upgrade_schema_with(&request(&system_url, None, None, true), &files)
        .await
        .expect("apply");
    assert_eq!(
        scalar(
            &system_url,
            "SELECT count(*) FROM pg_class c JOIN pg_roles r ON r.oid = c.relowner \
             WHERE c.relname = 'upgrade_probe_one' AND r.rolname = 'wamn_system'"
        )
        .await,
        1,
        "the file runs as wamn_system"
    );
    let recorded = scalar(
        &system_url,
        "SELECT count(*) FROM registry.schema_migrations",
    )
    .await;
    assert_eq!(recorded, i64::try_from(files.len()).unwrap());
    upgrade_schema_with(&request(&system_url, None, None, true), &files)
        .await
        .expect("second run");
    assert_eq!(
        scalar(
            &system_url,
            "SELECT count(*) FROM registry.schema_migrations"
        )
        .await,
        recorded,
        "a second run records nothing"
    );

    // An edited applied file refuses.
    let mut edited = files.clone();
    edited[system_files.len()] = probe(
        "system",
        system_files.len(),
        "probe_one",
        "CREATE TABLE registry.upgrade_probe_one (x bigint);",
    );
    assert!(
        refusal(upgrade_schema_with(&request(&system_url, None, None, true), &edited).await)
            .contains("schema-migration-drift")
    );

    // A failing file leaves no change and no record.
    let mut failing = files.clone();
    failing.push(probe(
        "system",
        files.len(),
        "probe_two",
        "CREATE TABLE registry.upgrade_probe_two (x integer); SELECT 1 / 0;",
    ));
    assert!(
        refusal(upgrade_schema_with(&request(&system_url, None, None, true), &failing).await)
            .contains("probe_two")
    );
    assert_eq!(
        scalar(
            &system_url,
            "SELECT count(*) FROM pg_class WHERE relname = 'upgrade_probe_two'"
        )
        .await,
        0
    );
    assert_eq!(
        scalar(
            &system_url,
            "SELECT count(*) FROM registry.schema_migrations"
        )
        .await,
        recorded
    );

    // --baseline once the record table has a row, and the wrong database.
    assert!(
        refusal(upgrade_schema_with(&request(&system_url, None, Some(0), true), &files).await)
            .contains("--baseline refused")
    );
    assert!(
        refusal(upgrade_schema_with(&request(&admin_url, None, None, true), &files).await)
            .contains("not wamn_system")
    );
    assert!(
        refusal(
            upgrade_schema_with(
                &request(&system_url, Some(&admin_url), None, true),
                project_files
            )
            .await
        )
        .contains("is not a project environment")
    );

    // A project database installed before its record table.
    let client = connect(&project_url).await;
    client
        .0
        .batch_execute("DROP TABLE app_system.schema_migrations")
        .await
        .expect("drop the record table");
    close(client).await;
    let mut files = project_files.to_vec();
    files.push(probe(
        "project",
        project_files.len(),
        "probe_one",
        "CREATE TABLE app_system.upgrade_probe_one (x integer);",
    ));
    assert!(
        refusal(
            upgrade_schema_with(
                &request(&system_url, Some(&project_url), None, true),
                &files
            )
            .await
        )
        .contains("--baseline")
    );
    let held = i32::try_from(project_files.len()).unwrap();
    upgrade_schema_with(
        &request(&system_url, Some(&project_url), Some(held), true),
        &files,
    )
    .await
    .expect("baseline, then apply");
    assert_eq!(
        scalar(
            &project_url,
            "SELECT count(*) FROM app_system.schema_migrations"
        )
        .await,
        i64::try_from(files.len()).unwrap()
    );
    assert_eq!(
        scalar(
            &project_url,
            "SELECT count(*) FROM pg_class WHERE relname = 'upgrade_probe_one'"
        )
        .await,
        1
    );
    drop(guard);
}
