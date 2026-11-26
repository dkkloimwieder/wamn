//! The per-call authority of the edge application.
//!
//! An edge application imports `wamn:node/types` only, so a call needs no
//! invocation binding. Activation tests the deadline, that the operation is
//! admitted and released, and the released-operation grant under the caller.

use crate::serving_context::ServingContext;
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use crate::native_operation::native_call::NativeInvocation;
use crate::native_operation::native_workload::native_component_name;
use anyhow::Context as _;
use tokio::time::Instant;
use wamn_app::ingress::AuthenticatedCaller;
use wamn_authority::edge::EdgeAuthority;
use wamn_component_policy::AdmittedComponent;
use wash_runtime::engine::workload::WorkloadItem;
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wit::{WitInterface, WitWorld};

/// Host-plugin identity of the edge policy.
pub const EDGE_POLICY_ID: &str = "wamn:edge-operation-policy";

/// The one import an edge application may have: the node ABI types.
const NODE_TYPES: &str = "wamn:node/types@0.2.0";

/// The facts of one edge call that only the policy reads.
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
    components: BTreeMap<String, Arc<AdmittedComponent>>,
    authority: EdgeAuthority,
}

impl EdgePolicy {
    /// A policy over `components`, each with its released operations.
    pub fn new(components: &[AdmittedComponent], release: &ServingContext) -> anyhow::Result<Self> {
        let mut policies = BTreeMap::new();
        for fact in components {
            release
                .manifest()
                .components
                .iter()
                .find(|served| {
                    served.package_id == fact.target.package_id
                        && served.descriptor.component == fact.component
                        && served.descriptor.component_digest == fact.component_digest
                        && served.descriptor.operations == fact.operations
                })
                .context("component-not-in-carried-release")?;
            policies.insert(native_component_name(fact)?, Arc::new(fact.clone()));
        }
        Ok(Self {
            components: policies,
            authority: EdgeAuthority::default(),
        })
    }

    pub(crate) fn activate(
        &self,
        component_id: &str,
        request: &NativeInvocation,
    ) -> anyhow::Result<()> {
        self.authority.authorize(
            component_id,
            request.operation.as_str(),
            request.facts.caller.as_ref(),
            Instant::now().into_std(),
            request.deadline.into_std(),
        )
    }

    pub(crate) fn shutdown(&self) {
        self.authority.clear();
    }
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
            .flat_map(|component| component.operations.keys())
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
        let component = self
            .components
            .get(name)
            .context("native binding does not name an admitted component")?;
        self.authority
            .bind(item.id().to_owned(), Arc::clone(component));
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
