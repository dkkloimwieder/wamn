//! Streaming file storage contracts.

use std::{error::Error, fmt, time::Duration};

use bytes::Bytes;
use futures_util::stream::BoxStream;
use tokio::io::AsyncRead;
use uuid::Uuid;

/// Maximum lifetime of a file signed URL.
pub const MAX_URL_LIFETIME: Duration = Duration::from_mins(15);

/// The category of a native storage failure, before boundary translation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStorageErrorType {
    Refused,
    NotFound,
    Body,
    Backend,
    Deadline,
}

/// A storage failure with its operation and original cause.
#[derive(Debug)]
pub struct FileStorageError {
    pub type_: FileStorageErrorType,
    pub operation: &'static str,
    source: Box<dyn Error + Send + Sync>,
}

impl FileStorageError {
    pub fn new(
        type_: FileStorageErrorType,
        operation: &'static str,
        source: impl Into<Box<dyn Error + Send + Sync>>,
    ) -> Self {
        Self {
            type_,
            operation,
            source: source.into(),
        }
    }
}
impl fmt::Display for FileStorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "file storage {} failed: {}", self.operation, self.source)
    }
}
impl Error for FileStorageError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref())
    }
}

/// Metadata computed from the exact bytes sent to staging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedFile {
    pub id: Uuid,
    pub blob_key: String,
    pub size: u64,
    pub sha256: wamn_digest::Digest,
}

/// Bytes returned by a confined streaming file read.
pub type FileStream = BoxStream<'static, Result<Bytes, Box<dyn Error + Send + Sync>>>;

/// File objects confined to the active storage generation.
#[async_trait::async_trait]
pub trait FileStore: fmt::Debug + Send + Sync {
    async fn stage(
        &self,
        id: Uuid,
        body: &mut (dyn AsyncRead + Unpin + Send),
    ) -> Result<StagedFile, FileStorageError>;
    async fn finish(&self, id: Uuid) -> Result<(), FileStorageError>;
    async fn get(&self, blob_key: &str) -> Result<FileStream, FileStorageError>;
    async fn delete(&self, blob_key: &str) -> Result<(), FileStorageError>;
    async fn expose(&self, blob_key: &str) -> Result<String, FileStorageError>;
    async fn unexpose(&self, blob_key: &str) -> Result<(), FileStorageError>;
    async fn signed_url(
        &self,
        blob_key: &str,
        lifetime: Duration,
    ) -> Result<String, FileStorageError>;
}
