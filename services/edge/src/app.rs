//! The loaded application of the edge release.
//!
//! The edge carries one release, so the application loads once at start and
//! serves every call.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use wamn_component_policy::AdmittedComponent;

use crate::native_operation::native_workload::{NativeApp, NativeWorkloadSpec, load_native_app};
use wamn_engine::operation::next_scope;
use wash_runtime::engine::Engine;
use wash_runtime::observability::{MeterKind, Meters};
use wash_runtime::plugin::{HostPlugin, PluginBindings};

use crate::policy::{EDGE_POLICY_ID, EdgePolicy};
use crate::release::EdgeRelease;

/// The one native application of the edge release.
#[derive(Debug)]
pub struct EdgeApp {
    app: Arc<NativeApp>,
}

impl EdgeApp {
    /// Load every component of `release` under a new [`EdgePolicy`].
    pub async fn load(engine: Arc<Engine>, release: &EdgeRelease) -> anyhow::Result<Self> {
        let policy = Arc::new(EdgePolicy::new(release.components(), release.release())?);
        let world = policy.world();
        let host_interfaces = world
            .imports
            .into_iter()
            .chain(world.exports)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let plugins: HashMap<&'static str, Arc<dyn HostPlugin>> =
            HashMap::from([(EDGE_POLICY_ID, Arc::clone(&policy) as Arc<dyn HostPlugin>)]);
        let target = release.release().target();
        let app = load_native_app(
            engine,
            NativeWorkloadSpec {
                id: next_scope("wamn-edge-app").into(),
                namespace: target.project_env_key.to_string(),
                name: target.env.to_string(),
                components: release.native_components(),
                local_resources: wash_runtime::types::LocalResources::default(),
                host_interfaces,
            },
            policy,
            &plugins,
            &PluginBindings::new(),
            &Meters::new(MeterKind::Duration),
        )
        .await?;
        Ok(Self { app })
    }

    /// The application loaded at start, when `components` is its closure.
    pub(crate) fn released_app(
        &self,
        components: &[AdmittedComponent],
    ) -> anyhow::Result<&Arc<NativeApp>> {
        let loaded = &self.app.workload.facts_by_component_id;
        let closure = components.len() == loaded.len()
            && components
                .iter()
                .all(|fact| loaded.values().any(|loaded| loaded == fact));
        anyhow::ensure!(closure, "native-release-component-closure-mismatch");
        Ok(&self.app)
    }
}
