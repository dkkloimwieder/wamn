# Native alignment: obstacles and conditions for proceeding

Review snapshot: September 10, 2026. Documentation owner: `wamn-ngkk`. Program owner: `wamn-0ct2`.

WAMN already uses unmodified wasmCloud 2.9. Native alignment replaces additional WAMN runtime mechanisms while preserving application contracts.
Three substitutions reached specific public API limits. One dependent substitution still needs isolation proofs.
HTTP work identifies a feasible fallback, but its owner needs transport and resource-limit decisions.
This is a dated explanation for external review, not a new design authority or an implementation authorization.

The WAMN source snapshot is `a48f7af827bb835b619eb8aa24db745916d24824`.
The upstream runtime is `68ebece9c537f8bb4b5c9999f274ec68d60f35a9`, with Wasmtime 47.0.4.
Links below identify immutable source revisions. Historical experiments retain their own revisions and limitations.
The [approved alignment plan][plan], [execution model][model], and [deviation ledger][ledger] supply the governing contracts.

## 1. Status and scope

The project is greenfield, with no running clients. No maintenance window or client migration blocks this work.
The owner prioritizes correctness and prohibits further excessive benchmarking. No new build, live experiment, or benchmark ran for this brief.
An upstream feature does not automatically authorize a new production mechanism.

| Item and owner | State at this snapshot | Reason or result |
| --- | --- | --- |
| A, `wamn-0ct2.1` | Implemented | Both services use the public descriptor helper at startup. |
| B, `wamn-0ct2.2` | Blocked | Native loading rejects a component selection that WAMN can express exactly. |
| B2, `wamn-0ct2.3` | Blocked | B must exist first. Isolation and request-lifetime proofs remain required. |
| C, `wamn-0ct2.4` | Closed as a stopped substitution | Private native message handles prevent the retained Rust policy from forwarding checked operations. No native adoption occurred. |
| C adapter decision, `wamn-0ct2.6` | Deferred | The owner requires a stable binding surface and a public message handle before reconsideration. |
| D, `wamn-ctc8.16` | Blocked on owner decisions | Source feasibility work identifies a fallback. Transport and resource-limit decisions await the owner. |
| E/P3, `wamn-0h0g.2.7.17` | Implemented | The production HTTP shell uses P3 and passes the recorded deployed proofs. |
| F, `wamn-0ct2.5` | Blocked after source inspection | The public PostgreSQL API does not expose a connection held across WAMN statement calls. |

These stops have different causes. C and P3 originally followed B because their edits shared files.
The owner removed those ordering dependencies after B released the files. B2 retains a technical dependency on B.
F blocks neither application delivery nor B, C, or D.

## 2. What each substitution must preserve

A component is an executable WebAssembly artifact. A digest identifies its exact bytes. A store contains a guest's execution state.
Authority means the actions that a caller can perform. A binding associates a workload with an approved resource.
WAMN decides these facts before it delegates runtime or network mechanics.

The replacement must preserve these contracts:

1. Exact release and candidate selection. A matching interface name cannot substitute for the selected component digest.
2. Invocation authority. Tenant, environment, caller, credential kind, operation, and resource restrictions apply to each call.
3. Restricted startup. Guest initialization receives no invocation authority.
4. Bounded execution. One enclosing deadline covers initialization, execution, and nested calls. A child cannot extend it.
5. Restricted data access. Guests select admitted statement references. The host retains SQL selection, credentials, and transaction ownership.
6. Truthful outcomes. Cancellation or a lost response does not prove that an external mutation did not occur.
7. A smaller implementation. Each converted production path loses its predecessor in the same landing.

