//! Named project connection reads and OAuth token refresh.

use std::collections::BTreeMap;

use uuid::Uuid;
use wamn_envelope::encryption::Envelope;
use wamn_registry::{
    ProjectId,
    connections::{ConnectionFact, ConnectionGeneration, CredentialReference},
};

use crate::oauth_token::{Available, Error, Fetch, Refresh};

/// Read connection data through the selected project's executor identity.
#[async_trait::async_trait]
pub trait ProjectConnections: Send + Sync {
    /// Read the active connection facts.
    async fn connection_fact(
        &self,
        project: &ProjectId,
        id: &Uuid,
    ) -> anyhow::Result<Option<ConnectionFact>>;

    /// Read one immutable generation.
    async fn connection_generation(
        &self,
        project: &ProjectId,
        id: &Uuid,
        generation: i64,
    ) -> anyhow::Result<Option<ConnectionGeneration>>;

    /// Read the reference and version for one immutable credential.
    async fn credential_reference(
        &self,
        project: &ProjectId,
        id: &Uuid,
    ) -> anyhow::Result<Option<CredentialReference>>;

    /// List the environment's connection names and IDs.
    async fn environment_connections(
        &self,
        project: &ProjectId,
    ) -> anyhow::Result<BTreeMap<String, Uuid>>;

    /// Read or refresh a token while the selected generation stays locked.
    async fn oauth_token(
        &self,
        project: &ProjectId,
        request: Refresh<'_>,
        envelope: &Envelope,
        fetch: Fetch<'_>,
    ) -> anyhow::Result<Result<Available, Error>>;
}
