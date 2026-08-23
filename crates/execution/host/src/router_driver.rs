//! The single production driver for direct and queued wiring delivery.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::num::NonZeroUsize;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use wamn_catalog::{AdmittedComponent, ServingComponent, ServingWiring};
use wamn_control_registry::identifiers::valid_runner;
use wamn_router::{
    ActiveWiring, CacheInsert, Delivery, ErrorDetail, Lookup, NodeError, NodeOutcome, Outcome,
    RateLimitDetail, Step, WiringCache, WiringCacheSnapshot,
};
use wamn_runtime::component_artifact_source::ComponentArtifactSource;
use wamn_runtime::engine::{MAX_HOST_CALL_DURATION, MEMORY_CAP_BYTES};
use wamn_runtime::plugins::connection_http::{
    self, CONNECTION_HTTP_ID, ConnectionHttp, ConnectionInvocation,
};
use wamn_runtime::plugins::wamn_credentials::WamnCredentials;
use wamn_runtime::plugins::wamn_logging::{WAMN_LOGGING_ID, WamnLogging};
use wamn_runtime::plugins::wamn_postgres::ResolvedActiveWiring;
use wamn_runtime::plugins::wamn_postgres::{
    ReleaseIdentity, SessionClaims, WAMN_POSTGRES_ID, WamnPostgres,
};
use wamn_runtime::release_manifest::ReleaseManifestWeld;
use wamn_runtime::wiring_doorbell::WiringDoorbellListener;
use wash_runtime::engine::Engine;
use wash_runtime::engine::InstancePolicy;
use wash_runtime::engine::ctx::{Ctx, SharedCtx, WamnStoreLimiter};
use wash_runtime::engine::workload::{WorkloadComponent, WorkloadItem};
use wash_runtime::host::allowed_hosts::AllowedHost;
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wasmtime::Store;
use wash_runtime::wasmtime::component::{Component, Linker};

use crate::{
    ExecutionInstancePool, ExecutionPoolKey, ExecutionPoolLimits, INVOCATIONS_PER_INSTANCE,
    InvocationDisposition, ReusableExecutionInstance,
};

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        path: "../router/wit",
        world: "node",
        exports: { default: async },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

use bindings::wamn::node::types as node_types;

/// Shared CLI/environment key for the only wiring cache in a serving process.
pub const WIRING_CACHE_CAPACITY_ENV: &str = "WAMN_WIRING_CACHE_CAPACITY";

/// Default entries in the process-local wiring cache.
///
/// Entries are parsed documents plus immutable catalog pointers (roughly KiB),
/// while the production working set is hundreds of active wirings per
/// environment. 1,024 therefore costs single-digit MiB and cheaply avoids hot
/// path re-parsing; the hit/eviction metrics make the choice evidence-tunable.
pub const DEFAULT_WIRING_CACHE_CAPACITY: usize = 1_024;

/// A non-zero wiring cache bound, parsed once at process construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WiringCacheCapacity(NonZeroUsize);

impl WiringCacheCapacity {
    pub fn get(self) -> NonZeroUsize {
        self.0
    }
}

impl Default for WiringCacheCapacity {
    fn default() -> Self {
        Self(NonZeroUsize::new(DEFAULT_WIRING_CACHE_CAPACITY).expect("default is non-zero"))
    }
}

impl fmt::Display for WiringCacheCapacity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for WiringCacheCapacity {
    type Err = InvalidWiringCacheCapacity;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let parsed = value
            .parse::<usize>()
            .ok()
            .and_then(NonZeroUsize::new)
            .ok_or(InvalidWiringCacheCapacity)?;
        Ok(Self(parsed))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidWiringCacheCapacity;

impl fmt::Display for InvalidWiringCacheCapacity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("wiring cache capacity must be a non-zero integer")
    }
}

impl std::error::Error for InvalidWiringCacheCapacity {}

/// Process-owned construction facts shared by the host and executor leaves.
#[derive(Debug, Clone)]
pub struct RouterDriverConfig {
    pub owner_prefix: String,
    pub project: String,
    pub schema: Option<String>,
    pub cache_capacity: WiringCacheCapacity,
    /// The exact cadence of the engine epoch ticker owned by this process.
    pub epoch_tick: Duration,
}

