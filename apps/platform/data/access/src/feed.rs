//! The change feed of a list read: its cursor and one page of changes
//! (`docs/plan/wamn-sync.md`, "Change feed (host)").
//!
//! The cursor is opaque to the client: the URL-safe base64 of
//! `<model>:<transaction>` or `<model>:<transaction>:<id>`. `transaction` is
//! the snapshot xmin the read started from. A cut page below xmin also carries
//! the transaction and the id of its last row. A page with visible rows at or
//! above xmin completes the pass and keeps its next cursor at xmin.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use wamn_postgres_statements::Uuid;

use crate::{Invalid, scalar};

/// Where a feed read starts: a transaction, and the last row read in it when
/// the previous page stopped at its limit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Cursor {
    pub transaction: i64,
    pub after: Option<Uuid>,
}

/// The cursor of `model` at `cursor`.
///
/// # Errors
///
/// [`Invalid`] when the id is outside its one spelling.
pub fn encode(model: &str, cursor: &Cursor) -> Result<String, Invalid> {
    let text = match &cursor.after {
        Some(id) => format!("{model}:{}:{}", cursor.transaction, scalar::uuid(&id.0)?.0),
        None => format!("{model}:{}", cursor.transaction),
    };
    Ok(URL_SAFE_NO_PAD.encode(text))
}

/// The position of a cursor that [`encode`] wrote for `model`.
///
/// # Errors
///
/// [`Invalid`] for a cursor of another model, or one that does not decode.
pub fn decode(encoded: &str, model: &str) -> Result<Cursor, Invalid> {
    let bytes = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| Invalid)?;
    let text = String::from_utf8(bytes).map_err(|_| Invalid)?;
    let rest = text
        .strip_prefix(model)
        .and_then(|rest| rest.strip_prefix(':'))
        .ok_or(Invalid)?;
    let (transaction, after) = match rest.split_once(':') {
        Some((transaction, id)) => (transaction, Some(scalar::uuid(id)?)),
        None => (rest, None),
    };
    let cursor = Cursor {
        transaction: transaction.parse().map_err(|_| Invalid)?,
        after,
    };
    // Encoding again refuses every other spelling of the same position.
    if encode(model, &cursor)? != encoded {
        return Err(Invalid);
    }
    Ok(cursor)
}

/// One changed row: its data, or `None` when the row is deleted or left the
/// scope of the read.
#[derive(Debug)]
pub struct Change<Row> {
    pub id: String,
    pub row_version: i32,
    pub data: Option<Row>,
}

/// One row the changes statement returned, before the page is cut.
#[derive(Debug)]
pub struct Changed<Row> {
    pub transaction: i64,
    pub id: Uuid,
    pub change: Change<Row>,
}

/// One page of changes and the cursor that continues the feed.
#[derive(Debug)]
pub struct Changes<Row> {
    pub rows: Vec<Change<Row>>,
    pub next_cursor: String,
    pub complete: bool,
}

