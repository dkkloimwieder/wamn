//! The write log as the engine's intent store, on disposable databases.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;
use tokio::time::Duration;
use wamn_run_state::IntentStore;
use wamn_run_state::intent_cases::{self, CaseStores, IntentStoreFixture};
use wamn_run_state::intent_store::{Begun, Intent, StoredOutcome};

use super::WriteLogStore;
use crate::plugins::wamn_postgres::claims::tests::{
    LIVE_PRINCIPAL, ensure_live_users_rows, live_guest_url,
};
use crate::plugins::wamn_postgres::{
    ClassCredentials, SessionClaims, WamnPostgres, WamnPostgresConfig,
};

/// Bounds a wait for an observed backend state. It only stops a test that a
/// defect would hang.
const HANG_GUARD: Duration = Duration::from_secs(120);
const PROJECT: &str = "default";
const SCOPE: &str = "write-log-item";

/// The write log section of the schema that every project database gets.
fn write_log_sql() -> &'static str {
    let schema = include_str!("../../../../../../../deploy/sql/app-schema.sql");
    let start = schema
        .find("CREATE TABLE IF NOT EXISTS app_system.write_log")
        .expect("the schema creates the write log");
    let grant = "GRANT DELETE ON app_system.write_log TO wamn_app;";
    let end = schema[start..]
        .find(grant)
        .expect("the schema grants the write log")
        + start
        + grant.len();
    &schema[start..end]
}

/// A plugin over a fresh database of `tenant`, with the write log installed.
async fn tenant_database(tenant: &str) -> (wamn_test_postgres::Database, Arc<WamnPostgres>) {
    let database = wamn_test_postgres::database();
    let guest_url = live_guest_url(database.url(), tenant).await;
    ensure_live_users_rows(database.url(), tenant, &[LIVE_PRINCIPAL]).await;
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

/// Each case gets two fresh databases: the database is the tenant.
#[derive(Default)]
struct Databases(Mutex<Vec<wamn_test_postgres::Database>>);

#[async_trait]
impl IntentStoreFixture for Databases {
    async fn stores(&self, _case: &'static str) -> CaseStores {
        let (database, postgres) = tenant_database("writeloga").await;
        let (other_database, other_postgres) = tenant_database("writelogb").await;
        self.0
            .lock()
            .expect("databases lock poisoned")
            .extend([database, other_database]);
        CaseStores {
            store: Arc::new(WriteLogStore::new(
                postgres,
                PROJECT.into(),
                "writeloga".into(),
            )),
            tenant: "writeloga".into(),
            other: Arc::new(WriteLogStore::new(
                other_postgres,
                PROJECT.into(),
                "writelogb".into(),
            )),
            other_tenant: "writelogb".into(),
        }
    }
}

/// The two-step store keeps every shared intent rule except operator
/// resolution, which the write log does not have: a claim that never
/// finishes stays uncertain, and the client sends a new key.
#[tokio::test]
async fn the_two_step_write_log_keeps_the_intent_rules() {
    let _lock = wamn_test_postgres::lock();
    let fixture = Databases::default();
    for case in intent_cases::CASES {
        if case == "a_resolved_intent_leaves_the_list_and_answers_its_basis" {
            continue;
        }
        intent_cases::run(&fixture, case).await;
    }
}

fn intent<'a>(key: &'a str, input_hash: &'a str) -> Intent<'a> {
    Intent {
        tenant: "writelogtx",
        release: "release-1",
        package: "base",
        operation: "base:widget/create@1.0.0",
        idempotency_key: key,
        input_hash,
        deadline_ms: 5_000,
    }
}

/// An operation with SQL claims in its item transaction: the claim commits
/// with the result, a retry answers the stored result or a conflict, a
/// rollback frees the key, and a second claim of a key waits for the first.
#[tokio::test]
async fn the_claim_of_an_item_commits_with_its_transaction() {
    let _lock = wamn_test_postgres::lock();
    let (database, postgres) = tenant_database("writelogtx").await;
    postgres
        .bind_session_claims(
            SCOPE,
            &SessionClaims {
                tenant: "writelogtx".into(),
                user_id: Some(LIVE_PRINCIPAL.into()),
                operation: Some("base:widget/create@1.0.0".into()),
                ..SessionClaims::default()
            },
        )
        .await
        .expect("claims bind");
    let begin = async || {
        postgres
            .begin_operation_transaction(SCOPE)
            .await
            .expect("the item transaction begins")
    };
    let rows = || {
        database
            .execute(&["SELECT count(*) FROM app_system.write_log"])
            .expect("count the write log")
    };
    let outcome = StoredOutcome(json!({"value": {"id": 7}}));

    let first = begin().await;
    let Begun::New(id) = first.begin(&intent("k1", "h1")).await.expect("claim") else {
        panic!("a new key is new");
    };
    assert_eq!(
        id.0, "base:widget/create#k1",
        "the version is not part of the claim"
    );
    first.finish(&id, &outcome).await.expect("finish");
    assert_eq!(rows().trim(), "0", "nothing commits before the owner");
    first.commit().await.expect("commit");

    let retry = begin().await;
    assert_eq!(
        retry.begin(&intent("k1", "h1")).await.expect("retry"),
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
        "a refused item frees its key for any request"
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
            let waits = database
                .execute(&["SELECT count(*) FROM pg_stat_activity \
                     WHERE wait_event_type = 'Lock' \
                       AND query LIKE 'INSERT INTO app_system.write_log%'"])
                .expect("read the lock waits");
            if waits.trim() == "1" {
                break;
            }
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
    assert_eq!(rows().trim(), "2", "k1 and k3 committed, k2 left no row");
}
