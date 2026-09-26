//! The SQLite intent log on a temporary file, including a process killed after
//! `begin`.

use std::io::{BufRead as _, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

use wamn_run_state::IntentStore;
use wamn_run_state::intent_store::{Begun, Intent, IntentId, StoreErrorKind, StoredOutcome};
use wamn_run_state::operator_action::OperatorActionBasis;
use wamn_run_state_sqlite::SqliteIntentStore;

/// The child reads the database path from this variable.
const CHILD_DATABASE: &str = "WAMN_INTENT_STORE_CHILD_DATABASE";
const CHILD_TEST: &str = "child_begins_an_intent_and_waits_to_be_killed";

/// A new database path under Cargo's temporary directory for this test target.
fn database(name: &str) -> PathBuf {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let directory = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "intent-store-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&directory).expect("create the test directory");
    directory.join(format!("{name}.db"))
}

fn intent<'a>(key: &'a str, input_hash: &'a str) -> Intent<'a> {
    Intent {
        tenant: "tenant-a",
        release: "release-1",
        package: "scale",
        operation: "record_sample",
        idempotency_key: key,
        input_hash,
        deadline_ms: 5_000,
    }
}

fn new_id(begun: Begun) -> IntentId {
    match begun {
        Begun::New(id) => id,
        other => panic!("expected a new intent, got {other:?}"),
    }
}

#[tokio::test]
async fn a_finished_key_returns_its_stored_outcome() {
    let store = SqliteIntentStore::open(database("finished")).expect("open");
    for (key, outcome) in [
        (
            "k-completed",
            StoredOutcome::Completed(serde_json::json!({"grams": 1250})),
        ),
        (
            "k-failed",
            StoredOutcome::Failed(serde_json::json!({"code": "refused"})),
        ),
    ] {
        let id = new_id(store.begin(&intent(key, "h1")).await.expect("begin"));
        store.finish(&id, &outcome).await.expect("finish");
        assert_eq!(
            store.begin(&intent(key, "h1")).await.expect("begin again"),
            Begun::Finished(outcome)
        );
    }
    assert!(store.uncertain(10).await.expect("uncertain").is_empty());
}

#[tokio::test]
async fn a_begun_key_is_uncertain_and_never_new_again() {
    let store = SqliteIntentStore::open(database("begun")).expect("open");
    let id = new_id(store.begin(&intent("k1", "h1")).await.expect("begin"));
    assert_eq!(
        store.begin(&intent("k1", "h1")).await.expect("begin again"),
        Begun::Uncertain(id)
    );
}

#[tokio::test]
async fn a_repeated_key_with_another_input_conflicts() {
    let store = SqliteIntentStore::open(database("conflict")).expect("open");
    let id = new_id(store.begin(&intent("k1", "h1")).await.expect("begin"));
    assert_eq!(
        store.begin(&intent("k1", "h2")).await.expect("begin again"),
        Begun::Conflict(id.clone())
    );
    assert_eq!(
        store
            .begin(&intent("k1", "h1"))
            .await
            .expect("begin a third time"),
        Begun::Uncertain(id),
        "a conflict leaves the stored intent unchanged"
    );
}

#[tokio::test]
async fn keys_belong_to_their_tenant() {
    let store = SqliteIntentStore::open(database("tenants")).expect("open");
    store.begin(&intent("k1", "h1")).await.expect("begin");
    let other = Intent {
        tenant: "tenant-b",
        ..intent("k1", "h2")
    };
    new_id(
        store
            .begin(&other)
            .await
            .expect("another tenant's key is new"),
    );
}

#[tokio::test]
async fn finish_closes_an_intent_once() {
    let store = SqliteIntentStore::open(database("finish-twice")).expect("open");
    let id = new_id(store.begin(&intent("k1", "h1")).await.expect("begin"));
    let outcome = StoredOutcome::Completed(serde_json::json!(null));
    store.finish(&id, &outcome).await.expect("finish");
    let error = store
        .finish(&id, &outcome)
        .await
        .expect_err("a finished intent does not finish again");
    assert_eq!(error.kind(), StoreErrorKind::Contract);
    let error = store
        .finish(&IntentId("999".into()), &outcome)
        .await
        .expect_err("an unknown intent does not finish");
    assert_eq!(error.kind(), StoreErrorKind::Contract);
}

#[tokio::test]
async fn uncertain_lists_open_intents_oldest_first_up_to_the_limit() {
    let store = SqliteIntentStore::open(database("uncertain")).expect("open");
    let first = new_id(store.begin(&intent("k1", "h1")).await.expect("begin"));
    let finished = new_id(store.begin(&intent("k2", "h2")).await.expect("begin"));
    let third = new_id(store.begin(&intent("k3", "h3")).await.expect("begin"));
    store
        .finish(&finished, &StoredOutcome::Completed(serde_json::json!(1)))
        .await
        .expect("finish");

    let open: Vec<IntentId> = store
        .uncertain(10)
        .await
        .expect("uncertain")
        .into_iter()
        .map(|intent| intent.id)
        .collect();
    assert_eq!(open, [first.clone(), third]);

    let limited = store.uncertain(1).await.expect("uncertain");
    assert_eq!(limited.len(), 1);
    assert_eq!(limited[0].id, first);
    assert_eq!(limited[0].idempotency_key, "k1");
    assert_eq!(limited[0].operation, "record_sample");
}

#[tokio::test]
async fn a_resolved_intent_leaves_the_list_and_answers_its_basis() {
    let store = SqliteIntentStore::open(database("resolve")).expect("open");
    let id = new_id(store.begin(&intent("k1", "h1")).await.expect("begin"));
    store
        .resolve(&id, OperatorActionBasis::OperatorJudgment)
        .await
        .expect("resolve");
    assert!(store.uncertain(10).await.expect("uncertain").is_empty());
    assert_eq!(
        store.begin(&intent("k1", "h1")).await.expect("begin again"),
        Begun::Resolved {
            id: id.clone(),
            basis: OperatorActionBasis::OperatorJudgment
        }
    );
    let error = store
        .resolve(&id, OperatorActionBasis::OperatorJudgment)
        .await
        .expect_err("a resolved intent does not resolve again");
    assert_eq!(error.kind(), StoreErrorKind::Contract);
    let error = store
        .finish(&id, &StoredOutcome::Completed(serde_json::json!(1)))
        .await
        .expect_err("a resolved intent does not finish");
    assert_eq!(error.kind(), StoreErrorKind::Contract);
}

#[tokio::test]
async fn a_finished_intent_does_not_resolve() {
    let store = SqliteIntentStore::open(database("resolve-finished")).expect("open");
    let id = new_id(store.begin(&intent("k1", "h1")).await.expect("begin"));
    store
        .finish(&id, &StoredOutcome::Completed(serde_json::json!(1)))
        .await
        .expect("finish");
    let error = store
        .resolve(&id, OperatorActionBasis::ExternalEvidence)
        .await
        .expect_err("a finished intent is not uncertain");
    assert_eq!(error.kind(), StoreErrorKind::Contract);
}

/// The kill test runs this test binary again as a child that begins one intent.
#[tokio::test]
async fn closed_resolves_when_the_last_clone_drops() {
    let path = database("closed");
    let store = SqliteIntentStore::open(&path).expect("open");
    let clone = store.clone();
    let mut closed = Box::pin(store.closed().wait());
    drop(store);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut closed)
            .await
            .is_err(),
        "a live clone keeps the file open"
    );
    assert!(
        SqliteIntentStore::open(&path).is_err(),
        "the clone holds the file"
    );
    drop(clone);
    closed.await;
    SqliteIntentStore::open(&path).expect("the closed file opens at once");
}

