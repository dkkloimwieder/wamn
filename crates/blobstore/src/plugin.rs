//! Blob storage resolves bindings under the shared invocation authority.

use super::binding::BindingError;
use std::collections::HashSet;
use std::sync::Arc;
use wamn_authority::effect_span;
use wamn_authority::effect_span::{EffectIdentity, record_graph};
use wash_runtime::engine::ctx::{SharedCtx, extract_active_ctx};
use wash_runtime::engine::workload::WorkloadItem;
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wasmtime::component::Linker;
use wash_runtime::wit::{WitInterface, WitWorld};

/// The host registration for wasmcloud:blobstore.
pub const WAMN_BLOBSTORE_ID: &str = "wamn-blobstore";

use object_store::aws::AmazonS3Builder;
use object_store::gcp::GoogleCloudStorageBuilder;
use object_store::{ClientOptions, ObjectStore, RetryConfig};

use super::store::{BoundContainer, ConnectionAuthority};
use wamn_connection_type::{BlobstoreDefinition, BlobstoreProvider};

use wamn_connection::connection::ConnectionInvocation;
use wamn_connection::connection_facts::{ConnectionAnswer, ConnectionFacts};
use wamn_connection::connection_memory::ConnectionStatus;
use wamn_operation::OperationHost;

/// The WAMN blobstore host service.
pub struct WamnBlobstore {
    pub(super) postgres: Arc<OperationHost>,
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
    pub fn new(postgres: Arc<OperationHost>) -> Self {
        Self {
            postgres,
            stores: wamn_connection::connection_memory::bounded(),
        }
    }

    /// Read signing and effect authority from the invocation record.
    pub fn invocation(&self, component_id: &str) -> Option<ConnectionInvocation> {
        wamn_connection::invocation::invocation(&self.postgres.scopes, component_id)
    }

    /// Open file storage after the native host authorizes its package binding.
    pub async fn file_store(
        &self,
        context: &wamn_connection::invocation::ReleaseContext,
        id: uuid::Uuid,
        read_only: bool,
        deadline: tokio::time::Instant,
    ) -> Result<super::files::FileStore, super::files::FileStorageError> {
        use super::files::{FileStorageError, FileStorageErrorType, FileStore};
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
            Ok::<_, anyhow::Error>(FileStore::new(
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

    pub(super) async fn container_for(
        &self,
        component_id: &str,
        alias: &wamn_package::canonical_name::CanonicalName,
    ) -> Result<BoundContainer, BindingError> {
        let resolve = async {
            let context =
                wamn_connection::invocation::context(&self.postgres.scopes, component_id)?;
            let invocation = self
                .invocation(component_id)
                .ok_or_else(|| anyhow::anyhow!("no connection invocation"))?;
            let binding = invocation
                .connection(&context.release, alias)
                .filter(|binding| binding.interface == "wasmcloud:blobstore")
                .ok_or_else(|| anyhow::anyhow!("no declared blobstore binding"))?;
            let package = &invocation.package_id;
            let read_only = binding
                .mode
                .read_only(package, &binding.name)
                .ok_or_else(|| anyhow::anyhow!("no connection mode"))?;
            let id = binding.connection_id.parse()?;
            let facts = &context.connections;
            let answer = active_answer(facts, &id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("connection disabled"))?;
            let generation = facts.generation(&id, answer.fact.active_generation).await?;
            anyhow::ensure!(generation.runtime_type == "host", "not a native store");
            let bound = BlobstoreDefinition::parse_for(
                &generation.parameters,
                provider(&binding.integration_type)?,
            )?;
            let client = self.client(facts, id, &answer, &bound).await?;
            Ok::<_, anyhow::Error>(
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
            )
        }
        .await;
        resolve.map_err(|error| {
            tracing::warn!(%error, component_id, %alias, "blobstore authority refused");
            BindingError::unauthorized()
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

pub(super) fn blobstore_span(
    plugin: &WamnBlobstore,
    component_id: &str,
    operation: &'static str,
) -> tracing::Span {
    let make_span = |identity: Option<&wamn_application::session_identity::SessionIdentity>| {
        effect_span!(
            "wamn.blobstore",
            EffectIdentity {
                project_env_key: identity
                    .as_ref()
                    .and_then(|identity| identity.project_env_key.as_ref()),
                project: identity
                    .as_ref()
                    .and_then(|identity| identity
                        .project
                        .as_ref()
                        .map(wamn_registry::ProjectId::as_str))
                    .unwrap_or(""),
                component: component_id,
            },
            None,
            effect.operation = operation,
        )
    };
    let span = plugin
        .postgres
        .scopes
        .read::<wamn_operation::scope_registry::Authority, _>(component_id, |record| {
            make_span(Some(&record.identity))
        })
        .unwrap_or_else(|| make_span(None));
    let invocation = plugin.invocation(component_id);
    record_graph(
        &span,
        invocation
            .as_ref()
            .map(wamn_connection::connection::ConnectionInvocation::effect_graph),
    );
    span
}

#[async_trait::async_trait]
impl HostPlugin for WamnBlobstore {
    fn id(&self) -> &'static str {
        WAMN_BLOBSTORE_ID
    }

    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::from([
                WitInterface::from("wasmcloud:blobstore/types@0.1.0"),
                WitInterface::from("wasmcloud:blobstore/container@0.1.0"),
                WitInterface::from("wasmcloud:blobstore/blobstore@0.1.0"),
            ]),
            exports: HashSet::new(),
        }
    }
    async fn on_workload_item_bind<'a>(
        &self,
        item: &mut WorkloadItem<'a>,
        interfaces: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        if interfaces.contains("wasmcloud", "blobstore", &[]) {
            add_to_linker(item.linker()?)?;
        }
        Ok(())
    }
}

/// Wire the blobstore host functions into a linker.
pub fn add_to_linker(linker: &mut Linker<SharedCtx>) -> wash_runtime::wasmtime::Result<()> {
    super::bindings::wasmcloud::blobstore::types::add_to_linker::<SharedCtx, SharedCtx>(
        linker,
        extract_active_ctx,
    )?;
    super::bindings::wasmcloud::blobstore::container::add_to_linker::<SharedCtx, SharedCtx>(
        linker,
        extract_active_ctx,
    )?;
    super::bindings::wasmcloud::blobstore::blobstore::add_to_linker::<SharedCtx, SharedCtx>(
        linker,
        extract_active_ctx,
    )
}
