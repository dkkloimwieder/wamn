//! The trusted `wamn:runner/egress` channel — per-flow outbound allowlists
//! (fqg.11).
//!
//! The run-worker drives a SINGLE long-lived flowrunner component; the host
//! never sees a per-run boundary, so it cannot resolve "the current flow's
//! allowed hosts" itself. Exactly the cjv.3 grant shape: the trusted,
//! compiled-in flow-runner declares each run's `allowed-hosts` (from the flow
//! definition) through a channel linked ONLY into its world, and the host
//! enforces it on the outgoing-`wasi:http` path (`RunnerEgress` in
//! run_worker.rs).
//!
//! Host-enforced invariants:
//!
//! - **Deny-all default:** a component with NO declaration — or a declared
//!   EMPTY list — gets no egress. Egress is opt-in by declaration, exactly
//!   like credentials.
//! - **Intersection:** a request must pass BOTH the runner's host-level
//!   allowlist and the declared per-flow set. Declaring a host the host-level
//!   list refuses grants nothing.
//! - **Fail-closed parsing:** a declared entry the [`AllowedHost`] grammar
//!   rejects is dropped (warned, target `wamn::egress`) — a typo narrows
//!   access, never widens it. Structural validation at flow registration
//!   (`wamn-flow` `invalid-allowed-host`) catches the grossly malformed.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

use wash_runtime::engine::ctx::{ActiveCtx, SharedCtx, extract_active_ctx};
use wash_runtime::host::allowed_hosts::AllowedHost;
use wash_runtime::plugin::HostPlugin;
use wash_runtime::wasmtime::component::Linker;
use wash_runtime::wit::{WitInterface, WitWorld};

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        world: "egress-plugin",
        imports: { default: async | trappable | tracing },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

use bindings::wamn::runner::egress;

pub const RUNNER_EGRESS_ID: &str = "wamn-runner-egress";

/// Wire the TRUSTED `wamn:runner/egress` `set-allowed-hosts` channel into a
/// linker. Call this ONLY for the trusted, compiled-in flow-runner — the sole
/// component allowed to declare its own per-run egress (the cjv.3 trust
/// argument; a custom node must never get this).
pub fn add_runner_to_linker(linker: &mut Linker<SharedCtx>) -> wash_runtime::wasmtime::Result<()> {
    egress::add_to_linker::<_, SharedCtx>(linker, extract_active_ctx)
}

/// The per-component declared egress sets: component id → the parsed
/// `allowed-hosts` of the flow it is currently running. Registered as a host
/// plugin so the guest-facing declaration channel can reach it through
/// [`ActiveCtx`]; the run-worker's outgoing handler holds its own [`Arc`] and
/// reads [`declared`](Self::declared) per request.
#[derive(Default)]
pub struct RunnerEgressPolicy {
    declared: RwLock<HashMap<String, Arc<[AllowedHost]>>>,
}

impl RunnerEgressPolicy {
    /// Register (or replace) `component_id`'s declared egress set. Entries the
    /// [`AllowedHost`] grammar rejects are dropped with a warning —
    /// fail-closed, the run proceeds with the narrower set.
    pub fn set_declared(&self, component_id: &str, hosts: &[String]) {
        let parsed: Arc<[AllowedHost]> = hosts
            .iter()
            .filter_map(|h| match h.parse::<AllowedHost>() {
                Ok(a) => Some(a),
                Err(e) => {
                    tracing::warn!(
                        target: "wamn::egress",
                        component = component_id,
                        host = %h,
                        error = %e,
                        "declared allowed-host entry dropped (unparseable, fail-closed)"
                    );
                    None
                }
            })
            .collect();
        self.declared
            .write()
            .expect("declared lock poisoned")
            .insert(component_id.to_string(), parsed);
    }

    /// The component's declared egress set. `None` (never declared) and
    /// `Some(empty)` both mean deny-all to the caller — the distinction only
    /// matters for logging.
    pub fn declared(&self, component_id: &str) -> Option<Arc<[AllowedHost]>> {
        self.declared
            .read()
            .expect("declared lock poisoned")
            .get(component_id)
            .cloned()
    }
}

impl HostPlugin for RunnerEgressPolicy {
    fn id(&self) -> &'static str {
        RUNNER_EGRESS_ID
    }

    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::from([WitInterface::from("wamn:runner/egress@0.1.0")]),
            exports: HashSet::new(),
        }
    }
}

fn plugin_of(
    ctx: &ActiveCtx<'_>,
) -> wash_runtime::wasmtime::Result<Arc<RunnerEgressPolicy>> {
    ctx.try_get_plugin::<RunnerEgressPolicy>(RUNNER_EGRESS_ID)
}

impl egress::Host for ActiveCtx<'_> {
    /// The trusted flow-runner declares the hosts the run it is about to
    /// dispatch may reach. Only components linked with
    /// [`add_runner_to_linker`] can call this.
    async fn set_allowed_hosts(
        &mut self,
        hosts: Vec<String>,
    ) -> wash_runtime::wasmtime::Result<()> {
        let plugin = plugin_of(self)?;
        let component = self.component_id.to_string();
        tracing::debug!(
            target: "wamn::egress",
            component,
            hosts = ?hosts,
            "per-run egress declared"
        );
        plugin.set_declared(&component, &hosts);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undeclared_component_has_no_egress_set() {
        let policy = RunnerEgressPolicy::default();
        assert!(policy.declared("runner").is_none());
    }

    #[test]
    fn declaration_replaces_and_unparseable_entries_drop() {
        let policy = RunnerEgressPolicy::default();
        policy.set_declared(
            "runner",
            &["notify.example".into(), "*bad-wildcard".into()],
        );
        let set = policy.declared("runner").expect("declared");
        // The bad wildcard dropped fail-closed; the valid entry survived.
        assert_eq!(set.len(), 1);

        // A later declaration REPLACES (the next run's flow may declare less).
        policy.set_declared("runner", &[]);
        let set = policy.declared("runner").expect("declared");
        assert!(set.is_empty(), "declared-empty is stored, and means deny-all");
    }
}
