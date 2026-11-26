//! Open object storage under an active connection generation.

use std::sync::Arc;

use wamn_connection::connection_facts::{ConnectionAnswer, ConnectionFacts};
use wamn_connection::connection_memory::ConnectionStatus;
use wamn_connection::invocation::ReleaseContext;
use wamn_connection_type::{BlobstoreDefinition, BlobstoreProvider, RuntimeType};
use wamn_registry::integration_code::IntegrationCode;
use wamn_storage::client::{self, StoreClient};

use crate::files::{FileStorageError, FileStorageErrorType, StoredFiles};
use crate::store::{BoundContainer, ConnectionAuthority};

/// Shares clients by connection identity and active generation.
pub struct ObjectStorage {
    stores: moka::sync::Cache<(uuid::Uuid, i64), Arc<StoreClient>>,
}

impl std::fmt::Debug for ObjectStorage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ObjectStorage")
            .finish_non_exhaustive()
    }
}

impl Default for ObjectStorage {
    fn default() -> Self {
        Self {
            stores: wamn_connection::connection_memory::bounded(),
        }
    }
}

impl ObjectStorage {
    /// Open a confined container for a declared guest binding.
    pub async fn container(
        &self,
        facts: &Arc<ConnectionFacts>,
        id: uuid::Uuid,
        integration_type: &IntegrationCode,
        read_only: bool,
    ) -> anyhow::Result<Arc<BoundContainer>> {
        let answer = active_answer(facts, &id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("connection disabled"))?;
        anyhow::ensure!(
            facts.runtime_type(&id) == Some(RuntimeType::Host),
            "not a native store"
        );
        let bound = BlobstoreDefinition::parse_for(
            &answer.generation.parameters,
            client::provider(integration_type)?,
        )?;
        let client = self.client(facts, id, &answer, &bound).await?;
        Ok(Arc::new(
            BoundContainer::new(
                client.store.clone(),
                bound.container,
                bound.prefix,
                ConnectionAuthority::Live {
                    facts: Arc::clone(facts),
                    id,
                    generation: answer.fact.active_generation,
                },
            )
            .with_read_only(read_only),
        ))
    }

    /// Open file storage after the native host authorizes its package binding.
    pub async fn open_files(
        &self,
        context: &ReleaseContext,
        id: uuid::Uuid,
        read_only: bool,
        deadline: tokio::time::Instant,
    ) -> Result<StoredFiles, FileStorageError> {
        let open = async {
            use anyhow::Context as _;
            let release = &context.release;
            let facts = &context.connections;
            anyhow::ensure!(
                facts.project_env_key()
                    == &wamn_registry::name::project_env_key(
                        &release.org,
                        &release.project,
                        &release.env
                    ),
                "file connection target mismatch"
            );
            let answer = active_answer(facts, &id)
                .await?
                .context("file connection disabled")?;
            let configuration = &answer.generation;
            anyhow::ensure!(
                matches!(configuration.integration_type.as_str(), "s3" | "gcs"),
                "not a file connection"
            );
            let bound = BlobstoreDefinition::parse_for(
                &configuration.parameters,
                client::provider(&configuration.integration_type)?,
            )?;
            let client = self.client(facts, id, &answer, &bound).await?;
            let container = BoundContainer::new(
                client.store.clone(),
                bound.container,
                bound.prefix,
                ConnectionAuthority::Live {
                    facts: Arc::clone(facts),
                    id,
                    generation: answer.fact.active_generation,
                },
            )
            .with_read_only(read_only);
            Ok::<_, anyhow::Error>(StoredFiles::new(
                container,
                client.signer.clone(),
                bound.cdn_host,
                &release.env,
                deadline,
            ))
        };
        let open = tokio::time::timeout(
            deadline.saturating_duration_since(tokio::time::Instant::now()),
            open,
        )
        .await
        .map_err(|error| FileStorageError::new(FileStorageErrorType::Deadline, "open", error))?;
        open.map_err(|error| {
            FileStorageError::new(
                FileStorageErrorType::Refused,
                "open",
                error.into_boxed_dyn_error(),
            )
        })
    }

    async fn client(
        &self,
        facts: &ConnectionFacts,
        id: uuid::Uuid,
        answer: &ConnectionAnswer,
        bound: &BlobstoreDefinition,
    ) -> anyhow::Result<Arc<StoreClient>> {
        use anyhow::Context as _;

        let key = (id, answer.fact.active_generation);
        if let Some(client) = self.stores.get(&key) {
            return Ok(client);
        }
        let secret = if bound.provider == BlobstoreProvider::S3 {
            Some(
                facts
                    .secret(
                        answer
                            .fact
                            .credential_id
                            .context("storage credential unavailable")?,
                        None,
                    )
                    .await?,
            )
        } else {
            None
        };
        self.stores
            .try_get_with(key, || {
                let client = match bound.provider {
                    BlobstoreProvider::S3 => client::s3(
                        bound.endpoint.as_deref().expect("validated s3 endpoint"),
                        &bound.container,
                        secret.as_deref().expect("s3 credential loaded").expose(),
                    )?,
                    BlobstoreProvider::Gcs => client::gcs(&bound.container)?,
                };
                Ok::<_, anyhow::Error>(Arc::new(client))
            })
            .map_err(|error| anyhow::anyhow!("storage client construction failed: {error}"))
    }
}

/// The fact of connection `id` while it is active.
async fn active_answer(
    facts: &ConnectionFacts,
    id: &uuid::Uuid,
) -> anyhow::Result<Option<Arc<ConnectionAnswer>>> {
    Ok(facts
        .answer(id)
        .await?
        .filter(|answer| answer.fact.status == ConnectionStatus::Active))
}
