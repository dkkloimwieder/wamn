//! Resolve wasm connection calls through the connection memory.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::Context as _;
use moka::sync::Cache;
use oci_client::secrets::RegistryAuth;
use tokio::sync::Mutex;
use wamn_engine::connector::{ConnectionDispatch, Connector, ConnectorBinding, ConnectorReader};
use wamn_package_artifact::registry_credentials::RegistryCredentialSource;
use wamn_package_artifact::{PackageRegistry, Repository};

use crate::connection_facts::{ConnectionFacts, bounded};
use crate::connection_memory::{ConnectionFact, ConnectionStatus};
use crate::plugins::wamn_postgres::WamnPostgres;

type InstanceKey = (uuid::Uuid, i64, Option<uuid::Uuid>);

#[derive(Debug, Clone)]
struct LinkedRead {
    digest: String,
    instance: Arc<Connector>,
}

#[derive(Debug)]
struct ReadState {
    linked: Cache<InstanceKey, LinkedRead>,
    seen: HashMap<uuid::Uuid, ConnectionFact>,
    bytes: HashMap<String, Arc<Vec<u8>>>,
}

struct ReadResources {
    facts: Arc<ConnectionFacts>,
    manifest_digest: String,
    registry: PackageRegistry,
    registry_credentials: RegistryCredentialSource,
    state: Mutex<ReadState>,
}

impl std::fmt::Debug for ReadResources {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReadResources")
            .field("manifest_digest", &self.manifest_digest)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct ApplicationReads {
    resources: Arc<ReadResources>,
    // Immutable declarations map each name to its component and package.
    names: HashMap<String, (String, String)>,
}

#[async_trait::async_trait]
impl ConnectionDispatch for ApplicationReads {
    async fn resolve(&self, name: &str) -> anyhow::Result<Option<Arc<Connector>>> {
        let Some((component, package)) = self.names.get(name) else {
            return Ok(None);
        };
        let resources = &self.resources;
        // Serialize fact observations and first links so an old observation cannot
        // evict a newer link, and concurrent misses prepare only one instance.
        let mut state = resources.state.lock().await;
        let release = resources.facts.release(&resources.manifest_digest).await?;
        let Some(binding) = release.binding(component, package, name) else {
            return Ok(None);
        };
        let id = binding.connection_id;
        let fact = resources.facts.connection(&id).await?;
        state.observe(id, fact.as_ref());
        let Some(fact) = fact.filter(|fact| fact.status == ConnectionStatus::Active) else {
            return Ok(None);
        };
        let key = (id, fact.active_generation, fact.credential_id);
        let generation = resources
            .facts
            .generation(&id, fact.active_generation)
            .await?;
        if generation.runtime_type != "wasm" {
            return Ok(None);
        }
        let version = resources
            .facts
            .version(&generation.integration_type, Some(binding.version))
            .await?;
        let digest = version
            .digest
            .as_deref()
            .context("wasm version has no digest")?;
        if let Some(linked) = state.linked.get(&key)
            && linked.digest == digest
        {
            return Ok(Some(linked.instance));
        }
        let authority = generation.parameters["authority"]
            .as_str()
            .context("wasm connection needs authority")?;
        let credential = match fact.credential_id {
            Some(id) => Some(resources.facts.secret(&id).await?.expose().to_owned()),
            None => None,
        };
        let bound = ConnectorBinding::new(authority, credential)?;
        let bytes = if let Some(bytes) = state.bytes.get(digest) {
            Arc::clone(bytes)
        } else {
            let registry_name =
                wamn_catalog::parse_component_artifact_base(&resources.registry.artifact_base)?;
            let credential = resources
                .registry_credentials
                .credentials(registry_name.registry())
                .await?;
            let repository = Repository::with_auth(
                &resources.registry,
                digest,
                RegistryAuth::Basic(
                    credential.username().to_owned(),
                    credential.password().to_owned(),
                ),
            )?;
            let bytes = Arc::new(
                wamn_package_artifact::connector::pull_from(
                    &repository,
                    &generation.integration_type,
                    digest,
                )
                .await?,
            );
            state.bytes.insert(digest.to_owned(), Arc::clone(&bytes));
            bytes
        };
        let instance = Arc::new(Connector::load(&bytes, bound).await?);
        state.linked.insert(
            key,
            LinkedRead {
                digest: digest.to_owned(),
                instance: Arc::clone(&instance),
            },
        );
        Ok(Some(instance))
    }
}

impl ReadState {
    fn observe(&mut self, id: uuid::Uuid, fact: Option<&ConnectionFact>) {
        if self.seen.get(&id) == fact {
            return;
        }
        for (key, _) in &self.linked {
            if key.0 == id
                && fact.is_none_or(|fact| {
                    fact.status != ConnectionStatus::Active
                        || (key.1, key.2) != (fact.active_generation, fact.credential_id)
                })
            {
                self.linked.invalidate(key.as_ref());
            }
        }
        match fact {
            Some(fact) => {
                self.seen.insert(id, fact.clone());
            }
            None => {
                self.seen.remove(&id);
            }
        }
    }
}

/// Process-owned readers with immutable declarations and shared links by identity.
#[derive(Debug)]
pub struct Connectors {
    readers: HashMap<String, Arc<ConnectorReader>>,
}

impl Connectors {
    /// Read immutable declarations before the application stores load.
    pub async fn new(
        facts: Arc<ConnectionFacts>,
        release: &wamn_engine::release_manifest::LoadedRelease,
        postgres: &WamnPostgres,
        project: &str,
        registry: PackageRegistry,
        registry_credentials: RegistryCredentialSource,
    ) -> anyhow::Result<Self> {
        let resources = Arc::new(ReadResources {
            facts,
            manifest_digest: release.release().manifest_digest.to_string(),
            registry,
            registry_credentials,
            state: Mutex::new(ReadState {
                linked: bounded(),
                seen: HashMap::new(),
                bytes: HashMap::new(),
            }),
        });
        let mut declarations = release
            .scope()
            .applications
            .keys()
            .map(|name| (name.clone(), HashMap::new()))
            .collect::<HashMap<_, _>>();
        for component in &release.manifest().components {
            let application = wamn_catalog::application_for_package(
                &release.scope().applications,
                &component.package_id,
            )
            .context("connection component has no application")?;
            let names = postgres
                .declared_connection_names(
                    project,
                    &release.scope().env_key,
                    &component.package_id,
                    &component.descriptor.component_digest,
                )
                .await?;
            let declared = declarations
                .get_mut(application)
                .context("connection application is not declared")?;
            for name in names {
                // The application declares one target for each name, even when
                // several of its components declare that same name.
                declared.entry(name).or_insert_with(|| {
                    (
                        component.descriptor.component_digest.clone(),
                        component.package_id.clone(),
                    )
                });
            }
        }
        Ok(Self {
            readers: declarations
                .into_iter()
                .map(|(application, names)| {
                    let dispatch = Arc::new(ApplicationReads {
                        resources: Arc::clone(&resources),
                        names,
                    });
                    (
                        application,
                        Arc::new(ConnectorReader::from_dispatch(dispatch)),
                    )
                })
                .collect(),
        })
    }

