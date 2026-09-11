//! The former reducer obligations now drive the generated-screen composition.

mod support;

use crossterm::event::KeyCode;
use serde_json::json;
use wamn_client::ClientError;
use wamn_client_terminal::operator::{Action, Application};
use wamn_client_tui::submission::State;
use wamn_receiving_tui::Panel;

use support::*;

#[test]
fn a_loaded_page_highlights_its_first_row() {
    let mut app = application();
    read(&mut app, "orders", orders(&[1, 2], None));
    assert_eq!(app.selected_row(Panel::Orders), 0);
    assert_eq!(app.screen(Panel::Orders).rows().len(), 2);
    assert!(render(&app).contains("PO-1"));
}

#[test]
fn a_second_page_appends_rather_than_replaces() {
    let mut app = application();
    read(&mut app, "first", orders(&[1], Some("opaque +/=% cursor")));
    let action = key(&mut app, KeyCode::F(8));
    let request = app
        .prepare(action, Some(&intent("second")))
        .expect("next page");
    assert_eq!(request.body.item()["cursor"], "opaque +/=% cursor");
    reply(&mut app, request, orders(&[2], None));
    assert_eq!(app.screen(Panel::Orders).rows().len(), 2);
    assert_eq!(
        app.screen(Panel::Orders).rows()[0]["purchase_order_number"],
        "PO-1"
    );
    assert!(app.screen(Panel::Orders).cursor().is_none());
}

#[test]
fn the_highlight_saturates_rather_than_wrapping() {
    let mut app = application();
    read(&mut app, "orders", orders(&[1, 2], None));
    key(&mut app, KeyCode::Down);
    assert_eq!(app.selected_row(Panel::Orders), 1);
    key(&mut app, KeyCode::Down);
    assert_eq!(app.selected_row(Panel::Orders), 1);
    key(&mut app, KeyCode::Up);
    key(&mut app, KeyCode::Up);
    assert_eq!(app.selected_row(Panel::Orders), 0);
}

#[test]
fn opening_a_receipt_clears_the_previous_entry() {
    let mut app = ready();
    key(&mut app, KeyCode::Esc);
    assert!(render(&app).contains("Discard draft"));
    key(&mut app, KeyCode::Char('y'));
    assert_eq!(app.panel(), Panel::Orders);
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.panel(), Panel::Lines);
    let draft = app.screen(Panel::Receipt).draft().item();
    assert!(draft.pointer("/value/receipt_reference").is_none());
    assert!(draft.pointer("/value/line").is_none());
    assert_eq!(
        draft["value"]["purchase_order_id"],
        "00000000-0000-0000-0000-000000000002"
    );
    assert!(app.location().is_none());
}

#[test]
fn only_a_well_formed_quantity_can_be_typed() {
    let mut app = loaded();
    key(&mut app, KeyCode::Enter);
    type_text(&mut app, "3.5.x7");
    assert_eq!(
        app.screen(Panel::Receipt).draft().item()["value"]["line"][0]["quantity"],
        "3.57"
    );
}

#[test]
fn a_blank_line_is_not_submitted_as_zero() {
    let mut app = ready();
    key(&mut app, KeyCode::F(2));
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Enter);
    let action = submit(&mut app);
    let request = app
        .prepare(action, Some(&intent("receipt")))
        .expect("one entered line");
    assert_eq!(
        request.body.item()["value"]["line"]
            .as_array()
            .expect("line")
            .len(),
        1
    );
    assert_eq!(
        request.body.item()["value"]["line"][0]["purchase_order_line_id"],
        LINE
    );
}

