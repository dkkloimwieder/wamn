//! A filter bar over the fields an operation admits filtering on.
//!
//! Filterable fields are a CONTRACT fact — an operation's paging contract
//! declares them — so a bar that offered every field would invite refusals for
//! the ones the server never indexed.

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;
use ratatui_core::widgets::Widget;
use wamn_client::FieldDescriptor;

/// One filter a caller has set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedFilter {
    /// The field being filtered.
    pub path: &'static str,
    /// The value, as typed or chosen.
    pub value: String,
}

/// A one-line bar showing what may be filtered and what currently is.
#[derive(Debug, Clone)]
pub struct FilterBar<'a> {
    filterable: &'a [FieldDescriptor],
    applied: &'a [AppliedFilter],
}

impl<'a> FilterBar<'a> {
    /// A bar over the fields an operation declares filterable.
    #[must_use]
    pub const fn new(filterable: &'a [FieldDescriptor], applied: &'a [AppliedFilter]) -> Self {
        Self {
            filterable,
            applied,
        }
    }
}

impl Widget for FilterBar<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.height == 0 {
            return;
        }
        let line = if self.filterable.is_empty() {
            // Said plainly rather than shown as an empty bar: an empty bar
            // reads as "no filters set", which is a different fact from "this
            // operation cannot be filtered".
            "no filters available".to_owned()
        } else {
            let rendered: Vec<String> = self
                .filterable
                .iter()
                .map(|field| {
                    self.applied
                        .iter()
                        .find(|filter| filter.path == field.path)
                        .map_or_else(
                            || field.leaf().to_owned(),
                            |filter| format!("{}={}", field.leaf(), filter.value),
                        )
                })
                .collect();
            rendered.join("  ")
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

    const SUPPLIER: FieldDescriptor = FieldDescriptor {
        path: "supplier_id",
        type_name: "uuid",
        nullable: true,
        values: &[],
    };
    const STATUS: FieldDescriptor = FieldDescriptor {
        path: "status",
        type_name: "text",
        nullable: true,
        values: &["open", "closed"],
    };

    #[test]
    fn the_bar_lists_the_filterable_fields() {
        const FIELDS: &[FieldDescriptor] = &[STATUS, SUPPLIER];
        let buffer = render_to_buffer(FilterBar::new(FIELDS, &[]), 60, 1);
        assert_eq!(row_text(&buffer, 0), "status  supplier_id");
    }

    #[test]
    fn an_applied_filter_shows_its_value() {
        const FIELDS: &[FieldDescriptor] = &[STATUS, SUPPLIER];
        let applied = [AppliedFilter {
            path: "status",
            value: "open".to_owned(),
        }];
        let buffer = render_to_buffer(FilterBar::new(FIELDS, &applied), 60, 1);
        assert_eq!(row_text(&buffer, 0), "status=open  supplier_id");
    }

    /// "Cannot be filtered" and "no filter set" are different facts and must
    /// not render identically.
    #[test]
    fn an_unfilterable_operation_says_so_rather_than_showing_an_empty_bar() {
        let buffer = render_to_buffer(FilterBar::new(&[], &[]), 60, 1);
        assert_eq!(row_text(&buffer, 0), "no filters available");
    }
}
