//! Entitlement-aware single-flight cache for composed execution bundles.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Debug, Display};
use std::future::Future;
use std::sync::{Arc, Mutex};

use tokio::sync::{Semaphore, watch};
use wamn_catalog::ExecutionBundleIdentity;

/// Whether an input is eligible for PLAN-2A's global cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundleInputScope {
    FirstParty,
    EntitlementScoped,
}

/// One input plug or adapter whose entitlement must be checked before delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleCacheResource {
    identity: Arc<str>,
    scope: BundleInputScope,
}

impl BundleCacheResource {
    pub fn new(identity: impl Into<Arc<str>>, scope: BundleInputScope) -> Self {
        Self {
            identity: identity.into(),
            scope,
        }
    }

    pub fn identity(&self) -> &str {
        &self.identity
    }

    pub const fn scope(&self) -> BundleInputScope {
        self.scope
    }
}

/// Cache key derived from canonical bundle identity plus cache-policy metadata.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct FirstPartyBundleKey {
    identity_hash: Arc<str>,
    platform_revision: Arc<str>,
    output_resource: Arc<str>,
    input_resources: Box<[Arc<str>]>,
}

impl FirstPartyBundleKey {
    pub fn new(
        identity: &ExecutionBundleIdentity,
        platform_revision: impl Into<Arc<str>>,
        output_resource: impl Into<Arc<str>>,
        inputs: Vec<BundleCacheResource>,
    ) -> Result<Self, FirstPartyBundleKeyError> {
        Self::from_parts(
            Arc::from(identity.hash()),
            platform_revision.into(),
            output_resource.into(),
            inputs,
        )
    }

    fn from_parts(
        identity_hash: Arc<str>,
        platform_revision: Arc<str>,
        output_resource: Arc<str>,
        inputs: Vec<BundleCacheResource>,
    ) -> Result<Self, FirstPartyBundleKeyError> {
        if identity_hash.is_empty()
            || platform_revision.is_empty()
            || output_resource.is_empty()
            || inputs.is_empty()
        {
            return Err(FirstPartyBundleKeyError {
                kind: FirstPartyBundleKeyErrorKind::MissingBoundary,
            });
        }
        if inputs
            .iter()
            .any(|input| input.scope != BundleInputScope::FirstParty)
        {
            return Err(FirstPartyBundleKeyError {
                kind: FirstPartyBundleKeyErrorKind::NonFirstPartyInput,
            });
        }
        let mut input_resources = inputs
            .into_iter()
            .map(|input| input.identity)
            .collect::<Vec<_>>();
        if input_resources.iter().any(|identity| identity.is_empty()) {
            return Err(FirstPartyBundleKeyError {
                kind: FirstPartyBundleKeyErrorKind::MissingBoundary,
            });
        }
        input_resources.sort_unstable();
        input_resources.dedup();
        Ok(Self {
            identity_hash,
            platform_revision,
            output_resource,
            input_resources: input_resources.into_boxed_slice(),
        })
    }

    pub fn identity_hash(&self) -> &str {
        &self.identity_hash
    }

    pub fn platform_revision(&self) -> &str {
        &self.platform_revision
    }

    pub fn output_resource(&self) -> &str {
        &self.output_resource
    }

    pub fn input_resources(&self) -> &[Arc<str>] {
        &self.input_resources
    }

    fn delivery_request(&self, principal: Arc<str>) -> BundleDeliveryRequest {
        BundleDeliveryRequest {
            principal,
            output_resource: self.output_resource.clone(),
            input_resources: self.input_resources.clone(),
        }
    }
}

/// Why a bundle key was rejected before it entered the global cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirstPartyBundleKeyErrorKind {
    MissingBoundary,
    NonFirstPartyInput,
}

/// A bundle key is incomplete or is not globally reusable in PLAN-2A.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirstPartyBundleKeyError {
    kind: FirstPartyBundleKeyErrorKind,
}

impl FirstPartyBundleKeyError {
    pub const fn kind(&self) -> FirstPartyBundleKeyErrorKind {
        self.kind
    }
}

