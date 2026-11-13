//! Begin an intent and wait for the parent recovery test to kill this process.

use std::io::Write as _;

use wamn_run_state::IntentStore as _;
use wamn_run_state::intent_store::{Begun, Intent};
use wamn_run_state_sqlite::SqliteIntentStore;

fn main() {
    let path = std::env::args_os()
        .nth(1)
        .expect("the parent supplies the database path");
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("build the child runtime")
        .block_on(async {
            let store = SqliteIntentStore::open(path).expect("open");
            let begun = store
                .begin(&Intent {
                    partition: "org-a",
                    release: "release-1",
                    package: "scale",
                    operation: "record_sample",
                    idempotency_key: "k-killed",
                    input_hash: "h1",
                    deadline_ms: 5_000,
                })
                .await
                .expect("begin");
            let Begun::New(id) = begun else {
                panic!("expected a new intent, got {begun:?}");
            };
            println!("begun {}", id.0);
            std::io::stdout()
                .flush()
                .expect("send the intent to the parent");
            std::future::pending::<()>().await;
            drop(store);
        });
}
