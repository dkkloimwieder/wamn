//! Live test of `delete-project-env` (`wamn-psss`,
//! docs/plan/environment-teardown.md section 5, issue 2).
//!
//! The test starts its own PostgreSQL 18 server with `wal_level=logical` and
//! `cluster_name` set to the pool cluster, as CloudNativePG sets it. Set
//! `WAMN_READER_NATS_URL` to a throwaway JetStream-enabled NATS. The test
//! provisions a triple, enables CDC, prepares generation `a` of one
//! project-environment family and of one control family, and puts fixture rows
//! in every control table of the delete order. It then runs the verb, provisions
//! the triple again, and inserts the same fixture rows with the same command id.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::time::Duration;

use async_nats::jetstream;
use tokio::task::JoinHandle;
use tokio_postgres::{Client, NoTls};

use wamn_control::delete_project_env::{
    CONTROL_TABLES, DeleteProjectEnvRequest, delete_project_env,
};
use wamn_control::enable_cdc_project_env::EnableCdcProjectEnvRequest;
use wamn_control::pat_client::PatIssuerConfig;
use wamn_control::provision_org::{ProvisionOrgRequest, provision_org};
use wamn_control::provision_project_env::{
    ProvisionProjectEnvRequest, provision_project_env, run_workload_action, secret_value,
};
use wamn_control_provision::events::materializer_consumer_config;
use wamn_control_provision::workload_role::WorkloadRoleFamily;
use wamn_control_provision::{cdc_object_name, event_stream_name, sql};
use wamn_gate_harness::environment::{apply_project_database, configure_cdc};

const ORG: &str = "td0";
const PROJECT: &str = "app";
const ENV: &str = "dev";
const TENANT: &str = "td0-tenant";
const CLUSTER: &str = "td0-pg18";
const SCHEMA: &str = "app";
const SHA_A: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SHA_B: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

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

/// Close a connection and wait until the server session is gone.
async fn close((client, task): (Client, JoinHandle<()>)) {
    drop(client);
    task.await.expect("connection task");
}

fn database_url(admin_url: &str, database: &str) -> String {
    let mut url = url::Url::parse(admin_url).expect("admin URL");
    url.set_path(&format!("/{database}"));
    url.into()
}

async fn install_control(admin_url: &str, system_url: &str) {
    let admin = connect(admin_url).await;
    admin
        .0
        .batch_execute(
            "CREATE ROLE wamn_system NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
             NOREPLICATION NOBYPASSRLS",
        )
        .await
        .expect("wamn_system role");
    admin
        .0
        .batch_execute(&sql::ensure_control_author_acl_role_sql())
        .await
        .expect("control author role");
    admin
        .0
        .batch_execute(sql::ensure_db_owner_role_sql())
        .await
        .expect("database owner role");
    admin
        .0
        .batch_execute("CREATE DATABASE wamn_system OWNER wamn_system")
        .await
        .expect("control database");
    close(admin).await;
    let system = connect(system_url).await;
    system
        .0
        .batch_execute("SET ROLE wamn_system")
        .await
        .unwrap();
    system
        .0
        .batch_execute(wamn_control_provision::SYSTEM_SCHEMA_SQL)
        .await
        .expect("system schema");
    system
        .0
        .batch_execute(wamn_control_provision::CONTROL_PORTABLE_STORE_SQL)
        .await
        .expect("control store");
    system
        .0
        .execute(
            "UPDATE registry.meta SET platform_domain = $1",
            &[&"example.invalid"],
        )
        .await
        .expect("platform domain");
    system.0.batch_execute("RESET ROLE").await.unwrap();
    system
        .0
        .batch_execute(sql::revoke_public_connect_floor_sql())
        .await
        .expect("revoke PUBLIC CONNECT");
    system
        .0
        .batch_execute(
            "DO $$ BEGIN EXECUTE format('REVOKE TEMPORARY ON DATABASE %I FROM PUBLIC', \
             current_database()); END $$;",
        )
        .await
        .expect("revoke PUBLIC TEMPORARY");
    close(system).await;
}

