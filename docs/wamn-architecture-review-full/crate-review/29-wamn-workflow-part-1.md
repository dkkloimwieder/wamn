# 29 wamn-workflow, part 1

Scope: lowering, the driver and the outcome mapping. Queue, schedule and contract are part 2. Nothing built or run.

## Files read

- `crates/execution/workflow/src/lib.rs` (in full)
- `src/wiring_lowering.rs` (non-test part, in full)
- `src/router_driver.rs` (non-test part, in full)
- `src/router_action.rs` (non-test part, in full)

## Callers read

- `src/queue.rs` (the claim loop, `drive_claim`, the `Emit` branch): the only caller of the driver and of `production_router_action`.
- `crates/catalog/model/src/wiring_compatibility.rs` (the two public checks), `workflow_gate.rs` (its call).
- `crates/platform/runtime/src/plugins/wamn_postgres/wiring_resolution.rs::resolved_wiring`, `ResolvedActiveWiring`.
- `wamn_postgres/claims.rs::{queued_run_caller, workflow_node_caller}`, `route_authentication.rs::queued_run_caller`.
- `crates/execution/host/src/operation.rs::call_intents`.
- `services/host/src/host.rs` (`RouterDriver::new`).

## One purpose per file

- `wiring_lowering.rs`: turn a resolved catalog wiring into the router graph.
- `router_driver.rs`: walk one workflow run, calling each node through `invoke_operation`.
- `router_action.rs`: map the walk result to the run-store completion.

## Findings

**1. The lowering checks the wiring a third time.** Fact: release publish runs `validate_resolved_wiring_compatibility` (`workflow_gate.rs`). Resolution (`resolved_wiring`) checks each node's component, interface and operation. `lower_active_wiring` then checks component, interface, operation, duplicate facts, parameters, output ports and input ports again. Release rows are immutable. In the product path, `lower_resolved_wiring` builds both scope arguments from the same values, so `ScopeMismatch` and `PackageVersionMismatch` cannot happen. `lower_active_wiring`, `GatedActiveWiring`, `ScopedWiringOperationFacts` and `WiringScope` are public only for `tests/wiring_lowering.rs`.

- A: the lowering only converts. Delete `WiringOperationFact`, `WiringParameterFact`, `ScopedWiringOperationFacts`, `GatedActiveWiring`, `WiringScope`, `project_component_operations`, `lower_active_wiring`, `validate_scope`, `resolve_operation`, `validate_parameters`, and every `WiringLoweringErrorType` except the router refusal. The "no to-port, exactly one input" rule uses the catalog's `resolve_input_port` (made public), so it exists once.

**2. Two sets of wiring types (carried from tab 28).** Fact: the router has `WiringNode`, `WiringEdge`, `Terminal`; the catalog has `WiringNode`, `WiringEdge`, `WiringTerminal`. `lower_terminal` maps one to one. The lowering always sets `connection: None`, and the driver never reads `NodeCall.connection`.

- A: the router compiles straight from the catalog `WiringDocument` plus each node's component digest. Delete the router's `WiringNode`, `WiringEdge`, `Terminal` (it uses `WiringTerminal`) and `wiring_lowering.rs`. Possible because of tab 28 finding 8 A (the router depends on `wamn-catalog`).
- B: keep the router types. After 1 A, the lowering is one small conversion function.
- Both: delete `WiringNode.connection` and `NodeCall.connection`.
- Recommend A.