#[test]
fn an_incomplete_receipt_names_what_is_missing() {
    let mut app = application();
    read(&mut app, "orders", orders(&[1], None));
    key(&mut app, KeyCode::Enter);
    read(&mut app, "projection", projection());
    read(&mut app, "locations", json!({"rows": []}));
    let action = submit(&mut app);
    assert_eq!(
        app.prepare(action, Some(&intent("receipt")))
            .expect_err("reference")
            .as_str(),
        "A receipt reference is required."
    );
    key(&mut app, KeyCode::F(3));
    type_text(&mut app, "R1");
    let action = submit(&mut app);
    assert_eq!(
        app.prepare(action, Some(&intent("receipt")))
            .expect_err("location")
            .as_str(),
        "A location is required."
    );
    let mut app = loaded();
    key(&mut app, KeyCode::F(3));
    type_text(&mut app, "R1");
    let action = submit(&mut app);
    assert_eq!(
        app.prepare(action, Some(&intent("receipt")))
            .expect_err("quantity")
            .as_str(),
        "Enter a quantity on at least one line."
    );
}

#[test]
fn cycling_locations_wraps_through_the_set() {
    let mut app = ready();
    assert_eq!(app.location().expect("location")["location_code"], "DOCK-A");
    key(&mut app, KeyCode::Char('l'));
    assert_eq!(app.location().expect("location")["location_code"], "DOCK-B");
    key(&mut app, KeyCode::Char('l'));
    assert_eq!(app.location().expect("location")["location_code"], "DOCK-A");
}

#[test]
fn a_refusal_keeps_what_the_operator_typed() {
    let mut app = ready();
    let action = submit(&mut app);
    let request = app
        .prepare(action, Some(&intent("receipt")))
        .expect("receipt");
    refuse(
        &mut app,
        request,
        json!({"code":"quantity_exceeds_remaining", "detail":{"field":"quantity", "id":LINE}}),
    );
    assert!(matches!(
        app.screen(Panel::Receipt).submission().state(),
        State::Refused(_)
    ));
    assert!(!app.pending());
    assert_eq!(
        app.screen(Panel::Receipt).draft().item()["value"]["line"][0]["quantity"],
        "3"
    );
    assert_eq!(
        app.screen(Panel::Receipt).draft().item()["value"]["receipt_reference"],
        "R1"
    );
    assert!(render(&app).contains("quantity_exceeds_remaining"));
}

#[test]
fn a_recorded_receipt_clears_the_entry_and_returns_to_the_list() {
    let mut app = ready();
    let action = submit(&mut app);
    let request = app
        .prepare(action, Some(&intent("receipt")))
        .expect("receipt");
    reply(&mut app, request, recorded());
    assert_eq!(app.panel(), Panel::Orders);
    assert!(matches!(
        app.screen(Panel::Receipt).submission().state(),
        State::Succeeded { .. }
    ));
    assert!(render(&app).contains(RECEIPT));
    assert!(render(&app).contains("PO-1"));
    assert!(
        matches!(app.next_action(), Action::None),
        "success must not schedule another command"
    );
    key(&mut app, KeyCode::Enter);
    assert!(
        app.screen(Panel::Receipt)
            .draft()
            .item()
            .pointer("/value/line")
            .is_none()
    );
    assert!(
        app.screen(Panel::Receipt)
            .draft()
            .item()
            .pointer("/value/receipt_reference")
            .is_none()
    );
}

#[test]
fn sending_clears_the_previous_verdict() {
    let mut app = ready();
    let action = submit(&mut app);
    let request = app
        .prepare(action, Some(&intent("receipt")))
        .expect("receipt");
    refuse(
        &mut app,
        request,
        json!({"code":"receipt_reference_conflict", "detail":{"constraint":"receipt_reference"}}),
    );
    let action = submit(&mut app);
    app.prepare(action, Some(&intent("new-intent")))
        .expect("confirmed refusal is editable");
    assert_eq!(
        app.screen(Panel::Receipt).submission().state(),
        &State::Pending
    );
    assert!(!render(&app).contains("receipt_reference_conflict"));
}

