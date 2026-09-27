//! The SQLite intent log on a temporary file: the shared intent store case
//! set, a process killed after `begin`, and the close signal.

use std::io::{BufRead as _, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use wamn_run_state::IntentStore;
use wamn_run_state::intent_cases::{self, CaseStores, IntentStoreFixture};
use wamn_run_state::intent_store::{Begun, Intent, IntentId, StoreErrorKind};
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

/// Each case of the shared intent store case set opens a fresh file. One
/// SQLite file serves every tenant, so both stores of a case are one store.
struct Files;

#[async_trait::async_trait]
impl IntentStoreFixture for Files {
    async fn stores(&self, case: &'static str) -> CaseStores {
        let store: Arc<dyn IntentStore> =
            Arc::new(SqliteIntentStore::open(database(case)).expect("open"));
        CaseStores {
            store: Arc::clone(&store),
            tenant: "tenant-a".to_owned(),
            other: store,
            other_tenant: "tenant-b".to_owned(),
        }
    }
}

#[tokio::test]
async fn a_finished_key_returns_its_stored_outcome() {
    intent_cases::a_finished_key_returns_its_stored_outcome(&Files).await;
}

#[tokio::test]
async fn a_begun_key_is_uncertain_and_never_new_again() {
    intent_cases::a_begun_key_is_uncertain_and_never_new_again(&Files).await;
}

#[tokio::test]
async fn a_repeated_key_with_another_input_conflicts() {
    intent_cases::a_repeated_key_with_another_input_conflicts(&Files).await;
}

#[tokio::test]
async fn keys_belong_to_their_tenant() {
    intent_cases::keys_belong_to_their_tenant(&Files).await;
}

#[tokio::test]
async fn finish_closes_an_intent_once() {
    intent_cases::finish_closes_an_intent_once(&Files).await;
}

#[tokio::test]
async fn uncertain_lists_open_intents_oldest_first_up_to_the_limit() {
    intent_cases::uncertain_lists_open_intents_oldest_first_up_to_the_limit(&Files).await;
}

#[tokio::test]
async fn a_resolved_intent_leaves_the_list_and_answers_its_basis() {
    intent_cases::a_resolved_intent_leaves_the_list_and_answers_its_basis(&Files).await;
}

#[tokio::test]
async fn a_finished_intent_does_not_resolve() {
    intent_cases::a_finished_intent_does_not_resolve(&Files).await;
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
