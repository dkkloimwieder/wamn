//! The declared cluster and its platform install inputs (docs/plan/platform-install.md P1).

mod broker;
pub use broker::{kind_nats, render_event_secrets, single_namespace_nats};
pub mod build;
mod cdc;
mod check;
pub(crate) mod control;
pub mod document;
mod edge;
mod environments;
pub use environments::render_host_group;
pub mod identity;
pub mod install;
mod invite;
pub mod job;
pub mod job_contract;
#[cfg(feature = "test-util")]
pub mod journey;
pub mod lock;
pub mod manifests;
pub(crate) mod oauth;
mod packages;
pub mod pools;
pub mod project;
pub mod provider;
mod releases;
pub mod render;
pub mod steps;
mod web;
