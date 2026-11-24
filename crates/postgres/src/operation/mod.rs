//! PostgreSQL implementation of authorized application database operations.

mod identity;
mod native;
mod query;
mod transaction;
mod values;
mod write_log;

use std::sync::Arc;

use async_trait::async_trait;
use wamn_application::database::{
    Claim, DatabaseTarget, OperationSession, OperationStore, SqlError, SqlRows, SqlValue,
};
use wamn_application::session_identity::SessionIdentity;
use wamn_event::Causation;
use wamn_identity_protocol::AuthorityClass;
use wamn_package::canonical_name::CanonicalName;
use wamn_registry::{ProjectEnvKey, ProjectId};

use crate::WamnPostgres;

use crate::connection_guard::ConnectionGuard;
use crate::transaction_identity::prepare_identity;
pub use native::lock_operation_grants;
use query::{QueryTransaction, open_stream};
use transaction::Transaction;
use values::checkout_pg_error;
use write_log::WriteLog;

#[async_trait]
impl OperationStore for WamnPostgres {
    fn host_projects(&self) -> Box<dyn Iterator<Item = &ProjectId> + '_> {
        Box::new(WamnPostgres::host_projects(self))
    }

    async fn bind_host_project(
        &self,
        project: &ProjectId,
        environment: &ProjectEnvKey,
    ) -> anyhow::Result<()> {
        WamnPostgres::bind_host_project(self, project, environment).await
    }

    async fn read(
        &self,
        target: DatabaseTarget<'_>,
        identity: &SessionIdentity,
        sql: String,
        params: Vec<SqlValue>,
    ) -> Result<SqlRows, SqlError> {
        let (connection, policy) = self
            .checkout_dynamic(target.project, target.environment)
            .await
            .map_err(checkout_pg_error)?;
        let mut connection =
            ConnectionGuard::new(connection, Arc::clone(self.destroyed_connections()));
        let xid = prepare_identity(
            AuthorityClass::DynamicSql,
            true,
            &identity.into(),
            policy.statement_timeout_ms,
        )?
        .bind(connection.connection())
        .await?;
        open_stream(connection, QueryTransaction::Dynamic(xid), sql, params).await
    }

    async fn begin(
        &self,
        scope: &str,
        target: DatabaseTarget<'_>,
        identity: &SessionIdentity,
        read: bool,
        causation: Option<Causation>,
    ) -> Result<Arc<dyn OperationSession>, SqlError> {
        let (connection, policy, authority) = self
            .checkout_workload(scope, target.project, target.environment)
            .await
            .map_err(checkout_pg_error)?;
        let mut connection =
            ConnectionGuard::new(connection, Arc::clone(self.destroyed_connections()));
        let bound = prepare_identity(
            authority,
            read,
            &identity.into(),
            policy.statement_timeout_ms,
        )?;
        let xid = bound.bind(connection.connection()).await?;
        Ok(Arc::new(Transaction::new(
            connection,
            Arc::clone(self.destroyed_connections()),
            xid,
            causation,
        )))
    }

    async fn claim(
        &self,
        target: DatabaseTarget<'_>,
        operation: &CanonicalName,
        key: &str,
        request: Vec<u8>,
    ) -> Result<Claim, SqlError> {
        WriteLog {
            postgres: self,
            target,
        }
        .claim(operation, key, request)
        .await
    }

    async fn complete(
        &self,
        target: DatabaseTarget<'_>,
        operation: &CanonicalName,
        key: &str,
        result: String,
    ) -> Result<(), SqlError> {
        WriteLog {
            postgres: self,
            target,
        }
        .complete(operation, key, result)
        .await
    }

    async fn release(
        &self,
        target: DatabaseTarget<'_>,
        operation: &CanonicalName,
        key: &str,
    ) -> Result<(), SqlError> {
        WriteLog {
            postgres: self,
            target,
        }
        .release_claim(operation, key)
        .await
    }
}