impl Display for FirstPartyBundleKeyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            FirstPartyBundleKeyErrorKind::MissingBoundary => {
                formatter.write_str("execution-bundle cache key has an empty policy boundary")
            }
            FirstPartyBundleKeyErrorKind::NonFirstPartyInput => formatter
                .write_str("entitlement-scoped input is ineligible for PLAN-2A global reuse"),
        }
    }
}

impl std::error::Error for FirstPartyBundleKeyError {}

/// Exact resources that an authorizer must approve for one delivery attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleDeliveryRequest {
    principal: Arc<str>,
    output_resource: Arc<str>,
    input_resources: Box<[Arc<str>]>,
}

impl BundleDeliveryRequest {
    pub fn principal(&self) -> &str {
        &self.principal
    }

    pub fn output_resource(&self) -> &str {
        &self.output_resource
    }

    pub fn input_resources(&self) -> &[Arc<str>] {
        &self.input_resources
    }
}

/// Hard bounds for cache metadata and active composition work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BundleCacheLimits {
    pub max_entries: usize,
    pub max_concurrent_compositions: usize,
}

impl BundleCacheLimits {
    pub fn validate(self) -> Result<Self, InvalidBundleCacheLimits> {
        if self.max_entries == 0 || self.max_concurrent_compositions == 0 {
            return Err(InvalidBundleCacheLimits);
        }
        Ok(self)
    }
}

/// One or more execution-bundle cache limits are zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidBundleCacheLimits;

impl Display for InvalidBundleCacheLimits {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("execution-bundle cache limits must be non-zero")
    }
}

impl std::error::Error for InvalidBundleCacheLimits {}

/// Result produced by the composition and provenance-verification layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposedBundle {
    Verified(Arc<[u8]>),
    Poisoned,
}

impl ComposedBundle {
    pub fn verified(bytes: impl Into<Arc<[u8]>>) -> Self {
        Self::Verified(bytes.into())
    }
}

/// Cumulative cache metrics with raw matches separate from delivered hits.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BundleCacheMetrics {
    pub requests: u64,
    pub authorization_checks: u64,
    pub raw_matches: u64,
    pub authorized_hits: u64,
    pub authorization_misses: u64,
    pub cache_misses: u64,
    pub single_flight_collapses: u64,
    pub compositions_started: u64,
    pub poisoned_refusals: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundleCacheErrorKind {
    Unauthorized,
    Capacity,
    Poisoned,
    Composition,
}

/// A cache request failed before an authorized bundle could be delivered.
#[derive(Debug)]
pub struct BundleCacheError<E> {
    kind: BundleCacheErrorKind,
    source: Option<E>,
}

impl<E> BundleCacheError<E> {
    pub const fn kind(&self) -> BundleCacheErrorKind {
        self.kind
    }
}

impl<E> Display for BundleCacheError<E>
where
    E: Display,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (&self.kind, &self.source) {
            (BundleCacheErrorKind::Unauthorized, _) => {
                formatter.write_str("execution-bundle delivery is unauthorized")
            }
            (BundleCacheErrorKind::Capacity, _) => {
                formatter.write_str("execution-bundle cache capacity is exhausted")
            }
            (BundleCacheErrorKind::Poisoned, _) => {
                formatter.write_str("execution-bundle cache entry is poisoned")
            }
            (BundleCacheErrorKind::Composition, Some(source)) => {
                write!(formatter, "execution-bundle composition failed: {source}")
            }
            (BundleCacheErrorKind::Composition, None) => {
                formatter.write_str("execution-bundle composition failed")
            }
        }
    }
}

impl<E> std::error::Error for BundleCacheError<E>
where
    E: std::error::Error + 'static,
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|source| source as &(dyn std::error::Error + 'static))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundleRetentionErrorKind {
    Missing,
    Poisoned,
}

/// Durable-reference accounting cannot be applied to this cache entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BundleRetentionError {
    kind: BundleRetentionErrorKind,
}

impl BundleRetentionError {
    pub const fn kind(&self) -> BundleRetentionErrorKind {
        self.kind
    }
}

impl Display for BundleRetentionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            BundleRetentionErrorKind::Missing => {
                formatter.write_str("execution-bundle cache entry is missing")
            }
            BundleRetentionErrorKind::Poisoned => {
                formatter.write_str("poisoned execution-bundle entry cannot retain references")
            }
        }
    }
}

