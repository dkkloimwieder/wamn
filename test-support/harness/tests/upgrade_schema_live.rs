//! Live test of `upgrade-schema` (`wamn-o8b9`, docs/history/schema-upgrade.md
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
use wamn_platform_identity::auth_changes::AuthChanges;

use tokio::task::JoinHandle;
use tokio_postgres::{Client, NoTls};

use wamn_control::pat_client::PatIssuerConfig;
use wamn_control::provision_org::{OrgRecord, ProvisionOrgRequest, provision_org};
use wamn_control::provision_project_env::{ProvisionProjectEnvRequest, provision_project_env};
use wamn_control::upgrade_schema::{UpgradeSchemaRequest, upgrade_schema_with};
use wamn_control_provision::schema_migrations::{Migration, MigrationTarget};
use wamn_gate_harness::environment::apply_project_database;

const ORG_BOUNDARY: &str =
    include_str!("../../../crates/control/lib/tests/fixtures/org-boundary-before-0031.sql");
const ADMINISTRATION_BOUNDARY: &str = include_str!("fixtures/administration-before-0038.sql");
const PLURAL_USER: &str =
    include_str!("../../../crates/control/lib/tests/fixtures/platform-user-before-0027.sql");
const PLURAL_TABLES: &str = include_str!(
    "../../../crates/control/lib/tests/fixtures/platform-singular-tables-before-0028.sql"
);
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

