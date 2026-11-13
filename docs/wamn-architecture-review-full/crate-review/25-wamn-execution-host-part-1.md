# 25 wamn-execution-host, part 1: the operation host

Nothing was built or run. Every statement is from reading the code. **Fact** = read in the code. **Inference** = my conclusion.

`crates/execution/host` is 18,677 lines (about 8,500 outside tests). It is read in three parts. This is part 1. It answers the two held questions: the scope record shared by plugins, and signed-file routes on the entry path.

## Files read

All under `crates/execution/host/src/`.

| File | Lines | Non-test lines |
| --- | --- | --- |
| `operation.rs` (+ `operation/tests.rs`, 136) | 965 | 828 |
| `operation/native_policy.rs` | 793 | 792 |
| `router_delivery.rs` | 563 | 384 |
| `operation/signed_result.rs` | 476 | 320 |
| `readiness.rs` | 335 | 292 |
| `route.rs` | 244 | 141 |
| `read_cache.rs` | 191 | 92 |
| `lib.rs` | 37 | 37 |
| Total | 3,604 | 2,886 |

Callers read: `services/host/src/host.rs` (host and bridge setup), `crates/execution/workflow/src/router_driver.rs` (closure, deadline adjustments, acquisition), `services/edge/src/delivery.rs`, the guest `http-route/src/entry.rs` and `guest.rs` (tags, deadline adjustments), `runtime/src/plugins/wamn_postgres/statements.rs` (statement types), `crates/catalog/model/src/component_facts.rs` (statement type), `wit/wamn-router-delivery-0.4/package.wit`.

## One purpose each

| File | Purpose | Verdict |
| --- | --- | --- |
| `operation.rs` | The host of one released application | Also copies statement types and trace header code |
| `native_policy.rs` | Grant and revoke the authority of each call | Writes one fact into eleven places |
| `router_delivery.rs` | Serve the delivery import for routes | Holds a workflow type and unused parts |
| `signed_result.rs` | Put signed URLs into a result | Half of it is the dead typed path |
| `readiness.rs` | Release readiness probe | Carries an unused invalidation |
| `route.rs` | One route call | One purpose |
| `read_cache.rs` | ETag values | A second copy of the guest's code |

## The held question: one scope record (tab 15 finding 2)

### 1. One call writes its facts into eleven registries

- **Fact.** `native_policy.rs::activate` makes these writes for one call scope: Postgres session claims, Postgres current run, Postgres invocation, Postgres statement members, Postgres statement operation, its activation, Postgres operation transaction, logging claim, HTTP invocation, blob store invocation, the policy's own scope set, and the trace.
- **Fact.** `revoke` undoes them with nine calls. Each write and each undo takes a lock in a different plugin.
- **Fact.** All of it comes from one value the caller already holds: `NativeFacts` (claims, causation, invocation, caller, transaction) plus the component's statements and the trace.
- **Fact.** The plugins that run as plain workloads (materializer, http-route guest) fill the same maps from workload config at bind. Logging and the event plugin never clear them (tabs 13, 18).
- **Option A (recommended).** One registry in `wamn-runtime`: scope → one shared, read-only record (claims, causation, wiring position, statements, transaction, trace, effect evidence). The policy inserts once and removes once. The four plugins read it. A workload bind inserts the same record under the component id and unbind removes it.
- **What goes with A.** The per-plugin maps and their bind and revoke functions in Postgres, logging, HTTP, blob store; `InvocationTraces` and its table scan; the policy's scope set. `activate` goes from eleven writes to one, `revoke` from nine to one.
- **Option B.** Keep the per-plugin maps; apply only the per-plugin rulings already made (tabs 11, 12, 13).
- This closes the held row of tab 15 finding 2, with tab 18 findings 10 B and 12, tab 19 finding 2 and tab 21 finding 7.

## The held question: signed-file routes on the entry path (tab 12 option C)

### 2. Signing happens after commit, in the host

