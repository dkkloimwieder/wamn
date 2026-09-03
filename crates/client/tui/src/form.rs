//! Forms and field editors, shaped by what the contract admits.
//!
//! The descriptor decides the control: a field with a closed value domain gets
//! a SELECTOR over those values, and one with an open domain gets a free
//! input. That is not a styling choice — offering free text where the contract
//! admits two values invites a refusal the caller could not have predicted,
//! and offering a picker where the domain is open hides most of the range.

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;
use ratatui_core::widgets::Widget;
use wamn_client::FieldDescriptor;

/// How one field is edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldEditor {
    /// Free text: the contract states no closed domain.
    Text,
    /// A choice among the contract's declared values.
    Choice,
}

impl FieldEditor {
    /// The editor a descriptor calls for.
    #[must_use]
    pub const fn of(field: &FieldDescriptor) -> Self {
        if field.is_closed() {
            Self::Choice
        } else {
            Self::Text
        }
    }
}

/// One field's current value, as the caller has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormValue {
    /// The descriptor path this value belongs to.
    pub path: &'static str,
    /// The value, already rendered as text.
    pub value: String,
}

/// A form over a contract's input fields.
#[derive(Debug, Clone)]
pub struct Form<'a> {
    fields: &'a [FieldDescriptor],
    values: &'a [FormValue],
    focused: Option<usize>,
}

impl<'a> Form<'a> {
    /// A form for `fields`, showing `values`.
    #[must_use]
    pub const fn new(fields: &'a [FieldDescriptor], values: &'a [FormValue]) -> Self {
        Self {
            fields,
            values,
            focused: None,
        }
    }

    /// Focus one field.
    #[must_use]
    pub const fn focus(mut self, index: Option<usize>) -> Self {
        self.focused = index;
        self
    }

    fn value_of(&self, path: &str) -> &str {
        self.values
            .iter()
            .find(|value| value.path == path)
            .map_or("", |value| value.value.as_str())
    }
}

impl Widget for Form<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        for (index, field) in self.fields.iter().enumerate() {
            let Ok(offset) = u16::try_from(index) else {
                return;
            };
            if offset >= area.height {
                return;
            }
            let value = self.value_of(field.path);
            // Optionality is SHOWN, because whether a field may be left blank
            // is the single thing a caller most needs to know before typing,
            // and it is a contract fact rather than a presentation choice.
            let requirement = if field.nullable { "" } else { "*" };
            let rendered = match FieldEditor::of(field) {
                FieldEditor::Choice => {
                    format!("{}{requirement} [{}]", field.leaf(), field.values.join("/"))
                }
                FieldEditor::Text => format!("{}{requirement} {value}", field.leaf()),
            };
            let marker = if self.focused == Some(index) {
                ">"
            } else {
                " "
            };
            let line = format!("{marker}{rendered}");
            for (column, symbol) in line.chars().take(area.width as usize).enumerate() {
                let x = area.x + u16::try_from(column).unwrap_or(u16::MAX);
                if x < area.x + area.width {
                    buffer[(x, area.y + offset)].set_char(symbol);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{render_to_buffer, row_text};

    const STATUS: FieldDescriptor = FieldDescriptor {
        path: "value.status",
        type_name: "text",
        nullable: false,
        values: &["open", "closed"],
    };
    const NOTE: FieldDescriptor = FieldDescriptor {
        path: "value.note",
        type_name: "text",
        nullable: true,
        values: &[],
    };

    /// A CLOSED domain is a choice; an OPEN one is free text. This is the
    /// descriptor deciding the control, which is the whole point of the crate.
    #[test]
    fn a_closed_domain_is_a_choice_and_an_open_one_is_text() {
        assert_eq!(FieldEditor::of(&STATUS), FieldEditor::Choice);
        assert_eq!(FieldEditor::of(&NOTE), FieldEditor::Text);
    }

    #[test]
    fn a_choice_field_offers_the_contracts_values() {
        const FIELDS: &[FieldDescriptor] = &[STATUS];
        let buffer = render_to_buffer(Form::new(FIELDS, &[]), 40, 1);
        assert_eq!(row_text(&buffer, 0), " status* [open/closed]");
    }

    /// A required field is marked. Whether a value may be omitted is the one
    /// thing a caller needs before typing, and it is a contract fact.
    #[test]
    fn requiredness_is_visible() {
        const FIELDS: &[FieldDescriptor] = &[NOTE];
        let buffer = render_to_buffer(Form::new(FIELDS, &[]), 40, 1);
        let line = row_text(&buffer, 0);
        assert!(
            !line.contains('*'),
            "an optional field was marked required: {line}"
        );
    }

    #[test]
    fn a_text_field_shows_its_current_value() {
        const FIELDS: &[FieldDescriptor] = &[NOTE];
        let values = [FormValue {
            path: "value.note",
            value: "damaged pallet".to_owned(),
        }];
        let buffer = render_to_buffer(Form::new(FIELDS, &values), 40, 1);
        assert_eq!(row_text(&buffer, 0), " note damaged pallet");
    }

    #[test]
    fn the_focused_field_is_marked() {
        const FIELDS: &[FieldDescriptor] = &[STATUS, NOTE];
        let buffer = render_to_buffer(Form::new(FIELDS, &[]).focus(Some(1)), 40, 2);
        assert!(!row_text(&buffer, 0).starts_with('>'));
        assert!(row_text(&buffer, 1).starts_with('>'));
    }

    /// A value for a field the form does not carry is ignored rather than
    /// rendered against the wrong row.
    #[test]
    fn a_value_for_an_absent_field_is_ignored() {
        const FIELDS: &[FieldDescriptor] = &[NOTE];
        let values = [FormValue {
            path: "value.nonexistent",
            value: "stray".to_owned(),
        }];
        let buffer = render_to_buffer(Form::new(FIELDS, &values), 40, 1);
        assert_eq!(row_text(&buffer, 0), " note");
    }
}