/// The shape of schema `platform`: each column, constraint, index and policy,
/// and the row security of each table, by name and not by position.
async fn platform_shape(url: &str) -> Vec<String> {
    let client = connect(url).await;
    let shape = client
        .0
        .query(
            "SELECT entry FROM ( \
               SELECT 'column ' || c.relname || '.' || a.attname || ' ' \
                      || format_type(a.atttypid, a.atttypmod) || ' ' || a.attnotnull AS entry \
                 FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid \
                WHERE c.relnamespace = 'platform'::regnamespace AND c.relkind = 'r' \
                  AND a.attnum > 0 AND NOT a.attisdropped \
               UNION ALL \
               SELECT 'constraint ' || c.relname || '.' || k.conname || ' ' \
                      || pg_get_constraintdef(k.oid) \
                 FROM pg_constraint k JOIN pg_class c ON c.oid = k.conrelid \
                WHERE c.relnamespace = 'platform'::regnamespace \
               UNION ALL \
               SELECT 'index ' || indexdef FROM pg_indexes WHERE schemaname = 'platform' \
               UNION ALL \
               SELECT 'policy ' || tablename || '.' || policyname || ' ' \
                      || COALESCE(qual, '') || ' ' || COALESCE(with_check, '') \
                 FROM pg_policies WHERE schemaname = 'platform' \
               UNION ALL \
               SELECT 'security ' || relname || ' ' || relrowsecurity || ' ' || relforcerowsecurity \
                 FROM pg_class WHERE relnamespace = 'platform'::regnamespace AND relkind = 'r' \
             ) q ORDER BY entry COLLATE \"C\"",
            &[],
        )
        .await
        .expect("read the shape of schema platform")
        .iter()
        .map(|row| row.get(0))
        .collect();
    close(client).await;
    shape
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
        record: Some(OrgRecord {
            system_database_url: system_url.into(),
            admin: "org-admin@example.invalid".into(),
        }),
    })
    .await
    .expect("provision the org");
    let outcome = provision_project_env(&ProvisionProjectEnvRequest {
        changes: wamn_platform_identity::auth_changes::AuthChanges::default(),
        org: ORG.into(),
        project: PROJECT.into(),
        env: ENV.into(),
        policy: None,
        disposable: None,
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
    // apply-package creates a model schema as wamn_db_owner.
    project
        .0
        .batch_execute("BEGIN; SET LOCAL ROLE wamn_db_owner; CREATE SCHEMA wms; ROLLBACK")
        .await
        .expect("wamn_db_owner creates a model schema");
    close(project).await;
    wamn_control::reconcile_run_plane::reconcile_run_plane(
        wamn_control::reconcile_run_plane::ReconcileRunPlaneRequest {
            changes: AuthChanges::default(),
            system_database_url: system_url.into(),
            admin_database_url: url.clone(),
            org: ORG.into(),
            project: PROJECT.into(),
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
            "SELECT count(*) FROM platform.schema_migrations"
        )
        .await,
        i64::try_from(project_files.len()).unwrap()
    );

    // A project database installed before 0024: the run reads the record
    // table in the old schema, applies the rename and records it in platform,
    // then applies 0025 to 0038. It loses what 0025, 0029, 0030 and 0038 add,
    // gets back what 0025 removes, has the table names from before
    // 0027 and 0028, and has the tenant floors that 0031 and 0038 drop.
    // Its platform rows and history keys carry the tenant, as before 0031.
    // Remove the workflow table and guard added by 0032 to 0035. Restore
    // empty tables for 0034 and 0036, which only drop those tables.
    // The system database loses the integrations and target tables from
    // system 0029, which the system upgrade below recreates and records.
    let fresh_shape = platform_shape(&project_url).await;
    let client = connect(&project_url).await;
    client
        .0
        .batch_execute(&format!(
            "SET wamn.fixture_tenant = '{TENANT}'; {ADMINISTRATION_BOUNDARY} {ORG_BOUNDARY}"
        ))
        .await
        .expect("restore the tenant floors before 0038 and 0031");
    client
        .0
        .batch_execute(PLURAL_USER)
        .await
        .expect("name the user table as before 0027");
    client
        .0
        .batch_execute(PLURAL_TABLES)
        .await
        .expect("name the role, user_role, permission and api_key tables as before 0028");
    client
        .0
        .batch_execute(
            "DELETE FROM platform.schema_migrations WHERE ordinal >= 24; \
             DROP TABLE catalog.workflows; \
             DROP FUNCTION catalog.guard_workflow_update(); \
             CREATE TABLE catalog.workflow_schedules (); \
             CREATE TABLE catalog.wirings (); \
             CREATE TABLE catalog.event_registrations (); \
             DROP FUNCTION platform.set_counter(uuid, text, text, bigint); \
             DROP FUNCTION platform.assign_counter(uuid, text, text); \
             DROP FUNCTION platform.bump_counter(uuid, text, text); \
             DROP TABLE platform.setting_history, platform.number_sequence_history, \
               platform.project_connection_history, platform.integration_allow_history, \
               platform.connection_history, platform.external_system_history; \
             DROP TABLE platform.setting, platform.number_counter, platform.number_sequence, \
               platform.project_connection, platform.integration_allow, platform.connection, \
               platform.external_system; \
             CREATE TABLE platform.configurations ( \
               tenant_id text NOT NULL, config_key text NOT NULL, config_value jsonb NOT NULL, \
               created_at timestamptz NOT NULL, created_by uuid NOT NULL, \
               updated_at timestamptz NOT NULL, updated_by uuid NOT NULL, \
               PRIMARY KEY (tenant_id, config_key)); \
             SELECT wamn_history.create_history_table('platform', 'configurations', true); \
             GRANT SELECT ON platform.api_keys TO wamn_app; \
             ALTER SCHEMA platform RENAME TO app_system;", // rename-app-system: keep
        )
        .await
        .expect("make the database as 0023 left it");
    close(client).await;
    let system = connect(&system_url).await;
    system
        .0
        .batch_execute(
            "DROP TABLE registry.credential_import, registry.integration_allow, registry.project_connection, registry.connection_generation, identity.credential, registry.connection, registry.external_system CASCADE; DROP FUNCTION registry.reject_connection_generation_change(); DROP TABLE registry.integrations, registry.target; \
             DELETE FROM registry.schema_migrations WHERE ordinal >= 29",
        )
        .await
        .expect("make the system database as 0028 left it");
    close(system).await;
    upgrade_schema_with(
        &request(&system_url, Some(&project_url), None, true),
        project_files,
    )
    .await
    .expect("apply the rename");
    assert_eq!(
        scalar(
            &project_url,
            "SELECT count(*) FROM platform.schema_migrations WHERE ordinal = 38"
        )
        .await,
        1,
        "the project upgrade records 0038"
    );
    assert_eq!(
        scalar(
            &project_url,
            "SELECT count(*) FROM platform.schema_migrations"
        )
        .await,
        i64::try_from(project_files.len()).unwrap()
    );
    assert_eq!(
        scalar(
            &project_url,
            "SELECT count(*) FROM pg_namespace WHERE nspname = 'app_system'" // rename-app-system: keep
        )
        .await,
        0,
        "the rename leaves no schema under the old name"
    );
    assert_eq!(
        platform_shape(&project_url).await,
        fresh_shape,
        "the upgraded schema platform has the shape of a fresh install"
    );
    assert_eq!(
        scalar(
            &project_url,
            "SELECT count(*) FROM platform.\"user\" u \
              WHERE u.type = 'platform' AND EXISTS (SELECT FROM platform.user_history h \
                    WHERE h.row_key = jsonb_build_object('id', u.id))"
        )
        .await,
        i64::try_from(wamn_control_provision::PlatformComponent::ALL.len()).unwrap(),
        "each platform row keeps its rows and its history under its new key"
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
            "SELECT count(*) FROM registry.schema_migrations WHERE ordinal = 29"
        )
        .await,
        1,
        "the system upgrade records 0029"
    );
    assert_eq!(
        scalar(&system_url, "SELECT count(*) FROM registry.integrations").await,
        3
    );
    assert_eq!(
        scalar(&system_url, "SELECT count(*) FROM registry.target").await,
        1
    );
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
        .batch_execute("DROP TABLE platform.schema_migrations")
        .await
        .expect("drop the record table");
    close(client).await;
    let mut files = project_files.to_vec();
    files.push(probe(
        "project",
        project_files.len(),
        "probe_one",
        "CREATE TABLE platform.upgrade_probe_one (x integer);",
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
            "SELECT count(*) FROM platform.schema_migrations"
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
