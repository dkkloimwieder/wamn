//! Live test of `provision-system` against a disposable PostgreSQL server.
//!
//! The test holds the process lock of its server, because the verb changes
//! cluster-wide roles and the PUBLIC CONNECT floor of every database.

use std::os::unix::fs::PermissionsExt as _;
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
    tokio::spawn(async move {
        let _ = connection.await;
    });
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
    let recorded = wamn_environment_control::provision_project_env::provision_project_env(
        &wamn_environment_control::provision_project_env::ProvisionProjectEnvRequest {
            changes: wamn_platform_identity::auth_changes::AuthChanges::default(),
            org: "applyprobe".into(),
            project: "created".into(),
            env: "dev".into(),
            policy: Some("dev".into()),
            target: "cloud".into(),
            disposable: None,
            system_database_url: Some(apply_secret["stringData"]["url"].as_str().unwrap().into()),
            cluster: Some("test-cluster".into()),
            connection_limit: None,
            cluster_namespace: "platform".into(),
            namespace: "hosts".into(),
            secret_namespace: Some("hosts".into()),
            emit_database: None,
            emit_role_sql: None,
            emit_privilege_sql: None,
            emit_secret: None,
            pat_issuer: wamn_identity_client::PatIssuerConfig::default(),
            emit_management_author_pat_secret: None,
            emit_operator_pat_secret: None,
        },
    )
    .await
    .expect("the apply login records the environment without owner authority");
    assert!(!recorded.database.is_empty());

    client
        .batch_execute(&wamn_control_provision::sql::ensure_platform_group_role_sql())
        .await
        .unwrap();
    client
        .batch_execute(&format!(
            "CREATE DATABASE {} OWNER wamn_db_owner",
            wamn_pg_core::quote_ident(&recorded.database),
        ))
        .await
        .unwrap();
    client
        .batch_execute(&format!(
            "GRANT CONNECT ON DATABASE {} TO wamn_apply; \
             GRANT wamn_platform TO wamn_db_owner WITH INHERIT TRUE, SET FALSE",
            wamn_pg_core::quote_ident(&recorded.database),
        ))
        .await
        .unwrap();
    let mut project_admin = parsed_admin.clone();
    project_admin.set_path(&recorded.database);
    let (project, project_driver) = tokio_postgres::connect(project_admin.as_str(), NoTls)
        .await
        .unwrap();
    let project_driver = tokio::spawn(project_driver);
    project
        .batch_execute(&wamn_control_provision::sql::install_platform_extensions_sql())
        .await
        .unwrap();
    drop(project);
    project_driver.await.unwrap().unwrap();
    let mut project_apply =
        url::Url::parse(apply_secret["stringData"]["url"].as_str().unwrap()).unwrap();
    project_apply.set_path(&recorded.database);
    wamn_environment_control::reconcile_run_plane::reconcile_run_plane(
        wamn_environment_control::reconcile_run_plane::ReconcileRunPlaneRequest {
            system_database_url: apply_secret["stringData"]["url"].as_str().unwrap().into(),
            admin_database_url: project_admin.to_string(),
            org: "applyprobe".into(),
            project: "created".into(),
            env: "dev".into(),
            schema: "wamn_run".into(),
            dry_run: false,
            changes: wamn_platform_identity::auth_changes::AuthChanges::default(),
        },
    )
    .await
    .expect("ensure reconciles with separate admin and apply connections");
    let document = wamn_environment_control::environment::document::EnvironmentDocument::from_json(
        br#"{"org":"applyprobe","project":"created","env":"dev","target":"cloud","release":"none","route_host":"created.example.test","policy":"dev","integrations":{},"applications":{}}"#,
    )
    .unwrap();
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
    tokio::spawn(async move {
        let _ = worker_connection.await;
    });
    let user: String = worker
        .query_one("SELECT current_user::text", &[])
        .await
        .expect("read the current user")
        .get(0);
    assert_eq!(user, "wamn_provisioner");
    std::fs::remove_dir_all(&directory).expect("remove the output directory");
}

async fn connect(url: &str) -> tokio_postgres::Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await.unwrap();
    tokio::spawn(async move {
        connection.await.unwrap();
    });
    client
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
    let client = connect(&url).await;
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
    let first = connect(concurrent_database.url()).await;
    let second = connect(concurrent_database.url()).await;
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

    let partial_database = wamn_test_postgres::database();
    let partial = connect(partial_database.url()).await;
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
}