/// Which resolution authority one delivery carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WiringResolution {
    /// A trusted attachment/registration resolved the current pointer. The DB
    /// rechecks that exact version is still active.
    Active,
    /// Queue admission already froze this immutable version. Pointer flips do
    /// not reinterpret it.
    Frozen,
}

/// Trusted coordinates handed from an ingress admission owner to the driver.
#[derive(Debug, Clone)]
pub struct RouterDriverRequest {
    pub tenant_id: String,
    pub catalog_id: String,
    pub environment: String,
    pub wiring_id: String,
    pub wiring_version: u32,
    pub delivery_id: String,
    pub payload: serde_json::Value,
    pub caller_attached: bool,
    pub resolution: WiringResolution,
    pub role: Option<String>,
    pub user_id: Option<String>,
    pub traceparent: Option<String>,
    pub tracestate: Option<String>,
}

/// One completely walked delivery, including the exact graph identity used.
#[derive(Debug, Clone)]
pub struct RouterDelivery {
    pub wiring_version: u32,
    pub graph_hash: Arc<str>,
    pub outcome: Outcome,
}

/// Read-only lifecycle totals for the two bounded driver stores.
#[derive(Debug, Clone)]
pub struct RouterDriverSnapshot {
    pub wiring_cache: WiringCacheSnapshot,
    pub instances: crate::ExecutionPoolSnapshot,
}

#[derive(Debug)]
struct CatalogFacts {
    catalog_version: u32,
    components: Arc<[AdmittedComponent]>,
}

impl CatalogFacts {
    fn from_resolved(resolved: &ResolvedActiveWiring) -> Self {
        Self {
            catalog_version: resolved.catalog_version,
            components: Arc::clone(&resolved.components),
        }
    }

    fn component(&self, digest: &str) -> Option<&AdmittedComponent> {
        self.components
            .iter()
            .find(|component| component.component_digest == digest)
    }
}

/// One router, cache, artifact source, and digest-keyed instance pool per
/// serving process. Both process leaves construct this exact type.
pub struct RouterDriver {
    engine: Arc<Engine>,
    postgres: Arc<WamnPostgres>,
    credentials: Arc<WamnCredentials>,
    logging: Arc<WamnLogging>,
    allowed_hosts: Arc<[AllowedHost]>,
    release: Arc<ReleaseManifestWeld>,
    source: ComponentArtifactSource,
    config: RouterDriverConfig,
    cache: Arc<WiringCache<CatalogFacts>>,
    instances: ExecutionInstancePool<NodeInstance>,
    _doorbell: WiringDoorbellListener,
    started: Instant,
}

impl fmt::Debug for RouterDriver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RouterDriver")
            .field("config", &self.config)
            .field("cache", &self.cache.snapshot())
            .field("instances", &self.instances.snapshot())
            .finish_non_exhaustive()
    }
}

