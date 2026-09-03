//! Paging over an opaque cursor.
//!
//! The pager holds a cursor and never reads one. A control that decoded a
//! cursor to show "page 3 of 9" would be depending on a shape the contract
//! declares private, and would break the first time keyset ordering changed —
//! so this renders what is actually known: how many rows are in hand, and
//! whether more exist.

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;
use ratatui_core::widgets::Widget;
use wamn_client::Cursor;

/// A one-line pager.
#[derive(Debug, Clone)]
pub struct Pager<'a> {
    loaded: usize,
    next: Option<&'a Cursor>,
}

impl<'a> Pager<'a> {
    /// A pager over `loaded` rows, with `next` when the server issued one.
    #[must_use]
    pub const fn new(loaded: usize, next: Option<&'a Cursor>) -> Self {
        Self { loaded, next }
    }
}

impl Widget for Pager<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.height == 0 {
            return;
        }
        // No total, because there is none to state: a keyset page knows what
        // it holds and whether more follow, and inventing "of N" would show a
        // number the server never sent.
        let line = match self.next {
            Some(_) => format!("{} rows  more available", self.loaded),
            None => format!("{} rows  complete", self.loaded),
        };
        for (column, symbol) in line.chars().take(area.width as usize).enumerate() {
            let x = area.x + u16::try_from(column).unwrap_or(u16::MAX);
            if x < area.x + area.width {
                buffer[(x, area.y)].set_char(symbol);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{render_to_buffer, row_text};

    #[test]
    fn a_page_with_a_cursor_says_more_is_available() {
        let cursor = Cursor::new("eyJ2IjoxfQ");
        let buffer = render_to_buffer(Pager::new(100, Some(&cursor)), 40, 1);
        assert_eq!(row_text(&buffer, 0), "100 rows  more available");
    }

    #[test]
    fn a_final_page_says_it_is_complete() {
        let buffer = render_to_buffer(Pager::new(7, None), 40, 1);
        assert_eq!(row_text(&buffer, 0), "7 rows  complete");
    }

    /// The cursor is never decoded, so nothing it contains can reach the
    /// screen. A pager that showed "page 3" would be reading a private shape.
    #[test]
    fn no_part_of_the_cursor_is_rendered() {
        let cursor = Cursor::new("cGFnZS0z");
        let buffer = render_to_buffer(Pager::new(50, Some(&cursor)), 60, 1);
        let line = row_text(&buffer, 0);
        assert!(!line.contains("cGFnZS0z"), "{line}");
        assert!(!line.contains("of"), "a total was invented: {line}");
    }
}
