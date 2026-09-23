//! Load admitted application facts through native workload compilation and resolution.
//!
//! Native compilation is keyed by verified component bytes. Component names identify
//! the complete admitted fact, so sharing compiled bytes never shares authority.

use std::collections::HashMap;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::Context as _;
use wamn_catalog::AdmittedComponent;
use wamn_engine::component_admission::component_digest;
use wash_runtime::engine::Engine;
use wash_runtime::engine::workload::ResolvedWorkload;
use wash_runtime::host::http::NullServer;
use wash_runtime::observability::Meters;
use wash_runtime::plugin::{HostPlugin, PluginBindings};
use wash_runtime::types::{Component, LocalResources, Workload};
use wash_runtime::wit::WitInterface;

use super::invocation_policy::InvocationPolicy;

#[cfg(test)]
mod tests;

/// One admitted authority fact and the exact bytes it names.
#[derive(Debug)]
pub(crate) struct NativeComponent {
    pub(crate) fact: AdmittedComponent,
    pub(crate) bytes: Vec<u8>,
}

/// The immutable release or candidate workload selected by the owning driver.
#[derive(Debug)]
pub(super) struct NativeWorkloadSpec {
    pub(super) warm_reuse: crate::warm_reuse::WarmReuse,
    pub(super) id: String,
    pub(super) namespace: String,
    pub(super) name: String,
    pub(super) components: Vec<NativeComponent>,
    pub(super) local_resources: LocalResources,
    pub(super) host_interfaces: Vec<WitInterface>,
}

/// A native workload and its admitted facts, keyed by native component identity.
#[derive(Debug)]
pub(crate) struct NativeWorkload {
    pub(crate) resolved: ResolvedWorkload,
    pub(super) facts_by_component_id: BTreeMap<String, AdmittedComponent>,
}

/// One release or candidate lifetime, retained by every native call it owns.
#[derive(Debug)]
pub(crate) struct NativeApplication<P: InvocationPolicy> {
    // Cleanup runs before the workload fields drop. The policy contains only
    // synchronous WAMN registries; native workload teardown owns warm stores.
    _cleanup: NativePolicyCleanup<P>,
    pub(crate) workload: Arc<NativeWorkload>,
    pub(super) policy: Arc<P>,
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
pub(super) async fn load_native_application<P: InvocationPolicy>(
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
    let application = Arc::new(NativeApplication {
        _cleanup: cleanup,
        workload,
        policy,
    });
    application.policy.bind_application(&application)?;
    Ok(application)
}

/// Identify a complete admitted fact before native plugin binding begins.
///
/// This name is metadata identity, not the digest used by the compilation cache.
pub(super) fn native_component_name(fact: &AdmittedComponent) -> anyhow::Result<String> {
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
    let workload = Workload {
        namespace: spec.namespace,
        name: spec.name,
        annotations: HashMap::new(),
        service: None,
        components,
        host_interfaces: spec.host_interfaces,
        volumes: Vec::new(),
    };
    let unresolved =
        tokio::task::spawn_blocking(move || engine.initialize_workload(&spec.id, workload))
            .await
            .context("join native application workload initialization")?
            .context("initialize native application workload")?;
    let resolved = unresolved
        .resolve(
            Some(plugins),
            plugin_bindings,
            Arc::new(NullServer::default()),
            meters,
        )
        .await
        .context("resolve native application workload")?;
    let mapped_facts = {
        let native_components = resolved.components();
        let native_components = native_components.read().await;
        native_components
            .values()
            .map(|component| {
                let fact = facts_by_name
                    .remove(component.name())
                    .ok_or_else(|| anyhow::anyhow!("native-component-admitted-fact-missing"))?;
                Ok((component.id().to_owned(), fact))
            })
            .collect::<anyhow::Result<BTreeMap<_, _>>>()
            .and_then(|facts| {
                anyhow::ensure!(
                    facts_by_name.is_empty(),
                    "native-workload-admitted-component-missing"
                );
                Ok(facts)
            })
    };
    let facts_by_component_id = match mapped_facts {
        Ok(facts) => facts,
        Err(error) => {
            resolved
                .unbind_all_plugins()
                .await
                .context("unbind native workload after component identity mismatch")?;
            return Err(error);
        }
    };
    Ok(NativeWorkload {
        resolved,
        facts_by_component_id,
    })
}
