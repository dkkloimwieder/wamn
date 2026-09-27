//! Live PostgreSQL 18 test of the write log and its three statements
//! (`docs/plan/write-log.md` 4.1 and 4.2).
//!
//! Each session applies `SET ROLE wamn_app`, so the grants of
//! `deploy/sql/app-schema.sql` decide what it can do. Each claim runs in a
//! `READ COMMITTED` transaction, as the generated codec runs it.

use std::time::Duration;

use tokio_postgres::{Client, NoTls};
use wamn_schema_generator::write_log::{LOG_CLAIM_SQL, LOG_FINISH_SQL, LOG_READ_SQL};

const RECORD_HISTORY_SQL: &str = include_str!("../../../../deploy/sql/record-history.sql");
const RECORD_HISTORY_APP_GRANTS_SQL: &str =
    include_str!("../../../../deploy/sql/record-history-app-grants.sql");
const APP_SCHEMA_SQL: &str = include_str!("../../../../deploy/sql/app-schema.sql");

const OPERATION: &str = "wamn-fixture:widget/create";
const BEGIN: &str = "BEGIN ISOLATION LEVEL READ COMMITTED";

fn log_database() -> wamn_test_postgres::Database {
    let database = wamn_test_postgres::database();
    database
        .execute(&[
            "DO $$ BEGIN CREATE ROLE wamn_app NOLOGIN; \
             EXCEPTION WHEN duplicate_object THEN NULL; END $$",
            RECORD_HISTORY_SQL,
            RECORD_HISTORY_APP_GRANTS_SQL,
            APP_SCHEMA_SQL,
        ])
        .expect("apply the app schema");
    database
}

/// One superuser session. It sees the wait state of every backend.
async fn superuser(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to the test database");
    tokio::spawn(connection);
    client
}

/// One session under the grants of `wamn_app`.
async fn session(url: &str) -> Client {
    let client = superuser(url).await;
    client
        .batch_execute("SET ROLE wamn_app")
        .await
        .expect("act as wamn_app");
    client
}

async fn claim(client: &Client, key: &str, request: &[u8]) -> bool {
    !client
        .query(LOG_CLAIM_SQL, &[&OPERATION, &key, &request])
        .await
        .expect("claim the key")
        .is_empty()
}

async fn read(client: &Client, key: &str) -> Option<(Vec<u8>, Option<String>)> {
    client
        .query_opt(LOG_READ_SQL, &[&OPERATION, &key])
        .await
        .expect("read the key")
        .map(|row| (row.get(0), row.get(1)))
}

async fn finish(client: &Client, key: &str, result: &str) {
    let rows = client
        .query(LOG_FINISH_SQL, &[&OPERATION, &key, &result])
        .await
        .expect("finish the claim");
    assert_eq!(rows.len(), 1, "the finish stores one result");
}

