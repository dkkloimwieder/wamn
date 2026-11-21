//! Concrete operation claims on disposable databases.

use std::sync::Arc;

use serde_json::json;

use super::WriteLogStore;
use crate::session_identity_tests::{
    LIVE_PRINCIPAL, ensure_live_user_rows, enter, live_guest_url, live_server, project_credentials,
};
use crate::{OperationHost, SessionIdentity};
use wamn_postgres::{WamnPostgres, WamnPostgresConfig};
use wamn_test_infrastructure::containers::Postgres;

const PROJECT: &str = "default";
const SCOPE: &str = "write-log-request-element";

/// A plugin over a private server's database of `project_env_key`, with the write log installed.
async fn project_env_key_database(
    project_env_key: &wamn_registry::ProjectEnvKey,
) -> (Postgres, String, Arc<OperationHost>) {
    let (server, database) = live_server().await;
    let guest_url = live_guest_url(database.as_str(), project_env_key).await;
    ensure_live_user_rows(database.as_str(), &[LIVE_PRINCIPAL]).await;
    let pools = WamnPostgres::new(WamnPostgresConfig {
        credentials: Some(project_credentials(&guest_url)),
        guest_pool_max_size: 3,
        platform_pool_max_size: 1,
        wait_timeout_ms: 2_000,
        statement_timeout_ms: 5_000,
    })
    .expect("live postgres pools");
    let postgres = Arc::new(OperationHost::new(Arc::new(pools), None));
    (server, database, postgres)
}

const OPERATION: &str = "base:widget/create@1.0.0";