/// Provision the triple and create its database. Returns the instance and the
/// database URL.
async fn provision(work: &Path, admin_url: &str, system_url: &str) -> (String, String) {
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
    // The dev loop's order: the emitted roles and the catalog author role come
    // before the database.
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
    wamn_control::dev::environment::install_journey_platform_floor(
        &project.0,
        TENANT,
        "example.invalid",
    )
    .await
    .expect("install the platform floor");
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
    (outcome.instance, url)
}

/// Prepare generation `a` of one family and return the role it names.
async fn prepare(
    family: WorkloadRoleFamily,
    system_url: &str,
    target: Option<&str>,
    secret: &Path,
) -> String {
    let mut args =
        wamn_control::dev::environment::generation_args(family, system_url, target, secret);
    args.org = ORG.into();
    args.project = PROJECT.into();
    args.env = ENV.into();
    args.tenant = Some(TENANT.into());
    run_workload_action(&args)
        .await
        .unwrap_or_else(|error| panic!("prepare {family:?} a: {error:#}"));
    let url = secret_value(secret, "url").expect("the Secret carries its URL");
    url::Url::parse(&url).unwrap().username().to_owned()
}

/// One row in each control table of the delete order. The same rows go in
/// before the verb and after the new provision.
async fn seed_control_rows(system_url: &str) {
    let system = connect(system_url).await;
    system
        .0
        .batch_execute(&format!(
            "INSERT INTO catalog.packages VALUES ('{TENANT}', 'td0pkg', '1.0.0', NULL, '{SHA_A}');
             INSERT INTO catalog.package_migrations VALUES
               ('{TENANT}', 'td0pkg', '1.0.0', 1, 'migrations/0001_initial.sql', '{SHA_A}');
             INSERT INTO catalog.component_digest_owners
               (tenant_id, environment_instance, component_digest, package_id)
             VALUES ('{TENANT}', 'fixture', '{SHA_B}', 'td0pkg');
             INSERT INTO catalog.component_library
               (tenant_id, environment_instance, package_id, package_version, component,
                interface_version, operations, component_digest, projection_hash, imports,
                imports_fingerprint, effects)
             VALUES ('{TENANT}', 'fixture', 'td0pkg', '1.0.0', 'main', '0.1.0',
                     '{{\"op\": {{}}}}', '{SHA_B}', '{SHA_A}', '[]', '{SHA_A}', '[]');
             INSERT INTO catalog.connection_requirements VALUES
               ('{TENANT}', 'fixture', '{SHA_B}', 'main', '{{}}', '{SHA_A}');
             INSERT INTO catalog.effective_releases (tenant_id, effective_release_id, environment)
               VALUES ('{TENANT}', 1, '{ENV}');
             INSERT INTO catalog.effective_release_packages VALUES ('{TENANT}', 1, 'td0pkg', '1.0.0');
             INSERT INTO catalog.effective_release_heads (tenant_id, environment, effective_release_id)
               VALUES ('{TENANT}', '{ENV}', 1);
             INSERT INTO catalog.deployment_attestations VALUES
               ('{TENANT}', 'fixture', 1, '{ORG}', '{PROJECT}', '{ENV}', '{SHA_A}', NULL, now());
             INSERT INTO catalog.authoring_command_audit
               (tenant_id, command_id, command_type, principal_id, principal_type,
                principal_subject, effective_role, org, project, environment, target_ref,
                request_hash, outcome_bytes)
             VALUES ('{TENANT}', 'gate-fixture', 'gate', 'principal', 'service', 'subject',
                     'project-author', '{ORG}', '{PROJECT}', '{ENV}', 'target', '{SHA_A}', '\\x01');
             INSERT INTO wamn_run.gate_reports (tenant_id, wiring_hash, passed, summary)
               VALUES ('{TENANT}', '{SHA_A}', true, '{{}}');"
        ))
        .await
        .expect("insert the control fixture rows");
    close(system).await;
}

async fn count(client: &Client, statement: &str, value: &str) -> i64 {
    client
        .query_one(statement, &[&value])
        .await
        .unwrap_or_else(|error| panic!("{statement}: {error}"))
        .get(0)
}

fn request(
    system_url: &str,
    admin_url: &str,
    nats_url: &str,
    password: &Path,
    confirm: bool,
) -> DeleteProjectEnvRequest {
    DeleteProjectEnvRequest {
        org: ORG.into(),
        project: PROJECT.into(),
        env: ENV.into(),
        system_database_url: system_url.into(),
        admin_database_url: admin_url.into(),
        nats_url: nats_url.into(),
        nats_username: "provisioning".into(),
        nats_password_file: password.into(),
        confirm,
    }
}

