//! Bounded lifecycle pool for independently stored execution instances.

use std::collections::{BTreeMap, VecDeque};
use std::fmt::{Debug, Display};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Immutable reuse boundary for one execution bundle.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExecutionPoolKey {
    bundle: Arc<str>,
    revision: Arc<str>,
    entitlement: Arc<str>,
}

impl ExecutionPoolKey {
    /// Create the exact bundle, platform revision, and entitlement boundary.
    pub fn new(
        bundle: impl Into<Arc<str>>,
        revision: impl Into<Arc<str>>,
        entitlement: impl Into<Arc<str>>,
    ) -> Self {
        Self {
            bundle: bundle.into(),
            revision: revision.into(),
            entitlement: entitlement.into(),
        }
    }

    pub fn bundle(&self) -> &str {
        &self.bundle
    }

    pub fn revision(&self) -> &str {
        &self.revision
    }

    pub fn entitlement(&self) -> &str {
        &self.entitlement
    }
}

/// Limits applied before an instance can become live or idle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionPoolLimits {
    pub max_instances: usize,
    pub max_reserved_bytes: usize,
    pub max_idle_per_bundle: usize,
    pub max_invocations_per_instance: u64,
    pub max_idle_age: Duration,
}

impl ExecutionPoolLimits {
    pub fn validate(self) -> Result<Self, InvalidExecutionPoolLimits> {
        if self.max_instances == 0
            || self.max_reserved_bytes == 0
            || self.max_idle_per_bundle == 0
            || self.max_invocations_per_instance == 0
        {
            return Err(InvalidExecutionPoolLimits);
        }
        Ok(self)
    }
}

/// One or more execution-pool bounds are zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidExecutionPoolLimits;

impl Display for InvalidExecutionPoolLimits {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("execution-pool count and byte limits must be non-zero")
    }
}

impl std::error::Error for InvalidExecutionPoolLimits {}

/// A reusable instance must reserve bounded memory and prove complete reset.
pub trait ReusableExecutionInstance: Debug + Send + 'static {
    type ResetError: std::error::Error + Send + Sync + 'static;

    /// Maximum bytes this live instance may consume, not its current RSS.
    fn reserved_bytes(&self) -> usize;

    /// Clear every invocation-scoped field and guest-memory sentinel.
    fn reset_invocation_state(&mut self) -> Result<(), Self::ResetError>;
}

/// Why an instance was destroyed instead of returned to idle reuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RetirementReason {
    Trap,
    Cancelled,
    Deadline,
    CleanupFailed,
    RevisionInvalidated,
    EntitlementInvalidated,
    MaxInvocations,
    IdleLimit,
    IdleAge,
}

/// Result of one guest invocation before cleanup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvocationDisposition {
    Reusable,
    Trap,
    Cancelled,
    Deadline,
    RevisionInvalidated,
    EntitlementInvalidated,
}

impl InvocationDisposition {
    fn retirement(self) -> Option<RetirementReason> {
        match self {
            Self::Reusable => None,
            Self::Trap => Some(RetirementReason::Trap),
            Self::Cancelled => Some(RetirementReason::Cancelled),
            Self::Deadline => Some(RetirementReason::Deadline),
            Self::RevisionInvalidated => Some(RetirementReason::RevisionInvalidated),
            Self::EntitlementInvalidated => Some(RetirementReason::EntitlementInvalidated),
        }
    }
}

/// Current density and cumulative lifecycle counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionPoolSnapshot {
    pub live_instances: usize,
    pub idle_instances: usize,
    pub checked_out_instances: usize,
    pub reserved_bytes: usize,
    pub peak_checked_out_instances: usize,
    pub inserted_instances: u64,
    pub checkouts: u64,
    pub reused_checkouts: u64,
    pub retirements: BTreeMap<RetirementReason, u64>,
}

impl ExecutionPoolSnapshot {
    fn empty() -> Self {
        Self {
            live_instances: 0,
            idle_instances: 0,
            checked_out_instances: 0,
            reserved_bytes: 0,
            peak_checked_out_instances: 0,
            inserted_instances: 0,
            checkouts: 0,
            reused_checkouts: 0,
            retirements: BTreeMap::new(),
        }
    }
}

struct IdleInstance<T> {
    instance: T,
    reserved_bytes: usize,
    invocations: u64,
    idle_since: Instant,
}

