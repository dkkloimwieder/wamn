//! The composed application uses the real client and captured request bytes.

#[expect(
    dead_code,
    reason = "The parity suite shares fixtures with these transport tests."
)]
mod support;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crossterm::event::KeyCode;
use serde_json::{Value, json};
use wamn_client::{ClientError, HttpRequest, HttpResponse, StaticPat, Transport, WamnClient};
use wamn_client_terminal::operator::{Action, Application};
use wamn_client_tui::submission::State;
use wamn_receiving_tui::{Panel, ReceivingApplication};

use support::*;

#[derive(Debug, Default)]
struct Deployment {
    responses: Mutex<BTreeMap<&'static str, (u16, Value)>>,
    sent: Mutex<Vec<HttpRequest>>,
}

impl Deployment {
    fn answer(&self, path: &'static str, status: u16, payload: Value) {
        self.responses
            .lock()
            .expect("responses")
            .insert(path, (status, payload));
    }

    fn client(self: &Arc<Self>) -> WamnClient {
        WamnClient::new(
            "http://receiving.test",
            Some("receiving.localhost".into()),
            Arc::new(StaticPat::new("pat-operator").expect("token")),
            Arc::clone(self) as Arc<dyn Transport>,
        )
    }
}

#[async_trait::async_trait]
impl Transport for Deployment {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, ClientError> {
        let path = request
            .url
            .strip_prefix("http://receiving.test")
            .expect("bound target");
        let body: Value = serde_json::from_slice(&request.body).expect("request JSON");
        let (status, mut payload) = self
            .responses
            .lock()
            .expect("responses")
            .get(path)
            .cloned()
            .unwrap_or_else(|| panic!("no fixture response at {path}"));
        if status == 200 {
            payload["request_id"] = body[0]["request_id"].clone();
            payload = json!([payload]);
        }
        self.sent.lock().expect("sent").push(request);
        Ok(HttpResponse {
            status,
            body: payload.to_string(),
        })
    }
}

async fn dispatch(app: &mut ReceivingApplication, client: &WamnClient, action: Action, id: &str) {
    let request = app.prepare(action, Some(&intent(id))).expect("request");
    let response = if request.fresh_only {
        client
            .submit_fresh(&request.route, &request.parameters, &request.body)
            .await
    } else {
        client
            .submit(&request.route, &request.parameters, &request.body)
            .await
    };
    app.resolve(request.screen, request.attempt, response);
}

#[tokio::test]
async fn the_receipt_entry_workflow_runs_end_to_end() {
    let deployment = Arc::new(Deployment::default());
    deployment.answer(
        "/purchase_order/query",
        200,
        json!({"value":orders(&[1, 2], Some("cursor-2"))}),
    );
    deployment.answer(
        "/receiving/load_receipt_screen",
        200,
        json!({"value":projection()}),
    );
    deployment.answer("/location/list", 200, json!({"value":locations()}));
    deployment.answer(
        "/receiving/record_receipt",
        200,
        json!({"value":recorded()}),
    );
    let client = deployment.client();
    let mut app = application();

    let action = app.next_action();
    dispatch(&mut app, &client, action, "orders").await;
    assert!(render(&app).contains("PO-1"));
    assert!(render(&app).contains("next available"));
    key(&mut app, KeyCode::Enter);
    let action = app.next_action();
    dispatch(&mut app, &client, action, "projection").await;
    let action = app.next_action();
    dispatch(&mut app, &client, action, "locations").await;
    assert!(render(&app).contains("WIDGET-1"));

    key(&mut app, KeyCode::Enter);
    type_text(&mut app, "4.0000");
    key(&mut app, KeyCode::F(3));
    type_text(&mut app, "GRN7");
    key(&mut app, KeyCode::F(4));
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);
    assert!(render(&app).contains("DOCK-B"));
    assert!(render(&app).contains("GRN7"));
    let action = submit(&mut app);
    dispatch(&mut app, &client, action, "receipt").await;

    let sent = deployment.sent.lock().expect("sent");
    assert_eq!(
        sent.len(),
        4,
        "the composition issues exactly three reads and one command"
    );
    let command = &sent[3];
    assert_eq!(command.headers["authorization"], "Bearer pat-operator");
    assert_eq!(command.headers["host"], "receiving.localhost");
    assert_eq!(
        command.body,
        br#"[{"request_id":"receipt","value":{"idempotency_key":"idem-receipt","line":[{"location_id":"22222222-0000-0000-0000-000000000002","purchase_order_line_id":"11111111-0000-0000-0000-000000000001","quantity":"4.0000"}],"occurred_at":"2026-09-03T10:15:00.000000Z","purchase_order_id":"00000000-0000-0000-0000-000000000001","receipt_reference":"GRN7"}}]"#,
    );
    assert_eq!(app.panel(), Panel::Orders);
    assert!(render(&app).contains(RECEIPT));
    assert!(matches!(app.next_action(), Action::None));
}

#[tokio::test]
async fn a_nested_fresh_credential_refusal_is_visible_without_a_retry() {
    let deployment = Arc::new(Deployment::default());
    deployment.answer("/receiving/record_receipt", 403,
        json!({"error":{"code":"fresh-credential-required", "operation":"receiving.record_receipt"}}));
    let client = deployment.client();
    let mut app = ready();
    let action = submit(&mut app);
    dispatch(&mut app, &client, action, "receipt").await;
    assert!(!app.pending());
    assert!(
        matches!(
            app.screen(Panel::Receipt).submission().state(),
            State::Uncertain { .. }
        ),
        "the shared layer cannot establish whole-submission refusal from this response"
    );
    assert_eq!(
        app.screen(Panel::Receipt).draft().item()["value"]["receipt_reference"],
        "R1"
    );
    let displayed = render(&app);
    assert!(displayed.contains("fresh-credential-required"));
    assert!(displayed.contains("requires a PAT"));
    assert!(matches!(app.next_action(), Action::None));
    assert_eq!(deployment.sent.lock().expect("sent").len(), 1);
}

#[tokio::test]
async fn an_undeclared_stale_write_reports_both_revisions_without_claiming_refusal() {
    let deployment = Arc::new(Deployment::default());
    deployment.answer("/receiving/record_receipt", 200,
        json!({"error":{"code":"concurrency_conflict", "detail":{"expected_row_version":4,"observed_row_version":7}}}));
    let client = deployment.client();
    let mut app = ready();
    let action = submit(&mut app);
    dispatch(&mut app, &client, action, "receipt").await;
    assert!(
        matches!(
            app.screen(Panel::Receipt).submission().state(),
            State::Uncertain { .. }
        ),
        "record_receipt does not declare concurrency_conflict"
    );
    let displayed = render(&app);
    assert!(displayed.contains("expected_row_version=4"), "{displayed}");
    assert!(displayed.contains("observed_row_version=7"), "{displayed}");
    assert_eq!(
        app.screen(Panel::Receipt).draft().item()["value"]["line"][0]["quantity"],
        "3"
    );
    assert_eq!(
        app.screen(Panel::Receipt).draft().item()["value"]["receipt_reference"],
        "R1"
    );
    assert_eq!(deployment.sent.lock().expect("sent").len(), 1);
}
