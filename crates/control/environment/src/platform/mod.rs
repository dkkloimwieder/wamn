//! Platform configuration, deployment resources, and service helpers.

pub mod bootstrap;
pub mod broker;
pub use broker::{render_event_secrets, single_namespace_nats};
pub mod artifact_plan;
pub mod control;
pub mod deploy;
pub mod document;
pub mod edge;
pub mod identity;
pub(crate) mod job;
pub mod manifests;
pub mod oauth;
pub mod pools;
pub mod project;
pub mod provider;
pub mod resources;
pub mod status;
pub mod web;