impl RouterDriver {
    #[expect(
        clippy::too_many_arguments,
        reason = "each host-owned capability is an independent production dependency"
    )]
    pub fn new(
        engine: Arc<Engine>,
        postgres: Arc<WamnPostgres>,
        credentials: Arc<WamnCredentials>,
        logging: Arc<WamnLogging>,
        allowed_hosts: Arc<[AllowedHost]>,
        release: Arc<ReleaseManifestWeld>,
        source: ComponentArtifactSource,
        config: RouterDriverConfig,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            valid_runner(&config.owner_prefix),
            "invalid router owner {:?}: 1-128 chars of [A-Za-z0-9_-] required",
            config.owner_prefix
        );
        anyhow::ensure!(!config.epoch_tick.is_zero(), "router-epoch-tick-zero");
        let cache = Arc::new(WiringCache::new(config.cache_capacity.get()));
        let doorbell = WiringDoorbellListener::postgres(
            Arc::clone(&postgres),
            Some(config.project.clone()),
            Arc::clone(&cache),
        )?;
        let instances = ExecutionInstancePool::new(ExecutionPoolLimits {
            max_instances: 512,
            max_reserved_bytes: MEMORY_CAP_BYTES.saturating_mul(512),
            max_idle_per_digest: 8,
            max_invocations_per_instance: INVOCATIONS_PER_INSTANCE,
            max_idle_age: Duration::from_secs(60),
        })?;
        Ok(Self {
            engine,
            postgres,
            credentials,
            logging,
            allowed_hosts,
            release,
            source,
            config,
            cache,
            instances,
            _doorbell: doorbell,
            started: Instant::now(),
        })
    }

    pub fn snapshot(&self) -> RouterDriverSnapshot {
        RouterDriverSnapshot {
            wiring_cache: self.cache.snapshot(),
            instances: self.instances.snapshot(),
        }
    }

    /// Execute one direct or queued delivery through the same router and node
    /// invoker. The caller owns acting on the terminal verdict.
    pub async fn execute(&self, request: RouterDriverRequest) -> anyhow::Result<RouterDelivery> {
        self.validate_request_scope(&request)?;
        let active = self.resolve(&request).await?;
        self.validate_wiring_closure(&request, &active)?;
        let wiring = Arc::clone(&active.wiring);
        let mut walk = wiring.start(Delivery {
            id: request.delivery_id.clone(),
            payload: request.payload.clone(),
            caller_attached: request.caller_attached,
        });
        loop {
            let now_ms = self.now_ms();
            match wiring.next(&mut walk, now_ms) {
                Step::Done(status) => {
                    return Ok(RouterDelivery {
                        wiring_version: active.version,
                        graph_hash: Arc::clone(&active.graph_hash),
                        outcome: Outcome {
                            status,
                            result: walk.result().clone(),
                            failure: walk.failure().cloned(),
                            hops: walk.hops(),
                            verdict: walk.verdict().cloned(),
                        },
                    });
                }
                Step::Wait { until_ms, .. } => {
                    let remaining = until_ms.saturating_sub(self.now_ms());
                    tokio::time::sleep(Duration::from_millis(remaining)).await;
                }
                Step::Invoke(call) => {
                    let outcome = self
                        .invoke_node(&request, &active, &call)
                        .await
                        .with_context(|| format!("invoke wiring node {:?}", call.node))?;
                    if let Err(refusal) = wiring.apply(&mut walk, &call, outcome, self.now_ms()) {
                        wiring
                            .fail_on_node_data(&mut walk, &call.node, refusal)
                            .context("router driver applied an impossible transition")?;
                    }
                }
            }
        }
    }

    async fn resolve(
        &self,
        request: &RouterDriverRequest,
    ) -> anyhow::Result<ActiveWiring<CatalogFacts>> {
        match request.resolution {
            WiringResolution::Active => self.resolve_active(request).await,
            WiringResolution::Frozen => self.resolve_frozen(request).await,
        }
    }

    async fn resolve_active(
        &self,
        request: &RouterDriverRequest,
    ) -> anyhow::Result<ActiveWiring<CatalogFacts>> {
        loop {
            let token = match self.cache.get(
                &request.tenant_id,
                &request.catalog_id,
                &request.environment,
                &request.wiring_id,
            ) {
                Lookup::Hit(active) if active.version == request.wiring_version => {
                    return Ok(active);
                }
                Lookup::Hit(_) => {
                    self.cache.invalidate(
                        &request.tenant_id,
                        &request.catalog_id,
                        &request.environment,
                        &request.wiring_id,
                    );
                    continue;
                }
                Lookup::Miss(token) => token,
            };
            let resolved = self
                .postgres
                .resolve_active_wiring(
                    &self.config.project,
                    &request.tenant_id,
                    &request.catalog_id,
                    &request.environment,
                    &request.wiring_id,
                    request.wiring_version,
                )
                .await?
                .ok_or_else(|| anyhow::anyhow!("active-wiring-not-found"))?;
            let facts = CatalogFacts::from_resolved(&resolved);
            match self.cache.insert(
                &request.tenant_id,
                &request.catalog_id,
                &request.environment,
                &request.wiring_id,
                resolved.version,
                Arc::clone(&resolved.graph_hash),
                resolved.wiring,
                facts,
                token,
            ) {
                CacheInsert::Installed(active) => return Ok(active),
                CacheInsert::Overtaken => continue,
                CacheInsert::HashMismatch => {
                    anyhow::bail!("active-wiring-immutable-hash-mismatch")
                }
            }
        }
    }

    async fn resolve_frozen(
        &self,
        request: &RouterDriverRequest,
    ) -> anyhow::Result<ActiveWiring<CatalogFacts>> {
        if let Some(active) = self.cache.get_version(
            &request.tenant_id,
            &request.catalog_id,
            &request.environment,
            &request.wiring_id,
            request.wiring_version,
        ) {
            return Ok(active);
        }
        let release = self.release.manifest();
        let resolved = self
            .postgres
            .resolve_release_wiring(
                &self.config.project,
                &request.tenant_id,
                &request.catalog_id,
                &request.environment,
                release.release.catalog_version,
                self.release.release().manifest_digest.as_str(),
                &request.wiring_id,
                request.wiring_version,
            )
            .await?
            .ok_or_else(|| anyhow::anyhow!("release-wiring-not-found"))?;
        let facts = CatalogFacts::from_resolved(&resolved);
        match self.cache.insert_version(
            &request.tenant_id,
            &request.catalog_id,
            &request.environment,
            &request.wiring_id,
            resolved.version,
            Arc::clone(&resolved.graph_hash),
            resolved.wiring,
            facts,
        ) {
            CacheInsert::Installed(active) => Ok(active),
            CacheInsert::HashMismatch => {
                anyhow::bail!("release-wiring-immutable-hash-mismatch")
            }
            CacheInsert::Overtaken => unreachable!("exact-version insert has no pointer token"),
        }
    }

    fn validate_request_scope(&self, request: &RouterDriverRequest) -> anyhow::Result<()> {
        let release = &self.release.manifest().release;
        anyhow::ensure!(request.wiring_version > 0, "wiring-version-zero");
        anyhow::ensure!(
            release.tenant_id == request.tenant_id
                && release.catalog_id == request.catalog_id
                && release.environment == request.environment,
            "router-request-release-scope-mismatch"
        );
        Ok(())
    }

    fn validate_wiring_closure(
        &self,
        request: &RouterDriverRequest,
        active: &ActiveWiring<CatalogFacts>,
    ) -> anyhow::Result<()> {
        let expected = ServingWiring {
            wiring_id: request.wiring_id.clone(),
            wiring_version: request.wiring_version,
            graph_hash: active.graph_hash.to_string(),
        };
        anyhow::ensure!(
            self.release.manifest().wirings.contains(&expected),
            "wiring-not-in-carried-release"
        );
        anyhow::ensure!(
            active.facts.catalog_version == self.release.manifest().release.catalog_version,
            "wiring-catalog-version-not-carried"
        );
        Ok(())
    }

    async fn invoke_node(
        &self,
        request: &RouterDriverRequest,
        active: &ActiveWiring<CatalogFacts>,
        call: &wamn_router::NodeCall,
    ) -> anyhow::Result<NodeOutcome> {
        let component = active
            .facts
            .component(&call.component)
            .ok_or_else(|| anyhow::anyhow!("router-node-component-fact-missing"))?;
        let release_component = ServingComponent {
            component: component.component.clone(),
            interface_version: component.interface_version.clone(),
            digest: component.component_digest.clone(),
        };
        anyhow::ensure!(
            self.release
                .manifest()
                .components
                .contains(&release_component),
            "component-not-in-carried-release"
        );
        let acquisition = NodeAcquisition {
            claims: SessionClaims {
                tenant: request.tenant_id.clone(),
                project: Some(self.config.project.clone()),
                schema: self.config.schema.clone(),
                runner: Some(self.config.owner_prefix.clone()),
                role: request.role.clone(),
                user_id: request.user_id.clone(),
                release: Some(ReleaseIdentity {
                    release_version: self.release.release().release_version,
                    manifest_digest: self.release.release().manifest_digest.clone(),
                }),
            },
            invocation: ConnectionInvocation {
                wiring_id: request.wiring_id.clone(),
                wiring_version: active.version,
                node_id: call.node.clone(),
                occurrence: call.occurrence,
                component_digest: component.component_digest.clone(),
            },
        };
        let key = ExecutionPoolKey::new(component.component_digest.clone());
        let mut lease = match self.instances.checkout(&key, &acquisition)? {
            Some(lease) => lease,
            None => {
                let bytes = self.source.pull_verified(component).await?;
                let instance = NodeInstance::instantiate(
                    &self.engine,
                    &bytes,
                    Arc::clone(&self.postgres),
                    Arc::clone(&self.credentials),
                    Arc::clone(&self.logging),
                    Arc::clone(&self.allowed_hosts),
                    Arc::clone(&self.release),
                    &self.config,
                    &request.tenant_id,
                    component,
                )
                .await?;
                self.instances
                    .checkout_new(key, instance, &acquisition)?
                    .ok_or_else(|| anyhow::anyhow!("component-instance-pool-capacity"))?
            }
        };
        let deadline_ms = bounded_node_deadline_ms(call.deadline_ms);
        let context = node_types::NodeContext {
            wiring_id: request.wiring_id.clone(),
            wiring_version: active.version,
            node_id: call.node.clone(),
            delivery_id: request.delivery_id.clone(),
            input_port: call.input_port.clone(),
            occurrence: call.occurrence,
            traceparent: request.traceparent.clone(),
            tracestate: request.tracestate.clone(),
            deadline_ms: Some(deadline_ms),
            config: serde_json::to_string(&call.config).context("encode node config")?,
        };
        let input = serde_json::to_string(&call.payload).context("encode node input")?;
        let invoked = lease
            .instance_mut()
            .run(&context, &input, deadline_ms)
            .await;
        match invoked {
            Ok(outcome) => {
                let disposition = if matches!(&outcome, Err(node_types::NodeError::Cancelled)) {
                    InvocationDisposition::Cancelled
                } else {
                    InvocationDisposition::Reusable
                };
                lease.finish(disposition)?;
                lower_node_outcome(outcome)
            }
            Err(error) => {
                lease.finish(InvocationDisposition::Trap)?;
                Err(error)
            }
        }
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

#[derive(Debug, Clone)]
struct NodeAcquisition {
    claims: SessionClaims,
    invocation: ConnectionInvocation,
}

#[derive(Debug)]
struct NodeIdentityBindError(anyhow::Error);

impl fmt::Display for NodeIdentityBindError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::error::Error for NodeIdentityBindError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}

