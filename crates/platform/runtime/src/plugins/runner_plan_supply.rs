//! Trusted immutable plan supply for the digest-pinned flowrunner.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::fmt::{Display, Write as _};
use std::sync::{Arc, Mutex};

use sha2::{Digest as _, Sha256};
use wash_runtime::engine::ctx::{ActiveCtx, SharedCtx, extract_active_ctx};
use wash_runtime::plugin::HostPlugin;
use wash_runtime::wasmtime::component::Linker;
use wash_runtime::wit::{WitInterface, WitWorld};

use super::wamn_postgres::{RunResolutionMetadata, WamnPostgres};

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        world: "runner-plan-supply-plugin",
        imports: { default: async | trappable | tracing },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

use bindings::wamn::runner::plan_supply::{
    self, ResolutionPlan, RunResolutionSnapshot, SupplyError,
};

pub const RUNNER_PLAN_SUPPLY_ID: &str = "wamn-runner-plan-supply";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PlanCacheKey {
    tenant_id: Arc<str>,
    execution_bundle_hash: Arc<str>,
}

#[derive(Debug)]
struct PlanCacheState {
    entries: HashMap<PlanCacheKey, Arc<[u8]>>,
    least_to_most_recent: VecDeque<PlanCacheKey>,
}

/// A deterministic entry-bounded cache keyed by tenant and exact plan identity.
#[derive(Debug)]
pub struct ResolutionPlanCache {
    max_entries: usize,
    state: Mutex<PlanCacheState>,
}

impl ResolutionPlanCache {
    pub fn new(max_entries: usize) -> Result<Self, InvalidPlanCacheLimit> {
        if max_entries == 0 {
            return Err(InvalidPlanCacheLimit);
        }
        Ok(Self {
            max_entries,
            state: Mutex::new(PlanCacheState {
                entries: HashMap::with_capacity(max_entries),
                least_to_most_recent: VecDeque::with_capacity(max_entries),
            }),
        })
    }

    fn get(&self, tenant_id: &str, execution_bundle_hash: &str) -> Option<Arc<[u8]>> {
        let mut state = self
            .state
            .lock()
            .expect("resolution plan cache lock poisoned");
        let key = PlanCacheKey {
            tenant_id: Arc::from(tenant_id),
            execution_bundle_hash: Arc::from(execution_bundle_hash),
        };
        let bytes = state.entries.get(&key)?.clone();
        touch(&mut state.least_to_most_recent, &key);
        Some(bytes)
    }

    fn insert_verified(
        &self,
        tenant_id: &str,
        execution_bundle_hash: &str,
        exact_bytes: Vec<u8>,
    ) -> Result<Arc<[u8]>, PlanSupplyFailure> {
        if execution_bundle_hash_of(&exact_bytes) != execution_bundle_hash {
            return Err(PlanSupplyFailure::new(
                PlanSupplyFailureKind::HashMismatch,
                format!("plan bytes do not match {execution_bundle_hash}"),
            ));
        }
        let key = PlanCacheKey {
            tenant_id: Arc::from(tenant_id),
            execution_bundle_hash: Arc::from(execution_bundle_hash),
        };
        let mut state = self
            .state
            .lock()
            .expect("resolution plan cache lock poisoned");
        if let Some(bytes) = state.entries.get(&key).cloned() {
            touch(&mut state.least_to_most_recent, &key);
            return Ok(bytes);
        }
        while state.entries.len() >= self.max_entries {
            let evicted = state
                .least_to_most_recent
                .pop_front()
                .expect("non-empty cache has an eviction key");
            state.entries.remove(&evicted);
        }
        let bytes: Arc<[u8]> = exact_bytes.into();
        state.entries.insert(key.clone(), bytes.clone());
        state.least_to_most_recent.push_back(key);
        Ok(bytes)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.state
            .lock()
            .expect("resolution plan cache lock poisoned")
            .entries
            .len()
    }
}

fn touch(order: &mut VecDeque<PlanCacheKey>, key: &PlanCacheKey) {
    if let Some(position) = order.iter().position(|candidate| candidate == key) {
        order.remove(position);
    }
    order.push_back(key.clone());
}

/// The cache must retain at least one immutable plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidPlanCacheLimit;

impl Display for InvalidPlanCacheLimit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("resolution plan cache entry limit must be non-zero")
    }
}

