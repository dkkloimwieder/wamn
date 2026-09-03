//! The operator screen's behaviour, asserted below the terminal.
//!
//! Every test here states a world and reads a consequence: no transport, no
//! clock, no terminal. This is where the slice puts its assertions, and the
//! rendered smoke proof rides on top of it.

use wamn_client::ClientError;
use wamn_receiving_tui::model::{Location, PurchaseOrderRow, ReceiptLine};
use wamn_receiving_tui::reduce::{Event, entered_lines, submittable};
use wamn_receiving_tui::request::{ClientSupplied, record_receipt};
use wamn_receiving_tui::{AppState, Screen, reduce};

fn order(number: &str, version: i64) -> PurchaseOrderRow {
    PurchaseOrderRow {
        id: format!("00000000-0000-0000-0000-{number:0>12}"),
        number: format!("PO-{number}"),
        status: "open".to_owned(),
        row_version: version,
    }
}

fn line(number: i64, item: &str) -> ReceiptLine {
    ReceiptLine {
        purchase_order_line_id: format!("11111111-0000-0000-0000-{number:0>12}"),
        line_number: number,
        item_number: item.to_owned(),
        ordered: "10".to_owned(),
        received: "0".to_owned(),
        remaining: "10".to_owned(),
        entered: String::new(),
    }
}

fn locations() -> Vec<Location> {
    vec![
        Location {
            id: "22222222-0000-0000-0000-000000000001".to_owned(),
            code: "A-01".to_owned(),
        },
        Location {
            id: "22222222-0000-0000-0000-000000000002".to_owned(),
            code: "B-02".to_owned(),
        },
    ]
}

fn apply(state: &AppState, events: Vec<Event>) -> AppState {
    events
        .into_iter()
        .fold(state.clone(), |state, event| reduce(&state, event))
}

/// A ready-to-send receipt against the first order's first line.
fn ready() -> AppState {
    let state = apply(
        &AppState::default(),
        vec![
            Event::OrdersLoaded {
                rows: vec![order("1", 4), order("2", 1)],
                next: None,
            },
            Event::OpenReceipt,
            Event::ReceiptLoaded {
                lines: vec![line(1, "ITEM-A"), line(2, "ITEM-B")],
            },
            Event::LocationsLoaded {
                locations: locations(),
            },
        ],
    );
    apply(
        &state,
        vec![
            Event::TypeQuantity('3'),
            Event::TypeReference('R'),
            Event::TypeReference('1'),
        ],
    )
}

#[test]
fn a_loaded_page_highlights_its_first_row() {
    let state = reduce(
        &AppState::default(),
        Event::OrdersLoaded {
            rows: vec![order("1", 1), order("2", 1)],
            next: None,
        },
    );
    assert_eq!(state.selected_order, Some(0));
    assert_eq!(state.orders.len(), 2);
}

/// Pages ACCUMULATE. A page that replaced the list would lose every row the
/// operator has already scrolled past — a cursor is a continuation.
#[test]
fn a_second_page_appends_rather_than_replaces() {
    let first = reduce(
        &AppState::default(),
        Event::OrdersLoaded {
            rows: vec![order("1", 1)],
            next: Some("cursor-1".to_owned()),
        },
    );
    assert!(first.next_page.is_some());
    let second = reduce(
        &first,
        Event::OrdersLoaded {
            rows: vec![order("2", 1)],
            next: None,
        },
    );
    assert_eq!(second.orders.len(), 2);
    assert_eq!(second.orders[0].number, "PO-1");
    assert!(
        second.next_page.is_none(),
        "the final page issued no cursor"
    );
}

/// Holding a key must stop at the end of a list, not reappear at the other end.
#[test]
fn the_highlight_saturates_rather_than_wrapping() {
    let state = reduce(
        &AppState::default(),
        Event::OrdersLoaded {
            rows: vec![order("1", 1), order("2", 1)],
            next: None,
        },
    );
    let down = apply(
        &state,
        vec![Event::MoveDown, Event::MoveDown, Event::MoveDown],
    );
    assert_eq!(down.selected_order, Some(1), "ran past the end");
    let up = apply(&down, vec![Event::MoveUp, Event::MoveUp, Event::MoveUp]);
    assert_eq!(up.selected_order, Some(0), "ran past the start");
}

