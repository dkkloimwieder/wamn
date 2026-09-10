# WAMN native alignment after wasmCloud 2.9

Execution approved on 2026-09-10. Beads owns implementation status.

Scope: targeted use of native wasmCloud capabilities after the zero-fork 2.9 cutover. Its charter remains in force.

Evidence baseline: original review at WAMN `41ba0334510c4e8bc8420d4c0d3cff15acd34a7b`, with an amendment review at `c32aeed9569a28b60fb42fe5ae51b8f220be8334`. The filing review uses `c0ac2cf67ff689ea4b06d417ba6442799bcc6377`. Its reviewed runtime, service, driver, execution-model and ledger paths remain unchanged from `c32aeed9`. Upstream remains unmodified v2.9.0 `68ebece9c537f8bb4b5c9999f274ec68d60f35a9`. Source anchors distinguish historical evidence from this review. No builds, tests or benchmarks ran for this revision.

## 1. Direction and boundaries

**Keep WAMN's application, release and authorization contracts. Prefer native wasmCloud for the runtime and broker mechanics behind them.** A necessary WAMN policy boundary does not make its current implementation permanent.

Use unmodified upstream source and public embedding APIs. Do not carry a mirror fork, vendor modified runtime code, copy private ingress/dispatch internals, or substitute `wash host` for WAMN's binary. Greenfield status permits changing component interfaces and deleting predecessors; it does not justify weakening authority or adding speculative infrastructure.

Preserve invocation isolation, the closed capability registry, verified application SQL, host-owned credentials, nested-operation authorization and exact release/candidate bindings. B first adopts native dispatch with fresh stores. B2 permits warm reuse after B and its correctness proofs. Package labels alone do not establish eligibility. Retain operation-specific retry behavior: timeout or cancellation does not prove that an effect did not occur. This work adds no production capture, durable-execution machinery, database structures, new test engine or application-language program.

Each substitution must identify the code it removes, the policy it retains, and an executed acceptance test. A failed probe produces a specific limitation and a disposition—not a permanent second implementation. Existing application and testing delivery must not wait for every item below.

## 2. Already adopted: retain and test, do not rebuild

The reviewed tree uses direct upstream 2.9.0 with one Wasmtime 47.0.4 family. Its cutover includes real Receiving, WMS and operator-recovery evidence, but does not claim complete release readiness. [S1, S2]

| Native mechanism | WAMN position at the review baseline |
|---|---|
| **2.8 engine sizing and epoch interruption** | Native engine/ticker and pooling allocator; WAMN configures heap/instance capacity and deadlines for manual stores. |
| **Native socket policy and OCI trust** | Public policy configuration with WAMN's enforced destination restrictions and configured trust roots. |
| **2.9 guest-memory accounting** | Native shared budget and limiter include WAMN-created application stores. Production remains **Count**, not aggregate refusal. |
| **2.9 lifecycle** | Native probes, liveness, ingress connection limits, bounded startup retry, drain signaling and telemetry flushing are wired into WAMN's services. |
| **2.9 startup and metrics** | Native concurrent-start control and duration metering are selected for the native host path. |
| **Zero-patch routing** | The expected-host router adapter supplies 503 during an expected host's unbound interval. Accepted missing-handle/alias limitations remain; exact former patch parity is not required. |

These are path-specific integrations. Native startup limits do not automatically bound WAMN's separate component compilation pipeline; native metering does not automatically measure its direct `TypedFunc` calls. Allocator reuse, compiled-artifact reuse, HTTP connection reuse and live guest-state reuse are different controls. [S1, S3, S4]

## 3. Recommended changes

### A. Complete the small embedding omission

**Observation:** upstream PR **#5529, “fix ingress descriptor exhaustion,”** explicitly requires embedders to call `raise_descriptor_limit()` themselves. `wash host` does so before deriving connection ceilings; the original WAMN review found no equivalent call. Recheck before editing. No current WAMN descriptor-exhaustion failure was demonstrated. [S3, S5, S18]

