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

/// One history entry. The operation states no database position, so an entry
/// carries the opaque cursor that reads the page after it.
fn entry(index: i64, kind: &str, before: &str, after: &str, current: &str) -> Value {
    json!({
        "cursor": format!("cursor-{index}"), "kind": kind,
        "operation": "wamn-receiving:receiving/record-receipt@1.0.0",
        "changed_by": "cccccccc-0000-0000-0000-000000000001",
        "changed_at": "2026-09-03T00:00:00.000000Z",
        "before": before, "after": after, "current": current,
    })
}

/// A page that fills the declared limit, so the panel asks for another one.
/// One insert, then ninety-nine updates that raise the revision by one.
fn full_page(current: &str) -> Value {
    let mut rows = vec![entry(
        1,
        "insert",
        "{}",
        r#"{"id": "00000000-0000-0000-0000-000000000001", "status": "complete", "row_version": 1}"#,
        current,
    )];
    for step in 2..=100 {
        rows.push(entry(
            step,
            "update",
            &format!(r#"{{"row_version": {}}}"#, step - 1),
            &format!(r#"{{"row_version": {step}}}"#),
            current,
        ));
    }
    Value::Array(rows)
}

/// Open the history of the first purchase order and answer each queued page.
/// Returns the `after_cursor` of every request that the panel sent.
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
        requested.push(item["after_cursor"].clone());
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
        entry(1, "insert", "{}", INSERTED, CURRENT),
        entry(
            2,
            "update",
            r#"{"status": "open", "row_version": 1}"#,
            r#"{"status": "complete", "row_version": 2}"#,
            CURRENT
        ),
    ])]);
    assert_eq!(requested, [Value::Null], "the first page states no cursor");
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
        entry(1, "insert", "{}", INSERTED, "{}"),
        entry(2, "delete", INSERTED, "{}", "{}"),
    ])]);
    assert_eq!(app.history_state(), Ok(RowState::Absent));
    assert!(render(&app).contains("State: absent"));
}

#[test]
fn a_row_with_no_retained_entries_is_unavailable() {
    let (app, requested) = history(&[json!([])]);
    assert_eq!(requested, [Value::Null]);
    assert_eq!(app.history_state(), Ok(RowState::Unavailable));
    assert!(render(&app).contains("State: unavailable"));
}

/// A full page is followed by another read, and a short page ends the history.
#[test]
fn a_full_page_reads_the_page_after_it() {
    let last = r#"{"id": "00000000-0000-0000-0000-000000000001", "status": "complete", "row_version": 101}"#;
    let (app, requested) = history(&[
        full_page(last),
        json!([entry(
            101,
            "update",
            r#"{"row_version": 100}"#,
            r#"{"row_version": 101}"#,
            last
        )]),
    ]);
    assert_eq!(
        requested,
        [Value::Null, json!("cursor-100")],
        "the second page follows the cursor of the last entry"
    );
    assert_eq!(
        present(&app)[1],
        ("row_version".to_owned(), "101".to_owned())
    );
}

/// A write between two pages changes the current image, so the panel reads the
/// history again from its first page.
#[test]
fn a_changed_row_between_pages_reads_the_history_again() {
    let (app, requested) = history(&[
        full_page(CURRENT),
        json!([entry(
            101,
            "update",
            r#"{"row_version": 2}"#,
            r#"{"row_version": 3}"#,
            CHANGED
        )]),
        json!([
            entry(1, "insert", "{}", INSERTED, CHANGED),
            entry(
                2,
                "update",
                r#"{"status": "open", "row_version": 1}"#,
                r#"{"status": "complete", "row_version": 2}"#,
                CHANGED
            ),
            entry(
                3,
                "update",
                r#"{"row_version": 2}"#,
                r#"{"row_version": 3}"#,
                CHANGED
            ),
        ]),
    ]);
    assert_eq!(requested, [Value::Null, json!("cursor-100"), Value::Null]);
    assert_eq!(present(&app)[1], ("row_version".to_owned(), "3".to_owned()));
    assert!(render(&app).contains("State: present"));
}

/// The fold refuses a history it cannot trust, and the panel shows the reason.
/// Two inserts of one row cannot both hold, so the state before the second one
/// does not follow.
#[test]
fn rows_that_do_not_fold_show_the_refusal() {
    let (mut app, requested) = history(&[json!([
        entry(1, "insert", "{}", INSERTED, CURRENT),
        entry(2, "insert", "{}", INSERTED, CURRENT),
        entry(3, "insert", "{}", INSERTED, CURRENT),
    ])]);
    assert_eq!(requested, [Value::Null]);
    key(&mut app, KeyCode::Up);
    key(&mut app, KeyCode::Up);
    let refusal = app
        .history_state()
        .expect_err("an entry that does not follow the state after it refuses");
    assert!(refusal.contains("BrokenChain"), "{refusal}");
    assert!(render(&app).contains("The history cannot fold"));
}
