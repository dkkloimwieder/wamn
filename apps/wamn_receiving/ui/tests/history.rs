//! The purchase order history panel pages a history read and folds its rows.

#[expect(
    dead_code,
    reason = "The history tests share the workflow fixtures and use only some of them."
)]
mod support;

use crossterm::event::KeyCode;
use serde_json::{Value, json};
use wamn_client_terminal::operator::{Action, Application};
use wamn_receiving_tui::{Panel, ReceivingApplication};
use wamn_record_history::RowState;

use support::*;

const INSERTED: &str =
    r#"{"id": "00000000-0000-0000-0000-000000000001", "status": "open", "row_version": 1}"#;
const CURRENT: &str =
    r#"{"id": "00000000-0000-0000-0000-000000000001", "status": "complete", "row_version": 2}"#;
const CHANGED: &str =
    r#"{"id": "00000000-0000-0000-0000-000000000001", "status": "complete", "row_version": 3}"#;

fn entry(position: i64, kind: &str, before: &str, after: &str, current: &str, head: i64) -> Value {
    json!({
        "position": position.to_string(), "kind": kind,
        "operation": "wamn-receiving:receiving/record-receipt@1.0.0",
        "changed_by": "cccccccc-0000-0000-0000-000000000001",
        "changed_at": "2026-09-03T00:00:00.000000Z",
        "transaction_id": "4294967297",
        "before": before, "after": after, "current": current,
        "head_position": head.to_string(),
    })
}

/// Open the history of the first purchase order and answer each queued page.
/// Returns the `after_position` of every request that the panel sent.
fn history(pages: &[Value]) -> (ReceivingApplication, Vec<Value>) {
    let mut app = application();
    read(&mut app, "orders", orders(&[1], None));
    key(&mut app, KeyCode::Char('h'));
    assert_eq!(app.panel(), Panel::History);
    let mut requested = Vec::new();
    for (index, rows) in pages.iter().enumerate() {
        let action = app.next_action();
        assert!(
            matches!(action, Action::Send { .. }),
            "expected a queued history page"
        );
        let request = app
            .prepare(action, Some(&intent(&format!("history-{index}"))))
            .expect("a valid history page request");
        let item = request.body.item();
        assert_eq!((&item["id"], &item["limit"]), (&json!(ORDER), &json!(100)));
        requested.push(item["after_position"].clone());
        reply(&mut app, request, json!({ "rows": rows }));
    }
    assert!(
        matches!(app.next_action(), Action::None),
        "the history read is complete"
    );
    (app, requested)
}

fn present(app: &ReceivingApplication) -> Vec<(String, String)> {
    let Ok(RowState::Present(image)) = app.history_state() else {
        panic!(
            "the purchase order is not present: {:?}",
            app.history_state()
        );
    };
    image
        .columns()
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect()
}

#[test]
fn a_present_row_shows_its_columns_at_each_entry() {
    let (mut app, requested) = history(&[json!([
        entry(1, "insert", "{}", INSERTED, CURRENT, 2),
        entry(
            2,
            "update",
            r#"{"status": "open", "row_version": 1}"#,
            r#"{"status": "complete", "row_version": 2}"#,
            CURRENT,
            2
        ),
    ])]);
    assert_eq!(requested, [json!(0)]);
    assert_eq!(present(&app)[1], ("row_version".to_owned(), "2".to_owned()));
    let displayed = render(&app);
    assert!(displayed.contains("State: present"), "{displayed}");
    assert!(displayed.contains(r#"status = "complete""#), "{displayed}");
    key(&mut app, KeyCode::Up);
    assert_eq!(
        present(&app)[2],
        ("status".to_owned(), r#""open""#.to_owned())
    );
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.panel(), Panel::Orders);
}

#[test]
fn a_deleted_row_is_absent_after_its_delete() {
    let (app, _) = history(&[json!([
        entry(1, "insert", "{}", INSERTED, "{}", 2),
        entry(2, "delete", INSERTED, "{}", "{}", 2),
    ])]);
    assert_eq!(app.history_state(), Ok(RowState::Absent));
    assert!(render(&app).contains("State: absent"));
}

#[test]
fn a_row_with_no_retained_entries_is_unavailable() {
    let (app, requested) = history(&[json!([])]);
    assert_eq!(requested, [json!(0)]);
    assert_eq!(app.history_state(), Ok(RowState::Unavailable));
    assert!(render(&app).contains("State: unavailable"));
}

#[test]
fn a_head_change_between_pages_reads_the_history_again() {
    let (app, requested) = history(&[
        json!([entry(1, "insert", "{}", INSERTED, CURRENT, 2)]),
        json!([entry(
            3,
            "update",
            r#"{"row_version": 2}"#,
            r#"{"row_version": 3}"#,
            CHANGED,
            3
        )]),
        json!([
            entry(1, "insert", "{}", INSERTED, CHANGED, 3),
            entry(
                2,
                "update",
                r#"{"status": "open", "row_version": 1}"#,
                r#"{"status": "complete", "row_version": 2}"#,
                CHANGED,
                3
            ),
            entry(
                3,
                "update",
                r#"{"row_version": 2}"#,
                r#"{"row_version": 3}"#,
                CHANGED,
                3
            ),
        ]),
    ]);
    assert_eq!(requested, [json!(0), json!(1), json!(0)]);
    assert_eq!(present(&app)[1], ("row_version".to_owned(), "3".to_owned()));
    assert!(render(&app).contains("State: present"));
}

#[test]
fn rows_that_do_not_fold_show_the_refusal() {
    let (app, requested) = history(&[
        json!([entry(1, "insert", "{}", INSERTED, CURRENT, 2)]),
        json!([]),
    ]);
    assert_eq!(requested, [json!(0), json!(1)]);
    let refusal = app
        .history_state()
        .expect_err("a read that ends before its head does not fold");
    assert!(refusal.contains("IncompleteRead"), "{refusal}");
    assert!(render(&app).contains("The history cannot fold"));
}