struct PoolState<T> {
    idle: BTreeMap<ExecutionPoolKey, VecDeque<IdleInstance<T>>>,
    revision_generations: BTreeMap<Arc<str>, u64>,
    entitlement_generations: BTreeMap<Arc<str>, u64>,
    snapshot: ExecutionPoolSnapshot,
}

struct SharedPool<T> {
    limits: ExecutionPoolLimits,
    state: Mutex<PoolState<T>>,
}

/// A bounded collection of exclusive, independently stored execution instances.
pub struct ExecutionInstancePool<T> {
    shared: Arc<SharedPool<T>>,
}

impl<T> Clone for ExecutionInstancePool<T> {
    fn clone(&self) -> Self {
        Self {
            shared: self.shared.clone(),
        }
    }
}

impl<T> Debug for ExecutionInstancePool<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExecutionInstancePool")
            .field("limits", &self.shared.limits)
            .finish_non_exhaustive()
    }
}

impl<T> ExecutionInstancePool<T>
where
    T: ReusableExecutionInstance,
{
    pub fn new(limits: ExecutionPoolLimits) -> Result<Self, InvalidExecutionPoolLimits> {
        Ok(Self {
            shared: Arc::new(SharedPool {
                limits: limits.validate()?,
                state: Mutex::new(PoolState {
                    idle: BTreeMap::new(),
                    revision_generations: BTreeMap::new(),
                    entitlement_generations: BTreeMap::new(),
                    snapshot: ExecutionPoolSnapshot::empty(),
                }),
            }),
        })
    }

    /// Insert one clean, invocation-ready instance during prewarming.
    pub fn insert(&self, key: ExecutionPoolKey, instance: T) -> Result<(), PoolCapacityError> {
        let reserved_bytes = instance.reserved_bytes();
        let mut state = self
            .shared
            .state
            .lock()
            .expect("execution pool lock poisoned");
        let snapshot = &state.snapshot;
        if snapshot.live_instances >= self.shared.limits.max_instances
            || reserved_bytes
                > self
                    .shared
                    .limits
                    .max_reserved_bytes
                    .saturating_sub(snapshot.reserved_bytes)
            || state.idle.get(&key).map_or(0, VecDeque::len)
                >= self.shared.limits.max_idle_per_bundle
        {
            return Err(PoolCapacityError);
        }
        state.idle.entry(key).or_default().push_back(IdleInstance {
            instance,
            reserved_bytes,
            invocations: 0,
            idle_since: Instant::now(),
        });
        state.snapshot.live_instances += 1;
        state.snapshot.idle_instances += 1;
        state.snapshot.reserved_bytes += reserved_bytes;
        state.snapshot.inserted_instances += 1;
        Ok(())
    }

    /// Exclusively remove one matching instance from the idle set.
    pub fn checkout(&self, key: &ExecutionPoolKey) -> Option<ExecutionLease<T>> {
        self.prune_idle();
        let mut state = self
            .shared
            .state
            .lock()
            .expect("execution pool lock poisoned");
        let idle = state.idle.get_mut(key)?.pop_front()?;
        if state.idle.get(key).is_some_and(VecDeque::is_empty) {
            state.idle.remove(key);
        }
        state.snapshot.idle_instances -= 1;
        state.snapshot.checked_out_instances += 1;
        state.snapshot.checkouts += 1;
        if idle.invocations > 0 {
            state.snapshot.reused_checkouts += 1;
        }
        state.snapshot.peak_checked_out_instances = state
            .snapshot
            .peak_checked_out_instances
            .max(state.snapshot.checked_out_instances);
        let revision_generation = generation(&state.revision_generations, key.revision());
        let entitlement_generation = generation(&state.entitlement_generations, key.entitlement());
        Some(ExecutionLease {
            shared: self.shared.clone(),
            key: key.clone(),
            instance: Some(idle.instance),
            reserved_bytes: idle.reserved_bytes,
            invocations: idle.invocations,
            revision_generation,
            entitlement_generation,
        })
    }

    /// Destroy every idle instance past the configured age.
    pub fn prune_idle(&self) {
        let now = Instant::now();
        let mut state = self
            .shared
            .state
            .lock()
            .expect("execution pool lock poisoned");
        let mut retired = Vec::new();
        state.idle.retain(|_, instances| {
            let mut retained = VecDeque::with_capacity(instances.len());
            while let Some(idle) = instances.pop_front() {
                if now.saturating_duration_since(idle.idle_since) >= self.shared.limits.max_idle_age
                {
                    retired.push(idle.reserved_bytes);
                } else {
                    retained.push_back(idle);
                }
            }
            *instances = retained;
            !instances.is_empty()
        });
        for reserved_bytes in retired {
            retire_locked(&mut state, reserved_bytes, RetirementReason::IdleAge, false);
        }
    }

    pub fn snapshot(&self) -> ExecutionPoolSnapshot {
        self.shared
            .state
            .lock()
            .expect("execution pool lock poisoned")
            .snapshot
            .clone()
    }

    /// Destroy idle instances whose bundle revision is no longer current.
    pub fn invalidate_revision(&self, revision: &str) {
        self.invalidate_matching(
            RetirementReason::RevisionInvalidated,
            |state| advance_generation(&mut state.revision_generations, revision),
            |key| key.revision() == revision,
        );
    }

    /// Destroy idle instances whose entitlement may no longer receive the bundle.
    pub fn invalidate_entitlement(&self, entitlement: &str) {
        self.invalidate_matching(
            RetirementReason::EntitlementInvalidated,
            |state| advance_generation(&mut state.entitlement_generations, entitlement),
            |key| key.entitlement() == entitlement,
        );
    }

    fn invalidate_matching(
        &self,
        reason: RetirementReason,
        advance: impl FnOnce(&mut PoolState<T>),
        mut matches: impl FnMut(&ExecutionPoolKey) -> bool,
    ) {
        let mut state = self
            .shared
            .state
            .lock()
            .expect("execution pool lock poisoned");
        advance(&mut state);
        let keys = state
            .idle
            .keys()
            .filter(|key| matches(key))
            .cloned()
            .collect::<Vec<_>>();
        for key in keys {
            let instances = state.idle.remove(&key).expect("collected idle key exists");
            for idle in instances {
                retire_locked(&mut state, idle.reserved_bytes, reason, false);
            }
        }
    }
}

