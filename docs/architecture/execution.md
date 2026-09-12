# Request execution

A request enters through a declared route and keeps its caller identity throughout execution.
The host binds the route to an admitted wiring in the supplied release.
The router decides which operation runs next. Native dispatch executes that operation.
A hot HTTP request creates no run or queue row.
Route concurrency limits refuse excess work with HTTP 429.

## Routing and availability

The HTTP ingress path authenticates the caller before invoking application code.
The host compares the route, operation, permissions, wiring, and release facts before dispatch.
Each nested operation retains the original caller and needs its own declared authority.
Component membership or an imported interface alone grants no permission to call an operation.

An unknown hostname returns the native 404 response.
An explicit hostname in the release returns 503 while its route binding is absent.
The adapter adds no `Retry-After` header and leaves application responses unchanged.
A selected route whose native dispatch handle disappears still returns 404.
Wildcard expansion, operator aliases, and readiness based on route bindings remain absent.

The [expected-host adapter](../../crates/platform/runtime/src/expected_router.rs) owns this narrow response distinction.
Service readiness does not establish route-aware traffic removal.
The native route controller can reach Pods directly, so a readiness change alone does not remove every rebind outage.

## Native dispatch

One native application owns a loaded release or frozen candidate.
Native dispatch reuses compiled component bytes and creates a fresh invocation store.
The store is Wasmtime's request state, including guest memory and bound authority.
One absolute deadline covers initialization, execution, and nested calls.
A child cannot extend that deadline.
Completed, cancelled, trapped, or timed-out invocations do not return that store to a reusable guest instance.

The runtime uses `InstancePolicy::Ephemeral` and refuses a positive `poolSize`.
Wasmtime's pooling allocator remains enabled under its separate resource limits.
Native epochs bound guest execution. Duration metering does not grant instruction fuel.
All cleanup paths retain their resource bounds and return failures to their owner.

The [driver](../../crates/execution/host/src/router_driver.rs) coordinates the graph through native dispatch.
Its private modules own admission, authority, dependency calls, workload loading, and invocation state.
The pure router owns deterministic graph decisions.
After initialization, the host records caller, SQL, claims, causation, and effect authority in an invocation scope.
Native callbacks restore the trace context from that scope.
Cleanup revokes the scope and clears bindings after success, failure, cancellation, or owner shutdown.
Candidate execution still refuses nested calls.
The executor owns durable queue claims and settlement through the existing run-state libraries.

## Results and effects

A wiring edge carries the declared route envelope.
Items retain their `request_id` and contain either `value` or `error`.
Each node consumes that envelope and emits its declared output port.
Connected ports require identical canonical schema digests.
A target with multiple input ports requires an explicit `to_port`.
A hop limit bounds graph traversal.
The router does not invent item fan-out or split one emission into independent branches.

A completed database mutation and a later failed effect remain distinct outcomes.
A response can establish partial completion only when its declared contract carries the committed result and failed outcome.
A transport failure or an unknown response does not establish that nothing ran.
Automatic whole-command replay cannot follow from an inner operation's idempotency.

The blob writer stores each successful item and preserves existing error items.
It enriches the value with storage information instead of replacing the original result.
A write failure fails the emission as a whole.
Independent reporting for each failed blob item remains unimplemented.

