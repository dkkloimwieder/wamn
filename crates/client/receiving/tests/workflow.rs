//! ONE terminal-level smoke proof of the receipt-entry workflow.
//!
//! The acceptance puts every other assertion at the reducer layer and asks for
//! a single proof at this one. This drives the whole path — a fake transport
//! answering the real published routes, the real client, the real reducer, the
//! real screens — and reads the rendered cells back at each step.
//!
//! It renders into a `Buffer` rather than a terminal: a TTY would add a
//! dependency on the machine running the test and prove nothing the buffer
//! does not already show.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;
use wamn_client::{
    ClientError, CredentialProvider, HttpRequest, HttpResponse, RouteMetadata, StaticPat,
    Transport, WamnClient,
};
use wamn_receiving_tui::model::{Location, PurchaseOrderRow, ReceiptLine};
use wamn_receiving_tui::request::{ClientSupplied, record_receipt};
use wamn_receiving_tui::screen::AppScreen;
use wamn_receiving_tui::{AppState, Event, reduce};

/// Answers the published routes with canned bodies, and records what was sent.
#[derive(Debug)]
struct Deployment {
    responses: Mutex<BTreeMap<String, (u16, String)>>,
    sent: Mutex<Vec<HttpRequest>>,
}

impl Deployment {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(BTreeMap::new()),
            sent: Mutex::new(Vec::new()),
        })
    }

    fn answer(&self, path: &str, status: u16, body: &str) {
        self.responses
            .lock()
            .expect("lock")
            .insert(path.to_owned(), (status, body.to_owned()));
    }

    fn last(&self) -> HttpRequest {
        self.sent
            .lock()
            .expect("lock")
            .last()
            .cloned()
            .expect("a request")
    }
}

#[async_trait::async_trait]
impl Transport for Deployment {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, ClientError> {
        self.sent.lock().expect("lock").push(request.clone());
        let path = request
            .url
            .rsplit_once("svc")
            .map_or(request.url.clone(), |(_, tail)| tail.to_owned());
        let (status, body) = self
            .responses
            .lock()
            .expect("lock")
            .get(&path)
            .cloned()
            .unwrap_or_else(|| panic!("the deployment publishes no route at {path}"));
        Ok(HttpResponse { status, body })
    }
}

/// The routes exactly as `packages/receiving/publication/attachments.json`
/// declares them. Written out rather than derived, so the test states the
/// contract it depends on instead of agreeing with whatever it reads.
fn route(template: &str) -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: template.to_owned(),
    }
}

