//! Disposable-PostgreSQL proof for the production DbState observation boundary.

use std::time::Duration;

use tokio_postgres::NoTls;
use wamn_scenario_model::TestCase;
use wamn_scenario_runtime::{
    DbStateCaptureFailure, DbStateCaptureFailureKind, DbStateCaptureLimits, capture_db_assertions,
    capture_db_assertions_with_limits,
};

const SCHEMA: &str = "scenario_dbstate_boundary";
const OBSERVER: &str = "wamn_dbstate_observer";
const PASSWORD: &str = "observer-secret";

fn case(name: &str, queries: &[&str]) -> TestCase {
    let expect = queries
        .iter()
        .map(|query| {
            serde_json::json!({
                "db-state": {
                    "query": query,
                    "params": [],
                    "expect": "empty"
                }
            })
        })
        .collect::<Vec<_>>();
    serde_json::from_value(serde_json::json!({
        "schema-version": "0.1",
        "name": name,
        "flow-ref": {"flow-id": "db-state-proof", "version": 1},
        "input": {},
        "expect": expect
    }))
    .expect("proof case is valid")
}

fn limits(timeout_ms: u64, max_rows: u32, max_json_bytes: u32) -> DbStateCaptureLimits {
    DbStateCaptureLimits::new(Duration::from_millis(timeout_ms), max_rows, max_json_bytes)
        .expect("proof limits are finite and non-zero")
}

fn capture_failure(error: &anyhow::Error) -> &DbStateCaptureFailure {
    error
        .downcast_ref::<DbStateCaptureFailure>()
        .expect("bounded observation refusal must retain its typed failure")
}

async fn assert_bounded_failure(
    client: &mut tokio_postgres::Client,
    name: &str,
    query: &str,
    limits: DbStateCaptureLimits,
    expected: DbStateCaptureFailureKind,
) {
    let error = capture_db_assertions_with_limits(client, &case(name, &[query]), limits)
        .await
        .expect_err("bounded DbState query must fail");
    let failure = capture_failure(&error);
    assert_eq!(failure.kind(), expected);
    assert!(!failure.to_string().contains(query));
    assert!(!failure.to_string().contains(PASSWORD));
}

async fn assert_read_only_rejection(client: &mut tokio_postgres::Client, name: &str, query: &str) {
    let error = capture_db_assertions(client, &case(name, &[query]))
        .await
        .expect_err("mutating DbState query must be rejected");
    let failure = error
        .downcast_ref::<DbStateCaptureFailure>()
        .unwrap_or_else(|| panic!("read-only rejection must retain its typed failure: {error:?}"));
    assert_eq!(failure.kind(), DbStateCaptureFailureKind::ReadOnlyViolation);
    assert_eq!(failure.to_string(), "db-state observation rejected a write");
    assert!(!failure.to_string().contains(query));
    assert!(!failure.to_string().contains(PASSWORD));
}

async fn assert_control_is_contained(client: &mut tokio_postgres::Client, command: &str) {
    let observation = "SELECT jsonb_build_object(\
        'read-only', current_setting('transaction_read_only') = 'on', \
        'value', value) \
        FROM fixture WHERE tenant_id = current_setting('app.tenant')";
    match capture_db_assertions(client, &case(command, &[command, observation])).await {
        Ok(captures) => {
            assert_eq!(captures.len(), 2);
            assert_eq!(
                captures[1].rows,
                vec![serde_json::json!({"read-only": true, "value": 1})]
            );
        }
        Err(error) => {
            let failure = error
                .downcast_ref::<DbStateCaptureFailure>()
                .expect("transaction-state rejection must be typed");
            assert_eq!(
                failure.kind(),
                DbStateCaptureFailureKind::TransactionControl
            );
            let captures = capture_db_assertions(client, &case("after-control", &[observation]))
                .await
                .expect("a rejected control statement must not poison the next observation");
            assert_eq!(
                captures[0].rows,
                vec![serde_json::json!({"read-only": true, "value": 1})]
            );
        }
    }
}