impl std::error::Error for InvalidPlanCacheLimit {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlanSupplyFailureKind {
    NotFound,
    Incomplete,
    HashMismatch,
    Unavailable,
}

#[derive(Debug)]
struct PlanSupplyFailure {
    kind: PlanSupplyFailureKind,
    detail: String,
}

impl PlanSupplyFailure {
    fn new(kind: PlanSupplyFailureKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}

impl Display for PlanSupplyFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for PlanSupplyFailure {}

/// Host-owned database and cache boundary for immutable per-run plans.
pub struct RunnerPlanSupply {
    postgres: Arc<WamnPostgres>,
    cache: ResolutionPlanCache,
}

impl std::fmt::Debug for RunnerPlanSupply {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RunnerPlanSupply")
            .field("cache", &self.cache)
            .finish_non_exhaustive()
    }
}

impl RunnerPlanSupply {
    pub fn new(
        postgres: Arc<WamnPostgres>,
        max_cache_entries: usize,
    ) -> Result<Self, InvalidPlanCacheLimit> {
        Ok(Self {
            postgres,
            cache: ResolutionPlanCache::new(max_cache_entries)?,
        })
    }

    async fn load_run_snapshot(
        &self,
        component_id: &str,
        run_id: &str,
    ) -> Result<RunResolutionSnapshot, PlanSupplyFailure> {
        let metadata = self
            .postgres
            .run_resolution_metadata(component_id, run_id)
            .await
            .map_err(|error| {
                PlanSupplyFailure::new(PlanSupplyFailureKind::Unavailable, error.to_string())
            })?
            .ok_or_else(|| {
                PlanSupplyFailure::new(
                    PlanSupplyFailureKind::NotFound,
                    format!("run {run_id:?} has no immutable resolution map"),
                )
            })?;
        validate_metadata(&metadata)?;

        // Retain one run-local view while populating the bounded cross-run cache.
        // A legal snapshot may exceed the cache capacity; eviction must never make
        // that one invocation incomplete.
        let mut exact_by_hash = HashMap::with_capacity(metadata.plans.len());
        let mut missing = BTreeSet::new();
        for plan in &metadata.plans {
            match self
                .cache
                .get(&metadata.tenant_id, &plan.execution_bundle_hash)
            {
                Some(bytes) => {
                    exact_by_hash.insert(plan.execution_bundle_hash.clone(), bytes);
                }
                None => {
                    missing.insert(plan.execution_bundle_hash.clone());
                }
            }
        }
        let missing = missing.into_iter().collect::<Vec<_>>();
        let loaded = self
            .postgres
            .resolution_plan_bytes(component_id, &missing)
            .await
            .map_err(|error| {
                PlanSupplyFailure::new(PlanSupplyFailureKind::Unavailable, error.to_string())
            })?;
        if loaded.len() != missing.len() {
            return Err(PlanSupplyFailure::new(
                PlanSupplyFailureKind::Incomplete,
                "one or more immutable resolution plans are missing",
            ));
        }
        for plan in loaded {
            let bytes = self.cache.insert_verified(
                &metadata.tenant_id,
                &plan.execution_bundle_hash,
                plan.exact_bytes,
            )?;
            exact_by_hash.insert(plan.execution_bundle_hash, bytes);
        }

        let plans = metadata
            .plans
            .into_iter()
            .map(|plan| {
                let exact_bytes = exact_by_hash
                    .get(&plan.execution_bundle_hash)
                    .cloned()
                    .ok_or_else(|| {
                        PlanSupplyFailure::new(
                            PlanSupplyFailureKind::Incomplete,
                            format!(
                                "verified plan {} is absent from the bounded cache",
                                plan.execution_bundle_hash
                            ),
                        )
                    })?;
                Ok(ResolutionPlan {
                    flow_id: plan.flow_id,
                    execution_bundle_hash: plan.execution_bundle_hash,
                    source_artifact_hash: plan.source_artifact_hash,
                    exact_bytes: exact_bytes.to_vec(),
                })
            })
            .collect::<Result<Vec<_>, PlanSupplyFailure>>()?;
        Ok(RunResolutionSnapshot {
            root_flow_id: metadata.root_flow_id,
            root_execution_bundle_hash: metadata.root_execution_bundle_hash,
            plans,
        })
    }
}

fn validate_metadata(metadata: &RunResolutionMetadata) -> Result<(), PlanSupplyFailure> {
    let mut flow_ids = HashSet::with_capacity(metadata.plans.len());
    let mut found_root = false;
    for plan in &metadata.plans {
        if !flow_ids.insert(plan.flow_id.as_str()) {
            return Err(PlanSupplyFailure::new(
                PlanSupplyFailureKind::Incomplete,
                format!("resolution map repeats flow {:?}", plan.flow_id),
            ));
        }
        if plan.flow_id == metadata.root_flow_id
            && plan.execution_bundle_hash == metadata.root_execution_bundle_hash
        {
            found_root = true;
        }
    }
    if !found_root {
        return Err(PlanSupplyFailure::new(
            PlanSupplyFailureKind::Incomplete,
            "resolution map omits the run's exact root plan",
        ));
    }
    Ok(())
}

fn execution_bundle_hash_of(exact_bytes: &[u8]) -> String {
    let digest = Sha256::digest(exact_bytes);
    let mut hash = String::with_capacity(71);
    hash.push_str("sha256:");
    for byte in digest {
        write!(hash, "{byte:02x}").expect("writing to String cannot fail");
    }
    hash
}

pub fn add_to_linker(linker: &mut Linker<SharedCtx>) -> wash_runtime::wasmtime::Result<()> {
    plan_supply::add_to_linker::<_, SharedCtx>(linker, extract_active_ctx)
}

impl HostPlugin for RunnerPlanSupply {
    fn id(&self) -> &'static str {
        RUNNER_PLAN_SUPPLY_ID
    }

    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::from([WitInterface::from("wamn:runner/plan-supply@0.1.0")]),
            exports: HashSet::new(),
        }
    }
}

