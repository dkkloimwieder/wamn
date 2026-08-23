//! Bounded lifecycle pool for independently stored execution instances.
//!
//! # Pool key, and what tenant isolation rests on (wamn-0h0g.17.1, .17.7)
//!
//! A pool is keyed by the **component digest alone** ([`ExecutionPoolKey`]), so
//! every wiring that resolves to the same component bytes shares one warm pool.
//! The key deliberately carries no entitlement: warm instances are no longer
//! partitioned by tenant, which is the whole of the cross-wiring amortization.
//!
//! Two separate arguments carry that. The first covers state this pool stores:
//! under [`INVOCATIONS_PER_INSTANCE`] an instance is destroyed at the end of the
//! checkout it was handed to, so no instance ever serves two invocations and the
//! idle set holds only never-invoked instances. No guest-visible state written
//! during one checkout can be observed by another.
//!
//! The second covers the identity an instance carries, and it is a different
//! argument from a different mechanism. **Reuse being off is not a mitigation
//! for misattributed identity**: `max_invocations_per_instance = 1` protects
//! residual GUEST state, and says nothing about whose tenant claim a warm store
//! resolves to. `wamn-0h0g.17.1` shipped on the premise that identity was
//! injected per checkout; it was not — it was bound once at construction and
//! held in the store's `Ctx` plus process-resident registries keyed by component
//! id. One digest's pool filled from two identities was therefore a cross-tenant
//! leak that nothing here could detect.
//!
//! # The checkout-time identity seam (wamn-0h0g.17.7)
//!
//! **Instances are fungible compute; identity is per-acquisition state.**
//! [`checkout`] takes the identity of the run it is serving and rebinds the
//! instance to it through [`ReusableExecutionInstance::bind_identity`] before
//! any lease is handed out, and every path that ends a checkout — repooled,
//! retired, or dropped — clears it again through
//! [`ReusableExecutionInstance::revoke_identity`]. An idle instance resolves to
//! no identity at all, so a store that skipped a rebind fails closed instead of
//! serving someone else's rows.
//!
//! A rebind that fails leaves an instance that may be half-bound, so it is
//! destroyed rather than returned or repooled — [`RetirementReason::IdentityBindFailed`].
//!
//! What "the identity" comprises, and where each element is rebound, is
//! enumerated by the implementor: see `ExecutionAcquisition` in this crate's
//! root module for the production tuple. A partial rebind is the same leak
//! wearing a fix, so an implementor that adds identity-derived state without
//! adding it to both halves has re-opened this defect.
//!
//! [`checkout`]: ExecutionInstancePool::checkout

use std::collections::{BTreeMap, VecDeque};
use std::fmt::{Debug, Display};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Invocations one instance may serve before it is destroyed.
///
/// One, deliberately, not as a placeholder: reuse stays off until windowed
/// state ships with explicit affinity. Single use is what lets a pool be keyed
/// by digest alone — see the module docs. Raising it re-opens every question
/// dropping the entitlement key closed.
pub const INVOCATIONS_PER_INSTANCE: u64 = 1;

/// Immutable reuse boundary: one pool per component digest.
///
/// The digest is content-addressed, so a new component revision is a new key
/// and cannot be served from the old pool. The production driver derives it
/// from the release-attested component fact; this type carries it, it does not
/// validate it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExecutionPoolKey {
    digest: Arc<str>,
}

impl ExecutionPoolKey {
    /// Key one pool by the digest of the exact component bytes.
    pub fn new(digest: impl Into<Arc<str>>) -> Self {
        Self {
            digest: digest.into(),
        }
    }

    pub fn digest(&self) -> &str {
        &self.digest
    }
}

/// Limits applied before an instance can become live or idle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionPoolLimits {
    pub max_instances: usize,
    pub max_reserved_bytes: usize,
    pub max_idle_per_digest: usize,
    pub max_invocations_per_instance: u64,
    pub max_idle_age: Duration,
}

