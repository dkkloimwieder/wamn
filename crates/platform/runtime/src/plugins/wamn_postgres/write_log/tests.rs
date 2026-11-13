//! The write log as the engine's intent store, on disposable databases.

use std::sync::Arc;

use serde_json::json;
use tokio::time::Duration;
use wamn_run_state::IntentStore;
use wamn_run_state::intent_store::{Begun, Intent, IntentId, StoreErrorType, StoredOutcome};

use super::WriteLogStore;
use crate::plugins::wamn_postgres::session_identity::tests::{
    LIVE_PRINCIPAL, ensure_live_user_rows, live_guest_url,
};
use crate::plugins::wamn_postgres::{
    ClassCredentials, SessionIdentity, WamnPostgres, WamnPostgresConfig,
};

/// Bounds a wait for an observed backend state. It only stops a test that a
/// defect would hang.
const HANG_GUARD: Duration = Duration::from_secs(120);
const PROJECT: &str = "default";
const SCOPE: &str = "write-log-request-element";

/// The write log section of the schema that every project database gets.
fn write_log_sql() -> &'static str {
    let schema = include_str!("../../../../../../../deploy/sql/app-schema.sql");
    let start = schema
        .find("CREATE TABLE IF NOT EXISTS platform.write_log")
        .expect("the schema creates the write log");
    let grant = "GRANT DELETE ON platform.write_log TO wamn_app;";
    let end = schema[start..]
        .find(grant)
        .expect("the schema grants the write log")
        + start
        + grant.len();
    &schema[start..end]
}

/// A plugin over a fresh database of `project_env_key`, with the write log installed.
async fn project_env_key_database(
    project_env_key: &str,
) -> (wamn_test_postgres::Database, Arc<WamnPostgres>) {
    let database = wamn_test_postgres::database();
    let guest_url = live_guest_url(database.url(), project_env_key).await;
    ensure_live_user_rows(database.url(), &[LIVE_PRINCIPAL]).await;
    database
        .execute(&[write_log_sql()])
        .expect("install the write log");
    let postgres = Arc::new(
        WamnPostgres::new(WamnPostgresConfig {
            credentials: Some(ClassCredentials::every_class(guest_url)),
            guest_pool_max_size: 3,
            platform_pool_max_size: 1,
            wait_timeout_ms: 2_000,
            statement_timeout_ms: 5_000,
            row_limit: 10,
        })
        .expect("live postgres plugin"),
    );
    (database, postgres)
}

/// Replay, release, conflict and partition behavior remain write-log contracts.
#[tokio::test]
async fn the_two_step_write_log_keeps_the_intent_rules() {
    let _lock = wamn_test_postgres::lock();
    let (_database, postgres) = project_env_key_database("test--dev--writelogtx").await;
    let (_other_database, other_postgres) = project_env_key_database("test--dev--otherlog").await;
    let store = WriteLogStore::new(
        postgres,
        PROJECT.to_owned(),
        "test--dev--writelogtx".to_owned(),
    );
    let other = WriteLogStore::new(
        other_postgres,
        PROJECT.to_owned(),
        "test--dev--otherlog".to_owned(),
    );
    let request = intent("key", "hash");
    let Begun::New(first) = store.begin(&request).await.unwrap() else {
        panic!("new claim")
    };
    let elsewhere = Intent {
        partition: "test--dev--otherlog",
        ..request
    };
    assert_eq!(
        store.begin(&elsewhere).await.unwrap_err().error_type(),
        StoreErrorType::Contract
    );
    assert!(matches!(
        other.begin(&elsewhere).await.unwrap(),
        Begun::New(_)
    ));
    let missing = IntentId(format!(
        "{}#missing",
        request.operation.split('@').next().unwrap()
    ));
    store.release(&first).await.unwrap();
    for id in [&missing, &first] {
        assert_eq!(
            store.release(id).await.unwrap_err().error_type(),
            StoreErrorType::Contract
        );
        assert_eq!(
            store
                .finish(id, &StoredOutcome(json!({"value": 0})))
                .await
                .unwrap_err()
                .error_type(),
            StoreErrorType::Contract
        );
    }
    let Begun::New(second) = store.begin(&request).await.unwrap() else {
        panic!("released key")
    };
    let outcome = StoredOutcome(json!({"value": 7}));
    store.finish(&second, &outcome).await.unwrap();
    assert_eq!(
        store.begin(&request).await.unwrap(),
        Begun::Finished(outcome.clone())
    );
    assert_eq!(
        store
            .finish(&second, &outcome)
            .await
            .unwrap_err()
            .error_type(),
        StoreErrorType::Contract
    );
    assert_eq!(
        store.release(&second).await.unwrap_err().error_type(),
        StoreErrorType::Contract
    );
    let malformed = IntentId("not-a-claim".to_owned());
    assert_eq!(
        store
            .finish(&malformed, &outcome)
            .await
            .unwrap_err()
            .error_type(),
        StoreErrorType::Contract
    );
    assert_eq!(
        store.release(&malformed).await.unwrap_err().error_type(),
        StoreErrorType::Contract
    );
}

