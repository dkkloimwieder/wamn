//! Descriptor-driven terminal primitives.
//!
//! Every control here is told WHAT to render by a slice of
//! [`FieldDescriptor`](wamn_client::FieldDescriptor) — the field set a
//! contract declares, emitted into generated bindings. None of them carries a
//! hand-written field list, so a column, an input or a filter appears when the
//! contract gains a field and disappears when it loses one, by regeneration
//! alone.
//!
//! # Primitives, not domain widgets
//!
//! A primitive is parameterized entirely by descriptors and values: it can
//! render any model, and it names none. A domain widget knows a particular
//! model — a `PurchaseOrderTable` that hardcodes which columns matter, or an
//! `InspectionForm` that groups fields by meaning. Domain widgets are NOT
//! built here; they wait for a second application to show which conventions
//! are real rather than incidental to the first.
//!
//! # Rendering below the terminal
//!
//! These are `ratatui-core` widgets: they render into a [`Buffer`], which is a
//! plain grid of cells. No backend, no terminal, no raw mode — so every test
//! renders into a buffer and reads the cells back, which is what lets the
//! slice's assertions live below the terminal layer.
//!
//! `ratatui-core` rather than `ratatui` is deliberate: the 0.30 split
//! published the core half for widget LIBRARIES and left backends, layout
//! caching and the widget zoo to the application. The terminal belongs to the
//! app, not to this crate.

pub mod filter;
pub mod form;
pub mod pager;
pub mod status;
pub mod table;

pub use filter::FilterBar;
pub use form::{FieldEditor, Form};
pub use pager::Pager;
pub use status::{Status, StatusKind};
pub use table::Table;

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;

/// Read one row of a buffer back as a string, trailing blanks trimmed.
///
/// Rendering is only testable if the result can be read; this is the reader
/// every test in this crate uses, so it lives beside the widgets rather than
/// being copied into each test module.
#[must_use]
pub fn row_text(buffer: &Buffer, row: u16) -> String {
    let area = buffer.area();
    let mut text = String::new();
    for column in area.x..area.x + area.width {
        text.push_str(buffer[(column, row)].symbol());
    }
    text.trim_end().to_owned()
}

/// Render a widget into a fresh buffer of the given size.
///
/// # Panics
///
/// If `width` or `height` is zero — a zero-area render has nothing to assert
/// and is always a mistake in a caller.
#[must_use]
pub fn render_to_buffer<W: ratatui_core::widgets::Widget>(
    widget: W,
    width: u16,
    height: u16,
) -> Buffer {
    assert!(width > 0 && height > 0, "a rendered area must be non-empty");
    let area = Rect::new(0, 0, width, height);
    let mut buffer = Buffer::empty(area);
    widget.render(area, &mut buffer);
    buffer
}