impl std::error::Error for BundleRetentionError {}

/// Result of an attempt to retire cached bundle bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundleRetirement {
    Retired,
    Protected { durable_references: usize },
    Missing,
    InFlight,
    Poisoned,
}

#[derive(Debug)]
struct ReadyBundle {
    bytes: Arc<[u8]>,
    durable_references: BTreeSet<Arc<str>>,
}

#[derive(Debug)]
struct Flight {
    changes: watch::Sender<u64>,
}

#[derive(Debug)]
enum CacheEntry {
    Ready(ReadyBundle),
    InFlight(Arc<Flight>),
    Poisoned,
}

#[derive(Debug)]
struct CacheState {
    entries: BTreeMap<FirstPartyBundleKey, CacheEntry>,
    metrics: BundleCacheMetrics,
}

#[derive(Debug)]
struct SharedCache {
    limits: BundleCacheLimits,
    compositions: Arc<Semaphore>,
    state: Mutex<CacheState>,
}

/// Bounded, cancellation-safe cache for globally reusable first-party bundles.
#[derive(Debug, Clone)]
pub struct ExecutionBundleCache {
    shared: Arc<SharedCache>,
}

impl ExecutionBundleCache {
    pub fn new(limits: BundleCacheLimits) -> Result<Self, InvalidBundleCacheLimits> {
        let limits = limits.validate()?;
        Ok(Self {
            shared: Arc::new(SharedCache {
                limits,
                compositions: Arc::new(Semaphore::new(limits.max_concurrent_compositions)),
                state: Mutex::new(CacheState {
                    entries: BTreeMap::new(),
                    metrics: BundleCacheMetrics::default(),
                }),
            }),
        })
    }

