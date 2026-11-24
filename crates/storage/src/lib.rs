//! Object storage implementations for the WAMN host.

mod client;
pub mod files;
mod pod_identity;
mod store;

pub use client::{ObjectStorage, health};

pub mod web_artifact;
