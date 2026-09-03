//! A table whose columns ARE the contract's fields.

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;
use ratatui_core::widgets::Widget;
use wamn_client::FieldDescriptor;

/// A table over rows of already-formatted cells.
///
/// Columns come from descriptors and nothing else. There is no column list, no
/// ordering table and no per-model configuration: a field added to the
/// contract becomes a column by regeneration.
#[derive(Debug, Clone)]
pub struct Table<'a> {
    fields: &'a [FieldDescriptor],
    rows: &'a [Vec<String>],
    selected: Option<usize>,
}

impl<'a> Table<'a> {
    /// A table of `rows` described by `fields`.
    ///
    /// Each row is expected to carry one cell per field, in field order. A row
    /// shorter than the field set renders its missing cells blank rather than
    /// panicking: a partial projection is a real server answer, and a control
    /// that aborted on one would take the whole screen down.
    #[must_use]
    pub const fn new(fields: &'a [FieldDescriptor], rows: &'a [Vec<String>]) -> Self {
        Self {
            fields,
            rows,
            selected: None,
        }
    }

    /// Highlight one row.
    #[must_use]
    pub const fn select(mut self, row: Option<usize>) -> Self {
        self.selected = row;
        self
    }

    /// Column width: the widest of the header and its cells, capped so one
    /// long value cannot push every other column off the screen.
    fn widths(&self) -> Vec<usize> {
        const MAX: usize = 24;
        self.fields
            .iter()
            .enumerate()
            .map(|(index, field)| {
                let widest = self
                    .rows
                    .iter()
                    .filter_map(|row| row.get(index))
                    .map(String::len)
                    .max()
                    .unwrap_or(0);
                field.leaf().len().max(widest).min(MAX)
            })
            .collect()
    }
}

fn write_line(buffer: &mut Buffer, area: Rect, row: u16, cells: &[String], widths: &[usize]) {
    let mut column = area.x;
    for (cell, width) in cells.iter().zip(widths) {
        for (offset, symbol) in cell.chars().take(*width).enumerate() {
            let x = column + u16::try_from(offset).unwrap_or(u16::MAX);
            if x < area.x + area.width && row < area.y + area.height {
                buffer[(x, row)].set_char(symbol);
            }
        }
        column += u16::try_from(width + 1).unwrap_or(u16::MAX);
        if column >= area.x + area.width {
            return;
        }
    }
}

impl Widget for Table<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.height == 0 || self.fields.is_empty() {
            return;
        }
        let widths = self.widths();
        let headers: Vec<String> = self
            .fields
            .iter()
            .map(|field| field.leaf().to_owned())
            .collect();
        write_line(buffer, area, area.y, &headers, &widths);

        for (index, row) in self.rows.iter().enumerate() {
            let Ok(offset) = u16::try_from(index + 1) else {
                return;
            };
            if offset >= area.height {
                return;
            }
            // A selected row is marked in the text itself, not with a style:
            // a caller reading cells back — which is how this is tested and
            // how a screen reader sees it — must be able to tell which row is
            // current without inspecting colours.
            let mut cells = row.clone();
            if self.selected == Some(index)
                && let Some(first) = cells.first_mut()
            {
                first.insert(0, '>');
            }
            write_line(buffer, area, area.y + offset, &cells, &widths);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{render_to_buffer, row_text};

    const FIELDS: &[FieldDescriptor] = &[
        FieldDescriptor {
            path: "value.id",
            type_name: "uuid",
            nullable: false,
            values: &[],
        },
        FieldDescriptor {
            path: "value.status",
            type_name: "text",
            nullable: false,
            values: &["open", "closed"],
        },
    ];

    fn rows() -> Vec<Vec<String>> {
        vec![
            vec!["3f8e".to_owned(), "open".to_owned()],
            vec!["9a1c".to_owned(), "closed".to_owned()],
        ]
    }

    /// The header IS the descriptor set — no column list anywhere.
    #[test]
    fn columns_come_from_the_descriptors() {
        let rows = rows();
        let buffer = render_to_buffer(Table::new(FIELDS, &rows), 40, 4);
        // Column width is the wider of header and widest cell: `id` is two
        // characters but `3f8e` is four, so the column is four wide.
        assert_eq!(row_text(&buffer, 0), "id   status");
        assert_eq!(row_text(&buffer, 1), "3f8e open");
        assert_eq!(row_text(&buffer, 2), "9a1c closed");
    }

    /// A field added to the contract becomes a column with no code change.
    #[test]
    fn a_new_descriptor_becomes_a_new_column() {
        const EXTENDED: &[FieldDescriptor] = &[
            FIELDS[0],
            FIELDS[1],
            FieldDescriptor {
                path: "value.supplier",
                type_name: "text",
                nullable: true,
                values: &[],
            },
        ];
        let rows = vec![vec![
            "3f8e".to_owned(),
            "open".to_owned(),
            "acme".to_owned(),
        ]];
        let buffer = render_to_buffer(Table::new(EXTENDED, &rows), 40, 3);
        assert!(
            row_text(&buffer, 0).contains("supplier"),
            "{}",
            row_text(&buffer, 0)
        );
        assert!(
            row_text(&buffer, 1).contains("acme"),
            "{}",
            row_text(&buffer, 1)
        );
    }

    /// A short row renders blanks rather than taking the screen down. A
    /// partial projection is a real server answer.
    #[test]
    fn a_row_shorter_than_the_field_set_still_renders() {
        let rows = vec![vec!["3f8e".to_owned()]];
        let buffer = render_to_buffer(Table::new(FIELDS, &rows), 40, 3);
        assert_eq!(row_text(&buffer, 1), "3f8e");
    }

    /// Selection is visible in the CELLS, not only in a style — otherwise a
    /// test, or a screen reader, cannot tell which row is current.
    #[test]
    fn the_selected_row_is_marked_in_the_text() {
        let rows = rows();
        let buffer = render_to_buffer(Table::new(FIELDS, &rows).select(Some(1)), 40, 4);
        assert!(!row_text(&buffer, 1).starts_with('>'));
        assert!(
            row_text(&buffer, 2).starts_with('>'),
            "{}",
            row_text(&buffer, 2)
        );
    }

    /// More rows than fit are dropped, not written outside the buffer.
    #[test]
    fn rows_beyond_the_area_are_not_written() {
        let rows: Vec<Vec<String>> = (0..50)
            .map(|index| vec![format!("row{index}"), "open".to_owned()])
            .collect();
        let buffer = render_to_buffer(Table::new(FIELDS, &rows), 40, 3);
        // Height 3 is a header and two rows; the fiftieth row never reaches
        // the buffer and nothing is written past the area.
        // Width comes from the widest cell in the column (`row49`), so a
        // shorter value is padded to it.
        assert_eq!(row_text(&buffer, 2), "row1  open");
    }

    /// An empty descriptor set renders nothing at all rather than a bare
    /// frame implying there is data behind it.
    #[test]
    fn no_descriptors_renders_nothing() {
        let rows = rows();
        let buffer = render_to_buffer(Table::new(&[], &rows), 40, 3);
        assert_eq!(row_text(&buffer, 0), "");
    }
}
