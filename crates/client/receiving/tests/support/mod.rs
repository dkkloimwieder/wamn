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
        "status": "open", "row_version": "4",
        "supplier_id": "aaaaaaaa-0000-0000-0000-000000000001",
        "created_at": "2026-09-03T00:00:00Z", "updated_at": "2026-09-03T00:00:00Z"
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
    let request_id = request.body.item()["request_id"].clone();
    app.resolve(
        request.screen,
        request.attempt,
        Ok(HttpResponse {
            status: 200,
            body: json!([{"request_id": request_id, "value": value}]).to_string(),
        }),
    );
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "The simulated transport consumes each prepared request and response."
)]
pub fn refuse(app: &mut ReceivingApplication, request: PreparedRequest, error: Value) {
    let request_id = request.body.item()["request_id"].clone();
    app.resolve(
        request.screen,
        request.attempt,
        Ok(HttpResponse {
            status: 200,
            body: json!([{"request_id": request_id, "error": error}]).to_string(),
        }),
    );
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