#[tokio::test]
async fn reopen_after_kill_finds_the_uncertain_intent() {
    let path = database("killed");
    let mut child = Command::new(std::env::current_exe().expect("the test binary"))
        .args([CHILD_TEST, "--exact", "--ignored", "--nocapture"])
        .env(CHILD_DATABASE, &path)
        .stdout(Stdio::piped())
        .spawn()
        .expect("start the child");
    let stdout = child.stdout.take().expect("the child's stdout");
    let begun = BufReader::new(stdout)
        .lines()
        .map(|line| line.expect("read the child's stdout"))
        .find_map(|line| line.strip_prefix("begun ").map(str::to_owned))
        .expect("the child begins an intent");

    let error = SqliteIntentStore::open(&path)
        .expect_err("the child holds the file, so a second open fails");
    assert_eq!(error.kind(), StoreErrorKind::Storage);

    // `Child::kill` sends SIGKILL on Unix: no destructor and no close runs.
    child.kill().expect("kill the child");
    child.wait().expect("reap the child");

    let store = SqliteIntentStore::open(&path).expect("reopen after the kill");
    let open = store.uncertain(10).await.expect("uncertain");
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].id, IntentId(begun));
    assert_eq!(open[0].idempotency_key, "k-killed");
    assert_eq!(
        store
            .begin(&intent("k-killed", "h1"))
            .await
            .expect("begin again"),
        Begun::Uncertain(open[0].id.clone())
    );
}

#[tokio::test]
#[ignore = "the child process of reopen_after_kill_finds_the_uncertain_intent"]
async fn child_begins_an_intent_and_waits_to_be_killed() {
    let path = std::env::var_os(CHILD_DATABASE).expect("run only by the kill test");
    let store = SqliteIntentStore::open(path).expect("open");
    let id = new_id(store.begin(&intent("k-killed", "h1")).await.expect("begin"));
    println!("begun {}", id.0);
    std::thread::sleep(std::time::Duration::from_secs(600));
}