/// Wait until the backend of `blocked` waits on a lock.
async fn wait_for_lock(observer: &Client, blocked: i32) {
    for _ in 0..200 {
        let waiting: bool = observer
            .query_one(
                "SELECT wait_event_type IS NOT DISTINCT FROM 'Lock' \
                   FROM pg_stat_activity WHERE pid = $1",
                &[&blocked],
            )
            .await
            .expect("read the activity of the waiting claim")
            .get(0);
        if waiting {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the second claim never waited on the first transaction");
}

async fn backend(client: &Client) -> i32 {
    client
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .expect("read the backend pid")
        .get(0)
}

/// A claim wins once. A later claim of the committed key inserts nothing, and
/// its read answers the stored request and result.
#[tokio::test(flavor = "current_thread")]
async fn a_claim_wins_once_and_a_later_claim_reads_the_committed_result() {
    let _lock = wamn_test_postgres::lock();
    let database = log_database();
    let first = session(database.url()).await;
    first.batch_execute(BEGIN).await.expect("begin");
    assert!(
        claim(&first, "k-1", b"request").await,
        "the first claim wins"
    );
    finish(&first, "k-1", r#"{"value":1}"#).await;
    first.batch_execute("COMMIT").await.expect("commit");

    let second = session(database.url()).await;
    second.batch_execute(BEGIN).await.expect("begin");
    assert!(
        !claim(&second, "k-1", b"request").await,
        "a committed key does not claim again"
    );
    assert_eq!(
        read(&second, "k-1").await,
        Some((b"request".to_vec(), Some(r#"{"value":1}"#.to_owned())))
    );
    second.batch_execute("ROLLBACK").await.expect("roll back");
}

/// A second claim of a key that an open transaction holds waits for it. After
/// the commit it inserts nothing, and its read sees the committed result.
#[tokio::test(flavor = "current_thread")]
async fn a_second_claim_waits_for_the_open_claim_and_reads_its_result() {
    let _lock = wamn_test_postgres::lock();
    let database = log_database();
    let first = session(database.url()).await;
    let observer = superuser(database.url()).await;
    first.batch_execute(BEGIN).await.expect("begin");
    assert!(claim(&first, "k-2", b"request").await);

    let url = database.url().to_owned();
    let (pid_sender, pid) = tokio::sync::oneshot::channel();
    let waiting = tokio::spawn(async move {
        let second = session(&url).await;
        pid_sender
            .send(backend(&second).await)
            .expect("send the pid");
        second.batch_execute(BEGIN).await.expect("begin");
        let claimed = claim(&second, "k-2", b"request").await;
        let stored = read(&second, "k-2").await;
        second.batch_execute("ROLLBACK").await.expect("roll back");
        (claimed, stored)
    });
    wait_for_lock(&observer, pid.await.expect("the waiting pid")).await;
    finish(&first, "k-2", r#"{"value":2}"#).await;
    first.batch_execute("COMMIT").await.expect("commit");

    let (claimed, stored) = waiting.await.expect("the second claim ends");
    assert!(
        !claimed,
        "the waiting claim inserts nothing after the commit"
    );
    assert_eq!(
        stored,
        Some((b"request".to_vec(), Some(r#"{"value":2}"#.to_owned())))
    );
}

/// A rolled-back claim leaves no row, and a claim that waited on it wins.
#[tokio::test(flavor = "current_thread")]
async fn a_rolled_back_claim_leaves_no_row() {
    let _lock = wamn_test_postgres::lock();
    let database = log_database();
    let first = session(database.url()).await;
    let observer = superuser(database.url()).await;
    first.batch_execute(BEGIN).await.expect("begin");
    assert!(claim(&first, "k-3", b"first").await);

    let url = database.url().to_owned();
    let (pid_sender, pid) = tokio::sync::oneshot::channel();
    let waiting = tokio::spawn(async move {
        let second = session(&url).await;
        pid_sender
            .send(backend(&second).await)
            .expect("send the pid");
        second.batch_execute(BEGIN).await.expect("begin");
        let claimed = claim(&second, "k-3", b"second").await;
        second.batch_execute("ROLLBACK").await.expect("roll back");
        claimed
    });
    wait_for_lock(&observer, pid.await.expect("the waiting pid")).await;
    first.batch_execute("ROLLBACK").await.expect("roll back");
    assert!(
        waiting.await.expect("the second claim ends"),
        "a claim that waited on a rollback wins"
    );
    assert_eq!(read(&observer, "k-3").await, None, "no claim committed");
}

/// `wamn_app` stores a result and changes no other column, and deletes nothing.
#[tokio::test(flavor = "current_thread")]
async fn wamn_app_updates_the_result_and_no_other_column() {
    let _lock = wamn_test_postgres::lock();
    let database = log_database();
    let client = session(database.url()).await;
    client.batch_execute(BEGIN).await.expect("begin");
    assert!(claim(&client, "k-4", b"request").await);
    finish(&client, "k-4", r#"{"value":4}"#).await;
    client.batch_execute("COMMIT").await.expect("commit");

    for statement in [
        "UPDATE app_system.write_log SET request = '\\x01'::bytea",
        "UPDATE app_system.write_log SET idempotency_key = 'other'",
        "UPDATE app_system.write_log SET operation = 'other'",
        "UPDATE app_system.write_log SET created_at = now()",
        "DELETE FROM app_system.write_log",
        "TRUNCATE app_system.write_log",
    ] {
        let error = client
            .batch_execute(statement)
            .await
            .expect_err("wamn_app holds no such privilege");
        assert_eq!(
            error.code(),
            Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE),
            "{statement}: {error}"
        );
    }
    client
        .batch_execute("UPDATE app_system.write_log SET result = '{}'")
        .await
        .expect("wamn_app updates the result");
}
