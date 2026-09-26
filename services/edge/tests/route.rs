//! The edge serves the route of the `apps/edge_device` bundle through the
//! http-route guest, and logs one intent for each item of a write.
//!
//! The tests need the bundle: `WAMN_EDGE_DEVICE_BUNDLE` names the directory
//! that `wamn dev edge-bundle` wrote (docs/operations/running-tests.md).

mod support;

use std::net::SocketAddr;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use wamn_edge::intents;
use wamn_engine::operation::intent::item_input_hash;
use wamn_run_state::IntentStore as _;
use wamn_run_state::intent_store::{Begun, Intent, IntentId, StoredOutcome};
use wamn_run_state::operator_action::OperatorActionBasis;
use wamn_run_state_sqlite::SqliteIntentStore;

use support::{
    HOST, OPERATION, ORG, PACKAGE, PATH, ROLE, bundle, closed, config, key, release, session, start,
};

/// The capture time of every item.
const CAPTURED_AT: &str = "2026-09-25T12:00:00.000Z";

/// POST `body` to the route and return the status and the response body.
async fn post(addr: SocketAddr, token: Option<&str>, body: &str) -> (u16, String) {
    let authorization = token
        .map(|token| format!("Authorization: Bearer {token}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "POST {PATH} HTTP/1.1\r\nHost: {HOST}\r\n{authorization}Content-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("send the request");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .expect("read the response");
    let response = String::from_utf8(response).expect("the response is UTF-8");
    let (head, body) = response
        .split_once("\r\n\r\n")
        .expect("the response has a head");
    let status = head
        .split(' ')
        .nth(1)
        .and_then(|status| status.parse().ok())
        .expect("the response has a status");
    let chunked = head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked");
    (
        status,
        if chunked {
            dechunk(body)
        } else {
            body.to_owned()
        },
    )
}

fn dechunk(mut body: &str) -> String {
    let mut output = String::new();
    loop {
        let (size, rest) = body.split_once("\r\n").expect("a chunk size line");
        let size = usize::from_str_radix(size.trim(), 16).expect("a hexadecimal chunk size");
        if size == 0 {
            return output;
        }
        output.push_str(&rest[..size]);
        body = &rest[size + 2..];
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: WAMN_EDGE_DEVICE_BUNDLE"]
async fn the_edge_serves_a_route_to_a_session_whose_role_grants_it() {
    let (known, public) = key("key-one", 1);
    let (unknown, _) = key("key-other", 2);
    let (directory, digest) = bundle("route", &public);
    let host = start(config(&directory, digest)).await;
    let addr = host.addr();
    let body = json!([item("r-1", " 12.5 kg ")]).to_string();
    let body = body.as_str();

    let (status, answer) = post(addr, Some(&session(&known, "key-one", &[ROLE])), body).await;
    assert_eq!(status, 200, "{answer}");
    assert_eq!(
        serde_json::from_str::<Value>(&answer).expect("a JSON body"),
        json!([outcome("r-1", "12.5 kg")]),
        "the device component trims the frame"
    );
    assert_eq!(post(addr, None, body).await.0, 401, "no credential");
    assert_eq!(
        post(addr, Some(&session(&unknown, "key-other", &[ROLE])), body)
            .await
            .0,
        401,
        "a key that the file does not hold"
    );
    assert_eq!(
        post(addr, Some(&session(&known, "key-one", &["viewer"])), body)
            .await
            .0,
        403,
        "a role without the permission"
    );
    host.stop().await.expect("the edge stops");
}

/// An item of the route's input, keyed by its request id.
fn item(request_id: &str, frame: &str) -> Value {
    json!({"request_id": request_id, "value": {"frame": frame, "captured_at": CAPTURED_AT}})
}

/// The answer of the device component to one item: the trimmed frame.
fn outcome(request_id: &str, frame: &str) -> Value {
    item(request_id, frame)
}

/// Begin the intent of `item` as the edge would, for a store seeded before the
/// edge starts.
async fn begin(store: &SqliteIntentStore, release: &str, item: &Value) -> IntentId {
    let begun = store
        .begin(&Intent {
            tenant: ORG,
            release,
            package: PACKAGE,
            operation: OPERATION,
            idempotency_key: item["request_id"].as_str().expect("a request id"),
            input_hash: &item_input_hash(item),
            deadline_ms: 1_000,
        })
        .await
        .expect("begin");
    let Begun::New(id) = begun else {
        panic!("expected a new intent, got {begun:?}");
    };
    id
}

/// A command route logs one intent for each item. A finished key answers its
/// stored outcome without running, an unfinished key answers intent-uncertain,
/// a resolved key answers intent-resolved with its basis, and only the new item
/// runs. The operator lists and resolves the uncertain intent with the edge
/// stopped.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: WAMN_EDGE_DEVICE_BUNDLE"]
async fn a_write_logs_one_intent_per_item_and_runs_only_new_items() {
    let (pair, public) = key("key-one", 1);
    let (directory, digest) = bundle("intents", &public);
    let db = directory.join("edge.db");
    let release = release(&directory);
    let (stored, open, resolved) = (
        item("r-stored", "1 kg"),
        item("r-open", "2 kg"),
        item("r-resolved", "3 kg"),
    );
    let open_id = {
        let store = SqliteIntentStore::open(&db).expect("open the store before the edge");
        let id = begin(&store, &release, &stored).await;
        // The component would answer "1 kg", so this answer shows no run.
        store
            .finish(
                &id,
                &StoredOutcome::Completed(json!({"value": {"frame": "stored"}})),
            )
            .await
            .expect("finish");
        let open_id = begin(&store, &release, &open).await;
        let id = begin(&store, &release, &resolved).await;
        store
            .resolve(&id, OperatorActionBasis::OperatorJudgment)
            .await
            .expect("resolve");
        open_id
    };

    let host = start(config(&directory, digest)).await;
    let addr = host.addr();
    let token = session(&pair, "key-one", &[ROLE]);
    let batch = json!([item("r-new", " 4 kg"), stored, open, resolved]).to_string();
    let (status, answer) = post(addr, Some(&token), &batch).await;
    assert_eq!(status, 200, "{answer}");
    let answer: Value = serde_json::from_str(&answer).expect("a JSON body");
    assert_eq!(answer[0], outcome("r-new", "4 kg"), "the new item runs");
    assert_eq!(
        answer[1],
        json!({"request_id": "r-stored", "value": {"frame": "stored"}}),
        "a finished key answers its stored outcome"
    );
    assert_eq!(answer[2]["error"]["code"], "intent-uncertain");
    assert_eq!(answer[2]["error"]["detail"]["intent"], open_id.0.as_str());
    assert_eq!(answer[3]["error"]["code"], "intent-resolved");
    assert_eq!(answer[3]["error"]["detail"]["basis"], "operator-judgment");

    let replay = json!([item("r-new", " 4 kg")]).to_string();
    let (status, again) = post(addr, Some(&token), &replay).await;
    assert_eq!(status, 200, "{again}");
    assert_eq!(
        serde_json::from_str::<Value>(&again).expect("a JSON body"),
        json!([outcome("r-new", "4 kg")])
    );
    let changed = json!([item("r-new", "5 kg")]).to_string();
    let (status, conflict) = post(addr, Some(&token), &changed).await;
    assert_eq!(status, 200, "{conflict}");
    let conflict: Value = serde_json::from_str(&conflict).expect("a JSON body");
    assert_eq!(conflict[0]["error"]["code"], "idempotency_conflict");
    host.stop().await.expect("the edge stops");
    closed(&db).await;

    let command = |args: &[&str]| args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
    let listed = intents::run(&command(&["list"]), &db)
        .await
        .expect("list with the edge stopped");
    assert_eq!(
        listed
            .lines()
            .map(|line| line.split('\t').next())
            .collect::<Vec<_>>(),
        [Some(open_id.0.as_str())],
        "only the unfinished intent is uncertain: {listed}"
    );
    intents::run(&command(&["resolve", &open_id.0, "external-evidence"]), &db)
        .await
        .expect("resolve with the edge stopped");
    assert_eq!(
        intents::run(&command(&["list"]), &db).await.expect("list"),
        ""
    );
}