fn intent<'a>(key: &'a str, input_hash: &'a str) -> Intent<'a> {
    Intent {
        partition: "test--dev--writelogtx",
        release: "release-1",
        package: "base",
        operation: "base:widget/create@1.0.0",
        idempotency_key: key,
        input_hash,
        deadline_ms: 5_000,
    }
}

/// A known result is inserted once, replays in full, and rolls back on refusal.
#[tokio::test]
async fn a_known_result_commits_in_the_claim_without_a_finish_update() {
    let _lock = wamn_test_postgres::lock();
    let (database, postgres) = project_env_key_database("test--dev--writelogtx").await;
    database
        .execute(&[
            "ALTER TABLE platform.write_log ADD CHECK (result IS NOT NULL); \
             CREATE FUNCTION public.refuse_log_update() RETURNS trigger LANGUAGE plpgsql AS $$ \
             BEGIN RAISE EXCEPTION 'the known result must not need an update'; END $$; \
             CREATE TRIGGER refuse_log_update BEFORE UPDATE ON platform.write_log \
             FOR EACH ROW EXECUTE FUNCTION public.refuse_log_update()",
        ])
        .unwrap();
    postgres
        .bind_session_identity(
            SCOPE,
            &SessionIdentity {
                project_env_key: "test--dev--writelogtx".into(),
                user_id: Some(LIVE_PRINCIPAL.into()),
                operation: Some("base:widget/delete@1.0.0".into()),
                ..SessionIdentity::default()
            },
        )
        .await
        .unwrap();
    let begin = async || {
        postgres
            .begin_operation_transaction(SCOPE)
            .await
            .unwrap()
            .with_known_result(super::DELETED_RESULT.to_owned())
    };
    let outcome = StoredOutcome(json!({"value": {"outcome": "deleted"}}));
    let first = begin().await;
    let Begun::New(id) = first.begin(&intent("known", "h1")).await.unwrap() else {
        panic!("new claim")
    };
    first.finish(&id, &outcome).await.unwrap();
    first.commit().await.unwrap();
    let retry = begin().await;
    assert_eq!(
        retry.begin(&intent("known", "h1")).await.unwrap(),
        Begun::Finished(outcome),
    );
    retry.rollback().await.unwrap();

    // Exercise the opaque entry path with the same known result and no UPDATE.
    let opaque = begin().await;
    assert!(matches!(
        opaque
            .begin_opaque("base:widget/delete@1", "opaque", vec![1])
            .await
            .unwrap(),
        super::Claim::New
    ));
    opaque.finish_opaque(
        "base:widget/delete@1",
        "opaque",
        super::DELETED_RESULT.to_owned(),
    );
    opaque.commit().await.unwrap();
    let replay = begin().await;
    assert!(matches!(
        replay
            .begin_opaque("base:widget/delete@2", "opaque", vec![1])
            .await
            .unwrap(),
        super::Claim::Finished(_)
    ));
    replay.rollback().await.unwrap();
    let refused = begin().await;
    assert!(matches!(
        refused.begin(&intent("refused", "h1")).await.unwrap(),
        Begun::New(_)
    ));
    refused.rollback().await.unwrap();
    let released = begin().await;
    assert!(matches!(
        released
            .begin(&intent("refused", "different"))
            .await
            .unwrap(),
        Begun::New(_)
    ));
    released.rollback().await.unwrap();
}

