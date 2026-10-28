//! The per-call authority of the edge application.
//!
//! An edge application imports `wamn:node/types` only, so a call needs no
//! capability binding. Activation checks the deadline, that the operation is
//! admitted and released, and the released-operation grant under the caller.

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, RwLock};

use anyhow::Context as _;
use tokio::time::Instant;
use wamn_catalog::{AdmittedComponent, ServingComponentOperation};
use wamn_engine::flow_http_routing::AuthenticatedCaller;
use wamn_engine::operation::invocation_policy::{
    InvocationPolicy, InvocationScope, NoItemTransaction,
};
use wamn_engine::operation::native_call::{NativeCallFailure, NativeInvocation};
use wamn_engine::operation::native_workload::native_component_name;
use wamn_engine::release_manifest::LoadedRelease;
use wamn_engine::router_delivery::authorize_released_operation;
use wash_runtime::engine::workload::WorkloadItem;
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wit::{WitInterface, WitWorld};

/// Host-plugin identity of the edge policy.
pub const EDGE_POLICY_ID: &str = "wamn:edge-operation-policy";

/// The one import an edge application may have: the node ABI types.
const NODE_TYPES: &str = "wamn:node/types@0.1.0";

/// One admitted component and the operations its release publishes for it.
#[derive(Debug)]
struct ComponentPolicy {
    fact: AdmittedComponent,
    operations: BTreeMap<String, ServingComponentOperation>,
}

/// The facts of one edge call that only the policy reads.
#[derive(Clone)]
pub struct EdgeFacts {
    caller: Option<AuthenticatedCaller>,
}

impl std::fmt::Debug for EdgeFacts {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EdgeFacts")
            .field("caller_attached", &self.caller.is_some())
            .finish()
    }
}

impl EdgeFacts {
    /// The facts of a call that `caller` entered.
    pub fn entry(caller: Option<AuthenticatedCaller>) -> Self {
        Self { caller }
    }
}

/// The admitted components of the loaded release and their native bindings.
#[derive(Debug)]
pub struct EdgePolicy {
    components: BTreeMap<String, Arc<ComponentPolicy>>,
    bindings: RwLock<BTreeMap<String, Arc<ComponentPolicy>>>,
}

impl EdgePolicy {
    /// A policy over `components`, each with its released operations.
    pub fn new(components: &[AdmittedComponent], release: &LoadedRelease) -> anyhow::Result<Self> {
        let mut policies = BTreeMap::new();
        for fact in components {
            let served = release
                .manifest()
                .components
                .iter()
                .find(|served| {
                    served.package_id == fact.scope.package_id
                        && served.descriptor.component == fact.component
                        && served.descriptor.component_digest.as_str() == fact.component_digest
                })
                .context("component-not-in-carried-release")?;
            policies.insert(
                native_component_name(fact)?,
                Arc::new(ComponentPolicy {
                    fact: fact.clone(),
                    operations: served.operations.clone(),
                }),
            );
        }
        Ok(Self {
            components: policies,
            bindings: RwLock::default(),
        })
    }

    /// Check one call: the deadline, the admitted and released operation, and
    /// the released-operation grant under the caller.
    fn authorize(
        &self,
        component_id: &str,
        request: &NativeInvocation<Self>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            Instant::now() < request.deadline,
            "native-node-deadline-exceeded"
        );
        let bindings = self
            .bindings
            .read()
            .expect("edge component bindings lock poisoned");
        let component = bindings
            .get(component_id)
            .context("native invocation component is not admitted")?;
        let operation = request.operation.as_str();
        anyhow::ensure!(
            component.fact.operation(operation).is_some(),
            "native invocation operation is not admitted"
        );
        let released = component
            .operations
            .get(operation)
            .context("native invocation operation is not released")?;
        authorize_released_operation(request.facts.caller.as_ref(), released)?;
        Ok(())
    }
}

impl InvocationPolicy for EdgePolicy {
    type Facts = EdgeFacts;
    /// No capability is bound, so nothing is revoked.
    type Authority = ();
    /// The edge has no SQL.
    type ItemTransaction = NoItemTransaction;

    fn begin_item(
        &self,
        _component_id: &str,
        _operation: &str,
        _facts: &EdgeFacts,
    ) -> impl Future<Output = anyhow::Result<Option<NoItemTransaction>>> + Send {
        std::future::ready(Ok(None))
    }

    fn item_facts(&self, _facts: &EdgeFacts, transaction: &NoItemTransaction) -> EdgeFacts {
        match *transaction {}
    }

    fn activate(
        &self,
        component_id: &str,
        _invocation_scope: &InvocationScope<Self>,
        request: &NativeInvocation<Self>,
        _failure: NativeCallFailure,
    ) -> impl Future<Output = anyhow::Result<()>> + Send {
        std::future::ready(self.authorize(component_id, request))
    }

    fn shutdown(&self) {
        self.bindings
            .write()
            .expect("edge component bindings lock poisoned")
            .clear();
    }

    fn revoke(&self, _scope: &str) {}
}

