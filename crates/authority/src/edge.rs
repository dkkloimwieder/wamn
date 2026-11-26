//! Per-call authorization for admitted edge components.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use anyhow::Context as _;
use wamn_app::ingress::AuthenticatedCaller;
use wamn_app::native_routes::authorize_released_operation;
use wamn_component_policy::AdmittedComponent;

/// Admitted components indexed by their current runtime identifier.
#[derive(Debug, Default)]
pub struct EdgeAuthority {
    bindings: RwLock<BTreeMap<String, Arc<AdmittedComponent>>>,
}

impl EdgeAuthority {
    /// Associate a runtime identifier with an admitted component.
    pub fn bind(&self, component_id: String, component: Arc<AdmittedComponent>) {
        self.bindings
            .write()
            .expect("edge component bindings lock poisoned")
            .insert(component_id, component);
    }

    /// Remove every runtime binding.
    pub fn clear(&self) {
        self.bindings
            .write()
            .expect("edge component bindings lock poisoned")
            .clear();
    }

    /// Require a live deadline and a released operation the caller may invoke.
    ///
    /// # Errors
    /// Returns a refusal when the deadline, binding, export, or grant check fails.
    pub fn authorize(
        &self,
        component_id: &str,
        operation: &str,
        caller: Option<&AuthenticatedCaller>,
        now: Instant,
        deadline: Instant,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(now < deadline, "native-node-deadline-exceeded");
        let bindings = self
            .bindings
            .read()
            .expect("edge component bindings lock poisoned");
        let component = bindings
            .get(component_id)
            .context("native invocation component is not admitted")?;
        let released = component
            .operations
            .get(operation)
            .context("native invocation operation is not admitted")?;
        authorize_released_operation(caller, released)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
