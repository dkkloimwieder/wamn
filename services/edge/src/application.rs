//! The loaded application of the edge release.
//!
//! The box carries one release, so the application loads once at start and
//! serves every call.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use wamn_catalog::AdmittedComponent;
use wamn_engine::operation::invocation_policy::ApplicationHost;
use wamn_engine::operation::native_workload::{
    NativeApplication, NativeWorkloadSpec, load_native_application,
};
use wamn_engine::operation::next_scope;
use wamn_engine::warm_reuse::WarmReuse;
use wash_runtime::engine::Engine;
use wash_runtime::observability::{MeterKind, Meters};
use wash_runtime::plugin::{HostPlugin, PluginBindings};

use crate::policy::{EDGE_POLICY_ID, EdgePolicy};
use crate::release::EdgeRelease;

/// The one native application of the edge release.
#[derive(Debug)]
pub struct EdgeApplication {
    application: Arc<NativeApplication<EdgePolicy>>,
}

impl EdgeApplication {
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
        let manifest = release.release().manifest();
        let application = load_native_application(
            engine,
            NativeWorkloadSpec {
                id: next_scope("wamn-edge-application").into(),
                namespace: manifest.release.tenant_id.clone(),
                name: manifest.release.environment.clone(),
                components: release.native_components(),
                warm_reuse: WarmReuse::default(),
                local_resources: wash_runtime::types::LocalResources::default(),
                host_interfaces,
            },
            policy,
            &plugins,
            &PluginBindings::new(),
            &Meters::new(MeterKind::Duration),
        )
        .await?;
        Ok(Self { application })
    }
}

impl ApplicationHost for EdgeApplication {
    type Policy = EdgePolicy;

    /// The application loaded at start, when `components` is its closure.
    fn released_application(
        &self,
        components: &[AdmittedComponent],
    ) -> impl Future<Output = anyhow::Result<Arc<NativeApplication<EdgePolicy>>>> + Send {
        let loaded = &self.application.workload.facts_by_component_id;
        let closure = components.len() == loaded.len()
            && components
                .iter()
                .all(|fact| loaded.values().any(|loaded| loaded == fact));
        std::future::ready(if closure {
            Ok(Arc::clone(&self.application))
        } else {
            Err(anyhow::anyhow!("native-release-component-closure-mismatch"))
        })
    }
}
