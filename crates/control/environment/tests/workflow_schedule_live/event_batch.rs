//! Event admission over the shared schedule fixture.

use super::*;
#[tokio::test(flavor = "multi_thread")]
async fn fanout_is_atomic_idempotent_and_records_the_history_actor() {
    let _lock = wamn_test_postgres::lock();
    let fixture = fixture().await;
    fixture.grant_workflow_user("reader").await;
    fixture.grant_workflow_user("admin").await;
    let admin = &fixture.admin;
    let txid = changed_order(&fixture).await;
    // A higher xid epoch still matches the envelope's low 32 bits.
    admin.batch_execute("UPDATE application_data.orders_history SET transaction_id = transaction_id + 4294967296").await.unwrap();
    event(&fixture, "a-role", 10, Some("reader")).await;
    event(&fixture, "b-invoker", 10, None).await;
    event(&fixture, "z-invalid", 10, Some("reader")).await;
    // The last graph fails its release gate after the earlier inserts. No
    // partial fan-out may survive the transaction.
    admin
        .batch_execute("ALTER TABLE catalog.workflows DISABLE TRIGGER workflows_controlled_update")
        .await
        .unwrap();
    admin
        .execute(
            "UPDATE catalog.workflows SET wiring_hash = $1 WHERE workflow_id = 'z-invalid'",
            &[&format!("sha256:{}", "0".repeat(64))],
        )
        .await
        .unwrap();
    admin
        .batch_execute("ALTER TABLE catalog.workflows ENABLE TRIGGER workflows_controlled_update")
        .await
        .unwrap();
    let inserted = envelope("insert", txid, 40);
    assert!(admit(&fixture, &inserted, 40, 40).await.is_err());
    assert_eq!(
        admin
            .query_one("SELECT count(*) FROM wamn_run.runs", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    admin
        .execute(
            "UPDATE catalog.workflows SET enabled = false WHERE workflow_id = 'z-invalid'",
            &[],
        )
        .await
        .unwrap();
    admit(&fixture, &inserted, 40, 40).await.unwrap();
    event(&fixture, "third", 40, Some("reader")).await;
    admit(&fixture, &inserted, 40, 40).await.unwrap();
    let rows = admin.query("SELECT wiring_id, execution_principal_id::text, execution_role, invoked_by, invoked_by_user_id::text, node_digests_json::text, event_source_run_id, event_root_run_id, event_depth FROM wamn_run.runs ORDER BY wiring_id", &[]).await.unwrap();
    assert_eq!(
        rows.len(),
        2,
        "redelivery and a later enable create no extra runs"
    );
    for (row, role) in rows.iter().zip(["reader", "admin"]) {
        assert_eq!(row.get::<_, String>(1), workflow_user_id().to_string());
        assert_eq!(row.get::<_, String>(2), role);
        assert_eq!(row.get::<_, String>(6), "source-run");
        assert_eq!(row.get::<_, String>(7), "root-run");
        assert_eq!(row.get::<_, i32>(8), 3);
        assert_eq!(
            row.get::<_, String>(3),
            format!(
                "event:40:{}",
                wamn_event_wire::msg_id(PROJECT, ENVIRONMENT, 40)
            )
        );
        assert_eq!(row.get::<_, Option<String>>(4).as_deref(), Some(READER));
        assert!(
            serde_json::from_str::<Value>(&row.get::<_, String>(5))
                .unwrap()
                .get("read")
                .is_some()
        );
    }
    assert_eq!(
        admin
            .query_one("SELECT count(*) FROM wamn_run.run_queue", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        2
    );

    admin
        .batch_execute("BEGIN; SET LOCAL app.operation = 'admin:event-batch-fixture'")
        .await
        .unwrap();
    admin
        .execute("SELECT set_config('app.user_id', $1, true)", &[&ADMIN])
        .await
        .unwrap();
    admin
        .batch_execute("DELETE FROM application_data.orders")
        .await
        .unwrap();
    let delete_txid: i64 = admin
        .query_one(
            "SELECT pg_current_xact_id()::text::bigint % 4294967296",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    admin.batch_execute("COMMIT").await.unwrap();
    admit(&fixture, &envelope("delete", delete_txid, 41), 41, 41)
        .await
        .unwrap();
    let actors = admin
        .query(
            "SELECT invoked_by_user_id::text FROM wamn_run.runs WHERE invoked_by = $1",
            &[&format!(
                "event:41:{}",
                wamn_event_wire::msg_id(PROJECT, ENVIRONMENT, 41)
            )],
        )
        .await
        .unwrap();
    assert_eq!(actors.len(), 3);
    assert!(
        actors
            .iter()
            .all(|row| row.get::<_, Option<String>>(0).as_deref() == Some(ADMIN)),
        "a delete records the deleting actor"
    );
    admit(&fixture, &envelope("delete", delete_txid + 1, 42), 42, 42)
        .await
        .unwrap();
    assert_eq!(
        admin
            .query_one(
                "SELECT count(*) FROM wamn_run.runs WHERE invoked_by_user_id IS NULL",
                &[]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        3
    );
    fixture.drain().await;
    let completed: i64 = admin
        .query_one(
            "SELECT count(*) FROM wamn_run.runs WHERE status = 'completed'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        completed, 8,
        "every event run executes through the workflow queue"
    );
}