#[tokio::test]
#[ignore = "requires WAMN_DB_STATE_TEST_ADMIN_URL pointing at disposable PostgreSQL"]
async fn db_state_boundary_rejects_writes_and_contains_transaction_control() {
    let admin_url = std::env::var("WAMN_DB_STATE_TEST_ADMIN_URL")
        .expect("set WAMN_DB_STATE_TEST_ADMIN_URL to disposable PostgreSQL");
    let (admin, admin_connection) = tokio_postgres::connect(&admin_url, NoTls)
        .await
        .expect("connect proof admin");
    let admin_task = tokio::spawn(async move {
        let _ = admin_connection.await;
    });
    admin
        .batch_execute(&format!(
            "DROP SCHEMA IF EXISTS {SCHEMA} CASCADE;
             DO $$
             BEGIN
                 CREATE ROLE {OBSERVER} LOGIN PASSWORD '{PASSWORD}'
                     NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS;
             EXCEPTION WHEN duplicate_object THEN
                 NULL;
             END
             $$;
             CREATE SCHEMA {SCHEMA} AUTHORIZATION postgres;
             CREATE TABLE {SCHEMA}.fixture (
                 tenant_id text NOT NULL,
                 value integer NOT NULL
             );
             ALTER TABLE {SCHEMA}.fixture ENABLE ROW LEVEL SECURITY;
             ALTER TABLE {SCHEMA}.fixture FORCE ROW LEVEL SECURITY;
             CREATE POLICY fixture_tenant ON {SCHEMA}.fixture
                 USING (tenant_id = current_setting('app.tenant'))
                 WITH CHECK (tenant_id = current_setting('app.tenant'));
             GRANT USAGE ON SCHEMA {SCHEMA} TO {OBSERVER};
             GRANT SELECT, INSERT, UPDATE, DELETE ON {SCHEMA}.fixture TO {OBSERVER};
             INSERT INTO {SCHEMA}.fixture VALUES ('tenant-a', 1), ('tenant-b', 9);"
        ))
        .await
        .expect("provision proof fixture");

    let mut observer_config: tokio_postgres::Config =
        admin_url.parse().expect("parse proof admin URL");
    observer_config.user(OBSERVER).password(PASSWORD);
    let (mut observer, observer_connection) = observer_config
        .connect(NoTls)
        .await
        .expect("connect least-privileged observer");
    let observer_task = tokio::spawn(async move {
        let _ = observer_connection.await;
    });
    observer
        .query_one(
            "SELECT set_config('app.tenant', 'tenant-a', false),
                    set_config('search_path', $1, false)",
            &[&SCHEMA],
        )
        .await
        .expect("scope observer");

    let select = capture_db_assertions(
        &mut observer,
        &case(
            "select",
            &["SELECT to_jsonb(f) FROM fixture f ORDER BY value"],
        ),
    )
    .await
    .expect("ordinary SELECT remains unchanged");
    assert_eq!(
        select[0].rows,
        vec![serde_json::json!({"tenant_id": "tenant-a", "value": 1})]
    );

    let original_statement_timeout: String = observer
        .query_one("SHOW statement_timeout", &[])
        .await
        .expect("read original statement timeout")
        .get(0);
    let local_timeout = capture_db_assertions_with_limits(
        &mut observer,
        &case(
            "transaction-local-timeout",
            &["SELECT to_jsonb(current_setting('statement_timeout'))"],
        ),
        limits(17, 2, 1024),
    )
    .await
    .expect("statement timeout is visible inside the observation");
    assert_eq!(local_timeout[0].rows, vec![serde_json::json!("17ms")]);
    let restored_statement_timeout: String = observer
        .query_one("SHOW statement_timeout", &[])
        .await
        .expect("read restored statement timeout")
        .get(0);
    assert_eq!(restored_statement_timeout, original_statement_timeout);

    assert_bounded_failure(
        &mut observer,
        "statement-timeout",
        "SELECT to_jsonb(count(*)) FROM generate_series(1, 10000000)",
        limits(1, 2, 1024),
        DbStateCaptureFailureKind::StatementTimeout,
    )
    .await;
    assert_bounded_failure(
        &mut observer,
        "row-limit",
        "SELECT to_jsonb(n) FROM generate_series(1, 3) AS n",
        limits(1_000, 2, 1024),
        DbStateCaptureFailureKind::RowLimit,
    )
    .await;
    assert_bounded_failure(
        &mut observer,
        "single-value-byte-limit",
        "SELECT to_jsonb(repeat('x', 8))",
        limits(1_000, 2, 9),
        DbStateCaptureFailureKind::ByteLimit,
    )
    .await;
    assert_bounded_failure(
        &mut observer,
        "cumulative-byte-limit",
        "SELECT to_jsonb('xxxx'::text) FROM generate_series(1, 2)",
        limits(1_000, 2, 10),
        DbStateCaptureFailureKind::ByteLimit,
    )
    .await;
    let exact_byte_boundary = capture_db_assertions_with_limits(
        &mut observer,
        &case("exact-byte-boundary", &["SELECT to_jsonb(repeat('x', 7))"]),
        limits(1_000, 1, 9),
    )
    .await
    .expect("a JSON value exactly at the byte limit remains valid");
    assert_eq!(
        exact_byte_boundary[0].rows,
        vec![serde_json::json!("xxxxxxx")]
    );
    assert_bounded_failure(
        &mut observer,
        "result-shape",
        "SELECT 1",
        limits(1_000, 2, 1024),
        DbStateCaptureFailureKind::ResultShape,
    )
    .await;
    assert_bounded_failure(
        &mut observer,
        "external-cancellation",
        "SELECT to_jsonb(pg_cancel_backend(pg_backend_pid()))",
        limits(1_000, 2, 1024),
        DbStateCaptureFailureKind::Cancelled,
    )
    .await;

    let after_bounded_failures = capture_db_assertions(
        &mut observer,
        &case("after-bounded-failures", &["SELECT to_jsonb(1)"]),
    )
    .await
    .expect("bounded failures roll back and do not poison the observer");
    assert_eq!(after_bounded_failures[0].rows, vec![serde_json::json!(1)]);

    assert_read_only_rejection(
        &mut observer,
        "update-returning",
        "UPDATE fixture SET value = 2 RETURNING to_jsonb(fixture)",
    )
    .await;
    assert_read_only_rejection(
        &mut observer,
        "data-modifying-cte",
        "WITH changed AS (
             DELETE FROM fixture RETURNING *
         )
         SELECT to_jsonb(changed) FROM changed",
    )
    .await;
    assert_read_only_rejection(
        &mut observer,
        "ddl",
        "CREATE TABLE durable_escape (value integer)",
    )
    .await;

    observer
        .batch_execute("CREATE TEMP TABLE rollback_probe (value integer); INSERT INTO rollback_probe VALUES (1)")
        .await
        .expect("create rollback witness");
    let rollback = capture_db_assertions(
        &mut observer,
        &case(
            "explicit-rollback",
            &[
                "UPDATE rollback_probe SET value = 2 RETURNING to_jsonb(rollback_probe)",
                "SELECT to_jsonb(rollback_probe) FROM rollback_probe",
            ],
        ),
    )
    .await
    .expect("temporary writes are permitted but must be rolled back");
    assert_eq!(rollback[0].rows, vec![serde_json::json!({"value": 2})]);
    assert_eq!(rollback[1].rows, vec![serde_json::json!({"value": 1})]);

    for command in ["COMMIT", "ROLLBACK", "SET TRANSACTION READ WRITE"] {
        assert_control_is_contained(&mut observer, command).await;
    }
    let combined =
        "SET TRANSACTION READ WRITE; UPDATE fixture SET value = 3 RETURNING to_jsonb(fixture)";
    let combined_error = capture_db_assertions(
        &mut observer,
        &case("combined-control-and-write", &[combined]),
    )
    .await
    .expect_err("extended-query path must reject multiple statements");
    let postgres_error = combined_error
        .downcast_ref::<tokio_postgres::Error>()
        .expect("multiple-statement rejection must retain PostgreSQL error");
    assert_eq!(
        postgres_error.code(),
        Some(&tokio_postgres::error::SqlState::SYNTAX_ERROR)
    );

    let fixture: Vec<(String, i32)> = admin
        .query(
            &format!("SELECT tenant_id, value FROM {SCHEMA}.fixture ORDER BY tenant_id"),
            &[],
        )
        .await
        .expect("verify fixture")
        .into_iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect();
    assert_eq!(
        fixture,
        vec![("tenant-a".to_string(), 1), ("tenant-b".to_string(), 9)]
    );
    let escaped: bool = admin
        .query_one(
            "SELECT EXISTS (
                 SELECT 1 FROM information_schema.tables
                 WHERE table_schema = $1 AND table_name = 'durable_escape'
             )",
            &[&SCHEMA],
        )
        .await
        .expect("verify DDL did not persist")
        .get(0);
    assert!(!escaped);

    let (mut doomed_observer, doomed_connection) = observer_config
        .connect(NoTls)
        .await
        .expect("connect dependency-loss observer");
    let doomed_task = tokio::spawn(async move {
        let _ = doomed_connection.await;
    });
    doomed_observer
        .query_one(
            "SELECT set_config('app.tenant', 'tenant-a', false),
                    set_config('search_path', $1, false)",
            &[&SCHEMA],
        )
        .await
        .expect("scope dependency-loss observer");
    assert_bounded_failure(
        &mut doomed_observer,
        "dependency-loss",
        "SELECT to_jsonb(pg_terminate_backend(pg_backend_pid()))",
        limits(1_000, 2, 1024),
        DbStateCaptureFailureKind::DependencyUnavailable,
    )
    .await;
    drop(doomed_observer);
    doomed_task.abort();

    drop(observer);
    observer_task.abort();
    admin
        .batch_execute(&format!("DROP SCHEMA {SCHEMA} CASCADE"))
        .await
        .expect("clean proof fixture");
    drop(admin);
    admin_task.abort();
}