**Change:** use that public helper at the WAMN process-start boundary before constructing resources that derive descriptor-based limits. Record the effective limit. Do not change explicit connection ceilings or introduce a separate limit calculator. Cover both host and executor startup. Keep the process-global mutation outside the shared engine builder.

**Acceptance:** a subprocess starts with a deliberately low soft limit and known hard limit; the native helper's effective result is observed and derived ceilings use it. Cover inability to raise the limit according to the helper's supported behavior. Do not alter the test runner's own limit or require elevated privileges.

A's [descriptor proof](../perf/2026.09/native-a-descriptor/report.md) records the service startup calls and isolated subprocess results under `wamn-0ct2.1`.

### B. Replace manual guest execution, including its duplicate caches

**Observation:** 2.9 exposes `DispatchTarget` / `GuestCall` and native digest-based component loading/caching. WAMN still owns `RouterDriver`'s compiled cache, `PreparedCache`, linker/pre-instantiation work and `NodeInstance`, including nested calls. Dispatch alone does not move loading into the native cache. [S4, S6, S7]

**Target:** WAMN resolves and authorizes the operation; native loading and dispatch own compilation reuse, instance lifecycle, deadline/abandonment handling and invocation metering. WAMN retains verified statement selection, capability authority, application outcomes and wiring traversal.

First prove one actual application node and a nested registered-operation call with **fresh stores**. Map the admitted release into a native `ResolvedWorkload` using existing deployment facts. Do not deploy every wiring node separately or introduce a competing workload model.

**Deletion contract for B's completed substitution:**

| WAMN-owned mechanism to retire | Replacement / retained responsibility |
|---|---|
| In-process digest → `Component` cache and its duplicate compilation path | Native digest-based loading/cache. Retain trusted artifact/digest verification and the native persistent Wasmtime disk cache; they are not duplicate guest lifecycles. |
| Router-owned linker cloning and `PreparedComponent.pre` / `PreparedCache` / `InstancePre` orchestration | Native workload resolution, plugin linking and pre-instantiation. Retain WAMN's admitted dependencies and immutable statement/authority facts, not a second pre-instantiation cache. |
| Manual `NodeInstance` store construction, invocation and teardown, including nested-call copies | Native dispatch. WAMN's per-invocation policy binding and revocation remain explicit around the call. |

**Acceptance:** exact admitted component/operation; no invocation authority during start functions; nested caller and credential-kind preservation; permission/fresh-only refusals; exact verified statements; released and frozen-candidate binding checks; cleanup on success, refusal, trap, deadline and cancellation; memory refunds; no child extension of an enclosing deadline; bounded native metric labels; and truthful committed/uncertain outcomes.

Prove compilation reuse through the native loader, including a shared digest under different release/operation facts: cached code must not substitute for checking those facts. Candidate bytes cannot enter a trusted digest cache merely by asserting that digest. Compare cold and steady runs with the manual baseline.

A probe passing is not B complete. Each converted path loses its predecessor in the same landing. Completion requires released, nested and candidate invocation paths accounted for and the duplicate mechanisms above removed. Any blocked path retains one named owner and exit condition; do not call the whole replacement complete or retain two selectable lifecycles for the same path.

Owner ruling, 2026-09-10: preserve one enclosing deadline across guest initialization, execution and nested calls. A child cannot extend that deadline. Native dispatch instantiates before its response timeout starts, so that response timer alone is insufficient. Prove bounded execution of a nonterminating start function. If public APIs cannot preserve this guarantee, record the exact obstacle and stop. Do not use private access. [S4, S6, S20]

**Stop condition:** a required context, candidate or loading boundary cannot be represented through public APIs. Record the exact obstacle before expanding the adapter. Do not copy internals or preserve duplicate machinery merely to report native adoption.

