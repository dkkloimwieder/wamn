//! Live test of the CDC ensure's observation (docs/plan/platform-deploy.md
//! §10.1 step 4, `wamn-1d3gr`): a reader row that a failed apply left behind
//! does not make the next apply skip CDC; only the publication together with
//! the row does.

use tokio_postgres::NoTls;

use wamn_control::environment::ensure::cdc_in_place;
use wamn_control_provision::{cdc_object_name, event_stream_name, sql};
use wamn_control_registry::Triple;
use wamn_control_registry::sql::{
    stamp_env_policy_sql, upsert_event_reader_sql, upsert_org_sql, upsert_project_env_sql,
    upsert_project_sql,
};

const ORG: &str = "cp0";
const PROJECT: &str = "app";
const ENV: &str = "dev";
const INSTANCE: &str = "k3m9x2p7";

#[tokio::test]
async fn a_recorded_reader_without_its_publication_is_not_in_place() {
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

    let triple = Triple::new(ORG, PROJECT, ENV);
    let cdc_name = cdc_object_name(ORG, PROJECT, ENV, INSTANCE);
    let in_place = || cdc_in_place(&db, &db, &triple, &cdc_name);

    // An apply recorded the reader, then failed before the publication.
    db.execute(
        upsert_event_reader_sql(),
        &[
            &ORG,
            &PROJECT,
            &ENV,
            &cdc_name,
            &cdc_name,
            &event_stream_name(ORG, PROJECT, ENV),
            &"wamn-cdc-cp0--app--dev",
            &None::<&str>,
            &true,
            &"app",
        ],
    )
    .await
    .expect("event-reader row");
    assert!(
        !in_place().await.expect("probe"),
        "a recorded reader without its publication must make the next apply enable CDC"
    );

    // The next apply creates the publication and records the reader last.
    db.batch_execute(&format!(
        "{};\n{}",
        sql::ensure_schema_sql("app"),
        sql::create_publication_sql(&cdc_name, "app")
    ))
    .await
    .expect("the publication");
    assert!(in_place().await.expect("probe"), "CDC is in place");

    // A publication whose reader was never recorded is a partial ensure too.
    db.execute("DELETE FROM registry.event_readers WHERE org = $1", &[&ORG])
        .await
        .expect("drop the reader row");
    assert!(
        !in_place().await.expect("probe"),
        "a publication without its recorded reader must make the next apply finish CDC"
    );
}
