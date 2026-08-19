//! Cross-crate guard: the `main` / `error` port names are declared twice and
//! must stay equal (wamn-0h0g.16.17).
//!
//! `wamn-flow` and `wamn-router` each declare their own `MAIN_PORT` /
//! `ERROR_PORT`. These names are PERSISTED bytes: an edge stored with one
//! side's spelling is read back and filtered against the other's, so a rename
//! on one side alone never fails loudly — the walk simply finds no successors
//! and every affected edge goes silently dead. `wamn-runtime` is the only crate
//! that links BOTH declarations, which is why the guard lives here.
//!
//! Single-homing the constant was considered and REJECTED: it resolves the
//! duplication by COUPLING, buying the singularity with a new bounded-context
//! edge (wamn-catalog to wamn-router) plus a byte-exact regeneration of the
//! machine-generated `crates/execution/host/effect-provider-revision.json`,
//! which records per-package dependency edges. Do not "fix" this guard away by
//! merging the two declarations into one.
//!
//! `wamn-flow` retires in wamn-0h0g.26.5. When that lands, the flow-side arm
//! follows the constant wherever it goes, or this guard retires with it — do
//! not let it decay into a one-sided tautology comparing a crate with itself.

#[test]
fn flow_and_router_port_constants_agree() {
    assert_eq!(
        wamn_flow::MAIN_PORT,
        wamn_router::MAIN_PORT,
        "the main-port name is declared twice and the declarations have \
         diverged: crates/execution/flow-model/src/types.rs:22 \
         (wamn_flow::MAIN_PORT) and crates/execution/router/src/outcome.rs:14 \
         (wamn_router::MAIN_PORT). These must move together (wamn-0h0g.16.17) \
         — the port name is persisted in stored edges, so a one-sided rename \
         silently kills every edge on that port instead of failing"
    );
    assert_eq!(
        wamn_flow::ERROR_PORT,
        wamn_router::ERROR_PORT,
        "the error-port name is declared twice and the declarations have \
         diverged: crates/execution/flow-model/src/types.rs:25 \
         (wamn_flow::ERROR_PORT) and crates/execution/router/src/outcome.rs:17 \
         (wamn_router::ERROR_PORT). These must move together (wamn-0h0g.16.17) \
         — the port name is persisted in stored edges, so a one-sided rename \
         silently kills every error-path edge instead of failing"
    );
}