fn render(state: &AppState) -> Vec<String> {
    let area = Rect::new(0, 0, 90, 12);
    let mut buffer = Buffer::empty(area);
    AppScreen::new(state).render(area, &mut buffer);
    (0..area.height)
        .map(|row| {
            (0..area.width)
                .map(|column| buffer[(column, row)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

fn screen_contains(state: &AppState, needle: &str) -> bool {
    render(state).iter().any(|line| line.contains(needle))
}

#[tokio::test]
async fn the_receipt_entry_workflow_runs_end_to_end() {
    let deployment = Deployment::new();
    deployment.answer(
        "/purchase_order/query",
        200,
        r#"[{"request_id":"r1","value":{"rows":[
             {"id":"00000000-0000-0000-0000-000000000001","purchase_order_number":"PO-1001",
              "status":"open","row_version":4}],"next":"cursor-2"}}]"#,
    );
    deployment.answer(
        "/receiving/load_receipt_screen",
        200,
        r#"[{"request_id":"r2","value":{"rows":[
             {"purchase_order_id":"00000000-0000-0000-0000-000000000001",
              "purchase_order_number":"PO-1001","purchase_order_status":"open","row_version":4,
              "supplier_id":"aaaaaaaa-0000-0000-0000-000000000001",
              "line_id":"11111111-0000-0000-0000-000000000001","line_number":1,
              "item_id":"bbbbbbbb-0000-0000-0000-000000000001","item_number":"WIDGET-9",
              "ordered_quantity":"10","received_quantity":"0","remaining_quantity":"10"}]}}]"#,
    );
    deployment.answer(
        "/location/list",
        200,
        r#"[{"request_id":"r3","value":{"rows":[
             {"id":"22222222-0000-0000-0000-000000000001","location_code":"DOCK-A"}]}}]"#,
    );
    deployment.answer(
        "/receiving/record_receipt",
        200,
        r#"[{"request_id":"r4","value":{"receipt_id":"33333333-0000-0000-0000-000000000009",
             "purchase_order_id":"00000000-0000-0000-0000-000000000001",
             "purchase_order_status":"open","row_version":5}}]"#,
    );

    let client = WamnClient::new(
        "http://flow-http.wamn-system.svc",
        Some("receiving.localhost".to_owned()),
        Arc::new(StaticPat::new("pat-operator").expect("token")) as Arc<dyn CredentialProvider>,
        Arc::clone(&deployment) as Arc<dyn Transport>,
    );

    // ---- 1. the list screen loads over the published route ----
    let mut state = reduce(
        &AppState::default(),
        Event::Sent {
            operation: "purchase_order.query".to_owned(),
        },
    );
    assert!(
        screen_contains(&state, "purchase_order.query"),
        "no pending line"
    );

    let outcomes = client
        .invoke(
            &route("/purchase_order/query"),
            &BTreeMap::new(),
            &[serde_json::json!({"request_id": "r1"})],
        )
        .await
        .expect("the list route answers");
    let page = outcomes[0].clone().into_result().expect("a page");
    state = reduce(
        &state,
        Event::OrdersLoaded {
            rows: page["rows"]
                .as_array()
                .expect("rows")
                .iter()
                .map(|row| PurchaseOrderRow {
                    id: row["id"].as_str().expect("id").to_owned(),
                    number: row["purchase_order_number"]
                        .as_str()
                        .expect("number")
                        .to_owned(),
                    status: row["status"].as_str().expect("status").to_owned(),
                    row_version: row["row_version"].as_i64().expect("row_version"),
                })
                .collect(),
            next: page["next"].as_str().map(str::to_owned),
        },
    );
    assert!(screen_contains(&state, "PO-1001"), "{:?}", render(&state));
    assert!(
        screen_contains(&state, "more available"),
        "{:?}",
        render(&state)
    );

    // ---- 2. open receipt entry and load the screen's projection ----
    state = reduce(&state, Event::OpenReceipt);
    let outcomes = client
        .invoke(
            &route("/receiving/load_receipt_screen"),
            &BTreeMap::new(),
            &[serde_json::json!({
                "request_id": "r2",
                "purchase_order_id": "00000000-0000-0000-0000-000000000001"
            })],
        )
        .await
        .expect("the receipt screen route answers");
    let screen = outcomes[0].clone().into_result().expect("rows");
    state = reduce(
        &state,
        Event::ReceiptLoaded {
            lines: screen["rows"]
                .as_array()
                .expect("rows")
                .iter()
                .filter(|row| !row["line_id"].is_null())
                .map(|row| ReceiptLine {
                    purchase_order_line_id: row["line_id"].as_str().expect("line").to_owned(),
                    line_number: row["line_number"].as_i64().expect("number"),
                    item_number: row["item_number"].as_str().expect("item").to_owned(),
                    ordered: row["ordered_quantity"]
                        .as_str()
                        .expect("ordered")
                        .to_owned(),
                    received: row["received_quantity"]
                        .as_str()
                        .expect("received")
                        .to_owned(),
                    remaining: row["remaining_quantity"]
                        .as_str()
                        .expect("remaining")
                        .to_owned(),
                    entered: String::new(),
                })
                .collect(),
        },
    );
    // The projection's line and its joined item name are on screen — the two
    // facts the base package could not supply before this slice.
    assert!(screen_contains(&state, "WIDGET-9"), "{:?}", render(&state));

    // ---- 3. the locations the operator may receive into ----
    let outcomes = client
        .invoke(
            &route("/location/list"),
            &BTreeMap::new(),
            &[serde_json::json!({"request_id": "r3"})],
        )
        .await
        .expect("the location route answers");
    let listed = outcomes[0].clone().into_result().expect("rows");
    state = reduce(
        &state,
        Event::LocationsLoaded {
            locations: listed["rows"]
                .as_array()
                .expect("rows")
                .iter()
                .map(|row| Location {
                    id: row["id"].as_str().expect("id").to_owned(),
                    code: row["location_code"].as_str().expect("code").to_owned(),
                })
                .collect(),
        },
    );

    // ---- 4. the operator enters a quantity and a reference ----
    for event in [
        Event::TypeQuantity('4'),
        Event::TypeReference('G'),
        Event::TypeReference('R'),
        Event::TypeReference('N'),
        Event::TypeReference('7'),
    ] {
        state = reduce(&state, event);
    }
    assert!(screen_contains(&state, "GRN7"), "{:?}", render(&state));
    assert!(screen_contains(&state, "DOCK-A"), "{:?}", render(&state));

    // ---- 5. send, and check what actually went on the wire ----
    let items = record_receipt(
        &state,
        &ClientSupplied {
            request_id: "r4".to_owned(),
            idempotency_key: "idem-workflow-1".to_owned(),
            occurred_at: "2026-09-03T10:15:00Z".to_owned(),
        },
    )
    .expect("the receipt is complete");
    let outcomes = client
        .invoke(
            &route("/receiving/record_receipt"),
            &BTreeMap::new(),
            &items,
        )
        .await
        .expect("the command route answers");

    let sent: serde_json::Value =
        serde_json::from_slice(&deployment.last().body).expect("the sent body is JSON");
    assert_eq!(sent[0]["value"]["idempotency_key"], "idem-workflow-1");
    assert_eq!(sent[0]["value"]["occurred_at"], "2026-09-03T10:15:00Z");
    assert_eq!(
        sent[0]["value"]["line"][0]["purchase_order_line_id"],
        "11111111-0000-0000-0000-000000000001"
    );
    assert_eq!(
        sent[0]["value"]["line"][0]["location_id"],
        "22222222-0000-0000-0000-000000000001"
    );
    assert_eq!(sent[0]["value"]["line"][0]["quantity"], 4);
    assert_eq!(
        deployment.last().headers["authorization"],
        "Bearer pat-operator"
    );

    // ---- 6. the recorded receipt returns the operator to the list ----
    let recorded = outcomes[0].clone().into_result().expect("a receipt");
    state = reduce(
        &state,
        Event::ReceiptRecorded {
            receipt_id: recorded["receipt_id"].as_str().expect("id").to_owned(),
        },
    );
    assert!(screen_contains(&state, "33333333"), "{:?}", render(&state));
    assert!(screen_contains(&state, "PO-1001"), "back on the list");
}