- **Fact.** `finish_result` replaces file ids with signed URLs after the write log holds the raw result and after commit. It needs the caller, the file routes and a blob store scope.
- **Fact.** On the entry path the guest builds the HTTP response itself. The route projection sends signed-file routes to the delivery import for this reason (`sealed_operation_id` is none for them).
- **Inference.** Moving these routes to the entry path needs a new host function that the guest calls to sign its result. That is new WIT surface.
- **Option A (recommended).** Keep signed-file routes on the host path. Close the held question.
- **Option B.** Add a signing function to `wamn:operation` and move them. Not recommended: it adds surface to save no code.

## Findings

### 3. The released component list is passed in, then checked against itself

- **Fact.** `OperationClosure::Released` carries the component list. `released_application(components)` then checks it against the list the host loaded: by pointer first, else by validating every component and comparing the two lists.
- **Fact.** The route path passes the host's own list, so the pointer check passes. `router_driver.rs` passes the list of its wiring resolution, a different allocation.
- **Inference.** On the workflow path each node call runs the full validation. Not run.
- **Option.** The host owns the list. `OperationClosure::Released` carries nothing, `released_application` takes no list, and `validate_released_components` runs once at load. To confirm against `router_driver.rs` in the workflow tab.

### 4. "The release carries this component" is written a third time

- **Fact.** `new_native_policy::served_of` compares package, component and digest against the manifest.
- **Correction to tab 24 finding 3.** I wrote that I found no cloud caller of `validate_component_in_release`. There is one: `operation.rs::validate_released_components`.
- **Option.** `served_of` uses the one function of tab 24 finding 3 and takes the served component it returns.

### 5. Statement types are copied from the catalog into the runtime

- **Fact.** `lower_statement_value_type` maps ten cases of `ComponentSqlValueType` to the same ten of `StatementValueType`. Tab 12 finding 6 ruled one enum.
- **Fact.** `VerifiedStatement` (runtime) has the fields of `ComponentSqlStatement` (catalog) without `name` and `path`. `lower_statement_field` and `lower_statement_set` copy one into the other.
- **Option.** The runtime takes the catalog statement type. Delete the four lowering functions, `VerifiedStatement` and `StatementField`. One test goes: `admitted_statement_types_lower_exhaustively_to_the_runtime_vocabulary`.

### 6. The same route facts are looked up by scanning the manifest on each request

- **Fact.** Per request, this crate scans `manifest.routes` for: the read route (`router_delivery.rs`), a delete route (`admit_and_open_transaction`), the route of the call (`call_intents`), and `signs_file_results`.
- **Fact.** `call_intents` copies four strings and the canonical form of the route for every call.
- **Fact.** Tab 21 finding 2 builds the route projection once in the engine.
- **Option.** That one projection also holds, per operation: type, reads, revision, signs files, idempotency field, canonical form. This crate reads it. `CallIntents` borrows from it.

### 7. `signed_result.rs`: half the file is the typed path

- **Fact.** `typed_paths`, `typed_member`, `typed_field`, `typed_at_mut` and the typed branch of `sign_result_inner` handle `NativeOutcome::Typed`, which never exists (tab 22 finding 5).
- **Option.** Delete them. About 150 of 320 lines.

### 8. `router_delivery.rs`: a workflow type and unused parts

- **Fact.** `DeadlineAdjustment` is defined here. The route path never fills it: the parameter is `_deadline_adjustments`. Only `router_driver.rs` in the workflow crate uses the type.
- **Fact.** The WIT field `deadline-adjustments` of the delivery report is therefore always empty from this host and from the edge. The guest writes a header from it.
- **Fact.** `RouterDeliveryBridge::jetstream()` and `release()` have no caller. The `jetstream` field serves only `bind_derived_scope`, which tab 18 finding 1 removes.
- **Fact.** `delivery_attributes` ignores its `_target` argument. `run_route` authorizes a list read twice.
- **Option.** Move `DeadlineAdjustment` to the workflow crate. Delete the WIT field, the conversion and the guest header. Delete the two getters, the `jetstream` field and the unused argument. Authorize once.

