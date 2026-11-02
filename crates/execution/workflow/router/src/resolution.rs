//! The resolved wiring and its immutable release facts.

use std::sync::Arc;

use crate::wiring::Wiring;

/// The immutable wiring one resolution produced.
#[derive(Debug, Clone)]
pub struct ActiveWiring<T = ()> {
    /// The exact version a delivery reports as its
    /// `wiring-version` and scopes an authored dedup key by.
    pub version: u32,
    /// RFC 8785 digest of the immutable wiring document.
    pub graph_hash: Arc<str>,
    pub wiring: Arc<Wiring>,
    /// Host-owned immutable facts resolved in the same store snapshot as the
    /// graph. The router never inspects them.
    pub facts: Arc<T>,
}
