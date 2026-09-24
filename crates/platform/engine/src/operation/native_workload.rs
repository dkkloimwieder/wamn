//! Load admitted application facts through native workload compilation and resolution.
//!
//! Native compilation is keyed by verified component bytes. Component names identify
//! the complete admitted fact, so sharing compiled bytes never shares authority.

use std::collections::HashMap;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::component_admission::component_digest;
use anyhow::Context as _;
use wamn_catalog::AdmittedComponent;
use wash_runtime::engine::Engine;
use wash_runtime::engine::InstancePolicy;
use wash_runtime::engine::dispatch::DispatchTarget;
use wash_runtime::engine::workload::ResolvedWorkload;
use wash_runtime::host::HostRef;
use wash_runtime::host::http::{HostHandler, NullServer};
use wash_runtime::observability::Meters;
use wash_runtime::plugin::{HostPlugin, PluginBindings};
use wash_runtime::types::{Component, LocalResources, Workload};
use wash_runtime::wit::WitInterface;

use super::invocation_policy::InvocationPolicy;

#[cfg(test)]
mod tests;

/// One admitted authority fact and the exact bytes it names.
#[derive(Debug)]
pub struct NativeComponent {
    pub fact: AdmittedComponent,
    pub bytes: Vec<u8>,
}

/// The immutable release or candidate workload selected by the owning driver.
#[derive(Debug)]
pub struct NativeWorkloadSpec {
    pub warm_reuse: crate::warm_reuse::WarmReuse,
    pub id: String,
    pub namespace: String,
    pub name: String,
    pub components: Vec<NativeComponent>,
    pub local_resources: LocalResources,
    pub host_interfaces: Vec<WitInterface>,
}

/// One native workload per admitted component, and the admitted facts, both
/// keyed by native component identity.
///
/// An admitted component is one application, composed at build, so a workload
/// is one application. wash-runtime links one component's export to a
/// sibling's import inside a workload. With one application per workload no
/// sibling exists, and nothing links across applications (wamn-2u14). A call
/// across applications is a workflow call through the host.
pub struct NativeWorkload {
    workloads: BTreeMap<String, ResolvedWorkload>,
    pub facts_by_component_id: BTreeMap<String, AdmittedComponent>,
    // The workload's stores hold this handler weakly, so the application keeps
    // it alive. `NullServer` refuses every outgoing request.
    _egress: Arc<dyn HostHandler>,
}

impl std::fmt::Debug for NativeWorkload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeWorkload")
            .field("workloads", &self.workloads)
            .field("facts_by_component_id", &self.facts_by_component_id)
            .finish_non_exhaustive()
    }
}

impl NativeWorkload {
    fn resolved(&self, component_id: &str) -> anyhow::Result<&ResolvedWorkload> {
        self.workloads
            .get(component_id)
            .context("native-workload-component-missing")
    }

    /// The native dispatch target of one admitted component.
    pub async fn dispatch_target(
        &self,
        component_id: &str,
        plugin: &'static str,
    ) -> anyhow::Result<DispatchTarget> {
        self.resolved(component_id)?
            .dispatch_target(component_id, plugin)
            .await
    }

    /// The native instance policy of one admitted component.
    pub async fn warm_instance_policy(&self, component_id: &str) -> anyhow::Result<InstancePolicy> {
        Ok(self
            .resolved(component_id)?
            .warm_instance_policy(component_id)
            .await)
    }

    /// Unbind every plugin from every component workload, and report the first failure.
    pub async fn unbind_all_plugins(&self) -> anyhow::Result<()> {
        unbind_all(self.workloads.values()).await
    }
}

async fn unbind_all<'a>(
    workloads: impl IntoIterator<Item = &'a ResolvedWorkload>,
) -> anyhow::Result<()> {
    let mut first = Ok(());
    for workload in workloads {
        let result = workload.unbind_all_plugins().await;
        if first.is_ok() {
            first = result;
        }
    }
    first
}