### 9. `readiness.rs`: an invalidation that nothing calls

- **Fact.** `RouterReadinessProbe::invalidate` has no caller. The `generation` counter exists only to protect against it.
- **Fact.** `synchronous_wirings` is zero at every write.
- **Fact.** `prepare_synchronous_release` tests `components.is_none()` twice where it is always true.
- **Fact.** With a composed entry, readiness checks the bindings and does not load the application. Without one, it loads and initializes every component.
- **Option.** Delete `invalidate`, `generation`, `RELEASE_READINESS_INVALIDATED`, `synchronous_wirings`. Write the two cases (entry or no entry) as a plain `match`.
- **Open, for the owner.** Whether a release with a composed entry should also load at readiness. The comment on `prepare_released` says the first request should find the application resident.

### 10. ETag code exists twice

- **Fact.** `read_cache.rs` builds the get, list and changes tags, the match test and the feed test. The guest `http-route/src/entry.rs` builds the same three tags and the same tests.
- **Fact.** Both crates already depend on `wamn-execution-contract`.
- **Option.** The tag functions move to `wamn-execution-contract`. Both call them. This is the ETag item carried from tab 12.

### 11. Smaller items

- **Fact.** `OperationHost.files` and `.integrations` are set after construction, with a guard "must precede application loading".
- **Fact.** `operation.rs` has its own trace header extractor (`TraceHeaders`); the engine has another in `invocation_trace.rs`.
- **Option.** Pass files and integrations at construction if `host.rs` allows the order (checked in the host tab). One extractor, in the engine.

## Carried, answered

- **Plugins bound twice (tab 23).** `native_policy.rs::bind_component` is the bind that stays on the final linker. It adds linker entries and inserts into `bindings` under the item id. A repeat insert overwrites the same entry. No Postgres scope is registered at bind on this path.

## Fine as is

- `activate` reads admission twice around the awaited claim bind, and holds the read lock while it installs. The reason is written down.
- `NativeAuthorityGuard`: one drop revokes on return, trap, failure and cancel.
- `route.rs`: resolve, authorize, one call.

## Tests

**Delete**

- `operation/tests.rs::admitted_statement_types_lower_exhaustively_to_the_runtime_vocabulary` (finding 5)
- `signed_result.rs::typed_failure_drops_the_entire_owned_result` (finding 7)
- `readiness.rs::invalidation_fences_an_older_in_flight_success` (finding 9)

**Change**

- `signed_result.rs::json_and_typed_results_sign_each_result_class`: keeps the JSON half (finding 7)
- `readiness.rs::failure_stays_not_ready_and_success_advances_the_same_generation`: no generation, no `synchronous_wirings` (finding 9)
- `router_delivery.rs::a_delivery_is_labelled_by_its_source_type_and_id`: one argument less (finding 8)
- `operation/native_policy/tests.rs` and its three sub-files: every assertion on a per-plugin map reads the one registry (finding 1 option A). I have not read these bodies; the count comes when finding 1 is applied.

**Move**

- `read_cache.rs`, 5 tests: to `wamn-execution-contract` with the tag functions (finding 10). The guest's tests of the same tags are then deleted there, one test per rule.

## Not read

- `operation/native_policy/tests.rs` and `tests/authenticated.rs`, `entry.rs`, `warm.rs`, `trace.rs` (3,898 lines): not read.
- Other test bodies: names only.
- `crates/execution/workflow/src/router_driver.rs`: the lines that call into this crate only.

## Rulings

None yet.

**Ruled 2026-11-11**

- Finding 1: **option A.** One scope registry in `wamn-runtime`. This closes the held row of tab 15 finding 2.
- Findings 3 to 11: accepted, including the cleanup in finding 9.
- Finding 9, open question (load the application at readiness when the release has a composed entry): asked again, see the chat.
- Finding 2: owner asked for an explanation before ruling.

### Finding 2: explanation

**The two paths today (facts)**