    pub fn reader(&self, application: &str) -> anyhow::Result<Arc<ConnectorReader>> {
        self.readers
            .get(application)
            .cloned()
            .context("connection application is not declared")
    }
}

/// Reader for the native HTTP entry. Operation workloads bind their own application.
#[derive(Debug)]
pub struct ConnectorIngress {
    pub postgres: Arc<crate::plugins::wamn_postgres::WamnPostgres>,
    pub project: String,
    pub connectors: Arc<Connectors>,
    pub release: Arc<wamn_engine::release_manifest::LoadedRelease>,
}

#[async_trait::async_trait]
impl wash_runtime::plugin::HostPlugin for ConnectorIngress {
    fn id(&self) -> &'static str {
        "wamn-connector-ingress"
    }
    fn world(&self) -> wash_runtime::wit::WitWorld {
        wash_runtime::wit::WitWorld {
            imports: HashSet::from([wash_runtime::wit::WitInterface::from(
                wamn_engine::connector::READER,
            )]),
            exports: HashSet::new(),
        }
    }
    async fn on_workload_item_bind<'a>(
        &self,
        item: &mut wash_runtime::engine::workload::WorkloadItem<'a>,
        _: wash_runtime::plugin::WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        let manifest = self.release.manifest();
        anyhow::ensure!(
            manifest.composed_entry.is_some(),
            "connector ingress requires a composed entry"
        );
        let package = &manifest
            .composing_member()
            .context("connector entry has no composing member")?
            .package_id;
        let application =
            wamn_catalog::application_for_package(&self.release.scope().applications, package)
                .context("connector entry application is ambiguous")?;
        let mut names = HashSet::new();
        for component in manifest.components.iter().filter(|component| {
            wamn_catalog::application_for_package(
                &self.release.scope().applications,
                &component.package_id,
            ) == Some(application)
        }) {
            names.extend(
                self.postgres
                    .declared_connection_names(
                        &self.project,
                        &self.release.scope().env_key,
                        &component.package_id,
                        &component.descriptor.component_digest,
                    )
                    .await?,
            );
        }
        self.connectors
            .reader(application)?
            .add_to_linker(item, names)
    }
}
