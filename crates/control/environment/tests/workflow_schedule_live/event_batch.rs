//! Event admission over the shared schedule fixture.

use super::*;
use sqlx::Row as _;
#[tokio::test(flavor = "multi_thread")]
async fn fanout_is_atomic_idempotent_and_records_the_history_actor() {
    let _lock = wamn_test_postgres::lock();
    let mut fixture = fixture().await;
    fixture.grant_workflow_user("reader").await;
    fixture.grant_workflow_user("admin").await;
    let txid = changed_order(&mut fixture).await;
    // A higher xid epoch still matches the envelope's low 32 bits.
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("UPDATE application_data.orders_history SET transaction_id = transaction_id + 4294967296")
            .to_string(),
    ))
    .execute(&mut fixture.admin)
    .await
    .map(drop)
    .unwrap();
    event(&mut fixture, "a-role", 10, Some("reader")).await;
    event(&mut fixture, "b-invoker", 10, None).await;
    event(&mut fixture, "z-invalid", 10, Some("reader")).await;
    // The last graph fails its release condition after the earlier inserts. No
    // partial fan-out may survive the transaction.
    sqlx::raw_sql("ALTER TABLE catalog.workflows DISABLE TRIGGER workflows_controlled_update")
        .execute(&mut fixture.admin)
        .await
        .map(drop)
        .unwrap();
    sqlx::query("UPDATE catalog.workflows SET wiring_hash = $1 WHERE workflow_id = 'z-invalid'")
        .bind(&format!("sha256:{}", "0".repeat(64)))
        .execute(&mut fixture.admin)
        .await
        .map(|result| result.rows_affected())
        .unwrap();
    sqlx::raw_sql("ALTER TABLE catalog.workflows ENABLE TRIGGER workflows_controlled_update")
        .execute(&mut fixture.admin)
        .await
        .map(drop)
        .unwrap();
    let inserted = envelope("insert", txid, 40);
    assert!(admit(&fixture, &inserted, 40, 40).await.is_err());
    assert_eq!(
        sqlx::query("SELECT count(*) FROM wamn_run.runs")
            .fetch_one(&mut fixture.admin)
            .await
            .unwrap()
            .get::<i64, _>(0),
        0
    );
    sqlx::query("UPDATE catalog.workflows SET enabled = false WHERE workflow_id = 'z-invalid'")
        .execute(&mut fixture.admin)
        .await
        .map(|result| result.rows_affected())
        .unwrap();
    admit(&fixture, &inserted, 40, 40).await.unwrap();
    event(&mut fixture, "third", 40, Some("reader")).await;
    admit(&fixture, &inserted, 40, 40).await.unwrap();
    let rows = sqlx::query("SELECT wiring_id, execution_principal_id::text, execution_role, invoked_by, invoked_by_user_id::text, node_digests_json::text, event_source_run_id, event_root_run_id, event_depth FROM wamn_run.runs ORDER BY wiring_id").fetch_all(&mut fixture.admin).await.unwrap();
    assert_eq!(
        rows.len(),
        2,
        "redelivery and a later enable create no extra runs"
    );
    for (row, role) in rows.iter().zip(["reader", "admin"]) {
        assert_eq!(row.get::<String, _>(1), workflow_user_id().to_string());
        assert_eq!(row.get::<String, _>(2), role);
        assert_eq!(row.get::<String, _>(6), "source-run");
        assert_eq!(row.get::<String, _>(7), "root-run");
        assert_eq!(row.get::<i32, _>(8), 3);
        assert_eq!(
            row.get::<String, _>(3),
            format!(
                "event:{}",
                wamn_event_wire::msg_id(
                    PROJECT,
                    ENVIRONMENT,
                    &wamn_event_wire::CdcIdentity {
                        system_id: 1,
                        timeline: 1,
                        commit_lsn: 40,
                        row_ordinal: 0
                    }
                )
            )
        );
        assert_eq!(row.get::<Option<String>, _>(4).as_deref(), Some(READER));
        assert!(
            serde_json::from_str::<Value>(&row.get::<String, _>(5))
                .unwrap()
                .get("read")
                .is_some()
        );
    }
    assert_eq!(
        sqlx::query("SELECT count(*) FROM wamn_run.run_queue")
            .fetch_one(&mut fixture.admin)
            .await
            .unwrap()
            .get::<i64, _>(0),
        2
    );

    sqlx::raw_sql("BEGIN; SET LOCAL app.operation = 'admin:event-batch-fixture'")
        .execute(&mut fixture.admin)
        .await
        .map(drop)
        .unwrap();
    sqlx::query("SELECT set_config('app.user_id', $1, true)")
        .bind(&ADMIN)
        .execute(&mut fixture.admin)
        .await
        .map(|result| result.rows_affected())
        .unwrap();
    sqlx::raw_sql("DELETE FROM application_data.orders")
        .execute(&mut fixture.admin)
        .await
        .map(drop)
        .unwrap();
    let delete_txid: i64 = sqlx::query("SELECT pg_current_xact_id()::text::bigint % 4294967296")
        .fetch_one(&mut fixture.admin)
        .await
        .unwrap()
        .get(0);
    sqlx::raw_sql("COMMIT")
        .execute(&mut fixture.admin)
        .await
        .map(drop)
        .unwrap();
    admit(&fixture, &envelope("delete", delete_txid, 41), 41, 41)
        .await
        .unwrap();
    let actors =
        sqlx::query("SELECT invoked_by_user_id::text FROM wamn_run.runs WHERE invoked_by = $1")
            .bind(&format!(
                "event:{}",
                wamn_event_wire::msg_id(
                    PROJECT,
                    ENVIRONMENT,
                    &wamn_event_wire::CdcIdentity {
                        system_id: 1,
                        timeline: 1,
                        commit_lsn: 41,
                        row_ordinal: 0
                    }
                )
            ))
            .fetch_all(&mut fixture.admin)
            .await
            .unwrap();
    assert_eq!(actors.len(), 3);
    assert!(
        actors
            .iter()
            .all(|row| row.get::<Option<String>, _>(0).as_deref() == Some(ADMIN)),
        "a delete records the deleting actor"
    );
    admit(&fixture, &envelope("delete", delete_txid + 1, 42), 42, 42)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query("SELECT count(*) FROM wamn_run.runs WHERE invoked_by_user_id IS NULL")
            .fetch_one(&mut fixture.admin)
            .await
            .unwrap()
            .get::<i64, _>(0),
        3
    );
    fixture.drain().await;
    let completed: i64 =
        sqlx::query("SELECT count(*) FROM wamn_run.runs WHERE status = 'completed'")
            .fetch_one(&mut fixture.admin)
            .await
            .unwrap()
            .get(0);
    assert_eq!(
        completed, 8,
        "every event run executes through the workflow queue"
    );
}