impl ExecutionPoolLimits {
    pub fn validate(self) -> Result<Self, InvalidExecutionPoolLimits> {
        if self.max_instances == 0
            || self.max_reserved_bytes == 0
            || self.max_idle_per_digest == 0
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

/// A reusable instance reserves bounded memory, takes its identity at checkout,
/// and proves complete reset.
pub trait ReusableExecutionInstance: Debug + Send + 'static {
    /// Everything about this instance that is derived from WHO it runs for.
    ///
    /// A prewarmed instance carries none of it. The pool never inspects this
    /// type — it is opaque here precisely because the pool must not grow an
    /// opinion about identity beyond "rebind it at every acquisition".
    type Identity;

    /// Why one identity could not be bound to this instance.
    type BindError: std::error::Error + Send + Sync + 'static;

    type ResetError: std::error::Error + Send + Sync + 'static;

    /// Maximum bytes this live instance may consume, not its current RSS.
    fn reserved_bytes(&self) -> usize;

    /// Rebind EVERY identity-derived element of this instance to `identity`.
    ///
    /// Called by [`ExecutionInstancePool::checkout`] before the lease exists, so
    /// no caller can observe the instance under the identity it last served. An
    /// element that is identity-derived and not rebound here is a cross-tenant
    /// leak channel; enumerate them in the implementation rather than covering
    /// the obvious ones.
    fn bind_identity(&mut self, identity: &Self::Identity) -> Result<(), Self::BindError>;

    /// Clear every element [`bind_identity`](Self::bind_identity) installed.
    ///
    /// Called on every path that ends a checkout, including a failed bind and a
    /// dropped lease, so an idle or destroyed instance resolves to no identity.
    fn revoke_identity(&mut self);

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
    /// A checkout could not rebind the instance to the acquiring identity, so
    /// the possibly half-bound instance was destroyed instead of handed out.
    IdentityBindFailed,
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
    digest_generations: BTreeMap<Arc<str>, u64>,
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
                    digest_generations: BTreeMap::new(),
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
                >= self.shared.limits.max_idle_per_digest
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

    /// Exclusively remove one matching instance and rebind it to `identity`.
    ///
    /// `Ok(None)` is "no warm instance under this key"; `Err` is "one was
    /// available and could not be made to serve you", which destroys it rather
    /// than leaving a half-bound instance in the idle set.
    pub fn checkout(
        &self,
        key: &ExecutionPoolKey,
        identity: &T::Identity,
    ) -> Result<Option<ExecutionLease<T>>, IdentityBindFailed<T::BindError>> {
        let Some(mut lease) = self.take_idle(key) else {
            return Ok(None);
        };
        if let Err(source) = lease.instance_mut().bind_identity(identity) {
            lease.retire(RetirementReason::IdentityBindFailed);
            return Err(IdentityBindFailed {
                digest: key.digest.clone(),
                source,
            });
        }
        Ok(Some(lease))
    }

    /// Admit one freshly instantiated component directly as a checked-out
    /// lease. This is the on-demand production path: the new store is never
    /// briefly visible as an unbound idle instance another tenant could steal
    /// between `insert` and `checkout`.
    ///
    /// `Ok(None)` is bounded capacity exhaustion. A bind refusal returns the
    /// same typed failure as a warm checkout and the never-admitted instance is
    /// dropped after its partial identity is revoked.
    pub fn checkout_new(
        &self,
        key: ExecutionPoolKey,
        mut instance: T,
        identity: &T::Identity,
    ) -> Result<Option<ExecutionLease<T>>, IdentityBindFailed<T::BindError>> {
        let reserved_bytes = instance.reserved_bytes();
        // Binding may acquire plugin-owned locks. Keep user-supplied trait code
        // outside the pool's global state lock so unrelated digests are not
        // serialized behind it.
        if let Err(source) = instance.bind_identity(identity) {
            instance.revoke_identity();
            return Err(IdentityBindFailed {
                digest: key.digest.clone(),
                source,
            });
        }
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
        {
            drop(state);
            instance.revoke_identity();
            return Ok(None);
        }
        let digest_generation = generation(&state.digest_generations, key.digest());
        state.snapshot.live_instances += 1;
        state.snapshot.checked_out_instances += 1;
        state.snapshot.reserved_bytes += reserved_bytes;
        state.snapshot.peak_checked_out_instances = state
            .snapshot
            .peak_checked_out_instances
            .max(state.snapshot.checked_out_instances);
        state.snapshot.inserted_instances += 1;
        state.snapshot.checkouts += 1;
        Ok(Some(ExecutionLease {
            shared: self.shared.clone(),
            key,
            instance: Some(instance),
            reserved_bytes,
            invocations: 0,
            digest_generation,
        }))
    }

    /// Exclusively remove one matching instance from the idle set.
    fn take_idle(&self, key: &ExecutionPoolKey) -> Option<ExecutionLease<T>> {
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
        let digest_generation = generation(&state.digest_generations, key.digest());
        Some(ExecutionLease {
            shared: self.shared.clone(),
            key: key.clone(),
            instance: Some(idle.instance),
            reserved_bytes: idle.reserved_bytes,
            invocations: idle.invocations,
            digest_generation,
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

    /// Destroy idle instances built from a component digest that is no longer
    /// servable, and fence the ones already checked out under it.
    ///
    /// The digest-keyed pool has no entitlement dimension left to invalidate.
    /// A revoked entitlement is now a property of one run, not of a warm
    /// instance, so it is carried by
    /// [`InvocationDisposition::EntitlementInvalidated`] at the end of the
    /// checkout it applies to.
    pub fn invalidate_digest(&self, digest: &str) {
        let mut state = self
            .shared
            .state
            .lock()
            .expect("execution pool lock poisoned");
        advance_generation(&mut state.digest_generations, digest);
        let Some(instances) = state.idle.remove(&ExecutionPoolKey::new(digest)) else {
            return;
        };
        for idle in instances {
            retire_locked(
                &mut state,
                idle.reserved_bytes,
                RetirementReason::RevisionInvalidated,
                false,
            );
        }
    }
}

/// A checkout could not bind its identity, so the warm instance was destroyed.
#[derive(Debug)]
pub struct IdentityBindFailed<E> {
    /// The component digest whose pool the instance came from.
    digest: Arc<str>,
    source: E,
}

impl<E> IdentityBindFailed<E> {
    pub fn digest(&self) -> &str {
        &self.digest
    }
}

impl<E> Display for IdentityBindFailed<E>
where
    E: Display,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "binding a checkout identity to a warm {} instance failed: {}",
            self.digest, self.source
        )
    }
}

impl<E> std::error::Error for IdentityBindFailed<E>
where
    E: std::error::Error + 'static,
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
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
    digest_generation: u64,
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

