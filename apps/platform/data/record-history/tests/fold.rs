//! The fold over the rows of a history read, with rows in the shape that the
//! log trigger and `wamn_history.row_image` write.

use wamn_record_history::{FoldErrorKind, HistoryRow, RowState, state_at};

const INSERTED: &str =
    r#"{"id": 1, "note": "first", "amount": 12.3400, "updated_at": "2026-10-01T09:07:00.000000Z"}"#;
const CURRENT: &str =
    r#"{"id": 1, "note": "second", "amount": 12.34, "updated_at": "2026-10-02T10:00:00.250000Z"}"#;

fn row<'a>(
    position: i64,
    kind: &'a str,
    before: &'a str,
    current: &'a str,
    head: i64,
) -> HistoryRow<'a> {
    HistoryRow {
        position,
        kind,
        before,
        current,
        head_position: head,
    }
}

/// The entries of one row: an insert, a note change, and a scale-only change.
fn complete_chain() -> Vec<HistoryRow<'static>> {
    vec![
        row(1, "insert", "{}", CURRENT, 5),
        row(
            3,
            "update",
            r#"{"note": "first", "updated_at": "2026-10-01T09:07:00.000000Z"}"#,
            CURRENT,
            5,
        ),
        row(5, "update", r#"{"amount": 12.3400}"#, CURRENT, 5),
    ]
}

fn present(state: RowState) -> Vec<(String, String)> {
    let RowState::Present(image) = state else {
        panic!("the row is not present: {state:?}");
    };
    image
        .columns()
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect()
}

fn columns(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect()
}

#[test]
fn a_complete_chain_reconstructs_every_retained_position() {
    let rows = complete_chain();
    let inserted = columns(&[
        ("amount", "12.3400"),
        ("id", "1"),
        ("note", "\"first\""),
        ("updated_at", "\"2026-10-01T09:07:00.000000Z\""),
    ]);
    assert_eq!(state_at(&rows, 0), Ok(RowState::Unavailable));
    assert_eq!(present(state_at(&rows, 1).unwrap()), inserted);
    assert_eq!(present(state_at(&rows, 2).unwrap()), inserted);
    assert_eq!(
        present(state_at(&rows, 4).unwrap()),
        columns(&[
            ("amount", "12.3400"),
            ("id", "1"),
            ("note", "\"second\""),
            ("updated_at", "\"2026-10-02T10:00:00.250000Z\""),
        ])
    );
    let current = columns(&[
        ("amount", "12.34"),
        ("id", "1"),
        ("note", "\"second\""),
        ("updated_at", "\"2026-10-02T10:00:00.250000Z\""),
    ]);
    assert_eq!(present(state_at(&rows, 5).unwrap()), current);
    assert_eq!(present(state_at(&rows, i64::MAX).unwrap()), current);
}

#[test]
fn a_truncated_chain_is_unavailable_before_its_oldest_entry() {
    let rows = complete_chain().split_off(1);
    assert_eq!(state_at(&rows, 1), Ok(RowState::Unavailable));
    assert_eq!(state_at(&rows, 2), Ok(RowState::Unavailable));
    assert_eq!(
        present(state_at(&rows, 3).unwrap()),
        columns(&[
            ("amount", "12.3400"),
            ("id", "1"),
            ("note", "\"second\""),
            ("updated_at", "\"2026-10-02T10:00:00.250000Z\""),
        ])
    );
}

