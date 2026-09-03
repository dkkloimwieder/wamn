//! The Receiving operator terminal.
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

pub mod model;
pub mod reduce;
pub mod request;
pub mod screen;

pub use model::{AppState, Location, PurchaseOrderRow, ReceiptLine, Screen};
pub use reduce::{Event, reduce};
