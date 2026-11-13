//! Live test of `provision-system` against a disposable PostgreSQL server.
//!
//! The test holds the process lock of its server, because the verb changes
//! cluster-wide roles and the PUBLIC CONNECT floor of every database.

use std::os::unix::fs::PermissionsExt as _;
use tokio::task::JoinHandle;
use tokio_postgres::NoTls;

use wamn_environment_control::provision_system::{
    EmitProvisionerRequest, ProvisionSystemRequest, SystemState, emit_provisioner_credential,
    initialize_system, install_control_store, provision_system, system_state,
};
use wamn_test_infrastructure::locked_database;

#[tokio::test]
async fn provision_system_repeats_without_changing_the_platform_domain() {
    let url = locked_database::database(wamn_test_postgres::database);
    let (client, connection) = connect(&url).await;
    // CloudNativePG creates the owner role at bootstrap. The test does it here.
    client
        .batch_execute(
            "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_system') THEN \
               CREATE ROLE wamn_system NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                 NOREPLICATION NOBYPASSRLS; \
             END IF; END $$;",
        )
        .await
        .expect("create the wamn_system role");
    // CloudNativePG makes wamn_system the database owner, which can connect.
    client
        .batch_execute(
            "DO $$ BEGIN EXECUTE format('GRANT CONNECT ON DATABASE %I TO wamn_system', \
                                        current_database()); END $$;",
        )
        .await
        .expect("let wamn_system connect as the owner can");
    let request = ProvisionSystemRequest {
        system_database_url: url.to_string(),
        platform_domain: "wamn.example.test".to_owned(),
    };

    provision_system(&request)
        .await
        .expect("the first run installs the control store");
    let actor = wamn_control_provision::PlatformComponent::Provisioning
        .principal_id()
        .to_string();
    wamn_test_infrastructure::record_history::assert_stamp(
        &client,
        "registry.meta",
        "true",
        &actor,
        &actor,
    )
    .await;
    let domain: Option<String> = client
        .query_one("SELECT platform_domain FROM registry.meta", &[])
        .await
        .expect("read the platform domain")
        .get(0);
    assert_eq!(domain.as_deref(), Some("wamn.example.test"));

    let before = snapshot(&client).await;
    provision_system(&request)
        .await
        .expect("repeat the same domain");
    assert_eq!(system_state(&client).await.unwrap(), SystemState::Complete);
    assert_eq!(
        snapshot(&client).await,
        before,
        "repeat changes no catalog or data rows"
    );

    // The provisioning worker's login holds exactly the measured grants.
    let role = client
        .query_one(
            "SELECT rolcanlogin, rolsuper, rolcreaterole, rolcreatedb, rolinherit, \
                    rolreplication, rolbypassrls, rolconfig::text[], \
                    ARRAY(SELECT parent.rolname::text || ':' || m.inherit_option || ':' \
                                 || m.set_option || ':' || m.admin_option \
                            FROM pg_auth_members m \
                            JOIN pg_roles parent ON parent.oid = m.roleid \
                           WHERE m.member = r.oid ORDER BY 1) \
               FROM pg_roles r WHERE rolname = 'wamn_provisioner'",
            &[],
        )
        .await
        .expect("read wamn_provisioner");
    assert_eq!(
        (0..7)
            .map(|column| role.get::<_, bool>(column))
            .collect::<Vec<_>>(),
        [true, false, true, true, true, true, true],
        "LOGIN NOSUPERUSER CREATEROLE CREATEDB INHERIT REPLICATION BYPASSRLS"
    );
    assert_eq!(
        role.get::<_, Vec<String>>(7),
        ["createrole_self_grant=set, inherit"]
    );
    assert_eq!(
        role.get::<_, Vec<String>>(8),
        [
            "pg_signal_backend:true:true:false",
            "wamn_db_owner:true:true:false",
            "wamn_system:true:true:false",
        ]
    );

    let apply_path =
        std::env::temp_dir().join(format!("wamn-apply-test-{}.json", std::process::id()));
    let parsed_admin = url::Url::parse(&request.system_database_url).unwrap();
    wamn_environment_control::provision_system::emit_apply_credential(
        &request.system_database_url,
        parsed_admin.host_str().unwrap(),
        parsed_admin.port().unwrap_or(5432),
        &apply_path,
    )
    .await
    .unwrap();
    let apply_secret: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&apply_path).unwrap()).unwrap();
    assert_eq!(apply_secret["metadata"]["name"], "wamn-apply");
    let (apply, driver) = tokio_postgres::connect(
        apply_secret["stringData"]["url"].as_str().unwrap(),
        tokio_postgres::NoTls,
    )
    .await
    .unwrap();
    let driver = tokio::spawn(driver);
    assert_eq!(
        apply
            .query_one("SELECT current_user::text", &[])
            .await
            .unwrap()
            .get::<_, String>(0),
        "wamn_apply"
    );
    assert_eq!(
        apply
            .query_one("SELECT platform_domain FROM registry.meta", &[])
            .await
            .unwrap()
            .get::<_, String>(0),
        "wamn.example.test"
    );
    assert!(apply.batch_execute("SET ROLE wamn_system").await.is_err());
    client
        .batch_execute(
            &wamn_control_provision::test_database::seed_org_project_env_sql(
                "applyprobe",
                "existing",
                "dev",
            ),
        )
        .await
        .unwrap();
    let document = wamn_environment_control::environment::document::EnvironmentDocument::from_json(
        br#"{"org":"applyprobe","project":"created","env":"dev","target":"cloud","policy":"dev","integrations":{},"applications": {}}"#,
    ).unwrap();
    let platform = wamn_environment_control::environment::Platform {
        system_database_url: apply_secret["stringData"]["url"].as_str().unwrap().into(),
        target: wamn_release_control::release_chart::Target {
            kubeconfig: std::path::PathBuf::new(),
            context: String::new(),
            namespace: "hosts".into(),
        },
        chart: std::path::PathBuf::new(),
        release_artifact_base: String::new(),
        registry_token_metadata: false,
        registry_auth_file: std::path::PathBuf::new(),
        oci_ca_paths: Vec::new(),
        insecure_registry: false,
        package_artifact_base: None,
        component_artifact_base: None,
        database_namespace: "platform".into(),
        database_host: None,
        database_port: 5432,
        host_group: None,
        pat_issuer: wamn_identity_client::PatIssuerConfig::default(),
        events: None,
        auth_nats: None,
    };
    let secret_path =
        std::env::temp_dir().join(format!("wamn-env-provision-{}.json", std::process::id()));
    let recorded = wamn_environment_control::environment::ensure::provision(
        &platform,
        &document,
        &document.policy,
        &url,
        &secret_path,
        None,
    )
    .await
    .expect("the SQL provisioning operation uses the apply login for registry writes");
    assert!(
        !client
            .query_one(
                "SELECT EXISTS (SELECT FROM pg_database WHERE datname=$1)",
                &[&recorded.database]
            )
            .await
            .unwrap()
            .get::<_, bool>(0),
        "CNPG, not provisioning SQL, creates the database"
    );
    let retry = wamn_environment_control::environment::ensure::provision(
        &platform,
        &document,
        &document.policy,
        &url,
        &secret_path,
        None,
    )
    .await
    .unwrap();
    assert_eq!(retry.database, recorded.database);
    assert_eq!(
        std::fs::metadata(&secret_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    std::fs::remove_file(&secret_path).unwrap();

    client
        .batch_execute(&format!(
            "CREATE DATABASE {} OWNER wamn_db_owner",
            postgres_protocol::escape::escape_identifier(&recorded.database),
        ))
        .await
        .unwrap();
    let mut project_apply = url::Url::parse(&platform.system_database_url).unwrap();
    project_apply.set_path(&recorded.database);
    wamn_environment_control::environment::ensure::ensure_project_database(
        &platform,
        &document,
        &[],
        &client,
        &url,
    )
    .await
    .expect("project initialization uses separate admin and apply connections");
    let drops = wamn_environment_control::environment::bindings::project_authority(
        apply_secret["stringData"]["url"].as_str().unwrap(),
        project_apply.as_str(),
        &document,
    )
    .await
    .expect("the apply login refreshes the projection as the project owner");
    wamn_broker::Publisher::default().publish(drops).await;
    let package = wamn_fixture_package::package_root();
    client
        .batch_execute(&wamn_control_provision::sql::ensure_package_owner_role_sql(
            wamn_fixture_package::PACKAGE_ID,
        ))
        .await
        .unwrap();
    client
        .batch_execute(&format!(
            "GRANT {} TO wamn_db_owner WITH ADMIN FALSE, INHERIT TRUE, SET TRUE",
            postgres_protocol::escape::escape_identifier(
                &wamn_control_provision::package_owner_role(wamn_fixture_package::PACKAGE_ID)
            )
        ))
        .await
        .unwrap();
    wamn_package_control::apply_package::apply_package(
        wamn_package_control::apply_package::ApplyPackageRequest {
            component_source: None,
            package: wamn_package_control::apply_package::PackageRoot {
                artifact_digest: Some(
                    wamn_test_infrastructure::fixture_digest::fixture_artifact_digest(&package)
                        .unwrap(),
                ),
                path: package,
            },
            changes: wamn_broker::Publisher::default(),
            database_url: project_apply.to_string(),
            org: "applyprobe".into(),
            project: "created".into(),
            environment: "dev".into(),
        },
    )
    .await
    .expect("the restricted apply login installs a package through its owner membership");

    drop(apply);
    driver.await.unwrap().unwrap();
    std::fs::remove_file(&apply_path).unwrap();

    // The emit run on the installed database writes the Secret and the role
    // statement. The statement sets the password the Secret carries.
    let directory =
        std::env::temp_dir().join(format!("wamn-provision-system-live-{}", std::process::id()));
    std::fs::create_dir(&directory).expect("create the private output directory");
    let parsed = url::Url::parse(&url).expect("parse the test URL");
    let emit = EmitProvisionerRequest {
        system_database_url: url.to_string(),
        emit_secret: directory.join("secret.json"),
        emit_provisioner_sql: directory.join("provisioner.sql"),
        db_host: parsed
            .host_str()
            .expect("the test URL has a host")
            .to_owned(),
        db_port: parsed.port().unwrap_or(5432),
    };
    emit_provisioner_credential(&emit).expect("the emit run writes both outputs");
    let statement =
        std::fs::read_to_string(&emit.emit_provisioner_sql).expect("read the statement");
    let secret: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&emit.emit_secret).expect("read the Secret"))
            .expect("parse the Secret");
    let worker_url = secret["stringData"]["url"]
        .as_str()
        .expect("the Secret has a url");
    let password = url::Url::parse(worker_url)
        .expect("parse the worker URL")
        .password()
        .expect("the worker URL has a password")
        .to_owned();
    assert!(
        !statement.contains(&password),
        "the statement holds only the verifier"
    );
    assert_eq!(
        std::fs::metadata(&emit.emit_secret)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    client
        .batch_execute(&statement)
        .await
        .expect("the superuser applies the statement");
    let (worker, worker_connection) = tokio_postgres::connect(worker_url, NoTls)
        .await
        .expect("wamn_provisioner logs in with the Secret's password");
    let worker_connection = tokio::spawn(worker_connection);
    let user: String = worker
        .query_one("SELECT current_user::text", &[])
        .await
        .expect("read the current user")
        .get(0);
    assert_eq!(user, "wamn_provisioner");
    disconnect(worker, worker_connection).await;
    disconnect(client, connection).await;
    std::fs::remove_dir_all(&directory).expect("remove the output directory");
}

