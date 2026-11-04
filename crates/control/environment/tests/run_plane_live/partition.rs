use super::{
    Client, SCHEMA, connect, indexdef, install_current_run_plane, locked_database,
    reconcile_run_plane, reset, schema, seed_run_admission_facts, table_exists,
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
    seed_run_admission_facts(su, "partition", "cat", "dev", "standard").await;
    su.batch_execute(&format!(
        "INSERT INTO {SCHEMA}.runs \
           (env_key,run_id,flow_id,flow_version,package_id, \
            environment) \
         VALUES ('partition','retained-run','f',1,'cat','dev'); \
         INSERT INTO {SCHEMA}.run_queue \
           (env_key,run_id,stream_seq) \
         VALUES ('partition','retained-run',7);"
    ))
    .await
    .expect("seed an unleased queue row");

    let columns: Vec<String> = su
        .query(
            "SELECT column_name FROM information_schema.columns \
              WHERE table_schema=$1 AND table_name='run_queue' \
              ORDER BY ordinal_position",
            &[&SCHEMA],
        )
        .await
        .expect("read global queue columns")
        .into_iter()
        .map(|row| row.get(0))
        .collect();
    assert_eq!(
        columns,
        [
            "created_at",
            "created_by",
            "updated_at",
            "updated_by",
            "env_key",
            "run_id",
            "priority",
            "available_at",
            "stream_seq",
            "lease_owner",
            "lease_expires_at",
            "lease_generation",
            "attempts",
            "max_attempts",
            "enqueued_at",
        ]
        .map(str::to_string)
    );
    let retained = su
        .query_one(
            &format!(
                "SELECT stream_seq,lease_owner,lease_generation,attempts \
                   FROM {SCHEMA}.run_queue \
                  WHERE env_key='partition' AND run_id='retained-run'"
            ),
            &[],
        )
        .await
        .expect("read retained queue row");
    assert_eq!(retained.get::<_, i64>(0), 7);
    assert_eq!(retained.get::<_, Option<String>>(1), None);
    assert_eq!(retained.get::<_, i64>(2), 0);
    assert_eq!(retained.get::<_, i32>(3), 0);
    assert!(!table_exists(su, SCHEMA, "partition_owner").await);
    assert!(!table_exists(su, SCHEMA, "run_dead_letters").await);
    assert!(indexdef(su, "run_queue_partition").await.is_none());
    let claimable = indexdef(su, "run_queue_claimable")
        .await
        .expect("global claim index exists");
    assert!(
        claimable.contains("(env_key, available_at, stream_seq, run_id, lease_expires_at)"),
        "global FIFO index shape: {claimable}"
    );
    assert!(
        !claimable.contains("WHERE"),
        "claim index is global: {claimable}"
    );

    let again = reconcile_run_plane::reconcile(su, &schema(), true)
        .await
        .expect("current queue reconciliation");
    assert!(
        again.is_noop(),
        "second partition reconcile: {:#?}",
        again.actions
    );
}