    /// Resolve one bundle, authorizing again after every single-flight wait.
    pub async fn get_or_compose<Authorize, AuthorizeFuture, ComposeFuture, E>(
        &self,
        key: FirstPartyBundleKey,
        principal: impl Into<Arc<str>>,
        authorize: Authorize,
        composition: ComposeFuture,
    ) -> Result<Arc<[u8]>, BundleCacheError<E>>
    where
        Authorize: Fn(BundleDeliveryRequest) -> AuthorizeFuture,
        AuthorizeFuture: Future<Output = bool>,
        ComposeFuture: Future<Output = Result<ComposedBundle, E>>,
    {
        let request = key.delivery_request(principal.into());
        let mut composition = Some(composition);
        {
            let mut state = self.lock_state();
            state.metrics.requests = state.metrics.requests.saturating_add(1);
        }

        loop {
            {
                let mut state = self.lock_state();
                state.metrics.authorization_checks =
                    state.metrics.authorization_checks.saturating_add(1);
            }
            if !authorize(request.clone()).await {
                let mut state = self.lock_state();
                if matches!(
                    state.entries.get(&key),
                    Some(CacheEntry::Ready(_) | CacheEntry::Poisoned)
                ) {
                    state.metrics.raw_matches = state.metrics.raw_matches.saturating_add(1);
                }
                state.metrics.authorization_misses =
                    state.metrics.authorization_misses.saturating_add(1);
                return Err(BundleCacheError {
                    kind: BundleCacheErrorKind::Unauthorized,
                    source: None,
                });
            }

            enum Action {
                Deliver(Arc<[u8]>),
                Wait(std::pin::Pin<Box<dyn Future<Output = ()> + Send>>),
                Compose(Arc<Flight>),
                Poisoned,
                Capacity,
            }

            let action = {
                let mut state = self.lock_state();
                let observed = match state.entries.get(&key) {
                    Some(CacheEntry::Ready(bundle)) => Action::Deliver(bundle.bytes.clone()),
                    Some(CacheEntry::InFlight(flight)) => {
                        let mut changes = flight.changes.subscribe();
                        Action::Wait(Box::pin(async move {
                            changes
                                .changed()
                                .await
                                .expect("single-flight sender remains live through notification");
                        }))
                    }
                    Some(CacheEntry::Poisoned) => Action::Poisoned,
                    None if state.entries.len() >= self.shared.limits.max_entries => {
                        Action::Capacity
                    }
                    None => {
                        let (changes, _) = watch::channel(0);
                        let flight = Arc::new(Flight { changes });
                        state
                            .entries
                            .insert(key.clone(), CacheEntry::InFlight(flight.clone()));
                        Action::Compose(flight)
                    }
                };
                match &observed {
                    Action::Deliver(_) => {
                        state.metrics.raw_matches = state.metrics.raw_matches.saturating_add(1);
                        state.metrics.authorized_hits =
                            state.metrics.authorized_hits.saturating_add(1);
                    }
                    Action::Wait(_) => {
                        state.metrics.single_flight_collapses =
                            state.metrics.single_flight_collapses.saturating_add(1);
                    }
                    Action::Compose(_) => {
                        state.metrics.cache_misses = state.metrics.cache_misses.saturating_add(1);
                        state.metrics.compositions_started =
                            state.metrics.compositions_started.saturating_add(1);
                    }
                    Action::Poisoned => {
                        state.metrics.raw_matches = state.metrics.raw_matches.saturating_add(1);
                        state.metrics.poisoned_refusals =
                            state.metrics.poisoned_refusals.saturating_add(1);
                    }
                    Action::Capacity => {}
                }
                observed
            };

            match action {
                Action::Deliver(bytes) => return Ok(bytes),
                Action::Wait(notified) => notified.await,
                Action::Poisoned => {
                    return Err(BundleCacheError {
                        kind: BundleCacheErrorKind::Poisoned,
                        source: None,
                    });
                }
                Action::Capacity => {
                    return Err(BundleCacheError {
                        kind: BundleCacheErrorKind::Capacity,
                        source: None,
                    });
                }
                Action::Compose(flight) => {
                    let mut guard = FlightGuard::new(self.shared.clone(), key.clone(), flight);
                    let permit = self
                        .shared
                        .compositions
                        .clone()
                        .acquire_owned()
                        .await
                        .expect("execution-bundle composition semaphore cannot close");
                    let outcome = composition
                        .take()
                        .expect("only one leader composes a request")
                        .await;
                    drop(permit);
                    match outcome {
                        Ok(ComposedBundle::Verified(bytes)) => {
                            guard.publish(CacheEntry::Ready(ReadyBundle {
                                bytes: bytes.clone(),
                                durable_references: BTreeSet::new(),
                            }));
                            {
                                let mut state = self.lock_state();
                                state.metrics.authorization_checks =
                                    state.metrics.authorization_checks.saturating_add(1);
                            }
                            if !authorize(request.clone()).await {
                                let mut state = self.lock_state();
                                state.metrics.authorization_misses =
                                    state.metrics.authorization_misses.saturating_add(1);
                                return Err(BundleCacheError {
                                    kind: BundleCacheErrorKind::Unauthorized,
                                    source: None,
                                });
                            }
                            return Ok(bytes);
                        }
                        Ok(ComposedBundle::Poisoned) => {
                            guard.publish(CacheEntry::Poisoned);
                            return Err(BundleCacheError {
                                kind: BundleCacheErrorKind::Poisoned,
                                source: None,
                            });
                        }
                        Err(source) => {
                            guard.cancel();
                            return Err(BundleCacheError {
                                kind: BundleCacheErrorKind::Composition,
                                source: Some(source),
                            });
                        }
                    }
                }
            }
        }
    }

    /// Protect a ready bundle while durable work or retention names it.
    pub fn retain(
        &self,
        key: &FirstPartyBundleKey,
        reference: impl Into<Arc<str>>,
    ) -> Result<(), BundleRetentionError> {
        let reference = reference.into();
        let mut state = self.lock_state();
        match state.entries.get_mut(key) {
            Some(CacheEntry::Ready(bundle)) => {
                bundle.durable_references.insert(reference);
                Ok(())
            }
            Some(CacheEntry::Poisoned) => Err(BundleRetentionError {
                kind: BundleRetentionErrorKind::Poisoned,
            }),
            Some(CacheEntry::InFlight(_)) | None => Err(BundleRetentionError {
                kind: BundleRetentionErrorKind::Missing,
            }),
        }
    }

