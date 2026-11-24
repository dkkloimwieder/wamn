//! Host ownership of an operation transaction and its known result.

use std::sync::Arc;

use wamn_application::database::{Claim, CommitFailure, DatabaseTarget};
use wamn_package::canonical_name::CanonicalName;

use super::operation_sql::Transaction;
use super::types::into_guest_error;
use super::{OperationHost, PgError};
use super::{operation_sql, scope_registry};

/// The host transaction of one request. The guest borrows its query handle.
#[derive(Debug, Clone)]
pub struct OperationTransaction {
    pub(super) session: Arc<Transaction>,
    known_result: Option<String>,
}

/// Whether the Postgres commit could have changed durable data.
#[derive(Debug)]
pub enum PgCommitFailure {
    RolledBack(PgError),
    Uncertain(PgError),
}

impl std::fmt::Display for PgCommitFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RolledBack(error) => write!(formatter, "the commit rolled back: {error:?}"),
            Self::Uncertain(error) => {
                write!(formatter, "the commit outcome is unknown: {error:?}")
            }
        }
    }
}

impl std::error::Error for PgCommitFailure {}

impl OperationTransaction {
    pub fn with_known_result(mut self, result: String) -> Self {
        self.known_result = Some(result);
        self
    }

    pub async fn begin_opaque(
        &self,
        operation: &CanonicalName,
        key: &str,
        request: Vec<u8>,
    ) -> Result<Claim, PgError> {
        let claim = self
            .session
            .claim(operation, key, request)
            .await
            .map_err(into_guest_error)?;
        if matches!(claim, Claim::New)
            && let Some(result) = &self.known_result
        {
            self.session.stage_finish(operation, key, result.to_owned());
        }
        Ok(claim)
    }

    pub(super) fn finish_opaque(&self, operation: &CanonicalName, key: &str, result: String) {
        if self.known_result.is_none() {
            self.session.stage_finish(operation, key, result);
        }
    }

    pub(super) async fn wal_position(&self) -> Result<String, PgError> {
        self.session.wal_position().await.map_err(into_guest_error)
    }

    pub async fn commit(&self) -> Result<(), PgCommitFailure> {
        self.session
            .commit()
            .await
            .map_err(|failure| match failure {
                CommitFailure::RolledBack(error) => {
                    PgCommitFailure::RolledBack(into_guest_error(error))
                }
                CommitFailure::Uncertain(error) => {
                    PgCommitFailure::Uncertain(into_guest_error(error))
                }
            })
    }

    pub async fn rollback(&self) -> Result<(), PgError> {
        self.session.rollback().await.map_err(into_guest_error)
    }
}

impl OperationHost {
    pub async fn begin_operation_transaction(
        &self,
        scope: &str,
    ) -> Result<OperationTransaction, PgError> {
        let identity = scope_registry::require_sql_identity(&self.scopes, scope)?;
        let project = scope_registry::project_for(&self.scopes, scope);
        let target = DatabaseTarget {
            project: &project,
            environment: identity
                .project_env_key
                .as_ref()
                .expect("required SQL identity"),
        };
        let read = scope_registry::active_read(&self.scopes, scope);
        let causation = if read {
            None
        } else {
            scope_registry::current_run_for(&self.scopes, scope)
        };
        let session = operation_sql::begin(&self.store, scope, target, &identity, read, causation)
            .await
            .map_err(into_guest_error)?;
        Ok(OperationTransaction {
            session,
            known_result: None,
        })
    }
}
