//! Result signing is supplied by the host, after SQL commit or replay.

use std::time::Instant;

use crate::ingress::AuthenticatedCaller;
use wamn_package::SignedFileReads;
use wamn_package::canonical_name::CanonicalName;

/// Retained operation authority; no open SQL transaction is required.
#[derive(Debug)]
pub struct ResultSigning<'a> {
    pub workload: &'a str,
    pub operation: &'a CanonicalName,
    pub caller: Option<&'a AuthenticatedCaller>,
    pub reads: &'a SignedFileReads,
    pub payload: String,
    pub deadline: Instant,
}

/// A terminal failure when signing an operation result.
#[derive(Debug)]
pub struct ResultSigningError {
    pub code: Option<String>,
    pub message: String,
}

impl std::fmt::Display for ResultSigningError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ResultSigningError {}

/// The host owns file storage without introducing a SQL to blobstore dependency.
#[async_trait::async_trait]
pub trait ResultSigner: Send + Sync + std::fmt::Debug {
    async fn sign(&self, request: ResultSigning<'_>) -> Result<String, ResultSigningError>;
}