    /// Release one durable reference after its retention obligation ends.
    pub fn release(
        &self,
        key: &FirstPartyBundleKey,
        reference: &str,
    ) -> Result<(), BundleRetentionError> {
        let mut state = self.lock_state();
        match state.entries.get_mut(key) {
            Some(CacheEntry::Ready(bundle)) => {
                bundle.durable_references.remove(reference);
                Ok(())
            }
            Some(CacheEntry::Poisoned) => Err(BundleRetentionError {
                kind: BundleRetentionErrorKind::Poisoned,
            }),
            Some(CacheEntry::InFlight(_)) | None => Err(BundleRetentionError {
                kind: BundleRetentionErrorKind::Missing,
            }),
        }
    }

    /// Retire bytes only after every durable reference has been released.
    pub fn retire(&self, key: &FirstPartyBundleKey) -> BundleRetirement {
        let mut state = self.lock_state();
        match state.entries.get(key) {
            Some(CacheEntry::Ready(bundle)) if !bundle.durable_references.is_empty() => {
                return BundleRetirement::Protected {
                    durable_references: bundle.durable_references.len(),
                };
            }
            Some(CacheEntry::Ready(_)) => BundleRetirement::Retired,
            Some(CacheEntry::InFlight(_)) => return BundleRetirement::InFlight,
            Some(CacheEntry::Poisoned) => return BundleRetirement::Poisoned,
            None => return BundleRetirement::Missing,
        };
        state.entries.remove(key);
        BundleRetirement::Retired
    }

    /// Remove a poison tombstone only after its external cause is corrected.
    pub fn clear_poisoned(&self, key: &FirstPartyBundleKey) -> bool {
        let mut state = self.lock_state();
        if matches!(state.entries.get(key), Some(CacheEntry::Poisoned)) {
            state.entries.remove(key);
            true
        } else {
            false
        }
    }

    pub fn metrics(&self) -> BundleCacheMetrics {
        self.lock_state().metrics
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, CacheState> {
        self.shared
            .state
            .lock()
            .expect("execution-bundle cache lock poisoned")
    }
}

struct FlightGuard {
    shared: Arc<SharedCache>,
    key: FirstPartyBundleKey,
    flight: Arc<Flight>,
    armed: bool,
}

impl FlightGuard {
    fn new(shared: Arc<SharedCache>, key: FirstPartyBundleKey, flight: Arc<Flight>) -> Self {
        Self {
            shared,
            key,
            flight,
            armed: true,
        }
    }

    fn publish(&mut self, entry: CacheEntry) {
        let mut state = self
            .shared
            .state
            .lock()
            .expect("execution-bundle cache lock poisoned");
        if entry_is_flight(&state.entries, &self.key, &self.flight) {
            state.entries.insert(self.key.clone(), entry);
        }
        self.armed = false;
        drop(state);
        wake_flight(&self.flight);
    }

    fn cancel(&mut self) {
        self.remove_flight();
        self.armed = false;
    }