/// One release or candidate lifetime, retained by every native call it owns.
#[derive(Debug)]
pub struct NativeApplication<P: InvocationPolicy> {
    // Cleanup runs before the workload fields drop. The policy contains only
    // synchronous WAMN registries; native workload teardown owns warm stores.
    _cleanup: NativePolicyCleanup<P>,
    pub workload: Arc<NativeWorkload>,
    pub policy: Arc<P>,
}

#[derive(Debug)]
struct NativePolicyCleanup<P: InvocationPolicy>(Arc<P>);

impl<P: InvocationPolicy> Drop for NativePolicyCleanup<P> {
    fn drop(&mut self) {
        self.0.shutdown();
    }
}

/// Load one application with a fresh policy and cancellation-safe ownership.
///
/// The guard precedes resolution because native rollback covers returned errors,
/// but dropping a pending resolution does not run the plugin unbind callbacks.
#[expect(
    clippy::implicit_hasher,
    reason = "wash-runtime takes the plugin map with the default hasher"
)]
pub async fn load_native_application<P: InvocationPolicy>(
    engine: Arc<Engine>,
    spec: NativeWorkloadSpec,
    policy: Arc<P>,
    plugins: &HashMap<&'static str, Arc<dyn HostPlugin>>,
    plugin_bindings: &PluginBindings,
    meters: &Meters,
) -> anyhow::Result<Arc<NativeApplication<P>>> {
    let cleanup = NativePolicyCleanup(Arc::clone(&policy));
    let workload =
        Arc::new(load_native_workload(engine, spec, plugins, plugin_bindings, meters).await?);
    Ok(Arc::new(NativeApplication {
        _cleanup: cleanup,
        workload,
        policy,
    }))
}

/// Identify a complete admitted fact before native plugin binding begins.
///
/// This name is metadata identity, not the digest used by the compilation cache.
pub fn native_component_name(fact: &AdmittedComponent) -> anyhow::Result<String> {
    let value = serde_json::to_value(fact).context("encode admitted component identity")?;
    Ok(format!(
        "wamn-fact:{}",
        wamn_execution_contract::canonical_json_sha256(&value)
    ))
}

/// Verify all supplied bytes, then load and resolve one native application workload.
///
/// Released and candidate inputs take the same verification path. The caller owns
/// admission, workload identity, and eventual native plugin unbinding.
pub(super) async fn load_native_workload(
    engine: Arc<Engine>,
    spec: NativeWorkloadSpec,
    plugins: &HashMap<&'static str, Arc<dyn HostPlugin>>,
    plugin_bindings: &PluginBindings,
    meters: &Meters,
) -> anyhow::Result<NativeWorkload> {
    let mut facts_by_name = BTreeMap::new();
    let mut components = Vec::new();
    for input in spec.components {
        // Native trusts a supplied digest even on a cache hit. Check every
        // input, including repeats, before any digest reaches that cache.
        anyhow::ensure!(
            component_digest(&input.bytes) == input.fact.component_digest,
            "native-component-bytes-digest-mismatch"
        );
        let name = native_component_name(&input.fact)?;
        if let Some(existing) = facts_by_name.get(&name) {
            anyhow::ensure!(
                existing == &input.fact,
                "native-component-metadata-identity-mismatch"
            );
            continue;
        }
        let mut component = Component {
            name: name.clone(),
            bytes: input.bytes.into(),
            digest: Some(input.fact.component_digest.clone()),
            local_resources: spec.local_resources.clone(),
            ..Component::default()
        };
        spec.warm_reuse.apply(&mut component);
        components.push(component);
        facts_by_name.insert(name, input.fact);
    }
    // Admission has already matched this import list to the exact bytes. Only
    // imported interfaces need a unique provider. Wiring selects export-only
    // palette handlers by full admitted fact, not by their shared interface.
    let imports: BTreeSet<_> = facts_by_name
        .values()
        .flat_map(|fact| fact.imports.iter().map(String::as_str))
        .collect();
    let mut providers = BTreeMap::new();
    for fact in facts_by_name.values() {
        for export in fact.operations.keys() {
            if imports.contains(export.as_str())
                && let Some(previous) = providers.insert(export, fact)
            {
                anyhow::bail!(
                    "imported operation interface {export:?} has ambiguous component providers: {}::{} digest {} and {}::{} digest {}",
                    previous.scope.package_id,
                    previous.component,
                    previous.component_digest,
                    fact.scope.package_id,
                    fact.component,
                    fact.component_digest,
                );
            }
        }
    }
    let egress: Arc<dyn HostHandler> = Arc::new(NullServer::default());
    let host = HostRef::from_handler(&egress);
    let mut workloads = BTreeMap::new();
    let mut facts_by_component_id = BTreeMap::new();
    for component in components {
        // Boxed so the loop does not carry native resolution inline in every caller's future.
        let loaded = Box::pin(load_component_workload(
            &engine,
            &spec.id,
            &spec.namespace,
            &spec.name,
            component,
            &spec.host_interfaces,
            plugins,
            plugin_bindings,
            &host,
            meters,
            &mut facts_by_name,
        ))
        .await;
        match loaded {
            Ok((component_id, resolved, fact)) => {
                facts_by_component_id.insert(component_id.clone(), fact);
                workloads.insert(component_id, resolved);
            }
            Err(error) => {
                unbind_all(workloads.values())
                    .await
                    .context("unbind native workloads after a component failed to load")?;
                return Err(error);
            }
        }
    }
    Ok(NativeWorkload {
        workloads,
        facts_by_component_id,
        _egress: egress,
    })
}

