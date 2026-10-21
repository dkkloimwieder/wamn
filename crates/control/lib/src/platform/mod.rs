//! The declared cluster and its platform install inputs (docs/plan/platform-install.md P1).

mod broker;
pub mod build;
pub mod document;
mod environments;
mod identity;
pub mod install;
pub mod job;
pub mod lock;
pub mod manifests;
mod packages;
pub mod project;
pub mod provider;
mod releases;
pub mod steps;
pub mod wired_release;
pub mod wiring_job;
