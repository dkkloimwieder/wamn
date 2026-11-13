use super::{
    Client, SCHEMA, connect, install_current_run_plane, locked_database, reconcile_run_plane,
    reset, schema, seed_run_admission_facts,
};
use sqlx::Row as _;

#[tokio::test]
async fn global_fifo_queue_live() {
    let url = locked_database::database(wamn_test_postgres::database);
    let mut su = connect(&url).await;
    global_fifo_queue_leg(&mut su).await;
}

pub(super) async fn global_fifo_queue_leg(su: &mut Client) {
    reset(su).await;
    install_current_run_plane(su).await;
    seed_run_admission_facts(su, "partition", "cat", "dev").await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "INSERT INTO {SCHEMA}.runs \
           (project_env_key,run_id,wiring_id,wiring_version,wiring_hash,binding_world_json,package_id, \
            environment) \
         VALUES ('partition','retained-run','fixture-wiring',1,'sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd','[]'::jsonb,'cat','dev'); \
         INSERT INTO {SCHEMA}.run_queue \
           (project_env_key,run_id) \
         VALUES ('partition','retained-run');"
    ))).execute(&mut *su).await.map(drop)
    .expect("seed an unleased queue row");

    let retained = sqlx::query(sqlx::AssertSqlSafe(
        (&format!(
            "SELECT lease_owner,lease_generation,attempts \
                   FROM {SCHEMA}.run_queue \
                  WHERE project_env_key='partition' AND run_id='retained-run'"
        ))
            .to_string(),
    ))
    .fetch_one(&mut *su)
    .await
    .expect("read retained queue row");
    assert_eq!(retained.get::<Option<String>, _>(0), None);
    assert_eq!(retained.get::<i64, _>(1), 0);
    assert_eq!(retained.get::<i32, _>(2), 0);
    let again = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect("current queue reconciliation");
    assert!(
        again.is_noop(),
        "second partition reconcile: {:#?}",
        again.actions
    );
}