#[derive(Debug, Clone, Copy)]
struct NodeResetUnavailable;

impl fmt::Display for NodeResetUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("node instance cannot prove guest-memory reset")
    }
}

impl std::error::Error for NodeResetUnavailable {}

struct NodeInstance {
    store: Store<SharedCtx>,
    node: bindings::Node,
    postgres: Arc<WamnPostgres>,
    logging: Arc<WamnLogging>,
    connection_http: Arc<ConnectionHttp>,
    scope: Box<str>,
    epoch_tick: Duration,
}

impl fmt::Debug for NodeInstance {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NodeInstance")
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

impl NodeInstance {
    #[expect(
        clippy::too_many_arguments,
        reason = "instance construction welds each independent host capability"
    )]
    async fn instantiate(
        engine: &Engine,
        bytes: &[u8],
        postgres: Arc<WamnPostgres>,
        credentials: Arc<WamnCredentials>,
        logging: Arc<WamnLogging>,
        allowed_hosts: Arc<[AllowedHost]>,
        release: Arc<ReleaseManifestWeld>,
        config: &RouterDriverConfig,
        tenant_id: &str,
        component_fact: &AdmittedComponent,
    ) -> anyhow::Result<Self> {
        let component = Component::new(engine.inner(), bytes)
            .map_err(|error| anyhow::anyhow!("compile wamn:node: {error}"))?;
        let mut linker: Linker<SharedCtx> = Linker::new(engine.inner());
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
        let local = wash_runtime::types::LocalResources {
            allowed_hosts: Arc::clone(&allowed_hosts),
            ..Default::default()
        };
        let connection_http = Arc::new(ConnectionHttp::new(
            Arc::clone(&postgres),
            credentials,
            tenant_id,
            config.project.as_str(),
            allowed_hosts,
            Some(release),
        ));
        let loopback = Arc::new(std::sync::Mutex::new(
            wash_runtime::sockets::loopback::Network::default(),
        ));
        let mut workload = WorkloadComponent::new(
            "router-driver",
            "router-driver",
            "wamn",
            component_fact.component.as_str(),
            component,
            linker,
            Vec::new(),
            local,
            loopback,
            InstancePolicy::Ephemeral,
        );
        let imports = workload.world().imports;
        // The driver has one credential-exact project. Route every named
        // Postgres instance through that same trusted project rather than
        // bypassing the plugin's `(implements ...)` binder with a raw linker.
        let imports: HashSet<_> = imports
            .into_iter()
            .map(|mut interface| {
                if interface.namespace == "wamn"
                    && interface.package == "postgres"
                    && interface.name.is_some()
                {
                    interface
                        .config
                        .insert("project".to_owned(), config.project.clone());
                }
                interface
            })
            .collect();
        {
            let mut item = WorkloadItem::Component(&mut workload);
            postgres
                .on_workload_item_bind(&mut item, WitInterfaces::new(&imports))
                .await?;
            logging
                .on_workload_item_bind(&mut item, WitInterfaces::new(&imports))
                .await?;
            if WitInterfaces::new(&imports).contains("wamn", "connection", &["http"]) {
                connection_http::add_to_linker(item.linker())?;
            }
        }
        let scope: Box<str> = workload.id().into();
        // Linker setup is not an identity bind. In particular WamnLogging's
        // plugin hook seeds even an empty claim. Clear every registry before
        // component instantiation so start functions cannot exercise tenant
        // authority; pool checkout is the sole identity installation point.
        postgres.revoke_session_claims(&scope);
        logging.clear_claim(&scope);
        connection_http.revoke_invocation(&scope);
        let mut plugins: HashMap<&'static str, Arc<dyn HostPlugin + Send + Sync>> = HashMap::new();
        plugins.insert(WAMN_POSTGRES_ID, Arc::clone(&postgres) as _);
        plugins.insert(WAMN_LOGGING_ID, Arc::clone(&logging) as _);
        plugins.insert(CONNECTION_HTTP_ID, Arc::clone(&connection_http) as _);
        let ctx = Ctx::builder(scope.to_string(), scope.to_string())
            .with_plugins(plugins)
            .build();
        let mut store = Store::new(engine.inner(), SharedCtx::new(ctx));
        store.data_mut().wamn_limiter = WamnStoreLimiter::new(MEMORY_CAP_BYTES, Arc::from(&*scope));
        store.limiter(|ctx| &mut ctx.wamn_limiter);
        store.set_epoch_deadline(1);
        let compiled = workload.component().clone();
        let node = bindings::Node::instantiate_async(&mut store, &compiled, workload.linker())
            .await
            .map_err(|error| anyhow::anyhow!("instantiate wamn:node: {error}"))?;
        Ok(Self {
            store,
            node,
            postgres,
            logging,
            connection_http,
            scope,
            epoch_tick: config.epoch_tick,
        })
    }

    async fn run(
        &mut self,
        context: &node_types::NodeContext,
        input: &String,
        deadline_ms: u64,
    ) -> anyhow::Result<Result<node_types::Emission, node_types::NodeError>> {
        self.store
            .set_epoch_deadline(deadline_ticks(deadline_ms, self.epoch_tick));
        self.node
            .wamn_node_handler()
            .call_run(&mut self.store, context, input)
            .await
            .map_err(|error| anyhow::anyhow!("wamn:node handler.run trapped: {error}"))
    }
}

