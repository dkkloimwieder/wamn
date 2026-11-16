//! Blob storage resolves bindings under the shared invocation authority.

use std::sync::Arc;

use object_store::aws::AmazonS3Builder;
use object_store::gcp::GoogleCloudStorageBuilder;
use object_store::{ClientOptions, ObjectStore, RetryConfig};

use super::store::{BoundContainer, ConnectionAuthority};
use wamn_catalog::{BlobstoreDefinition, BlobstoreProvider};

use wamn_connection::connection::ConnectionInvocation;
use wamn_connection::connection_facts::ConnectionFacts;
use wamn_connection::connection_memory::{ConnectionFact, ConnectionStatus};
use wamn_sql::WamnPostgres;

/// The WAMN blobstore host service.
pub struct WamnBlobstore {
    pub(super) postgres: Arc<WamnPostgres>,
    stores: moka::sync::Cache<(uuid::Uuid, i64), Arc<StoreClient>>,
}

impl std::fmt::Debug for WamnBlobstore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The vault is never rendered.
        formatter
            .debug_struct("WamnBlobstore")
            .finish_non_exhaustive()
    }
}

impl WamnBlobstore {
    /// Build the plugin over the connection-authority reader and the
    /// connection facts.
    pub fn new(postgres: Arc<WamnPostgres>) -> Self {
        Self {
            postgres,
            stores: wamn_connection::connection_facts::bounded(),
        }
    }

    /// Read signing and effect authority from the invocation record.
    pub fn invocation(&self, component_id: &str) -> Option<ConnectionInvocation> {
        wamn_connection::invocation::invocation(&self.postgres.scopes, component_id)
    }

    /// The absolute deadline of the admitted invocation.
    pub fn invocation_deadline(&self, component_id: &str) -> Option<tokio::time::Instant> {
        self.postgres
            .scopes
            .execution(component_id)
            .map(|execution| execution.deadline())
    }

    /// Open file storage after the native host authorizes its package binding.
    pub async fn file_store(
        &self,
        context: &wamn_connection::invocation::ReleaseContext,
        connection_name: &str,
        read_only: bool,
        deadline: tokio::time::Instant,
    ) -> Result<super::files::FileStore, super::files::FileStorageError> {
        use super::files::{FileStorageError, FileStorageErrorType, FileStore};
        let open = async {
            use anyhow::Context as _;
            let release = &context.release;
            let facts = &context.connections;
            anyhow::ensure!(
                facts.project_env_key() == &release.target().project_env_key,
                "file connection target mismatch"
            );
            let id = *facts
                .env_connections()
                .await?
                .connections
                .get(connection_name)
                .context("no file connection")?;
            let fact = active_fact(facts, &id)
                .await?
                .context("file connection disabled")?;
            let configuration = facts.generation(&id, fact.active_generation).await?;
            anyhow::ensure!(
                configuration.runtime_type == "host"
                    && matches!(configuration.integration_type.as_str(), "s3" | "gcs"),
                "not a file connection"
            );
            let version = facts
                .release(&release.release().manifest_digest)
                .await?
                .version(&id);
            facts
                .version(&configuration.integration_type, version)
                .await?;
            let bound = BlobstoreDefinition::parse_for(
                &configuration.parameters,
                provider(&configuration.integration_type)?,
            )?;
            let client = self.client(facts, id, &fact, &bound).await?;
            let container = BoundContainer::new(
                Arc::clone(&client.store),
                bound.container,
                bound.prefix,
                ConnectionAuthority::Live {
                    facts: Arc::clone(facts),
                    id,
                    generation: fact.active_generation,
                },
            )
            .with_read_only(read_only);
            Ok::<_, anyhow::Error>(FileStore::new(
                container,
                Arc::clone(&client.signer),
                bound.cdn_host,
                &release.target().environment,
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
        facts: &Arc<ConnectionFacts>,
        id: uuid::Uuid,
        fact: &ConnectionFact,
        bound: &BlobstoreDefinition,
    ) -> anyhow::Result<Arc<StoreClient>> {
        let key = (id, fact.active_generation);
        if let Some(client) = self.stores.get(&key) {
            return Ok(client);
        }
        let secret = if bound.provider == BlobstoreProvider::S3 {
            let credential = fact
                .credential_id
                .ok_or_else(|| anyhow::anyhow!("storage credential unavailable"))?;
            Some(facts.secret(&credential).await?)
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
                            secret.as_ref().expect("s3 credential loaded").expose(),
                        )?;
                        StoreClient {
                            store: Arc::<object_store::aws::AmazonS3>::clone(&client),
                            signer: client,
                        }
                    }
                    BlobstoreProvider::Gcs => {
                        let client = build_gcs_store(&bound.container)?;
                        StoreClient {
                            store: Arc::<object_store::gcp::GoogleCloudStorage>::clone(&client),
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
pub(crate) struct StoreClient {
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
async fn active_fact(
    facts: &ConnectionFacts,
    id: &uuid::Uuid,
) -> anyhow::Result<Option<ConnectionFact>> {
    Ok(facts
        .connection(id)
        .await?
        .filter(|fact| fact.status == ConnectionStatus::Active))
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
                .with_timeout(wamn_engine::engine::MAX_HOST_CALL_DURATION),
        )
        .with_retry(RetryConfig {
            retry_timeout: wamn_engine::engine::MAX_HOST_CALL_DURATION,
            ..Default::default()
        })
        .build()?;
    Ok(Arc::new(store))
}

/// Build Cloud Storage with pod metadata credentials and IAM signing.
/// Explicit providers prevent fallback to stored ADC keys.
fn build_gcs_store(container: &str) -> anyhow::Result<Arc<object_store::gcp::GoogleCloudStorage>> {
    let (credentials, signing_credentials) = wamn_connection::pod_identity::providers()?;
    let store = GoogleCloudStorageBuilder::new()
        .with_credentials(credentials)
        .with_signing_credentials(signing_credentials)
        .with_bucket_name(container)
        .with_client_options(
            ClientOptions::new().with_timeout(wamn_engine::engine::MAX_HOST_CALL_DURATION),
        )
        .with_retry(RetryConfig {
            retry_timeout: wamn_engine::engine::MAX_HOST_CALL_DURATION,
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
