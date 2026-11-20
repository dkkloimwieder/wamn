//! Live test of `provision-system` against a disposable PostgreSQL server.
//!
//! The test holds the process lock of its server, because the verb changes
//! cluster-wide roles and the PUBLIC CONNECT base grants of every database.

use sqlx::Row as _;

use wamn_environment::provision_system::{
    SystemState, initialize_system, install_control_store, system_state,
};

async fn connect(url: &str) -> sqlx::PgConnection {
    wamn_postgres::connect(url).await.unwrap()
}

#[sqlx::test(migrations = false)]
async fn platform_initialization_applies_pending_migrations_on_upgrade(pool: sqlx::PgPool) {
    use sqlx::SqlSafeStr as _;
    use sqlx::migrate::{Migration, MigrationType};
    use wamn_provision::migrations::MigrationDatabase;
    use wamn_environment::platform_migrations;
    let _lock = wamn_test_postgres::lock();
    let url = wamn_test_postgres::url(&pool).to_string();
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
    let migration_input = wamn_provision::test_database::platform_migration_input(
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

// Enumerate catalog and durable rows. W38 records each native call in tx_identity.
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
          AND c.oid <> COALESCE(to_regclass('platform.tx_identity'), 0)
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
#[ignore = "requires: postgres"]
async fn bootstrap_is_atomic_and_uses_database_facts() {
    let _lock = wamn_test_postgres::lock();
    let mut case_server = wamn_test_postgres::start(&[]).unwrap();
    let url = url::Url::parse(
        case_server
            .create_database("fixture_1")
            .await
            .unwrap()
            .url(),
    )
    .unwrap()
    .to_string();
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

    // Refuse a revoke during the final bootstrap stage.
    // This is a server-side SQL failure, not a replacement bootstrap or validator.
    sqlx::raw_sql(
        r"
        CREATE FUNCTION public.reject_final_grant() RETURNS event_trigger LANGUAGE plpgsql AS $$
        BEGIN
          IF to_regnamespace('catalog') IS NOT NULL THEN
            PERFORM 1 / 0;
          END IF;
        END $$;
        CREATE EVENT TRIGGER reject_final_grant ON ddl_command_end WHEN TAG IN ('REVOKE')
          EXECUTE FUNCTION public.reject_final_grant();
    ",
    )
    .execute(&mut client)
    .await
    .map(drop)
    .unwrap();
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
    let concurrent_database = url::Url::parse(
        case_server
            .create_database("fixture_2")
            .await
            .unwrap()
            .url(),
    )
    .unwrap();
    let mut first = connect(concurrent_database.as_str()).await;
    let mut second = connect(concurrent_database.as_str()).await;
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

    let partial_database = url::Url::parse(
        case_server
            .create_database("fixture_extra")
            .await
            .unwrap()
            .url(),
    )
    .unwrap();
    let mut partial = connect(partial_database.as_str()).await;
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