These requirements explain the stops. The plan permits public upstream APIs and forbids private runtime copies, modified forks, and duplicate production mechanisms.
Changing a contract to fit an API requires a separate owner decision. The [plan's boundaries and acceptance][plan] already establish this distinction.

## 3. B: exact component selection fails before dispatch

### Intended replacement

B transfers component loading, compilation reuse, linking, and invocation lifecycle to native wasmCloud.
Calling `DispatchTarget` alone does not complete that replacement. The native loader must also replace the duplicate WAMN caches.
WAMN retains authorization and trusted artifact selection around that mechanism.

| Existing mechanism to remove | Required native replacement |
| --- | --- |
| Digest-to-`Component` cache and duplicate compilation | Native loading and compilation reuse, after WAMN establishes artifact trust |
| `PreparedCache`, `PreparedComponent.pre`, linker cloning, and `InstancePre` orchestration | Native workload resolution and preparation |
| Manual `NodeInstance` construction, execution, and teardown | Native dispatch for released, nested, and frozen-candidate paths |

Trusted digest inspection and the persistent Wasmtime disk cache remain legitimate responsibilities.
Immutable statement and authority facts also remain. Neither justifies a second guest lifecycle.
The [B report][b-report] records that no production execution code was removed at the stopped experiment.

### Executed counterexample

WAMN can represent a release with two different components that export the same registered operation.
Its manifest selects one exact component digest for a dependency. The production parser refuses the manifest if that selected digest disappears.
The remaining component cannot satisfy the dependency merely because its interface matches.

The experiment constructs this sequence:

```text
Release contains root R and children X and Y.
X and Y export the same operation interface.
R's admitted dependency explicitly selects digest X.
The host installs its policy function through on_workload_item_bind.
Native resolution sees two exporters and refuses the workload.
The installed host policy function never receives the call.
```

The native error contains `cannot disambiguate the provider`.
The [upstream resolver][up-resolver] refuses duplicate exported interfaces before it considers existing host functions in the linker.
`UnresolvedWorkload` exposes no public selection override or component mutation before resolution.
The public component map belongs to the resolved workload, which this case cannot create.

The positive control uses two components. It returns the host-selected value `37` and a typed refusal in separate cases.
Direct initialization of the child traps. Successful execution through the root therefore distinguishes the host function from automatic child linking.
The three-component case then isolates the earlier resolution failure. The [executed fixtures and receipts][b-report] contain both controls.

This establishes a mismatch between the production manifest parser and native workload resolution.
The fixtures use scalar functions, not a complete admitted production release. They do not establish every admission or application-delivery condition.
They also do not show that native dispatch fails for all workloads.

### Why the apparent shortcuts do not complete B

Rejecting all duplicate exports adds an admission restriction that the owner did not approve.
Creating a separate workload for every node changes the deployment model.
Retaining manual preparation while calling native dispatch leaves the deletion contract incomplete.
Changing private resolver code introduces the fork or copied internals that the plan forbids.

The verbatim stop condition is:

> a required context, candidate or loading boundary cannot be represented through public APIs. Record the exact obstacle before expanding the adapter. Do not copy internals or preserve duplicate machinery merely to report native adoption.

B can resume when a supported public resolution path preserves the exact selection within one admitted release.
The path must honor the required host policy boundary before interface ambiguity prevents resolution.
A new upstream release alone does not establish this property. The distinguishing experiment must pass through that supported path.

### The deadline issue is narrower than the loading blocker

Native dispatch initializes the component before its response timer starts. That timer alone leaves initialization outside WAMN's required budget.
The [dispatch source][up-dispatch] establishes that ordering.
The experiment therefore places one absolute deadline around the complete dispatch future.

The positive cases use a 200-millisecond enclosing deadline and a 30-second native response timer.
The native timer cannot explain a positive result within the enclosing budget.
Separate subprocesses test nonterminating initialization, execution, cancellation, memory return, and a subsequent finite allocation.
The native-timer-only control retains guest allocation until a two-second external watchdog terminates it.

The first execution experiment fails with Tokio's default current-thread scheduling.
An isolated diagnostic records a 6.153198497-second delay before control returns.
Native epoch interruption makes the guest task runnable again. A batch of 61 scheduled tasks delays timer polling.
The public `event_interval(1)` setting resolves that isolated case without increasing the deadline.

The final experiment passes seven tests, with zero failures and zero ignored cases. Focused Clippy also passes.
Those results support the enclosing-deadline approach. They do not install it in the production runtime.
Production nested authorization, caller propagation, child budget inheritance, candidate handling, and bounded metric labels remain unproved for B.
The [B evidence][b-report] records the failed runs and the final result separately.

## 4. B2: warm stores need a different request lifetime

B2 reuses initialized guest stores across calls. Existing allocator reuse and compiled-code reuse do not establish that behavior.
WAMN currently places request state in store data. A warmed store outlives one request.
The [execution model][model] and [native pool contract][up-pool] expose this lifetime difference.

A one-tenant workload does not, by itself, isolate requests within that tenant.
Different callers can hold different permissions or credential types. An operation can require fresh credentials even when another operation accepts an existing session.
A retained transaction, resource handle, result, or unfinished task can carry authority from an earlier call.

B2 requires the following correctness evidence:

1. Actual instance reuse and one tenant scope per workload, with release, environment, and binding separation.
2. Request state outside the warmed store's lifetime, with explicit authority binding and revocation on each invocation.
3. Alternating callers and credential types, including permission refusals and fresh-credential requirements.
4. No prior transaction, invocation resource, caller-dependent result, or unfinished task reaching the next call.
5. Eligibility across every linked component that shares the store. An unproved component remains ephemeral, which means created for one call.
6. `maxConcurrency = 1`, with ephemeral fallback when the warm pool is full.
7. Independent request and memory limits, plus retirement after traps, deadlines, release changes, and credential changes.

Pool size does not bound total admission because overflow creates ephemeral stores.
Each request still needs its own admission and memory accounting. Idle reclamation must also release retained resources.
These are required properties, not claims of a demonstrated leak in the current fresh-store path.

The mechanism, resource rules, admission rules, execution model, and ledger row 4 must land in one commit.
The current `poolSize` refusal remains until that complete landing. A policy-only amendment or proof-only pool cannot establish production reuse.
The owner explicitly removed the benchmark prerequisite. The later comparison against 2.9 records noisy or neutral results without blocking correctness adoption.

B2 currently waits on B and these unexecuted correctness conditions. It does not wait on a maintenance window or a throughput result.
The [approved B2 section][plan] records this corrected gate.
No warm-store implementation or production isolation proof is claimed here.

## 5. C: native broker mechanics cannot cross the retained host boundary

### What native NATS already provides

Native `wasmcloud:nats` provides bounded pulls, message metadata, acknowledgements, delayed redelivery, termination, and publication with a server acknowledgement.
Named bindings keep connection credentials on the host.
The [native interfaces][up-nats-wit] therefore supply substantial JetStream functionality.

WAMN adds release-registration rules and correlated dead letters. A dead letter records a failed event.
The host associates each fetched broker message with its original identity.
That association determines which failed event the host can publish and acknowledge.

The Receiving materializer calls the existing delivery bridge and `RouterDriver::execute_with_causation`.
Native NATS does not require B's native application dispatch for this path.
The [C source report][c-report] establishes that independence and records the approved stop.

### Where public reuse stops

The native connection, consumer, and message modules use `pub(super)`, which restricts access to their parent module.
Public types inside those private modules do not become public embedding APIs.
`WasmcloudNats` exposes construction and workload binding, but no supported forwarding method for WAMN's checked broker message.
The [module exports][up-nats-mod] and [plugin implementation][up-nats-plugin] identify this boundary.

WAMN first establishes the exact package and registration against the serving release.
It then retains the fetched message alongside `DeadLetterIdentity` in the host.
The host derives the destination, deduplication identifier, source stream, sequence, attempts, headers, and body from that retained state.
It awaits publication acknowledgement and requires `DEAD_LETTER_STREAM`. The [WAMN implementation][wamn-nats] contains these operations.

Native `term` settles the original message. It does not perform that correlated publication.
A guest publication with caller-supplied fields does not preserve the existing host-owned association with the fetched message.
Retaining WAMN's policy therefore requires a supported bridge to the native message, which the inspected public Rust API lacks.

Native consumer attachment already enforces stream permissions, stored subject filters, and the pull-consumer shape.
It does not accept WAMN's exact release-registration context or a host authorization callback for each attachment.
Its public consumer information also omits acknowledgement policy. WAMN needs that policy for exact consumer configuration comparison.
The [native attachment implementation][up-nats-attach] identifies these distinctions.

The plan already permits WAMN to retain consumer administration and configuration enforcement.
Retaining those operations does not expose the native message handle.
There is also a limit to the authority argument: current WAMN accepts a guest-supplied durable consumer name.
The missing native consumer-name grant alone is therefore not evidence of a newly lost host guarantee.

### Why the adapter is a separate decision

A trusted platform component can host the policy and import native NATS.
The capability registry can restrict that import to the adapter alone.
That restriction supplies a real answer to direct-import bypass. C does not dismiss the adapter as inherently bypassable.

The adapter changes where trusted platform policy executes. It therefore needs an owner decision and an executed proof of the restriction.
That proof must also preserve original-message identity, registration, reserved publication authority, completion before acknowledgement, isolation, and cleanup.
Implementing the adapter now does not qualify as completing C under the retained Rust-host boundary.

Deferred decision `wamn-0ct2.6` reopens after the binding surface stabilizes and the native message handle becomes public.
The [upstream manifest][up-features] proves that `host-component-plugins` is opt-in.
The [component-plugin module][up-component-host] describes persistent supervised stores, resource proxies, caller identity, and cancellation support.
Those mechanisms do not expose private native message types to an external Rust embedder.

The combined reopening trigger is owner policy. The pinned manifest does not state the quoted rationale about binding maturity.
This brief therefore does not attribute that exact rationale to an upstream source.
Upstream feature availability alone does not reopen implementation.

The verbatim stop condition is:

> identify any release, metadata, acknowledgement or authority requirement the public interface cannot preserve. Keep the necessary WAMN portion and document why; do not widen grants or rewrite the event architecture to claim adoption.

C records four successful Git identity commands and hashes for 15 source files.
It ran zero builds, broker tests, cluster journeys, or benchmarks. It removed no production code.
The result is a source-level stop, not an executed failure of native event delivery or an adapter bypass proof.
The [C receipts][c-report] retain that distinction.

## 6. F: command transactions need one held database session

A transaction commits its statements together or rolls them back.
WAMN commands use one held PostgreSQL session across claim creation, parameterized statements, result handling, and finalization.
The guest names admitted statement digests. The host selects SQL and retains credentials, operation authority, and transaction ownership.

The native `PgId::query` and `PgId::execute` methods each acquire a connection for that call.
The connection pool field is private, and `client()` is `pub(super)`.
Prepared-token execution reacquires a connection and prepares the statement again.
The [named PostgreSQL implementation][up-pg] exposes these facts directly.

This sequence does not establish the required transaction:

```text
execute("BEGIN")       -> acquire and release one connection
query(statement, args) -> acquire and release a connection
execute("COMMIT")      -> acquire and release a connection
```

The calls do not own one continuous lease. Another checkout can use another session or interleave between calls.
A pool limit of one does not express exclusive ownership across the complete command.
It also does not establish cleanup when cancellation interrupts that command.

Native PostgreSQL does support SQL batches through guest interfaces. It also supports asynchronous result streams.
The batch method accepts SQL without parameter bindings or row results and is private on the external `PgId` Rust surface.
The [async implementation][up-pg-async] retains a connection for one result stream, not a reusable transaction resource across separate calls.
Neither fact demonstrates the held-session API that F requires.

Concatenating SQL and values changes the admitted-statement boundary and parameter handling.
A read-only native backend beside the existing transaction backend retains parallel ownership without satisfying the proposed deletion.
The plan excludes those approaches unless a narrower substitution demonstrably removes machinery without duplicating pool ownership.

Current WAMN stores the held connection in transaction state. Its cancellation guard destroys an unfinished connection instead of returning it to the pool.
The statement transaction retains its allowed statement set and uses the same state for execution, commit, and rollback.
The [transaction implementation][wamn-pg] identifies the mechanics that a replacement must preserve.

The verbatim stop condition is:

> public APIs cannot expose the necessary session, checks or cleanup without private access, a fork, concatenated SQL/value substitution or weakened authority. Record that specific result and stop; do not build a parallel read-only backend solely to claim reuse. A narrower substitution proceeds only if it demonstrably removes existing machinery without duplicating pool ownership. No new database objects, roles or proxy infrastructure.

F can resume when a supported public API exposes a held connection or equivalent command transaction resource.
It must preserve parameterized execution, credential-generation routing, exact database identity, typed errors, and cancellation cleanup.
The required live proof then covers a real admitted query, a multi-statement commit, and rollback after an intermediate failure.
F currently records source inspection only. No failing database experiment or successful native transaction substitution is claimed.

## 7. D: a feasible fallback awaits product decisions

D owns HTTP connection reuse under `wamn-ctc8.16`. It does not depend on native application dispatch, native NATS, or PostgreSQL substitution.
Its separate owner started from the source snapshot named above.
At this snapshot, D completes source feasibility work and awaits owner decisions about the transport implementation and resource limits.
No D production edit, build, or benchmark is claimed.

The historical [HTTP experiment][d-report] ran on the WAMN 2.8 fork at `735b57982545358409a7d965a22549b08487ca09`.
Its final run produced 27 expected records and demonstrated connection reuse.
That evidence is not an executed 2.9 adoption proof.

One cleartext HTTP/1.1 case approves `127.0.0.2:43935` through WAMN's resolver.
The native client independently resolves the logical destination and sends to `127.0.0.1:43935`.
Observing the connected address afterward cannot prevent that dispatch. Destination authorization must constrain the connection before the request reaches its peer.

The pinned [2.9 client source][up-http] still constructs its connector privately.
D must establish whether the public path can enforce the approved endpoint before dispatch.
If it cannot, the approved plan permits bounded reuse around WAMN's existing pinned-destination transport.
That fallback preserves one transport implementation and requires no runtime fork.

The D owner identifies direct use of the existing Hyper client as a candidate fallback.
The proposal combines a pinned TCP/TLS connector, connection-owned quota permits, and disabled retries for canceled requests.
This is a source-level proposal, not an executed guarantee. Transport and resource-limit decisions remain with D's owner.

The pending transport question asks whether direct Hyper use can replace per-call client construction without a runtime fork.
The proposal requires permits that live as long as their sockets and `retry_canceled_requests(false)`.
Its purpose is explicit connection accounting and retry control. No production implementation or executed transport proof exists at this snapshot.

The D owner also requests decisions on these proposed limits:

| Boundary | Proposed value | Scope or consequence |
| --- | --- | --- |
| Retained clients | 128 per process | Includes retained transport entries. |
| Sockets and active requests | 64 sockets and 32 requests per process | Counts active, idle, and draining sockets within the socket limit. |
| Logical connection | Eight sockets and eight requests | Shares limits across bindings and credential generations. |
| Request and response bodies | 8 MiB each | Larger transfers can fail. |
| Headers | 32 KiB and 100 fields per block | Larger header blocks can fail. |
| Total transport duration | 30 seconds | Covers the complete transport operation. |
| Idle retention | 30 seconds, two idle sockets per client | Bounds unused connection retention. |
| Capacity saturation | Immediate refusal | Adds no waiting queue. |

The proposed logical key contains tenant, project, environment, and connection instance.
The limits apply per process, with no cluster-wide quota claim.
These values are proposed product policy, not measurements, approved limits, or required upstream constants.
The owner must decide the behavior for large transfers and saturated capacity before D implements it.

Reuse also requires authority and resource proofs:

1. Clients live above the request-local store and plugin lifetime, with authority enforced on every request, including cache hits.
2. Tenant, project, environment, binding, credential generation, approved destination, and TLS policy remain separate.
3. Old generations drain without lending authority or multiplying the same logical quota.
4. Connection counts, concurrent requests, retained clients, headers, body bytes, and total duration stay bounded.
5. Released, candidate, and nested callers retain their existing authority rules across supported protocols.
6. Mutation dispatch counts and lost responses retain truthful outcomes without hidden mutation retries.

The old experiment explains why separate limits matter.
Two generation keys each receive a connection allowance of one, which permits two connections for one logical scope.
HTTP/2 also carries four held requests on one connection. A connection limit therefore does not bound request concurrency.

Those observations do not prove every protocol or cancellation race.
The experiment's 1 KiB diagnostic threshold is not a production body policy.
Its phase timeouts do not establish a total request deadline. Its single-dispatch cases do not prove every retry race.
The [HTTP report][d-report] states these limitations explicitly.

## 8. Completed work and remaining cutover gaps

A calls `raise_descriptor_limit()` in host and executor startup before descriptor-derived limits.
Four isolated subprocess cases pass, and the omission mutant fails the two expected raise cases.
The parent runner's limits remain unchanged. Hard-ceiling cases do not inject a failing system call.
The [A report][a-report] identifies source `de444a64ebc862815f0c604058dcc83785a292b1`, binary hashes, commands, and results.

P3 converts the actual HTTP shell to `wasi:http/handler@0.3.0`.
Its executed source is `a2ea0ef32dde01408521041c0657e944fccf649d`.
The same HTTP artifact passes 13 authenticated routes, eight P3 protocol cases, and the deployed Receiving journey.
Receiving records 19 histories, 129 steps, and seven boundary cases in the [P3 report][p3-report].

The raw HTTP artifact SHA-256 is `33e08d96ece969573bb2dd153b8617d1d0755700fef168372a0d8b88a90b7da8`.
Its OCI manifest digest is `sha256:35890f2307bf532bb44efd0022220c36eedf18af3237ead481f60acf8d688efd`.
Synchronous inner WAMN calls remain. P3 does not establish native application dispatch, warm stores, outbound HTTP pooling, or asynchronous execution throughout the application.

The separate [2.9 cutover report][cutover-report] retains three open follow-ups:

| Issue | Remaining work | Relation to alignment |
| --- | --- | --- |
| `wamn-10yt.74` | Define and implement the authorized production automation admission producer | No synthetic producer is permitted merely to close a proof gap. |
| `wamn-10yt.75` | Prove executor drain with actual queued or active work | The active-work proof depends on the real producer. Idle drain evidence is narrower. |
| `wamn-10yt.76` | Resolve the native initial NATS connection timeout retry gap | Existing recovery evidence does not establish successful retry for that initial timeout. |

These gaps remain visible after the runtime cutover. Existing baseline failures and unavailable proofs also prevent a whole-product readiness claim.
They do not invalidate the specific completed A and P3 results.
They do not authorize a fork, fabricated admission path, or wider grants.

## 9. Questions for external reviewers

The B, B2, and C owner decisions are settled. D retains separate transport and resource-limit questions.
External review can challenge the source conclusions with a concrete public API path or propose a separately approved contract change.
An answer to D's questions does not resolve the B, C, or F API boundaries.
The useful review questions are:

| Question | Evidence that resolves it | Existing owner |
| --- | --- | --- |
| Does a supported loader path preserve exact digest selection and the host policy function before duplicate-interface refusal? | The B counterexample passes within one admitted release, without private access or new admission restrictions. | `wamn-0ct2.2` |
| Can production dispatch preserve the enclosing deadline and cleanup through initialization and nested calls? | Actual WAMN node and nested-call proofs retain caller identity, child budgets, and memory return. | `wamn-0ct2.2` |
| Which components can safely retain a live store across differently authorized requests? | Observed reuse, request-state separation, linked-component eligibility, and the complete B2 correctness proof. | `wamn-0ct2.3` |
| At the stated upstream trigger, where does trusted NATS policy belong? | Public binding and handle APIs, an owner decision, and an executed sole-importer and message-identity proof. | `wamn-0ct2.6` |
| Does an overlooked public PostgreSQL API expose the required transaction lifetime? | Parameterized calls and cancellation cleanup on one held session, followed by commit and intermediate-failure rollback proofs. | `wamn-0ct2.5` |
| Can native HTTP enforce the approved destination before dispatch? | Executed 2.9 destination and authority evidence, or the approved bounded fallback with its full acceptance. | `wamn-ctc8.16` |

Every future substitution must identify its source, artifacts, executed commands, counts, removed code, and remaining deviations.
Ledger and reproduction instructions move with the implementation.
Performance-sensitive substitutions retain the plan's identified 2.9 comparison requirement, subject to the owner's limits on benchmarking.
B2's comparison follows its correctness landing and is not a prerequisite.

## 10. Evidence provenance and limits of this brief

This brief reads pinned source, existing reports, their recorded results, and current issue status.
It introduces no production code, compiled artifact, runtime configuration, grant, or interface change.
It removes no production code and changes no ledger rule or test recipe.
The quoted stop conditions retain their exact text from the approved plan.

B's proof baseline is `826037ff03702a35a649ab9d21a280f6f021c15a`, with checkpoint landing `b214506aa6cda434202e22d22941ff19390c37c8`.
C's source baseline is `88db1d3a138fe6f3ae21e450223c3f30d1a64532`, with stop landing `f83b83164e6462b481d58294a66fa39f3fd343ad`.
The linked reports preserve exact patches, artifact identities where applicable, commands, failure records, and result counts.
F remains source-only and has no compiled proof artifact.

All GitHub links pin locally inspected revisions. Browser retrieval returned cache misses, so this brief does not claim successful web retrieval.
Local Git object and source inspection support the citations. New documentation checks concern links, source anchors, and whitespace only.
Historical test counts describe their recorded source revisions, not a new test run at the brief's snapshot.

[plan]: https://github.com/dkkloimwieder/wamn/blob/a48f7af827bb835b619eb8aa24db745916d24824/docs/architecture/wamn_native_alignment_plan.md
[model]: https://github.com/dkkloimwieder/wamn/blob/a48f7af827bb835b619eb8aa24db745916d24824/docs/exe-model.md
[ledger]: https://github.com/dkkloimwieder/wamn/blob/a48f7af827bb835b619eb8aa24db745916d24824/docs/architecture/native-alignment-ledger.md
[b-report]: https://github.com/dkkloimwieder/wamn/blob/a48f7af827bb835b619eb8aa24db745916d24824/docs/perf/2026.09/native-b-dispatch/report.md
[up-resolver]: https://github.com/wasmCloud/wasmCloud/blob/68ebece9c537f8bb4b5c9999f274ec68d60f35a9/crates/wash-runtime/src/engine/workload.rs#L1158
[up-dispatch]: https://github.com/wasmCloud/wasmCloud/blob/68ebece9c537f8bb4b5c9999f274ec68d60f35a9/crates/wash-runtime/src/engine/dispatch.rs#L759
[up-pool]: https://github.com/wasmCloud/wasmCloud/blob/68ebece9c537f8bb4b5c9999f274ec68d60f35a9/crates/wash-runtime/src/engine/instance_pool.rs
[c-report]: https://github.com/dkkloimwieder/wamn/blob/a48f7af827bb835b619eb8aa24db745916d24824/docs/perf/2026.09/native-c-nats/report.md
[up-nats-wit]: https://github.com/wasmCloud/wasmCloud/blob/68ebece9c537f8bb4b5c9999f274ec68d60f35a9/wit/nats/wit/world.wit#L219
[up-nats-mod]: https://github.com/wasmCloud/wasmCloud/blob/68ebece9c537f8bb4b5c9999f274ec68d60f35a9/crates/wash-runtime/src/plugin/wasmcloud_nats/mod.rs#L8
[up-nats-plugin]: https://github.com/wasmCloud/wasmCloud/blob/68ebece9c537f8bb4b5c9999f274ec68d60f35a9/crates/wash-runtime/src/plugin/wasmcloud_nats/plugin.rs#L43
[wamn-nats]: https://github.com/dkkloimwieder/wamn/blob/a48f7af827bb835b619eb8aa24db745916d24824/crates/platform/runtime/src/plugins/wamn_jetstream.rs#L1834
[up-nats-attach]: https://github.com/wasmCloud/wasmCloud/blob/68ebece9c537f8bb4b5c9999f274ec68d60f35a9/crates/wash-runtime/src/plugin/wasmcloud_nats/interfaces/jetstream/mod.rs#L329
[up-features]: https://github.com/wasmCloud/wasmCloud/blob/68ebece9c537f8bb4b5c9999f274ec68d60f35a9/crates/wash-runtime/Cargo.toml#L35
[up-component-host]: https://github.com/wasmCloud/wasmCloud/blob/68ebece9c537f8bb4b5c9999f274ec68d60f35a9/crates/wash-runtime/src/plugin/component_host/mod.rs#L1
[up-pg]: https://github.com/wasmCloud/wasmCloud/blob/68ebece9c537f8bb4b5c9999f274ec68d60f35a9/crates/wash-runtime/src/plugin/wasmcloud_postgres/multiplexed.rs#L48
[up-pg-async]: https://github.com/wasmCloud/wasmCloud/blob/68ebece9c537f8bb4b5c9999f274ec68d60f35a9/crates/wash-runtime/src/plugin/wasmcloud_postgres/async_p3.rs
[wamn-pg]: https://github.com/dkkloimwieder/wamn/blob/a48f7af827bb835b619eb8aa24db745916d24824/crates/platform/runtime/src/plugins/wamn_postgres/resources.rs#L34
[d-report]: https://github.com/dkkloimwieder/wamn/blob/a48f7af827bb835b619eb8aa24db745916d24824/docs/perf/2026.09/ctc8-13-native-http/report.md
[up-http]: https://github.com/wasmCloud/wasmCloud/blob/68ebece9c537f8bb4b5c9999f274ec68d60f35a9/crates/wash-runtime/src/host/http_client.rs
[a-report]: https://github.com/dkkloimwieder/wamn/blob/a48f7af827bb835b619eb8aa24db745916d24824/docs/perf/2026.09/native-a-descriptor/report.md
[p3-report]: https://github.com/dkkloimwieder/wamn/blob/a48f7af827bb835b619eb8aa24db745916d24824/docs/perf/2026.09/p3-http-cutover/report.md
[cutover-report]: https://github.com/dkkloimwieder/wamn/blob/a48f7af827bb835b619eb8aa24db745916d24824/docs/perf/2026.09/wasmcloud-2-9-cutover/report.md
