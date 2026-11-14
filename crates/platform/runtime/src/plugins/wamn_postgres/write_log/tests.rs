//! Concrete operation claims on disposable databases.

use std::sync::Arc;

use serde_json::json;

use super::WriteLogStore;
use crate::plugins::wamn_postgres::session_identity::tests::{
    LIVE_PRINCIPAL, ensure_live_user_rows, live_guest_url,
};
use crate::plugins::wamn_postgres::{
    ClassCredentials, SessionIdentity, WamnPostgres, WamnPostgresConfig,
};

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

const OPERATION: &str = "base:widget/create@1.0.0";

#[tokio::test]
async fn claims_commit_with_their_transaction_and_rollback_releases_them() {
    let _lock = wamn_test_postgres::lock();
    let (database, postgres) = project_env_key_database("test--dev--writelogtx").await;
    postgres
        .bind_session_identity(
            SCOPE,
            &SessionIdentity {
                project_env_key: "test--dev--writelogtx".into(),
                user_id: Some(LIVE_PRINCIPAL.into()),
                operation: Some(OPERATION.into()),
                ..SessionIdentity::default()
            },
        )
        .await
        .unwrap();
    let transaction = postgres.begin_operation_transaction(SCOPE).await.unwrap();
    assert!(matches!(
        transaction
            .begin_opaque(OPERATION, "key", vec![1])
            .await
            .unwrap(),
        super::Claim::New
    ));
    transaction.finish_opaque(OPERATION, "key", json!({"value":{"id":7}}).to_string());
    let mut observer = wamn_postgres::connect(database.url()).await.unwrap();
    let visible: bool = sqlx::query_scalar("SELECT EXISTS (SELECT FROM platform.write_log)")
        .fetch_one(&mut observer)
        .await
        .unwrap();
    assert!(!visible);
    transaction.commit().await.unwrap();
    let retry = postgres.begin_operation_transaction(SCOPE).await.unwrap();
    let super::Claim::Finished(result) =
        retry.begin_opaque(OPERATION, "key", vec![1]).await.unwrap()
    else {
        panic!("completed claim replays");
    };
    let result: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_eq!(result["value"]["id"], 7);
    assert!(matches!(
        retry.begin_opaque(OPERATION, "key", vec![2]).await.unwrap(),
        super::Claim::Conflict
    ));
    retry.rollback().await.unwrap();
    let refused = postgres.begin_operation_transaction(SCOPE).await.unwrap();
    assert!(matches!(
        refused
            .begin_opaque(OPERATION, "refused", vec![1])
            .await
            .unwrap(),
        super::Claim::New
    ));
    refused.rollback().await.unwrap();
    let again = postgres.begin_operation_transaction(SCOPE).await.unwrap();
    assert!(matches!(
        again
            .begin_opaque(OPERATION, "refused", vec![2])
            .await
            .unwrap(),
        super::Claim::New
    ));
    again.rollback().await.unwrap();

    let holder = postgres.begin_operation_transaction(SCOPE).await.unwrap();
    assert!(matches!(
        holder
            .begin_opaque(OPERATION, "competing", vec![1])
            .await
            .unwrap(),
        super::Claim::New
    ));
    let waiter = postgres.begin_operation_transaction(SCOPE).await.unwrap();
    let waiting = tokio::spawn(async move {
        let result = waiter.begin_opaque(OPERATION, "competing", vec![1]).await;
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
    holder.finish_opaque(OPERATION, "competing", json!({"value":7}).to_string());
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
        lost.begin_opaque(OPERATION, "lost", vec![1]).await.unwrap(),
        super::Claim::New
    ));
    database.execute(&["SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = current_database() AND state = 'idle in transaction' AND pid <> pg_backend_pid()"]).unwrap();
    assert!(
        lost.begin_opaque(OPERATION, "after-loss", vec![1])
            .await
            .is_err()
    );
    lost.rollback().await.unwrap();
    let recovered = postgres.begin_operation_transaction(SCOPE).await.unwrap();
    assert!(matches!(
        recovered
            .begin_opaque(OPERATION, "lost", vec![2])
            .await
            .unwrap(),
        super::Claim::New
    ));
    recovered.rollback().await.unwrap();
}

