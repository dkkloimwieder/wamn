//! Authorized connection reads and storage for OAuth tokens.

mod oauth;

use std::collections::BTreeMap;

use anyhow::Context as _;
use uuid::Uuid;
use wamn_connection::{
    oauth_token::{Available, Error, Fetch, Refresh},
    project_connections::ProjectConnections,
};
use wamn_envelope::encryption::Envelope;
use wamn_identity_protocol::AuthorityClass;
use wamn_registry::{
    ProjectId,
    connections::{
        ConnectionFact, ConnectionGeneration, ConnectionStore as _, CredentialReference,
    },
};

use crate::{Registry, WamnPostgres, operation::open_executor_read};

pub use oauth::write_token;

struct ProjectDatabase<'a> {
    pools: &'a WamnPostgres,
    project: &'a ProjectId,
}

impl ProjectDatabase<'_> {
    async fn read<T>(
        &self,
        context: &'static str,
        read: impl AsyncFnOnce(&mut Registry<'_>) -> Result<T, sqlx::Error>,
    ) -> anyhow::Result<T> {
        let mut guard = open_executor_read(self.pools, self.project).await?;
        let value = read(&mut Registry::new(guard.connection()))
            .await
            .context(context)?;
        guard.commit().await?;
        Ok(value)
    }

    async fn oauth_connection(&self) -> anyhow::Result<crate::Object> {
        let (connection, _) = self
            .pools
            .checkout_platform(self.project, AuthorityClass::Platform)
            .await?;
        Ok(connection)
    }
}

#[async_trait::async_trait]
impl ProjectConnections for WamnPostgres {
    async fn connection_fact(
        &self,
        project: &ProjectId,
        id: &Uuid,
    ) -> anyhow::Result<Option<ConnectionFact>> {
        ProjectDatabase {
            pools: self,
            project,
        }
        .read("read connection fact", async |store| store.fact(id).await)
        .await
    }

    async fn connection_generation(
        &self,
        project: &ProjectId,
        id: &Uuid,
        generation: i64,
    ) -> anyhow::Result<Option<ConnectionGeneration>> {
        ProjectDatabase {
            pools: self,
            project,
        }
        .read("read connection generation", async |store| {
            store.generation(id, generation).await
        })
        .await
    }

    async fn credential_reference(
        &self,
        project: &ProjectId,
        id: &Uuid,
    ) -> anyhow::Result<Option<CredentialReference>> {
        ProjectDatabase {
            pools: self,
            project,
        }
        .read("read credential reference", async |store| {
            store.credential(id).await
        })
        .await
    }

    async fn environment_connections(
        &self,
        project: &ProjectId,
    ) -> anyhow::Result<BTreeMap<String, Uuid>> {
        ProjectDatabase {
            pools: self,
            project,
        }
        .read("read environment connections", async |store| {
            store.environment().await
        })
        .await
    }

    async fn oauth_token(
        &self,
        project: &ProjectId,
        request: Refresh<'_>,
        envelope: &Envelope,
        fetch: Fetch<'_>,
    ) -> anyhow::Result<Result<Available, Error>> {
        oauth::get_token(
            &ProjectDatabase {
                pools: self,
                project,
            },
            &request,
            envelope,
            fetch,
        )
        .await
    }
}

mod administration;
mod credential_cleanup;
mod references;

pub use administration::{Administration, required_connection_version};
pub use credential_cleanup::retired_credentials;