    fn remove_flight(&self) {
        let mut state = self
            .shared
            .state
            .lock()
            .expect("execution-bundle cache lock poisoned");
        if entry_is_flight(&state.entries, &self.key, &self.flight) {
            state.entries.remove(&self.key);
        }
        drop(state);
        wake_flight(&self.flight);
    }
}

impl Drop for FlightGuard {
    fn drop(&mut self) {
        if self.armed {
            self.remove_flight();
        }
    }
}

fn entry_is_flight(
    entries: &BTreeMap<FirstPartyBundleKey, CacheEntry>,
    key: &FirstPartyBundleKey,
    flight: &Arc<Flight>,
) -> bool {
    matches!(
        entries.get(key),
        Some(CacheEntry::InFlight(current)) if Arc::ptr_eq(current, flight)
    )
}

fn wake_flight(flight: &Flight) {
    flight.changes.send_modify(|generation| {
        *generation = generation.saturating_add(1);
    });
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use tokio::sync::{Notify, oneshot};

    use super::*;

    fn limits() -> BundleCacheLimits {
        BundleCacheLimits {
            max_entries: 8,
            max_concurrent_compositions: 2,
        }
    }

    fn key(identity: &str, revision: &str) -> FirstPartyBundleKey {
        FirstPartyBundleKey::from_parts(
            Arc::from(identity),
            Arc::from(revision),
            Arc::from(format!("bundle:{identity}")),
            vec![
                BundleCacheResource::new("plug:pure", BundleInputScope::FirstParty),
                BundleCacheResource::new("runner:r0", BundleInputScope::FirstParty),
            ],
        )
        .expect("fixture is globally reusable")
    }

    fn allow(_: BundleDeliveryRequest) -> std::future::Ready<bool> {
        std::future::ready(true)
    }

    #[test]
    fn global_cache_refuses_entitlement_scoped_inputs() {
        let error = FirstPartyBundleKey::from_parts(
            Arc::from("identity"),
            Arc::from("r0"),
            Arc::from("bundle:identity"),
            vec![BundleCacheResource::new(
                "private:plug",
                BundleInputScope::EntitlementScoped,
            )],
        )
        .expect_err("private inputs belong to PLAN-2D, not the global cache");
        assert_eq!(
            error.kind(),
            FirstPartyBundleKeyErrorKind::NonFirstPartyInput
        );
    }

    #[tokio::test]
    async fn concurrent_same_key_requests_compose_exactly_once() {
        let cache = ExecutionBundleCache::new(limits()).expect("valid limits");
        let key = key("fleet-a", "r0");
        let started = Arc::new(Notify::new());
        let (release, released) = oneshot::channel();
        let compositions = Arc::new(AtomicUsize::new(0));

        let leader = {
            let cache = cache.clone();
            let key = key.clone();
            let started = started.clone();
            let compositions = compositions.clone();
            tokio::spawn(async move {
                cache
                    .get_or_compose(key, "org-a", allow, async move {
                        compositions.fetch_add(1, Ordering::SeqCst);
                        started.notify_one();
                        released.await.expect("test releases composition");
                        Ok::<_, Infallible>(ComposedBundle::verified(&b"bundle-r0"[..]))
                    })
                    .await
            })
        };
        started.notified().await;

        let mut followers = Vec::new();
        for index in 0..5 {
            let cache = cache.clone();
            let key = key.clone();
            let compositions = compositions.clone();
            followers.push(tokio::spawn(async move {
                cache
                    .get_or_compose(key, format!("org-{index}"), allow, async move {
                        compositions.fetch_add(1, Ordering::SeqCst);
                        Ok::<_, Infallible>(ComposedBundle::verified(&b"wrong"[..]))
                    })
                    .await
            }));
        }
        wait_for_metric(&cache, |metrics| metrics.single_flight_collapses == 5).await;
        release.send(()).expect("leader still waits");

        assert_eq!(
            leader
                .await
                .expect("leader task succeeds")
                .unwrap()
                .as_ref(),
            b"bundle-r0"
        );
        for follower in followers {
            assert_eq!(
                follower
                    .await
                    .expect("follower task succeeds")
                    .unwrap()
                    .as_ref(),
                b"bundle-r0"
            );
        }
        assert_eq!(compositions.load(Ordering::SeqCst), 1);
        let metrics = cache.metrics();
        assert_eq!(metrics.requests, 6);
        assert_eq!(metrics.compositions_started, 1);
        assert_eq!(metrics.single_flight_collapses, 5);
        assert_eq!(metrics.authorized_hits, 5);
    }

    #[tokio::test]
    async fn revocation_rechecks_every_resource_and_never_counts_as_a_hit() {
        let cache = ExecutionBundleCache::new(limits()).expect("valid limits");
        let key = key("fleet-a", "r0");
        let allowed = Arc::new(AtomicBool::new(true));
        let observed = Arc::new(Mutex::new(Vec::new()));
        let authorize = |request: BundleDeliveryRequest| {
            observed
                .lock()
                .expect("authorization observations lock")
                .push(request);
            std::future::ready(allowed.load(Ordering::SeqCst))
        };

        cache
            .get_or_compose(key.clone(), "org-a", authorize, async {
                Ok::<_, Infallible>(ComposedBundle::verified(&b"bundle-r0"[..]))
            })
            .await
            .expect("initial authorized composition");
        allowed.store(false, Ordering::SeqCst);
        let refusal = cache
            .get_or_compose(key.clone(), "org-a", authorize, async {
                Ok::<_, Infallible>(ComposedBundle::verified(&b"must-not-run"[..]))
            })
            .await
            .expect_err("revocation blocks a new delivery");
        assert_eq!(refusal.kind(), BundleCacheErrorKind::Unauthorized);

        let metrics = cache.metrics();
        assert_eq!(metrics.raw_matches, 1);
        assert_eq!(metrics.authorized_hits, 0);
        assert_eq!(metrics.authorization_misses, 1);
        let observed = observed.lock().expect("authorization observations lock");
        assert_eq!(observed.len(), 3);
        for request in observed.iter() {
            assert_eq!(request.output_resource(), "bundle:fleet-a");
            assert_eq!(
                request
                    .input_resources()
                    .iter()
                    .map(AsRef::as_ref)
                    .collect::<Vec<_>>(),
                ["plug:pure", "runner:r0"]
            );
        }
    }

    #[tokio::test]
    async fn revocation_during_composition_blocks_leader_delivery() {
        let cache = ExecutionBundleCache::new(limits()).expect("valid limits");
        let key = key("fleet-a", "r0");
        let allowed = Arc::new(AtomicBool::new(true));
        let authorize =
            |_: BundleDeliveryRequest| std::future::ready(allowed.load(Ordering::SeqCst));
        let refusal = {
            let allowed = allowed.clone();
            cache
                .get_or_compose(key.clone(), "org-a", authorize, async move {
                    allowed.store(false, Ordering::SeqCst);
                    Ok::<_, Infallible>(ComposedBundle::verified(&b"bundle-r0"[..]))
                })
                .await
        }
        .expect_err("revocation before leader delivery is enforced");
        assert_eq!(refusal.kind(), BundleCacheErrorKind::Unauthorized);
        let metrics = cache.metrics();
        assert_eq!(metrics.authorization_checks, 2);
        assert_eq!(metrics.authorization_misses, 1);
        assert_eq!(metrics.authorized_hits, 0);

        allowed.store(true, Ordering::SeqCst);
        assert_eq!(
            cache
                .get_or_compose(key, "org-a", authorize, async {
                    Ok::<_, Infallible>(ComposedBundle::verified(&b"wrong"[..]))
                })
                .await
                .expect("authorized cached delivery")
                .as_ref(),
            b"bundle-r0"
        );
        assert_eq!(cache.metrics().authorized_hits, 1);
    }

    #[tokio::test]
    async fn poison_tombstone_refuses_delivery_until_explicitly_cleared() {
        let cache = ExecutionBundleCache::new(limits()).expect("valid limits");
        let key = key("fleet-a", "r0");
        let compositions = Arc::new(AtomicUsize::new(0));
        let first = {
            let compositions = compositions.clone();
            cache
                .get_or_compose(key.clone(), "org-a", allow, async move {
                    compositions.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, Infallible>(ComposedBundle::Poisoned)
                })
                .await
        };
        assert_eq!(
            first.expect_err("poison is refused").kind(),
            BundleCacheErrorKind::Poisoned
        );

        let second = {
            let compositions = compositions.clone();
            cache
                .get_or_compose(key.clone(), "org-a", allow, async move {
                    compositions.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, Infallible>(ComposedBundle::verified(&b"wrong"[..]))
                })
                .await
        };
        assert_eq!(
            second.expect_err("tombstone is refused").kind(),
            BundleCacheErrorKind::Poisoned
        );
        assert_eq!(compositions.load(Ordering::SeqCst), 1);
        assert!(cache.clear_poisoned(&key));
    }

    #[tokio::test]
    async fn r0_bytes_survive_retirement_while_durable_references_exist() {
        let cache = ExecutionBundleCache::new(limits()).expect("valid limits");
        let key = key("fleet-a", "r0");
        cache
            .get_or_compose(key.clone(), "org-a", allow, async {
                Ok::<_, Infallible>(ComposedBundle::verified(&b"bundle-r0"[..]))
            })
            .await
            .expect("compose R0");
        cache
            .retain(&key, "queued:run-1")
            .expect("retain queued run");
        cache
            .retain(&key, "audit:run-0")
            .expect("retain audit record");
        assert_eq!(
            cache.retire(&key),
            BundleRetirement::Protected {
                durable_references: 2
            }
        );
        assert_eq!(
            cache
                .get_or_compose(key.clone(), "org-b", allow, async {
                    Ok::<_, Infallible>(ComposedBundle::verified(&b"wrong"[..]))
                })
                .await
                .expect("referenced R0 remains fetchable")
                .as_ref(),
            b"bundle-r0"
        );
        cache.release(&key, "queued:run-1").expect("release queue");
        assert_eq!(
            cache.retire(&key),
            BundleRetirement::Protected {
                durable_references: 1
            }
        );
        cache.release(&key, "audit:run-0").expect("release audit");
        assert_eq!(cache.retire(&key), BundleRetirement::Retired);
        assert_eq!(cache.retire(&key), BundleRetirement::Missing);
    }

    #[tokio::test]
    async fn cancelled_leader_wakes_waiter_to_retry_composition() {
        let cache = ExecutionBundleCache::new(limits()).expect("valid limits");
        let key = key("fleet-a", "r0");
        let started = Arc::new(Notify::new());
        let leader = {
            let cache = cache.clone();
            let key = key.clone();
            let started = started.clone();
            tokio::spawn(async move {
                cache
                    .get_or_compose(key, "org-a", allow, async move {
                        started.notify_one();
                        std::future::pending::<Result<ComposedBundle, Infallible>>().await
                    })
                    .await
            })
        };
        started.notified().await;
        let follower = {
            let cache = cache.clone();
            let key = key.clone();
            tokio::spawn(async move {
                cache
                    .get_or_compose(key, "org-b", allow, async {
                        Ok::<_, Infallible>(ComposedBundle::verified(&b"retry"[..]))
                    })
                    .await
            })
        };
        wait_for_metric(&cache, |metrics| metrics.single_flight_collapses == 1).await;
        leader.abort();
        assert!(leader.await.expect_err("leader was aborted").is_cancelled());
        assert_eq!(
            follower
                .await
                .expect("follower task succeeds")
                .unwrap()
                .as_ref(),
            b"retry"
        );
        assert_eq!(cache.metrics().compositions_started, 2);
    }

    #[tokio::test]
    async fn composition_semaphore_backpressures_distinct_keys() {
        let cache = ExecutionBundleCache::new(BundleCacheLimits {
            max_entries: 4,
            max_concurrent_compositions: 1,
        })
        .expect("valid limits");
        let first_started = Arc::new(Notify::new());
        let second_started = Arc::new(AtomicBool::new(false));
        let (release, released) = oneshot::channel();
        let first = {
            let cache = cache.clone();
            let first_started = first_started.clone();
            tokio::spawn(async move {
                cache
                    .get_or_compose(key("fleet-a", "r0"), "org-a", allow, async move {
                        first_started.notify_one();
                        released.await.expect("release first composition");
                        Ok::<_, Infallible>(ComposedBundle::verified(&b"first"[..]))
                    })
                    .await
            })
        };
        first_started.notified().await;
        let second = {
            let cache = cache.clone();
            let second_started = second_started.clone();
            tokio::spawn(async move {
                cache
                    .get_or_compose(key("fleet-b", "r0"), "org-a", allow, async move {
                        second_started.store(true, Ordering::SeqCst);
                        Ok::<_, Infallible>(ComposedBundle::verified(&b"second"[..]))
                    })
                    .await
            })
        };
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert!(!second_started.load(Ordering::SeqCst));
        release.send(()).expect("first composition waits");
        first.await.expect("first task succeeds").unwrap();
        second.await.expect("second task succeeds").unwrap();
        assert!(second_started.load(Ordering::SeqCst));
    }

    async fn wait_for_metric(
        cache: &ExecutionBundleCache,
        predicate: impl Fn(BundleCacheMetrics) -> bool,
    ) {
        for _ in 0..100 {
            if predicate(cache.metrics()) {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!(
            "cache metric did not reach expected value: {:?}",
            cache.metrics()
        );
    }
}
