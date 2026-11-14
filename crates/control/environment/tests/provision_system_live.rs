//! Live test of `provision-system` against a disposable PostgreSQL server.
//!
//! The test holds the process lock of its server, because the verb changes
//! cluster-wide roles and the PUBLIC CONNECT base grants of every database.

use sqlx::Row as _;
use std::os::unix::fs::PermissionsExt as _;

use wamn_environment::provision_system::{
    ProvisionSystemRequest, SystemState, initialize_system, install_control_store,
    provision_system, system_state,
};
use wamn_test_infrastructure::locked_database;

#[tokio::test]
async fn provision_system_repeats_without_changing_the_platform_domain() {
    let url = locked_database::database(wamn_test_postgres::database);
    let mut client = connect(&url).await;
    // CloudNativePG creates the owner role at bootstrap. The test does it here.
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_system') THEN \
               CREATE ROLE wamn_system NOLOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE \
                 NOREPLICATION NOBYPASSRLS; \
             END IF; END $$;")
            .to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)
    .expect("create the wamn_system role");
    // CloudNativePG makes wamn_system the database owner, which can connect.
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("DO $$ BEGIN EXECUTE format('GRANT CONNECT ON DATABASE %I TO wamn_system', \
                                        current_database()); END $$;")
            .to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)
    .expect("let wamn_system connect as the owner can");
    let request = ProvisionSystemRequest {
        migration_database_url: Some(
            wamn_control_provision::test_database::platform_migration_input(
                &*url,
                wamn_control_provision::migrations::MigrationDatabase::System,
            )
            .await
            .unwrap(),
        ),
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
        &mut client,
        "registry.meta",
        "true",
        &actor,
        &actor,
    )
    .await;
    let domain: Option<String> = sqlx::query("SELECT platform_domain FROM registry.meta")
        .fetch_one(&mut client)
        .await
        .expect("read the platform domain")
        .get(0);
    assert_eq!(domain.as_deref(), Some("wamn.example.test"));

    let before = snapshot(&mut client).await;
    provision_system(&request)
        .await
        .expect("repeat the same domain");
    assert_eq!(
        system_state(&mut client).await.unwrap(),
        SystemState::Complete
    );
    assert_eq!(
        snapshot(&mut client).await,
        before,
        "repeat changes no catalog or data rows"
    );

    let apply_input =
        wamn_test_infrastructure::credentials::apply_login(&request.system_database_url)
            .await
            .unwrap();
    let mut apply = wamn_postgres::connect(&apply_input).await.unwrap();

    assert_eq!(
        sqlx::query("SELECT current_user::text")
            .fetch_one(&mut apply)
            .await
            .unwrap()
            .get::<String, _>(0),
        "wamn_apply"
    );
    assert_eq!(
        sqlx::query("SELECT platform_domain FROM registry.meta")
            .fetch_one(&mut apply)
            .await
            .unwrap()
            .get::<String, _>(0),
        "wamn.example.test"
    );
    assert!(
        sqlx::raw_sql("SET ROLE wamn_system")
            .execute(&mut apply)
            .await
            .map(drop)
            .is_err()
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::test_database::seed_org_project_env_sql(
            "applyprobe",
            "existing",
            "dev",
        ))
            .to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)
    .unwrap();
    let document = wamn_environment::environment::document::EnvironmentDocument::from_json(
        br#"{"org":"applyprobe","project":"created","env":"dev","target":"cloud","policy":"dev","revocation_seconds":60,"integrations":{},"applications": {}}"#,
    ).unwrap();
    let platform = wamn_environment::environment::Platform {
        system_database_url: apply_input,
        target: wamn_release_control::release_chart::Target {
            kubeconfig: std::path::PathBuf::new(),
            context: String::new(),
            namespace: "hosts".into(),
        },
        chart: std::path::PathBuf::new(),
        chart_download: None,
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
    let recorded = wamn_environment::environment::ensure::provision(
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
        !sqlx::query("SELECT EXISTS (SELECT FROM pg_database WHERE datname=$1)")
            .bind(&recorded.database)
            .fetch_one(&mut client)
            .await
            .unwrap()
            .get::<bool, _>(0),
        "CNPG, not provisioning SQL, creates the database"
    );
    let retry = wamn_environment::environment::ensure::provision(
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

    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "CREATE DATABASE {} OWNER wamn_db_owner",
            postgres_protocol::escape::escape_identifier(&recorded.database),
        ))
            .to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)
    .unwrap();
    let mut project_apply = url::Url::parse(&platform.system_database_url).unwrap();
    project_apply.set_path(&recorded.database);
    wamn_environment::environment::ensure::ensure_project_database(
        &platform,
        &document,
        &[],
        &mut client,
        &url,
    )
    .await
    .expect("project initialization uses separate admin and apply connections");
    let drops = wamn_environment::environment::bindings::project_authority(
        &platform.system_database_url,
        project_apply.as_str(),
        &document,
    )
    .await
    .expect("the apply login refreshes the projection as the project owner");
    wamn_broker::Publisher::default().publish(drops).await;
    let package = wamn_fixture_package::package_root();
    wamn_environment::apply_package::apply_package(
        wamn_environment::apply_package::ApplyPackageRequest {
            migration_database_url: wamn_control_provision::test_database::package_migration_input(
                &wamn_postgres::database_configuration(&url, &recorded.database).unwrap(),
                &"applyprobe",
                &"created",
                &"dev",
            )
            .await
            .unwrap(),
            component_source: None,
            package: wamn_environment::apply_package::PackageRoot {
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

    drop(client);
}

async fn connect(url: &str) -> sqlx::PgConnection {
    wamn_postgres::connect(url).await.unwrap()
}

#[tokio::test]
async fn platform_initialization_runs_sqlx_once_and_explicit_apply_advances_it() {
    use sqlx::SqlSafeStr as _;
    use sqlx::migrate::{Migration, MigrationType};
    use wamn_control_provision::migrations::MigrationDatabase;
    use wamn_environment::platform_migrations;
    let url = locked_database::database(wamn_test_postgres::database);
    let mut client = connect(&url).await;
    sqlx::raw_sql("DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_system') THEN CREATE ROLE wamn_system NOLOGIN NOINHERIT; END IF; END $$;")
        .execute(&mut client).await.unwrap();
    let source = |version, sql: &str| {
        Migration::new(
            version,
            "probe".into(),
            MigrationType::Simple,
            sqlx::AssertSqlSafe(sql.to_owned()).into_sql_str(),
            false,
        )
    };
    let first = source(
        10,
        "CREATE TABLE registry.migration_probe(value int); INSERT INTO registry.migration_probe VALUES (1)",
    );
    let second = source(30, "INSERT INTO registry.migration_probe VALUES (2)");
    let migration_input = wamn_control_provision::test_database::platform_migration_input(
        &*url,
        MigrationDatabase::System,
    )
    .await
    .unwrap();
    assert!(
        platform_migrations::apply_reported(
            &mut client,
            platform_migrations::MigrationTarget::System,
            std::slice::from_ref(&first)
        )
        .await
        .is_err()
    );
    initialize_system(&mut client, &migration_input, std::slice::from_ref(&first))
        .await
        .unwrap();
    let sources = [first, second];
    initialize_system(&mut client, &migration_input, &sources)
        .await
        .unwrap();
    let mut migration = connect(&migration_input).await;
    let values: Vec<i32> =
        sqlx::query_scalar("SELECT value FROM registry.migration_probe ORDER BY value")
            .fetch_all(&mut client)
            .await
            .unwrap();
    assert_eq!(values, [1]);
    platform_migrations::apply_reported(
        &mut migration,
        platform_migrations::MigrationTarget::System,
        &sources,
    )
    .await
    .unwrap();
    let observed = wamn_environment::platform_migrations::observe(
        &mut client,
        MigrationDatabase::System,
        &sources,
    )
    .await
    .unwrap();
    assert!(
        observed.pending.is_empty() && observed.missing.is_empty() && observed.warnings.is_empty()
    );
    let values: Vec<i32> =
        sqlx::query_scalar("SELECT value FROM registry.migration_probe ORDER BY value")
            .fetch_all(&mut client)
            .await
            .unwrap();
    assert_eq!(values, [1, 2]);
    let owns: bool = sqlx::query_scalar("SELECT relowner=(SELECT oid FROM pg_roles WHERE rolname='wamn_system') FROM pg_class WHERE oid='registry._sqlx_migrations'::regclass").fetch_one(&mut client).await.unwrap();
    assert!(owns);
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
async fn snapshot(client: &mut sqlx::PgConnection) -> serde_json::Value {
    let catalog: serde_json::Value = sqlx::query(r"
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
    ").fetch_one(&mut *client).await.unwrap().get(0);
    let mut data = serde_json::Map::new();
    for row in sqlx::query(sqlx::AssertSqlSafe(
        (r"
        SELECT format('%I.%I', n.nspname, c.relname)
        FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
        WHERE c.relkind = 'r' AND n.nspname !~ '^pg_' AND n.nspname <> 'information_schema'
        ORDER BY 1
    ")
        .to_string(),
    ))
    .fetch_all(&mut *client)
    .await
    .unwrap()
    {
        let relation: String = row.get(0);
        let contents = sqlx::query(sqlx::AssertSqlSafe(format!(r"
            SELECT COALESCE(jsonb_agg(jsonb_build_array(to_jsonb(t), xmin::text) ORDER BY to_jsonb(t)::text), '[]')
            FROM {relation} t
        "))).fetch_one(&mut *client).await.unwrap();
        data.insert(relation, contents.get::<serde_json::Value, _>(0));
    }
    serde_json::json!({"catalog": catalog, "data": data})
}

#[tokio::test]
async fn bootstrap_is_atomic_and_uses_database_facts() {
    let url = locked_database::database(wamn_test_postgres::database);
    let mut client = connect(&url).await;
    sqlx::raw_sql("DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_system') THEN CREATE ROLE wamn_system NOLOGIN NOINHERIT; END IF; END $$;").execute(&mut client).await.map(drop).unwrap();
    assert_eq!(
        system_state(&mut client).await.unwrap(),
        SystemState::Absent
    );

    // Development resets retain the shared history schema.
    sqlx::raw_sql("CREATE SCHEMA wamn_history AUTHORIZATION wamn_system")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    assert_eq!(
        system_state(&mut client).await.unwrap(),
        SystemState::Absent
    );

    // Fail the final apply grant after both schema batches and baseline insertion.
    // This is a server-side SQL failure, not a replacement bootstrap or validator.
    sqlx::raw_sql(r"
        CREATE FUNCTION public.reject_final_grant() RETURNS event_trigger LANGUAGE plpgsql AS $$
        DECLARE granted boolean;
        BEGIN
          IF to_regclass('identity.principals') IS NOT NULL
             AND EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_apply') THEN
            EXECUTE 'SELECT has_column_privilege(''wamn_apply'', ''identity.org_memberships'', ''status'', ''UPDATE'')' INTO granted;
            IF granted THEN PERFORM 1 / 0; END IF;
          END IF;
        END $$;
        CREATE EVENT TRIGGER reject_final_grant ON ddl_command_end WHEN TAG IN ('GRANT')
          EXECUTE FUNCTION public.reject_final_grant();
    ").execute(&mut client).await.map(drop).unwrap();
    let before = snapshot(&mut client).await;
    let failure = install_control_store(&mut client).await.unwrap_err();
    assert_eq!(
        failure
            .downcast_ref::<sqlx::Error>()
            .unwrap()
            .as_database_error()
            .and_then(|e| e.code())
            .as_deref(),
        Some("22012")
    );
    assert_eq!(
        snapshot(&mut client).await,
        before,
        "late failure rolls back schema, roles, grants and data"
    );
    assert_eq!(
        system_state(&mut client).await.unwrap(),
        SystemState::Absent
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("DROP EVENT TRIGGER reject_final_grant; DROP FUNCTION public.reject_final_grant();")
            .to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)
    .unwrap();

    assert!(install_control_store(&mut client).await.unwrap());
    assert_eq!(
        system_state(&mut client).await.unwrap(),
        SystemState::Complete
    );
    let concurrent_database = wamn_test_postgres::database();
    let mut first = connect(concurrent_database.url()).await;
    let mut second = connect(concurrent_database.url()).await;
    let (left, right) = tokio::join!(
        install_control_store(&mut first),
        install_control_store(&mut second)
    );
    assert_ne!(left.unwrap(), right.unwrap(), "only one caller initializes");
    assert_eq!(
        system_state(&mut first).await.unwrap(),
        SystemState::Complete
    );
    let concurrent = snapshot(&mut first).await;
    assert!(!install_control_store(&mut first).await.unwrap());
    assert_eq!(snapshot(&mut first).await, concurrent);
    drop(first);
    drop(second);

    let partial_database = wamn_test_postgres::database();
    let mut partial = connect(partial_database.url()).await;
    sqlx::raw_sql("CREATE SCHEMA registry")
        .execute(&mut partial)
        .await
        .map(drop)
        .unwrap();
    assert_eq!(
        system_state(&mut partial).await.unwrap(),
        SystemState::Incomplete
    );
    let before = snapshot(&mut partial).await;
    assert!(install_control_store(&mut partial).await.is_err());
    assert_eq!(snapshot(&mut partial).await, before);
    drop(partial);

    drop(client);
}