impl ReusableExecutionInstance for NodeInstance {
    type Identity = NodeAcquisition;
    type BindError = NodeIdentityBindError;
    type ResetError = NodeResetUnavailable;

    fn reserved_bytes(&self) -> usize {
        MEMORY_CAP_BYTES
    }

    fn bind_identity(&mut self, identity: &Self::Identity) -> Result<(), Self::BindError> {
        self.postgres
            .bind_session_claims(&self.scope, &identity.claims)
            .map_err(NodeIdentityBindError)?;
        self.logging.set_claim(
            &self.scope,
            &identity.claims.tenant,
            identity
                .claims
                .project
                .as_deref()
                .unwrap_or(wamn_runtime::plugins::wamn_postgres::DEFAULT_PROJECT),
        );
        if let Err(error) = self
            .connection_http
            .bind_invocation(&self.scope, identity.invocation.clone())
        {
            self.logging.clear_claim(&self.scope);
            self.postgres.revoke_session_claims(&self.scope);
            return Err(NodeIdentityBindError(error));
        }
        Ok(())
    }

    fn revoke_identity(&mut self) {
        self.connection_http.revoke_invocation(&self.scope);
        self.logging.clear_claim(&self.scope);
        self.postgres.revoke_session_claims(&self.scope);
    }

