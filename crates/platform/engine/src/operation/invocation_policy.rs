//! The seams that one operation call reaches its host through.
//!
//! [`InvocationPolicy`] grants and revokes the authority of one call on plugins
//! that the call path cannot name. [`ApplicationHost`] gives the loaded
//! application of the carried release.

use std::sync::{Arc, Mutex};

use wamn_catalog::AdmittedComponent;
use wash_runtime::plugin::HostPlugin;

use super::native_call::{NativeCallFailure, NativeInvocation};
use super::native_workload::NativeApplication;

/// The per-call authority of one native application.
///
/// The call path dispatches through this plugin by its [`HostPlugin::id`].
pub trait InvocationPolicy: HostPlugin + Sized {
    /// The facts of one call that only the policy reads.
    /// A logged call clones them for each item it runs.
    type Facts: Clone + Send + Sync + 'static;
    /// Revokes the authority of one call when it drops.
    type Authority: Send;
    /// The host transaction of one item of a logged call.
    type ItemTransaction: ItemTransaction;

    /// Begin the host transaction of one item of a logged call, before its
    /// claim and its guest call (`docs/plan/host-transaction.md` 4.1). It
    /// admits the call as [`activate`](Self::activate) does. An operation that
    /// declares no SQL gets `None`.
    fn begin_item(
        &self,
        component_id: &str,
        operation: &str,
        facts: &Self::Facts,
    ) -> impl Future<Output = anyhow::Result<Option<Self::ItemTransaction>>> + Send;

    /// The facts of the item's guest call, which carry its transaction to
    /// [`activate`](Self::activate).
    fn item_facts(&self, facts: &Self::Facts, transaction: &Self::ItemTransaction) -> Self::Facts;

    /// Grant authority after native initialization, with rollback on every partial failure.
    fn activate(
        &self,
        component_id: &str,
        invocation_scope: &InvocationScope<Self>,
        request: &NativeInvocation<Self>,
        failure: NativeCallFailure,
    ) -> impl Future<Output = anyhow::Result<Self::Authority>> + Send;

    /// Close component admission and revoke the authority of every active call.
    fn shutdown(&self);

    /// Revoke the authority of one call scope.
    fn revoke(&self, scope: &str);
}

/// The host transaction of one item of a logged call. The engine commits it
/// when the item succeeds and rolls it back otherwise.
pub trait ItemTransaction: Send + Sync {
    /// Commit the transaction.
    fn commit(&self) -> impl Future<Output = Result<(), ItemCommitFailure>> + Send;
    /// Roll the transaction back.
    fn rollback(&self) -> impl Future<Output = anyhow::Result<()>> + Send;
}

/// Why the commit of an item transaction did not succeed.
#[derive(Debug)]
pub enum ItemCommitFailure {
    /// The transaction rolled back. `code` is the generated error literal that
    /// the item answers: `retry`, `timeout` or `internal_error`.
    RolledBack { code: &'static str, message: String },
    /// `COMMIT` was sent and its answer was not read, so the item may have
    /// committed. The item answers `intent-uncertain`.
    Uncertain { message: String },
}

/// The item transaction of a host that has no SQL.
#[derive(Debug)]
pub enum NoItemTransaction {}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "the type has no value, so neither method runs"
)]
impl ItemTransaction for NoItemTransaction {
    async fn commit(&self) -> Result<(), ItemCommitFailure> {
        match *self {}
    }

    async fn rollback(&self) -> anyhow::Result<()> {
        match *self {}
    }
}

/// The host that loads the application of the carried release.
pub trait ApplicationHost {
    /// The policy of the applications this host loads.
    type Policy: InvocationPolicy;

    /// The loaded application of the carried release, over its complete component list.
    fn released_application(
        &self,
        components: &[AdmittedComponent],
    ) -> impl Future<Output = anyhow::Result<Arc<NativeApplication<Self::Policy>>>> + Send;
}

/// Caller-owned cancellation boundary, separate from the native store lifetime.
#[derive(Debug)]
pub struct InvocationScope<P: InvocationPolicy> {
    pub id: Box<str>,
    pub closed: Mutex<bool>,
    cancelled: tokio::sync::Notify,
    policy: Arc<P>,
}

impl<P: InvocationPolicy> InvocationScope<P> {
    pub fn new(policy: Arc<P>) -> Self {
        Self {
            id: super::next_scope("native-invocation"),
            closed: Mutex::new(false),
            cancelled: tokio::sync::Notify::new(),
            policy,
        }
    }

    pub async fn cancelled(&self) {
        loop {
            let notified = self.cancelled.notified();
            if *self.closed.lock().expect("invocation scope lock poisoned") {
                return;
            }
            notified.await;
        }
    }

    pub fn close(&self) {
        let mut closed = self.closed.lock().expect("invocation scope lock poisoned");
        *closed = true;
        self.policy.revoke(&self.id);
        self.cancelled.notify_waiters();
    }
}