B's [public dispatch checkpoint](../perf/2026.09/native-b-dispatch/report.md) tests the deadline and exact component selection boundaries.
At the pinned 2.9 source, native resolution rejects an imported interface with multiple component exporters before considering an installed host policy function.
WAMN's manifest can select one such component by its exact digest.
The checkpoint records that loading obstacle under `wamn-0ct2.2` before any production adapter expansion.
The production substitution and its deletion contract remain open.

#### B2. Warm reuse after B with its policy amendment

The owner ruling of 2026-09-10 replaces both the state/affinity trigger and the benchmark prerequisite. B2 depends on B and the correctness conditions below. Per-workload pools require proof that each workload serves one tenant scope. That mapping alone does not isolate callers, operations or invocation resources within the tenant. WAMN currently stores request state in store data. Move that state out of the warmed store's lifetime before enabling reuse. [S15, S16]

Package ownership and database-backed business state do not establish that arbitrary guest code is stateless. Native warm stores retain globals, resources, frozen configuration and unawaited tasks. Eligibility must cover the **entire linked store closure**: upstream disables pooling if a linked component has not opted in. [S16]

After B and the correctness proofs, adopt bounded native `poolSize` and `reclaimWindowSeconds` for eligible components. Keep `maxConcurrency = 1` per instance. Prove that pool saturation falls back to ephemeral stores. Pool capacity does not bound total admission. Keep request and memory bounds independent of pool capacity. [S16]

Run the existing throughput bench after the B2 landing. Compare fresh and warm execution against the identified unmodified 2.9 baseline. Record the artifacts, commands, counts and machine load. A noisy or neutral result is a recorded measurement, not a blocker. No benchmark result or measured benefit is a prerequisite for B2.

**Eligibility proof:** observe actual instance reuse, not just configured pooling; alternate differently authorized callers in one tenant, including PAT/session and fresh-only cases; preserve tenant/environment/release/binding separation; rebind and revoke authority per invocation; demonstrate that prior transactions, invocation resources, caller-dependent results and unfinished work cannot leak into the next call. Immutable, authority-independent caches may survive. Check trap/timeout retirement, release/credential changes and idle reclamation. No new statelessness analyzer is required; an unproved component remains ephemeral.

Palette or other components without this evidence keep `poolSize` unset/zero. This preserves per-call state for components; it does **not** implement windowed state or affinity, and long-lived services have a different lifetime contract. Land the production mechanism, resource/admission rules, execution-model amendment and ledger row 4 in the same commit. The ledger amendment is a B2 deliverable, with no separate issue. Keep the current global refusal until that complete landing. Do not silently turn it into an allow-all. [S15, S16]

### C. Re-evaluate `wamn:jetstream` against native NATS

**Observation:** 2.9's `wasmcloud:nats` provides broker-aware publish, pull and acknowledgement functionality with host-owned credentials and scoped grants. The historical justification that upstream lacks these mechanics is no longer sufficient. WAMN nevertheless has additional release-registration, causation and namespace rules. Native plugin internals are not all public reuse APIs. [S7, S8]

**Target:** native NATS handles broker mechanics where its supported interfaces fit; WAMN retains the application delivery policy. Adapt WIT and component bindings when necessary rather than copy private Rust modules or create another generic messaging abstraction. Do not enable every upstream default provider or grant new application imports implicitly.

**First capability-configuration evaluation:** C maps the existing requirement → connection instance → binding model into native named bindings. Use 2.9's unified `plugins` block and `wasi:config` stable-merge fix as inputs, not a second configuration authority. Prove deterministic configuration/precedence, host-owned credential protection and exact binding selection; no credential is exposed through guest-readable config. `identity.get-binding-name` identifies the binding, not caller authority. No standalone framework or enabling all default providers. [S7]

Use the existing Receiving event path and private `quality.create_inspection` handler. Preserve registration-specific selection, durable-consumer behavior, required event metadata, bounded retries, completion-before-ack, correlated dead letters, and separation of event-plane traffic from control-plane doorbells. Causation remains provenance, not borrowed caller authority. Native `term` alone does not establish WAMN dead-letter publication.