async fn connect(
    url: &str,
) -> (
    tokio_postgres::Client,
    JoinHandle<Result<(), tokio_postgres::Error>>,
) {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await.unwrap();
    (client, tokio::spawn(connection))
}

async fn disconnect(
    client: tokio_postgres::Client,
    connection: JoinHandle<Result<(), tokio_postgres::Error>>,
) {
    drop(client);
    connection
        .await
        .expect("connection task completes without panicking")
        .expect("connection closes cleanly");
}

#[tokio::test]
async fn fresh_system_runs_image_migrations_and_reapply_runs_none() {
    use wamn_control_provision::schema_migrations::MigrationDatabase;
    use wamn_environment_control::platform_migrations;
    use wamn_schema_control::MigrationSource;

    let url = locked_database::database(wamn_test_postgres::database);
    let (mut client, connection) = connect(&url).await;
    client.batch_execute("DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_system') THEN CREATE ROLE wamn_system NOLOGIN; END IF; END $$;").await.unwrap();
    let mut sources = platform_migrations::embedded_sources(MigrationDatabase::System);
    sources.push(MigrationSource {
        relative_path: format!("migrations/system/{:04}_fresh_probe.sql", sources.len() + 1),
        bytes: b"CREATE TABLE registry.dep5_fresh_probe (value int); INSERT INTO registry.dep5_fresh_probe VALUES (1);".to_vec(),
    });
    initialize_system(&mut client, &sources).await.unwrap();
    assert_eq!(
        client
            .query_one("SELECT value FROM registry.dep5_fresh_probe", &[])
            .await
            .unwrap()
            .get::<_, i32>(0),
        1
    );
    let recorded = platform_migrations::plan(&client, MigrationDatabase::System, &sources)
        .await
        .unwrap();
    assert!(!recorded.adoption);
    assert!(recorded.migrations.pending.is_empty());
    assert_eq!(
        recorded
            .recorded
            .iter()
            .map(|row| row.relative_path.as_str())
            .collect::<Vec<_>>(),
        sources
            .iter()
            .map(|source| source.relative_path.as_str())
            .collect::<Vec<_>>()
    );

    let before = snapshot(&client).await;
    let earlier = &sources[..sources.len() - 1];
    let preview = platform_migrations::plan(&client, MigrationDatabase::System, earlier)
        .await
        .unwrap();
    assert!(
        preview.missing.is_some(),
        "diff warns when the image lacks recorded files"
    );
    assert_eq!(preview.recorded, recorded.recorded);
    assert!(
        platform_migrations::apply_reported(
            &mut client,
            platform_migrations::MigrationTarget::System,
            earlier,
            None
        )
        .await
        .is_err()
    );
    assert_eq!(
        snapshot(&client).await,
        before,
        "missing bytes never authorize SQL or history changes"
    );

    sources.push(MigrationSource {
        relative_path: format!("migrations/system/{:04}_later_probe.sql", sources.len() + 1),
        bytes: b"INSERT INTO registry.dep5_fresh_probe VALUES (2);".to_vec(),
    });
    let before = snapshot(&client).await;
    initialize_system(&mut client, &sources).await.unwrap();
    assert_eq!(
        snapshot(&client).await,
        before,
        "reapply runs no pending SQL and preserves initialization"
    );
    assert!(
        !platform_migrations::plan(&client, MigrationDatabase::System, &sources)
            .await
            .unwrap()
            .migrations
            .pending
            .is_empty()
    );
    client.batch_execute("SET ROLE wamn_system").await.unwrap();
    platform_migrations::apply_reported(
        &mut client,
        platform_migrations::MigrationTarget::System,
        &sources,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        client
            .query(
                "SELECT value FROM registry.dep5_fresh_probe ORDER BY value",
                &[]
            )
            .await
            .unwrap()
            .iter()
            .map(|row| row.get::<_, i32>(0))
            .collect::<Vec<_>>(),
        [1, 2]
    );
    platform_migrations::apply_reported(
        &mut client,
        platform_migrations::MigrationTarget::System,
        &sources,
        None,
    )
    .await
    .unwrap();
    // The image's full file set produces the same substrate through fresh installation.
    let fresh = wamn_test_postgres::database();
    let (mut fresh_client, fresh_connection) = connect(fresh.url()).await;
    initialize_system(&mut fresh_client, &sources)
        .await
        .unwrap();
    assert_eq!(schema_dump(fresh.url()).await, schema_dump(&url).await);
    assert_eq!(
        platform_migrations::plan(&fresh_client, MigrationDatabase::System, &sources)
            .await
            .unwrap()
            .recorded,
        platform_migrations::plan(&client, MigrationDatabase::System, &sources)
            .await
            .unwrap()
            .recorded
    );
    disconnect(fresh_client, fresh_connection).await;
    sources.push(MigrationSource {
        relative_path: format!("migrations/system/{:04}_pending.sql", sources.len() + 1),
        bytes: b"INSERT INTO registry.dep5_fresh_probe VALUES (3);".to_vec(),
    });
    let upgraded = snapshot(&client).await;
    initialize_system(&mut client, &sources).await.unwrap();
    assert_eq!(
        snapshot(&client).await,
        upgraded,
        "reapply after upgrade runs no bootstrap or pending migration"
    );
    disconnect(client, connection).await;
}