/// Initialize and resolve one admitted component as a workload of its own.
#[expect(
    clippy::too_many_arguments,
    reason = "one component takes the application's identity, bindings and meters"
)]
async fn load_component_workload(
    engine: &Arc<Engine>,
    application_id: &str,
    namespace: &str,
    name: &str,
    component: Component,
    host_interfaces: &[WitInterface],
    plugins: &HashMap<&'static str, Arc<dyn HostPlugin>>,
    plugin_bindings: &PluginBindings,
    host: &HostRef,
    meters: &Meters,
    facts_by_name: &mut BTreeMap<String, AdmittedComponent>,
) -> anyhow::Result<(String, ResolvedWorkload, AdmittedComponent)> {
    // The component name is a fixed-length fact digest, so no workload id is a
    // prefix of another. Plugins clear their state by workload id prefix.
    let id = format!("{application_id}/{}", component.name);
    let fact = facts_by_name
        .remove(&component.name)
        .context("native-component-admitted-fact-missing")?;
    let workload = Workload {
        namespace: namespace.to_owned(),
        name: name.to_owned(),
        annotations: HashMap::new(),
        service: None,
        components: vec![component],
        host_interfaces: host_interfaces.to_vec(),
        volumes: Vec::new(),
    };
    let engine = Arc::clone(engine);
    let unresolved = tokio::task::spawn_blocking(move || engine.initialize_workload(&id, workload))
        .await
        .context("join native application workload initialization")?
        .context("initialize native application workload")?;
    let resolved = unresolved
        .resolve(Some(plugins), plugin_bindings, host, meters)
        .await
        .context("resolve native application workload")?;
    let component_id = {
        let native_components = resolved.components();
        let native_components = native_components.read().await;
        let mut ids = native_components.keys();
        match (ids.next(), ids.next()) {
            (Some(id), None) => Ok(id.to_string()),
            _ => Err(anyhow::anyhow!(
                "native-workload-admitted-component-missing"
            )),
        }
    };
    match component_id {
        Ok(component_id) => Ok((component_id, resolved, fact)),
        Err(error) => {
            resolved
                .unbind_all_plugins()
                .await
                .context("unbind native workload after component identity mismatch")?;
            Err(error)
        }
    }
}
