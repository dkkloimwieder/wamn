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
pub(crate) trait InvocationPolicy: HostPlugin + Sized {
    /// The facts of one call that only the policy reads.
    type Facts: Send + Sync + 'static;
    /// Revokes the authority of one call when it drops.
    type Authority: Send;

    /// Retain the application of this policy, once.
    fn bind_application(&self, application: &Arc<NativeApplication<Self>>) -> anyhow::Result<()>;

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

/// The host that loads the application of the carried release.
pub(crate) trait ApplicationHost {
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
pub(crate) struct InvocationScope<P: InvocationPolicy> {
    pub(super) id: Box<str>,
    pub(super) closed: Mutex<bool>,
    cancelled: tokio::sync::Notify,
    policy: Arc<P>,
}

impl<P: InvocationPolicy> InvocationScope<P> {
    pub(super) fn new(policy: Arc<P>) -> Self {
        Self {
            id: super::next_scope("native-invocation"),
            closed: Mutex::new(false),
            cancelled: tokio::sync::Notify::new(),
            policy,
        }
    }

    pub(super) async fn cancelled(&self) {
        loop {
            let notified = self.cancelled.notified();
            if *self.closed.lock().expect("invocation scope lock poisoned") {
                return;
            }
            notified.await;
        }
    }

    pub(super) fn close(&self) {
        let mut closed = self.closed.lock().expect("invocation scope lock poisoned");
        *closed = true;
        self.policy.revoke(&self.id);
        self.cancelled.notify_waiters();
    }
}