**Acceptance:** deliver a committed receipt through the real broker/materializer path; prove a second handler delivery leaves one inspection without resetting its business state. A distinct valid receipt must still create its own inspection. Exercise interruption after commit but before acknowledgement, poison handling followed by valid-event progress, denied foreign registration/subject access, and bounded delivery pressure. A duplicate publication suppressed by the broker is not a duplicate-handler proof.

The existing WAMN consumer owner creates the durable consumer and refuses configuration drift. Native `open_pull_consumer` attaches to an existing consumer. Retain WAMN registration and exact configuration enforcement around that attachment. [S8, S21]

An idempotent inspection result is not a platform-wide exactly-once claim. Keep stream/consumer administration in its existing owning layer; do not replace run/lease semantics with broker semantics. The 2.8 generic async messaging interface is not an interchangeable substitute: its documented retry disposition did not itself provide broker redelivery. [S9]

**Stop condition:** identify any release, metadata, acknowledgement or authority requirement the public interface cannot preserve. Keep the necessary WAMN portion and document why; do not widen grants or rewrite the event architecture to claim adoption.

Owner ruling, 2026-09-10: C can proceed ahead of B because their ordering protects shared-file edits, not a technical dependency.
The [C source checkpoint](../perf/2026.09/native-c-nats/report.md) reaches the public message-handle stop condition under `wamn-0ct2.4`.
The owner retains registration checks and dead-letter construction in the Rust host.
Deferred decision `wamn-0ct2.6` owns a trusted adapter component after the upstream binding surface stabilizes and the message handle becomes public.
The capability registry can make that adapter the sole native NATS importer, but policy placement and bypass resistance require a separate decision and proof.
B2 still requires B.

### D. Finish HTTP transport reuse without weakening destination authority

**Observation:** the retained native-pooling probe ran on 2.8. It demonstrated reuse, but also sent to an address different from WAMN's approved peer. Tagged 2.9 still constructs its connector privately. Production `ConnectionHttp` continues to build a client per call and collect the response body in full. The historical probe is not an executed 2.9 adoption proof. [S10, S11]

**Decision path:** recheck the public 2.9 integration for the specific blocker. Prefer native transport only if the approved endpoint is enforced **before dispatch**. Observing a mismatch afterward is insufficient. Standard WASI HTTP remains a separate interface option, not a prerequisite or authority bypass.

If the public native path remains unsuitable, use bounded reuse around WAMN's existing pinned-destination transport. This fallback belongs to the existing HTTP work, not a second runtime. Reusable clients must live above the invocation-local plugin/store lifetime; placing a cache inside an object rebuilt per request does not provide cross-request reuse.

The reuse boundary must preserve tenant/project/environment, binding and credential generation, approved destination and TLS policy. Invocation IDs are not pool keys. Keep authority checks on every request, including hits. Retired clients drain without lending their authority to a new generation, and overlapping generations must not each receive an independent copy of the same logical quota.

**Acceptance:** warm reuse plus denied-access controls; generation/policy changes; released and candidate bindings; nested callers; actual connected peer; and all supported HTTP/TLS/protocol paths. Bound retained clients/connections, concurrent requests, headers, body bytes and total duration. A connection quota is not a request quota. Observe mutation dispatch counts and response-loss handling; do not add hidden mutation retries or claim that a timeout proves non-dispatch.

Select either native adoption or the scoped fallback, with measured evidence, under existing `wamn-ctc8.13` / `wamn-ctc8.16` ownership after checking current tracker status. Delete the replaced per-call construction path. [S10]

### E. Complete the separate P3 HTTP cutover

**Observation:** the reviewed HTTP shell still exports P2 `incoming-handler` and calls synchronous WAMN routing/auth/delivery imports. Runtime support for P3 does not convert those interfaces. [S12]

