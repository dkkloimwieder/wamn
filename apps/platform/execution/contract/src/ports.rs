//! Persisted port and entry names.
//!
//! These are stored bytes, not display strings: an edge written with one
//! spelling is read back and filtered against another, so a rename here is a
//! data migration. `crates/platform/runtime/tests/port_constant_agreement.rs`
//! guards the port names against the router's independent declarations.

/// The default (main) output port of a node.
pub const MAIN_PORT: &str = "main";

/// The reserved output port a node emits on when it errors — the "error path"
/// (5.2). Edges from this port route failures without aborting the run.
pub const ERROR_PORT: &str = "error";

/// The unique engine-reserved entry kind an exposed attachment resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Request,
    Event,
}
