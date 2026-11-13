//! Shared Kubernetes transport, Secret storage, and bounded Job execution.

pub mod environment_state;
pub mod forward;
pub mod helm;
pub mod job;
pub mod kubectl;
pub mod kubernetes;
pub mod kubernetes_api;
pub mod secret;
#[cfg(feature = "test-util")]
pub mod test_util;

use std::error::Error as StdError;
use std::fmt;

/// The immutable database reference returned by a write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreVersion {
    pub store: &'static str,
    pub reference: String,
    pub version: String,
}

/// A store failure with operation context and its source.
#[derive(Debug)]
pub struct Error {
    pub operation: &'static str,
    pub reference: String,
    source: Box<dyn StdError + Send + Sync>,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} Kubernetes operation on {} failed",
            self.operation, self.reference
        )
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(self.source.as_ref())
    }
}

pub(crate) fn failure(
    operation: &'static str,
    reference: &str,
    source: impl Into<Box<dyn StdError + Send + Sync>>,
) -> Error {
    Error {
        operation,
        reference: reference.to_owned(),
        source: source.into(),
    }
}

/// Credential storage. Each write names its environment; values stay outside the database.
#[async_trait::async_trait]
pub trait Store: Send + Sync + fmt::Debug {
    async fn put(
        &self,
        scope: &wamn_control_registry::Triple,
        credential_id: uuid::Uuid,
        value: &[u8],
    ) -> Result<StoreVersion, Error>;
    async fn get(&self, reference: &str, version: &str) -> Result<Vec<u8>, Error>;
    async fn destroy(&self, reference: &str, version: &str) -> Result<(), Error>;
}

/// A selected cluster and namespace for command line operations.
#[derive(Clone, Debug)]
pub struct Target {
    pub kubeconfig: std::path::PathBuf,
    pub context: String,
    pub namespace: String,
}

#[cfg(test)]
mod tests;