#[tokio::test]
async fn fresh_system_failure_keeps_progress_for_explicit_schema_apply() {
    use wamn_control_provision::schema_migrations::MigrationDatabase;
    use wamn_environment_control::platform_migrations;
    use wamn_schema_control::MigrationSource;

    let url = locked_database::database(wamn_test_postgres::database);
    let (mut client, connection) = connect(&url).await;
    client.batch_execute("DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_system') THEN CREATE ROLE wamn_system NOLOGIN; END IF; END $$;").await.unwrap();
    let mut sources = platform_migrations::embedded_sources(MigrationDatabase::System);
    sources.push(MigrationSource {
        relative_path: format!(
            "migrations/system/{:04}_resume_probe.sql",
            sources.len() + 1
        ),
        bytes: b"CREATE TABLE registry.dep5_resume_probe (value int);".to_vec(),
    });
    sources.push(MigrationSource {
        relative_path: format!(
            "migrations/system/{:04}_wait_for_repair.sql",
            sources.len() + 1
        ),
        bytes: b"SELECT 1 / count(*)::int FROM registry.dep5_resume_probe;".to_vec(),
    });
    let error = initialize_system(&mut client, &sources).await.unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<tokio_postgres::Error>()
            .unwrap()
            .code(),
        Some(&tokio_postgres::error::SqlState::DIVISION_BY_ZERO)
    );
    assert_eq!(system_state(&client).await.unwrap(), SystemState::Complete);
    let interrupted = platform_migrations::plan(&client, MigrationDatabase::System, &sources)
        .await
        .unwrap();
    assert_eq!(interrupted.recorded.len(), sources.len() - 1);
    assert_eq!(
        interrupted.migrations.pending[0].relative_path,
        sources.last().unwrap().relative_path
    );
    let before = snapshot(&client).await;
    initialize_system(&mut client, &sources).await.unwrap();
    assert_eq!(snapshot(&client).await, before);
    client
        .batch_execute("INSERT INTO registry.dep5_resume_probe VALUES (1); SET ROLE wamn_system;")
        .await
        .unwrap();
    platform_migrations::apply_reported(
        &mut client,
        platform_migrations::MigrationTarget::System,
        &sources,
        None,
    )
    .await
    .unwrap();
    let resumed = platform_migrations::plan(&client, MigrationDatabase::System, &sources)
        .await
        .unwrap();
    assert!(resumed.migrations.pending.is_empty());
    assert_eq!(resumed.recorded.len(), sources.len());
    disconnect(client, connection).await;
}

// Compare substrate objects, including functions, grants, and history guards.
// The application-data fixture lives in public, outside the substrate.
// A fixed restrict key removes pg_dump's random session delimiter.
async fn schema_dump(url: &str) -> String {
    let output = tokio::process::Command::new("/usr/lib/postgresql/18/bin/pg_dump")
        .args([
            "--schema-only",
            "--no-comments",
            "--exclude-schema=public",
            "--restrict-key=wamnDEP506",
            url,
        ])
        .output()
        .await
        .expect("dump the disposable PostgreSQL schema");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("pg_dump emits UTF-8 SQL")
}