/// The pool cannot admit another live instance within its count or byte bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolCapacityError;

impl Display for PoolCapacityError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("execution pool capacity exhausted")
    }
}

impl std::error::Error for PoolCapacityError {}

/// Cleanup failed, so the checked-out instance was destroyed.
#[derive(Debug)]
pub struct PoolCleanupError<E> {
    source: E,
}

impl<E> Display for PoolCleanupError<E>
where
    E: Display,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "execution instance cleanup failed: {}",
            self.source
        )
    }
}

impl<E> std::error::Error for PoolCleanupError<E>
where
    E: std::error::Error + 'static,
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Exclusive ownership of one instance and its store for one invocation.
pub struct ExecutionLease<T>
where
    T: ReusableExecutionInstance,
{
    shared: Arc<SharedPool<T>>,
    key: ExecutionPoolKey,
    instance: Option<T>,
    reserved_bytes: usize,
    invocations: u64,
    revision_generation: u64,
    entitlement_generation: u64,
}

impl<T> Debug for ExecutionLease<T>
where
    T: ReusableExecutionInstance,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExecutionLease")
            .field("key", &self.key)
            .field("invocations", &self.invocations)
            .finish_non_exhaustive()
    }
}

impl<T> ExecutionLease<T>
where
    T: ReusableExecutionInstance,
{
    pub fn instance(&self) -> &T {
        self.instance
            .as_ref()
            .expect("live execution lease owns its instance")
    }

    pub fn instance_mut(&mut self) -> &mut T {
        self.instance
            .as_mut()
            .expect("live execution lease owns its instance")
    }

    /// Complete the invocation, resetting before reuse or destroying on policy.
    pub fn finish(
        mut self,
        disposition: InvocationDisposition,
    ) -> Result<(), PoolCleanupError<T::ResetError>> {
        let mut instance = self
            .instance
            .take()
            .expect("live execution lease owns its instance");
        let reserved_bytes = self.reserved_bytes;
        if let Some(reason) = disposition.retirement() {
            let mut state = self
                .shared
                .state
                .lock()
                .expect("execution pool lock poisoned");
            retire_locked(&mut state, reserved_bytes, reason, true);
            return Ok(());
        }

        let invocations = self.invocations.saturating_add(1);
        if invocations >= self.shared.limits.max_invocations_per_instance {
            let mut state = self
                .shared
                .state
                .lock()
                .expect("execution pool lock poisoned");
            retire_locked(
                &mut state,
                reserved_bytes,
                RetirementReason::MaxInvocations,
                true,
            );
            return Ok(());
        }
        if let Err(source) = instance.reset_invocation_state() {
            let mut state = self
                .shared
                .state
                .lock()
                .expect("execution pool lock poisoned");
            retire_locked(
                &mut state,
                reserved_bytes,
                RetirementReason::CleanupFailed,
                true,
            );
            return Err(PoolCleanupError { source });
        }

        let mut state = self
            .shared
            .state
            .lock()
            .expect("execution pool lock poisoned");
        let invalidation = if generation(&state.revision_generations, self.key.revision())
            != self.revision_generation
        {
            Some(RetirementReason::RevisionInvalidated)
        } else if generation(&state.entitlement_generations, self.key.entitlement())
            != self.entitlement_generation
        {
            Some(RetirementReason::EntitlementInvalidated)
        } else {
            None
        };
        if let Some(reason) = invalidation {
            retire_locked(&mut state, reserved_bytes, reason, true);
            return Ok(());
        }
        let idle_count = state.idle.get(&self.key).map_or(0, VecDeque::len);
        if idle_count >= self.shared.limits.max_idle_per_bundle {
            retire_locked(
                &mut state,
                reserved_bytes,
                RetirementReason::IdleLimit,
                true,
            );
            return Ok(());
        }
        state
            .idle
            .entry(self.key.clone())
            .or_default()
            .push_back(IdleInstance {
                instance,
                reserved_bytes,
                invocations,
                idle_since: Instant::now(),
            });
        state.snapshot.checked_out_instances -= 1;
        state.snapshot.idle_instances += 1;
        Ok(())
    }
}

