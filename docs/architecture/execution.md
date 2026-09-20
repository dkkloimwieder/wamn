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

An attachment with auth policy `none` has no principal, so it cannot write.
If the reachable wiring of an anonymous attachment holds a registered operation or a transactional statement, [release mint](../../crates/control/lib/src/publish_release/components.rs) refuses it.

A route input that fails its schema returns HTTP 400 with the code `schema-invalid`.
The body carries the RFC 6901 pointer of the offending value in `data.pointer`:

```json
{"error":{"code":"schema-invalid","data":{"pointer":"/0/change/created_by"}}}
```

For an unexpected property, the pointer names that property.
An unparseable payload carries the root pointer `""`.
A platform cause, such as a missing or uncompiled schema, gives a body with no `data`.
The [route validator](../../crates/platform/runtime/src/plugins/flow_http_routing.rs) and the [HTTP route adapter](../../apps/platform/ingress/http-route/src/lib.rs) own this response.

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
Native dispatch reuses compiled component bytes. Untrusted components receive a fresh store, which holds guest memory and resources, for each call.
The deployment owner can select reviewed component digests for native warm-instance reuse.
[Deployment configuration](../operations/deployment.md#trusted-component-reuse) owns this selection and pool sizing.
Application manifests cannot grant that trust.

Warm eligibility covers the complete unit that shares a native store.
If any member requires fresh execution, native dispatch keeps the entire shared unit fresh.
Independent components can use different lifetimes in one workload.
The host does not split linked stores to permit reuse.

Each call receives a new host-owned authority scope.
Capability and nested calls use that scope, and cancellation revokes it independently of native store teardown.
A retained guest context cannot select the next call's permissions.
A call that leaves host resource handles open discards those resources and retires its instance while preserving its completed result.
The host never clears a resource table and then reuses that instance, because old handles can alias new resources.

Trusted component code owns request-local caller data, caller-dependent cache cleanup, and completion or cancellation of spawned tasks before return.
The host does not sanitize guest memory or enforce those promises against faulty or malicious trusted code.
Use fresh instances wherever that trust is unacceptable.
`maxConcurrency = 1` serializes calls on an instance. It does not isolate guest memory or background tasks.
One absolute deadline covers initialization, execution, and nested calls.
A child cannot extend that deadline.

Connection activation changes the generation selected for new work.
Admitted runs that carry a generation pin continue to use that exact immutable generation.
A disabled instance or an unavailable pinned credential refuses execution without selecting a newer generation.
Superseded generation records remain immutable. Retaining a record does not extend the lifetime of its credential.


The host bounds each requested node deadline to 1–30,000 milliseconds and continues execution.
When the host changes a deadline, the delivery report carries the node, `requested-ms`, and `effective-ms`.
HTTP responses carry this JSON array in the `wamn-deadline-adjustments` header, including execution failures.
Application response bodies remain unchanged.
Queued runs store the array in `runs.deadline_adjustments_json` before settlement or retry, under the current lease.
The column retains the latest attempt that reports a change.
An absent header or null column means that no change was reported.
Nested calls still inherit the enclosing deadline. Their remaining time is not a separate node deadline adjustment.

Native dispatch retires warm instances after traps, deadlines, or cancellation.
Caller authority ends immediately. Physical teardown follows native cancellation rules. A spinning abandoned guest must exceed both the native grace and its continuous-execution threshold.
Successful calls can reuse an eligible instance only when no host resource handles remain.
A full pool serves overflow from fresh stores.
Native idle reclamation and a 1,000-call instance limit bound retention.
Pools belong to one immutable release or candidate and never cross tenant or environment boundaries.
A replacement release or deployment trust configuration uses new pools.
Credential checks retain the exact pinned generation rules above.

Pool capacity does not limit request admission or total memory.
Existing route and queue admission limits, native memory accounting, and deployment memory limits remain independent.
Wasmtime's pooling allocator remains enabled under its separate resource limits.
Native epochs bound guest execution. Duration metering does not grant instruction fuel.
All cleanup paths retain their resource bounds and return failures to their owner.

The [driver](../../crates/execution/host/src/router_driver.rs) coordinates the graph through native dispatch.
Its private modules own admission, authority, dependency calls, workload loading, and invocation state.

Typed operation inputs and successful results contain owned lists or records.
Admission checks the context, node errors, and every nested value.
It refuses store-owned resources at these boundaries.
Nested calls use the typed entrypoint under the existing authority and deadline.
JSON adapters serve HTTP and dynamic routing.
The pure router owns deterministic graph decisions.
After initialization, the host records caller, SQL, claims, causation, and effect authority in an invocation scope.
Native callbacks restore the trace context from that scope.
Cleanup revokes the scope and clears bindings after success, failure, cancellation, or owner shutdown.
Candidate execution still refuses nested calls.
The host owns durable queue claims and settlement through the existing run-state libraries.
HTTP admission and queue delivery use separate concurrency bounds, so one workload cannot consume the other's capacity.
Each host replica polls the durable queue and claims work through database leases; replicas need no wake service or process-local handoff.

The host binds the executing principal as `app.user_id` in each claims transaction that it opens.
[Record history](data-access.md#record-history) stamps that principal on every write.

- An authenticated route caller binds its principal id.
- A nested call binds the executing principal of its parent.
- A post-commit registration delivery binds `wamn:materializer`.
- An automation delivery binds its admitted service principal.
- A legacy queue delivery or management candidate case binds `wamn:executor`.

The same transaction binds the executing operation as `app.operation`.
A node binds the operation token that it runs, so a nested call binds its own token.
A registration delivery binds the token of its handler.
A host queue claim, reap, renew, or complete transaction binds the `wamn:executor` credential-class identity.
An absent operation binds an empty value, so a pooled connection keeps no earlier operation.
The [record history log](data-access.md#history-tables-and-the-log-trigger) records the operation in each entry.

A platform claims transaction that only reads binds no principal and no operation.
A bound principal alone does not move a read out of the autocommit path.
The [claims owner](../../crates/platform/runtime/src/plugins/wamn_postgres/claims.rs) refuses a transactional statement with no executing principal before it reaches PostgreSQL.
That refusal carries SQLSTATE `55000` and the message `actor-required`, as the stamp trigger does.

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
Receiving and Acme publish both modes on their existing authenticated application routes.
Both modes use the same operation permissions. Private event handlers remain private.
A personal access token, or PAT, is an opaque bearer credential.
PAT authentication reads the current principal, credential, organization scope, and applicable membership before admitting the request.
Authorization reads current operation permissions.
Service credentials retain their declared scope and cannot mint a human session.

A session token carries the human roles selected at minting and its revocable authority.
For each new request, the host reads the current principal, environment membership, and password login or source PAT.
It intersects signed roles with current assignments and reads current permissions for the active tenant user.
No active-session result is cached. Missing, revoked, expired, or mismatched authority returns 401.
An unavailable identity read refuses admission with 503. The read has a five-second timeout.
Logout, reset, account disablement, membership removal, and PAT revocation therefore affect the next admission.
Already admitted work retains its caller. Revocation does not cancel that work or undo its effects.

The exact audience is `urn:wamn:project-env:{org}:{project}:{env}:{instance_suffix}`.
The configured environment supplies its current suffix.
No historical suffix-uniqueness promise follows from that value.

Session tokens use Ed25519 with `typ=wamn-session+jwt`.
They require `iss`, `sub`, `org`, `aud`, `roles`, `exp`, `iat`, `jti`, and `authority`.
`authority` is exactly one object: `{"login":"<login UUID>"}` or `{"pat":"<source PAT UUID>"}`.
The password issuer binds the existing login record. PAT exchange binds the authenticated source PAT.
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
PAT exchange keeps no login record or renewal credential.
The current client retains its session token only in memory.
Password sessions have no PAT fallback. Renewal happens only when an application request needs a new access token.
A local credential refusal occurs before HTTP submission and records a refused operation, not an unknown server outcome.
Failed renewal or login expiry requires explicit login and submission. Quitting clears local credentials and requests server logout.
[Terminal login](../operations/development-loop.md#receiving-password-login) describes configuration and prompts.
External federation and unbuilt identity design remain in the [identity plan](../plan/identity.md).

### Consistent session access

Sessions and PATs use the same operation permission checks, including nested operations.
The legacy `fresh-only` metadata and client methods no longer require a PAT.
Every human session receives the same current authority checks without operation-specific password prompts.
Renewal stays automatic during active use and never extends the absolute login deadline.

Deploy the issuer, host, and client changes together. Reconcile identity-reader grants before restarting hosts.
The identity reader needs SELECT on `identity.password_logins` in addition to its existing identity tables.
Session-only hosts also require their scoped `WAMN_SYSTEM_URL` identity reader.
Existing tokens lack the required authority claim, so hosts reject them after this update. People must sign in again.

## Password enrollment foundation

The [identity library](../../crates/identity/platform/src/password.rs) supports password storage, invitation enrollment, and password authentication.
The identity service exposes password HTTP endpoints. Receiving supports hidden terminal enrollment and password login.
The [identity plan](../plan/identity-plan.md) owns the remaining delivery scope.

The [login storage library](../../crates/identity/platform/src/password_login.rs) supplies database primitives for renewal and revocation.
The HTTP service and terminal use these primitives for renewal and server-side logout.
Each login binds one human, issuer, and exact environment audience.
Its authentication time and eight-hour absolute deadline remain fixed.
Successful renewal extends the inactivity deadline by 30 minutes, capped at the absolute deadline.

The database stores only hashes of random, single-use renewal credentials.
Reusing a consumed credential revokes its login family, the sequence of replacement credentials for one login.
The caller commits that refusal so revocation persists.
Every login and renewal repeats the current membership, environment instance, tenant-user status, and assigned-role checks.
Credential rotation and access-token signing share one transaction after those checks.
The service delivers credentials only after that transaction commits.
Access-token expiry cannot exceed the login's absolute deadline.
The caller locks the principal before password verification and keeps that lock through login creation.
Renewal, logout, and password reset use the same principal lock.
Principal disablement revokes its families through a database trigger, so reactivation cannot restore them.

The issuer cannot change a login's principal, audience, authentication time, or absolute deadline.
All writes require an actor, and credential tables have no row-image history.
Consumed credentials remain until absolute expiry.
Global request admission removes at most 100 expired families per call, including their credentials.

Enrollment accepts an active, unenrolled human principal and a matching, unexpired invitation.
The library locks that principal, creates its password, and consumes every outstanding invitation in one database transaction.
Issuance records the authorized operator. Successful enrollment records the invited person.
The existing principal, memberships, and PATs remain unchanged.
The identity issuer has narrow grants for enrollment and authentication.
A restricted database function locks a principal without granting permission to change that principal.

Invitation secrets contain 32 random bytes and expire after 24 hours.
Reset secrets use the same implementation with a separate prefix, reset purpose, and 15-minute expiry.
The database stores their SHA-256 digests and explicit purpose, never the bearer secret.
Password hashes use Argon2id version 19 with 19 MiB, two iterations, one lane, and independent 16-byte salts.
These parameters meet the [OWASP minimum profile](https://cheatsheetseries.owasp.org/cheatsheets/Password_Storage_Cheat_Sheet.html#argon2id).
One shared worker budget permits two jobs, with no waiting queue, for 38 MiB of Argon2 working memory.
Cancellation retains the permit until the blocking worker ends.
Stored hashes with another cost profile refuse before expensive work starts.

Enrollment requires 15 Unicode characters and accepts at most 1,024 UTF-8 bytes without trimming or normalization.
The owner deferred common and compromised password screening.
The service limits password requests to eight per replica and HTTPS connections to 128 per replica.
Database counters enforce 60-second windows across replicas: 120 requests globally, 20 per source address, and five per account.
Enrollment uses the principal ID for its account counter; login uses a digest of the normalized email.
The source is the TLS peer address, never a forwarded header.
Refused attempts do not extend the window. Counters older than two minutes are removed during admission.
Password requests expire after ten seconds and accept at most 8,192 body bytes.
Operators behind a shared proxy also share its source limit.

`POST /invitations` requires the existing verified operator certificate and a `principal_id`.
The service sends the one-time secret through Resend to that principal's stored email.
A successful provider response means accepted for delivery, not confirmed inbox delivery.
The service makes no automatic send retry and reports failed or uncertain sends as unavailable.

`POST /password/enroll` accepts `principal_id`, `invitation`, and `password`.
`POST /password/session` accepts `email`, `password`, and `aud`.
It reuses the PAT exchange's membership, current environment, active tenant user, role, and signing checks.
Its response contains `access_token`, `token_type`, `expires_at`, `renewal_token`, and `login_expires_at`.
The two expiry fields contain Unix seconds. `token_type` is `Bearer`.

`POST /password/renew` accepts `renewal_token` and `aud` and returns the same response fields.
Each successful request replaces the renewal credential. A lost response requires another password login.
There is no retry grace period or automatic application request replay.

`POST /password/logout` accepts `renewal_token` and `aud` and revokes that login family.
It returns HTTP 204, including when the credential is already unusable.
`POST /password/logout-all` accepts the same fields and requires a currently usable renewal credential.
It revokes all password login families for that person and returns HTTP 204.
Neither operation requires continued application access. Neither revokes PATs.
Previously issued password tokens fail new admission after logout commits.

`POST /password/recover` accepts `email` and shares the global, source, and account limits.
The service sends a reset secret only for an active human with an established password.
After admission, the account-dependent path has a six-second deadline and a fixed response delay.
Known and unknown accounts receive HTTP 202 with `{"status":"if_eligible_email_will_arrive"}`.
That response does not promise delivery. Provider failure does not expose account existence.
The service makes no automatic mail retry.

`POST /password/reset` accepts `email`, `secret`, and `password`.
It requires a matching, unexpired reset credential and the existing password policy.
One transaction replaces the password, consumes all outstanding invitation/reset secrets, and revokes all renewal families for that person.
The service sends a password-change notification after commit and creates no session.
HTTP 200 reports `{"status":"password_reset","notification":"accepted_for_delivery"}` when the provider accepts the notification.
If notification fails, the response reports `"notification":"unavailable"`. The password change remains committed.
Normal login uses the replacement password. Existing PATs retain their separate revocation path.
The terminal supports recovery through the same endpoints and requires normal login after reset.
For mailbox loss, an authorized administrator updates the existing principal's email through the [operator procedure](../operations/deployment.md#mailbox-loss-recovery).
That transaction consumes outstanding email secrets and revokes renewal families. The person then uses normal recovery at the replacement address.

`POST /password/environments` accepts only `email` and `password`.
After password authentication, it returns the configured environments that pass those same access checks.
Its response is `{"environments":[{"aud":"...","org":"...","project":"...","env":"..."}]}`.
The list is ordered by audience and can be empty.

Discovery returns no session, PAT, application address, or database credential.
It shares the login throttle and request deadline with session issuance.
An unavailable authority read fails the whole request instead of returning an incomplete list.
Session issuance repeats the access checks after selection. A discovery result grants no access.

The terminal matches this list against deployment-owned application addresses.
It opens one match directly and asks the person to select among several matches.
An empty match refuses login. The issuer never supplies application addresses.

The development environment owns a separate identity process from startup until explicit teardown.
Application builds and operator exits preserve that process, its identity database, and its signing keys.
This process ownership does not add an external identity provider or change the planned OIDC adapter.

Unknown accounts and incorrect passwords receive the same unauthorized response.
These routes are enabled only when the service has a Resend key and sender.
The terminal serializes renewal and preserves the original absolute deadline with both wall-clock and monotonic expiry checks.
It clears credentials before renewal I/O, so failure or cancellation requires another login without retrying a consumed credential.
Logout clears local credentials even when server revocation cannot be confirmed.
Unknown and unenrolled accounts perform the same bounded hashing profile as existing accounts.
Password buffers erase their owned bytes on drop. Diagnostics redact passwords and invitation secrets.

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
Generated screens show the [record history](data-access.md#record-history) stamp columns `created_at`, `created_by`, `updated_at`, and `updated_by`.

Fields separately declare whether a property is required and whether its value can be null.
Editors retain `Absent`, `Null`, and `Value` according to those two facts.
Nested and repeated fields retain their declared bounds.
Unsupported input types block submission.
Unknown output types remain display-only and appear as opaque values.

Record screens recognize the reserved `created_by` and `updated_by` fields.
The platform resolves their returned IDs through `app_system.users.display_name` in the same tenant.
Label access inherits the authorized record read and requires no additional permission.
The delivery report carries labels separately, and HTTP sends them in the `wamn-actor-labels` header.
Generated SQL, application result contracts, and actor IDs remain unchanged.
The screen shows the full ID when a name is missing, empty, or unavailable.
HTTP omits label metadata above 4,096 bytes, so those results also show full IDs.
Labels remain within the accepted screen response and reset with its records or session.
The history view keeps its existing behavior.

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
A `schema-invalid` body whose `data` holds only a string `pointer` is a refusal before delivery, and the client reports that pointer.

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
Queue liveness follows durable polls and successful lease renewals in each host replica.
No timer creates a synthetic beat.
An absent first beat fails through the normal silence budget.

The host drains readiness, stops queue claims, and starts native HTTP shutdown together.
The same admission signal refuses further HTTP requests on existing connections.
One shutdown signal stops HTTP admission and queue polling, bounds active work in both paths, and then performs shared cleanup.
Ingress failure, probe failure, or command-task failure follows the failure cleanup path.
The process bounds plugin coordination, auxiliary task cleanup, telemetry flush, and final runtime shutdown.
Cleanup timeout remains an error and can leave plugin cleanup incomplete.
The HTTP-stop allowance bounds coordination and does not guarantee native completion.

Queue shutdown bounds its current turn and auxiliary cleanup.
A turn beyond the budget retains the existing durable lease for recovery.
The [enqueue-run command](../operations/queued-automation.md) admits production automation under an active service principal.
The host reads that principal and its current application permissions before each delivery.
The normal operation checks also apply to nested calls.
The legacy `fresh-only` restriction still refuses queued service callers. Human session support does not widen queued automation.
SIGTERM and SIGINT share the host cleanup budget. The budget retains the native drain and plugin shutdown allowances.
An aborted call loses its invocation authority, and native teardown releases its store.
After the lease expires, recovery takes a new lease generation. The old generation cannot complete the run.
The [runtime tests](../operations/running-tests.md#host-and-package-runtime) cover the retained host and package paths.

OpenTelemetry records invocation and effect spans, request timing, and delivery outcomes.
Operational logs use INFO, and detailed request traces go to Tempo.
The bounded router tap supplies the live view without durable node histories.

The [deployment guide](../operations/deployment.md) owns probe ports, grace periods, and rollout commands.
The [native alignment page](native-alignment.md#current-limits) retains the unresolved operator retry and liveness limits.
