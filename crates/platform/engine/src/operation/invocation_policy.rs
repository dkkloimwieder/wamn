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
    type Facts: Clone + Send + Sync + 'static;
    /// Revokes the authority of one call when it drops.
    type Authority: Send;
    /// Admit one invocation and retain its authority through result delivery.
    fn prepare(
        &self,
        _component_id: &str,
        _operation: &str,
        facts: &Self::Facts,
    ) -> impl Future<Output = anyhow::Result<Self::Facts>> + Send {
        async { Ok(facts.clone()) }
    }

    /// The prepared invocation's scope, when the policy owns its lifetime.
    fn scope_id<'a>(&self, _facts: &'a Self::Facts) -> Option<&'a str> {
        None
    }

    /// Grant authority after native initialization, with rollback on every partial failure.
    fn activate(
        &self,
        component_id: &str,
        invocation_scope: &InvocationScope<Self>,
        request: &NativeInvocation<Self>,
        failure: NativeCallFailure,
    ) -> impl Future<Output = anyhow::Result<Self::Authority>> + Send;

    /// Dispatch through the host's operation owner. HTTP lends an already claimed call.
    fn invoke(
        &self,
        _component_id: &str,
        target: &wash_runtime::engine::dispatch::DispatchTarget,
        request: super::native_call::NativeInvocation<Self>,
    ) -> impl Future<Output = anyhow::Result<super::native_call::NativeOutcome>> + Send {
        super::native_call::invoke_raw(target, request)
    }

    /// Replace host-owned result fields after guest execution. JSON results
    /// reach this hook after intent replay and commit, so URLs are never logged.
    fn finish_result(
        &self,
        _component_id: &str,
        _operation: &str,
        _facts: &Self::Facts,
        outcome: super::native_call::NativeOutcome,
    ) -> impl Future<Output = anyhow::Result<super::native_call::NativeOutcome>> + Send {
        async { Ok(outcome) }
    }

    /// Close component admission and revoke the authority of every active call.
    fn shutdown(&self);

    /// Revoke the authority of one call scope.
    fn revoke(&self, scope: &str);
}

/// The host that loads the application of the carried release.
pub trait ApplicationHost {
    /// The policy of the applications this host loads.
    type Policy: InvocationPolicy;

    /// The loaded application of the carried release, over its complete component list.
    fn released_application(
        &self,
        components: &Arc<[AdmittedComponent]>,
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

    pub fn with_id(policy: Arc<P>, id: Box<str>) -> Self {
        Self {
            id,
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
