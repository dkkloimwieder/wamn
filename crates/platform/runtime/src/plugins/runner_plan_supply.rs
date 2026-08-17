//! Trusted immutable plan supply for the digest-pinned flowrunner.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::fmt::{Display, Write as _};
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};

use sha2::{Digest as _, Sha256};
use wash_runtime::engine::ctx::{ActiveCtx, SharedCtx, extract_active_ctx};
use wash_runtime::plugin::HostPlugin;
use wash_runtime::wasmtime::component::Linker;
use wash_runtime::wit::{WitInterface, WitWorld};

use super::wamn_postgres::{RunReleaseBinding, WamnPostgres};
use crate::plan_artifact::{PlanFetchErrorKind, PlanSource};
use crate::release_manifest::ReleaseManifestWeld;

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

/// Deterministic entry-bounded cache keyed by tenant and exact plan identity.
#[derive(Debug)]
pub(crate) struct ResolutionPlanCache {
    max_entries: usize,
    state: Mutex<PlanCacheState>,
}

impl ResolutionPlanCache {
    pub(crate) fn new(max_entries: usize) -> Result<Self, InvalidPlanCacheLimit> {
        let max_entries = NonZeroUsize::new(max_entries).ok_or(InvalidPlanCacheLimit)?;
        Ok(Self::from_non_zero(max_entries))
    }

    fn from_non_zero(max_entries: NonZeroUsize) -> Self {
        Self {
            max_entries: max_entries.get(),
            state: Mutex::new(PlanCacheState {
                entries: HashMap::with_capacity(max_entries.get()),
                least_to_most_recent: VecDeque::with_capacity(max_entries.get()),
            }),
        }
    }

