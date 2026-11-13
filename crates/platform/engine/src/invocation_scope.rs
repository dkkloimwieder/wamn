//! The host-owned lifetime and context of each invocation.

use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

use wash_runtime::plugin::HostPlugin;
use wash_runtime::wit::WitWorld;

use crate::invocation_trace::InvocationTrace;

pub const INVOCATION_SCOPES_ID: &str = "wamn:invocation-scopes";

struct Record {
    trace: InvocationTrace,
    authority: Box<dyn Any + Send + Sync>,
}

/// One registry shared by the host capability readers.
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
    /// Read authority without exposing its capability-specific type to the engine.
    pub fn read<T: 'static, R>(&self, scope: &str, read: impl FnOnce(&T) -> R) -> Option<R> {
        let records = self
            .records
            .read()
            .expect("invocation scopes lock poisoned");
        Some(read(records.get(scope)?.authority.downcast_ref()?))
    }

    /// Populate host-owned workload facts before execution.
    pub fn update<T: Default + Send + Sync + 'static, R>(
        &self,
        scope: &str,
        update: impl FnOnce(&mut T) -> R,
    ) -> R {
        let mut records = self
            .records
            .write()
            .expect("invocation scopes lock poisoned");
        let record = records.entry(scope.to_owned()).or_insert_with(|| Record {
            trace: InvocationTrace::capture(),
            authority: Box::<T>::default(),
        });
        update(
            record
                .authority
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
        Some(update(records.get_mut(scope)?.authority.downcast_mut()?))
    }

    /// Install an admitted invocation atomically and return its lifetime owner.
    pub fn enter<T: Send + Sync + 'static>(
        self: &Arc<Self>,
        scope: String,
        authority: T,
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
                trace,
                authority: Box::new(authority),
            },
        );
        Ok(ScopeLifetime {
            registry: Arc::clone(self),
            id: scope,
        })
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
            .retain(|scope, _| !scope.starts_with(workload));
    }

    pub fn is_empty(&self) -> bool {
        self.records
            .read()
            .expect("invocation scopes lock poisoned")
            .is_empty()
    }

    #[cfg(feature = "test-util")]
    pub fn scopes(&self) -> Vec<String> {
        self.records
            .read()
            .expect("invocation scopes lock poisoned")
            .keys()
            .cloned()
            .collect()
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
