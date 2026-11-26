//! Request claims and outcomes for operations that call external effects.

use std::sync::Arc;

use wamn_app::database::DatabaseTarget;
use wamn_package::canonical_name::CanonicalName;
use wamn_postgres::WamnPostgres;
use wamn_registry::{ProjectEnvKey, ProjectId};

use super::types::into_guest_error;
use super::{OperationHost, PgError, operation_sql};

pub use wamn_app::database::Claim;

/// The complete success body of a generated delete, before request correlation.
pub const DELETED_RESULT: &str = r#"{"value":{"outcome":"deleted"}}"#;

#[derive(Debug, Clone)]
pub struct WriteLogStore {
    store: Arc<WamnPostgres>,
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
            env: &self.project_env_key,
        }
    }

    pub async fn claim(
        &self,
        operation: &CanonicalName,
        key: &str,
        request: Vec<u8>,
    ) -> Result<Claim, PgError> {
        operation_sql::claim(&self.store, self.target(), operation, key, request)
            .await
            .map_err(into_guest_error)
    }

    pub async fn complete(
        &self,
        operation: &CanonicalName,
        key: &str,
        result: String,
    ) -> Result<(), PgError> {
        operation_sql::complete(&self.store, self.target(), operation, key, result)
            .await
            .map_err(into_guest_error)
    }

    pub async fn release_claim(&self, operation: &CanonicalName, key: &str) -> Result<(), PgError> {
        operation_sql::release(&self.store, self.target(), operation, key)
            .await
            .map_err(into_guest_error)
    }
}
