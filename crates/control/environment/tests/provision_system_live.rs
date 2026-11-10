//! Live test of `provision-system` against a disposable PostgreSQL server.
//!
//! The test holds the process lock of its server, because the verb changes
//! cluster-wide roles and the PUBLIC CONNECT floor of every database.

use std::os::unix::fs::PermissionsExt as _;
use tokio::task::JoinHandle;
use tokio_postgres::NoTls;

use wamn_environment_control::provision_system::{
    EmitProvisionerRequest, ProvisionSystemRequest, SystemState, emit_provisioner_credential,
    install_control_store, provision_system, system_state,
};
use wamn_test_infrastructure::locked_database;

#[tokio::test]
async fn provision_system_repeats_without_changing_the_platform_domain() {
    let url = locked_database::database(wamn_test_postgres::database);
    let (client, connection) = tokio_postgres::connect(&url, NoTls)
        .await
        .expect("connect to the disposable system database");
    let connection = tokio::spawn(connection);
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
            wamn_pg_core::quote_ident(&recorded.database),
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
    wamn_platform_identity::auth_changes::AuthChanges::default()
        .publish(drops)
        .await;
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
            wamn_pg_core::quote_ident(&wamn_control_provision::package_owner_role(
                wamn_fixture_package::PACKAGE_ID
            ))
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
            changes: wamn_platform_identity::auth_changes::AuthChanges::default(),
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

    install_control_store(&client).await.unwrap();
    assert_eq!(system_state(&client).await.unwrap(), SystemState::Complete);
    let concurrent_database = wamn_test_postgres::database();
    let (first, first_connection) = connect(concurrent_database.url()).await;
    let (second, second_connection) = connect(concurrent_database.url()).await;
    let (left, right) = tokio::join!(
        install_control_store(&first),
        install_control_store(&second)
    );
    left.unwrap();
    right.unwrap();
    assert_eq!(system_state(&first).await.unwrap(), SystemState::Complete);
    let concurrent = snapshot(&first).await;
    install_control_store(&first).await.unwrap();
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
    scope: wamn_control_provision::schema_migrations::MigrationScope,
) -> (
    wamn_test_postgres::Database,
    tokio_postgres::Client,
    JoinHandle<Result<(), tokio_postgres::Error>>,
) {
    use wamn_control_provision::schema_migrations::MigrationScope;
    let database = match scope {
        MigrationScope::System => wamn_test_postgres::database(),
        MigrationScope::Project => wamn_project_state::test_database::tenant_platform(),
    };
    let (client, connection) = connect(database.url()).await;
    match scope {
        MigrationScope::System => {
            client.batch_execute("DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_system') THEN CREATE ROLE wamn_system NOLOGIN; END IF; END $$;").await.unwrap();
            install_control_store(&client).await.unwrap();
        }
        MigrationScope::Project => {
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
    let owner = match scope {
        MigrationScope::System => "wamn_system",
        MigrationScope::Project => "wamn_db_owner",
    };
    client
        .batch_execute(&format!(
            "ALTER DATABASE {} OWNER TO {owner}",
            wamn_pg_core::quote_ident(database.name())
        ))
        .await
        .unwrap();
    if scope == MigrationScope::Project {
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
    use wamn_control_provision::schema_migrations::MigrationScope;
    use wamn_environment_control::platform_migrations::{MigrationTarget, apply, plan};
    use wamn_schema_control::{MigrationOutcome, MigrationSource};

    let _lock = wamn_test_postgres::lock();
    for scope in [MigrationScope::System, MigrationScope::Project] {
        let (database, mut client, mut connection) = migration_database(scope).await;
        let (registry, system, system_connection) =
            migration_database(MigrationScope::System).await;
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
        let target = match scope {
            MigrationScope::System => MigrationTarget::System,
            MigrationScope::Project => MigrationTarget::Project(&lifecycle),
        };
        let (name, schema, owner) = match scope {
            MigrationScope::System => ("system", "registry", "wamn_system"),
            MigrationScope::Project => ("project", "platform", "wamn_db_owner"),
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
        assert!(plan(&client, scope, &[]).await.unwrap().adoption);
        assert!(client.query_one("SELECT to_regclass('public.migration_effect') IS NULL AND to_regclass('public.unsafe_a') IS NULL AND to_regclass('public.unsafe_b') IS NULL", &[]).await.unwrap().get::<_, bool>(0));

        // A failed first transactional file rolls back its DDL and history creation.
        let failing = vec![source(
            1,
            "CREATE TABLE public.failed_effect(id int); SELECT 1/0;".into(),
        )];
        assert!(apply(&mut client, target, &failing, None).await.is_err());
        assert!(plan(&client, scope, &[]).await.unwrap().adoption);
        assert!(
            client
                .query_one("SELECT to_regclass('public.failed_effect') IS NULL", &[])
                .await
                .unwrap()
                .get::<_, bool>(0)
        );

        // Read-only planning describes adoption and pending files without writing.
        let files = vec![second, first];
        let preview = plan(&client, scope, &files).await.unwrap();
        assert!(preview.adoption);
        assert!(preview.recorded.is_empty());
        assert!(plan(&client, scope, &[]).await.unwrap().adoption);
        assert_eq!(apply(&mut client, target, &[], None).await.unwrap(), 0);
        let adopted = plan(&client, scope, &files).await.unwrap();
        assert!(!adopted.adoption);
        assert!(
            adopted.recorded.is_empty(),
            "adoption claims no executed file"
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
            plan(&client, scope, &files)
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
        let partial = plan(&client, scope, &interrupted).await.unwrap();
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
        let completed = plan(&client, scope, &interrupted).await.unwrap();
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
            assert_eq!(
                record.sha256,
                wamn_execution_contract::bytes_sha256(&source.bytes)
            );
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
        if scope == MigrationScope::System {
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
            plan(&client, scope, &interrupted).await.unwrap().recorded,
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
        let error = plan(&client, scope, &interrupted)
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
    use wamn_control_provision::schema_migrations::MigrationScope;
    use wamn_environment_control::platform_migrations::{MigrationTarget, apply, plan};
    use wamn_package_control::migrations::{MigrationResolution, Resolution};
    use wamn_schema_control::{MigrationOutcome, MigrationSource};

    let _lock = wamn_test_postgres::lock();
    for scope in [MigrationScope::System, MigrationScope::Project] {
        let (_database, mut client, connection) = migration_database(scope).await;
        let (registry, system, system_connection) =
            migration_database(MigrationScope::System).await;
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
        let target = match scope {
            MigrationScope::System => MigrationTarget::System,
            MigrationScope::Project => MigrationTarget::Project(&lifecycle),
        };
        let (name, schema) = match scope {
            MigrationScope::System => ("system", "registry"),
            MigrationScope::Project => ("project", "platform"),
        };
        let path = format!("migrations/{name}/0001_index.sql");
        let mut files = vec![MigrationSource { relative_path: path.clone(),
            bytes: b"-- wamn:no-transaction\nCREATE UNIQUE INDEX CONCURRENTLY migration_effect_idx ON public.migration_effect(id);".to_vec() }];
        // A nontransactional first failure retains adoption and its durable started row.
        assert!(apply(&mut client, target, &files, None).await.is_err());
        let uncertain = plan(&client, scope, &files).await.unwrap();
        assert!(!uncertain.adoption);
        assert_eq!(uncertain.recorded.len(), files.len());
        assert_eq!(uncertain.recorded[0].outcome, MigrationOutcome::Started);
        assert_eq!(uncertain.migrations.pending[0].relative_path, path);
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
            plan(&client, scope, &files)
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
            plan(&client, scope, &files)
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
            plan(&client, scope, &files)
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