impl<T> Drop for ExecutionLease<T>
where
    T: ReusableExecutionInstance,
{
    fn drop(&mut self) {
        let Some(_instance) = self.instance.take() else {
            return;
        };
        let mut state = self
            .shared
            .state
            .lock()
            .expect("execution pool lock poisoned");
        retire_locked(
            &mut state,
            self.reserved_bytes,
            RetirementReason::Cancelled,
            true,
        );
    }
}

fn generation(generations: &BTreeMap<Arc<str>, u64>, boundary: &str) -> u64 {
    generations.get(boundary).copied().unwrap_or_default()
}

fn advance_generation(generations: &mut BTreeMap<Arc<str>, u64>, boundary: &str) {
    let generation = generations.entry(Arc::from(boundary)).or_default();
    *generation = generation.saturating_add(1);
}

fn retire_locked<T>(
    state: &mut PoolState<T>,
    reserved_bytes: usize,
    reason: RetirementReason,
    checked_out: bool,
) {
    state.snapshot.live_instances -= 1;
    state.snapshot.reserved_bytes -= reserved_bytes;
    if checked_out {
        state.snapshot.checked_out_instances -= 1;
    } else {
        state.snapshot.idle_instances -= 1;
    }
    *state.snapshot.retirements.entry(reason).or_default() += 1;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Default, PartialEq, Eq)]
    struct InvocationSentinels {
        tenant: Option<String>,
        run: Option<String>,
        credential: Option<String>,
        egress: Option<String>,
        trace: Option<String>,
        config: Option<String>,
        generation: Option<u64>,
        memory: Vec<u8>,
    }

    #[derive(Debug)]
    struct FixtureInstance {
        id: usize,
        reservation: usize,
        reset_fails: bool,
        sentinels: InvocationSentinels,
    }

    #[derive(Debug)]
    struct FixtureResetError;

    impl Display for FixtureResetError {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("fixture reset failed")
        }
    }

    impl std::error::Error for FixtureResetError {}

    impl ReusableExecutionInstance for FixtureInstance {
        type ResetError = FixtureResetError;

        fn reserved_bytes(&self) -> usize {
            self.reservation
        }

        fn reset_invocation_state(&mut self) -> Result<(), Self::ResetError> {
            if self.reset_fails {
                return Err(FixtureResetError);
            }
            self.sentinels = InvocationSentinels::default();
            Ok(())
        }
    }

    fn limits() -> ExecutionPoolLimits {
        ExecutionPoolLimits {
            max_instances: 4,
            max_reserved_bytes: 4_096,
            max_idle_per_bundle: 4,
            max_invocations_per_instance: 8,
            max_idle_age: Duration::from_secs(60),
        }
    }

    fn key(bundle: &str) -> ExecutionPoolKey {
        ExecutionPoolKey::new(bundle, "r0", "org-a")
    }

    fn fixture(id: usize) -> FixtureInstance {
        FixtureInstance {
            id,
            reservation: 1_024,
            reset_fails: false,
            sentinels: InvocationSentinels::default(),
        }
    }

    #[test]
    fn alternating_invocations_reset_every_state_and_memory_sentinel() {
        let pool = ExecutionInstancePool::new(limits()).expect("valid pool limits");
        let key = key("bundle-a");
        pool.insert(key.clone(), fixture(7))
            .expect("prewarm instance");

        for index in 0..6 {
            let mut lease = pool.checkout(&key).expect("warm instance available");
            assert_eq!(
                lease.instance().id,
                7,
                "the same store is deliberately reused"
            );
            assert_eq!(lease.instance().sentinels, InvocationSentinels::default());
            lease.instance_mut().sentinels = InvocationSentinels {
                tenant: Some(format!("tenant-{index}")),
                run: Some(format!("run-{index}")),
                credential: Some(format!("credential-{index}")),
                egress: Some(format!("egress-{index}")),
                trace: Some(format!("trace-{index}")),
                config: Some(format!("config-{index}")),
                generation: Some(index),
                memory: vec![index as u8; 64],
            };
            lease
                .finish(InvocationDisposition::Reusable)
                .expect("fixture resets cleanly");
        }

        let snapshot = pool.snapshot();
        assert_eq!(snapshot.live_instances, 1);
        assert_eq!(snapshot.idle_instances, 1);
        assert_eq!(snapshot.reused_checkouts, 5);
    }

    #[test]
    fn one_store_is_checked_out_once_and_density_is_measurable() {
        let pool = ExecutionInstancePool::new(limits()).expect("valid pool limits");
        let key = key("bundle-a");
        for id in 0..3 {
            pool.insert(key.clone(), fixture(id))
                .expect("prewarm instance");
        }

        let first = pool.checkout(&key).expect("first store");
        let second = pool.checkout(&key).expect("second store");
        let third = pool.checkout(&key).expect("third store");
        assert!(
            pool.checkout(&key).is_none(),
            "no store can be checked out twice"
        );
        let ids = BTreeMap::from([
            (first.instance().id, ()),
            (second.instance().id, ()),
            (third.instance().id, ()),
        ]);
        assert_eq!(ids.len(), 3);
        assert_eq!(pool.snapshot().peak_checked_out_instances, 3);

        first
            .finish(InvocationDisposition::Reusable)
            .expect("clean first");
        second
            .finish(InvocationDisposition::Reusable)
            .expect("clean second");
        third
            .finish(InvocationDisposition::Reusable)
            .expect("clean third");
    }

    #[test]
    fn count_and_reserved_memory_bounds_reject_excess_instances() {
        let mut bounded = limits();
        bounded.max_instances = 2;
        bounded.max_reserved_bytes = 2_048;
        let pool = ExecutionInstancePool::new(bounded).expect("valid pool limits");
        let key = key("bundle-a");
        pool.insert(key.clone(), fixture(0))
            .expect("first instance");
        pool.insert(key.clone(), fixture(1))
            .expect("second instance");
        assert_eq!(pool.insert(key, fixture(2)), Err(PoolCapacityError));
        assert_eq!(pool.snapshot().reserved_bytes, 2_048);
    }

    #[test]
    fn every_unsafe_outcome_destroys_instead_of_repooling() {
        let dispositions = [
            (InvocationDisposition::Trap, RetirementReason::Trap),
            (
                InvocationDisposition::Cancelled,
                RetirementReason::Cancelled,
            ),
            (InvocationDisposition::Deadline, RetirementReason::Deadline),
            (
                InvocationDisposition::RevisionInvalidated,
                RetirementReason::RevisionInvalidated,
            ),
            (
                InvocationDisposition::EntitlementInvalidated,
                RetirementReason::EntitlementInvalidated,
            ),
        ];
        for (disposition, reason) in dispositions {
            let pool = ExecutionInstancePool::new(limits()).expect("valid pool limits");
            let key = key("bundle-a");
            pool.insert(key.clone(), fixture(0))
                .expect("prewarm instance");
            pool.checkout(&key)
                .expect("instance")
                .finish(disposition)
                .expect("retirement does not clean");
            assert!(pool.checkout(&key).is_none());
            assert_eq!(pool.snapshot().retirements.get(&reason), Some(&1));
        }
    }

    #[test]
    fn failed_cleanup_and_dropped_lease_destroy_instances() {
        let pool = ExecutionInstancePool::new(limits()).expect("valid pool limits");
        let key = key("bundle-a");
        let mut dirty = fixture(0);
        dirty.reset_fails = true;
        pool.insert(key.clone(), dirty)
            .expect("prewarm dirty fixture");
        let failure = pool
            .checkout(&key)
            .expect("instance")
            .finish(InvocationDisposition::Reusable)
            .expect_err("failed cleanup is reported");
        assert_eq!(
            failure.to_string(),
            "execution instance cleanup failed: fixture reset failed"
        );
        assert_eq!(
            pool.snapshot()
                .retirements
                .get(&RetirementReason::CleanupFailed),
            Some(&1)
        );

        pool.insert(key.clone(), fixture(1))
            .expect("replacement instance");
        drop(pool.checkout(&key).expect("dropped invocation lease"));
        assert_eq!(
            pool.snapshot()
                .retirements
                .get(&RetirementReason::Cancelled),
            Some(&1)
        );
    }

    #[test]
    fn max_invocations_idle_limit_age_and_invalidation_retire_instances() {
        let mut bounded = limits();
        bounded.max_invocations_per_instance = 2;
        bounded.max_idle_per_bundle = 1;
        let pool = ExecutionInstancePool::new(bounded).expect("valid pool limits");
        let pool_key = key("bundle-a");
        pool.insert(pool_key.clone(), fixture(0))
            .expect("first instance");
        pool.checkout(&pool_key)
            .expect("first invocation")
            .finish(InvocationDisposition::Reusable)
            .expect("first reset");
        pool.checkout(&pool_key)
            .expect("second invocation")
            .finish(InvocationDisposition::Reusable)
            .expect("max count retirement");
        assert_eq!(
            pool.snapshot()
                .retirements
                .get(&RetirementReason::MaxInvocations),
            Some(&1)
        );

        pool.insert(pool_key.clone(), fixture(1))
            .expect("first live instance");
        let lease = pool.checkout(&pool_key).expect("checked-out instance");
        pool.insert(pool_key.clone(), fixture(2))
            .expect("second live instance");
        lease
            .finish(InvocationDisposition::Reusable)
            .expect("idle cap retirement");
        assert_eq!(
            pool.snapshot()
                .retirements
                .get(&RetirementReason::IdleLimit),
            Some(&1)
        );

        let revision_lease = pool.checkout(&pool_key).expect("old revision checkout");
        pool.invalidate_revision("r0");
        revision_lease
            .finish(InvocationDisposition::Reusable)
            .expect("checked-out old revision is retired");
        assert_eq!(
            pool.snapshot()
                .retirements
                .get(&RetirementReason::RevisionInvalidated),
            Some(&1)
        );

        let entitlement_key = ExecutionPoolKey::new("bundle-a", "r1", "org-b");
        pool.insert(entitlement_key.clone(), fixture(3))
            .expect("new revision");
        let entitlement_lease = pool
            .checkout(&entitlement_key)
            .expect("old entitlement checkout");
        pool.invalidate_entitlement("org-b");
        entitlement_lease
            .finish(InvocationDisposition::Reusable)
            .expect("checked-out old entitlement is retired");
        assert_eq!(
            pool.snapshot()
                .retirements
                .get(&RetirementReason::EntitlementInvalidated),
            Some(&1)
        );

        let mut expiring = limits();
        expiring.max_idle_age = Duration::ZERO;
        let expiring_pool = ExecutionInstancePool::new(expiring).expect("valid pool limits");
        let expiring_key = key("expiring");
        expiring_pool
            .insert(expiring_key.clone(), fixture(4))
            .expect("expiring instance");
        assert!(expiring_pool.checkout(&expiring_key).is_none());
        assert_eq!(
            expiring_pool
                .snapshot()
                .retirements
                .get(&RetirementReason::IdleAge),
            Some(&1)
        );
    }
}
