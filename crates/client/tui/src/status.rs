//! Status, pending and error lines.
//!
//! The error arm renders a [`ClientError`] rather than a string, so what a
//! caller reads is what the client actually decided — including the two
//! distinctions the transport works to preserve: a concurrency conflict shows
//! BOTH revisions, and an authentication failure shows nothing about the
//! credential.

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;
use ratatui_core::widgets::Widget;
use wamn_client::ClientError;

/// What the line is reporting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusKind {
    /// Nothing in flight.
    Idle,
    /// A request is outstanding.
    Pending {
        /// What is being awaited, e.g. `purchase_order.query`.
        operation: String,
    },
    /// The last request succeeded.
    Done {
        /// What completed.
        operation: String,
    },
    /// The last request failed.
    Failed {
        /// The client's own decision, rendered as it stands.
        error: ClientError,
    },
}

/// A one-line status.
#[derive(Debug, Clone)]
pub struct Status<'a> {
    kind: &'a StatusKind,
}

impl<'a> Status<'a> {
    /// A status line for `kind`.
    #[must_use]
    pub const fn new(kind: &'a StatusKind) -> Self {
        Self { kind }
    }
}

impl Widget for Status<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.height == 0 {
            return;
        }
        let line = match self.kind {
            StatusKind::Idle => String::new(),
            StatusKind::Pending { operation } => format!("... {operation}"),
            StatusKind::Done { operation } => format!("ok  {operation}"),
            // Rendered through Display, so the client's wording is what a
            // caller sees. Restating an error here would be a second error
            // vocabulary that could disagree with the one the client decided.
            StatusKind::Failed { error } => format!("!!  {error}"),
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
    fn pending_and_done_name_the_operation() {
        let pending = StatusKind::Pending {
            operation: "purchase_order.query".to_owned(),
        };
        let buffer = render_to_buffer(Status::new(&pending), 60, 1);
        assert_eq!(row_text(&buffer, 0), "... purchase_order.query");

        let done = StatusKind::Done {
            operation: "purchase_order.update".to_owned(),
        };
        let buffer = render_to_buffer(Status::new(&done), 60, 1);
        assert_eq!(row_text(&buffer, 0), "ok  purchase_order.update");
    }

    /// A conflict must reach the screen with BOTH revisions: the expected one
    /// alone cannot tell a caller whether to retry or to merge.
    #[test]
    fn a_conflict_shows_both_revisions_on_screen() {
        let failed = StatusKind::Failed {
            error: ClientError::ConcurrencyConflict {
                expected_row_version: 4,
                observed_row_version: 7,
            },
        };
        let buffer = render_to_buffer(Status::new(&failed), 80, 1);
        let line = row_text(&buffer, 0);
        assert!(line.contains('4') && line.contains('7'), "{line}");
    }

    /// The indistinguishability the client preserves must survive rendering:
    /// a status line that leaked why a credential failed would undo it at the
    /// last step.
    #[test]
    fn an_authentication_failure_shows_nothing_about_the_credential() {
        let failed = StatusKind::Failed {
            error: ClientError::from_status(401, r#"{"reason":"expired at 2026-01-01"}"#),
        };
        let buffer = render_to_buffer(Status::new(&failed), 80, 1);
        let line = row_text(&buffer, 0);
        assert!(!line.contains("expired"), "{line}");
        assert!(!line.contains("2026"), "{line}");
    }

    /// Idle renders nothing rather than a placeholder that reads as a result.
    #[test]
    fn idle_renders_nothing() {
        let buffer = render_to_buffer(Status::new(&StatusKind::Idle), 40, 1);
        assert_eq!(row_text(&buffer, 0), "");
    }
}