#[async_trait::async_trait]
impl HostPlugin for EdgePolicy {
    fn id(&self) -> &'static str {
        EDGE_POLICY_ID
    }

    fn world(&self) -> WitWorld {
        let exports = self
            .components
            .values()
            .flat_map(|policy| policy.fact.operations.keys())
            .map(|name| WitInterface::from(name.as_str()))
            .collect();
        WitWorld {
            imports: HashSet::from([WitInterface::from(NODE_TYPES)]),
            exports,
        }
    }

    async fn on_workload_item_bind<'a>(
        &self,
        item: &mut WorkloadItem<'a>,
        _interfaces: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        let name = match &*item {
            WorkloadItem::Component(component) => component.name(),
            WorkloadItem::Service(_) => anyhow::bail!("native-operation-service-not-admitted"),
        };
        let policy = self
            .components
            .get(name)
            .context("native binding does not name an admitted component")?;
        self.bindings
            .write()
            .expect("edge component bindings lock poisoned")
            .insert(item.id().to_owned(), Arc::clone(policy));
        Ok(())
    }

    async fn on_workload_unbind(
        &self,
        _workload_id: &str,
        _interfaces: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        self.shutdown();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::Duration;
    use wamn_engine::flow_http_routing::CredentialType;
    use wamn_engine::operation::native_workload::{
        NativeApplication, NativeWorkloadSpec, load_native_application,
    };
    use wamn_engine::operation::node_types;
    use wamn_engine::router_delivery::{OperationRefusal, OperationRefusalType};
    use wash_runtime::observability::{MeterKind, Meters};
    use wash_runtime::plugin::PluginBindings;

    use super::*;
    use crate::release::test_util::{OPERATION, release};

    async fn request() -> NativeInvocation<EdgePolicy> {
        let release = release().await;
        let policy = Arc::new(EdgePolicy::new(release.components(), release.release()).unwrap());
        let application: Arc<NativeApplication<EdgePolicy>> = load_native_application(
            Arc::new(wamn_engine::build_engine(&[]).unwrap()),
            NativeWorkloadSpec {
                warm_reuse: wamn_engine::warm_reuse::WarmReuse::default(),
                id: "edge-policy-test".into(),
                namespace: "test".into(),
                name: "test".into(),
                components: Vec::new(),
                entry: None,
                local_resources: wash_runtime::types::LocalResources::default(),
                host_interfaces: Vec::new(),
            },
            Arc::clone(&policy),
            &HashMap::new(),
            &PluginBindings::new(),
            &Meters::new(MeterKind::Off),
        )
        .await
        .unwrap();
        let component = policy.components.values().next().unwrap().clone();
        policy
            .bindings
            .write()
            .unwrap()
            .insert("bound".into(), component);
        NativeInvocation {
            operation: OPERATION.into(),
            context: node_types::NodeContext {
                wiring_id: String::new(),
                wiring_version: 0,
                node_id: String::new(),
                delivery_id: "test".into(),
                input_port: None,
                occurrence: 0,
                traceparent: None,
                tracestate: None,
                deadline_ms: Some(1000),
                config: "null".into(),
            },
            input: "{}".into(),
            deadline: Instant::now() + Duration::from_secs(60),
            facts: EdgeFacts::entry(Some(AuthenticatedCaller::new(
                "record-http",
                "principal",
                CredentialType::Session,
                false,
                [wamn_catalog::sealed_operation_reference(OPERATION).to_owned()].into(),
                Some("operator".into()),
            ))),
            application,
        }
    }

    #[tokio::test]
    async fn an_expired_deadline_refuses() {
        let mut request = request().await;
        let policy = &request.application.policy;
        policy.authorize("bound", &request).unwrap();
        request.deadline = Instant::now() - Duration::from_secs(1);
        assert_eq!(
            policy.authorize("bound", &request).unwrap_err().to_string(),
            "native-node-deadline-exceeded"
        );
    }

    #[tokio::test]
    async fn an_operation_not_released_refuses() {
        let request = request().await;
        let policy = &request.application.policy;
        policy.authorize("bound", &request).unwrap();
        {
            let mut bindings = policy.bindings.write().unwrap();
            let component = bindings.get("bound").unwrap();
            let fact = component.fact.clone();
            bindings.insert(
                "bound".into(),
                Arc::new(ComponentPolicy {
                    fact,
                    operations: BTreeMap::new(),
                }),
            );
        }
        assert_eq!(
            policy.authorize("bound", &request).unwrap_err().to_string(),
            "native invocation operation is not released"
        );
    }

    #[tokio::test]
    async fn an_unbound_component_refuses() {
        let request = request().await;
        let policy = &request.application.policy;
        policy.authorize("bound", &request).unwrap();
        assert_eq!(
            policy
                .authorize("unbound", &request)
                .unwrap_err()
                .to_string(),
            "native invocation component is not admitted"
        );
    }

    #[tokio::test]
    async fn a_caller_without_the_grant_refuses() {
        let mut request = request().await;
        let policy = &request.application.policy;
        policy.authorize("bound", &request).unwrap();
        request.facts = EdgeFacts::entry(Some(AuthenticatedCaller::new(
            "record-http",
            "principal",
            CredentialType::Session,
            false,
            HashSet::new(),
            Some("viewer".into()),
        )));
        let error = policy.authorize("bound", &request).unwrap_err();
        let refusal = error.downcast_ref::<OperationRefusal>().unwrap();
        assert_eq!(
            refusal.refusal_type(),
            OperationRefusalType::PermissionDenied
        );
        assert_eq!(refusal.operation(), OPERATION);
    }
}
