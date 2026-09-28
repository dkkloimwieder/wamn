//! One keyset read, handed out one row at a time as the host reads it.
//!
//! The statement reads one row past the limit, and that row says whether a
//! next read has rows. The cursor comes from the last row handed out.

use std::fmt;

use wamn_postgres_statements::{RowStream, StatementError};

/// Mints the cursor that starts the next read after one row.
type CursorFn<Row, E> = Box<dyn Fn(&Row) -> Result<String, E>>;

/// The rows of one read and the cursor that continues it, if anything does.
///
/// `E` is the refusal of the package that owns the read.
pub struct Page<Row, E> {
    rows: RowStream<Row>,
    limit: usize,
    handed: usize,
    statement: fn(&StatementError) -> E,
    cursor: CursorFn<Row, E>,
    next_cursor: Option<String>,
}

impl<Row, E> fmt::Debug for Page<Row, E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Page")
            .field("limit", &self.limit)
            .field("handed", &self.handed)
            .finish_non_exhaustive()
    }
}

impl<Row, E> Page<Row, E> {
    /// A read of `limit` rows over a statement that reads `limit + 1`.
    ///
    /// `statement` translates a failed read, and `cursor` mints the cursor
    /// from the last row.
    pub fn new(
        rows: RowStream<Row>,
        limit: i64,
        statement: fn(&StatementError) -> E,
        cursor: impl Fn(&Row) -> Result<String, E> + 'static,
    ) -> Self {
        Self {
            rows,
            limit: usize::try_from(limit).unwrap_or(0),
            handed: 0,
            statement,
            cursor: Box::new(cursor),
            next_cursor: None,
        }
    }

    /// The next row, or `None` after `limit` rows or the last row.
    ///
    /// # Errors
    ///
    /// `E` when the statement fails or a row cannot mint a cursor.
    pub async fn next(&mut self) -> Result<Option<Row>, E> {
        if self.handed == self.limit {
            return Ok(None);
        }
        let Some(row) = self.read().await? else {
            self.handed = self.limit;
            return Ok(None);
        };
        self.handed += 1;
        // The row past the limit says whether a next read has rows.
        if self.handed == self.limit && self.read().await?.is_some() {
            self.next_cursor = Some((self.cursor)(&row)?);
        }
        Ok(Some(row))
    }

    /// The cursor that continues this read, once `next` returned `None`.
    pub fn next_cursor(&self) -> Option<String> {
        self.next_cursor.clone()
    }

    async fn read(&mut self) -> Result<Option<Row>, E> {
        self.rows
            .next()
            .await
            .map_err(|error| (self.statement)(&error))
    }
}
