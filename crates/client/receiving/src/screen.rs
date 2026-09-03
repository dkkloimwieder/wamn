//! Rendering a state through the primitive controls.
//!
//! Nothing here decides anything: every value shown was settled by the
//! reducer. The screens are assembled from `wamn-client-tui` primitives, so
//! this crate contributes the DOMAIN — which fields matter for receiving, in
//! what order — and no new control behaviour.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;
use wamn_client::FieldDescriptor;
use wamn_client_tui::status::{Status, StatusKind};
use wamn_client_tui::{Pager, Table};

use crate::model::{AppState, Screen};

/// The purchase order list's columns.
const ORDER_FIELDS: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "purchase_order_number",
        type_name: "text",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "status",
        type_name: "text",
        nullable: false,
        values: &["open", "complete", "cancelled"],
    },
    FieldDescriptor {
        path: "row_version",
        type_name: "int64",
        nullable: false,
        values: &[],
    },
];

/// The receipt screen's columns.
const LINE_FIELDS: &[FieldDescriptor] = &[
    FieldDescriptor {
        path: "line_number",
        type_name: "int32",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "item_number",
        type_name: "text",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "ordered",
        type_name: "numeric",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "received",
        type_name: "numeric",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "remaining",
        type_name: "numeric",
        nullable: false,
        values: &[],
    },
    FieldDescriptor {
        path: "entered",
        type_name: "numeric",
        nullable: true,
        values: &[],
    },
];

/// The whole terminal, for one state.
#[derive(Debug, Clone)]
pub struct AppScreen<'a> {
    state: &'a AppState,
}

impl<'a> AppScreen<'a> {
    /// Render `state`.
    #[must_use]
    pub const fn new(state: &'a AppState) -> Self {
        Self { state }
    }

    fn status(&self) -> StatusKind {
        // Order matters: a refusal outranks a pending request, because a
        // request that has already failed is not still running.
        if let Some(error) = &self.state.failure {
            StatusKind::Failed {
                error: error.clone(),
            }
        } else if let Some(operation) = &self.state.pending {
            StatusKind::Pending {
                operation: operation.clone(),
            }
        } else if let Some(notice) = &self.state.notice {
            StatusKind::Done {
                operation: notice.clone(),
            }
        } else {
            StatusKind::Idle
        }
    }
}

impl Widget for AppScreen<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.height < 3 {
            return;
        }
        let body = Rect::new(area.x, area.y, area.width, area.height - 2);
        let pager_row = area.y + area.height - 2;
        let status_row = area.y + area.height - 1;

        match self.state.screen {
            Screen::List => {
                let rows: Vec<Vec<String>> = self
                    .state
                    .orders
                    .iter()
                    .map(|order| {
                        vec![
                            order.number.clone(),
                            order.status.clone(),
                            order.row_version.to_string(),
                        ]
                    })
                    .collect();
                Table::new(ORDER_FIELDS, &rows)
                    .select(self.state.selected_order)
                    .render(body, buffer);
                Pager::new(self.state.orders.len(), self.state.next_page.as_ref())
                    .render(Rect::new(area.x, pager_row, area.width, 1), buffer);
            }
            Screen::Receipt => {
                let rows: Vec<Vec<String>> = self
                    .state
                    .lines
                    .iter()
                    .map(|line| {
                        vec![
                            line.line_number.to_string(),
                            line.item_number.clone(),
                            line.ordered.clone(),
                            line.received.clone(),
                            line.remaining.clone(),
                            line.entered.clone(),
                        ]
                    })
                    .collect();
                Table::new(LINE_FIELDS, &rows)
                    .select(self.state.selected_line)
                    .render(body, buffer);

                // The two client-visible choices, on one line, because a
                // receipt cannot be sent without either and an operator must
                // not have to remember what they picked.
                let reference = if self.state.receipt_reference.is_empty() {
                    "<reference>"
                } else {
                    &self.state.receipt_reference
                };
                let location = self
                    .state
                    .picked_location()
                    .map_or("<location>", |location| location.code.as_str());
                let line = format!("ref {reference}  into {location}");
                for (column, symbol) in line.chars().take(area.width as usize).enumerate() {
                    let x = area.x + u16::try_from(column).unwrap_or(u16::MAX);
                    if x < area.x + area.width {
                        buffer[(x, pager_row)].set_char(symbol);
                    }
                }
            }
        }
        Status::new(&self.status()).render(Rect::new(area.x, status_row, area.width, 1), buffer);
    }
}