    fn reset_invocation_state(&mut self) -> Result<(), Self::ResetError> {
        Err(NodeResetUnavailable)
    }
}

fn deadline_ticks(deadline_ms: u64, epoch_tick: Duration) -> u64 {
    let ticks = Duration::from_millis(deadline_ms)
        .as_nanos()
        .div_ceil(epoch_tick.as_nanos());
    u64::try_from(ticks).unwrap_or(u64::MAX).max(1)
}

fn bounded_node_deadline_ms(deadline_ms: Option<u64>) -> u64 {
    deadline_ms
        .unwrap_or(MAX_HOST_CALL_DURATION.as_millis() as u64)
        .clamp(1, MAX_HOST_CALL_DURATION.as_millis() as u64)
}

fn lower_node_outcome(
    outcome: Result<node_types::Emission, node_types::NodeError>,
) -> anyhow::Result<NodeOutcome> {
    match outcome {
        Ok(emission) => {
            let payload = serde_json::from_str(&emission.payload)
                .context("wamn:node emitted invalid JSON")?;
            Ok(NodeOutcome::Success {
                payload,
                port: emission
                    .port
                    .unwrap_or_else(|| wamn_router::MAIN_PORT.to_owned()),
            })
        }
        Err(node_types::NodeError::Retryable(detail)) => Ok(NodeOutcome::Error(
            NodeError::Retryable(lower_detail(detail)),
        )),
        Err(node_types::NodeError::RateLimited(detail)) => Ok(NodeOutcome::Error(
            NodeError::RateLimited(RateLimitDetail {
                detail: lower_detail(detail.detail),
                retry_after_ms: detail.retry_after_ms,
                target_host: None,
            }),
        )),
        Err(node_types::NodeError::Terminal(detail)) => Ok(NodeOutcome::Error(
            NodeError::Terminal(lower_detail(detail)),
        )),
        Err(node_types::NodeError::InvalidInput(detail)) => Ok(NodeOutcome::Error(
            NodeError::InvalidInput(lower_detail(detail)),
        )),
        Err(node_types::NodeError::Cancelled) => Ok(NodeOutcome::Cancelled),
    }
}

fn lower_detail(detail: node_types::ErrorDetail) -> ErrorDetail {
    ErrorDetail {
        message: detail.message,
        code: detail.code,
        data: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_deadline_is_nonzero_and_host_bounded() {
        let ceiling = MAX_HOST_CALL_DURATION.as_millis() as u64;

        assert_eq!(bounded_node_deadline_ms(None), ceiling);
        assert_eq!(bounded_node_deadline_ms(Some(0)), 1);
        assert_eq!(bounded_node_deadline_ms(Some(ceiling + 1)), ceiling);
        assert_eq!(bounded_node_deadline_ms(Some(17)), 17);
        assert_eq!(deadline_ticks(30, Duration::from_millis(7)), 5);
        assert_eq!(deadline_ticks(1, Duration::from_millis(10)), 1);
    }
}
