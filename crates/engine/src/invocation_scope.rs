//! The host-owned lifetime and context of each invocation.

use std::any::{Any, TypeId};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

use tokio::sync::watch;
use tokio::time::Instant;

use wash_runtime::plugin::HostPlugin;
use wash_runtime::wit::WitWorld;

use wamn_application::MAX_HOST_CALL_DURATION;
use wamn_application::ingress::RouteProvider;
use wamn_application::invocation_authority::InvocationAuthority;
use crate::invocation_trace::InvocationTrace;

pub const INVOCATION_SCOPES_ID: &str = "wamn:invocation-scopes";

#[derive(Default)]
struct RoutingScope(Option<Arc<dyn RouteProvider>>);
impl InvocationAuthority for RoutingScope {
    fn routes(&self) -> Option<Arc<dyn RouteProvider>> {
        self.0.as_ref().map(Arc::clone)
    }
}

struct Record {
    workload: Option<Box<str>>,
    trace: InvocationTrace,
    authorities: HashMap<TypeId, Box<dyn InvocationAuthority>>,
    execution: InvocationExecution,
}

impl Drop for Record {
    fn drop(&mut self) {
        self.execution.cancel();
    }
}

/// The absolute time limit and cancellation signal shared by a call's host work.
#[derive(Debug, Clone)]
pub struct InvocationExecution {
    deadline: Instant,
    cancelled: watch::Sender<bool>,
}

impl InvocationExecution {
    pub fn new(deadline: Instant) -> Self {
        Self {
            deadline,
            cancelled: watch::channel(false).0,
        }
    }

    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    pub fn is_active(&self) -> bool {
        !*self.cancelled.borrow() && Instant::now() < self.deadline
    }

    pub fn cancel(&self) {
        self.cancelled.send_replace(true);
    }

    /// Drop host work before polling it once the invocation ends.
    pub async fn run<F: Future>(&self, work: F) -> Option<F::Output> {
        let mut cancelled = self.cancelled.subscribe();
        tokio::select! {
            biased;
            _ = cancelled.wait_for(|cancelled| *cancelled) => None,
            () = tokio::time::sleep_until(self.deadline) => None,
            output = work => Some(output),
        }
    }
}

impl Default for InvocationExecution {
    fn default() -> Self {
        Self::new(Instant::now() + MAX_HOST_CALL_DURATION)
    }
}

/// One registry shared by the host service readers.
#[derive(Default)]
pub struct InvocationScopes {
    records: RwLock<HashMap<String, Record>>,
}

impl std::fmt::Debug for InvocationScopes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InvocationScopes").finish_non_exhaustive()
    }
}

