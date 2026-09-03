//! The Receiving operator screens.
//!
//! Two screens — a purchase order list and receipt entry — over the routes the
//! release publishes. The interesting parts are [`model`] and [`reduce`]: a
//! plain state value and a pure function over it, with no transport, clock or
//! terminal inside either. That is what lets the behaviour be asserted without
//! a screen, and it is where nearly every assertion in this crate lives.
//!
//! [`request`] builds the `record_receipt` envelope, and [`screen`] renders a
//! state through the primitive controls. Neither decides anything the reducer
//! has not already decided.
//!
//! No terminal is entered here and no binary runs these screens yet. The
//! driver that would — raw mode, the alternate screen, an event stream —
//! currently lives with the developer client in services/ctl and moves out
//! when this crate takes a binary (wamn-10yt.5.9).

pub mod model;
pub mod reduce;
pub mod request;
pub mod screen;

pub use model::{AppState, Location, PurchaseOrderRow, ReceiptLine, Screen};
pub use reduce::{Event, reduce};