/// A stale write is rendered with BOTH revisions, per the detail matrix, and
/// the operator's entry survives to be corrected.
#[tokio::test]
async fn a_stale_write_reaches_the_screen_with_both_revisions() {
    let deployment = Deployment::new();
    deployment.answer(
        "/receiving/record_receipt",
        200,
        r#"[{"request_id":"r1","error":{"code":"concurrency_conflict",
             "detail":{"expected_row_version":4,"observed_row_version":7}}}]"#,
    );
    let client = WamnClient::new(
        "http://flow-http.wamn-system.svc",
        None,
        Arc::new(StaticPat::new("pat-operator").expect("token")) as Arc<dyn CredentialProvider>,
        Arc::clone(&deployment) as Arc<dyn Transport>,
    );

    let mut state = AppState::default();
    for event in [
        Event::OrdersLoaded {
            rows: vec![PurchaseOrderRow {
                id: "00000000-0000-0000-0000-000000000001".to_owned(),
                number: "PO-1001".to_owned(),
                status: "open".to_owned(),
                row_version: 4,
            }],
            next: None,
        },
        Event::OpenReceipt,
        Event::ReceiptLoaded {
            lines: vec![ReceiptLine {
                purchase_order_line_id: "11111111-0000-0000-0000-000000000001".to_owned(),
                line_number: 1,
                item_number: "WIDGET-9".to_owned(),
                ordered: "10".to_owned(),
                received: "0".to_owned(),
                remaining: "10".to_owned(),
                entered: String::new(),
            }],
        },
        Event::LocationsLoaded {
            locations: vec![Location {
                id: "22222222-0000-0000-0000-000000000001".to_owned(),
                code: "DOCK-A".to_owned(),
            }],
        },
        Event::TypeQuantity('4'),
        Event::TypeReference('G'),
    ] {
        state = reduce(&state, event);
    }

    let items = record_receipt(
        &state,
        &ClientSupplied {
            request_id: "r1".to_owned(),
            idempotency_key: "idem-1".to_owned(),
            occurred_at: "2026-09-03T10:15:00Z".to_owned(),
        },
    )
    .expect("complete");
    let outcomes = client
        .invoke(
            &route("/receiving/record_receipt"),
            &BTreeMap::new(),
            &items,
        )
        .await
        .expect("the envelope itself succeeded");
    let error = outcomes[0].clone().into_result().expect_err("a refusal");
    state = reduce(&state, Event::Failed { error });

    let rendered = render(&state).join("\n");
    assert!(
        rendered.contains('4') && rendered.contains('7'),
        "{rendered}"
    );
    assert!(
        rendered.contains("revision"),
        "the conflict was not stated in words: {rendered}"
    );
    // The entry survives so the operator can correct it rather than retype it.
    assert_eq!(state.lines[0].entered, "4");
    assert_eq!(state.receipt_reference, "G");
}