- **Entry path.** The http-route guest and the application are composed into one component. The guest calls `wamn:operation` only for authority and the write log: `authorize` or `begin-operation`, then `commit(result)` or `rollback`. It calls the application export inside its own component and builds the HTTP response itself. The host sees the result only as the text passed to `commit`, which it stores unchanged.
- **Host path.** The guest calls `wamn:router-delivery/deliver`. The host runs the call (`invoke_operation`), commits, then runs `finish_result`, which puts signed URLs into the result. The signed result goes back to the guest in the delivery report.

**Why signing lives in the host (facts)**

- Signing needs the authenticated caller, the file storage of the connection (`FileRoutes::signed_url` with a blob store scope) and the release-pinned list of signed fields. All of these are host-side.
- A signed URL expires, so it must not be stored in the write log or behind an ETag. The host signs after commit, so a replay of a stored result is signed again with fresh URLs.

**What moving them to the entry path would need (inference)**

- A new host function on `wamn:operation`, for example `sign-result(result) -> result<string, refusal>`, called by the guest after `commit` and after a replayed `finished` result.
- Guest code in `http-route` to call it.
- The host signing code (`signed_result.rs`, `finish_result`) stays: the host path still serves workflow nodes and the edge.

**What it would remove (fact)**

- Three `signs_files` filters in the route projection (`sealed_operation_id`, `etag_inputs`, `cache_control`), about 10 lines.

**Options**

- **A (recommended).** Keep signed-file routes on the host path. No new WIT surface.
- **B.** Add `sign-result` and move them. Every authored route then uses the entry path, at the cost of one new host function and one more host call per signed request.

**Ruled 2026-11-11, finding 2: option B1.** Add `sign-result(result: string) -> result<string, refusal>` to `wamn:operation`. The guest calls it only on a route that signs: after `commit`, after a replayed `finished` result, and after a read finishes. Signed-file routes move to the entry path. The three `signs_files` checks in the route projection go; whether a route signs becomes a field the guest reads. Owner's words: "i think my preference is to use wit/interface".

### Finding 9: readiness, explained

**What readiness is today (facts)**

- `host.rs::preload_release` builds one `RouterReadinessProbe`, calls `refresh` once at start, and stops the host if it is not ready. Nothing else uses the probe: no repeated refresh, no snapshot reader, no invalidation.
- Without a composed entry: readiness pulls every component, loads the application and initializes each component. The comment cites a measured 35.8 s first request without this.
- With a composed entry: readiness only checks that the connection bindings are ready. It does not pull or load anything.

**Who uses the loaded application when there is an entry (facts)**

- Authored HTTP routes run in the http workload, which wash starts after readiness from the composed entry. That start pulls, compiles and initializes the entry itself.
- The host's own loaded application (`OperationHost`) serves the host path: workflow nodes and queued runs, and today signed-file routes (moving to the entry path by finding 2 B1). It loads on the first host-path call.

**What that first call costs (inference, not measured)**

- A second pull of the entry bytes through the host's artifact source.
- Probably no second compile: the native loader compiles the entry under the same digest as the http start, so the compile cache should hit.
- Loading the workload and binding the plugins.

**Options**

- **A (recommended).** Keep: no load at readiness when there is an entry. Workflow runs are queued, so a delay on the first run does not hold an HTTP caller. A host that never runs a workflow never pays the memory.
- **B.** Load at readiness when the release has an entry. The first workflow run is fast; every host pays startup time and memory for an application it may not use.

**One more fact, single option.** The probe is a state machine (mutexes, attempts, snapshot, status enum, public snapshot type and refusal constants) for one call at start. Option: replace `RouterReadinessProbe`, `RouterReadinessSnapshot`, `RouterReadinessStatus` and `ReadinessState` with one function `preload_release(operations) -> anyhow::Result<PreparedReleaseReadiness>`. The test `failure_stays_not_ready_and_success_advances_the_same_generation` goes with them.

**Ruled 2026-11-11, finding 9:** option A (no load at readiness when there is an entry), and the one-function replacement of the readiness probe. Tab 25 is fully ruled.