**3. The workflow caller is read twice, by two code paths.** Fact: at claim, `queue.rs` calls `queued_run_caller` (its own SQL, `QUEUED_RUN_CALLER_SQL`, callable-HTTP login). Then for every node the driver calls `workflow_node_caller` (grants through the host's auth memory). The claim-time caller only reaches the invocation span. The `role: None` (service) branch of `queued_run_caller` has only test callers. This is the item carried from tab 17 ("run caller under the guest login").

- A: delete the claim-time read, `RouterDriverRequest.caller`, `WamnPostgres::queued_run_caller`, `route_authentication::queued_run_caller` and its SQL. The span takes the per-node caller. One grants read, as tab 17 finding 8 A2 ruled.

**4. Dead options in the driver.** Fact: `execute_resolved` has one caller, `execute_workflow`. It always passes `Some(PlatformComponent::Executor)` and `Some(&identity)`. So the `None => request.caller.clone()` branch is dead, and comments about "a released or candidate delivery" describe removed paths.

- A: plain arguments; merge `execute_resolved` into `execute_workflow`; delete the dead branch and comments.

**5. Trace fields that are always empty.** Fact: `queue.rs` is the only builder of `RouterDriverRequest` and sets `traceparent: None`, `tracestate: None`.

- A: delete both fields and `remote_request_context`.

**6. Two request structs for one request.** Fact: `WorkflowRunRequest` wraps `RouterDriverRequest`; both have one builder (`queue.rs`).

- A: one struct.

**7. Three holders for the same facts.** Fact: `CatalogFacts` copies `components` and `node_components` from `ResolvedActiveWiring`; its `manifest_digest` is never read. `ActiveWiring` copies `version` and `graph_hash` from it.

- A: the driver keeps `ResolvedActiveWiring` and the compiled `Wiring`. Delete `CatalogFacts`. This changes tab 28 finding 6 from "move `ActiveWiring` here" to "delete `ActiveWiring`".

**8. Per-hop work on fixed data.** Fact: each node call looks up its component fact twice (the loop, then `invoke_node`) and runs `validate_component_in_release` again. The components are fixed for the whole run.

- A: one lookup per node call; check every node component against the release once, after resolution.

**9. Deadline adjustments travel by a side path.** Fact: the loop collects them outside, the returned `RouterDelivery` carries an empty list that is patched afterwards, and on error they ride as the error context `DeadlineAdjustments`, which `queue.rs` downcasts.

- A: `execute_workflow` returns the adjustments next to its result. Delete `DeadlineAdjustments` and the downcast.

**10. A bool written as an enum.** Fact: `production_router_action` turns `caller_attached` into `RouterResultMode` and calls `production_router_action_with_mode`, its only caller.

- A: one function. Delete the wrapper and the enum.

**11. Failure codes in two places.** Fact: the router writes codes for 5 failure types (`hop-limit`, `unreleased-caller`, …). `router_failure_code` lists the codes of all 8 again.

- A: one `FailureKind::code()` in the router, used by the router and by `router_action.rs`.

**12. The emit node id is dropped, then read back.** Fact: `ProductionRouterAction::Emit` leaves out `node_id`; `queue.rs` reads it back from the verdict, with an 8-line comment explaining why.

- A: `Emit` carries `node_id`. Delete the read-back and the comment.

## Tests deleted, changed or moved

- Change: `tests/wiring_lowering.rs`: drift tests (scope, version, missing component, parameters, ports) are deleted; the release gate tests in `wiring_compatibility.rs` already cover each refusal (finding 1). The walk tests in it stay or move to the router (finding 2 A).
- Delete: the `wiring_lowering.rs` unit test moved from the engine (finding 1).
- Change: tests that call `queued_run_caller` as a helper: `route_authentication/tests/connection_live.rs`, `wamn_postgres/claims/tests.rs`, `control/environment/tests/integration_principal_live.rs`, `application_activation_live.rs`, `queue/automation_live.rs` (finding 3).
- Change: `router_action.rs` unit tests call the one function (finding 10).
- Change: `router_driver.rs` unit test `node_call()` drops removed fields (findings 2, 5).

## Not read

- `src/queue.rs` outside the parts above, `src/schedule.rs`, `src/contract.rs`, `src/contract/postgres.rs`, `src/queue/automation_live*` (part 2).
- Test bodies (test lists on hold).

## Rulings

- Finding 2: A (router compiles from the catalog `WiringDocument`; router wiring types and `wiring_lowering.rs` deleted).
- All other findings accepted (1, 3–12: A). Finding 3 closes the tab 17 carried item. Finding 7 changes tab 28 finding 6 to "delete `ActiveWiring`".
