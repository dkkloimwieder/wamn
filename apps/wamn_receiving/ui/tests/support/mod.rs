//! Shared fixtures drive the actual generated application through its public loop API.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use serde_json::{Value, json};
use wamn_client::HttpResponse;
use wamn_client_terminal::operator::{Action, Application, PreparedRequest};
use wamn_client_tui::screen::IntentValues;
use wamn_client_tui::submission::SessionBinding;
use wamn_receiving_tui::ReceivingApplication;

pub const ORDER: &str = "00000000-0000-0000-0000-000000000001";
pub const LINE: &str = "11111111-0000-0000-0000-000000000001";
pub const LOCATION: &str = "22222222-0000-0000-0000-000000000001";
pub const RECEIPT: &str = "33333333-0000-0000-0000-000000000009";

pub fn application() -> ReceivingApplication {
    ReceivingApplication::new(
        "Receiving",
        SessionBinding {
            url: "http://receiving.test".into(),
            host: Some("receiving.localhost".into()),
            target_instance: "activation-a".into(),
        },
    )
}

pub fn acme_application() -> ReceivingApplication {
    ReceivingApplication::acme(
        "Acme Receiving",
        SessionBinding {
            url: "http://receiving.test".into(),
            host: Some("receiving.localhost".into()),
            target_instance: "activation-acme".into(),
        },
    )
}

pub fn intent(id: &str) -> IntentValues {
    IntentValues {
        request_id: id.into(),
        idempotency_key: format!("idem-{id}"),
        occurred_at: "2026-09-03T10:15:00+00:00".into(),
    }
}

pub fn key(app: &mut ReceivingApplication, code: KeyCode) -> Action {
    app.key(KeyEvent::new(code, KeyModifiers::NONE))
}

pub fn submit(app: &mut ReceivingApplication) -> Action {
    app.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))
}

pub fn type_text(app: &mut ReceivingApplication, value: &str) {
    for character in value.chars() {
        key(app, KeyCode::Char(character));
    }
    key(app, KeyCode::Enter);
}

pub fn orders(numbers: &[u8], cursor: Option<&str>) -> Value {
    json!({"item": numbers.iter().map(|number| json!({
        "id": format!("00000000-0000-0000-0000-{number:012}"),
        "purchase_order_number": format!("PO-{number}"),
        "status": "open", "row_version": 4,
        "supplier_id": "aaaaaaaa-0000-0000-0000-000000000001",
        "created_at": "2026-09-03T00:00:00Z", "updated_at": "2026-09-03T00:00:00Z",
        "created_by": "cccccccc-0000-0000-0000-000000000001",
        "updated_by": "cccccccc-0000-0000-0000-000000000001"
    })).collect::<Vec<_>>(), "next_cursor": cursor})
}

pub fn projection() -> Value {
    json!({"rows": ([1, 2].map(|number| json!({
        "purchase_order_id": ORDER, "purchase_order_number": "PO-1",
        "purchase_order_status": "open", "row_version": 4,
        "supplier_id": "aaaaaaaa-0000-0000-0000-000000000001",
        "line_id": format!("11111111-0000-0000-0000-{number:012}"), "line_number": number,
        "item_id": "bbbbbbbb-0000-0000-0000-000000000001", "item_number": format!("WIDGET-{number}"),
        "ordered_quantity": "10", "received_quantity": "0", "remaining_quantity": "10"
    })))})
}

pub fn locations() -> Value {
    json!({"rows": [
        {"id": LOCATION, "location_code": "DOCK-A"},
        {"id": "22222222-0000-0000-0000-000000000002", "location_code": "DOCK-B"}
    ]})
}

pub fn recorded() -> Value {
    json!({"receipt_id": RECEIPT, "purchase_order_id": ORDER,
        "purchase_order_status": "open", "row_version": 5})
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "The simulated transport consumes each prepared request and response."
)]
pub fn reply(app: &mut ReceivingApplication, request: PreparedRequest, value: Value) {
    let body = outcome(&request, "value", value);
    app.resolve(
        request.screen,
        request.attempt,
        Ok(HttpResponse {
            actor_labels: std::collections::BTreeMap::new(),
            status: 200,
            body,
        }),
    );
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "The simulated transport consumes each prepared request and response."
)]
pub fn refuse(app: &mut ReceivingApplication, request: PreparedRequest, error: Value) {
    let body = outcome(&request, "error", error);
    app.resolve(
        request.screen,
        request.attempt,
        Ok(HttpResponse {
            actor_labels: std::collections::BTreeMap::new(),
            status: 200,
            body,
        }),
    );
}

/// The one outcome the release returns: a write echoes its `request_id`, and
/// a read carries none, because its outcome matches its item by position.
fn outcome(request: &PreparedRequest, member: &str, value: Value) -> String {
    let mut outcome = serde_json::Map::from_iter([(member.to_owned(), value)]);
    if let Some(request_id) = request.body.item().get("request_id") {
        outcome.insert("request_id".to_owned(), request_id.clone());
    }
    json!([outcome]).to_string()
}

pub fn read(app: &mut ReceivingApplication, id: &str, value: Value) {
    let action = app.next_action();
    assert!(
        matches!(action, Action::Send { .. }),
        "expected queued read"
    );
    let request = app.prepare(action, Some(&intent(id))).expect("valid read");
    reply(app, request, value);
}

pub fn loaded() -> ReceivingApplication {
    let mut app = application();
    read(&mut app, "orders", orders(&[1, 2], None));
    key(&mut app, KeyCode::Enter);
    read(&mut app, "projection", projection());
    read(&mut app, "locations", locations());
    app
}

pub fn ready() -> ReceivingApplication {
    let mut app = loaded();
    key(&mut app, KeyCode::Enter);
    type_text(&mut app, "3");
    key(&mut app, KeyCode::F(3));
    type_text(&mut app, "R1");
    app
}

pub fn acme_ready() -> ReceivingApplication {
    let mut app = acme_application();
    read(&mut app, "orders", orders(&[1, 2], None));
    key(&mut app, KeyCode::Enter);
    read(&mut app, "projection", projection());
    read(&mut app, "locations", locations());
    key(&mut app, KeyCode::Enter);
    type_text(&mut app, "3");
    key(&mut app, KeyCode::F(3));
    type_text(&mut app, "R1");
    app
}

pub fn render(app: &ReceivingApplication) -> String {
    let area = Rect::new(0, 0, 260, 42);
    let mut buffer = Buffer::empty(area);
    app.render(area, &mut buffer);
    (0..area.height)
        .map(|row| {
            (0..area.width)
                .map(|column| buffer[(column, row)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}