    /// Destroy this instance now, clearing its identity first.
    ///
    /// Every end-of-checkout path routes through here or through the revoke at
    /// the head of [`finish`](Self::finish), so no instance is ever destroyed or
    /// repooled while it still resolves to the identity it served.
    fn retire(&mut self, reason: RetirementReason) {
        let Some(mut instance) = self.instance.take() else {
            return;
        };
        instance.revoke_identity();
        let mut state = self
            .shared
            .state
            .lock()
            .expect("execution pool lock poisoned");
        retire_locked(&mut state, self.reserved_bytes, reason, true);
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
        // The checkout is over the moment its outcome is known, so the identity
        // it was bound to goes first — before any branch decides whether the
        // instance is destroyed or returned to the idle set.
        instance.revoke_identity();
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
        if generation(&state.digest_generations, self.key.digest()) != self.digest_generation {
            retire_locked(
                &mut state,
                reserved_bytes,
                RetirementReason::RevisionInvalidated,
                true,
            );
            return Ok(());
        }
        let idle_count = state.idle.get(&self.key).map_or(0, VecDeque::len);
        if idle_count >= self.shared.limits.max_idle_per_digest {
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
        self.retire(RetirementReason::Cancelled);
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
        /// The identity this instance currently resolves to, if any. A warm,
        /// never-checked-out instance carries `None` and must go back to `None`
        /// on every path that ends a checkout.
        bound: Option<String>,
    }

    /// One acquisition's identity. `refuse` is how a bind that cannot be
    /// completed is exercised without a live plugin.
    #[derive(Debug)]
    struct FixtureIdentity {
        tenant: String,
        refuse: bool,
    }

    fn tenant(tenant: &str) -> FixtureIdentity {
        FixtureIdentity {
            tenant: tenant.to_string(),
            refuse: false,
        }
    }

    #[derive(Debug)]
    struct FixtureResetError;

    impl Display for FixtureResetError {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("fixture reset failed")
        }
    }

    impl std::error::Error for FixtureResetError {}

    #[derive(Debug)]
    struct FixtureBindError;

