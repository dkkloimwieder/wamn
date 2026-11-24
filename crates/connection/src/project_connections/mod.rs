//! Authorized connection reads and storage for OAuth tokens.

mod oauth;

use std::collections::BTreeMap;

use crate::oauth_token::{Available, Error, Fetch, Refresh};
use anyhow::Context as _;
use uuid::Uuid;
use wamn_envelope::encryption::Envelope;
use wamn_identity_protocol::AuthorityClass;
use wamn_registry::{
    ProjectId,
    connections::{ConnectionFact, ConnectionGeneration, CredentialReference},
};

use wamn_postgres::{ExecutorOpenError, Object, WamnPostgres, open_executor_read};

use self::store::ConnectionStore;

pub use oauth::write_token;

struct ProjectDatabase<'a> {
    pools: &'a WamnPostgres,
    project: &'a ProjectId,
}

impl ProjectDatabase<'_> {
    async fn read<T>(
        &self,
        context: &'static str,
        read: impl AsyncFnOnce(&mut ConnectionStore<'_>) -> Result<T, sqlx::Error>,
    ) -> anyhow::Result<T> {
        let mut guard = Box::pin(open_executor_read(self.pools, self.project))
            .await
            .map_err(ExecutorOpenError::into_read_error)?;
        let value = read(&mut ConnectionStore::new(guard.connection()))
            .await
            .context(context)?;
        guard.commit().await?;
        Ok(value)
    }

    async fn oauth_connection(&self) -> anyhow::Result<Object> {
        let (connection, _) = self
            .pools
            .checkout_platform(self.project, AuthorityClass::Platform)
            .await?;
        Ok(connection)
    }
}

pub(crate) async fn connection_fact(
    pools: &WamnPostgres,
    project: &ProjectId,
    id: &Uuid,
) -> anyhow::Result<Option<ConnectionFact>> {
    ProjectDatabase { pools, project }
        .read("read connection fact", async |store| store.fact(id).await)
        .await
}

pub(crate) async fn connection_generation(
    pools: &WamnPostgres,
    project: &ProjectId,
    id: &Uuid,
    generation: i64,
) -> anyhow::Result<Option<ConnectionGeneration>> {
    ProjectDatabase { pools, project }
        .read("read connection generation", async |store| {
            store.generation(id, generation).await
        })
        .await
}

pub(crate) async fn credential_reference(
    pools: &WamnPostgres,
    project: &ProjectId,
    id: &Uuid,
) -> anyhow::Result<Option<CredentialReference>> {
    ProjectDatabase { pools, project }
        .read("read credential reference", async |store| {
            store.credential(id).await
        })
        .await
}

pub(crate) async fn environment_connections(
    pools: &WamnPostgres,
    project: &ProjectId,
) -> anyhow::Result<BTreeMap<String, Uuid>> {
    ProjectDatabase { pools, project }
        .read("read environment connections", async |store| {
            store.environment().await
        })
        .await
}

pub(crate) async fn oauth_token(
    pools: &WamnPostgres,
    project: &ProjectId,
    request: Refresh<'_>,
    envelope: &Envelope,
    fetch: Fetch<'_>,
) -> anyhow::Result<Result<Available, Error>> {
    oauth::get_token(
        &ProjectDatabase { pools, project },
        &request,
        envelope,
        fetch,
    )
    .await
}

mod administration;
mod credential_cleanup;
mod references;
mod store;

pub use administration::{Administration, configure_integration_type, required_connection_version};
pub(crate) use administration::{Lookup, NewConnection, TokenChange};
pub use credential_cleanup::retired_credentials;
