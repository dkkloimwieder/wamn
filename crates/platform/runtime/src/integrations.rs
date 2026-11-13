//! Resolve integration calls through the connection memory.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::Context as _;
use moka::sync::Cache;
use oci_client::secrets::RegistryAuth;
use tokio::sync::Mutex;
use wamn_engine::integration::{
    ConnectionDispatch, Integration, IntegrationBinding, IntegrationImports,
};
use wamn_package_artifact::registry_credentials::RegistryCredentialSource;
use wamn_package_artifact::{PackageRegistry, Repository};

use crate::connection_facts::{ConnectionFacts, bounded};
use crate::connection_memory::{ConnectionFact, ConnectionStatus};
use crate::plugins::wamn_postgres::WamnPostgres;

type InstanceKey = (uuid::Uuid, i64, Option<uuid::Uuid>);

#[derive(Debug, Clone)]
struct LinkedRead {
    digest: String,
    instance: Arc<Integration>,
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
struct ComponentReads {
    resources: Arc<ReadResources>,
    component: String,
    package: String,
    names: HashSet<String>,
}

#[async_trait::async_trait]
impl ConnectionDispatch for ComponentReads {
    async fn call(
        &self,
        name: &str,
        interface: &str,
        function: &str,
        parameters: &[wasmtime::component::Val],
    ) -> anyhow::Result<Option<anyhow::Result<Vec<wasmtime::component::Val>>>> {
        if !self.names.contains(name) {
            return Ok(None);
        }
        let release = self
            .resources
            .facts
            .release(&self.resources.manifest_digest)
            .await?;
        let Some(binding) = release.binding(&self.component, &self.package, name) else {
            return Ok(None);
        };
        let guard = match self
            .resources
            .facts
            .exclusive_call(binding.connection_id)
            .await
        {
            Ok(guard) => guard,
            Err(error) => {
                tracing::warn!(connection = %binding.connection_id, %error, "connection unavailable");
                return Ok(None);
            }
        };
        let Some(instance) = self.resolve(name).await? else {
            return Ok(None);
        };
        let values = instance.call(interface, function, parameters).await;
        if guard.as_ref().is_some_and(|guard| !guard.held()) {
            return Ok(None);
        }
        Ok(Some(values))
    }

    async fn resolve(&self, name: &str) -> anyhow::Result<Option<Arc<Integration>>> {
        if !self.names.contains(name) {
            return Ok(None);
        }
        let resources = &self.resources;
        // Serialize fact observations and first links so an old observation cannot
        // evict a newer link, and concurrent misses prepare only one instance.
        let mut state = resources.state.lock().await;
        let release = resources.facts.release(&resources.manifest_digest).await?;
        let Some(binding) = release.binding(&self.component, &self.package, name) else {
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
        let credential = match fact
            .credential_id
            .filter(|_| generation.auth_type == "custom")
        {
            Some(id) => Some(resources.facts.secret(&id).await?.expose().to_owned()),
            None => None,
        };
        let bound = IntegrationBinding::new(authority, credential)?
            .with_parameters(generation.parameters.clone(), &generation.auth_type);
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
                wamn_package_artifact::integration::pull_from(
                    &repository,
                    &generation.integration_type,
                    digest,
                )
                .await?,
            );
            state.bytes.insert(digest.to_owned(), Arc::clone(&bytes));
            bytes
        };
        let instance = Arc::new(Integration::load(&bytes, bound).await?);
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

/// Process-owned imports with immutable declarations and shared links by identity.
#[derive(Debug)]
pub struct Integrations {
    imports: HashMap<(String, String), Arc<IntegrationImports>>,
    declarations: HashMap<(String, String), HashSet<String>>,
}

impl Integrations {
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
        let mut imports = HashMap::new();
        let mut declarations = HashMap::new();
        for component in &release.manifest().components {
            let digest = &component.descriptor.component_digest;
            let package = &component.package_id;
            let declared = postgres
                .declared_connection_interfaces(
                    project,
                    &release.scope().project_env_key,
                    package,
                    digest,
                )
                .await?;
            let declared: Vec<_> = declared
                .into_iter()
                .filter(|(_, interface)| !host_interface(interface))
                .collect();
            let names: HashSet<_> = declared.iter().map(|(name, _)| name.clone()).collect();
            let interfaces = declared
                .into_iter()
                .map(|(_, interface)| interface)
                .collect();
            let dispatch = Arc::new(ComponentReads {
                resources: Arc::clone(&resources),
                component: digest.clone(),
                package: package.clone(),
                names: names.clone(),
            });
            let key = (digest.clone(), package.clone());
            declarations.insert(key.clone(), names);
            imports.insert(
                key,
                Arc::new(IntegrationImports::from_dispatch(dispatch, interfaces)),
            );
        }
        Ok(Self {
            imports,
            declarations,
        })
    }