    pub(crate) fn get(&self, tenant_id: &str, execution_bundle_hash: &str) -> Option<Arc<[u8]>> {
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

    /// Insert bytes after the calling owner has completed its verification.
    /// The fetched-byte path carries a SHA-only contract.
    pub(crate) fn insert_verified(
        &self,
        tenant_id: &str,
        execution_bundle_hash: &str,
        exact_bytes: Vec<u8>,
    ) -> Arc<[u8]> {
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
            return bytes;
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
        bytes
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
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

/// The one release a serving process resolves plans against.
///
/// Reader 1 of the four the weld enumerates: it consults the weld by reference
/// and never loads, parses or digest-verifies a manifest of its own.
pub struct PlanRelease {
    weld: Arc<ReleaseManifestWeld>,
    source: Arc<dyn PlanSource>,
}

impl std::fmt::Debug for PlanRelease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PlanRelease")
            .field("release", self.weld.release())
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

impl PlanRelease {
    /// Bind the loaded weld to the source its plan bytes are pulled from.
    pub fn new(weld: Arc<ReleaseManifestWeld>, source: Arc<dyn PlanSource>) -> Self {
        Self { weld, source }
    }

    /// The weld this release resolves against.
    ///
    /// Exposed for the construction site, which injects the same object's
    /// `(release version, manifest digest)` pair into the claim path
    /// (`wamn-0h0g.15.103`). Reading it from here rather than keeping a second
    /// handle beside it is what makes the pair the pod RECORDS and the manifest
    /// the pod RESOLVES AGAINST provably the same object.
    pub fn weld(&self) -> &ReleaseManifestWeld {
        &self.weld
    }
}

/// Host-owned release and cache boundary for immutable per-run plans.
pub struct RunnerPlanSupply {
    postgres: Arc<WamnPostgres>,
    /// `None` in a process that was given no manifest root — see
    /// [`RunnerPlanSupply::new`].
    release: Option<PlanRelease>,
    cache: ResolutionPlanCache,
}

impl std::fmt::Debug for RunnerPlanSupply {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RunnerPlanSupply")
            .field("release", &self.release)
            .field("cache", &self.cache)
            .finish_non_exhaustive()
    }
}

impl RunnerPlanSupply {
    /// `release` is `None` when this process carries no release.
    ///
    /// Absence is a deployment fact, not a fallback: a gate, a bench or any other
    /// non-serving path constructs the flowrunner host with nothing to mount and
    /// nothing to pull from, and every `load_run_snapshot` on it reports
    /// `unavailable` rather than resolving plans from somewhere else. A *serving*
    /// pod is given a manifest root, and a manifest root it cannot load refuses
    /// host construction outright — that decision is made at the construction
    /// site (`crates/execution/host/src/lib.rs`), where it is visible, not here
    /// behind an `Option`.
    pub fn new(
        postgres: Arc<WamnPostgres>,
        release: Option<PlanRelease>,
        max_cache_entries: usize,
    ) -> Result<Self, InvalidPlanCacheLimit> {
        Ok(Self {
            postgres,
            release,
            cache: ResolutionPlanCache::new(max_cache_entries)?,
        })
    }

    async fn load_run_snapshot(
        &self,
        component_id: &str,
        run_id: &str,
    ) -> Result<RunResolutionSnapshot, PlanSupplyFailure> {
        let release = self.release.as_ref().ok_or_else(|| {
            PlanSupplyFailure::new(
                PlanSupplyFailureKind::Unavailable,
                "this process carries no release manifest",
            )
        })?;
        let binding = self
            .postgres
            .run_release_binding(component_id, run_id)
            .await
            .map_err(|error| {
                PlanSupplyFailure::new(PlanSupplyFailureKind::Unavailable, error.to_string())
            })?
            .ok_or_else(|| {
                PlanSupplyFailure::new(
                    PlanSupplyFailureKind::NotFound,
                    format!("run {run_id:?} has no run row in this tenant"),
                )
            })?;
        resolve_release_snapshot(release, &self.cache, &binding).await
    }
}

/// Resolve one run's plan set out of the release this process carries.
///
/// Free of the database and of `self` so the three pre-effect dispositions can be
/// proven against a stub source without a live Postgres.
async fn resolve_release_snapshot(
    release: &PlanRelease,
    cache: &ResolutionPlanCache,
    binding: &RunReleaseBinding,
) -> Result<RunResolutionSnapshot, PlanSupplyFailure> {
    let carried = release.weld.release();
    // The run-scoped binding, restored on the supply side: a run admitted under
    // one release must not be resolved against another pod's. A NULL digest is a
    // row from before anything wrote that column — nothing does until
    // wamn-0h0g.15.101 feeds the claim — and is admitted rather than refused,
    // because refusing it would stop every run that exists today.
    if let Some(recorded) = binding.manifest_digest.as_deref()
        && recorded != carried.manifest_digest.as_str()
    {
        return Err(PlanSupplyFailure::new(
            PlanSupplyFailureKind::Incomplete,
            format!(
                "run was admitted under serving manifest {recorded}, this pod serves {}",
                carried.manifest_digest
            ),
        ));
    }

    let manifest = release.weld.manifest();
    // The digest check above cannot carry this one: `runs.manifest_digest` is NULL
    // on every row that exists today, so without it a pod mounted with the wrong
    // tenant's manifest would hand that tenant's plans to this tenant's runs. One
    // pod serves one project and therefore one tenant, and a release is minted for
    // exactly one — so disagreement is a mis-mounted pod, never a legal shape.
    if manifest.release.tenant_id != binding.tenant_id {
        return Err(PlanSupplyFailure::new(
            PlanSupplyFailureKind::Incomplete,
            format!(
                "serving manifest {} was minted for tenant {:?}, this run is {:?}",
                carried.manifest_digest, manifest.release.tenant_id, binding.tenant_id
            ),
        ));
    }

    let reachable = manifest.reachable_flows(&binding.flow_id);
    if reachable.is_empty() {
        return Err(PlanSupplyFailure::new(
            PlanSupplyFailureKind::Incomplete,
            format!(
                "serving manifest {} does not contain root flow {:?}",
                carried.manifest_digest, binding.flow_id
            ),
        ));
    }
    // `reachable_flows` is total over its own flow map, so every id it yields is
    // a member. Anything else is a broken manifest type, not bad input.
    let wanted = reachable
        .iter()
        .map(|flow_id| {
            let flow = manifest
                .flows
                .get(flow_id)
                .expect("reachable_flows yields member flows");
            (flow_id, &flow.plan_hash, &flow.source_artifact)
        })
        .collect::<Vec<_>>();

    // Retain one run-local view while populating the bounded cross-run cache.
    // A legal snapshot may exceed the cache capacity; eviction must never make
    // that one invocation incomplete.
    let mut exact_by_hash = HashMap::with_capacity(wanted.len());
    let mut missing = BTreeSet::new();
    for (_, plan_hash, _) in &wanted {
        match cache.get(&binding.tenant_id, plan_hash) {
            Some(bytes) => {
                exact_by_hash.insert(plan_hash.as_str(), bytes);
            }
            None => {
                missing.insert(plan_hash.as_str());
            }
        }
    }
    for plan_hash in missing {
        // No project lock is held across this fetch, and it happens before any
        // effect: a source that is slow, absent or unreachable releases the run
        // to be requeued rather than half-executing it.
        let exact_bytes = release.source.fetch(plan_hash).await.map_err(|error| {
            let kind = match error.kind() {
                PlanFetchErrorKind::Unavailable => PlanSupplyFailureKind::Unavailable,
                PlanFetchErrorKind::Mismatched => PlanSupplyFailureKind::HashMismatch,
            };
            PlanSupplyFailure::new(kind, error.to_string())
        })?;
        let bytes = insert_verified(cache, &binding.tenant_id, plan_hash, exact_bytes)?;
        exact_by_hash.insert(plan_hash, bytes);
    }

    let plans = wanted
        .into_iter()
        .map(|(flow_id, plan_hash, source_artifact)| {
            let exact_bytes = exact_by_hash
                .get(plan_hash.as_str())
                .expect("every wanted plan is in the run-local view");
            ResolutionPlan {
                flow_id: flow_id.clone(),
                execution_bundle_hash: plan_hash.clone(),
                source_artifact_hash: source_artifact.clone(),
                exact_bytes: exact_bytes.to_vec(),
            }
        })
        .collect::<Vec<_>>();
    let root_execution_bundle_hash = manifest
        .flows
        .get(&binding.flow_id)
        .expect("a non-empty reachable set proves the root is a member")
        .plan_hash
        .clone();
    Ok(RunResolutionSnapshot {
        root_flow_id: binding.flow_id.clone(),
        root_execution_bundle_hash,
        plans,
    })
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

fn insert_verified(
    cache: &ResolutionPlanCache,
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
    Ok(cache.insert_verified(tenant_id, execution_bundle_hash, exact_bytes))
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
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use wamn_catalog::{RELEASE_MANIFEST_FILE_NAME, ServingFlow, ServingManifest, ServingRelease};

    use super::*;
    use crate::plan_artifact::PlanFetchError;
    use crate::plugins::wamn_postgres::WamnPostgresConfig;

    const TENANT: &str = "tenant-a";
    const ROOT_BYTES: &[u8] = b"root-plan-bytes";
    const CALLEE_BYTES: &[u8] = b"callee-plan-bytes";
    const ROOT_ARTIFACT: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const CALLEE_ARTIFACT: &str =
        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const OTHER_RELEASE: &str =
        "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

    /// A source that answers from memory and counts what was asked of it.
    ///
    /// Stubbed rather than pointed at a registry because what these tests prove is
    /// the supply logic and its three pre-effect dispositions. The real transport
    /// is proven separately, against a throwaway registry, in
    /// `tests/oci_plan_source_live.rs`.
    #[derive(Debug, Default)]
    struct StubSource {
        published: HashMap<String, Vec<u8>>,
        fault: Option<PlanFetchErrorKind>,
        fetches: AtomicUsize,
    }

    impl StubSource {
        fn publishing(plans: &[&[u8]]) -> Arc<Self> {
            Arc::new(Self {
                published: plans
                    .iter()
                    .map(|bytes| (execution_bundle_hash_of(bytes), bytes.to_vec()))
                    .collect(),
                ..Self::default()
            })
        }

        /// Publish `bytes` under a digest they do not hash to.
        fn forging(named: &str, bytes: &[u8]) -> Arc<Self> {
            Arc::new(Self {
                published: HashMap::from([(named.to_string(), bytes.to_vec())]),
                ..Self::default()
            })
        }

        fn faulting(kind: PlanFetchErrorKind) -> Arc<Self> {
            Arc::new(Self {
                fault: Some(kind),
                ..Self::default()
            })
        }

        fn fetches(&self) -> usize {
            self.fetches.load(Ordering::Relaxed)
        }
    }

    #[async_trait]
    impl PlanSource for StubSource {
        async fn fetch(&self, plan_hash: &str) -> Result<Vec<u8>, PlanFetchError> {
            self.fetches.fetch_add(1, Ordering::Relaxed);
            if let Some(kind) = self.fault {
                return Err(PlanFetchError::new(kind, plan_hash, "stub fault"));
            }
            self.published.get(plan_hash).cloned().ok_or_else(|| {
                PlanFetchError::new(PlanFetchErrorKind::Unavailable, plan_hash, "not published")
            })
        }
    }

    fn flow(plan_bytes: &[u8], artifact: &str, calls: BTreeSet<String>) -> ServingFlow {
        ServingFlow {
            flow_version: 1,
            plan_hash: execution_bundle_hash_of(plan_bytes),
            source_artifact: artifact.into(),
            binding_base_artifact: artifact.into(),
            callable_contract: None,
            calls,
        }
    }

    /// `root` calls `callee`; both are members, so the reachable set is both.
    fn release_manifest() -> ServingManifest {
        ServingManifest::new(
            ServingRelease {
                tenant_id: TENANT.into(),
                catalog_id: "cat".into(),
                catalog_version: 4,
                environment: "prod".into(),
            },
            BTreeMap::from([
                (
                    "root".to_string(),
                    flow(
                        ROOT_BYTES,
                        ROOT_ARTIFACT,
                        BTreeSet::from(["callee".to_string()]),
                    ),
                ),
                (
                    "callee".to_string(),
                    flow(CALLEE_BYTES, CALLEE_ARTIFACT, BTreeSet::new()),
                ),
            ]),
            BTreeMap::new(),
            BTreeMap::new(),
        )
        .expect("fixture manifest is valid")
    }

    /// A scratch manifest mount, named for its test so runs cannot collide.
    ///
    /// The weld has exactly one constructor and it reads a file, so a test that
    /// needs a weld writes the mount. That is the point: there is no `from_parts`
    /// shortcut past the verification a serving pod performs, not even here.
    struct Mount {
        root: PathBuf,
    }

    impl Mount {
        fn holding(manifest: &ServingManifest, test: &str) -> Self {
            let root = std::env::temp_dir()
                .join(format!("wamn-plan-supply-{}-{test}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).expect("scratch mount");
            std::fs::write(
                root.join(RELEASE_MANIFEST_FILE_NAME),
                manifest.canonical_bytes(),
            )
            .expect("write manifest");
            Self { root }
        }

        fn weld(&self) -> Arc<ReleaseManifestWeld> {
            Arc::new(ReleaseManifestWeld::load_from(&self.root).expect("fixture mount loads"))
        }
    }

    impl Drop for Mount {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn binding(flow_id: &str, manifest_digest: Option<&str>) -> RunReleaseBinding {
        RunReleaseBinding {
            tenant_id: TENANT.to_string(),
            flow_id: flow_id.to_string(),
            manifest_digest: manifest_digest.map(str::to_string),
        }
    }

    #[tokio::test]
    async fn the_reachable_plan_set_is_supplied_from_the_manifest() {
        let manifest = release_manifest();
        let mount = Mount::holding(&manifest, "reachable");
        let weld = mount.weld();
        let source = StubSource::publishing(&[ROOT_BYTES, CALLEE_BYTES]);
        let release = PlanRelease::new(weld.clone(), source.clone());
        let cache = ResolutionPlanCache::new(8).unwrap();

        let snapshot = resolve_release_snapshot(&release, &cache, &binding("root", None))
            .await
            .expect("the release contains the root flow");

        assert_eq!(snapshot.root_flow_id, "root");
        assert_eq!(
            snapshot.root_execution_bundle_hash,
            execution_bundle_hash_of(ROOT_BYTES)
        );
        // Both the plan digest and the source artifact come from the manifest —
        // the fields the deleted per-run resolution map used to carry.
        let supplied = snapshot
            .plans
            .iter()
            .map(|plan| {
                (
                    plan.flow_id.as_str(),
                    plan.execution_bundle_hash.as_str(),
                    plan.source_artifact_hash.as_str(),
                    plan.exact_bytes.as_slice(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            supplied,
            vec![
                (
                    "callee",
                    execution_bundle_hash_of(CALLEE_BYTES).as_str(),
                    CALLEE_ARTIFACT,
                    CALLEE_BYTES
                ),
                (
                    "root",
                    execution_bundle_hash_of(ROOT_BYTES).as_str(),
                    ROOT_ARTIFACT,
                    ROOT_BYTES
                ),
            ]
        );
        assert_eq!(source.fetches(), 2);
    }

    /// Pre-effect disposition 1: the digest is absent from the release, so this
    /// deployment can never serve the run. Deployment-invalid, not retryable.
    #[tokio::test]
    async fn a_root_flow_absent_from_the_release_is_a_deployment_invalid_refusal() {
        let manifest = release_manifest();
        let mount = Mount::holding(&manifest, "absent-root");
        let release = PlanRelease::new(mount.weld(), StubSource::publishing(&[ROOT_BYTES]));
        let cache = ResolutionPlanCache::new(8).unwrap();

        let error = resolve_release_snapshot(&release, &cache, &binding("stranger", None))
            .await
            .expect_err("a non-member root refuses");

        assert_eq!(error.kind, PlanSupplyFailureKind::Incomplete);
    }

    /// Pre-effect disposition 2: the fetch failed before any effect ran, so the
    /// run is released to be requeued rather than half-executed.
    #[tokio::test]
    async fn a_fetch_fault_before_effects_releases_the_run() {
        let manifest = release_manifest();
        let mount = Mount::holding(&manifest, "fetch-fault");
        let release = PlanRelease::new(
            mount.weld(),
            StubSource::faulting(PlanFetchErrorKind::Unavailable),
        );
        let cache = ResolutionPlanCache::new(8).unwrap();

        let error = resolve_release_snapshot(&release, &cache, &binding("root", None))
            .await
            .expect_err("an unreachable source refuses");

        assert_eq!(error.kind, PlanSupplyFailureKind::Unavailable);
        assert_eq!(cache.len(), 0);
    }

    /// Pre-effect disposition 3: bytes that do not hash to the digest the release
    /// names are an integrity refusal, and they never reach the cache or a guest.
    #[tokio::test]
    async fn forged_plan_bytes_are_an_integrity_refusal() {
        let manifest = release_manifest();
        let mount = Mount::holding(&manifest, "forged");
        let source = StubSource::forging(&execution_bundle_hash_of(ROOT_BYTES), b"forged");
        let release = PlanRelease::new(mount.weld(), source);
        let cache = ResolutionPlanCache::new(8).unwrap();

        let error = resolve_release_snapshot(&release, &cache, &binding("root", None))
            .await
            .expect_err("forged bytes refuse");

        assert_eq!(error.kind, PlanSupplyFailureKind::HashMismatch);
        assert_eq!(cache.len(), 0);
    }

    /// The same integrity refusal reached from the transport's own check: an
    /// artifact whose layer disagrees with the name it was published under.
    #[tokio::test]
    async fn an_artifact_that_disagrees_with_its_name_is_an_integrity_refusal() {
        let manifest = release_manifest();
        let mount = Mount::holding(&manifest, "mismatched-artifact");
        let release = PlanRelease::new(
            mount.weld(),
            StubSource::faulting(PlanFetchErrorKind::Mismatched),
        );
        let cache = ResolutionPlanCache::new(8).unwrap();

        let error = resolve_release_snapshot(&release, &cache, &binding("root", None))
            .await
            .expect_err("a mismatched artifact refuses");

        assert_eq!(error.kind, PlanSupplyFailureKind::HashMismatch);
    }

    #[tokio::test]
    async fn a_run_admitted_under_another_release_refuses() {
        let manifest = release_manifest();
        let mount = Mount::holding(&manifest, "foreign-release");
        let release = PlanRelease::new(
            mount.weld(),
            StubSource::publishing(&[ROOT_BYTES, CALLEE_BYTES]),
        );
        let cache = ResolutionPlanCache::new(8).unwrap();

        let error =
            resolve_release_snapshot(&release, &cache, &binding("root", Some(OTHER_RELEASE)))
                .await
                .expect_err("a foreign release refuses");

        assert_eq!(error.kind, PlanSupplyFailureKind::Incomplete);
    }

    /// The leak the digest binding cannot catch while every recorded digest is
    /// NULL: a pod mounted with another tenant's manifest.
    #[tokio::test]
    async fn a_manifest_minted_for_another_tenant_refuses() {
        let manifest = release_manifest();
        let mount = Mount::holding(&manifest, "foreign-tenant");
        let release = PlanRelease::new(
            mount.weld(),
            StubSource::publishing(&[ROOT_BYTES, CALLEE_BYTES]),
        );
        let cache = ResolutionPlanCache::new(8).unwrap();
        let foreign = RunReleaseBinding {
            tenant_id: "tenant-b".to_string(),
            flow_id: "root".to_string(),
            manifest_digest: None,
        };

        let error = resolve_release_snapshot(&release, &cache, &foreign)
            .await
            .expect_err("a foreign tenant refuses");

        assert_eq!(error.kind, PlanSupplyFailureKind::Incomplete);
        assert_eq!(
            cache.len(),
            0,
            "no plan may be fetched for a foreign tenant"
        );
    }

    #[tokio::test]
    async fn the_recorded_digest_this_pod_serves_is_admitted() {
        let manifest = release_manifest();
        let mount = Mount::holding(&manifest, "own-release");
        let weld = mount.weld();
        let recorded = weld.release().manifest_digest.as_str().to_string();
        let release = PlanRelease::new(weld, StubSource::publishing(&[ROOT_BYTES, CALLEE_BYTES]));
        let cache = ResolutionPlanCache::new(8).unwrap();

        resolve_release_snapshot(&release, &cache, &binding("root", Some(&recorded)))
            .await
            .expect("this pod's own release resolves");
    }

    /// Nothing writes `runs.manifest_digest` until wamn-0h0g.15.101 feeds the
    /// claim, so every run that exists today has NULL there. Refusing it would
    /// refuse all of them.
    #[tokio::test]
    async fn a_null_recorded_digest_is_admitted_as_a_pre_release_model_row() {
        let manifest = release_manifest();
        let mount = Mount::holding(&manifest, "null-digest");
        let release = PlanRelease::new(
            mount.weld(),
            StubSource::publishing(&[ROOT_BYTES, CALLEE_BYTES]),
        );
        let cache = ResolutionPlanCache::new(8).unwrap();

        resolve_release_snapshot(&release, &cache, &binding("root", None))
            .await
            .expect("a pre-release-model row resolves");
    }

    #[tokio::test]
    async fn verified_plans_are_not_refetched() {
        let manifest = release_manifest();
        let mount = Mount::holding(&manifest, "cached");
        let weld = mount.weld();
        let source = StubSource::publishing(&[ROOT_BYTES, CALLEE_BYTES]);
        let release = PlanRelease::new(weld, source.clone());
        let cache = ResolutionPlanCache::new(8).unwrap();

        for _ in 0..3 {
            resolve_release_snapshot(&release, &cache, &binding("root", None))
                .await
                .expect("every resolve succeeds");
        }

        assert_eq!(source.fetches(), 2, "only the first resolve pulls");
    }

    /// R2: a process given no manifest root has no release, and says so rather
    /// than resolving plans from anywhere else. Reached before the database, so
    /// this holds with no Postgres configured.
    #[tokio::test]
    async fn a_process_without_a_release_reports_unavailable() {
        let postgres = Arc::new(
            WamnPostgres::new(WamnPostgresConfig {
                database_url: None,
                pool_max_size: 1,
                wait_timeout_ms: 1_000,
                statement_timeout_ms: 1_000,
                row_limit: 1,
            })
            .unwrap(),
        );
        let supply = RunnerPlanSupply::new(postgres, None, 8).unwrap();

        let error = supply
            .load_run_snapshot("owner-1", "run-1")
            .await
            .expect_err("no release refuses");

        assert_eq!(error.kind, PlanSupplyFailureKind::Unavailable);
    }

    #[test]
    fn cache_is_entry_bounded_and_tenant_scoped() {
        let cache = ResolutionPlanCache::new(2).unwrap();
        let a = execution_bundle_hash_of(b"a");
        let b = execution_bundle_hash_of(b"b");
        let c = execution_bundle_hash_of(b"c");
        insert_verified(&cache, "tenant-a", &a, b"a".to_vec()).unwrap();
        insert_verified(&cache, "tenant-a", &b, b"b".to_vec()).unwrap();
        assert!(cache.get("tenant-b", &a).is_none());
        assert_eq!(cache.get("tenant-a", &a).as_deref(), Some(b"a".as_slice()));
        insert_verified(&cache, "tenant-a", &c, b"c".to_vec()).unwrap();
        assert_eq!(cache.len(), 2);
        assert!(cache.get("tenant-a", &b).is_none());
        assert!(cache.get("tenant-a", &a).is_some());
        assert!(cache.get("tenant-a", &c).is_some());
    }

    #[test]
    fn hash_mismatch_never_enters_the_cache() {
        let cache = ResolutionPlanCache::new(1).unwrap();
        let expected = execution_bundle_hash_of(b"expected");
        let error = insert_verified(&cache, "tenant-a", &expected, b"forged".to_vec()).unwrap_err();
        assert_eq!(error.kind, PlanSupplyFailureKind::HashMismatch);
        assert_eq!(cache.len(), 0);
    }

    // `metadata_requires_one_exact_root_and_unique_flow_names` is deleted, not
    // ported (wamn-0h0g.15.12). It asserted that a resolution map named its root
    // exactly once and repeated no flow. Both are now properties of the type the
    // set comes from: `ServingManifest::flows` is a `BTreeMap`, so a repeated flow
    // id cannot be represented, and the root is looked up in that map rather than
    // searched for in a list. Keeping the test would have asserted ground truth
    // about `BTreeMap` (M-TAUTOLOGICAL-TESTS). What replaced its real content is
    // `a_root_flow_absent_from_the_release_is_a_deployment_invalid_refusal`.

    #[test]
    fn zero_entry_cache_is_refused() {
        assert_eq!(
            ResolutionPlanCache::new(0).unwrap_err().to_string(),
            "resolution plan cache entry limit must be non-zero"
        );
    }
}
