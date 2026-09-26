//! One keyset read, handed out a row at a time.
//!
//! The statement reads one row past the limit, and that row says whether a
//! next read has rows. The cursor is minted from the last row handed out.

use wamn_postgres_statements::RowStream;

use crate::error::{AccessError, AllowedConstraints};

/// Mints the cursor that starts the next read after one row.
type CursorFn<Row> = Box<dyn Fn(&Row) -> Result<Box<str>, AccessError>>;

/// The rows of one read, handed out one at a time as the host reads them,
/// and the cursor that continues the read, if anything does.
pub struct Page<Row> {
    rows: RowStream<Row>,
    context: &'static str,
    limit: usize,
    handed: usize,
    cursor: CursorFn<Row>,
    next_cursor: Option<Box<str>>,
}

impl<Row> std::fmt::Debug for Page<Row> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Page")
            .field("context", &self.context)
            .field("limit", &self.limit)
            .field("handed", &self.handed)
            .finish_non_exhaustive()
    }
}

impl<Row> Page<Row> {
    /// A read of `limit` rows over a statement that reads `limit + 1`.
    pub(crate) fn new(
        rows: RowStream<Row>,
        context: &'static str,
        limit: i64,
        cursor: impl Fn(&Row) -> Result<Box<str>, AccessError> + 'static,
    ) -> Self {
        Self {
            rows,
            context,
            limit: usize::try_from(limit).unwrap_or(0),
            handed: 0,
            cursor: Box::new(cursor),
            next_cursor: None,
        }
    }

    /// The next row, or `None` after `limit` rows or the last row.
    ///
    /// # Errors
    ///
    /// [`AccessError`] when the statement fails or a row cannot mint a cursor.
    pub async fn next(&mut self) -> Result<Option<Row>, AccessError> {
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
    pub fn next_cursor(&self) -> Option<Box<str>> {
        self.next_cursor.clone()
    }

    async fn read(&mut self) -> Result<Option<Row>, AccessError> {
        self.rows.next().await.map_err(|source| {
            AccessError::from_statement(self.context, &source, AllowedConstraints::NONE)
        })
    }
}