/// An operation with SQL claims in its request element transaction: the claim commits
/// with the result, a retry answers the stored result or a conflict, a
/// rollback frees the key, and a second claim of a key waits for the first.
#[tokio::test]
async fn the_claim_of_a_request_element_commits_with_its_transaction() {
    let _lock = wamn_test_postgres::lock();
    let (database, postgres) = project_env_key_database("test--dev--writelogtx").await;
    postgres
        .bind_session_identity(
            SCOPE,
            &SessionIdentity {
                project_env_key: "test--dev--writelogtx".into(),
                user_id: Some(LIVE_PRINCIPAL.into()),
                operation: Some("base:widget/create@1.0.0".into()),
                ..SessionIdentity::default()
            },
        )
        .await
        .expect("claims bind");
    let begin = async || {
        postgres
            .begin_operation_transaction(SCOPE)
            .await
            .expect("the request element transaction begins")
    };
    let mut observer = wamn_postgres::connect(database.url()).await.unwrap();
    let outcome = StoredOutcome(json!({"value": {"id": 7}}));

    let first = begin().await;
    let Begun::New(id) = first.begin(&intent("k1", "h1")).await.expect("claim") else {
        panic!("a new key is new");
    };
    first.finish(&id, &outcome).await.expect("finish");
    let visible: bool = sqlx::query_scalar("SELECT EXISTS (SELECT FROM platform.write_log)")
        .fetch_one(&mut observer)
        .await
        .unwrap();
    assert!(!visible, "the claim is not visible before commit");
    first.commit().await.expect("commit");

    let retry = begin().await;
    assert_eq!(
        retry
            .begin(&Intent {
                operation: "base:widget/create@2.0.0",
                ..intent("k1", "h1")
            })
            .await
            .expect("retry"),
        Begun::Finished(outcome.clone()),
        "the same request answers the stored result"
    );
    assert_eq!(
        retry.begin(&intent("k1", "h2")).await.expect("retry"),
        Begun::Conflict(id),
        "another request under the key conflicts"
    );
    retry.rollback().await.expect("rollback");

    let refused = begin().await;
    assert!(matches!(
        refused.begin(&intent("k2", "h1")).await.expect("claim"),
        Begun::New(_)
    ));
    refused.rollback().await.expect("rollback");
    let again = begin().await;
    assert!(
        matches!(
            again.begin(&intent("k2", "h9")).await.expect("claim"),
            Begun::New(_)
        ),
        "a refused request element frees its key for any request"
    );
    again.rollback().await.expect("rollback");

    let holder = begin().await;
    let Begun::New(held) = holder.begin(&intent("k3", "h1")).await.expect("claim") else {
        panic!("a new key is new");
    };
    let waiter = begin().await;
    let waiting = tokio::spawn({
        let waiter = waiter.clone();
        async move { waiter.begin(&intent("k3", "h1")).await }
    });
    tokio::time::timeout(HANG_GUARD, async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT EXISTS (SELECT FROM pg_stat_activity WHERE datname = current_database() AND cardinality(pg_blocking_pids(pid)) > 0)")
                .fetch_one(&mut observer).await.unwrap();
            if waiting { break; }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the second claim waits for the first");
    holder.finish(&held, &outcome).await.expect("finish");
    holder.commit().await.expect("commit");
    assert_eq!(
        waiting.await.unwrap().expect("the waiting claim"),
        Begun::Finished(outcome),
        "the second claim answers what the first committed"
    );
    waiter.rollback().await.expect("rollback");

    // A statement that finds its connection lost fails, and the rollback
    // after it answers that the transaction ended.
    let lost = begin().await;
    lost.begin(&intent("lost", "h1"))
        .await
        .expect("open the transaction before disconnect");
    database
        .execute(&["SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
             WHERE state = 'idle in transaction' AND pid <> pg_backend_pid()"])
        .expect("end the request element connection");
    assert_eq!(
        lost.begin(&intent("k4", "h1"))
            .await
            .unwrap_err()
            .error_type(),
        StoreErrorType::Storage,
        "a lost connection is a storage failure"
    );
    lost.rollback()
        .await
        .expect("the rollback of a lost connection succeeds");
    let recovered = begin().await;
    assert!(matches!(
        recovered.begin(&intent("lost", "different")).await.unwrap(),
        Begun::New(_)
    ));
    recovered.rollback().await.unwrap();
}

/// A failed finish must abort business writes even when COMMIT is already queued.
#[tokio::test]
async fn a_missing_finish_rolls_back_the_pipelined_commit() {
    let _lock = wamn_test_postgres::lock();
    let (_database, postgres) = project_env_key_database("test--dev--writelogtx").await;
    postgres
        .bind_session_identity(
            SCOPE,
            &SessionIdentity {
                project_env_key: "test--dev--writelogtx".into(),
                user_id: Some(LIVE_PRINCIPAL.into()),
                operation: Some("base:widget/create@1.0.0".into()),
                ..SessionIdentity::default()
            },
        )
        .await
        .unwrap();
    let transaction = postgres.begin_operation_transaction(SCOPE).await.unwrap();
    let Begun::New(mut id) = transaction.begin(&intent("open", "h1")).await.unwrap() else {
        panic!("new claim")
    };
    id.0.push_str("-missing");
    transaction
        .finish(&id, &StoredOutcome(json!({"value": 1})))
        .await
        .unwrap();
    assert!(matches!(
        transaction.commit().await,
        Err(super::super::operation_transaction::PgCommitFailure::RolledBack(_))
    ));
    let retry = postgres.begin_operation_transaction(SCOPE).await.unwrap();
    assert!(matches!(
        retry.begin(&intent("open", "different")).await.unwrap(),
        Begun::New(_)
    ));
    retry.rollback().await.unwrap();
}

/// Every writer's committed WAL invalidates weak model tags, including deletes and truncates.
#[tokio::test]
async fn model_versions_follow_wal_without_a_counter() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::database();
    let family = wamn_control_provision::WorkloadRoleFamily::HttpAdmitter;
    let role = wamn_control_provision::workload_login(
        family,
        wamn_control_provision::WorkloadRoleIdentity::ProjectEnvironment {
            org: "wal-test",
            project: PROJECT,
            environment: "test",
            database: database.name(),
        },
    )
    .unwrap();
    database
        .execute(&[&format!(
            "CREATE ROLE {} LOGIN PASSWORD 'wal-reader'; {}",
            postgres_protocol::escape::escape_identifier(&role),
            wamn_control_provision::test_database::membership_sql(family, &role, true),
        )])
        .unwrap();
    let mut reader_url = url::Url::parse(database.url()).unwrap();
    reader_url.set_username(&role).unwrap();
    reader_url.set_password(Some("wal-reader")).unwrap();
    let postgres = WamnPostgres::new(WamnPostgresConfig {
        credentials: Some(ClassCredentials::default().with_class(
            wamn_run_state::AuthorityClass::CallableHttp,
            reader_url.to_string(),
        )),
        guest_pool_max_size: 1,
        platform_pool_max_size: 1,
        wait_timeout_ms: 5_000,
        statement_timeout_ms: 5_000,
        row_limit: 10,
    })
    .unwrap();
    database
        .execute(&[
            "CREATE TABLE public.etag_rows (id int); INSERT INTO public.etag_rows VALUES (1), (2)",
        ])
        .unwrap();
    let mut previous = postgres.model_version(PROJECT).await.unwrap();
    for statement in [
        "UPDATE public.etag_rows SET id = 3 WHERE id = 1",
        "DELETE FROM public.etag_rows WHERE id = 3",
        "TRUNCATE public.etag_rows",
    ] {
        database.execute(&[statement]).unwrap();
        let current = postgres.model_version(PROJECT).await.unwrap();
        assert_ne!(previous, current, "{statement}");
        previous = current;
    }
}

/// The call bound changes the refusal without preventing a late result or expiry.
#[tokio::test]
async fn unfinished_claims_have_a_call_bound_and_accept_late_completion() {
    let _lock = wamn_test_postgres::lock();
    let (database, postgres) = project_env_key_database("test--dev--writelogtx").await;
    let store = WriteLogStore::new(
        postgres,
        PROJECT.to_owned(),
        "test--dev--writelogtx".to_owned(),
    );
    let request = intent("late", "hash");
    let Begun::New(id) = store.begin(&request).await.unwrap() else {
        panic!("new claim")
    };
    assert!(matches!(
        store.begin(&request).await.unwrap(),
        Begun::InProgress(_)
    ));
    assert!(matches!(
        store.begin(&intent("late", "changed")).await.unwrap(),
        Begun::Conflict(_)
    ));
    assert!(matches!(
        store.begin(&request).await.unwrap(),
        Begun::InProgress(_)
    ));
    let bound = wamn_engine::engine::MAX_HOST_CALL_DURATION.as_secs() + 1;
    database.execute(&[&format!("UPDATE platform.write_log SET created_at = clock_timestamp() - interval '{bound} seconds'")]).unwrap();
    assert!(matches!(
        store.begin(&request).await.unwrap(),
        Begun::OutcomeUnknown(_)
    ));
    let outcome = StoredOutcome(json!({"value": 7}));
    store.finish(&id, &outcome).await.unwrap();
    assert_eq!(
        store.begin(&request).await.unwrap(),
        Begun::Finished(outcome)
    );
    let expired = intent("expired", "hash");
    assert!(matches!(
        store.begin(&expired).await.unwrap(),
        Begun::New(_)
    ));
    database
        .execute(&[
            "UPDATE platform.write_log SET created_at = clock_timestamp() - interval '31 days'",
            wamn_control_provision::audit_retention::WRITE_LOG_RETENTION_SQL,
        ])
        .unwrap();
    assert!(matches!(
        store.begin(&expired).await.unwrap(),
        Begun::New(_)
    ));
}
