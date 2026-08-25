//! The trusted `wamn:runner/egress` channel for connection-derived outbound
//! authority (fqg.11).
//!
//! The run-worker drives a SINGLE long-lived runner component; the host
//! never sees a per-run boundary on the generic outgoing-handler path. The
//! trusted, compiled-in runner therefore supplies the hosts resolved from
//! the active connection instance through a channel linked ONLY into its world,
//! and the host enforces them on the outgoing-`wasi:http` path (`RunnerEgress`
//! in `crates/execution/host/src/lib.rs`).
//!
//! Host-enforced invariants:
//!
//! - **Deny-all default:** a component with NO declaration — or a declared
//!   EMPTY list — gets no egress. Egress is opt-in by resolved authority.
//! - **Intersection:** a request must pass BOTH the runner's host-level
//!   allowlist and the resolved connection set. A connection cannot widen the
//!   host-level list.
//! - **Fail-closed parsing:** a declared entry the [`AllowedHost`] grammar
//!   rejects is dropped (warned, target `wamn::egress`) — a typo narrows
//!   access, never widens it.

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
/// linker. Call this ONLY for the trusted, compiled-in runner — the sole
/// component allowed to supply resolved per-run egress authority; ordinary
/// components must never get this.
pub fn add_runner_to_linker(linker: &mut Linker<SharedCtx>) -> wash_runtime::wasmtime::Result<()> {
    egress::add_to_linker::<_, SharedCtx>(linker, extract_active_ctx)
}

/// The per-component resolved egress sets: component id → the parsed hosts of
/// the active connection instance. Registered as a host
/// plugin so the guest-facing declaration channel can reach it through
/// [`ActiveCtx`]; the run-worker's outgoing handler holds its own [`Arc`] and
/// reads [`declared`](Self::declared) per request.
#[derive(Default)]
pub struct RunnerEgressPolicy {
    declared: RwLock<HashMap<String, Arc<[AllowedHost]>>>,
}

impl RunnerEgressPolicy {
    /// Register (or replace) `component_id`'s resolved egress set. Entries the
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

    /// Remove `component_id`'s declaration entirely (wamn-0h0g.17.10).
    ///
    /// The counterpart of [`set_declared`](Self::set_declared), for a claim
    /// scope that is a POOLED execution instance: the declaration belongs to the
    /// run that supplied it, so it must leave when that run's checkout ends
    /// rather than stand until the next run overwrites it. `allows_connection`
    /// treats an absent declaration as "no flow-level narrowing", so removing it
    /// restores exactly the never-supplied state a prewarmed instance has.
    pub fn clear_declared(&self, component_id: &str) {
        self.declared
            .write()
            .expect("declared lock poisoned")
            .remove(component_id);
    }

    /// The component's resolved egress set. `None` (never supplied) and
    /// `Some(empty)` both mean deny-all to the caller — the distinction only
    /// matters for logging.
    pub fn declared(&self, component_id: &str) -> Option<Arc<[AllowedHost]>> {
        self.declared
            .read()
            .expect("declared lock poisoned")
            .get(component_id)
            .cloned()
    }

    /// Apply optional flow-level narrowing to an already-authorized portable
    /// connection authority. Absent/empty means no additional narrowing; the
    /// environment-owned connection plus host policy remain authoritative.
    pub fn allows_connection(&self, component_id: &str, uri: &hyper::Uri) -> bool {
        self.declared(component_id)
            .as_deref()
            .is_none_or(|hosts| hosts.is_empty() || hosts.iter().any(|host| host.matches(uri)))
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

fn plugin_of(ctx: &ActiveCtx<'_>) -> wash_runtime::wasmtime::Result<Arc<RunnerEgressPolicy>> {
    ctx.try_get_plugin::<RunnerEgressPolicy>(RUNNER_EGRESS_ID)
}

impl egress::Host for ActiveCtx<'_> {
    /// The trusted runner declares the hosts the run it is about to
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
        policy.set_declared("runner", &["notify.example".into(), "*bad-wildcard".into()]);
        let set = policy.declared("runner").expect("declared");
        // The bad wildcard dropped fail-closed; the valid entry survived.
        assert_eq!(set.len(), 1);

        // A later declaration REPLACES (the next run's flow may declare less).
        policy.set_declared("runner", &[]);
        let set = policy.declared("runner").expect("declared");
        assert!(set.is_empty(), "declared-empty is stored");
    }

    /// wamn-0h0g.17.10 — clearing REMOVES rather than replaces.
    ///
    /// A pooled instance's declaration must not survive the checkout that
    /// supplied it. `set_declared(id, &[])` is not a substitute: an empty
    /// declaration is stored and read back as `Some(empty)`, which is a
    /// deny-all NARROWING, while an absent one is the never-supplied state a
    /// prewarmed instance has. Both halves are asserted so a clear implemented
    /// as an empty write fails here.
    #[test]
    fn clearing_a_declaration_removes_it_rather_than_storing_an_empty_one() {
        let policy = RunnerEgressPolicy::default();
        policy.set_declared("runner", &["notify.example".into()]);
        policy.set_declared("other-runner", &["notify.example".into()]);

        policy.clear_declared("runner");
        assert_eq!(
            policy.declared("runner").map(|hosts| hosts.len()),
            None,
            "a cleared scope is absent, not declared-empty"
        );
        assert_eq!(
            policy.declared("other-runner").map(|hosts| hosts.len()),
            Some(1),
            "clearing one claim scope must not disturb another's declaration"
        );
    }

    #[test]
    fn empty_declaration_does_not_narrow_a_portable_connection() {
        let policy = RunnerEgressPolicy::default();
        policy.set_declared("runner", &[]);
        let target = "http://serve-echo:8091/hook"
            .parse()
            .expect("logical connection URL");

        assert!(policy.allows_connection("runner", &target));
    }
}
