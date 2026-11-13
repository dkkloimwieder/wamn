use super::{
    Client, SCHEMA, connect, install_current_run_plane, locked_database, reconcile_run_plane,
    reset, schema, seed_run_admission_facts,
};

#[tokio::test]
async fn global_fifo_queue_live() {
    let url = locked_database::database(wamn_test_postgres::database);
    let su = connect(&url).await;
    global_fifo_queue_leg(&su).await;
}

pub(super) async fn global_fifo_queue_leg(su: &Client) {
    reset(su).await;
    install_current_run_plane(su).await;
    seed_run_admission_facts(su, "partition", "cat", "dev").await;
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs \
           (env_key,run_id,wiring_id,wiring_version,wiring_hash,binding_world_json,package_id, \
            environment) \
         VALUES ('partition','retained-run','fixture-wiring',1,'sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd','[]'::jsonb,'cat','dev'); \
         INSERT INTO {SCHEMA}.run_queue \
           (env_key,run_id) \
         VALUES ('partition','retained-run');"
    ))
    .await
    .expect("seed an unleased queue row");

    let retained = su
        .query_one(
            &format!(
                "SELECT lease_owner,lease_generation,attempts \
                   FROM {SCHEMA}.run_queue \
                  WHERE env_key='partition' AND run_id='retained-run'"
            ),
            &[],
        )
        .await
        .expect("read retained queue row");
    assert_eq!(retained.get::<_, Option<String>>(0), None);
    assert_eq!(retained.get::<_, i64>(1), 0);
    assert_eq!(retained.get::<_, i32>(2), 0);
    let again = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect("current queue reconciliation");
    assert!(
        again.is_noop(),
        "second partition reconcile: {:#?}",
        again.actions
    );
}
