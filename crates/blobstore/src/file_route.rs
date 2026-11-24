//! File metadata, refusals, and catalog access contracts.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;
use wamn_identity_protocol::PrincipalId;
use wamn_package::canonical_name::CanonicalName;
use wamn_registry::{ApplicationName, ProjectId};

use crate::files::{FileStorageError, FileStorageErrorType};

/// Exposure stored in the platform file catalog. Unknown literals refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Exposure {
    Private,
    Application,
    Public,
}

impl Exposure {
    pub fn literal(self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Application => "application",
            Self::Public => "public",
        }
    }
}

/// Metadata returned by the short catalog read.
#[derive(Debug, Clone)]
pub struct FileRecord {
    pub id: Uuid,
    pub connection_id: String,
    pub media_type: String,
    pub size: i64,
    pub exposure: Exposure,
    pub url: Option<String>,
    pub blob_key: String,
}

/// The refusal literals of the file operation contracts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileRefusal {
    InvalidInput,
    PermissionDenied,
    NotFound,
    Private,
    BindingRefused,
    Referenced,
    CdnHostRequired,
}

impl FileRefusal {
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::PermissionDenied => "permission_denied",
            Self::NotFound => "file_not_found",
            Self::Private => "file_private",
            Self::BindingRefused => "binding_refused",
            Self::Referenced => "file_referenced",
            Self::CdnHostRequired => "cdn_host_required",
        }
    }
    pub fn status(self) -> u16 {
        match self {
            Self::InvalidInput => 400,
            Self::PermissionDenied | Self::Private | Self::BindingRefused => 403,
            Self::NotFound => 404,
            Self::Referenced | Self::CdnHostRequired => 409,
        }
    }
}

/// A file failure with the input field, refusal, and original cause.
#[derive(Debug)]
pub struct FileError {
    pub refusal: Option<FileRefusal>,
    pub field: &'static str,
    source: Option<anyhow::Error>,
}

impl FileError {
    /// Retain the underlying failure for a declared refusal.
    pub fn with_source(mut self, source: impl Into<anyhow::Error>) -> Self {
        self.source = Some(source.into());
        self
    }
}

impl fmt::Display for FileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.source {
            Some(error) => write!(f, "file {} failed: {error}", self.field),
            None => write!(f, "file {} refused: {:?}", self.field, self.refusal),
        }
    }
}
impl std::error::Error for FileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_ref().map(std::convert::AsRef::as_ref)
    }
}
pub fn refused(refusal: FileRefusal, field: &'static str) -> FileError {
    FileError {
        refusal: Some(refusal),
        field,
        source: None,
    }
}
pub fn failed(field: &'static str, source: impl Into<anyhow::Error>) -> FileError {
    FileError {
        refusal: None,
        field,
        source: Some(source.into()),
    }
}
impl From<FileStorageError> for FileError {
    fn from(error: FileStorageError) -> Self {
        let refusal = match error.type_ {
            FileStorageErrorType::NotFound => Some(FileRefusal::NotFound),
            FileStorageErrorType::Refused => Some(FileRefusal::BindingRefused),
            FileStorageErrorType::Body => Some(FileRefusal::InvalidInput),
            FileStorageErrorType::Backend | FileStorageErrorType::Deadline => None,
        };
        Self {
            refusal,
            field: "binding",
            source: Some(error.into()),
        }
    }
}

/// One generation selected by the targeted registry reader, never by the caller.
#[derive(Debug, Clone)]
pub struct FileConnection {
    pub id: String,
    pub generation: i64,
    pub definition: Value,
    pub read_only: bool,
}

/// The registry authority needed by native file handlers.
#[async_trait::async_trait]
pub trait FileConnections: Send + Sync + fmt::Debug {
    /// Enumerate immutable store generations using the same targeted registry reader.
    async fn generations(&self) -> Result<Vec<FileConnection>, FileError> {
        Err(failed(
            "collector",
            anyhow::anyhow!("store generation listing unavailable"),
        ))
    }

    /// Resolve an application's binding, or an existing file's active generation.
    async fn lookup(
        &self,
        application: &wamn_registry::ApplicationName,
        binding: Option<&wamn_package::canonical_name::CanonicalName>,
        pinned: Option<&str>,
    ) -> Result<FileConnection, FileError>;
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileRequest {
    pub application: wamn_registry::ApplicationName,
    pub file_id: Uuid,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeleteRequest {
    pub application: wamn_registry::ApplicationName,
    pub file_id: Uuid,
    pub binding: wamn_package::canonical_name::CanonicalName,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExposeRequest {
    pub application: wamn_registry::ApplicationName,
    pub file_id: Uuid,
    pub binding: wamn_package::canonical_name::CanonicalName,
    pub exposure: Exposure,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UploadQuery {
    pub binding: wamn_package::canonical_name::CanonicalName,
    pub name: String,
}

pub fn valid_media_type(value: &str) -> bool {
    !value.is_empty()
        && value.is_ascii()
        && !value.bytes().any(|byte| byte < 32 || byte == 127)
        && value.contains('/')
}

/// Metadata for the row inserted after an upload is staged.
#[derive(Debug)]
pub struct FileInsert<'a> {
    pub id: Uuid,
    pub application: &'a ApplicationName,
    pub connection_id: &'a str,
    pub name: &'a str,
    pub media_type: &'a str,
    pub size: i64,
    pub sha256: &'a wamn_digest::Digest,
    pub blob_key: &'a str,
}

/// File rows and session locks for native file operations.
#[async_trait::async_trait]
pub trait FileCatalog: fmt::Debug + Send + Sync {
    /// Hold a session lock until the returned guard is dropped.
    async fn lock(&self, project: &ProjectId, id: Uuid) -> Result<Box<dyn Send>, FileError>;

    /// Read one row under the caller's administration authority.
    async fn record(
        &self,
        project: &ProjectId,
        principal: &PrincipalId,
        operation: &CanonicalName,
        application: &ApplicationName,
        id: Uuid,
    ) -> Result<FileRecord, FileError>;

    /// Commit the staged file's metadata before promoting its bytes.
    async fn insert(
        &self,
        project: &ProjectId,
        principal: &PrincipalId,
        operation: &CanonicalName,
        file: FileInsert<'_>,
    ) -> Result<(), FileError>;

    /// Delete one row under the caller's administration authority.
    async fn delete(
        &self,
        project: &ProjectId,
        principal: &PrincipalId,
        operation: &CanonicalName,
        application: &ApplicationName,
        id: Uuid,
    ) -> Result<(), FileError>;

    /// Return query errors in the inner result after a confirmed rollback.
    async fn set_exposure(
        &self,
        project: &ProjectId,
        principal: &PrincipalId,
        operation: &CanonicalName,
        file: FileExposure<'_>,
    ) -> Result<Result<(), FileError>, FileError>;
}

/// Exposure and URL to write for one file.
#[derive(Debug)]
pub struct FileExposure<'a> {
    pub application: &'a ApplicationName,
    pub id: Uuid,
    pub exposure: Exposure,
    pub url: Option<&'a str>,
}
