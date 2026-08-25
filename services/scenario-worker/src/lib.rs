//! Internal authoring management and durable inline-test persistence.
//!
//! MVP outcome: publish gate.

pub mod authoring;
pub mod management;
pub mod store;
mod test_set;

#[cfg(test)]
pub(crate) mod source_scan;