/// Opening a receipt carries nothing over: lines, reference and location all
/// belong to the order they were entered against.
#[test]
fn opening_a_receipt_clears_the_previous_entry() {
    let entered = ready();
    assert!(!entered.receipt_reference.is_empty());

    let reopened = apply(&entered, vec![Event::Back, Event::OpenReceipt]);
    assert_eq!(reopened.screen, Screen::Receipt);
    assert!(reopened.receipt_reference.is_empty(), "a reference leaked");
    assert!(reopened.lines.is_empty(), "lines leaked");
}

/// A quantity is digits and at most one decimal point, filtered at entry so
/// the operator learns immediately rather than by a refusal after a round trip.
#[test]
fn only_a_well_formed_quantity_can_be_typed() {
    let state = ready();
    let typed = apply(
        &state,
        vec![
            Event::TypeQuantity('.'),
            Event::TypeQuantity('5'),
            Event::TypeQuantity('.'),
            Event::TypeQuantity('x'),
            Event::TypeQuantity('7'),
        ],
    );
    assert_eq!(
        typed.lines[0].entered, "3.57",
        "{:?}",
        typed.lines[0].entered
    );
}

/// A blank line is NOT a zero receipt. Leaving a line alone means "not
/// received"; sending zero would record a receipt for goods that never came.
#[test]
fn a_blank_line_is_not_submitted_as_zero() {
    let state = ready();
    let entered = entered_lines(&state);
    assert_eq!(entered.len(), 1, "only the typed line is submitted");
    assert_eq!(entered[0].line_number, 1);
}

/// Everything `record_receipt` requires is checked BEFORE a request is built,
/// so the operator is told what is missing rather than handed a refusal for
/// something the screen already knew.
#[test]
fn an_incomplete_receipt_names_what_is_missing() {
    let base = apply(
        &AppState::default(),
        vec![
            Event::OrdersLoaded {
                rows: vec![order("1", 4)],
                next: None,
            },
            Event::OpenReceipt,
            Event::ReceiptLoaded {
                lines: vec![line(1, "ITEM-A")],
            },
        ],
    );
    assert_eq!(submittable(&base), Err("a receipt reference is required"));

    let referenced = apply(&base, vec![Event::TypeReference('R')]);
    assert_eq!(submittable(&referenced), Err("a location is required"));

    let located = reduce(
        &referenced,
        Event::LocationsLoaded {
            locations: locations(),
        },
    );
    assert_eq!(
        submittable(&located),
        Err("enter a quantity on at least one line")
    );

    let complete = reduce(&located, Event::TypeQuantity('2'));
    assert_eq!(submittable(&complete), Ok(()));
}

#[test]
fn cycling_locations_wraps_through_the_set() {
    let state = ready();
    assert_eq!(
        state.picked_location().map(|l| l.code.as_str()),
        Some("A-01")
    );
    let next = reduce(&state, Event::NextLocation);
    assert_eq!(
        next.picked_location().map(|l| l.code.as_str()),
        Some("B-02")
    );
    let wrapped = reduce(&next, Event::NextLocation);
    assert_eq!(
        wrapped.picked_location().map(|l| l.code.as_str()),
        Some("A-01")
    );
}

/// A refusal KEEPS the entry: it is something to correct, and discarding what
/// the operator typed would make them retype it.
#[test]
fn a_refusal_keeps_what_the_operator_typed() {
    let state = ready();
    let failed = reduce(
        &state,
        Event::Failed {
            error: ClientError::ConcurrencyConflict {
                expected_row_version: 4,
                observed_row_version: 7,
            },
        },
    );
    assert_eq!(failed.lines[0].entered, "3", "the entry was discarded");
    assert_eq!(failed.receipt_reference, "R1");
    assert!(failed.pending.is_none());
    assert!(matches!(
        failed.failure,
        Some(ClientError::ConcurrencyConflict { .. })
    ));
}