#[tokio::test]
#[ignore = "requires: docker"]
async fn claims_commit_with_their_transaction_and_rollback_releases_them() {
    let (_server, database, postgres) =
        project_env_key_database(&"test--dev--writelogtx".parse().unwrap()).await;
    let _invocation = enter(
        &postgres,
        SCOPE,
        SessionIdentity {
            project_env_key: Some("test--dev--writelogtx".parse().unwrap()),
            user_id: Some(LIVE_PRINCIPAL.parse().unwrap()),
            operation: Some(OPERATION.parse().unwrap()),
            ..SessionIdentity::default()
        },
    );
    let transaction = postgres.begin_operation_transaction(SCOPE).await.unwrap();
    assert!(matches!(
        transaction
            .begin_opaque(&OPERATION.parse().unwrap(), "key", vec![1])
            .await
            .unwrap(),
        super::Claim::New
    ));
    transaction.finish_opaque(
        &OPERATION.parse().unwrap(),
        "key",
        json!({"value":{"id":7}}).to_string(),
    );
    let mut observer = wamn_postgres::connect(database.as_str()).await.unwrap();
    let visible: bool = sqlx::query_scalar("SELECT EXISTS (SELECT FROM platform.write_log)")
        .fetch_one(&mut observer)
        .await
        .unwrap();
    assert!(!visible);
    transaction.commit().await.unwrap();
    let retry = postgres.begin_operation_transaction(SCOPE).await.unwrap();
    let super::Claim::Finished(result) = retry
        .begin_opaque(&OPERATION.parse().unwrap(), "key", vec![1])
        .await
        .unwrap()
    else {
        panic!("completed claim replays");
    };
    let result: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_eq!(result["value"]["id"], 7);
    assert!(matches!(
        retry
            .begin_opaque(&OPERATION.parse().unwrap(), "key", vec![2])
            .await
            .unwrap(),
        super::Claim::Conflict
    ));
    retry.rollback().await.unwrap();
    let refused = postgres.begin_operation_transaction(SCOPE).await.unwrap();
    assert!(matches!(
        refused
            .begin_opaque(&OPERATION.parse().unwrap(), "refused", vec![1])
            .await
            .unwrap(),
        super::Claim::New
    ));
    refused.rollback().await.unwrap();
    let again = postgres.begin_operation_transaction(SCOPE).await.unwrap();
    assert!(matches!(
        again
            .begin_opaque(&OPERATION.parse().unwrap(), "refused", vec![2])
            .await
            .unwrap(),
        super::Claim::New
    ));
    again.rollback().await.unwrap();

    let holder = postgres.begin_operation_transaction(SCOPE).await.unwrap();
    assert!(matches!(
        holder
            .begin_opaque(&OPERATION.parse().unwrap(), "competing", vec![1])
            .await
            .unwrap(),
        super::Claim::New
    ));
    let waiter = postgres.begin_operation_transaction(SCOPE).await.unwrap();
    let waiting = tokio::spawn(async move {
        let result = waiter
            .begin_opaque(&OPERATION.parse().unwrap(), "competing", vec![1])
            .await;
        (waiter, result)
    });
    tokio::time::timeout(std::time::Duration::from_secs(120), async {
        loop {
            let blocked: bool = sqlx::query_scalar("SELECT EXISTS (SELECT FROM pg_stat_activity WHERE datname = current_database() AND cardinality(pg_blocking_pids(pid)) > 0)")
                .fetch_one(&mut observer).await.unwrap();
            if blocked { break; }
            tokio::task::yield_now().await;
        }
    }).await.expect("competing claim waits for the uncommitted claim");
    holder.finish_opaque(
        &OPERATION.parse().unwrap(),
        "competing",
        json!({"value":7}).to_string(),
    );
    holder.commit().await.unwrap();
    let (waiter, result) = waiting.await.unwrap();
    let super::Claim::Finished(result) = result.unwrap() else {
        panic!("competing claim replays the committed result");
    };
    let result: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_eq!(result["value"], 7);
    waiter.rollback().await.unwrap();

    let lost = postgres.begin_operation_transaction(SCOPE).await.unwrap();
    assert!(matches!(
        lost.begin_opaque(&OPERATION.parse().unwrap(), "lost", vec![1])
            .await
            .unwrap(),
        super::Claim::New
    ));
    sqlx::raw_sql("SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = current_database() AND state = 'idle in transaction' AND pid <> pg_backend_pid()").execute(&mut wamn_postgres::connect(database.as_str()).await.unwrap()).await.unwrap();
    assert!(
        lost.begin_opaque(&OPERATION.parse().unwrap(), "after-loss", vec![1])
            .await
            .is_err()
    );
    lost.rollback().await.unwrap();
    let recovered = postgres.begin_operation_transaction(SCOPE).await.unwrap();
    assert!(matches!(
        recovered
            .begin_opaque(&OPERATION.parse().unwrap(), "lost", vec![2])
            .await
            .unwrap(),
        super::Claim::New
    ));
    recovered.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires: docker"]
async fn known_results_finish_at_host_commit_and_a_missing_finish_rolls_back() {
    let (_server, _database, postgres) =
        project_env_key_database(&"test--dev--writelogtx".parse().unwrap()).await;
    let _invocation = enter(
        &postgres,
        SCOPE,
        SessionIdentity {
            project_env_key: Some("test--dev--writelogtx".parse().unwrap()),
            user_id: Some(LIVE_PRINCIPAL.parse().unwrap()),
            operation: Some(OPERATION.parse().unwrap()),
            ..SessionIdentity::default()
        },
    );
    let transaction = postgres.begin_operation_transaction(SCOPE).await.unwrap();
    assert!(matches!(
        transaction
            .begin_opaque(&OPERATION.parse().unwrap(), "missing", vec![1])
            .await
            .unwrap(),
        super::Claim::New
    ));
    transaction.finish_opaque(
        &OPERATION.parse().unwrap(),
        "missing-other",
        json!({"value":1}).to_string(),
    );
    assert!(transaction.commit().await.is_err());
    let known = postgres
        .begin_operation_transaction(SCOPE)
        .await
        .unwrap()
        .with_known_result(super::DELETED_RESULT.to_owned());
    assert!(matches!(
        known
            .begin_opaque(&OPERATION.parse().unwrap(), "known", vec![1])
            .await
            .unwrap(),
        super::Claim::New
    ));
    known.finish_opaque(
        &OPERATION.parse().unwrap(),
        "known",
        super::DELETED_RESULT.to_owned(),
    );
    known.commit().await.unwrap();
    let replay = postgres.begin_operation_transaction(SCOPE).await.unwrap();
    assert!(matches!(
        replay
            .begin_opaque(&OPERATION.parse().unwrap(), "known", vec![1])
            .await
            .unwrap(),
        super::Claim::Finished(_)
    ));
    replay.rollback().await.unwrap();
}

/// The call bound changes the refusal without preventing a late result or expiry.
#[tokio::test]
#[ignore = "requires: docker"]
async fn unfinished_claims_have_a_call_bound_and_accept_late_completion() {
    let (_server, database, postgres) =
        project_env_key_database(&"test--dev--writelogtx".parse().unwrap()).await;
    let store = WriteLogStore::new(
        postgres,
        PROJECT.parse().unwrap(),
        "test--dev--writelogtx".parse().unwrap(),
    );
    assert!(matches!(
        store
            .claim(&OPERATION.parse().unwrap(), "late", vec![1])
            .await
            .unwrap(),
        super::Claim::New
    ));
    assert!(matches!(
        store
            .claim(&OPERATION.parse().unwrap(), "late", vec![1])
            .await
            .unwrap(),
        super::Claim::InProgress
    ));
    assert!(matches!(
        store
            .claim(&OPERATION.parse().unwrap(), "late", vec![2])
            .await
            .unwrap(),
        super::Claim::Conflict
    ));
    let bound = wamn_engine::engine::MAX_HOST_CALL_DURATION.as_secs() + 1;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "UPDATE platform.write_log SET created_at = clock_timestamp() - interval '{bound} seconds'"
    )))
    .execute(&mut wamn_postgres::connect(database.as_str()).await.unwrap())
    .await
    .unwrap();
    assert!(matches!(
        store
            .claim(&OPERATION.parse().unwrap(), "late", vec![1])
            .await
            .unwrap(),
        super::Claim::InProgress
    ));
    let bound =
        (wamn_engine::engine::MAX_HOST_CALL_DURATION + super::CLAIM_CLOCK_SKEW).as_secs() + 1;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "UPDATE platform.write_log SET created_at = clock_timestamp() - interval '{bound} seconds'"
    )))
    .execute(&mut wamn_postgres::connect(database.as_str()).await.unwrap())
    .await
    .unwrap();
    assert!(matches!(
        store
            .claim(&OPERATION.parse().unwrap(), "late", vec![1])
            .await
            .unwrap(),
        super::Claim::OutcomeUnknown
    ));
    store
        .complete(
            &OPERATION.parse().unwrap(),
            "late",
            json!({"value":7}).to_string(),
        )
        .await
        .unwrap();
    let super::Claim::Finished(result) = store
        .claim(&OPERATION.parse().unwrap(), "late", vec![1])
        .await
        .unwrap()
    else {
        panic!("late completion replays");
    };
    let result: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_eq!(result["value"], 7);
    assert!(
        store
            .complete(
                &OPERATION.parse().unwrap(),
                "late",
                json!({"value":9}).to_string()
            )
            .await
            .is_err()
    );
    assert!(matches!(
        store
            .claim(&OPERATION.parse().unwrap(), "expired", vec![1])
            .await
            .unwrap(),
        super::Claim::New
    ));
    {
        let batches: &[&str] = &[
            "UPDATE platform.write_log SET created_at = clock_timestamp() - interval '31 days'",
            wamn_provision::audit_retention::WRITE_LOG_RETENTION_SQL,
        ];
        let mut setup_connection = wamn_postgres::connect(database.as_str()).await.unwrap();
        for batch in batches {
            sqlx::raw_sql(sqlx::AssertSqlSafe(*batch))
                .execute(&mut setup_connection)
                .await
                .unwrap();
        }
    }
    assert!(matches!(
        store
            .claim(&OPERATION.parse().unwrap(), "expired", vec![1])
            .await
            .unwrap(),
        super::Claim::New
    ));
}
