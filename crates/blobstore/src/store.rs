//! The bound object contract exposed to the host plugin.

use crate::confinement::KeyRefusal;
use crate::intake::IntakeError;

/// Why a store operation failed.
///
/// Confinement and intake refusals are kept distinct from transport failures:
/// the first two are decided here before anything leaves the host, and
/// [`StoreError::Backend`] and [`StoreError::ResponseLost`] describe the remote.
#[derive(Debug)]
pub enum StoreError {
    /// The invocation deadline elapsed.
    Deadline,
    /// The guest-supplied key broke a containment rule.
    Confinement(KeyRefusal),
    /// The body exceeded the ceiling, or the drain did not establish that it was complete.
    Intake(IntakeError),
    /// The object does not exist.
    NoSuchObject,
    /// The backend refused or failed.
    Backend(BackendError),
    /// The store answered and its body did not arrive.
    ///
    /// Distinct from [`StoreError::Backend`] because the store found the object
    /// and began the read. The remedy is to
    /// re-read, never to write again (`wamn-b2m6.3`).
    ResponseLost(Box<dyn std::error::Error + Send + Sync>),
    /// The verb is refused by WAMN and will not be implemented in this shape.
    Refused {
        /// The verb refused.
        verb: &'static str,
        /// Why, in one sentence.
        reason: &'static str,
    },
}

impl core::fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Deadline => formatter.write_str("invocation deadline elapsed"),
            Self::Confinement(refusal) => {
                write!(formatter, "object key refused: {}", refusal.code())
            }
            Self::Intake(error) => write!(formatter, "{error}"),
            Self::NoSuchObject => formatter.write_str("no such object"),
            Self::Backend(error) => write!(formatter, "object store failed: {error}"),
            Self::ResponseLost(error) => write!(
                formatter,
                "the object was found and its body did not arrive, so re-read rather than \
                 write again: {error}"
            ),
            Self::Refused { verb, reason } => {
                write!(formatter, "{verb} is refused by this platform: {reason}")
            }
        }
    }
}

impl std::error::Error for StoreError {}

/// What the store knows about one object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectHead {
    /// Object size in bytes.
    pub size: u64,
    /// Last-modified time, nanoseconds since the Unix epoch.
    pub last_modified_unix_nanos: u64,
}

/// The backend outcomes distinguished by the guest contract and effect evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendErrorType {
    NotFound,
    AccessDenied,
    AlreadyExists,
    Timeout,
    Other,
}

/// An object storage failure with its original cause.
#[derive(Debug)]
pub struct BackendError {
    pub type_: BackendErrorType,
    source: Box<dyn std::error::Error + Send + Sync>,
}

impl BackendError {
    pub fn new(
        type_: BackendErrorType,
        source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Self {
        Self {
            type_,
            source: source.into(),
        }
    }
}

impl core::fmt::Display for BackendError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.source.fmt(formatter)
    }
}

impl std::error::Error for BackendError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

/// The object operations permitted through a bound container.
#[async_trait::async_trait]
pub trait ObjectContainer: core::fmt::Debug + Send + Sync {
    fn container(&self) -> &str;
    async fn put(&self, author_key: &str, body: Vec<u8>) -> Result<(), StoreError>;
    async fn get_range(
        &self,
        author_key: &str,
        start: u64,
        end: u64,
    ) -> Result<Vec<u8>, StoreError>;
    async fn has(&self, author_key: &str) -> Result<bool, StoreError>;
    async fn delete(&self, author_key: &str) -> Result<(), StoreError>;
    async fn head(&self, author_key: &str) -> Result<ObjectHead, StoreError>;
    async fn clear(&self) -> Result<(), StoreError>;
    async fn list(&self) -> Result<Vec<String>, StoreError>;
}