// Enumerate the actual catalog and data rather than pinning a relation roster.
async fn snapshot(client: &tokio_postgres::Client) -> serde_json::Value {
    let catalog: serde_json::Value = client.query_one(r"
        WITH schemas AS (SELECT * FROM pg_namespace WHERE nspname !~ '^pg_' AND nspname <> 'information_schema')
        SELECT jsonb_build_object(
          'schemas', (SELECT jsonb_agg(to_jsonb(n) ORDER BY oid) FROM schemas n),
          'relations', (SELECT jsonb_agg(jsonb_build_array(to_jsonb(c), c.xmin::text) ORDER BY c.oid)
                        FROM pg_class c JOIN schemas n ON n.oid = c.relnamespace),
          'columns', (SELECT jsonb_agg(to_jsonb(a) ORDER BY a.attrelid, a.attnum)
                      FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid JOIN schemas n ON n.oid = c.relnamespace),
          'functions', (SELECT jsonb_agg(to_jsonb(p) ORDER BY p.oid) FROM pg_proc p JOIN schemas n ON n.oid = p.pronamespace),
          'roles', (SELECT jsonb_agg(to_jsonb(r) ORDER BY oid) FROM pg_roles r),
          'memberships', (SELECT jsonb_agg(to_jsonb(m) ORDER BY roleid, member, grantor) FROM pg_auth_members m),
          'databases', (SELECT jsonb_agg(jsonb_build_array(datname, datacl::text, xmin::text) ORDER BY datname) FROM pg_database))
    ", &[]).await.unwrap().get(0);
    let mut data = serde_json::Map::new();
    for row in client
        .query(
            r"
        SELECT format('%I.%I', n.nspname, c.relname)
        FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
        WHERE c.relkind = 'r' AND n.nspname !~ '^pg_' AND n.nspname <> 'information_schema'
        ORDER BY 1
    ",
            &[],
        )
        .await
        .unwrap()
    {
        let relation: String = row.get(0);
        let contents = client.query_one(&format!(r"
            SELECT COALESCE(jsonb_agg(jsonb_build_array(to_jsonb(t), xmin::text) ORDER BY to_jsonb(t)::text), '[]')
            FROM {relation} t
        "), &[]).await.unwrap();
        data.insert(relation, contents.get::<_, serde_json::Value>(0));
    }
    serde_json::json!({"catalog": catalog, "data": data})
}

#[tokio::test]
async fn bootstrap_is_atomic_and_uses_database_facts() {
    let url = locked_database::database(wamn_test_postgres::database);
    let (client, connection) = connect(&url).await;
    client.batch_execute("DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_system') THEN CREATE ROLE wamn_system NOLOGIN; END IF; END $$;").await.unwrap();
    assert_eq!(system_state(&client).await.unwrap(), SystemState::Absent);

    // Development resets retain the shared history schema.
    client
        .batch_execute("CREATE SCHEMA wamn_history AUTHORIZATION wamn_system")
        .await
        .unwrap();
    assert_eq!(system_state(&client).await.unwrap(), SystemState::Absent);

    // Fail the final apply grant after both schema batches and baseline insertion.
    // This is a server-side SQL failure, not a replacement bootstrap or checker.
    client.batch_execute(r"
        CREATE FUNCTION public.reject_final_grant() RETURNS event_trigger LANGUAGE plpgsql AS $$
        DECLARE granted boolean;
        BEGIN
          IF to_regclass('registry.schema_baseline') IS NOT NULL
             AND EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_apply') THEN
            EXECUTE 'SELECT has_column_privilege(''wamn_apply'', ''identity.org_memberships'', ''status'', ''UPDATE'')' INTO granted;
            IF granted THEN PERFORM 1 / 0; END IF;
          END IF;
        END $$;
        CREATE EVENT TRIGGER reject_final_grant ON ddl_command_end WHEN TAG IN ('GRANT')
          EXECUTE FUNCTION public.reject_final_grant();
    ").await.unwrap();
    let before = snapshot(&client).await;
    let failure = install_control_store(&client).await.unwrap_err();
    assert_eq!(
        failure
            .downcast_ref::<tokio_postgres::Error>()
            .unwrap()
            .code(),
        Some(&tokio_postgres::error::SqlState::DIVISION_BY_ZERO)
    );
    assert_eq!(
        snapshot(&client).await,
        before,
        "late failure rolls back schema, roles, grants and data"
    );
    assert_eq!(system_state(&client).await.unwrap(), SystemState::Absent);
    client
        .batch_execute(
            "DROP EVENT TRIGGER reject_final_grant; DROP FUNCTION public.reject_final_grant();",
        )
        .await
        .unwrap();

    assert!(install_control_store(&client).await.unwrap());
    assert_eq!(system_state(&client).await.unwrap(), SystemState::Complete);
    let concurrent_database = wamn_test_postgres::database();
    let (first, first_connection) = connect(concurrent_database.url()).await;
    let (second, second_connection) = connect(concurrent_database.url()).await;
    let (left, right) = tokio::join!(
        install_control_store(&first),
        install_control_store(&second)
    );
    assert_ne!(left.unwrap(), right.unwrap(), "only one caller initializes");
    assert_eq!(system_state(&first).await.unwrap(), SystemState::Complete);
    let concurrent = snapshot(&first).await;
    assert!(!install_control_store(&first).await.unwrap());
    assert_eq!(snapshot(&first).await, concurrent);
    disconnect(first, first_connection).await;
    disconnect(second, second_connection).await;

    let partial_database = wamn_test_postgres::database();
    let (partial, partial_connection) = connect(partial_database.url()).await;
    partial
        .batch_execute("CREATE SCHEMA registry")
        .await
        .unwrap();
    assert_eq!(
        system_state(&partial).await.unwrap(),
        SystemState::Incomplete
    );
    let before = snapshot(&partial).await;
    assert!(install_control_store(&partial).await.is_err());
    assert_eq!(snapshot(&partial).await, before);
    disconnect(partial, partial_connection).await;

    client
        .batch_execute("UPDATE registry.schema_baseline SET sha256 = 'sha256:' || repeat('0', 64)")
        .await
        .unwrap();
    assert!(matches!(
        system_state(&client).await.unwrap(),
        SystemState::BaselineMismatch { .. }
    ));
    let before = snapshot(&client).await;
    assert!(install_control_store(&client).await.is_err());
    assert_eq!(
        snapshot(&client).await,
        before,
        "mismatch does not replay or reset"
    );
    disconnect(client, connection).await;
}

async fn migration_database(
    database_type: wamn_control_provision::schema_migrations::MigrationDatabase,
) -> (
    wamn_test_postgres::Database,
    tokio_postgres::Client,
    JoinHandle<Result<(), tokio_postgres::Error>>,
) {
    use wamn_control_provision::schema_migrations::MigrationDatabase;
    let database = match database_type {
        MigrationDatabase::System => wamn_test_postgres::database(),
        MigrationDatabase::Project => wamn_platform_identity::test_database::tenant_platform(),
    };
    let (client, connection) = connect(database.url()).await;
    match database_type {
        MigrationDatabase::System => {
            client.batch_execute("DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_system') THEN CREATE ROLE wamn_system NOLOGIN; END IF; END $$;").await.unwrap();
            install_control_store(&client).await.unwrap();
        }
        MigrationDatabase::Project => {
            client
                .batch_execute(include_str!(
                    "../../../../deploy/sql/app-schema-baseline.sql"
                ))
                .await
                .unwrap();
            wamn_package_control::schema_baseline::record_project(&client)
                .await
                .unwrap();
        }
    }
    let owner = match database_type {
        MigrationDatabase::System => "wamn_system",
        MigrationDatabase::Project => "wamn_db_owner",
    };
    client
        .batch_execute(&format!(
            "ALTER DATABASE {} OWNER TO {owner}",
            postgres_protocol::escape::escape_identifier(database.name())
        ))
        .await
        .unwrap();
    if database_type == MigrationDatabase::Project {
        // The existing fixture installs as its administrator. Match the production substrate owner.
        client.batch_execute("ALTER SCHEMA platform OWNER TO wamn_db_owner; ALTER TABLE platform.schema_baseline OWNER TO wamn_db_owner;").await.unwrap();
    }
    client
        .batch_execute(&format!("SET ROLE {owner}"))
        .await
        .unwrap();
    (database, client, connection)
}

#[tokio::test]
async fn platform_migrations_adopt_order_commit_and_resume_exact_bytes() {
    use wamn_control_provision::schema_migrations::MigrationDatabase;
    use wamn_environment_control::platform_migrations::{MigrationTarget, apply, plan};
    use wamn_schema_control::{MigrationOutcome, MigrationSource};

    let _lock = wamn_test_postgres::lock();
    for database_type in [MigrationDatabase::System, MigrationDatabase::Project] {
        let (database, mut client, mut connection) = migration_database(database_type).await;
        let (registry, system, system_connection) =
            migration_database(MigrationDatabase::System).await;
        system
            .batch_execute(
                &wamn_control_provision::test_database::seed_org_project_env_sql(
                    "migration",
                    "fixture",
                    "dev",
                ),
            )
            .await
            .unwrap();
        let triple = wamn_control_registry::Triple::new("migration", "fixture", "dev");
        let lifecycle = wamn_environment_control::environment::lock::LifecycleLock::acquire(
            registry.url(),
            &triple,
        )
        .await
        .unwrap();
        let target = match database_type {
            MigrationDatabase::System => MigrationTarget::System,
            MigrationDatabase::Project => MigrationTarget::Project(&lifecycle),
        };
        let (name, schema, owner) = match database_type {
            MigrationDatabase::System => ("system", "registry", "wamn_system"),
            MigrationDatabase::Project => ("project", "platform", "wamn_db_owner"),
        };
        let baseline_sql =
            format!("SELECT sha256, installed_at::text FROM {schema}.schema_baseline");
        let before = client.query_one(&baseline_sql, &[]).await.unwrap();
        let initialization = (before.get::<_, String>(0), before.get::<_, String>(1));
        let path = |ordinal: usize| format!("migrations/{name}/{ordinal:04}_effect.sql");
        let source = |ordinal, sql: String| MigrationSource {
            relative_path: path(ordinal),
            bytes: sql.into_bytes(),
        };
        let first = source(
            1,
            format!(
                "CREATE TABLE public.migration_effect (sequence int GENERATED ALWAYS AS IDENTITY, path text UNIQUE); INSERT INTO public.migration_effect(path) VALUES ('{}');",
                path(1)
            ),
        );
        let second = source(
            2,
            format!(
                "INSERT INTO public.migration_effect(path) VALUES ('{}');",
                path(2)
            ),
        );

        // The unsafe later file refuses before the earlier file or adoption writes.
        let unsafe_files = vec![first.clone(), source(2, "-- wamn:no-transaction\nCREATE TABLE public.unsafe_a(id int); CREATE TABLE public.unsafe_b(id int);".into())];
        assert!(
            apply(&mut client, target, &unsafe_files, None)
                .await
                .is_err()
        );
        assert!(plan(&client, database_type, &[]).await.unwrap().adoption);
        assert!(client.query_one("SELECT to_regclass('public.migration_effect') IS NULL AND to_regclass('public.unsafe_a') IS NULL AND to_regclass('public.unsafe_b') IS NULL", &[]).await.unwrap().get::<_, bool>(0));

        // A failed first transactional file rolls back its DDL and history creation.
        let failing = vec![source(
            1,
            "CREATE TABLE public.failed_effect(id int); SELECT 1/0;".into(),
        )];
        assert!(apply(&mut client, target, &failing, None).await.is_err());
        assert!(plan(&client, database_type, &[]).await.unwrap().adoption);
        assert!(
            client
                .query_one("SELECT to_regclass('public.failed_effect') IS NULL", &[])
                .await
                .unwrap()
                .get::<_, bool>(0)
        );

        // Read-only planning describes adoption and pending files without writing.
        let files = vec![second, first];
        let preview = plan(&client, database_type, &files).await.unwrap();
        assert!(preview.adoption);
        assert!(preview.recorded.is_empty());
        assert!(plan(&client, database_type, &[]).await.unwrap().adoption);
        assert_eq!(apply(&mut client, target, &[], None).await.unwrap(), 0);
        let adopted = plan(&client, database_type, &files).await.unwrap();
        assert!(!adopted.adoption);
        assert!(
            adopted.recorded.is_empty(),
            "adoption records no executed file"
        );

        // A real record-write failure rolls back the file's non-idempotent effect.
        client.batch_execute(&format!("CREATE FUNCTION public.reject_migration_record() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'record unavailable'; END $$; CREATE TRIGGER reject_migration_record BEFORE INSERT ON {schema}.schema_migration FOR EACH ROW EXECUTE FUNCTION public.reject_migration_record();")).await.unwrap();
        assert!(apply(&mut client, target, &files, None).await.is_err());
        assert!(
            client
                .query_one("SELECT to_regclass('public.migration_effect') IS NULL", &[])
                .await
                .unwrap()
                .get::<_, bool>(0)
        );
        assert!(
            plan(&client, database_type, &files)
                .await
                .unwrap()
                .recorded
                .is_empty()
        );
        client.batch_execute(&format!("DROP TRIGGER reject_migration_record ON {schema}.schema_migration; DROP FUNCTION public.reject_migration_record();")).await.unwrap();

        // Earlier files commit while a later transactional failure leaves no row.
        let mut interrupted = files;
        interrupted.push(source(
            3,
            "CREATE TABLE public.failed_effect(id int); SELECT 1/0;".into(),
        ));
        assert!(
            apply(&mut client, target, &interrupted, None)
                .await
                .is_err()
        );
        let partial = plan(&client, database_type, &interrupted).await.unwrap();
        assert_eq!(partial.recorded.len(), preview.migrations.pending.len());
        assert!(
            partial
                .recorded
                .iter()
                .all(|row| row.outcome == MigrationOutcome::Applied)
        );
        assert!(
            client
                .query_one("SELECT to_regclass('public.failed_effect') IS NULL", &[])
                .await
                .unwrap()
                .get::<_, bool>(0)
        );
        interrupted.last_mut().unwrap().bytes = format!(
            "INSERT INTO public.migration_effect(path) VALUES ('{}');",
            path(3)
        )
        .into_bytes();
        assert_eq!(
            apply(&mut client, target, &interrupted, None)
                .await
                .unwrap(),
            partial.migrations.pending.len()
        );

        // Discard the result, reconnect, and repeat. A CREATE without IF NOT EXISTS runs once.
        // Only the result is discarded; the client still closes its connection normally.
        disconnect(client, connection).await;
        (client, connection) = connect(database.url()).await;
        client
            .batch_execute(&format!("SET ROLE {owner}"))
            .await
            .unwrap();
        assert_eq!(
            apply(&mut client, target, &interrupted, None)
                .await
                .unwrap(),
            0
        );
        let completed = plan(&client, database_type, &interrupted).await.unwrap();
        let mut expected = interrupted
            .iter()
            .map(|file| file.relative_path.as_str())
            .collect::<Vec<_>>();
        expected.sort_unstable();
        let actual = client
            .query(
                "SELECT path FROM public.migration_effect ORDER BY sequence",
                &[],
            )
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.get::<_, String>(0))
            .collect::<Vec<_>>();
        assert_eq!(
            actual, expected,
            "effects follow the artifact's ordinal order exactly once"
        );
        assert_eq!(
            completed
                .recorded
                .iter()
                .map(|row| row.relative_path.as_str())
                .collect::<Vec<_>>(),
            expected
        );
        for record in &completed.recorded {
            let source = interrupted
                .iter()
                .find(|file| file.relative_path == record.relative_path)
                .unwrap();
            assert_eq!(record.sha256, wamn_digest::bytes_sha256(&source.bytes));
        }
        let actor = wamn_control_provision::PlatformComponent::Provisioning
            .principal_id()
            .to_string();
        wamn_test_infrastructure::record_history::assert_stamp(
            &client,
            &format!("{schema}.schema_migration"),
            "ordinal=1",
            &actor,
            &actor,
        )
        .await;
        if database_type == MigrationDatabase::System {
            assert!(client.query_one("SELECT has_table_privilege('wamn_apply', 'registry.schema_migration', 'SELECT') AND NOT has_table_privilege('wamn_apply', 'registry.schema_migration', 'INSERT,UPDATE,DELETE')", &[]).await.unwrap().get::<_, bool>(0));
        }

        // A drift refusal blocks every later file before its effect.
        let mut edited = interrupted.clone();
        edited[0].bytes.push(b' ');
        edited.push(source(
            4,
            "CREATE TABLE public.must_not_run(id int);".into(),
        ));
        let error = apply(&mut client, target, &edited, None).await.unwrap_err();
        assert!(
            error
                .downcast_ref::<wamn_schema_control::PackageMigrationError>()
                .is_some()
        );
        assert!(
            client
                .query_one("SELECT to_regclass('public.must_not_run') IS NULL", &[])
                .await
                .unwrap()
                .get::<_, bool>(0)
        );
        assert_eq!(
            plan(&client, database_type, &interrupted)
                .await
                .unwrap()
                .recorded,
            completed.recorded
        );
        for sql in [
            format!("DELETE FROM {schema}.schema_migration"),
            format!("UPDATE {schema}.schema_migration SET sha256='sha256:' || repeat('0',64)"),
            format!("UPDATE {schema}.schema_migration SET applied_at=NULL"),
            format!("UPDATE {schema}.schema_migration SET outcome='started', applied_at=NULL"),
        ] {
            assert!(client.batch_execute(&sql).await.is_err());
        }
        let after = client.query_one(&baseline_sql, &[]).await.unwrap();
        assert_eq!(
            (after.get::<_, String>(0), after.get::<_, String>(1)),
            initialization
        );

        // A different initialization refuses and exposes both digests without migration writes.
        client
            .batch_execute(&format!(
                "UPDATE {schema}.schema_baseline SET sha256='sha256:' || repeat('0',64)"
            ))
            .await
            .unwrap();
        let error = plan(&client, database_type, &interrupted)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains(&initialization.0));
        assert!(error.contains(&format!("sha256:{}", "0".repeat(64))));
        disconnect(client, connection).await;
        disconnect(system, system_connection).await;
    }
}

#[tokio::test]
async fn platform_nontransactional_files_keep_started_and_share_resolution() {
    use wamn_control_provision::schema_migrations::MigrationDatabase;
    use wamn_environment_control::platform_migrations::{MigrationTarget, apply, plan};
    use wamn_package_control::migrations::{MigrationResolution, Resolution};
    use wamn_schema_control::{MigrationOutcome, MigrationSource};

    let _lock = wamn_test_postgres::lock();
    for database_type in [MigrationDatabase::System, MigrationDatabase::Project] {
        let (_database, mut client, connection) = migration_database(database_type).await;
        let (registry, system, system_connection) =
            migration_database(MigrationDatabase::System).await;
        system
            .batch_execute(
                &wamn_control_provision::test_database::seed_org_project_env_sql(
                    "migration",
                    "fixture",
                    "dev",
                ),
            )
            .await
            .unwrap();
        let triple = wamn_control_registry::Triple::new("migration", "fixture", "dev");
        let lifecycle = wamn_environment_control::environment::lock::LifecycleLock::acquire(
            registry.url(),
            &triple,
        )
        .await
        .unwrap();
        let target = match database_type {
            MigrationDatabase::System => MigrationTarget::System,
            MigrationDatabase::Project => MigrationTarget::Project(&lifecycle),
        };
        let (name, schema) = match database_type {
            MigrationDatabase::System => ("system", "registry"),
            MigrationDatabase::Project => ("project", "platform"),
        };
        let path = format!("migrations/{name}/0001_index.sql");
        let mut files = vec![MigrationSource { relative_path: path.clone(),
            bytes: b"-- wamn:no-transaction\nCREATE UNIQUE INDEX CONCURRENTLY migration_effect_idx ON public.migration_effect(id);".to_vec() }];
        // A nontransactional first failure retains adoption and its durable started row.
        assert!(apply(&mut client, target, &files, None).await.is_err());
        let uncertain = plan(&client, database_type, &files).await.unwrap();
        assert!(!uncertain.adoption);
        assert_eq!(uncertain.recorded.len(), files.len());
        assert_eq!(uncertain.recorded[0].outcome, MigrationOutcome::Started);
        assert_eq!(uncertain.migrations.pending[0].relative_path, path);
        assert!(
            client
                .query_one(
                    &format!("SELECT age(c.xmin) > age(m.xmin) FROM pg_class c CROSS JOIN {schema}.schema_migration m WHERE c.oid = '{schema}.schema_migration'::regclass"),
                    &[],
                )
                .await
                .unwrap()
                .get::<_, bool>(0),
            "adoption commits in an earlier transaction than the first started row"
        );
        let observed =
            wamn_package_control::schema_baseline::observe(&client, database_type, &files)
                .await
                .unwrap();
        assert!(observed.installed.is_empty());
        assert!(observed.pending.is_empty());
        assert_eq!(
            observed
                .uncertain
                .iter()
                .map(|file| &file.relative_path)
                .collect::<Vec<_>>(),
            files
                .iter()
                .map(|file| &file.relative_path)
                .collect::<Vec<_>>()
        );

        assert!(apply(&mut client, target, &files, None).await.is_err());
        // A started row can complete, but its execution identity cannot change.
        for assignment in [
            "ordinal=ordinal+1000",
            "relative_path=replace(relative_path,'.sql','_edited.sql')",
            "sha256='sha256:' || repeat('0',64)",
        ] {
            assert!(client.batch_execute(&format!("UPDATE {schema}.schema_migration SET {assignment}, outcome='applied', applied_at=now() WHERE outcome='started'")).await.is_err());
        }
        let mut choice = MigrationResolution {
            package: name.into(),
            path: path.clone(),
            action: Resolution::Retry,
        };
        let original = files[0].bytes.clone();
        files[0].bytes.push(b' ');
        let refusal = apply(&mut client, target, &files, Some(&choice))
            .await
            .unwrap_err();
        assert_eq!(
            refusal
                .downcast_ref::<wamn_schema_control::PackageMigrationError>()
                .unwrap()
                .error_type(),
            wamn_schema_control::PackageMigrationErrorType::MigrationDrift
        );
        files[0].bytes = original;
        // The developer supplies the missing prerequisite, then retries the same bytes.
        client
            .batch_execute("CREATE TABLE public.migration_effect(id int);")
            .await
            .unwrap();
        assert_eq!(
            apply(&mut client, target, &files, Some(&choice))
                .await
                .unwrap(),
            files.len()
        );
        assert!(
            plan(&client, database_type, &files)
                .await
                .unwrap()
                .recorded
                .iter()
                .all(|file| file.outcome == MigrationOutcome::Applied)
        );

        // Lose the completion write after a non-idempotent statement commits.
        let next_path = format!("migrations/{name}/0002_effect.sql");
        files.push(MigrationSource {
            relative_path: next_path.clone(),
            bytes: b"-- wamn:no-transaction\nCREATE TABLE public.once_only(id int);".to_vec(),
        });
        client.batch_execute(&format!("CREATE FUNCTION public.reject_completion() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'completion unavailable'; END $$; CREATE TRIGGER reject_completion BEFORE UPDATE ON {schema}.schema_migration FOR EACH ROW EXECUTE FUNCTION public.reject_completion();")).await.unwrap();
        assert!(apply(&mut client, target, &files, None).await.is_err());
        assert!(
            client
                .query_one("SELECT to_regclass('public.once_only') IS NOT NULL", &[])
                .await
                .unwrap()
                .get::<_, bool>(0)
        );
        assert_eq!(
            plan(&client, database_type, &files)
                .await
                .unwrap()
                .recorded
                .last()
                .unwrap()
                .outcome,
            MigrationOutcome::Started
        );
        client.batch_execute(&format!("DROP TRIGGER reject_completion ON {schema}.schema_migration; DROP FUNCTION public.reject_completion();")).await.unwrap();
        choice.path = next_path;
        choice.action = Resolution::Applied;
        assert_eq!(
            apply(&mut client, target, &files, Some(&choice))
                .await
                .unwrap(),
            0
        );
        assert!(
            plan(&client, database_type, &files)
                .await
                .unwrap()
                .migrations
                .pending
                .is_empty()
        );
        assert_eq!(
            apply(&mut client, target, &files, Some(&choice))
                .await
                .unwrap(),
            0,
            "repeated resolution after a lost result does not execute again"
        );
        let complete: bool = client.query_one(&format!("SELECT bool_and((outcome='applied') = (applied_at IS NOT NULL)) FROM {schema}.schema_migration"), &[]).await.unwrap().get(0);
        assert!(complete);
        disconnect(client, connection).await;
        disconnect(system, system_connection).await;
    }
}

#[tokio::test]
async fn project_schema_preserves_applications_and_other_environments_and_holds_exclusion() {
    use wamn_control_provision::schema_migrations::MigrationDatabase;
    use wamn_environment_control::environment::{
        Platform, document::EnvironmentDocument, lock::LifecycleLock, sql,
    };
    use wamn_environment_control::{
        platform_migrations,
        reconcile_run_plane::{ReconcileRunPlaneRequest, reconcile_run_plane_with_migrations},
    };
    use wamn_schema_control::MigrationSource;

    let mut postgres = wamn_test_postgres::start(&[]).unwrap();
    let system_database = postgres.create_database("wamn_system").unwrap();
    let (mut system, system_connection) = connect(system_database.url()).await;
    system
        .batch_execute("CREATE ROLE wamn_system NOLOGIN")
        .await
        .unwrap();
    install_control_store(&system).await.unwrap();
    wamn_environment_control::provision_system::set_platform_domain(&mut system, "example.invalid")
        .await
        .unwrap();
    for env in ["dev", "other"] {
        system
            .batch_execute(
                &wamn_control_provision::test_database::seed_org_project_env_sql(
                    "dep5", "fixture", env,
                ),
            )
            .await
            .unwrap();
    }
    let platform = Platform {
        system_database_url: system_database.url().into(),
        target: wamn_release_control::release_chart::Target {
            kubeconfig: std::path::PathBuf::new(),
            context: String::new(),
            namespace: "platform".into(),
        },
        chart: std::path::PathBuf::new(),
        release_artifact_base: String::new(),
        registry_token_metadata: false,
        registry_auth_file: std::path::PathBuf::new(),
        oci_ca_paths: Vec::new(),
        insecure_registry: false,
        package_artifact_base: None,
        component_artifact_base: None,
        database_namespace: "platform".into(),
        database_host: None,
        database_port: 5432,
        host_group: None,
        pat_issuer: wamn_identity_client::PatIssuerConfig::default(),
        events: None,
        auth_nats: None,
    };
    let document = EnvironmentDocument::from_json(br#"{"org":"dep5","project":"fixture","env":"dev","target":"cloud","policy":"dev","applications":{}}"#).unwrap();
    let mut databases = Vec::new();
    for env in ["dev", "other"] {
        let name =
            wamn_control_provision::project_env_database_name("dep5", "fixture", env, "aaaaaaa1");
        system
            .batch_execute(&format!(
                "CREATE DATABASE {} OWNER wamn_db_owner",
                postgres_protocol::escape::escape_identifier(&name)
            ))
            .await
            .unwrap();
        let database = postgres.database(&name).unwrap();
        databases.push(database);
    }
    let (project, project_connection) = connect(databases[0].url()).await;
    let (other, other_connection) = connect(databases[1].url()).await;
    let request = || ReconcileRunPlaneRequest {
        system_database_url: system_database.url().into(),
        admin_database_url: databases[0].url().into(),
        org: "dep5".into(),
        project: "fixture".into(),
        env: "dev".into(),
        schema: "wamn_run".into(),
        dry_run: false,
        changes: wamn_broker::Publisher::default(),
    };
    let mut sources = platform_migrations::embedded_sources(MigrationDatabase::Project);
    sources.push(MigrationSource {
        relative_path: format!("migrations/project/{:04}_fresh.sql", sources.len() + 1),
        bytes: b"CREATE TABLE platform.dep5_probe (value int); INSERT INTO platform.dep5_probe VALUES (1);".to_vec(),
    });
    let fresh = reconcile_run_plane_with_migrations(request(), &sources)
        .await
        .unwrap();
    assert!(fresh.env_key_identity.app_schema_installed);
    let installed = platform_migrations::plan(&project, MigrationDatabase::Project, &sources)
        .await
        .unwrap();
    assert!(installed.migrations.pending.is_empty());
    assert_eq!(
        installed
            .recorded
            .iter()
            .map(|row| &row.relative_path)
            .collect::<Vec<_>>(),
        sources
            .iter()
            .map(|file| &file.relative_path)
            .collect::<Vec<_>>()
    );
    let baseline: (String, String) = {
        let row = project
            .query_one(
                "SELECT sha256, installed_at::text FROM platform.schema_baseline",
                &[],
            )
            .await
            .unwrap();
        (row.get(0), row.get(1))
    };
    sources.push(MigrationSource {
        relative_path: format!("migrations/project/{:04}_selected.sql", sources.len() + 1),
        bytes: b"INSERT INTO platform.dep5_probe VALUES (2);".to_vec(),
    });
    let repeated = reconcile_run_plane_with_migrations(request(), &sources)
        .await
        .unwrap();
    assert!(!repeated.env_key_identity.app_schema_installed);
    assert_eq!(
        platform_migrations::plan(&project, MigrationDatabase::Project, &sources)
            .await
            .unwrap()
            .recorded,
        installed.recorded
    );
    project.batch_execute("CREATE TABLE public.application_data (id int PRIMARY KEY, value text); INSERT INTO public.application_data VALUES (1, 'keep');").await.unwrap();
    other.batch_execute("CREATE TABLE public.application_data (id int PRIMARY KEY, value text); INSERT INTO public.application_data VALUES (2, 'other');").await.unwrap();
    let unchanged_other = snapshot(&other).await;
    let unchanged_system = snapshot(&system).await;
    let app_before: serde_json::Value = project
        .query_one("SELECT to_jsonb(t) FROM public.application_data t", &[])
        .await
        .unwrap()
        .get(0);
    let expected = sql::ExpectedDatabase::read(&platform, &document)
        .await
        .unwrap();
    {
        let held = LifecycleLock::acquire(system_database.url(), &document.triple())
            .await
            .unwrap();
        let mut output = Vec::new();
        let operation = sql::project_schema(
            &platform,
            &document,
            sql::ProjectSchemaRequest {
                expected: &expected,
                admin_url: system_database.url(),
                sources: &sources,
                apply: true,
                resolution: None,
            },
            &mut output,
        );
        tokio::pin!(operation);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(250), &mut operation)
                .await
                .is_err()
        );
        assert_eq!(
            platform_migrations::plan(&project, MigrationDatabase::Project, &sources)
                .await
                .unwrap()
                .recorded,
            installed.recorded
        );
        held.release().await;
        tokio::time::timeout(std::time::Duration::from_secs(5), operation)
            .await
            .unwrap()
            .unwrap();
    }
    let completed = platform_migrations::plan(&project, MigrationDatabase::Project, &sources)
        .await
        .unwrap();
    assert!(completed.migrations.pending.is_empty());
    let values: Vec<i32> = project
        .query("SELECT value FROM platform.dep5_probe ORDER BY value", &[])
        .await
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect();
    assert_eq!(values, [1, 2]);
    let app_after: serde_json::Value = project
        .query_one("SELECT to_jsonb(t) FROM public.application_data t", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(app_after, app_before);
    assert_eq!(snapshot(&other).await, unchanged_other);
    assert_eq!(snapshot(&system).await, unchanged_system);
    let row = project
        .query_one(
            "SELECT sha256, installed_at::text FROM platform.schema_baseline",
            &[],
        )
        .await
        .unwrap();
    assert_eq!((row.get::<_, String>(0), row.get::<_, String>(1)), baseline);
    // Project provisioning has its own initialization path into the shared executor.
    // Use the same database identity because its name is bound into SQL helpers.
    let mut fresh_postgres = wamn_test_postgres::start(&[]).unwrap();
    let fresh_registry = fresh_postgres.create_database("wamn_system").unwrap();
    let (mut fresh_system, fresh_system_connection) = connect(fresh_registry.url()).await;
    fresh_system
        .batch_execute("CREATE ROLE wamn_system NOLOGIN")
        .await
        .unwrap();
    install_control_store(&fresh_system).await.unwrap();
    wamn_environment_control::provision_system::set_platform_domain(
        &mut fresh_system,
        "example.invalid",
    )
    .await
    .unwrap();
    fresh_system
        .batch_execute(
            &wamn_control_provision::test_database::seed_org_project_env_sql(
                "dep5", "fixture", "dev",
            ),
        )
        .await
        .unwrap();
    let name =
        wamn_control_provision::project_env_database_name("dep5", "fixture", "dev", "aaaaaaa1");
    fresh_system
        .batch_execute(&format!(
            "CREATE DATABASE {} OWNER wamn_db_owner",
            postgres_protocol::escape::escape_identifier(&name)
        ))
        .await
        .unwrap();
    let fresh = fresh_postgres.database(&name).unwrap();
    reconcile_run_plane_with_migrations(
        ReconcileRunPlaneRequest {
            system_database_url: fresh_registry.url().into(),
            admin_database_url: fresh.url().into(),
            ..request()
        },
        &sources,
    )
    .await
    .unwrap();
    let (fresh_client, fresh_connection) = connect(fresh.url()).await;
    assert_eq!(
        schema_dump(fresh.url()).await,
        schema_dump(databases[0].url()).await
    );
    assert_eq!(
        platform_migrations::plan(&fresh_client, MigrationDatabase::Project, &sources)
            .await
            .unwrap()
            .recorded,
        completed.recorded
    );
    disconnect(fresh_client, fresh_connection).await;
    disconnect(fresh_system, fresh_system_connection).await;
    sources.push(MigrationSource {
        relative_path: format!("migrations/project/{:04}_pending.sql", sources.len() + 1),
        bytes: b"INSERT INTO platform.dep5_probe VALUES (3);".to_vec(),
    });
    let before_reapply = snapshot(&project).await;
    let repeated = reconcile_run_plane_with_migrations(request(), &sources)
        .await
        .unwrap();
    assert!(!repeated.env_key_identity.app_schema_installed);
    assert_eq!(snapshot(&project).await, before_reapply);
    let observed = wamn_package_control::schema_baseline::observe(
        &project,
        MigrationDatabase::Project,
        &sources,
    )
    .await
    .unwrap();
    assert_eq!(
        observed
            .installed
            .iter()
            .map(|file| &file.relative_path)
            .collect::<Vec<_>>(),
        completed
            .recorded
            .iter()
            .map(|file| &file.relative_path)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        observed
            .pending
            .iter()
            .map(|file| &file.relative_path)
            .collect::<Vec<_>>(),
        sources
            .last()
            .map(|file| &file.relative_path)
            .into_iter()
            .collect::<Vec<_>>()
    );
    // Restore the original selection for the unchanged apply and partial-installation contracts.
    sources.pop();
    // A read-only diff and an identical apply leave every record unchanged.
    for apply in [false, true] {
        let before = snapshot(&project).await;
        sql::project_schema(
            &platform,
            &document,
            sql::ProjectSchemaRequest {
                expected: &expected,
                admin_url: system_database.url(),
                sources: &sources,
                apply,
                resolution: None,
            },
            Vec::new(),
        )
        .await
        .unwrap();
        assert_eq!(snapshot(&project).await, before);
    }
    // A fresh database keeps committed progress after a later file fails.
    system
        .batch_execute(
            &wamn_control_provision::test_database::seed_org_project_env_sql(
                "dep5",
                "fixture",
                "interrupted",
            ),
        )
        .await
        .unwrap();
    let name = wamn_control_provision::project_env_database_name(
        "dep5",
        "fixture",
        "interrupted",
        "aaaaaaa1",
    );
    system
        .batch_execute(&format!(
            "CREATE DATABASE {} OWNER wamn_db_owner",
            postgres_protocol::escape::escape_identifier(&name)
        ))
        .await
        .unwrap();
    let interrupted = postgres.database(&name).unwrap();
    let mut failing = sources;
    failing.push(MigrationSource {
        relative_path: format!(
            "migrations/project/{:04}_interrupted.sql",
            failing.len() + 1
        ),
        bytes: b"CREATE TABLE platform.failed_probe (id int); SELECT 1/0;".to_vec(),
    });
    let interrupted_request = || ReconcileRunPlaneRequest {
        system_database_url: system_database.url().into(),
        admin_database_url: interrupted.url().into(),
        org: "dep5".into(),
        project: "fixture".into(),
        env: "interrupted".into(),
        schema: "wamn_run".into(),
        dry_run: false,
        changes: wamn_broker::Publisher::default(),
    };
    let error = reconcile_run_plane_with_migrations(interrupted_request(), &failing)
        .await
        .unwrap_err();
    let report = format!("{error:#}");
    let (interrupted_client, interrupted_connection) = connect(interrupted.url()).await;
    let partial =
        platform_migrations::plan(&interrupted_client, MigrationDatabase::Project, &failing)
            .await
            .unwrap();
    assert_eq!(partial.recorded.len(), failing.len() - 1);
    for file in &partial.recorded {
        assert!(report.contains(&file.relative_path), "{report}");
    }
    for file in &partial.migrations.pending {
        assert!(report.contains(&file.relative_path), "{report}");
    }
    assert!(
        interrupted_client
            .query_one("SELECT to_regclass('platform.failed_probe') IS NULL", &[])
            .await
            .unwrap()
            .get::<_, bool>(0)
    );
    reconcile_run_plane_with_migrations(interrupted_request(), &failing)
        .await
        .unwrap();
    assert_eq!(
        platform_migrations::plan(&interrupted_client, MigrationDatabase::Project, &failing)
            .await
            .unwrap()
            .recorded,
        partial.recorded
    );
    failing.last_mut().unwrap().bytes = b"CREATE TABLE platform.failed_probe (id int);".to_vec();
    let mut interrupted_document = document;
    interrupted_document.env = "interrupted".into();
    let expected = sql::ExpectedDatabase::read(&platform, &interrupted_document)
        .await
        .unwrap();
    sql::project_schema(
        &platform,
        &interrupted_document,
        sql::ProjectSchemaRequest {
            expected: &expected,
            admin_url: system_database.url(),
            sources: &failing,
            apply: true,
            resolution: None,
        },
        Vec::new(),
    )
    .await
    .unwrap();
    assert!(
        platform_migrations::plan(&interrupted_client, MigrationDatabase::Project, &failing)
            .await
            .unwrap()
            .migrations
            .pending
            .is_empty()
    );
    disconnect(interrupted_client, interrupted_connection).await;
    disconnect(project, project_connection).await;
    disconnect(other, other_connection).await;
    disconnect(system, system_connection).await;
}
