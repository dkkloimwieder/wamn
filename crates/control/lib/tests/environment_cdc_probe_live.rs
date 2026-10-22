//! Live test of the CDC ensure's observation (docs/plan/platform-deploy.md
//! §10.1 step 4, `wamn-1d3gr`): the publication alone is the observation, so
//! a reader row that a failed apply left behind does not make the next apply
//! skip CDC, and recording the reader on every apply leaves one row.

use tokio_postgres::NoTls;

use wamn_control::enable_cdc_project_env::record_event_reader;
use wamn_control::environment::ensure::cdc_in_place;
use wamn_control_provision::{cdc_object_name, sql};
use wamn_control_registry::Triple;
use wamn_control_registry::sql::{
    stamp_env_policy_sql, upsert_org_sql, upsert_project_env_sql, upsert_project_sql,
};

const ORG: &str = "cp0";
const PROJECT: &str = "app";
const ENV: &str = "dev";
const INSTANCE: &str = "k3m9x2p7";

#[tokio::test]
async fn the_publication_alone_is_the_observation_and_the_reader_is_recorded_once() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::database();
    let (db, connection) = tokio_postgres::connect(database.url(), NoTls)
        .await
        .expect("connect the test database");
    tokio::spawn(connection);
    db.batch_execute(
        "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_system') THEN \
           CREATE ROLE wamn_system NOLOGIN; \
         END IF; END $$;",
    )
    .await
    .expect("the registry owner role");
    db.batch_execute(wamn_control_provision::SYSTEM_SCHEMA_SQL)
        .await
        .expect("install the system schema");
    db.execute(upsert_org_sql(), &[&ORG, &"pooled", &"wamn-pg"])
        .await
        .expect("org row");
    db.execute(upsert_project_sql(), &[&ORG, &PROJECT])
        .await
        .expect("project row");
    db.execute(
        stamp_env_policy_sql(),
        &[
            &ORG,
            &ENV,
            &r#"{"kind":"pool"}"#,
            &0i32,
            &1i32,
            &"1Gi",
            &"250m",
            &"256Mi",
            &"postgres:18",
            &"",
            &"",
            &"off",
            &"standard",
        ],
    )
    .await
    .expect("env-policy row");
    db.execute(
        upsert_project_env_sql(),
        &[
            &ORG,
            &PROJECT,
            &ENV,
            &"wamn-db-cp0--app--dev",
            &None::<&str>,
            &INSTANCE,
            &false,
        ],
    )
    .await
    .expect("project-env row");

    db.batch_execute(
        "ALTER TABLE registry.project_envs OWNER TO wamn_system; \
         ALTER TABLE registry.event_readers OWNER TO wamn_system",
    )
    .await
    .expect("registry owner");

    let triple = Triple::new(ORG, PROJECT, ENV);
    let cdc_name = cdc_object_name(ORG, PROJECT, ENV, INSTANCE);
    let in_place = || cdc_in_place(&db, &cdc_name, "app");
    let record = || record_event_reader(database.url(), &triple, "app", &cdc_name, None);
    let readers = || async {
        db.query_one(
            "SELECT count(*) FROM registry.event_readers WHERE org = $1",
            &[&ORG],
        )
        .await
        .expect("count the reader rows")
        .get::<_, i64>(0)
    };

    // No publication: not in place, with or without a reader row.
    assert!(!in_place().await.expect("probe"), "no publication, no row");
    record().await.expect("record the reader");
    assert!(
        !in_place().await.expect("probe"),
        "a reader row a failed apply left must not skip CDC"
    );

    // The publication is the observation.
    db.batch_execute(&format!(
        "{};\n{}",
        sql::ensure_schema_sql("app"),
        sql::create_publication_sql(&cdc_name, "app")
    ))
    .await
    .expect("the publication");
    assert!(in_place().await.expect("probe"), "CDC is in place");

    assert!(!cdc_in_place(&db, &cdc_name, "another").await.unwrap());
    db.batch_execute(&format!(
        "CREATE SCHEMA another; ALTER PUBLICATION \"{cdc_name}\" SET TABLES IN SCHEMA another"
    ))
    .await
    .unwrap();
    assert!(!in_place().await.unwrap());
    db.batch_execute(&format!(
        "ALTER PUBLICATION \"{cdc_name}\" SET TABLES IN SCHEMA app"
    ))
    .await
    .unwrap();
    assert!(in_place().await.unwrap());

    // Every apply records the reader; recording it again leaves one row.
    record().await.expect("record the reader again");
    assert_eq!(readers().await, 1);
}
