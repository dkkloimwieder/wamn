//! Live test of the CDC ensure's observation (docs/architecture/overview.md#environment-lifecycle,
//! `wamn-1d3gr`): the publication alone is the observation, so
//! a reader row that a failed apply left behind does not make the next apply
//! skip CDC, and recording the reader on every apply leaves one row.

use sqlx::Row as _;

use wamn_control_provision::{cdc_object_name, sql};
use wamn_environment::enable_cdc_project_env::record_event_reader;
use wamn_environment::environment::ensure::cdc_in_place;
use wamn_registry::Triple;
use wamn_registry::sql::{
    stamp_env_policy_sql, upsert_org_sql, upsert_project_env_with_policy_sql, upsert_project_sql,
};

const ORG: &str = "cp0";
const PROJECT: &str = "app";
const ENV: &str = "dev";
const INSTANCE: &str = "k3m9x2p7";

#[tokio::test]
async fn the_publication_alone_is_the_observation_and_the_reader_is_recorded_once() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::database();
    let mut db = wamn_postgres::connect(database.url())
        .await
        .expect("connect the test database");

    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_system') THEN \
           CREATE ROLE wamn_system NOLOGIN NOINHERIT; \
         END IF; END $$;")
            .to_string(),
    ))
    .execute(&mut db)
    .await
    .map(drop)
    .expect("the registry owner role");
    sqlx::raw_sql(wamn_control_provision::SYSTEM_SCHEMA_SQL)
        .execute(&mut db)
        .await
        .map(drop)
        .expect("install the system schema");
    sqlx::raw_sql("SET app.user_id = '770df186-ac15-579e-b46b-c297cae2011b'")
        .execute(&mut db)
        .await
        .map(drop)
        .unwrap();
    sqlx::query(sqlx::AssertSqlSafe((upsert_org_sql()).to_string()))
        .bind(&ORG)
        .bind(&"pooled")
        .bind(&"wamn-pg")
        .execute(&mut db)
        .await
        .map(|result| result.rows_affected())
        .expect("org row");
    sqlx::query(sqlx::AssertSqlSafe((upsert_project_sql()).to_string()))
        .bind(&ORG)
        .bind(&PROJECT)
        .execute(&mut db)
        .await
        .map(|result| result.rows_affected())
        .expect("project row");
    sqlx::query(sqlx::AssertSqlSafe((stamp_env_policy_sql()).to_string()))
        .bind(&ORG)
        .bind(&ENV)
        .bind(&r#"{"type":"pool"}"#)
        .bind(&0i32)
        .bind(&1i32)
        .bind(&"1Gi")
        .bind(&"250m")
        .bind(&"256Mi")
        .bind(&"postgres:18")
        .bind(&"")
        .bind(&"")
        .bind(&"off")
        .bind(&"standard")
        .execute(&mut db)
        .await
        .map(|result| result.rows_affected())
        .expect("env-policy row");
    sqlx::query(sqlx::AssertSqlSafe(
        (upsert_project_env_with_policy_sql()).to_string(),
    ))
    .bind(&ORG)
    .bind(&PROJECT)
    .bind(&ENV)
    .bind(&"wamn-db-cp0--dev--app")
    .bind(&None::<&str>)
    .bind(&INSTANCE)
    .bind(&false)
    .bind(&ENV)
    .bind(&Option::<String>::None)
    .execute(&mut db)
    .await
    .map(|result| result.rows_affected())
    .expect("project-env row");

    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("ALTER TABLE registry.environment_projects OWNER TO wamn_system; \
         ALTER TABLE registry.event_readers OWNER TO wamn_system")
            .to_string(),
    ))
    .execute(&mut db)
    .await
    .map(drop)
    .expect("registry owner");

    let triple = Triple::new(ORG, ENV, PROJECT);
    let cdc_name = cdc_object_name(ORG, PROJECT, ENV, INSTANCE);
    let schemas = vec!["app".to_owned(), "other".to_owned()];
    let observations = wamn_postgres::pool(database.url(), 4).await.unwrap();
    let in_place = async || {
        let mut observation = observations.acquire().await?;
        cdc_in_place(&mut observation, &cdc_name, &schemas).await
    };
    let record = || record_event_reader(database.url(), &triple, &schemas, &cdc_name, None);
    let readers = || async {
        sqlx::query("SELECT count(*) FROM registry.event_readers WHERE org = $1")
            .bind(&ORG)
            .fetch_one(&observations)
            .await
            .expect("count the reader rows")
            .get::<i64, _>(0)
    };

    // No publication: not in place, with or without a reader row.
    assert!(!in_place().await.expect("probe"), "no publication, no row");
    record().await.expect("record the reader");
    assert!(
        !in_place().await.expect("probe"),
        "a reader row a failed apply left must not skip CDC"
    );

    // The publication is the observation.
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "{};\n{};\n{}",
            sql::ensure_schema_sql("app"),
            sql::ensure_schema_sql("other"),
            sql::create_publication_schemas_sql(&cdc_name, &schemas)
        ))
            .to_string(),
    ))
    .execute(&mut db)
    .await
    .map(drop)
    .expect("the publication");
    assert!(in_place().await.expect("probe"), "CDC is in place");

    assert!(
        !cdc_in_place(&mut db, &cdc_name, &["another".to_owned()])
            .await
            .unwrap()
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "CREATE SCHEMA another; ALTER PUBLICATION \"{cdc_name}\" SET TABLES IN SCHEMA another"
        ))
            .to_string(),
    ))
    .execute(&mut db)
    .await
    .map(drop)
    .unwrap();
    assert!(!in_place().await.unwrap());
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "ALTER PUBLICATION \"{cdc_name}\" SET TABLES IN SCHEMA app, other"
    )))
    .execute(&mut db)
    .await
    .map(drop)
    .unwrap();
    assert!(in_place().await.unwrap());

    // Every apply records the reader; recording it again leaves one row.
    record().await.expect("record the reader again");
    assert_eq!(readers().await, 1);
    assert!(
        cdc_in_place(&mut db, &cdc_name, &["other".to_owned(), "app".to_owned()])
            .await
            .unwrap()
    );
    let registered: Vec<String> =
        sqlx::query("SELECT schema FROM registry.event_readers WHERE org = $1")
            .bind(&ORG)
            .fetch_one(&mut db)
            .await
            .unwrap()
            .get(0);
    assert_eq!(registered, schemas);
}