Commands use the [transaction rules](data-access.md#inputs-queries-and-transactions).
Effects use the [capability contracts](capabilities.md), including their exact refusal and outcome types.
No second error vocabulary replaces a public WIT or serialized application contract.

The premium durable effect protocol remains shelved.
Its rule forbids redispatch after a recorded attempt without a recorded outcome.
A configured durability class alone supplies no such protection.

## Caller authentication

The route declares PAT authentication, session authentication, or both.
A personal access token, or PAT, is an opaque bearer credential.
PAT authentication reads the current principal, credential, organization scope, and applicable membership before admitting the request.
Authorization reads current operation permissions.
Service credentials retain their declared scope and cannot mint a human session.

A session token carries the human roles selected at minting.
The host reads current permissions for those roles on each request.
It does not reread identity membership for each session request.
PAT revocation, principal disablement, and removal of membership or roles therefore affect existing sessions through their bounded lifetime.
Permission changes apply to the next request.

The exact audience is `urn:wamn:project-env:{org}:{project}:{env}:{instance_suffix}`.
The configured environment supplies its current suffix.
No historical suffix-uniqueness promise follows from that value.

Session tokens use Ed25519 with `typ=wamn-session+jwt`.
They require `iss`, `sub`, `org`, `aud`, `roles`, `exp`, `iat`, and `jti`.
The `kid` selects a key only within the configured issuer.
Wrong issuer, organization, audience, algorithm, signature, or time bounds returns 401.
Unknown roles grant no implicit authority, and an empty permission set returns 403.

The token lifetime is at most 900 seconds, with 30 seconds of time tolerance.
Minting anchors expiry to the start of credential validation and records the actual signing time in `iat`.
A delayed mint cannot restart the lifetime.
The host reconsiders admission freshness after permission reads.
Age bounds limit delayed admission without promising that identity remains unchanged during a pause.

Fresh-only operations require PAT authentication, including nested calls under the original caller.
A session that otherwise has permission receives `fresh-credential-required`.
The client does not replay that operation or silently replace its credential.
Authentication deadlines bound new admission and do not cancel work already accepted.

The [session token owner](../../crates/identity/platform/src/session_token.rs) defines the token and time rules.
The service keeps no session table, denylist, per-session write, or refresh token.
The current client retains its session token only in memory.
External federation and unbuilt identity design remain in the [identity plan](../plan/identity.md).

## Session keys

The host uses an explicit HTTPS issuer endpoint and nonempty trusted CA configuration.
It does not discover endpoints, follow redirects, use ambient proxies, or add ambient trust roots.
A fetched key set replaces the preceding complete set.
Unknown keys cannot obtain authority from another issuer.

Key freshness is bounded to 300 seconds from request start, including the response's HTTP `Age`.
The cache permits one fetch per issuer at a time, with starts at least one second apart.
A fetch permits five seconds and 65,536 response bytes.
A stale set refuses authentication even for a previously known key.
Final admission cannot extend freshness after slow permission reads.

The identity service publishes a public key before activating it for signing.
A transaction lock establishes the stop-signing boundary before retirement.
Public keys remain available for 930 seconds after that boundary.
An emergency key removal does not activate a replacement.
Previously cached keys can still authenticate for their remaining 300-second freshness window.

Private signing keys remain in the system database and identity service.
The public endpoint exposes only public key material.
The [key cache](../../crates/platform/runtime/src/session_keys.rs) and [key owner](../../crates/identity/platform/src/session_keys.rs) define these bounds.

## Operator clients

Generated screens use the [client contract](../../crates/schema/generator/src/client_ir.rs), derived from admitted declarations.
Operation kinds come from the manifest.
Record links come from declared relations, keys, and compatible reads.
The generator never guesses a record from a field name ending in `_id`.

A revision-bearing operation needs a declared compatible record read and revision mapping.
Without that mapping, the screen requires ordinary Rust composition and blocks submission.
A user cannot type a revision or obtain a guessed read route.
Query columns come from that query's result descriptors, not a union of model fields.

Fields separately declare whether a property is required and whether its value can be null.
Editors retain `Absent`, `Null`, and `Value` according to those two facts.
Nested and repeated fields retain their declared bounds.
Unsupported input types block submission.
Unknown output types remain display-only and appear as opaque values.

The served route supplies its terminal response contract and whole-operation replay guarantee.
A composed route does not inherit replay safety from an inner command.
Unknown replay information grants no retry.
An operation without an HTTP route remains visible as unexposed.
Event handlers do not become operator actions.

The shared request builder canonicalizes typed UUID, timestamp, and numeric input through `wamn-client`.
The invocation layer canonicalizes bytes and does not reinterpret field types.
The current client submits one outer envelope item, while nested repeated fields remain supported.
Cursors remain opaque and reset when filters or sorting change.

The [submission reducer](../../crates/client/tui/src/submission.rs) owns `Editable`, `Pending`, `Succeeded`, `Refused`, `PartiallyCompleted`, and `Uncertain`.
A pending command blocks a second submission.
A confirmed refusal without completion keeps the draft editable.
Success and confirmed partial completion spend the submission.
Unknown errors, malformed responses, and transport failures retain uncertainty about completion.

Uncertainty belongs to the whole submitted intent.
A later retry refusal does not clear uncertainty from an earlier attempt.
Safe retry uses the captured body, key, time, and expected revision byte-for-byte.
Authorization headers are derived again.
Only the served claim-replay contract permits this retry.
A state command or effectful composition requires a refresh before a new intent.
Abandoning local intent cancels no server work.

The client binds to the served URL, host, and target instance.
A replacement target resets records, revisions, cursors, drafts, and pending state.
Submissions remain blocked until the matching replacement succeeds.
A rebuild that preserves the earlier activation leaves that session usable.
An invalidated session cannot replay old-target mutations against a replacement.
Process restart prevents old responses from reaching the new session.

The generated client contains no endpoint, host, or token.
Development supplies them from the served target.
Scaffolding copies screens into application-owned Rust over the same request and submission primitives.
Typed incompatibilities fail compilation, while declared interaction tests cover only their asserted behavior.
Deferred population declarations, multiple outer items, and web clients remain in the [operator UI plan](../plan/operator-ui.md).

## Process lifecycle

Host readiness requires a real native command-loop beat.
Executor liveness follows queue turns and successful lease renewals.
No timer creates a synthetic beat.
An absent first beat fails through the normal silence budget.

The host drains readiness before its configured signal delay and native shutdown.
Ingress failure, probe failure, or command-task failure follows the failure cleanup path.
The process bounds plugin coordination, auxiliary task cleanup, telemetry flush, and final runtime shutdown.
Cleanup timeout remains an error and can leave plugin cleanup incomplete.
The HTTP-stop allowance bounds coordination and does not guarantee native completion.

Executor shutdown bounds its current queue turn and auxiliary cleanup.
A turn beyond the budget retains the existing durable lease for recovery.
Production automation admission remains absent under `wamn-10yt.74`.
The dependent active-work shutdown case remains under `wamn-10yt.75`.
Idle shutdown does not establish either behavior.

OpenTelemetry records invocation and effect spans, request timing, and delivery outcomes.
Operational logs use INFO, and detailed request traces go to Tempo.
The bounded router tap supplies the live view without durable node histories.

The [deployment guide](../operations/deployment.md) owns probe ports, grace periods, and rollout commands.
The [native alignment page](native-alignment.md#current-limits) retains the unresolved operator retry and liveness limits.