#[test]
fn the_envelope_carries_what_the_client_supplies_and_what_was_entered() {
    let mut app = ready();
    let action = submit(&mut app);
    let supplied = intent("receipt");
    let request = app.prepare(action, Some(&supplied)).expect("receipt");
    let bytes: serde_json::Value =
        serde_json::from_slice(request.body.body()).expect("canonical JSON");
    assert_eq!(bytes.as_array().expect("envelope").len(), 1);
    assert_eq!(bytes[0]["request_id"], "receipt");
    assert_eq!(bytes[0]["value"]["idempotency_key"], "idem-receipt");
    assert_eq!(
        bytes[0]["value"]["occurred_at"],
        "2026-09-03T10:15:00.000000Z"
    );
    assert_eq!(bytes[0]["value"]["receipt_reference"], "R1");
    assert_eq!(bytes[0]["value"]["line"][0]["location_id"], LOCATION);
    assert_eq!(bytes[0]["value"]["line"][0]["purchase_order_line_id"], LINE);
}

#[test]
fn a_quantity_is_sent_as_a_string_carrying_the_typed_digits() {
    let mut app = ready();
    key(&mut app, KeyCode::F(2));
    key(&mut app, KeyCode::Enter);
    type_text(&mut app, ".5");
    let action = submit(&mut app);
    let request = app
        .prepare(action, Some(&intent("receipt")))
        .expect("receipt");
    assert_eq!(request.body.item()["value"]["line"][0]["quantity"], "3.5");
}

#[test]
fn a_typed_scale_survives_onto_the_wire() {
    let mut app = ready();
    key(&mut app, KeyCode::F(2));
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Backspace);
    type_text(&mut app, "5.0000");
    let action = submit(&mut app);
    let request = app
        .prepare(action, Some(&intent("receipt")))
        .expect("receipt");
    assert_eq!(
        request.body.item()["value"]["line"][0]["quantity"],
        "5.0000"
    );
}

#[test]
fn a_retry_of_one_action_reuses_its_idempotency_key() {
    let mut app = ready();
    let action = submit(&mut app);
    let request = app
        .prepare(action, Some(&intent("receipt")))
        .expect("receipt");
    let original = request.body.body().to_vec();
    app.resolve(
        request.screen,
        request.attempt,
        Err(ClientError::Transport {
            detail: "response lost".into(),
        }),
    );
    assert!(matches!(
        app.screen(Panel::Receipt).submission().state(),
        State::Uncertain { .. }
    ));
    assert!(
        matches!(app.next_action(), Action::None),
        "retry is an operator act"
    );
    let action = key(&mut app, KeyCode::F(7));
    let retry = app
        .prepare(action, None)
        .expect("claim-backed captured retry");
    assert_eq!(retry.body.body(), original);
}

#[test]
fn double_submit_and_editing_are_blocked_while_pending() {
    let mut app = ready();
    let action = submit(&mut app);
    let request = app
        .prepare(action, Some(&intent("receipt")))
        .expect("receipt");
    assert!(matches!(submit(&mut app), Action::None));
    let duplicate = Action::Send {
        screen: request.screen,
        retry: false,
        delete_confirmed: false,
    };
    assert!(app.prepare(duplicate, Some(&intent("duplicate"))).is_err());
    assert!(matches!(key(&mut app, KeyCode::Char('q')), Action::None));
    key(&mut app, KeyCode::F(3));
    assert_eq!(
        app.screen(Panel::Receipt).draft().item()["value"]["receipt_reference"],
        "R1"
    );
}

#[test]
fn a_projection_for_another_order_cannot_populate_the_receipt() {
    let mut app = application();
    read(&mut app, "orders", orders(&[1], None));
    key(&mut app, KeyCode::Enter);
    let mut wrong = projection();
    wrong["rows"][0]["purchase_order_id"] = json!("00000000-0000-0000-0000-000000000002");
    read(&mut app, "projection", wrong);
    assert!(matches!(app.next_action(), Action::None));
    key(&mut app, KeyCode::Enter);
    assert!(
        app.screen(Panel::Receipt)
            .draft()
            .item()
            .pointer("/value/line")
            .is_none()
    );
}
