//! What the operator is looking at, and what they have typed.
//!
//! A plain value with no transport and no terminal in it. Every assertion the
//! slice makes about behaviour is made here, against this type, because a
//! screen is a rendering of a state and the interesting decisions all happen
//! before the rendering.

use wamn_client::{ClientError, Cursor};

/// One purchase order row on the list screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PurchaseOrderRow {
    /// Server identity, carried so a selection can name it.
    pub id: String,
    /// Human-facing order number.
    pub number: String,
    /// Contract-declared status.
    pub status: String,
    /// Revision the row was read at — the value a later write must present.
    pub row_version: i64,
}

/// One line of the receipt screen, as the release projects it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptLine {
    /// The purchase order line this receives against.
    pub purchase_order_line_id: String,
    /// Line number within the order.
    pub line_number: i64,
    /// Item display name, joined from `item`.
    pub item_number: String,
    /// Quantity ordered, as text — quantities are `numeric` and are never
    /// parsed into a float here, because a receipt is a count of goods and a
    /// binary float is the wrong carrier for one.
    pub ordered: String,
    /// Quantity already received.
    pub received: String,
    /// Ordered minus received, as the projection computed it.
    pub remaining: String,
    /// What the operator has typed for this line, if anything.
    pub entered: String,
}

/// A location the operator may receive into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    /// Server identity.
    pub id: String,
    /// Human-facing code.
    pub code: String,
}

/// Which screen is in front of the operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Screen {
    /// The purchase order list.
    List,
    /// Receipt entry against one purchase order.
    Receipt,
}

/// What the operator is doing and what the server has said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppState {
    /// The screen in front of them.
    pub screen: Screen,
    /// Loaded purchase orders.
    pub orders: Vec<PurchaseOrderRow>,
    /// Cursor for the next page, when the server issued one.
    pub next_page: Option<Cursor>,
    /// Highlighted order.
    pub selected_order: Option<usize>,
    /// The order receipt entry is against.
    pub receiving: Option<PurchaseOrderRow>,
    /// Lines of the receipt screen.
    pub lines: Vec<ReceiptLine>,
    /// Highlighted line.
    pub selected_line: Option<usize>,
    /// Locations the operator may pick.
    pub locations: Vec<Location>,
    /// The picked location.
    pub selected_location: Option<usize>,
    /// The reference the operator typed for this receipt.
    pub receipt_reference: String,
    /// Whether a request is outstanding.
    pub pending: Option<String>,
    /// The last refusal, kept as the client decided it.
    pub failure: Option<ClientError>,
    /// The last success worth telling the operator about.
    pub notice: Option<String>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            screen: Screen::List,
            orders: Vec::new(),
            next_page: None,
            selected_order: None,
            receiving: None,
            lines: Vec::new(),
            selected_line: None,
            locations: Vec::new(),
            selected_location: None,
            receipt_reference: String::new(),
            pending: None,
            failure: None,
            notice: None,
        }
    }
}

impl AppState {
    /// The order the operator has highlighted.
    #[must_use]
    pub fn highlighted_order(&self) -> Option<&PurchaseOrderRow> {
        self.selected_order.and_then(|index| self.orders.get(index))
    }

    /// The line the operator has highlighted.
    #[must_use]
    pub fn highlighted_line(&self) -> Option<&ReceiptLine> {
        self.selected_line.and_then(|index| self.lines.get(index))
    }

    /// The location the operator has picked.
    #[must_use]
    pub fn picked_location(&self) -> Option<&Location> {
        self.selected_location
            .and_then(|index| self.locations.get(index))
    }
}