fn refusal(result: anyhow::Result<()>) -> String {
    format!("{:#}", result.expect_err("the verb must refuse"))
}

#[tokio::test]
#[ignore = "requires: WAMN_READER_NATS_URL"]
async fn delete_project_env_frees_the_triple_for_a_new_instance() {
    wamn_test_postgres::require_prerequisites(&["WAMN_READER_NATS_URL"]);
    let nats_url = std::env::var("WAMN_READER_NATS_URL").expect("WAMN_READER_NATS_URL");
    let postgres =
        wamn_test_postgres::start(&[("wal_level", "logical"), ("cluster_name", CLUSTER)])
            .expect("start a PostgreSQL 18 server");
    let admin_url = postgres.database("postgres").unwrap().url().to_owned();
    let system_url = postgres.database("wamn_system").unwrap().url().to_owned();
    // A private directory of this run, removed when the test ends or fails.
    let guard =
        WorkDirectory(std::env::temp_dir().join(format!("wamn-psss-live-{}", std::process::id())));
    let work = guard.0.clone();
    fs::create_dir(&work).expect("work directory");
    fs::set_permissions(&work, fs::Permissions::from_mode(0o700)).unwrap();
    let password = work.join("nats-password");
    fs::write(&password, "unused").unwrap();
    fs::set_permissions(&password, fs::Permissions::from_mode(0o600)).unwrap();
    let stream = event_stream_name(ORG, PROJECT, ENV);
    let advisory = wamn_event_wire::delivery_advisory_stream(&stream);
    let js = jetstream::new(async_nats::connect(&nats_url).await.expect("connect NATS"));
    let _ = js.delete_stream(&stream).await;
    let _ = js.delete_stream(&advisory).await;

    install_control(&admin_url, &system_url).await;
    provision_org(ProvisionOrgRequest {
        org: ORG.into(),
        template: wamn_control_registry::Template::trials(),
        pool: CLUSTER.into(),
        cluster_namespace: "platform".into(),
        system_database_url: Some(system_url.clone()),
    })
    .await
    .expect("provision the org");
    let first = work.join("first");
    fs::create_dir(&first).unwrap();
    let (old_instance, project_url) = provision(&first, &admin_url, &system_url).await;

    let host = url::Url::parse(&admin_url).unwrap();
    let consumer = materializer_consumer_config(
        "td0_materializer",
        &format!("evt.{ORG}.{PROJECT}.{ENV}.>"),
        Duration::from_secs(30),
        5,
    );
    let cluster = connect(&admin_url).await;
    let project = connect(&project_url).await;
    configure_cdc(
        EnableCdcProjectEnvRequest {
            org: ORG.into(),
            project: PROJECT.into(),
            env: ENV.into(),
            schema: SCHEMA.into(),
            system_database_url: Some(system_url.clone()),
            cluster: Some(CLUSTER.into()),
            replication_password: "td0-replication".into(),
            db_host: host.host_str().map(str::to_owned),
            db_port: host.port().unwrap_or(5432),
            namespace: "platform".into(),
            secret_namespace: Some("platform".into()),
            stream: None,
            nats_url: nats_url.clone(),
            nats_username: "provisioning".into(),
            nats_password_file: password.clone(),
            stream_replicas: 1,
            dup_window_secs: 120,
            consumer_config: vec![serde_json::to_string(&consumer).unwrap()],
            emit_role_sql: Some(first.join("cdc-role.sql")),
            emit_cdc_sql: Some(first.join("cdc.sql")),
            emit_secret: Some(first.join("cdc.json")),
        },
        &cluster.0,
        &project.0,
    )
    .await
    .expect("enable CDC");
    close(project).await;

    let instance_role = prepare(
        WorkloadRoleFamily::ExecutorPlatform,
        &system_url,
        Some(&project_url),
        &first.join("executor-platform.json"),
    )
    .await;
    let control_role = prepare(
        WorkloadRoleFamily::RegistryReader,
        &system_url,
        None,
        &first.join("registry-reader.json"),
    )
    .await;
    seed_control_rows(&system_url).await;

    let slot = cdc_object_name(ORG, PROJECT, ENV, &old_instance);
    let database = url::Url::parse(&project_url).unwrap().path()[1..].to_owned();
    let role_count = "SELECT count(*) FROM pg_roles WHERE rolname = $1";
    for role in [&slot, &instance_role, &control_role] {
        assert_eq!(
            count(&cluster.0, role_count, role).await,
            1,
            "{role} exists"
        );
    }
    js.get_stream(&stream)
        .await
        .expect("the source stream exists");
    js.get_stream(&advisory)
        .await
        .expect("the advisory stream exists");

    // The plan alone changes nothing.
    delete_project_env(&request(
        &system_url,
        &admin_url,
        &nats_url,
        &password,
        false,
    ))
    .await
    .expect("the plan runs");
    assert_eq!(
        count(
            &cluster.0,
            "SELECT count(*) FROM pg_database WHERE datname = $1",
            &database
        )
        .await,
        1
    );

    // A session on the database refuses, and the refusal names its login.
    let session = connect(&project_url).await;
    let login = url::Url::parse(&project_url).unwrap().username().to_owned();
    let refused = refusal(
        delete_project_env(&request(
            &system_url,
            &admin_url,
            &nats_url,
            &password,
            true,
        ))
        .await,
    );
    assert!(
        refused.contains("has sessions of") && refused.contains(&login),
        "{refused}"
    );
    close(session).await;

    // An admin URL that is not the postgres database refuses.
    let refused = refusal(
        delete_project_env(&request(
            &system_url,
            &system_url,
            &nats_url,
            &password,
            true,
        ))
        .await,
    );
    assert!(refused.contains("not postgres"), "{refused}");

    delete_project_env(&request(
        &system_url,
        &admin_url,
        &nats_url,
        &password,
        true,
    ))
    .await
    .expect("delete the environment");

    assert_eq!(
        count(
            &cluster.0,
            "SELECT count(*) FROM pg_database WHERE datname = $1",
            &database
        )
        .await,
        0,
        "the database is gone"
    );
    assert_eq!(
        count(
            &cluster.0,
            "SELECT count(*) FROM pg_replication_slots WHERE slot_name = $1",
            &slot
        )
        .await,
        0,
        "the slot is gone"
    );
    assert_eq!(
        count(&cluster.0, role_count, &slot).await,
        0,
        "the CDC role is gone"
    );
    assert_eq!(
        count(&cluster.0, role_count, &instance_role).await,
        0,
        "the instance role is gone"
    );
    assert_eq!(
        count(&cluster.0, role_count, &control_role).await,
        1,
        "the control role stays"
    );
    assert!(
        js.get_stream(&stream).await.is_err(),
        "the source stream is gone"
    );
    assert!(
        js.get_stream(&advisory).await.is_err(),
        "the advisory stream is gone"
    );

    let system = connect(&system_url).await;
    for (table, _) in CONTROL_TABLES {
        let statement = format!("SELECT count(*) FROM {table} WHERE tenant_id = $1");
        assert_eq!(
            count(&system.0, &statement, TENANT).await,
            0,
            "{table} is empty"
        );
    }
    assert_eq!(
        count(
            &system.0,
            "SELECT count(*) FROM registry.project_envs WHERE org = $1",
            ORG
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &system.0,
            "SELECT count(*) FROM catalog.tenant_environments WHERE tenant_id = $1",
            TENANT
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &system.0,
            "SELECT count(*) FROM registry.retired_project_envs WHERE instance_suffix = $1",
            &old_instance
        )
        .await,
        1,
        "the retired table records the old suffix"
    );
    close(system).await;

    let refused = refusal(
        delete_project_env(&request(
            &system_url,
            &admin_url,
            &nats_url,
            &password,
            true,
        ))
        .await,
    );
    assert!(
        refused.contains("no registry.project_envs row"),
        "{refused}"
    );

    let second = work.join("second");
    fs::create_dir(&second).unwrap();
    let (new_instance, _) = provision(&second, &admin_url, &system_url).await;
    assert_ne!(
        new_instance, old_instance,
        "the new provision mints a new suffix"
    );
    // The same rows with the same command id go in again.
    seed_control_rows(&system_url).await;
    close(cluster).await;
    drop(guard);
}
