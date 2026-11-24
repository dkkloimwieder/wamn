//! S3 and GCS clients and bound storage operations.

use std::sync::Arc;

use object_store::aws::AmazonS3Builder;
use object_store::gcp::GoogleCloudStorageBuilder;
use object_store::{ClientOptions, ObjectStore, RetryConfig};
use wamn_blobstore::Storage;
use wamn_blobstore::files::{FileStorageError, FileStorageErrorType, FileStore};
use wamn_blobstore::store::ObjectContainer;
use wamn_connection::connection_facts::{ConnectionAnswer, ConnectionFacts};
use wamn_connection::connection_memory::ConnectionStatus;
use wamn_connection::invocation::ReleaseContext;
use wamn_connection_type::{BlobstoreDefinition, BlobstoreProvider};
use wamn_registry::integration_code::IntegrationCode;

use crate::files::StoredFiles;
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

#[async_trait::async_trait]
impl Storage for ObjectStorage {
    async fn file_store(
        &self,
        context: &ReleaseContext,
        id: uuid::Uuid,
        read_only: bool,
        deadline: tokio::time::Instant,
    ) -> Result<Box<dyn FileStore>, FileStorageError> {
        Ok(Box::new(
            self.open_files(context, id, read_only, deadline).await?,
        ))
    }

    async fn container(
        &self,
        facts: &Arc<ConnectionFacts>,
        id: uuid::Uuid,
        integration_type: &IntegrationCode,
        read_only: bool,
    ) -> anyhow::Result<Arc<dyn ObjectContainer>> {
        let answer = active_answer(facts, &id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("connection disabled"))?;
        let generation = facts.generation(&id, answer.fact.active_generation).await?;
        anyhow::ensure!(generation.runtime_type == "host", "not a native store");
        let bound =
            BlobstoreDefinition::parse_for(&generation.parameters, provider(integration_type)?)?;
        let client = self.client(facts, id, &answer, &bound).await?;
        Ok(Arc::new(
            BoundContainer::new(
                Arc::clone(&client.store),
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
}

impl ObjectStorage {
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
                        &release.environment
                    ),
                "file connection target mismatch"
            );
            let answer = active_answer(facts, &id)
                .await?
                .context("file connection disabled")?;
            let configuration = facts.generation(&id, answer.fact.active_generation).await?;
            anyhow::ensure!(
                matches!(configuration.integration_type.as_str(), "s3" | "gcs"),
                "not a file connection"
            );
            let bound = BlobstoreDefinition::parse_for(
                &configuration.parameters,
                provider(&configuration.integration_type)?,
            )?;
            let client = self.client(facts, id, &answer, &bound).await?;
            let container = BoundContainer::new(
                Arc::clone(&client.store),
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
                Arc::clone(&client.signer),
                bound.cdn_host,
                &release.environment,
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
                    BlobstoreProvider::S3 => {
                        let client = build_s3_store(
                            bound.endpoint.as_deref().expect("validated s3 endpoint"),
                            &bound.container,
                            secret.as_deref().expect("s3 credential loaded").expose(),
                        )?;
                        let store = Arc::clone(&client);
                        StoreClient {
                            store,
                            signer: client,
                        }
                    }
                    BlobstoreProvider::Gcs => {
                        let client = build_gcs_store(&bound.container)?;
                        let store = Arc::clone(&client);
                        StoreClient {
                            store,
                            signer: client,
                        }
                    }
                };
                Ok::<_, anyhow::Error>(Arc::new(client))
            })
            .map_err(|error| anyhow::anyhow!("storage client construction failed: {error}"))
    }
}

// Shared clients cap requests at the host limit; callers enforce earlier deadlines.
struct StoreClient {
    store: Arc<dyn ObjectStore>,
    signer: Arc<dyn object_store::signer::Signer>,
}

fn provider(
    integration_type: &wamn_registry::integration_code::IntegrationCode,
) -> anyhow::Result<BlobstoreProvider> {
    match integration_type.as_str() {
        "s3" => Ok(BlobstoreProvider::S3),
        "gcs" => Ok(BlobstoreProvider::Gcs),
        _ => anyhow::bail!("not a storage integration"),
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

/// Build the S3 client for one binding.
///
/// The secret enters HERE and nowhere else: it is handed straight to the
/// signer and is never stored on [`BoundContainer`], never logged, and never
/// reachable from a guest-visible structure.
fn build_s3_store(
    endpoint: &str,
    container: &str,
    secret: &str,
) -> anyhow::Result<Arc<object_store::aws::AmazonS3>> {
    #[derive(serde::Deserialize)]
    struct Credential {
        #[serde(rename = "ACCESS_KEY_ID")]
        access_key: String,
        #[serde(rename = "ACCESS_SECRET_KEY")]
        secret_key: String,
    }
    let credential: Credential = serde_json::from_str(secret)?;
    let store = AmazonS3Builder::new()
        .with_endpoint(endpoint)
        .with_bucket_name(container)
        .with_access_key_id(credential.access_key)
        .with_secret_access_key(credential.secret_key)
        .with_region("us-east-1")
        .with_virtual_hosted_style_request(false)
        .with_client_options(
            ClientOptions::new()
                .with_allow_http(endpoint.starts_with("http://"))
                .with_timeout(wamn_application::MAX_HOST_CALL_DURATION),
        )
        .with_retry(RetryConfig {
            retry_timeout: wamn_application::MAX_HOST_CALL_DURATION,
            ..Default::default()
        })
        .build()?;
    Ok(Arc::new(store))
}

/// Build Cloud Storage with pod metadata credentials and IAM signing.
/// Explicit providers prevent fallback to stored ADC keys.
fn build_gcs_store(container: &str) -> anyhow::Result<Arc<object_store::gcp::GoogleCloudStorage>> {
    let (credentials, signing_credentials) = crate::pod_identity::providers()?;
    let store = GoogleCloudStorageBuilder::new()
        .with_credentials(credentials)
        .with_signing_credentials(signing_credentials)
        .with_bucket_name(container)
        .with_client_options(
            ClientOptions::new().with_timeout(wamn_application::MAX_HOST_CALL_DURATION),
        )
        .with_retry(RetryConfig {
            retry_timeout: wamn_application::MAX_HOST_CALL_DURATION,
            ..Default::default()
        })
        .build()?;
    Ok(Arc::new(store))
}

/// List the first object under candidate storage coordinates without writing facts.
pub async fn health(
    integration_type: &wamn_registry::integration_code::IntegrationCode,
    parameters: &serde_json::Value,
    credential: Option<&str>,
) -> anyhow::Result<()> {
    use object_store::list::{PaginatedListOptions, PaginatedListStore};

    let bound = BlobstoreDefinition::parse_for(parameters, provider(integration_type)?)?;
    let prefix = object_store::path::Path::parse(&bound.prefix)?;
    let store: Arc<dyn PaginatedListStore> = match bound.provider {
        BlobstoreProvider::Gcs => build_gcs_store(&bound.container)?,
        BlobstoreProvider::S3 => build_s3_store(
            bound.endpoint.as_deref().expect("validated s3 endpoint"),
            &bound.container,
            credential.ok_or_else(|| anyhow::anyhow!("storage credential unavailable"))?,
        )?,
    };
    let prefix = format!("{}/", prefix.as_ref().trim_end_matches('/'));
    store
        .list_paginated(
            Some(&prefix),
            PaginatedListOptions {
                max_keys: Some(1),
                ..PaginatedListOptions::default()
            },
        )
        .await?;
    Ok(())
}