    impl Display for FixtureBindError {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("fixture identity refused")
        }
    }

    impl std::error::Error for FixtureBindError {}

    impl ReusableExecutionInstance for FixtureInstance {
        type Identity = FixtureIdentity;
        type BindError = FixtureBindError;
        type ResetError = FixtureResetError;

        fn reserved_bytes(&self) -> usize {
            self.reservation
        }

        fn bind_identity(&mut self, identity: &FixtureIdentity) -> Result<(), FixtureBindError> {
            // Deliberately writes BEFORE refusing, so a caller that kept a
            // refused instance would be keeping a half-bound one.
            self.bound = Some(identity.tenant.clone());
            if identity.refuse {
                return Err(FixtureBindError);
            }
            Ok(())
        }

        fn revoke_identity(&mut self) {
            self.bound = None;
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
            max_idle_per_digest: 4,
            max_invocations_per_instance: 8,
            max_idle_age: Duration::from_secs(60),
        }
    }

    fn key(digest: &str) -> ExecutionPoolKey {
        ExecutionPoolKey::new(digest)
    }

    fn fixture(id: usize) -> FixtureInstance {
        FixtureInstance {
            id,
            reservation: 1_024,
            reset_fails: false,
            sentinels: InvocationSentinels::default(),
            bound: None,
        }
    }

    /// Check out under a throwaway identity, for tests about lifecycle rather
    /// than about identity.
    fn checkout(
        pool: &ExecutionInstancePool<FixtureInstance>,
        key: &ExecutionPoolKey,
    ) -> Option<ExecutionLease<FixtureInstance>> {
        pool.checkout(key, &tenant("fixture"))
            .expect("the fixture identity binds")
    }

    #[test]
    fn alternating_invocations_reset_every_state_and_memory_sentinel() {
        let pool = ExecutionInstancePool::new(limits()).expect("valid pool limits");
        let key = key("bundle-a");
        pool.insert(key.clone(), fixture(7))
            .expect("prewarm instance");

        for index in 0..6 {
            let mut lease = checkout(&pool, &key).expect("warm instance available");
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

        let first = checkout(&pool, &key).expect("first store");
        let second = checkout(&pool, &key).expect("second store");
        let third = checkout(&pool, &key).expect("third store");
        assert!(
            checkout(&pool, &key).is_none(),
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
            checkout(&pool, &key)
                .expect("instance")
                .finish(disposition)
                .expect("retirement does not clean");
            assert!(checkout(&pool, &key).is_none());
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
        let failure = checkout(&pool, &key)
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
        drop(checkout(&pool, &key).expect("dropped invocation lease"));
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
        bounded.max_idle_per_digest = 1;
        let pool = ExecutionInstancePool::new(bounded).expect("valid pool limits");
        let pool_key = key("bundle-a");
        pool.insert(pool_key.clone(), fixture(0))
            .expect("first instance");
        checkout(&pool, &pool_key)
            .expect("first invocation")
            .finish(InvocationDisposition::Reusable)
            .expect("first reset");
        checkout(&pool, &pool_key)
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
        let lease = checkout(&pool, &pool_key).expect("checked-out instance");
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

        let revision_lease = checkout(&pool, &pool_key).expect("old digest checkout");
        pool.invalidate_digest("bundle-a");
        revision_lease
            .finish(InvocationDisposition::Reusable)
            .expect("checked-out stale digest is retired");
        assert_eq!(
            pool.snapshot()
                .retirements
                .get(&RetirementReason::RevisionInvalidated),
            Some(&1)
        );

        pool.insert(pool_key.clone(), fixture(3))
            .expect("idle instance under the stale digest");
        pool.invalidate_digest("bundle-a");
        assert!(checkout(&pool, &pool_key).is_none());
        assert_eq!(
            pool.snapshot()
                .retirements
                .get(&RetirementReason::RevisionInvalidated),
            Some(&2)
        );

        let mut expiring = limits();
        expiring.max_idle_age = Duration::ZERO;
        let expiring_pool = ExecutionInstancePool::new(expiring).expect("valid pool limits");
        let expiring_key = key("expiring");
        expiring_pool
            .insert(expiring_key.clone(), fixture(4))
            .expect("expiring instance");
        assert!(checkout(&expiring_pool, &expiring_key).is_none());
        assert_eq!(
            expiring_pool
                .snapshot()
                .retirements
                .get(&RetirementReason::IdleAge),
            Some(&1)
        );
    }

    /// Two identities, ONE digest pool, INTERLEAVED checkouts.
    ///
    /// A checks out, B checks out, both act, both end — so the assertion is made
    /// while BOTH leases are live. A sequential pair would pass against an
    /// implementation that bound identity once and never rebound it; this one
    /// cannot, because two live instances under one key must resolve to two
    /// different identities at the same instant.
    #[test]
    fn interleaved_checkouts_under_one_key_never_share_an_identity() {
        let pool = ExecutionInstancePool::new(limits()).expect("valid pool limits");
        let key = key("bundle-a");
        pool.insert(key.clone(), fixture(0)).expect("warm A");
        pool.insert(key.clone(), fixture(1)).expect("warm B");
        assert!(
            pool.snapshot().idle_instances == 2,
            "both prewarmed instances are idle"
        );

        let mut first = pool
            .checkout(&key, &tenant("tenant-a"))
            .expect("tenant A binds")
            .expect("a warm instance is available");
        let mut second = pool
            .checkout(&key, &tenant("tenant-b"))
            .expect("tenant B binds")
            .expect("the same key still has a warm instance");

        assert_eq!(first.instance().bound.as_deref(), Some("tenant-a"));
        assert_eq!(
            second.instance().bound.as_deref(),
            Some("tenant-b"),
            "the second acquisition on one digest pool must not inherit the first's identity"
        );

        first.instance_mut().sentinels.tenant = Some("a-wrote".to_string());
        second.instance_mut().sentinels.tenant = Some("b-wrote".to_string());
        assert_ne!(first.instance().id, second.instance().id);

        first
            .finish(InvocationDisposition::Reusable)
            .expect("A repools");
        second
            .finish(InvocationDisposition::Reusable)
            .expect("B repools");

        // Both went back to the idle set, and neither carries the identity it
        // served: a warm instance an acquisition forgot to rebind fails closed.
        let idle = checkout(&pool, &key).expect("a repooled instance");
        assert_eq!(idle.instance().bound.as_deref(), Some("fixture"));
        drop(idle);
        let untouched = pool
            .checkout(&key, &tenant("tenant-c"))
            .expect("tenant C binds")
            .expect("the other repooled instance");
        assert_eq!(untouched.instance().bound.as_deref(), Some("tenant-c"));
    }

    /// Every end-of-checkout path clears the identity before the instance is
    /// repooled or destroyed — including a dropped lease and a failed reset.
    #[test]
    fn every_ending_revokes_the_identity_it_served() {
        for disposition in [
            InvocationDisposition::Reusable,
            InvocationDisposition::Trap,
            InvocationDisposition::Cancelled,
            InvocationDisposition::Deadline,
            InvocationDisposition::RevisionInvalidated,
            InvocationDisposition::EntitlementInvalidated,
        ] {
            let pool = ExecutionInstancePool::new(limits()).expect("valid pool limits");
            let key = key("bundle-a");
            pool.insert(key.clone(), fixture(0)).expect("warm instance");
            pool.checkout(&key, &tenant("tenant-a"))
                .expect("tenant A binds")
                .expect("a warm instance is available")
                .finish(disposition)
                .expect("the fixture resets cleanly");
            if disposition == InvocationDisposition::Reusable {
                let repooled = checkout(&pool, &key).expect("the repooled instance");
                assert_eq!(
                    repooled.instance().bound.as_deref(),
                    Some("fixture"),
                    "a repooled instance must not still resolve to tenant-a"
                );
            }
        }

        // A dropped lease is the cancellation path: the instance is destroyed,
        // and its identity goes with it rather than after it.
        let pool = ExecutionInstancePool::new(limits()).expect("valid pool limits");
        let key = key("bundle-a");
        pool.insert(key.clone(), fixture(0)).expect("warm instance");
        drop(
            pool.checkout(&key, &tenant("tenant-a"))
                .expect("tenant A binds")
                .expect("a warm instance is available"),
        );
        assert_eq!(
            pool.snapshot()
                .retirements
                .get(&RetirementReason::Cancelled),
            Some(&1)
        );
    }

    /// A refused identity destroys the instance instead of handing it out.
    ///
    /// The fixture writes its identity before refusing, so an implementation
    /// that returned the instance to the idle set would be returning a
    /// half-bound one — and the next checkout would inherit it.
    #[test]
    fn a_refused_identity_destroys_the_instance() {
        let pool = ExecutionInstancePool::new(limits()).expect("valid pool limits");
        let key = key("bundle-a");
        pool.insert(key.clone(), fixture(0)).expect("warm instance");

        let refused = pool
            .checkout(
                &key,
                &FixtureIdentity {
                    tenant: "tenant-a".to_string(),
                    refuse: true,
                },
            )
            .expect_err("a refused identity is an error, not an empty pool");
        assert_eq!(
            refused.to_string(),
            "binding a checkout identity to a warm bundle-a instance failed: \
             fixture identity refused"
        );
        assert_eq!(refused.digest(), "bundle-a");
        assert_eq!(
            pool.snapshot()
                .retirements
                .get(&RetirementReason::IdentityBindFailed),
            Some(&1)
        );
        assert!(
            checkout(&pool, &key).is_none(),
            "the half-bound instance was destroyed, not repooled"
        );
        assert_eq!(pool.snapshot().live_instances, 0);
    }

    /// An empty pool is `Ok(None)`, not an error: "nothing warm here" and "one
    /// was here and could not serve you" are different answers to a caller that
    /// has to decide whether to build a fresh instance.
    #[test]
    fn an_empty_pool_is_not_an_identity_failure() {
        let pool: ExecutionInstancePool<FixtureInstance> =
            ExecutionInstancePool::new(limits()).expect("valid pool limits");
        assert!(
            pool.checkout(&key("bundle-a"), &tenant("tenant-a"))
                .expect("an empty pool is not a bind failure")
                .is_none()
        );
    }
}
