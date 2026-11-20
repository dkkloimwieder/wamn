//! Result signing is supplied by the host, after SQL commit or replay.

use wamn_catalog::SignedFileReads;
use wamn_catalog::canonical_name::CanonicalName;
use wamn_engine::ingress::AuthenticatedCaller;
use wamn_engine::operation::node_types::NodeError;

/// Retained operation authority; no open SQL transaction is required.
#[derive(Debug)]
pub struct ResultSigning<'a> {
    pub workload: &'a str,
    pub operation: &'a CanonicalName,
    pub caller: Option<&'a AuthenticatedCaller>,
    pub reads: &'a SignedFileReads,
    pub payload: String,
    pub deadline: tokio::time::Instant,
}

/// The host owns file storage without introducing a SQL to blobstore dependency.
#[async_trait::async_trait]
pub trait ResultSigner: Send + Sync + std::fmt::Debug {
    async fn sign(&self, request: ResultSigning<'_>) -> Result<String, NodeError>;
}