#[test]
fn a_log_turned_on_after_the_row_existed_starts_at_its_first_update() {
    let rows = [
        row(7, "update", r#"{"note": "before the log"}"#, CURRENT, 8),
        row(8, "update", r#"{"amount": 12.3400}"#, CURRENT, 8),
    ];
    assert_eq!(state_at(&rows, 6), Ok(RowState::Unavailable));
    assert_eq!(
        present(state_at(&rows, 7).unwrap()),
        columns(&[
            ("amount", "12.3400"),
            ("id", "1"),
            ("note", "\"second\""),
            ("updated_at", "\"2026-10-02T10:00:00.250000Z\""),
        ])
    );
}

#[test]
fn a_deleted_row_reconstructs_from_its_delete_image() {
    let rows = [
        row(1, "insert", "{}", "{}", 4),
        row(2, "update", r#"{"note": "first"}"#, "{}", 4),
        row(4, "delete", CURRENT, "{}", 4),
    ];
    assert_eq!(state_at(&rows, 4), Ok(RowState::Absent));
    assert_eq!(state_at(&rows, 9), Ok(RowState::Absent));
    assert_eq!(
        present(state_at(&rows, 3).unwrap()),
        columns(&[
            ("amount", "12.34"),
            ("id", "1"),
            ("note", "\"second\""),
            ("updated_at", "\"2026-10-02T10:00:00.250000Z\""),
        ])
    );
    assert_eq!(
        present(state_at(&rows, 1).unwrap()),
        columns(&[
            ("amount", "12.34"),
            ("id", "1"),
            ("note", "\"first\""),
            ("updated_at", "\"2026-10-02T10:00:00.250000Z\""),
        ])
    );

    // After retention removes the delete image, nothing shows the row.
    let expired = [row(4, "delete", CURRENT, "{}", 4)];
    assert_eq!(state_at(&expired, 3), Ok(RowState::Unavailable));
    assert_eq!(state_at(&expired, 4), Ok(RowState::Absent));
}

#[test]
fn a_key_change_reads_as_a_delete_and_an_insert() {
    let moved = r#"{"id": 2, "note": "first", "amount": 12.3400, "updated_at": "2026-10-01T09:07:00.000000Z"}"#;
    let old_key = [
        row(1, "insert", "{}", "{}", 2),
        row(2, "delete", INSERTED, "{}", 2),
    ];
    let new_key = [row(3, "insert", "{}", moved, 3)];
    assert_eq!(
        present(state_at(&old_key, 1).unwrap()),
        columns(&[
            ("amount", "12.3400"),
            ("id", "1"),
            ("note", "\"first\""),
            ("updated_at", "\"2026-10-01T09:07:00.000000Z\""),
        ])
    );
    assert_eq!(state_at(&old_key, 3), Ok(RowState::Absent));
    assert_eq!(state_at(&new_key, 2), Ok(RowState::Unavailable));
    assert_eq!(
        present(state_at(&new_key, 3).unwrap()),
        columns(&[
            ("amount", "12.3400"),
            ("id", "2"),
            ("note", "\"first\""),
            ("updated_at", "\"2026-10-01T09:07:00.000000Z\""),
        ])
    );
}

#[test]
fn a_row_with_no_retained_entries_is_unavailable() {
    for position in [i64::MIN, 0, 1, i64::MAX] {
        assert_eq!(state_at(&[], position), Ok(RowState::Unavailable));
    }
}

#[test]
fn every_value_spelling_survives_the_fold() {
    let current = r#"{"id": 1, "a\"b\\c": "x", "doc": {"k": [1.50, "}", ","]}, "gone": null, "flag": true, "text": "caf\u00e9 \ud83d\ude00"}"#;
    let rows = [
        row(1, "insert", "{}", current, 2),
        row(
            2,
            "update",
            r#"{"doc": {"k": []}, "amount": 0.000100}"#,
            current,
            2,
        ),
    ];
    assert_eq!(
        present(state_at(&rows, 2).unwrap()),
        columns(&[
            ("a\"b\\c", "\"x\""),
            ("doc", r#"{"k": [1.50, "}", ","]}"#),
            ("flag", "true"),
            ("gone", "null"),
            ("id", "1"),
            ("text", r#""caf\u00e9 \ud83d\ude00""#),
        ])
    );
    assert_eq!(
        present(state_at(&rows, 1).unwrap()),
        columns(&[
            ("a\"b\\c", "\"x\""),
            ("amount", "0.000100"),
            ("doc", r#"{"k": []}"#),
            ("flag", "true"),
            ("gone", "null"),
            ("id", "1"),
            ("text", r#""caf\u00e9 \ud83d\ude00""#),
        ])
    );
}

#[test]
fn the_fold_refuses_rows_that_do_not_form_one_read() {
    let refusal = |rows: &[HistoryRow<'_>]| {
        let error = state_at(rows, 1).expect_err("the rows must refuse");
        (error.kind(), error.position())
    };
    assert_eq!(
        refusal(&[
            row(1, "insert", "{}", CURRENT, 3),
            row(3, "update", "{}", CURRENT, 4)
        ]),
        (FoldErrorKind::HeadMismatch, 1)
    );
    assert_eq!(
        refusal(&[
            row(3, "insert", "{}", CURRENT, 3),
            row(3, "update", "{}", CURRENT, 3)
        ]),
        (FoldErrorKind::PositionOrder, 3)
    );
    assert_eq!(
        refusal(&[row(1, "insert", "{}", CURRENT, 3)]),
        (FoldErrorKind::IncompleteRead, 1)
    );
    assert_eq!(
        refusal(&[row(1, "upsert", "{}", CURRENT, 1)]),
        (FoldErrorKind::UnknownKind, 1)
    );
    assert_eq!(
        refusal(&[row(1, "update", "{}", "[1]", 1)]),
        (FoldErrorKind::MalformedImage, 1)
    );
    assert_eq!(
        refusal(&[row(1, "update", "{}", r#"{"id": 1,}"#, 1)]),
        (FoldErrorKind::MalformedImage, 1)
    );
    assert_eq!(
        refusal(&[
            row(1, "insert", "{}", "{}", 3),
            row(2, "delete", CURRENT, "{}", 3),
            row(3, "delete", CURRENT, "{}", 3),
        ]),
        (FoldErrorKind::BrokenChain, 2)
    );
}
