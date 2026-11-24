//! File and confined object storage contracts.
//!
//! WAMN is the single runtime owner of `wasmcloud:blobstore@0.1.0`. Upstream's
//! `wasi_blobstore` providers are prior-art source only and are never
//! registered — and, since the `wasi-blobstore` cargo feature is not enabled,
//! they are not even compiled, so a second runtime for this contract is a
//! structural impossibility rather than a convention (wamn-jpxo).
//!
//! This crate holds the parts that decide, before any request exists, what a
//! guest is allowed to do:
//!
//! * [`confinement`] turns the descriptor's environment-owned bucket and
//!   prefix into refusals — an author may name an object, never a container.
//! * [`intake`] bounds a body and requires its complete contents before a commit,
//!   so a truncated stream cannot overwrite a good object under a
//!   deterministic key.
//!
//! # Two object-store semantics decided here, not inherited
//!
//! **`..` is refused as a SEGMENT, never as a substring.** A substring test
//! would also refuse the legitimate key `report..2026.zpl`, and a containment
//! rule that refuses valid names teaches authors to work around it.
//!
//! **The prefix separator is never doubled.** S3 treats `a//b` and `a/b` as
//! different keys, so a trailing slash on the environment-owned prefix would
//! put one logical object under two names — which breaks the deterministic-key
//! overwrite rule §2c depends on. Both prefix spellings resolve identically,
//! and a test says so.

pub mod binding;
pub mod confinement;
pub mod file_route;
pub mod files;
pub mod intake;
pub mod store;

use std::sync::Arc;

use tokio::time::Instant;
use uuid::Uuid;
use wamn_connection::connection_facts::ConnectionFacts;
use wamn_connection::invocation::ReleaseContext;
use wamn_registry::integration_code::IntegrationCode;

use crate::files::{FileStorageError, FileStore};
use crate::store::ObjectContainer;

/// Open storage under the active generation and the caller's binding permissions.
#[async_trait::async_trait]
pub trait Storage: std::fmt::Debug + Send + Sync {
    /// Open a confined container for a declared guest binding.
    async fn container(
        &self,
        facts: &Arc<ConnectionFacts>,
        id: Uuid,
        integration_type: &IntegrationCode,
        read_only: bool,
    ) -> anyhow::Result<Arc<dyn ObjectContainer>>;

    /// Open file storage after the native handler authorizes the binding.
    async fn file_store(
        &self,
        context: &ReleaseContext,
        id: Uuid,
        read_only: bool,
        deadline: Instant,
    ) -> Result<Box<dyn FileStore>, FileStorageError>;
}
