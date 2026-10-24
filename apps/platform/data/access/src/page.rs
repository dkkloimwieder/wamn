//! One keyset read: its rows and the cursor that continues it.
//!
//! The statement reads one row past the limit, and that row says whether a
//! next read has rows. The cursor comes from the last row of the page.

/// The rows of one read and the cursor that continues it, if anything does.
#[derive(Debug)]
pub struct Page<Row> {
    rows: Vec<Row>,
    next_cursor: Option<String>,
    feed_cursor: Option<String>,
}

impl<Row> Page<Row> {
    /// A page of `limit` rows from a statement that read up to `limit + 1`.
    ///
    /// `cursor` mints the cursor from the last row of the page.
    ///
    /// # Errors
    ///
    /// `E` when the last row cannot mint a cursor.
    pub fn new<E>(
        mut rows: Vec<Row>,
        limit: i64,
        cursor: impl Fn(&Row) -> Result<String, E>,
    ) -> Result<Self, E> {
        let limit = usize::try_from(limit).unwrap_or(0);
        // The row past the limit says whether a next read has rows.
        let next_cursor = if rows.len() > limit {
            rows.truncate(limit);
            rows.last().map(cursor).transpose()?
        } else {
            None
        };
        Ok(Self {
            rows,
            next_cursor,
            feed_cursor: None,
        })
    }

    /// The page, with the cursor that its change feed starts from
    /// ([`crate::feed`]).
    #[must_use]
    pub fn with_feed_cursor(self, feed_cursor: String) -> Self {
        Self {
            feed_cursor: Some(feed_cursor),
            ..self
        }
    }

    /// The cursor that the change feed of a list read starts from, when the
    /// list read has a feed.
    pub fn feed_cursor(&self) -> Option<&str> {
        self.feed_cursor.as_deref()
    }

    /// The rows of the page, and the cursor that continues the read.
    pub fn into_parts(self) -> (Vec<Row>, Option<String>) {
        (self.rows, self.next_cursor)
    }
}

#[cfg(test)]
mod tests {
    use super::Page;

    fn page(rows: Vec<u8>, limit: i64) -> (Vec<u8>, Option<String>) {
        Page::new(rows, limit, |row| Ok::<_, ()>(row.to_string()))
            .unwrap()
            .into_parts()
    }

    #[test]
    fn the_row_past_the_limit_mints_the_cursor_from_the_last_row() {
        assert_eq!(page(vec![1, 2, 3], 2), (vec![1, 2], Some("2".to_owned())));
        assert_eq!(page(vec![1, 2], 2), (vec![1, 2], None));
        assert_eq!(page(Vec::new(), 2), (Vec::new(), None));
    }
}