The P3 cutover owner is `wamn-0h0g.2.7.17`, under the cutover follow-ons. The successful probe remains `wamn-0h0g.17.26`. Owner ruling, 2026-09-10: proceed with P3 ahead of blocked B after B released the shared files. Keep the shared HTTP shell and dispatch edits serialized. Convert the actual HTTP shell, request/response handling, WIT bindings, manifests, publication tooling and owning tests. Preserve request limits, origin-form/Host authority handling, authentication, typed responses and fresh stores.

**Acceptance:** the same P3 artifact runs through in-process and deployed proofs, including valid requests, refusals, cancellation and bounded bodies. Recheck actual post-build/post-virtualization imports; move any necessary pin, digest and exact capability-registry rows together. Do not broaden admission with a WASI wildcard.

Remove P2-specific production wiring and tests when their subject is replaced; retain historical evidence. No permanent P2 compatibility track without a real consumer. Assess synchronous inner WAMN crossings separately: changing the outer shell does not establish end-to-end async behavior or a latency improvement. P3, native dispatch and HTTP transport reuse are separate decisions. [S1, S12]

### F. Retain one WAMN PostgreSQL implementation

Accepted trade: retain one WAMN PostgreSQL implementation because commands require exclusive held-session transactions.
A held session keeps one database connection throughout a transaction.
Owner decision, 2026-09-10, supersedes the bounded native probe under `wamn-0ct2.5`.
F has no native substitution, parallel read backend, or new database machinery.

