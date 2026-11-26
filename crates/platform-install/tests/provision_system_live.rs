//! Live test of `provision-system` against a disposable PostgreSQL server.
//!
//! Each test starts its own server, because the verb changes cluster-wide
//! roles and the PUBLIC CONNECT base grants of every database.

use sqlx::Row as _;

use wamn_platform_install::provision_system::{SystemState, install_control_store, system_state};

async fn connect(url: &str) -> sqlx::PgConnection {
    wamn_postgres::connect(url).await.unwrap()
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
          'databases', (SELECT jsonb_agg(jsonb_build_array(datname, datacl::text) ORDER BY datname) FROM pg_database))
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
#[ignore = "requires: docker"]
async fn bootstrap_is_atomic_and_uses_database_facts() {
    let postgres = wamn_test_infrastructure::containers::start_postgres()
        .await
        .unwrap();
    postgres.create_system_role().await.unwrap();
    let url = postgres.create_database("fixture_1").await.unwrap();
    let mut client = connect(url.as_str()).await;
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
            .and_then(sqlx::error::DatabaseError::code)
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
    let wrong_history: Vec<String> = sqlx::query_scalar(
        "SELECT format('%I.%I', schemaname, tablename) FROM pg_tables
         WHERE schemaname IN ('registry', 'identity')
           AND tablename::text !~ '_history$'
           AND ((schemaname = 'registry'
                OR schemaname = 'identity' AND NOT starts_with(tablename::text, 'authentication_event')
                   AND tablename NOT IN (
                    'session_key', 'nats_key', 'password_login', 'password_token',
                    'password_attempt', 'renewal_credential', 'session_signing_state'
                )) <> EXISTS (
               SELECT FROM pg_trigger
               WHERE tgrelid = to_regclass(format('%I.%I', schemaname, tablename))
                 AND tgfoid = 'wamn_history.log_row_change()'::regprocedure
           )
           OR (schemaname = 'identity' AND starts_with(tablename::text, 'authentication_event')
               AND (to_regclass(format('%I.%I', schemaname, tablename || '_history')) IS NOT NULL
                    OR EXISTS (SELECT FROM pg_trigger
                        WHERE tgrelid = to_regclass(format('%I.%I', schemaname, tablename))
                          AND tgfoid = 'wamn_history.stamp_row()'::regprocedure))))",
    )
    .fetch_all(&mut client)
    .await
    .unwrap();
    assert!(wrong_history.is_empty(), "{wrong_history:?}");
    let recorded_seed: bool = sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT FROM registry.target t
            JOIN registry.target_history h ON h.row_key = jsonb_build_object('id', t.id)
            WHERE t.name = 'cloud' AND h.type = 'insert'
              AND h.changed_by = $1 AND h.operation = 'wamn:provisioning'
         )",
    )
    .bind(sqlx::types::Uuid::from(
        wamn_principal::PlatformComponent::Provisioning.principal_id(),
    ))
    .fetch_one(&mut client)
    .await
    .unwrap();
    assert!(recorded_seed);
    let org = "fixture-org".parse().unwrap();
    let logins =
        wamn_platform_install::provision_org::create_org_logins(&mut client, &org, "fixture_1")
            .await
            .unwrap();
    for login in logins {
        let limit: i32 = sqlx::query_scalar("SELECT rolconnlimit FROM pg_roles WHERE rolname=$1")
            .bind(login.login.as_str())
            .fetch_one(&mut client)
            .await
            .unwrap();
        assert_eq!(limit, -1, "{}", login.login);
    }
    let concurrent_database = postgres.create_database("fixture_2").await.unwrap();
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

    let partial_database = postgres.create_database("fixture_extra").await.unwrap();
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
