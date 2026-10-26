//! Change feeds with typed record ids
//! (`docs/plan/wamn-sync.md`, "Change feed (host)").
//!
//! The cursor is opaque to the client: the URL-safe base64 of
//! `<model>:<transaction>` or `<model>:<transaction>:<id>`. `transaction` is
//! the snapshot xmin the read started from. A cut page below xmin also carries
//! the transaction and the id of its last row. A page with visible rows at or
//! above xmin completes the pass and keeps its next cursor at xmin.

use crate::cursor::id::Id;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use crate::Invalid;

/// Where a feed read starts: a transaction, and the last row read in it when
/// the previous page stopped at its limit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Cursor<I> {
    pub transaction: i64,
    pub after: Option<I>,
}

/// The cursor of `model` at `cursor`.
///
/// # Errors
///
/// [`Invalid`] when the id is outside its one spelling.
pub fn encode<I: Id>(model: &str, cursor: &Cursor<I>) -> Result<String, Invalid> {
    let text = match &cursor.after {
        Some(id) => format!(
            "{model}:{}:{}",
            cursor.transaction,
            match id.to_json()? {
                serde_json::Value::String(value) => value,
                value => value.to_string(),
            }
        ),
        None => format!("{model}:{}", cursor.transaction),
    };
    Ok(URL_SAFE_NO_PAD.encode(text))
}

/// The position of a cursor that [`encode`] wrote for `model`.
///
/// # Errors
///
/// [`Invalid`] for a cursor of another model, or one that does not decode.
pub fn decode<I: Id>(encoded: &str, model: &str) -> Result<Cursor<I>, Invalid> {
    let bytes = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| Invalid)?;
    let text = String::from_utf8(bytes).map_err(|_| Invalid)?;
    let rest = text
        .strip_prefix(model)
        .and_then(|rest| rest.strip_prefix(':'))
        .ok_or(Invalid)?;
    let (transaction, after) = match rest.split_once(':') {
        Some((transaction, id)) => (
            transaction,
            Some(
                I::from_json(&serde_json::Value::String(id.to_owned()))
                    .or_else(|_| I::from_json(&serde_json::from_str(id).map_err(|_| Invalid)?))?,
            ),
        ),
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
pub struct Change<Row, I> {
    pub id: I,
    pub row_version: i32,
    pub data: Option<Row>,
}

/// One row the changes statement returned, before the page is cut.
#[derive(Debug)]
pub struct Changed<Row, I> {
    pub transaction: i64,
    pub id: I,
    pub change: Change<Row, I>,
}

/// One page of changes and the cursor that continues the feed.
#[derive(Debug)]
pub struct Changes<Row, I> {
    pub rows: Vec<Change<Row, I>>,
    pub next_cursor: String,
    pub complete: bool,
}

impl<Row, I: Id + Clone> Changes<Row, I> {
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
        mut rows: Vec<Changed<Row, I>>,
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
                    after: None::<I>,
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

    fn round_trip<I: Id + Clone + PartialEq + std::fmt::Debug>(id: &I) {
        let cursor = Cursor {
            transaction: 812,
            after: Some(id.clone()),
        };
        let encoded = encode("item", &cursor).unwrap();
        assert_eq!(decode::<I>(&encoded, "item").unwrap(), cursor);
        assert_eq!(decode::<I>(&encoded, "other"), Err(Invalid));
        let rows = (0..2)
            .map(|_| Changed {
                transaction: 812,
                id: id.clone(),
                change: Change {
                    id: id.clone(),
                    row_version: 1,
                    data: Some(()),
                },
            })
            .collect();
        let page = Changes::new("item", None, 900, rows, 1).unwrap();
        assert!(!page.complete);
        assert_eq!(&page.rows[0].id, id);
        assert_eq!(page.next_cursor, encoded);
        let empty = Changes::<(), I>::new("item", Some(&encoded), 900, vec![], 1).unwrap();
        assert!(empty.complete);
        assert_eq!(empty.next_cursor, encoded);
        let complete = Changes::<(), I>::new("item", None, 900, vec![], 1).unwrap();
        assert_eq!(
            decode::<I>(&complete.next_cursor, "item")
                .unwrap()
                .transaction,
            900
        );
    }

    fn concurrent_transactions<I: Id + Clone + PartialEq + std::fmt::Debug>(ids: &[I; 3]) {
        let changed = |transaction| {
            let id = &ids[match transaction {
                106 => 1,
                107 => 2,
                _ => 0,
            }];
            Changed {
                transaction,
                id: id.clone(),
                change: Change {
                    id: id.clone(),
                    row_version: 1,
                    data: Some(()),
                },
            }
        };
        // Transaction 105 is open while 106 and 107 are already visible.
        let page = Changes::new("item", None, 105, vec![changed(106), changed(107)], 1).unwrap();
        assert_eq!(page.rows.len(), 1);
        assert!(page.complete);
        let cursor = decode::<I>(&page.next_cursor, "item").unwrap();
        assert_eq!(cursor.transaction, 105);
        assert_eq!(cursor.after, None);

        // After 105 commits, it must still qualify for the next read.
        let rows = [105, 106, 107]
            .into_iter()
            .filter(|transaction| *transaction >= cursor.transaction)
            .map(changed)
            .collect();
        let next = Changes::new("item", Some(&page.next_cursor), 108, rows, 1).unwrap();
        assert!(!next.complete);
        assert_eq!(
            decode::<I>(&next.next_cursor, "item").unwrap(),
            Cursor {
                transaction: 105,
                after: Some(ids[0].clone())
            }
        );

        // An extra row outside the returned page cannot complete the pass.
        let below = Changes::new("item", None, 105, vec![changed(104), changed(106)], 1).unwrap();
        assert!(!below.complete);
        assert_eq!(
            decode::<I>(&below.next_cursor, "item").unwrap(),
            Cursor {
                transaction: 104,
                after: Some(ids[0].clone())
            }
        );
    }

    #[test]
    fn int4_feed_keeps_a_late_commit_reachable() {
        concurrent_transactions(&[i32::MAX - 2, i32::MAX - 1, i32::MAX]);
    }

    #[test]
    fn int8_feed_keeps_a_late_commit_reachable() {
        concurrent_transactions(&[
            9_007_199_254_740_993_i64,
            9_007_199_254_740_994,
            9_007_199_254_740_995,
        ]);
    }

    #[test]
    fn integer_feed_pages_keep_full_width_ids() {
        for id in [i32::MIN, 0, i32::MAX] {
            round_trip(&id);
        }
        for id in [i64::MIN, 9_007_199_254_740_993, i64::MAX] {
            round_trip(&id);
        }
        for text in [
            "item:812:+1",
            "item:812:01",
            "item:812:-0",
            "item:812:9223372036854775808",
        ] {
            assert_eq!(
                decode::<i64>(&URL_SAFE_NO_PAD.encode(text), "item"),
                Err(Invalid)
            );
        }
        assert_eq!(
            decode::<i32>(&URL_SAFE_NO_PAD.encode("item:812:2147483648"), "item"),
            Err(Invalid)
        );
    }
}
