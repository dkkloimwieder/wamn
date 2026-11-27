//! Shared Kubernetes transport, Secret storage, and bounded Job execution.

pub mod client;
pub mod deployment_components;
pub mod env_resources;
pub mod helm;
pub mod job;
pub mod kubernetes;
pub mod org_resources;
pub mod platform_env;
pub mod web_release;
pub mod worker_mounts;
pub mod kubernetes_api {
    pub use crate::client::KubernetesApi;
}

use std::error::Error as StdError;
use std::fmt;

/// A store failure with operation context and its source.
#[derive(Debug)]
pub struct Error {
    pub type_: Option<ErrorType>,
    pub operation: &'static str,
    pub reference: String,
    source: Box<dyn StdError + Send + Sync>,
}

/// A classified failure at the credential-store boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorType {
    /// The process has no in-cluster API endpoint.
    NoCluster,
    /// The project chart has not created the credential Secret.
    ProjectCredentialSecretMissing,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.type_ == Some(ErrorType::ProjectCredentialSecretMissing) {
            return write!(
                f,
                "the project chart has not created credential Secret {}",
                self.reference
            );
        }
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
        type_: None,
        operation,
        reference: reference.to_owned(),
        source: source.into(),
    }
}

/// A selected cluster and namespace for command line operations.
#[derive(Clone, Debug)]
pub struct Target {
    pub kubeconfig: std::path::PathBuf,
    pub context: String,
    pub namespace: String,
    pub client: client::KubernetesApi,
}

#[cfg(test)]
mod tests;