impl InvocationScopes {
    /// Read authority without exposing its service-specific type to the engine.
    pub fn read<T: 'static, R>(&self, scope: &str, read: impl FnOnce(&T) -> R) -> Option<R> {
        let records = self
            .records
            .read()
            .expect("invocation scopes lock poisoned");
        Some(read(
            (records
                .get(scope)?
                .authorities
                .get(&TypeId::of::<T>())?
                .as_ref() as &dyn Any)
                .downcast_ref()?,
        ))
    }

    /// Observe host-owned authority without exposing registry records.
    pub fn any<T: 'static>(&self, predicate: impl Fn(&T) -> bool) -> bool {
        self.records
            .read()
            .expect("invocation scopes lock poisoned")
            .values()
            .any(|record| {
                record
                    .authorities
                    .get(&TypeId::of::<T>())
                    .and_then(|authority| (authority.as_ref() as &dyn Any).downcast_ref::<T>())
                    .is_some_and(&predicate)
            })
    }

    /// Populate host-owned workload facts before execution.
    pub fn update<T: Default + InvocationAuthority, R>(
        &self,
        scope: &str,
        update: impl FnOnce(&mut T) -> R,
    ) -> R {
        let mut records = self
            .records
            .write()
            .expect("invocation scopes lock poisoned");
        let record = records.entry(scope.to_owned()).or_insert_with(|| Record {
            workload: None,
            trace: InvocationTrace::capture(),
            authorities: HashMap::new(),
            execution: InvocationExecution::default(),
        });
        update(
            (record
                .authorities
                .entry(TypeId::of::<T>())
                .or_insert_with(|| Box::<T>::default())
                .as_mut() as &mut dyn Any)
                .downcast_mut()
                .expect("scope authority type differs"),
        )
    }

    pub fn update_existing<T: 'static, R>(
        &self,
        scope: &str,
        update: impl FnOnce(&mut T) -> R,
    ) -> Option<R> {
        let mut records = self
            .records
            .write()
            .expect("invocation scopes lock poisoned");
        Some(update(
            (records
                .get_mut(scope)?
                .authorities
                .get_mut(&TypeId::of::<T>())?
                .as_mut() as &mut dyn Any)
                .downcast_mut()?,
        ))
    }

    /// Install an admitted invocation atomically and return its lifetime owner.
    pub fn enter(
        self: &Arc<Self>,
        scope: String,
        authorities: impl IntoIterator<Item = Box<dyn InvocationAuthority>>,
        trace: InvocationTrace,
    ) -> anyhow::Result<ScopeLifetime> {
        let mut records = self
            .records
            .write()
            .expect("invocation scopes lock poisoned");
        anyhow::ensure!(
            !records.contains_key(&scope),
            "invocation scope is already bound"
        );
        records.insert(
            scope.clone(),
            Record {
                workload: None,
                trace,
                authorities: authorities
                    .into_iter()
                    .map(|authority| ((authority.as_ref() as &dyn Any).type_id(), authority))
                    .collect(),
                execution: InvocationExecution::default(),
            },
        );
        Ok(ScopeLifetime {
            registry: Arc::clone(self),
            id: scope,
        })
    }

    /// Retain a request's trace until its Store releases the returned owner.
    pub fn enter_trace(
        self: &Arc<Self>,
        scope: String,
        trace: InvocationTrace,
    ) -> anyhow::Result<ScopeLifetime> {
        self.enter(
            scope,
            [Box::new(RoutingScope::default()) as Box<dyn InvocationAuthority>],
            trace,
        )
    }

    /// Associate a bound component's authority with its runtime workload.
    pub fn bind_workload(&self, scope: &str, workload: &str) {
        if let Some(record) = self
            .records
            .write()
            .expect("invocation scopes lock poisoned")
            .get_mut(scope)
        {
            record.workload = Some(workload.into());
        }
    }

    /// Bind the host's route facts for a workload component.
    pub fn bind_routes(&self, scope: &str, routes: Arc<dyn RouteProvider>) {
        self.update::<RoutingScope, _>(scope, |record| record.0 = Some(routes));
    }

    /// Resolve the route facts selected by the host for this scope.
    pub fn routes(&self, scope: &str) -> Option<Arc<dyn RouteProvider>> {
        self.records
            .read()
            .expect("invocation scopes lock poisoned")
            .get(scope)?
            .authorities
            .values()
            .find_map(|authority| authority.routes())
    }

    /// Record the execution span once native initialization completes.
    pub fn set_trace(&self, scope: &str, trace: InvocationTrace) -> anyhow::Result<()> {
        let mut records = self
            .records
            .write()
            .expect("invocation scopes lock poisoned");
        let record = records
            .get_mut(scope)
            .ok_or_else(|| anyhow::anyhow!("invocation scope was revoked"))?;
        record.trace = trace;
        Ok(())
    }

    pub fn trace(&self, scope: &str) -> Option<InvocationTrace> {
        self.records
            .read()
            .expect("invocation scopes lock poisoned")
            .get(scope)
            .map(|record| record.trace.clone())
    }

    pub fn execution(&self, scope: &str) -> Option<InvocationExecution> {
        self.records
            .read()
            .expect("invocation scopes lock poisoned")
            .get(scope)
            .map(|record| record.execution.clone())
    }

    /// Bind native dispatch to the authority record that its host calls read.
    pub fn set_execution(
        &self,
        scope: &str,
        mut execution: InvocationExecution,
    ) -> anyhow::Result<()> {
        let mut records = self
            .records
            .write()
            .expect("invocation scopes lock poisoned");
        let record = records
            .get_mut(scope)
            .ok_or_else(|| anyhow::anyhow!("invocation scope was revoked"))?;
        execution.deadline = execution.deadline.min(record.execution.deadline());
        record.execution.cancel();
        record.execution = execution;
        Ok(())
    }

    pub fn revoke(&self, scope: &str) {
        self.records
            .write()
            .expect("invocation scopes lock poisoned")
            .remove(scope);
    }

    pub fn revoke_workload(&self, workload: &str) {
        self.records
            .write()
            .expect("invocation scopes lock poisoned")
            .retain(|scope, record| {
                record.workload.as_deref() != Some(workload) && !scope.starts_with(workload)
            });
    }

    pub fn is_empty(&self) -> bool {
        self.records
            .read()
            .expect("invocation scopes lock poisoned")
            .is_empty()
    }
}

/// Revokes the complete record on completion or cancellation.
#[derive(Debug)]
pub struct ScopeLifetime {
    registry: Arc<InvocationScopes>,
    id: String,
}

impl ScopeLifetime {
    pub fn id(&self) -> &str {
        &self.id
    }
}

impl Drop for ScopeLifetime {
    fn drop(&mut self) {
        self.registry.revoke(&self.id);
    }
}

#[async_trait::async_trait]
impl HostPlugin for InvocationScopes {
    fn id(&self) -> &'static str {
        INVOCATION_SCOPES_ID
    }
    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::new(),
            exports: HashSet::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authority_observation_ends_with_the_invocation_lifetime() {
        let scopes = Arc::new(InvocationScopes::default());
        let lifetime = scopes
            .enter(
                "request".to_owned(),
                [Box::new(RoutingScope::default()) as Box<dyn InvocationAuthority>],
                InvocationTrace::capture(),
            )
            .unwrap();
        assert!(scopes.any::<RoutingScope>(|_| true));
        drop(lifetime);
        assert!(!scopes.any::<RoutingScope>(|_| true));
    }
}
