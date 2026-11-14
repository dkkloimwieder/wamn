//! Blob storage resolves bindings under the shared invocation authority.

use std::collections::HashSet;
use std::sync::Arc;

use object_store::ObjectStore;
use object_store::aws::AmazonS3Builder;
use object_store::gcp::GoogleCloudStorageBuilder;
use wash_runtime::engine::ctx::{SharedCtx, extract_active_ctx};
use wash_runtime::plugin::HostPlugin;
use wash_runtime::wasmtime::component::Linker;
use wash_runtime::wit::{WitInterface, WitWorld};

use wamn_capability_authority::effect_span::{EffectIdentity, effect_span, record_wiring};

use super::binding::{self, BindingError};
use super::store::{BoundContainer, ConnectionAuthority};
use wamn_catalog::{BlobstoreDefinition, BlobstoreProvider};

use wamn_capability_authority::connection::ConnectionInvocation;
use wamn_connections::connection_facts::ConnectionFacts;
use wamn_connections::connection_memory::{ConnectionFact, ConnectionStatus};
use wamn_postgres_capability::WamnPostgres;

/// Plugin id, as the host registry knows it.
pub const WAMN_BLOBSTORE_ID: &str = "wamn-blobstore";

/// The WAMN blobstore capability.
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
            stores: wamn_connections::connection_facts::bounded(),
        }
    }

    /// Read signing and effect authority from the invocation record.
    pub fn invocation(&self, component_id: &str) -> Option<ConnectionInvocation> {
        self.postgres.invocation(component_id)
    }

    /// Open file storage after the native host authorizes its package binding.
    pub async fn file_store(
        &self,
        context: &wamn_postgres_capability::invocation::ReleaseContext,
        connection_name: &str,
        read_only: bool,
    ) -> Result<super::files::FileStore, super::files::FileStorageError> {
        use super::files::{FileStorageError, FileStorageErrorType, FileStore};
        let open = async {
            use anyhow::Context as _;
            let release = &context.release;
            let facts = &context.connections;
            anyhow::ensure!(
                facts.project_env_key() == release.target().project_env_key,
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
                .release(release.release().manifest_digest.as_str())
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
            FileStore::new(
                container,
                Arc::clone(&client.signer),
                bound.cdn_host,
                &release.target().environment,
            )
            .map_err(anyhow::Error::from)
        }
        .await;
        open.map_err(|error| {
            FileStorageError::new(
                FileStorageErrorType::Refused,
                "open",
                error.into_boxed_dyn_error(),
            )
        })
    }

    /// Resolve one declared alias under the shared invocation authority.
    pub(super) async fn container_for(
        &self,
        component_id: &str,
        store_alias: &str,
    ) -> Result<BoundContainer, BindingError> {
        let context =
            wamn_postgres_capability::invocation::context(&self.postgres.scopes, component_id)
                .map_err(|_| BindingError::unauthorized())?;
        let facts = &context.connections;
        let snapshot = wamn_postgres_capability::connection_binding::resolve(
            &self.postgres,
            component_id,
            store_alias,
        )
        .await
        .map_err(|error| {
            tracing::warn!(%error, component_id, store_alias, "blobstore authority refused");
            BindingError::unauthorized()
        })?;
        let id = snapshot
            .instance_id
            .as_deref()
            .and_then(|id| id.parse().ok())
            .ok_or(BindingError::unauthorized())?;
        let fact = active_fact(facts, &id)
            .await
            .map_err(|_| BindingError::unauthorized())?
            .filter(|fact| Some(fact.active_generation) == snapshot.generation)
            .ok_or(BindingError::unauthorized())?;
        let configuration = facts
            .generation(&id, fact.active_generation)
            .await
            .map_err(|_| BindingError::unauthorized())?;
        let provider =
            provider(&configuration.integration_type).map_err(|_| BindingError::unauthorized())?;
        let bound = binding::resolve(&snapshot, provider)?;
        let client = self
            .client(facts, id, &fact, &bound)
            .await
            .map_err(|error| {
                tracing::warn!(%error, "blobstore client unavailable");
                BindingError::unauthorized()
            })?;
        Ok(BoundContainer::new(
            Arc::clone(&client.store),
            bound.container,
            bound.prefix,
            ConnectionAuthority::Live {
                facts: Arc::clone(facts),
                id,
                generation: fact.active_generation,
            },
        )
        .with_read_only(snapshot.read_only))
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

pub(crate) struct StoreClient {
    store: Arc<dyn ObjectStore>,
    signer: Arc<dyn object_store::signer::Signer>,
}

fn provider(integration_type: &str) -> anyhow::Result<BlobstoreProvider> {
    match integration_type {
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
        .with_allow_http(endpoint.starts_with("http://"))
        .with_virtual_hosted_style_request(false)
        .build()?;
    Ok(Arc::new(store))
}

/// Build Cloud Storage with pod metadata credentials and IAM signing.
/// Explicit providers prevent fallback to stored ADC keys.
fn build_gcs_store(container: &str) -> anyhow::Result<Arc<object_store::gcp::GoogleCloudStorage>> {
    let (credentials, signing_credentials) = wamn_connections::pod_identity::providers()?;
    let store = GoogleCloudStorageBuilder::new()
        .with_credentials(credentials)
        .with_signing_credentials(signing_credentials)
        .with_bucket_name(container)
        .build()?;
    Ok(Arc::new(store))
}

/// List the first object under candidate storage coordinates without writing facts.
pub async fn health(
    integration_type: &str,
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

/// The `wamn.blobstore` span over one guest object-store effect.
///
/// Carries the shared identity vocabulary plus the invocation this component
/// was entered under: the package, the wiring position, the occurrence, the
/// admitted component name and the node operation, copied from the
/// host-attested invocation — never anything the guest sent. A
/// pooled instance with no invocation bound records those keys empty, which says
/// "about to be refused" where a missing field would look like lost
/// instrumentation.
///
/// The object KEY is deliberately absent: keys are guest-authored and can carry
/// project data, and a span is a wider audience than the effect itself.
pub(super) fn blobstore_span(
    plugin: &WamnBlobstore,
    component_id: &str,
    operation: &'static str,
) -> tracing::Span {
    let make_span = |identity: Option<&wamn_capability_authority::identity::SessionIdentity>| {
        effect_span!(
            "wamn.blobstore",
            EffectIdentity {
                project_env_key: identity
                    .as_ref()
                    .map_or("", |identity| identity.project_env_key.as_str()),
                project: identity
                    .as_ref()
                    .and_then(|identity| identity.project.as_deref())
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
        .read::<wamn_postgres_capability::invocation::Authority, _>(component_id, |record| {
            make_span(Some(&record.identity))
        })
        .unwrap_or_else(|| make_span(None));
    let invocation = plugin.invocation(component_id);
    record_wiring(
        &span,
        invocation
            .as_ref()
            .map(wamn_capability_authority::connection::ConnectionInvocation::effect_wiring),
    );
    span
}

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