#[tokio::test]
async fn a_known_result_needs_no_update_and_a_missing_finish_rolls_back() {
    let _lock = wamn_test_postgres::lock();
    let (database, postgres) = project_env_key_database("test--dev--writelogtx").await;
    postgres
        .bind_session_identity(
            SCOPE,
            &SessionIdentity {
                project_env_key: "test--dev--writelogtx".into(),
                user_id: Some(LIVE_PRINCIPAL.into()),
                operation: Some(OPERATION.into()),
                ..SessionIdentity::default()
            },
        )
        .await
        .unwrap();
    let transaction = postgres.begin_operation_transaction(SCOPE).await.unwrap();
    assert!(matches!(
        transaction
            .begin_opaque(OPERATION, "missing", vec![1])
            .await
            .unwrap(),
        super::Claim::New
    ));
    transaction.finish_opaque(OPERATION, "missing-other", json!({"value":1}).to_string());
    assert!(transaction.commit().await.is_err());
    database.execute(&["ALTER TABLE platform.write_log ADD CHECK (result IS NOT NULL); CREATE FUNCTION public.refuse_log_update() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'unexpected update'; END $$; CREATE TRIGGER refuse_log_update BEFORE UPDATE ON platform.write_log FOR EACH ROW EXECUTE FUNCTION public.refuse_log_update()"]).unwrap();
    let known = postgres
        .begin_operation_transaction(SCOPE)
        .await
        .unwrap()
        .with_known_result(super::DELETED_RESULT.to_owned());
    assert!(matches!(
        known
            .begin_opaque(OPERATION, "known", vec![1])
            .await
            .unwrap(),
        super::Claim::New
    ));
    known.finish_opaque(OPERATION, "known", super::DELETED_RESULT.to_owned());
    known.commit().await.unwrap();
    let replay = postgres.begin_operation_transaction(SCOPE).await.unwrap();
    assert!(matches!(
        replay
            .begin_opaque(OPERATION, "known", vec![1])
            .await
            .unwrap(),
        super::Claim::Finished(_)
    ));
    replay.rollback().await.unwrap();
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
            wamn_identity_protocol::AuthorityClass::CallableHttp,
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
    assert!(matches!(
        store.claim(OPERATION, "late", vec![1]).await.unwrap(),
        super::Claim::New
    ));
    assert!(matches!(
        store.claim(OPERATION, "late", vec![1]).await.unwrap(),
        super::Claim::InProgress
    ));
    assert!(matches!(
        store.claim(OPERATION, "late", vec![2]).await.unwrap(),
        super::Claim::Conflict
    ));
    let bound = wamn_engine::engine::MAX_HOST_CALL_DURATION.as_secs() + 1;
    database.execute(&[&format!("UPDATE platform.write_log SET created_at = clock_timestamp() - interval '{bound} seconds'")]).unwrap();
    assert!(matches!(
        store.claim(OPERATION, "late", vec![1]).await.unwrap(),
        super::Claim::OutcomeUnknown
    ));
    store
        .complete(OPERATION, "late", json!({"value":7}).to_string())
        .await
        .unwrap();
    let super::Claim::Finished(result) = store.claim(OPERATION, "late", vec![1]).await.unwrap()
    else {
        panic!("late completion replays");
    };
    let result: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_eq!(result["value"], 7);
    assert!(
        store
            .complete(OPERATION, "late", json!({"value":9}).to_string())
            .await
            .is_err()
    );
    assert!(matches!(
        store.claim(OPERATION, "expired", vec![1]).await.unwrap(),
        super::Claim::New
    ));
    database
        .execute(&[
            "UPDATE platform.write_log SET created_at = clock_timestamp() - interval '31 days'",
            wamn_control_provision::audit_retention::WRITE_LOG_RETENTION_SQL,
        ])
        .unwrap();
    assert!(matches!(
        store.claim(OPERATION, "expired", vec![1]).await.unwrap(),
        super::Claim::New
    ));
}

#[tokio::test]
async fn cancellation_retains_external_claims_and_rolls_back_transactional_claims() {
    use crate::invocation::{AuthorizedInvocation, InvocationRequest, PrincipalVerification};
    use wamn_engine::operation::{native_call::NativeOutcome, node_types::NodeError};

    let _lock = wamn_test_postgres::lock();
    let (database, postgres) = project_env_key_database("test--dev--writelogtx").await;
    let mut observer = wamn_postgres::connect(database.url()).await.unwrap();
    let route = wamn_catalog::ServingRoute {
        package_id: "base".into(),
        component: "widget".into(),
        operation: OPERATION.into(),
        type_: wamn_catalog::OperationType::Command,
        idempotency: None,
        reads: Default::default(),
        revision: None,
    };
    for (key, transacted) in [("external", false), ("sql", true)] {
        let authority = AuthorizedInvocation::enter(
            &postgres,
            InvocationRequest {
                scope: format!("cancellation-{key}"),
                identity: SessionIdentity {
                    project_env_key: "test--dev--writelogtx".into(),
                    user_id: Some(LIVE_PRINCIPAL.into()),
                    operation: Some(OPERATION.into()),
                    ..SessionIdentity::default()
                },
                caller: None,
                registered_operation: None,
                causation: None,
                invocation: None,
                statements: WamnPostgres::prepare_statement_set(Default::default(), false),
                members: WamnPostgres::prepare_statement_members(std::iter::empty()),
                principal: PrincipalVerification::Authenticated,
            },
        )
        .await
        .unwrap();
        let result = postgres
            .invoke_node(
                &authority,
                OPERATION,
                &route,
                transacted,
                &json!([{"request_id":key,"value":7}]),
                |_| std::future::ready(Ok(NativeOutcome::Json(Err(NodeError::Cancelled)))),
            )
            .await
            .unwrap();
        assert!(matches!(result, Err(NodeError::Cancelled)));
        assert!(postgres.operation_transaction_for(authority.id()).is_none());
        let rows: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM platform.write_log WHERE idempotency_key = $1 AND result IS NULL",
        )
        .bind(key)
        .fetch_one(&mut observer)
        .await
        .unwrap();
        assert_eq!(rows, if transacted { 0 } else { 1 });
    }
}