The existing `wamn:postgres` boundary retains statement-by-reference admission, SQLx corpus identity, claim/causation binding, database identity, operation authorization, and transaction ownership.
Guests name admitted statement digests, and the host resolves the exact SQL and parameter contract.
The transaction retains its statement set and one connection through execution, commit, or rollback.
An armed cancellation guard destroys an unfinished connection instead of returning it to the pool.
See [statement resolution](https://github.com/dkkloimwieder/wamn/blob/1d38b6da38a460753d89f877ec0a0c68345a7d60/crates/platform/runtime/src/plugins/wamn_postgres/statements.rs#L229), [transaction creation](https://github.com/dkkloimwieder/wamn/blob/1d38b6da38a460753d89f877ec0a0c68345a7d60/crates/platform/runtime/src/plugins/wamn_postgres/resources.rs#L439), [transaction execution](https://github.com/dkkloimwieder/wamn/blob/1d38b6da38a460753d89f877ec0a0c68345a7d60/crates/platform/runtime/src/plugins/wamn_postgres/resources.rs#L1033), and the [cancellation guard](https://github.com/dkkloimwieder/wamn/blob/1d38b6da38a460753d89f877ec0a0c68345a7d60/crates/platform/runtime/src/plugins/wamn_postgres/resources.rs#L43).

Source review at upstream `68ebece9` found no public held-session entrypoint in the named or async PostgreSQL modules. Public query and execution calls acquire connections separately. The async path streams results without exposing a public session across command statements. F records this source-level stop condition. No live probe or adapter ran. [S17]

Completion test: the retained implementation keeps its admitted statements, exclusive transaction session, credential ownership, and cancellation guarantees.
F changes documentation only and does not claim a new runtime proof or native transaction success.
The earlier probe acceptance and automatic resume condition are retired by the owner decision.
The [historical review §6](native-alignment-external-review-2026-09-10.md#6-f-command-transactions-need-one-held-database-session) retains the original API finding.

## 4. Retained boundaries and demand-gated work

| Area | Disposition |
|---|---|
| **Warm guest reuse and idle reclamation** | B2 requires B and the correctness proofs. Its policy and mechanism land together. The throughput comparison follows that landing. Other components stay ephemeral. |
| **Aggregate memory Enforce** | Retain production Count and existing Enforce proofs. A later rollout needs measured demand, host-memory headroom and explicit approval—not another accounting implementation. |
| **Unified plugin configuration and named bindings** | Evaluate first in C, using the native `plugins` policy and stable `wasi:config` merge. Derive from WAMN facts; keep one authority. No standalone configuration framework. |
| **PostgreSQL capability** | F retains one WAMN implementation for exclusive command transactions. No native substitution, parallel read backend, or new database machinery. Existing statement and cancellation guarantees remain in force. |
| **Blobstore** | Retain WAMN's credential, bucket/prefix and explicit-commit policy. Revisit shared native mechanics only against a specific demonstrated replacement. |
| **503 adapter and Events RBAC** | Keep the landed adapter and the scoped overlay while their documented residual/permission gaps remain. Do not reopen the fork or add routing infrastructure merely for exact status parity. |

B2 explicitly proposes changing the existing fresh-store adoption rule; the other dispositions retain policy while testing simpler implementations. None is a claim of completed adoption. [S1, S7, S13, S15]

## 5. Delivery, proof and handoff

**This is a prioritized backlog, not concurrent replacement programs.** The original amendment review uses `c32aeed9`, including the effects integration receipt. It does not certify machine or lane availability. Before implementation, recheck source, ledger, Beads and shared-file ownership. The completed cutover is not reopened; its unproved legs are not declared green. [S19]

| Order | Placement and exit |
|---|---|
| **First** | Complete the descriptor hook and update stale ledger comparisons. Continue existing operational proofs under their owners. |
| **Next native-alignment work** | B first proves native fresh-store dispatch and its explicit deletion targets; C evaluates native NATS and named configuration as a separately reviewable substitution. Serialize shared runtime/host edits. |
| **Existing follow-ons** | Continue HTTP pooling and P3 under their current owners. Coordinate overlapping files rather than create duplicate epics. |
| **Retained PostgreSQL implementation** | F records the approved retention decision. The native probe is retired. F introduces no dependency for B/C or application delivery. |
| **After B and correctness proofs** | B2 lands warm reuse, resource/admission rules, execution-model and row 4 together. The throughput comparison follows. Stateful affinity remains separate. |

The tracker owns status and dependencies under epic `wamn-0ct2`. A is `wamn-0ct2.1`, B is `.2`, B2 is `.3`, C is `.4`, and F is `.5`. D references `wamn-ctc8.13` and `wamn-ctc8.16`. The closed P3 probe is `wamn-0h0g.17.26`. The P3 implementation owner is `wamn-0h0g.2.7.17`. The owner removed its ordering dependency on blocked B on 2026-09-10.

Use separate worktrees and serialize A, B and C landings. The shared driver is `crates/execution/host/src/router_driver.rs`. Coordinate its edits with `services/host`, `services/executor`, `crates/platform/runtime`, affected manifests and shared proofs. Identity and TUI owners keep their current files and stay outside each active substitution boundary. Each owner rebases after the boundary lands. F requires only its scoped documentation landing.

Keep the existing proof gaps visible: `wamn-10yt.74` owns the missing automation producer; `.75` owns dependent active-work shutdown evidence; `.76` owns the native startup-refusal/recovery gap. Do not fabricate an admission path to satisfy those tests. A path-dependent replacement needs its relevant proof, but unrelated application work does not wait for every gap. [S2]

Reuse existing application journeys, authority tests and throughput benchmarks. Follow `docs/operations/build-and-test.md`'s named mutation, negative-control, distinguishing-step and live-test arming rules rather than create a second evidence policy. [S14] Test meaningful allowed/refused pairs through production code; a compiling adapter or empty test selection is not acceptance.

Compare each performance-sensitive substitution with an identified unmodified **2.9 baseline** using comparable artifacts, features, profiles, resource limits and repeated measurements. Preserve 2.8 as historical cutover evidence. Record latency distributions, throughput, errors, CPU/memory and relevant connection/queue counts. Do not relax gate thresholds to obtain a pass or attribute noisy changes to P3/native code without evidence. For B2, run this comparison after the correctness landing. A noisy or neutral measurement does not block that landing.

Each landed change includes the exact source/artifact identities, executed commands and counts, failing or unavailable legs, code removed, remaining deviations, and the ledger/recipe update. Hold shared-file ownership during overlapping changes and exclusive machine use during measurements. For B2, record the later comparison under the same issue. Changes must reduce total maintenance.

The owner approved five children: A, B, B2, C and F. B2 includes the ledger amendment and requires B plus correctness proofs, with no benchmark prerequisite. Production warm reuse waits for those proofs and the single policy/mechanism commit. D and E retain separate ownership. No additional production mechanism opens because upstream exposes it.

## Source anchors

Original review/evidence links retain `41ba0334`; S4 and S15/S19 use the amendment-check tip `c32aeed9`. Runtime source links pin upstream `68ebece9`; the PR link identifies its merged change. Recommendations and acceptance conditions are proposed, not claims of implementation.

- **S1 — Adopted boundaries:** [native-alignment ledger](https://github.com/dkkloimwieder/wamn/blob/41ba0334510c4e8bc8420d4c0d3cff15acd34a7b/docs/architecture/native-alignment-ledger.md).
- **S2 — Executed evidence and remaining gaps:** [2.9 cutover report](https://github.com/dkkloimwieder/wamn/blob/41ba0334510c4e8bc8420d4c0d3cff15acd34a7b/docs/perf/2026.09/wasmcloud-2-9-cutover/report.md).
- **S3 — Actual embedding:** [WAMN host](https://github.com/dkkloimwieder/wamn/blob/41ba0334510c4e8bc8420d4c0d3cff15acd34a7b/services/host/src/host.rs) and [shared engine](https://github.com/dkkloimwieder/wamn/blob/41ba0334510c4e8bc8420d4c0d3cff15acd34a7b/crates/platform/runtime/src/engine.rs).
- **S4 — Manual application execution:** [router driver](https://github.com/dkkloimwieder/wamn/blob/c32aeed9569a28b60fb42fe5ae51b8f220be8334/crates/execution/host/src/router_driver.rs), especially `prepare_released_operation_components`, `instantiate_prepared`, `NestedOperationHost` and `NodeInstance::run`.
- **S5 — Descriptor setup:** [upstream host CLI](https://github.com/wasmCloud/wasmCloud/blob/68ebece9c537f8bb4b5c9999f274ec68d60f35a9/crates/wash/src/cli/host.rs) and [public quota helper](https://github.com/wasmCloud/wasmCloud/blob/68ebece9c537f8bb4b5c9999f274ec68d60f35a9/crates/wash-runtime/src/host/quota.rs).
- **S6 — Native invocation API:** [dispatch module](https://github.com/wasmCloud/wasmCloud/blob/68ebece9c537f8bb4b5c9999f274ec68d60f35a9/crates/wash-runtime/src/engine/dispatch.rs).
- **S7 — 2.9 capabilities:** [official release article source](https://github.com/wasmCloud/wasmcloud.com/blob/9d4756a287c734d43a0d4f5e01622ecdaa4d907e/blog/2026-09-08-wasmcloud-2.9.0/index.mdx) and [native NATS module](https://github.com/wasmCloud/wasmCloud/blob/68ebece9c537f8bb4b5c9999f274ec68d60f35a9/crates/wash-runtime/src/plugin/wasmcloud_nats/mod.rs).
- **S8 — WAMN delivery policy:** [JetStream plugin](https://github.com/dkkloimwieder/wamn/blob/41ba0334510c4e8bc8420d4c0d3cff15acd34a7b/crates/platform/runtime/src/plugins/wamn_jetstream.rs).
- **S9 — 2.8 capabilities and messaging limits:** [official release article source](https://github.com/wasmCloud/wasmcloud.com/blob/9d4756a287c734d43a0d4f5e01622ecdaa4d907e/blog/2026-08-26-wasmcloud-2.8.0/index.mdx).
- **S10 — HTTP probe and owner:** [native HTTP report](https://github.com/dkkloimwieder/wamn/blob/41ba0334510c4e8bc8420d4c0d3cff15acd34a7b/docs/perf/2026.09/ctc8-13-native-http/report.md).
- **S11 — HTTP implementations:** [WAMN connection HTTP](https://github.com/dkkloimwieder/wamn/blob/41ba0334510c4e8bc8420d4c0d3cff15acd34a7b/crates/platform/runtime/src/plugins/connection_http.rs) and [upstream pooled client](https://github.com/wasmCloud/wasmCloud/blob/68ebece9c537f8bb4b5c9999f274ec68d60f35a9/crates/wash-runtime/src/host/http_client.rs).
- **S12 — P3 scope:** [shipped HTTP guest](https://github.com/dkkloimwieder/wamn/blob/41ba0334510c4e8bc8420d4c0d3cff15acd34a7b/components/ingress/http-route/src/guest.rs) and [cutover charter §7](https://github.com/dkkloimwieder/wamn/blob/41ba0334510c4e8bc8420d4c0d3cff15acd34a7b/docs/wamn_wasmcloud_2_9_cutover.md#7-follow-ons-not-prerequisites).
- **S13 — Native PostgreSQL capability:** [upstream plugin](https://github.com/wasmCloud/wasmCloud/blob/68ebece9c537f8bb4b5c9999f274ec68d60f35a9/crates/wash-runtime/src/plugin/wasmcloud_postgres/mod.rs).
- **S14 — Evidence discipline:** [build and test guide](https://github.com/dkkloimwieder/wamn/blob/41ba0334510c4e8bc8420d4c0d3cff15acd34a7b/docs/operations/build-and-test.md), especially live-test arming and the named rules under “Traps.”
- **S15 — Current invocation-lifetime policy:** [execution model](https://github.com/dkkloimwieder/wamn/blob/c32aeed9569a28b60fb42fe5ae51b8f220be8334/docs/exe-model.md), “Runtime target,” including request state in store data.
- **S16 — Native warm semantics and linked-closure eligibility:** [instance pool](https://github.com/wasmCloud/wasmCloud/blob/68ebece9c537f8bb4b5c9999f274ec68d60f35a9/crates/wash-runtime/src/engine/instance_pool.rs), module contract, `poolable`, overflow behavior and `InstancePolicy`.
- **S17 — PostgreSQL public/private boundary:** [named PostgreSQL implementation](https://github.com/wasmCloud/wasmCloud/blob/68ebece9c537f8bb4b5c9999f274ec68d60f35a9/crates/wash-runtime/src/plugin/wasmcloud_postgres/multiplexed.rs), `PgId::client/query/execute/prepare/exec`; [async implementation](https://github.com/wasmCloud/wasmCloud/blob/68ebece9c537f8bb4b5c9999f274ec68d60f35a9/crates/wash-runtime/src/plugin/wasmcloud_postgres/async_p3.rs) shares the pools and adds streamed results, not a public command transaction in the inspected entrypoints.
- **S18 — Descriptor omission provenance:** [merged upstream PR #5529](https://github.com/wasmCloud/wasmCloud/pull/5529), including its explicit third-party embedder instruction.
- **S19 — Latest integration boundary:** [main integration commit](https://github.com/dkkloimwieder/wamn/commit/c32aeed9569a28b60fb42fe5ae51b8f220be8334). Its source/evidence publication does not establish present lane availability.

- S20: [native response deadline](https://github.com/wasmCloud/wasmCloud/blob/68ebece9c537f8bb4b5c9999f274ec68d60f35a9/crates/wash-runtime/src/engine/abandon.rs), `await_reply` starts after dispatch creates the instance.
- S21: [native consumer attachment](https://github.com/wasmCloud/wasmCloud/blob/68ebece9c537f8bb4b5c9999f274ec68d60f35a9/crates/wash-runtime/src/plugin/wasmcloud_nats/interfaces/jetstream/mod.rs), `open_pull_consumer`.