/// A success SPENDS the entry. Leaving typed quantities on screen after a
/// recorded receipt is how the same receipt gets submitted twice.
#[test]
fn a_recorded_receipt_clears_the_entry_and_returns_to_the_list() {
    let recorded = reduce(
        &ready(),
        Event::ReceiptRecorded {
            receipt_id: "33333333-0000-0000-0000-000000000001".to_owned(),
        },
    );
    assert_eq!(recorded.screen, Screen::List);
    assert!(recorded.lines.is_empty());
    assert!(recorded.receipt_reference.is_empty());
    assert!(recorded.receiving.is_none());
    assert!(recorded.failure.is_none());
    assert!(
        recorded
            .notice
            .as_deref()
            .is_some_and(|notice| notice.contains("33333333"))
    );
}

/// A new request clears the last verdict: a stale refusal beside a spinner
/// tells the operator a request failed that has not finished.
#[test]
fn sending_clears_the_previous_verdict() {
    let failed = reduce(
        &ready(),
        Event::Failed {
            error: ClientError::Unauthenticated,
        },
    );
    let sending = reduce(
        &failed,
        Event::Sent {
            operation: "receiving.record_receipt".to_owned(),
        },
    );
    assert!(sending.failure.is_none(), "a stale refusal survived");
    assert_eq!(sending.pending.as_deref(), Some("receiving.record_receipt"));
}

/// The client supplies request_id, the idempotency key and occurred_at; the
/// operator types none of them.
#[test]
fn the_envelope_carries_what_the_client_supplies_and_what_was_entered() {
    let supplied = ClientSupplied {
        request_id: "req-1".to_owned(),
        idempotency_key: "idem-1".to_owned(),
        occurred_at: "2026-09-03T10:15:00Z".to_owned(),
    };
    let items = record_receipt(&ready(), &supplied).expect("a complete receipt builds");
    assert_eq!(items.len(), 1, "one operator action is one envelope item");
    let item = &items[0];

    assert_eq!(item["request_id"], "req-1");
    assert_eq!(item["value"]["idempotency_key"], "idem-1");
    assert_eq!(item["value"]["occurred_at"], "2026-09-03T10:15:00Z");
    assert_eq!(item["value"]["receipt_reference"], "R1");

    let lines = item["value"]["line"].as_array().expect("lines");
    assert_eq!(lines.len(), 1, "only the entered line is sent");
    assert_eq!(
        lines[0]["purchase_order_line_id"],
        "11111111-0000-0000-0000-000000000001"
    );
    assert_eq!(
        lines[0]["location_id"],
        "22222222-0000-0000-0000-000000000001"
    );
}

/// The quantity is a JSON NUMBER, not a string: the contract declares
/// `numeric`, and a quoted quantity is a different wire value the input
/// schema refuses.
#[test]
fn a_quantity_is_sent_as_a_number_carrying_the_typed_digits() {
    let supplied = ClientSupplied {
        request_id: "req-1".to_owned(),
        idempotency_key: "idem-1".to_owned(),
        occurred_at: "2026-09-03T10:15:00Z".to_owned(),
    };
    let decimal = apply(
        &ready(),
        vec![Event::TypeQuantity('.'), Event::TypeQuantity('5')],
    );
    let items = record_receipt(&decimal, &supplied).expect("builds");
    let quantity = &items[0]["value"]["line"][0]["quantity"];
    assert!(quantity.is_number(), "quantity was not a JSON number");
    assert_eq!(quantity.to_string(), "3.5", "the typed digits changed");
}

/// The same operator action retried must carry the SAME idempotency key —
/// a key minted per attempt makes every retry a new receipt, which is the
/// exact failure at-least-once delivery needs the key to prevent.
#[test]
fn a_retry_of_one_action_reuses_its_idempotency_key() {
    let state = ready();
    let supplied = ClientSupplied {
        request_id: "req-1".to_owned(),
        idempotency_key: "idem-stable".to_owned(),
        occurred_at: "2026-09-03T10:15:00Z".to_owned(),
    };
    let first = record_receipt(&state, &supplied).expect("builds");

    // The first attempt is refused; the operator retries the same entry.
    let after_failure = reduce(
        &state,
        Event::Failed {
            error: ClientError::Transport {
                detail: "connection reset".to_owned(),
            },
        },
    );
    let retry = record_receipt(&after_failure, &supplied).expect("builds");

    assert_eq!(
        first[0]["value"]["idempotency_key"],
        retry[0]["value"]["idempotency_key"]
    );
    assert_eq!(first, retry, "the retried envelope differs from the first");
}
