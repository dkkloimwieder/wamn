//! Platform configuration, deployment resources, and service helpers.

pub mod artifact_plan;
pub mod bootstrap;
pub mod control;
pub mod deploy;
pub mod document;
pub mod edge;
pub mod identity;
pub(crate) mod job;
pub use job::system_schema;
pub mod manifests;
pub mod oauth;
pub mod operations;
pub mod pools;
pub mod project;
pub mod provider;
pub mod resources;
pub mod status;

pub mod tofu;