impl<Row> Changes<Row> {
    /// A page of up to `limit` changes from a statement that read up to
    /// `limit + 1`, in transaction and id order.
    ///
    /// `end` is the snapshot xmin of the statement. A page with any row at or
    /// above `end` is complete and continues from `end`, without an id.
    /// Otherwise, a cut page continues after its last row, and a complete
    /// page with rows continues from `end`. A page with no rows keeps `cursor`,
    /// so an unchanged feed keeps its cursor. With no cursor, the feed starts
    /// at `end`.
    ///
    /// # Errors
    ///
    /// [`Invalid`] when a row id is outside its one spelling.
    pub fn new(
        model: &str,
        cursor: Option<&str>,
        end: i64,
        mut rows: Vec<Changed<Row>>,
        limit: i64,
    ) -> Result<Self, Invalid> {
        let limit = usize::try_from(limit).unwrap_or(0);
        let within_limit = rows.len() <= limit;
        rows.truncate(limit);
        let complete = within_limit || rows.iter().any(|row| row.transaction >= end);
        let next_cursor = if !complete {
            let last = rows.last().ok_or(Invalid)?;
            encode(
                model,
                &Cursor {
                    transaction: last.transaction,
                    after: Some(last.id.clone()),
                },
            )?
        } else if let (true, Some(cursor)) = (rows.is_empty(), cursor) {
            cursor.to_owned()
        } else {
            encode(
                model,
                &Cursor {
                    transaction: end,
                    after: None,
                },
            )?
        };
        Ok(Self {
            rows: rows.into_iter().map(|row| row.change).collect(),
            next_cursor,
            complete,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "01234567-89ab-cdef-0123-456789abcdef";

    fn changed(transaction: i64, id: &str) -> Changed<()> {
        Changed {
            transaction,
            id: Uuid(id.to_owned()),
            change: Change {
                id: id.to_owned(),
                row_version: 1,
                data: Some(()),
            },
        }
    }

    #[test]
    fn a_cursor_round_trips_unchanged_and_names_its_model() {
        for cursor in [
            Cursor {
                transaction: 812,
                after: None,
            },
            Cursor {
                transaction: 812,
                after: Some(Uuid(ID.to_owned())),
            },
        ] {
            let encoded = encode("packaging", &cursor).unwrap();
            assert_eq!(decode(&encoded, "packaging").unwrap(), cursor);
            assert_eq!(
                encode("packaging", &decode(&encoded, "packaging").unwrap()).unwrap(),
                encoded
            );
            assert_eq!(decode(&encoded, "location"), Err(Invalid));
            assert_eq!(decode(&encoded, "packagin"), Err(Invalid));
        }
        assert_eq!(
            URL_SAFE_NO_PAD
                .decode(
                    encode(
                        "packaging",
                        &Cursor {
                            transaction: 812,
                            after: None
                        }
                    )
                    .unwrap()
                )
                .unwrap(),
            b"packaging:812"
        );
    }

    #[test]
    fn a_cursor_in_another_spelling_is_refused() {
        for text in [
            "packaging:",
            "packaging:x",
            "packaging:0812",
            "packaging:+812",
            "packaging:812:",
            "packaging:812:01234567-89AB-CDEF-0123-456789ABCDEF",
            "packaging:812:not-a-uuid",
            "packaging",
        ] {
            assert_eq!(
                decode(&URL_SAFE_NO_PAD.encode(text), "packaging"),
                Err(Invalid),
                "{text}"
            );
        }
        assert_eq!(decode("not base64!", "packaging"), Err(Invalid));
        assert_eq!(
            decode(
                &format!("{}=", URL_SAFE_NO_PAD.encode("packaging:812")),
                "packaging"
            ),
            Err(Invalid)
        );
    }

    #[test]
    fn a_page_cut_at_its_limit_continues_after_its_last_row() {
        let page = Changes::new(
            "packaging",
            Some("old"),
            900,
            vec![
                changed(800, ID),
                changed(800, "11111111-1111-7111-8111-111111111111"),
            ],
            1,
        )
        .unwrap();
        assert!(!page.complete);
        assert_eq!(page.rows.len(), 1);
        assert_eq!(
            decode(&page.next_cursor, "packaging").unwrap(),
            Cursor {
                transaction: 800,
                after: Some(Uuid(ID.to_owned()))
            }
        );
    }

    #[test]
    fn a_page_fills_above_xmin_and_completes_even_with_an_extra_row() {
        for count in [100, 101] {
            let rows = (0..count)
                .map(|index| {
                    changed(
                        104 + index,
                        &format!("00000000-0000-4000-8000-{index:012x}"),
                    )
                })
                .collect();
            let page = Changes::new("packaging", Some("old"), 105, rows, 100).unwrap();
            assert!(page.complete);
            assert_eq!(page.rows.len(), 100);
            assert_eq!(
                page.rows.last().unwrap().id,
                "00000000-0000-4000-8000-000000000063"
            );
            assert_eq!(
                decode(&page.next_cursor, "packaging").unwrap(),
                Cursor {
                    transaction: 105,
                    after: None,
                }
            );
        }
    }

    #[test]
    fn an_extra_row_above_xmin_does_not_complete_a_cut_page_below_it() {
        let page = Changes::new(
            "packaging",
            Some("old"),
            105,
            vec![changed(104, ID), changed(106, ID)],
            1,
        )
        .unwrap();
        assert!(!page.complete);
        assert_eq!(page.rows.len(), 1);
        assert_eq!(
            decode(&page.next_cursor, "packaging").unwrap(),
            Cursor {
                transaction: 104,
                after: Some(Uuid(ID.to_owned())),
            }
        );
    }

    #[test]
    fn a_complete_page_continues_from_the_snapshot_and_an_empty_one_keeps_its_cursor() {
        let page = Changes::new("packaging", Some("old"), 900, vec![changed(800, ID)], 1).unwrap();
        assert!(page.complete);
        assert_eq!(
            decode(&page.next_cursor, "packaging").unwrap(),
            Cursor {
                transaction: 900,
                after: None
            }
        );

        let empty = Changes::<()>::new("packaging", Some("old"), 900, Vec::new(), 1).unwrap();
        assert!(empty.complete);
        assert_eq!(empty.next_cursor, "old");

        let start = Changes::<()>::new("packaging", None, 900, Vec::new(), 1).unwrap();
        assert_eq!(
            decode(&start.next_cursor, "packaging").unwrap().transaction,
            900
        );
    }
}

pub mod id;
