# 28 wamn-router

Scope: `crates/execution/workflow/router` (the graph walk). Nothing built or run.

## Files read

- `src/lib.rs`, `src/resolution.rs`, `src/outcome.rs`, `src/terminal.rs`, `src/retry.rs`, `src/wiring.rs`, `src/walk.rs` (in full)
- `src/invariants.rs` (head and public items)

## Callers read

- `crates/execution/workflow/src/router_driver.rs` (the walk loop, `lower_node_outcome`): the only product caller.
- `crates/execution/workflow/src/router_action.rs`, `wiring_lowering.rs` (imports and uses only).
- Every use of each public item, repo-wide.
- Tab 01 rulings (ports, `node_contract`).

## One purpose per file

- `walk.rs`: the walk state machine (`start`, `next`, `apply`).
- `wiring.rs`: the router graph type and its index.
- `outcome.rs`: what a node returned.
- `terminal.rs`: respond / emit / discard.
- `retry.rs`: retry policy and backoff.
- `resolution.rs`: `ActiveWiring` (held, never read, by the router).
- `invariants.rs`: walk checks for the test simulator.
- `lib.rs`: exports, `NodeInvoker`, `route`, `Outcome`.

## Findings

**1. `route` and `NodeInvoker` are test-only.** Fact: no product code calls `route` or implements `NodeInvoker` (`effect_span.rs` says so too). `router_driver.rs` writes its own loop, and builds `Outcome` from the walk by hand twice.

- A: delete `route` and `NodeInvoker`. Add one `Walk::outcome()` that builds `Outcome`; the driver calls it in both places.

**2. `invariants.rs` (322 lines) is test code in the product.** Fact: no product caller. Only `tests/walk.rs` (the simulator) calls `check`. Its own comment calls product use a deferred plan.

- A: move it into `tests/support/` with the simulator.
- B: delete it and the simulator checks that call it.
- Recommend A.

**3. The throttle key and credential are never used.** Fact: `build_call` always sets `NodeCall.credential` to `None`. `lower_node_outcome` always sets `RateLimitDetail.target_host` to `None`. The driver ignores `Step::Wait.throttle` (`Step::Wait { until_ms, .. }`). No shared rate-limit gate exists.

- A: delete `ThrottleKey`, `Active.throttle`, `Step::Wait.throttle`, `NodeCall.credential`, `RateLimitDetail.target_host`. Also delete `Step::Wait.node` and `Step::Wait.attempt` (only tests read them). `rate_limited` keeps `retry_after_ms`.

**4. Unused public items.** Fact, product callers: none for `Walk::step_seq` (and its field), `Walk::delivery_id` (and `Delivery.id`, which only feeds it), `Wiring::set_hop_limit`, `Wiring::hop_limit`, `NodeOutcome::ok_on`, `ErrorDetail::msg`.

- A: delete them. The hop limit becomes the constant `DEFAULT_HOP_LIMIT`. Tests that lower the limit use the constant (a 10,000-hop loop runs in memory).

**5. `apply` refuses atomically for a driver that re-asks; no driver re-asks.** Fact: both `route` and `router_driver.rs` call `fail_on_node_data` at once after every refusal. The split between "data" and "driver defect" refusals exists only for that public step.

- A: `apply` folds `RespondWithoutCaller` and `SecondVerdict` into the walk itself. Delete public `fail_on_node_data` and `node_data_failure`. `ApplyError` keeps only the three driver defects.

**6. `ActiveWiring<T>` lives in the router, which never reads it.** Fact: the only `T` is `CatalogFacts` in `router_driver.rs`. The router never reads `version`, `graph_hash` or `facts`.

- A: move it into `router_driver.rs`, not generic. Delete `resolution.rs`.

**7. Each hop copies node data.** Fact: `build_call` clones `component`, `operation`, `config`, `connection` and `payload` into every `NodeCall`. `enqueue_successors` clones `payload` once per edge. Project rule: a clone is never free.

- A: `NodeCall` borrows the `WiringNode` from the `Wiring` (it outlives the walk), and payloads are `Arc<Value>`.

**8. Port names defined three times (tab 01, open question: which crate).** Fact: `wamn_router::{MAIN_PORT, ERROR_PORT}`, `wamn_execution_contract::{MAIN_PORT, ERROR_PORT}`, a private copy in `apps/platform/execution/jsonata/src/lib.rs`. `crates/execution/workflow/tests/port_constant_agreement.rs` checks two agree. The index records "port names go to `wamn-catalog`". `wamn-workflow`, the only user of the router, already depends on `wamn-catalog`.

- A: the router depends on `wamn-catalog` and uses its constants. Delete the router copy and the agreement test. No new edge in the product build.
- B: the constants go to `wamn-event-wire`, which both already depend on.
- Recommend A (matches the index).

**9. History in doc comments.** Fact: comments describe the retired flow engine, plan ids (D8a, D8b, rev 4) and bead ids. Tests assert with notes like "engine asserted 6".

- A: delete the history; keep only what the code does now.

## Carried to tab 29 (wiring\_lowering)

- The router keeps its own `WiringNode`, `WiringEdge`, `Terminal`; the catalog has `WiringNode`, `WiringEdge`, `WiringTerminal`. `wiring_lowering.rs` maps one to the other, 1 to 1 for terminals. With finding 8 A the router depends on the catalog, so walking the catalog types directly becomes possible. Option after reading `wiring_lowering.rs`.

## Tests deleted, changed or moved

- Delete: `crates/execution/workflow/tests/port_constant_agreement.rs` (finding 8).
- Change: `tests/route.rs` (2 tests) drive `start`/`next`/`apply` directly (findings 1, 5).
- Change: `tests/walk.rs`: `rate_limited_honors_retry_after_and_emits_the_shared_throttle_key` keeps the retry-after assert only; `node_call_carries_component_config_and_deadline_without_wiring_credentials` drops the credential assert; `step_seq` asserts go; `set_hop_limit` users use the constant (findings 3, 4).
- Change: `tests/terminal.rs` refusal tests expect the folded failure (finding 5).
- Move: `invariants.rs` to `tests/support/` (finding 2 A).
- Keep: `retry.rs` unit tests.

## Not read

- `invariants.rs` bodies after the public items; `tests/*.rs` bodies (test lists on hold).

## Rulings

- Finding 8: A (router uses the `wamn-catalog` port constants; agreement test deleted).
- Findings 1, 3, 4, 5, 6, 7, 9: A.
- Finding 2: B. Delete `invariants.rs`, the `proptest!` block in `tests/walk.rs` and its helpers (`check_invariants`, the generators, `OutcomeSpec`), `tests/walk.proptest-regressions`, and the `proptest` dev-dependency. Reason: rules 3, 4 and 6 redo the walk's own logic; the hand-written tests stay.
