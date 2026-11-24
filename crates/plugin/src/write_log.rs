//! Request claims and outcomes for operations that call external effects.

use std::sync::Arc;

use wamn_application::database::{DatabaseTarget, OperationStore};
use wamn_package::canonical_name::CanonicalName;
use wamn_registry::{ProjectEnvKey, ProjectId};

use super::types::into_guest_error;
use super::{OperationHost, PgError};

pub use wamn_application::database::Claim;

/// The complete success body of a generated delete, before request correlation.
pub const DELETED_RESULT: &str = r#"{"value":{"outcome":"deleted"}}"#;

#[derive(Debug, Clone)]
pub struct WriteLogStore {
    store: Arc<dyn OperationStore>,
    project: ProjectId,
    project_env_key: ProjectEnvKey,
}

impl WriteLogStore {
    pub fn new(
        postgres: &OperationHost,
        project: ProjectId,
        project_env_key: ProjectEnvKey,
    ) -> Self {
        Self {
            store: Arc::clone(&postgres.store),
            project,
            project_env_key,
        }
    }

    fn target(&self) -> DatabaseTarget<'_> {
        DatabaseTarget {
            project: &self.project,
            environment: &self.project_env_key,
        }
    }

    pub async fn claim(
        &self,
        operation: &CanonicalName,
        key: &str,
        request: Vec<u8>,
    ) -> Result<Claim, PgError> {
        self.store
            .claim(self.target(), operation, key, request)
            .await
            .map_err(into_guest_error)
    }

    pub async fn complete(
        &self,
        operation: &CanonicalName,
        key: &str,
        result: String,
    ) -> Result<(), PgError> {
        self.store
            .complete(self.target(), operation, key, result)
            .await
            .map_err(into_guest_error)
    }

    pub async fn release_claim(&self, operation: &CanonicalName, key: &str) -> Result<(), PgError> {
        self.store
            .release(self.target(), operation, key)
            .await
            .map_err(into_guest_error)
    }
}
