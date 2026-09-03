//! Every decision the operator screen makes, as a pure function.
//!
//! `reduce(state, event) -> state`. No transport, no clock, no terminal: an
//! event carries whatever the outside world contributed, so a test states the
//! world and reads the consequence. This is where the slice's assertions live,
//! and it is why they can live below the terminal layer.

use wamn_client::ClientError;

use crate::model::{AppState, Location, PurchaseOrderRow, ReceiptLine, Screen};

/// Something that happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Move the highlight on the current screen.
    MoveUp,
    /// Move the highlight on the current screen.
    MoveDown,
    /// A page of purchase orders arrived.
    OrdersLoaded {
        /// The rows.
        rows: Vec<PurchaseOrderRow>,
        /// The cursor for the following page, when the server issued one.
        next: Option<String>,
    },
    /// Open receipt entry against the highlighted order.
    OpenReceipt,
    /// The receipt screen's projection arrived.
    ReceiptLoaded {
        /// The order's lines.
        lines: Vec<ReceiptLine>,
    },
    /// The pickable locations arrived.
    LocationsLoaded {
        /// The locations.
        locations: Vec<Location>,
    },
    /// The operator typed into the highlighted line's quantity.
    TypeQuantity(char),
    /// The operator deleted from the highlighted line's quantity.
    BackspaceQuantity,
    /// The operator typed into the receipt reference.
    TypeReference(char),
    /// Cycle the picked location.
    NextLocation,
    /// A request was sent.
    Sent {
        /// What is outstanding.
        operation: String,
    },
    /// The receipt was recorded.
    ReceiptRecorded {
        /// The server's receipt id.
        receipt_id: String,
    },
    /// A request was refused.
    Failed {
        /// The client's own decision.
        error: ClientError,
    },
    /// Go back to the list.
    Back,
}

/// Apply one event.
///
/// Returns a NEW state rather than mutating: a screen is a rendering of a
/// value, and a test that can hold the before and the after side by side is
/// the one that can say what changed.
#[must_use]
pub fn reduce(state: &AppState, event: Event) -> AppState {
    let mut next = state.clone();
    match event {
        Event::MoveUp => move_highlight(&mut next, -1),
        Event::MoveDown => move_highlight(&mut next, 1),
        Event::OrdersLoaded { rows, next: cursor } => {
            // Pages ACCUMULATE. A page that replaced the list would lose every
            // row the operator has already scrolled past, and a cursor is a
            // continuation, not a new result.
            next.orders.extend(rows);
            next.next_page = cursor.map(wamn_client::Cursor::new);
            if next.selected_order.is_none() && !next.orders.is_empty() {
                next.selected_order = Some(0);
            }
            next.pending = None;
        }
        Event::OpenReceipt => {
            if let Some(order) = state.highlighted_order() {
                next.receiving = Some(order.clone());
                next.screen = Screen::Receipt;
                // Nothing is carried over from a previous receipt: lines, the
                // typed reference and the picked location all belong to the
                // order they were entered against.
                next.lines = Vec::new();
                next.selected_line = None;
                next.receipt_reference = String::new();
                next.selected_location = None;
                next.failure = None;
                next.notice = None;
            }
        }
        Event::ReceiptLoaded { lines } => {
            next.selected_line = (!lines.is_empty()).then_some(0);
            next.lines = lines;
            next.pending = None;
        }
        Event::LocationsLoaded { locations } => {
            next.selected_location = (!locations.is_empty()).then_some(0);
            next.locations = locations;
            next.pending = None;
        }
        Event::TypeQuantity(symbol) => {
            // A quantity is digits and at most one decimal point. Filtering at
            // entry means the operator is told immediately, rather than by a
            // refusal after a round trip.
            if let Some(index) = next.selected_line
                && let Some(line) = next.lines.get_mut(index)
                && (symbol.is_ascii_digit() || (symbol == '.' && !line.entered.contains('.')))
            {
                line.entered.push(symbol);
            }
        }
        Event::BackspaceQuantity => {
            if let Some(index) = next.selected_line
                && let Some(line) = next.lines.get_mut(index)
            {
                line.entered.pop();
            }
        }
        Event::TypeReference(symbol) => next.receipt_reference.push(symbol),
        Event::NextLocation => {
            if !next.locations.is_empty() {
                let current = next.selected_location.unwrap_or(0);
                next.selected_location = Some((current + 1) % next.locations.len());
            }
        }
        Event::Sent { operation } => {
            next.pending = Some(operation);
            // A new request clears the last verdict: showing a stale refusal
            // beside a spinner tells the operator a request failed that has
            // not finished.
            next.failure = None;
            next.notice = None;
        }
        Event::ReceiptRecorded { receipt_id } => {
            next.pending = None;
            next.failure = None;
            next.notice = Some(format!("recorded receipt {receipt_id}"));
            // The entry is spent. Leaving the typed quantities on screen after
            // a success is how an operator submits the same receipt twice.
            next.lines = Vec::new();
            next.selected_line = None;
            next.receipt_reference = String::new();
            next.screen = Screen::List;
            next.receiving = None;
        }
        Event::Failed { error } => {
            next.pending = None;
            // The entry is KEPT. A refusal is something to correct, and
            // discarding what the operator typed would make them retype it.
            next.failure = Some(error);
        }
        Event::Back => {
            next.screen = Screen::List;
            next.receiving = None;
            next.lines = Vec::new();
            next.selected_line = None;
            next.failure = None;
        }
    }
    next
}

fn move_highlight(state: &mut AppState, delta: isize) {
    let (selected, length) = match state.screen {
        Screen::List => (&mut state.selected_order, state.orders.len()),
        Screen::Receipt => (&mut state.selected_line, state.lines.len()),
    };
    if length == 0 {
        *selected = None;
        return;
    }
    let current = selected.unwrap_or(0);
    // Saturating, not wrapping: an operator holding a key expects to stop at
    // the end of a list, not to reappear at the other end of it.
    let moved = isize::try_from(current).unwrap_or(0) + delta;
    let last = length - 1;
    *selected = Some(usize::try_from(moved).unwrap_or(0).min(last));
}

/// The lines the operator has actually entered a quantity for.
///
/// A blank line is not a zero receipt: leaving a line alone means "not
/// received", and sending a zero would record a receipt event for goods that
/// never arrived.
#[must_use]
pub fn entered_lines(state: &AppState) -> Vec<&ReceiptLine> {
    state
        .lines
        .iter()
        .filter(|line| !line.entered.trim().is_empty())
        .collect()
}

/// Whether the receipt is complete enough to send.
///
/// Every part `record_receipt` requires must be present BEFORE a request is
/// built, so an operator is told what is missing instead of being handed a
/// contract refusal for something the screen already knew.
pub fn submittable(state: &AppState) -> Result<(), &'static str> {
    if state.receiving.is_none() {
        return Err("no purchase order is open");
    }
    if state.receipt_reference.trim().is_empty() {
        return Err("a receipt reference is required");
    }
    if state.picked_location().is_none() {
        return Err("a location is required");
    }
    if entered_lines(state).is_empty() {
        return Err("enter a quantity on at least one line");
    }
    Ok(())
}