fn plugin_of(ctx: &ActiveCtx<'_>) -> wash_runtime::wasmtime::Result<Arc<RunnerPlanSupply>> {
    ctx.try_get_plugin::<RunnerPlanSupply>(RUNNER_PLAN_SUPPLY_ID)
}

impl plan_supply::Host for ActiveCtx<'_> {
    async fn load_run_snapshot(
        &mut self,
        run_id: String,
    ) -> wash_runtime::wasmtime::Result<Result<RunResolutionSnapshot, SupplyError>> {
        let plugin = plugin_of(self)?;
        let result = plugin
            .load_run_snapshot(self.component_id.as_ref(), &run_id)
            .await;
        Ok(result.map_err(|error| {
            tracing::warn!(run_id, error = %error, "immutable plan supply refused");
            match error.kind {
                PlanSupplyFailureKind::NotFound => SupplyError::NotFound,
                PlanSupplyFailureKind::Incomplete => SupplyError::Incomplete,
                PlanSupplyFailureKind::HashMismatch => SupplyError::HashMismatch,
                PlanSupplyFailureKind::Unavailable => SupplyError::Unavailable,
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::wamn_postgres::ResolutionPlanMetadata;

    fn metadata(plans: Vec<ResolutionPlanMetadata>) -> RunResolutionMetadata {
        RunResolutionMetadata {
            tenant_id: "tenant-a".into(),
            root_flow_id: "root".into(),
            root_execution_bundle_hash: "sha256:root".into(),
            plans,
        }
    }

    fn plan(flow_id: &str, hash: &str) -> ResolutionPlanMetadata {
        ResolutionPlanMetadata {
            flow_id: flow_id.into(),
            execution_bundle_hash: hash.into(),
            source_artifact_hash: format!("artifact-{flow_id}"),
        }
    }

    #[test]
    fn cache_is_entry_bounded_and_tenant_scoped() {
        let cache = ResolutionPlanCache::new(2).unwrap();
        let a = execution_bundle_hash_of(b"a");
        let b = execution_bundle_hash_of(b"b");
        let c = execution_bundle_hash_of(b"c");
        cache
            .insert_verified("tenant-a", &a, b"a".to_vec())
            .unwrap();
        cache
            .insert_verified("tenant-a", &b, b"b".to_vec())
            .unwrap();
        assert!(cache.get("tenant-b", &a).is_none());
        assert_eq!(cache.get("tenant-a", &a).as_deref(), Some(b"a".as_slice()));
        cache
            .insert_verified("tenant-a", &c, b"c".to_vec())
            .unwrap();
        assert_eq!(cache.len(), 2);
        assert!(cache.get("tenant-a", &b).is_none());
        assert!(cache.get("tenant-a", &a).is_some());
        assert!(cache.get("tenant-a", &c).is_some());
    }

    #[test]
    fn hash_mismatch_never_enters_the_cache() {
        let cache = ResolutionPlanCache::new(1).unwrap();
        let expected = execution_bundle_hash_of(b"expected");
        let error = cache
            .insert_verified("tenant-a", &expected, b"forged".to_vec())
            .unwrap_err();
        assert_eq!(error.kind, PlanSupplyFailureKind::HashMismatch);
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn metadata_requires_one_exact_root_and_unique_flow_names() {
        let valid = metadata(vec![
            plan("root", "sha256:root"),
            plan("callee", "sha256:c"),
        ]);
        validate_metadata(&valid).unwrap();

        let missing = metadata(vec![plan("root", "sha256:other")]);
        assert_eq!(
            validate_metadata(&missing).unwrap_err().kind,
            PlanSupplyFailureKind::Incomplete
        );

        let duplicate = metadata(vec![
            plan("root", "sha256:root"),
            plan("root", "sha256:root"),
        ]);
        assert_eq!(
            validate_metadata(&duplicate).unwrap_err().kind,
            PlanSupplyFailureKind::Incomplete
        );
    }

    #[test]
    fn zero_entry_cache_is_refused() {
        assert_eq!(
            ResolutionPlanCache::new(0).unwrap_err().to_string(),
            "resolution plan cache entry limit must be non-zero"
        );
    }
}