    pub fn interfaces(&self) -> HashSet<wash_runtime::wit::WitInterface> {
        self.imports
            .values()
            .flat_map(|imports| imports.interfaces())
            .map(wash_runtime::wit::WitInterface::from)
            .collect()
    }

    pub fn imports(
        &self,
        component: &str,
        package: &str,
    ) -> anyhow::Result<Arc<IntegrationImports>> {
        self.imports
            .get(&(component.to_owned(), package.to_owned()))
            .cloned()
            .context("connection component is not declared")
    }

    /// Link only the labels declared by the exact component and package owners.
    pub async fn add_to_linker(
        &self,
        item: &mut wash_runtime::engine::workload::WorkloadItem<'_>,
        owners: &[(&str, &str)],
    ) -> anyhow::Result<()> {
        let mut routes = HashMap::new();
        let mut interfaces = HashSet::new();
        for &(component, package) in owners {
            let key = (component.to_owned(), package.to_owned());
            let Some(names) = self.declarations.get(&key) else {
                continue;
            };
            let imports = self.imports(component, package)?;
            for name in names {
                routes
                    .entry(name.clone())
                    .or_insert_with(Vec::new)
                    .push(Arc::clone(&imports));
            }
            interfaces.extend(imports.interfaces().map(str::to_owned));
        }
        let names = routes.keys().cloned().collect();
        let imports = Arc::new(IntegrationImports::from_dispatch(
            Arc::new(ComponentImports { routes }),
            interfaces,
        ));
        imports.add_to_linker(item, &names).await
    }
}

fn host_interface(interface: &str) -> bool {
    interface.starts_with("wamn:connection/") || interface.starts_with("wasmcloud:blobstore/")
}

#[derive(Debug)]
struct ComponentImports {
    routes: HashMap<String, Vec<Arc<IntegrationImports>>>,
}

#[async_trait::async_trait]
impl ConnectionDispatch for ComponentImports {
    async fn call(
        &self,
        name: &str,
        interface: &str,
        function: &str,
        parameters: &[wasmtime::component::Val],
    ) -> anyhow::Result<Option<anyhow::Result<Vec<wasmtime::component::Val>>>> {
        // Keep the existing refusal when composed owners bind a label differently.
        if self.resolve(name).await?.is_none() {
            return Ok(None);
        }
        self.routes[name][0]
            .call(name, interface, function, parameters)
            .await
    }

    async fn resolve(&self, name: &str) -> anyhow::Result<Option<Arc<Integration>>> {
        let Some(imports) = self.routes.get(name) else {
            return Ok(None);
        };
        let mut resolved = None;
        for imports in imports {
            let Some(instance) = imports.resolve(name).await? else {
                return Ok(None);
            };
            if let Some(previous) = &resolved {
                anyhow::ensure!(
                    Arc::ptr_eq(previous, &instance),
                    "unbound: component bindings differ for {name}"
                );
            }
            resolved = Some(instance);
        }
        Ok(resolved)
    }
}

/// Typed imports of the native HTTP entry, scoped to its component owners.
#[derive(Debug)]
pub struct IntegrationIngress {
    pub integrations: Arc<Integrations>,
    pub release: Arc<wamn_engine::release_manifest::LoadedRelease>,
}

#[async_trait::async_trait]
impl wash_runtime::plugin::HostPlugin for IntegrationIngress {
    fn id(&self) -> &'static str {
        "wamn-integration-ingress"
    }
    fn world(&self) -> wash_runtime::wit::WitWorld {
        wash_runtime::wit::WitWorld {
            imports: self.integrations.interfaces(),
            exports: HashSet::new(),
        }
    }
    fn supports_named_instances(&self) -> bool {
        true
    }

    async fn on_workload_item_bind<'a>(
        &self,
        item: &mut wash_runtime::engine::workload::WorkloadItem<'a>,
        _: wash_runtime::plugin::WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        let manifest = self.release.manifest();
        anyhow::ensure!(
            manifest.composed_entry.is_some(),
            "integration ingress requires a composed entry"
        );
        let package = &manifest
            .composing_member()
            .context("integration entry has no composing member")?
            .package_id;
        let application =
            wamn_catalog::application_for_package(&self.release.scope().applications, package)
                .context("integration entry application is ambiguous")?;
        let owners = manifest
            .components
            .iter()
            .filter(|component| {
                wamn_catalog::application_for_package(
                    &self.release.scope().applications,
                    &component.package_id,
                ) == Some(application)
            })
            .map(|component| {
                (
                    component.descriptor.component_digest.as_str(),
                    component.package_id.as_str(),
                )
            })
            .collect::<Vec<_>>();
        self.integrations.add_to_linker(item, &owners).await
    }
}
