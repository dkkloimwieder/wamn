# Request execution

A request enters through a declared route and keeps its caller identity throughout execution.
The host binds the route to one component operation or to an admitted wiring in the supplied release.
A route target calls its operation once, with no graph walk and no retry.
For a wiring, the router decides which operation runs next. Native dispatch executes each operation.
A hot HTTP request creates no run or queue row.
Route concurrency limits refuse excess work with HTTP 429.

## Routing and availability

The HTTP ingress path authenticates the caller before invoking application code.
The host compares the route, operation, permissions, wiring, and release facts before dispatch.
The host side of the ingress guest's two imports lives in `wamn-engine`: the [route plugin](../../crates/platform/engine/src/flow_http_routing.rs) and the [delivery plugin](../../crates/platform/engine/src/router_delivery.rs).
A host plugs in through two traits.
`RouteAuthenticator` reads the credential of a protected route, and `RouteDelivery` serves one delivery request.
The cloud implements them as [`PlatformRouteAuthenticator`](../../crates/platform/runtime/src/plugins/route_authentication.rs) and [`RouterDeliveryBridge`](../../crates/execution/host/src/router_delivery.rs).
Publish decides authorization, and the runtime trusts the loaded release.
An application's components compose at build into one component, so a call inside an application reaches no host.
Publish folds each export's call graph into its released operation: the union of the permissions, `fresh_only` if any callee sets it, and the union of the statements.
The host checks that grant once, before the entry runs, under the original caller.
Component membership alone grants no permission to call an operation.

An attachment with auth policy `none` has no principal, so it cannot write.
If the reachable wiring of an anonymous attachment holds a registered operation or a transactional statement, [release mint](../../crates/control/lib/src/publish_release/components.rs) refuses it.

The method of a route follows from the kind of its operation, and no author writes it.
Publish writes GET for a route to a `get`, `query` or `projection` operation, and POST for every other route and every wiring.
A GET carries exactly one request item in its query string, and the router reads no body for it.
Each top-level member is one parameter, and its value is the compact JSON text of the member.
Parameters stand in byte order of their names, and every byte outside the RFC 3986 unreserved set is escaped.
The router refuses any other spelling, and a request target longer than 8 KiB, with HTTP 400 and the code `invalid-target`.
The [read query codec](../../apps/platform/execution/contract/src/read_query.rs) owns this encoding, and its fixture vectors bind the TypeScript encoder to it.

The host derives the `Cache-Control` value of each read route from its operation kind and auth policy when it serves the route, and the router only writes it.
A `get` sends `no-cache`, and a `query` or `projection` sends `max-age=10, stale-while-revalidate=60`.
Both are `private`, unless the auth policy of the route is `none`, and then they are `public`.
A successful read also sends `Vary: Authorization, Cookie`.
Every other response sends `Cache-Control: no-store`: a write, any status other than 200, and any request that carries `x-wamn-csrf`.
[`read_cache_control`](../../crates/platform/engine/src/flow_http_routing.rs) owns the values.

A read also carries an ETag, which the host derives and compares.
A `get` has a strong ETag from the manifest digest of the release and the revision field of the record that it returns.
A `query` or `projection` has a weak ETag from the manifest digest and the [model versions](data-access.md#model-versions) of the relations that it reads.
Publish writes the relations and the revision field into the serving manifest route from the generated contract, and a history table stands for its model relation.
The router forwards `If-None-Match` on a GET, and the host compares it with the weak comparison.
For a list, the host applies the operation grant, reads the versions, and answers not-modified on a match without running the read.
For a `get`, the host runs the read and then compares.
On not-modified, the router answers 304 with the ETag, the `Cache-Control` and `Vary` of the route, and no body.
A read without a revision field, or with no declared relations, has no ETag, and a failed version read leaves the list without one.
A `get` tag follows `row_version`, so a row that is deleted and created again under the same natural key can match an old tag.
[`read_cache`](../../crates/execution/host/src/read_cache.rs) owns the tags.

A route input that fails its schema returns HTTP 400 with the code `schema-invalid`.
The body carries the RFC 6901 pointer of the offending value in `data.pointer`:

```json
{"error":{"code":"schema-invalid","data":{"pointer":"/0/change/created_by"}}}
```

For an unexpected property, the pointer names that property.
An unparseable payload carries the root pointer `""`.
A platform cause, such as a missing or uncompiled schema, gives a body with no `data`.
The [route validator](../../crates/platform/engine/src/flow_http_routing.rs) and the [HTTP route adapter](../../apps/platform/ingress/http-route/src/lib.rs) own this response.

An unknown hostname returns the native 404 response.
An explicit hostname in the release returns 503 while its route binding is absent.
The adapter adds no `Retry-After` header and leaves application responses unchanged.
A selected route whose native dispatch handle disappears still returns 404.
Wildcard expansion, operator aliases, and readiness based on route bindings remain absent.

The [expected-host adapter](../../crates/platform/engine/src/expected_router.rs) owns this narrow response distinction.
Service readiness does not establish route-aware traffic removal.
The native route controller can reach Pods directly, so a readiness change alone does not remove every rebind outage.

## Native dispatch

One native application owns a loaded release or frozen candidate.
Native dispatch reuses compiled component bytes. Untrusted components receive a fresh store, which holds guest memory and resources, for each call.
The deployment owner can select reviewed component digests for native warm-instance reuse.
[Deployment configuration](../operations/deployment.md#trusted-component-reuse) owns this selection and pool sizing.
Application manifests cannot grant that trust.

An application is one composed component, and it loads as its own native workload.
No two applications share a store, and wash-runtime links no application to another.
Applications in one release can use different lifetimes.
A call across applications is a workflow call through the host.

Each call receives a new host-owned authority scope.
Capability calls use that scope, and cancellation revokes it independently of native store teardown.
A retained guest context cannot select the next call's permissions.
A call that leaves host resource handles open discards those resources and retires its instance while preserving its completed result.
The host never clears a resource table and then reuses that instance, because old handles can alias new resources.

Trusted component code owns request-local caller data, caller-dependent cache cleanup, and completion or cancellation of spawned tasks before return.
The host does not sanitize guest memory or enforce those promises against faulty or malicious trusted code.
Use fresh instances wherever that trust is unacceptable.
`maxConcurrency = 1` serializes calls on an instance. It does not isolate guest memory or background tasks.
One absolute deadline covers initialization and execution.
A guest cannot extend that deadline.

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
Caller authority ends immediately. Physical teardown follows native cancellation rules. The engine sets wash-runtime's abandoned-call grace to zero, so a cancelled call's store traps at its next epoch yield and returns its memory.
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

The [engine operation module](../../crates/platform/engine/src/operation.rs) runs one component export through native dispatch with `invoke_operation`.
It owns the deadline, workload loading, and invocation state.
It reaches the host through two traits: `ApplicationHost` gives the loaded application, and `InvocationPolicy` grants and revokes the authority of each call.
The [host operation module](../../crates/execution/host/src/operation.rs) implements both traits and owns authority.
The [driver](../../crates/execution/workflow/src/router_driver.rs) in `wamn-workflow` walks a wiring graph and calls the operation module for each node.
The [route path](../../crates/execution/host/src/route.rs) calls it once for a route target and never enters the driver.
A route reports a retryable or rate-limited error as the failure that a wiring reports after its last attempt.
The caller decides whether to send a new request.

Typed operation inputs and successful results contain owned lists or records.
Admission checks the context, node errors, and every nested value.
It refuses store-owned resources at these boundaries.
A composed call inside an application uses the typed entrypoint.
JSON adapters serve HTTP and dynamic routing.
The pure router, `wamn-router`, owns deterministic graph decisions.
After initialization, the host records caller, SQL, claims, causation, and effect authority in an invocation scope.
Native callbacks restore the trace context from that scope.
Cleanup revokes the scope and clears bindings after success, failure, cancellation, or owner shutdown.
The [queue](../../crates/execution/workflow/src/queue.rs) in `wamn-workflow` owns durable queue claims and settlement through the `RunStore` trait of `wamn-run-state`.
The queue owner binds these transactions to `wamn_run`, independently of the application data schema and database default search path.
HTTP admission and queue delivery use separate concurrency bounds, so one workload cannot consume the other's capacity.
Each host replica polls the durable queue and claims work through database leases; replicas need no wake service or process-local handoff.

The host binds the executing principal as `app.user_id` in each claims transaction that it opens.
[Record history](data-access.md#record-history) stamps that principal on every write.

- An authenticated route caller binds its principal id.
- A post-commit registration delivery binds `wamn:materializer`.
- An automation delivery binds its admitted service principal.
- A legacy queue delivery or management candidate case binds `wamn:executor`.

The same transaction binds the executing operation as `app.operation`.
A node binds the operation token of the entry that it runs. A composed callee runs under that token.
A participant statement records the participant operation and then restores the entry's token.
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
Items contain either `value` or `error`.
A write item retains its `request_id`. A read item carries none, and its outcomes match its items by position.
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
The optional `csrf` claim is the lowercase SHA-256 hex of a CSRF token. Only the cookie carrier sets it.
The password issuer binds the existing login record. PAT exchange binds the authenticated source PAT.
The `kid` selects a key only within the configured issuer.
Wrong issuer, organization, audience, algorithm, signature, or time bounds returns 401.
Unknown roles grant no implicit authority, and an empty permission set returns 403.

A browser presents the session token in the `__Host-wamn-session` cookie instead of the `Authorization` header.
The host reads that cookie only when the request has no `Authorization` header and the route admits a session.
A request that carries both refuses with 401, and so does a repeated session cookie.
The host verifies a cookie token as it verifies a bearer token, and then applies the CSRF check.
A cookie token without the `csrf` claim refuses with 401.
The CSRF check needs one `x-wamn-csrf` header whose SHA-256 hex equals the claim.
Today every route requires the header. `wamn-glgg` exempts read routes when the route kind reaches the serving manifest.

The token lifetime is at most 900 seconds, with 30 seconds of time tolerance.
Minting anchors expiry to the start of credential validation and records the actual signing time in `iat`.
A delayed mint cannot restart the lifetime.
The host reconsiders admission freshness after permission reads.
Age bounds limit delayed admission without promising that identity remains unchanged during a pause.

Fresh-only operations require PAT authentication. An entry whose call graph reaches a fresh-only operation is fresh-only.
A session that otherwise has permission receives `fresh-credential-required`.
The client does not replay that operation or silently replace its credential.
Authentication deadlines bound new admission and do not cancel work already accepted.

The [session token owner](../../crates/identity/session/src/token.rs) defines the token and time rules.
The [session verifier](../../crates/identity/session/src/verifier.rs) takes its keys from a key source: the issuer over HTTPS in the cloud, or a key file on the edge.
PAT exchange keeps no login record or renewal credential.
The current terminal client retains its session token only in memory.
The browser client holds no token. `@wamn/web-runtime` signs in with the cookie carrier and renews on load and before expiry.
Password sessions have no PAT fallback. Renewal happens only when an application request needs a new access token.
A local credential refusal occurs before HTTP submission and records a refused operation, not an unknown server outcome.
Failed renewal or login expiry requires explicit login and submission. Quitting clears local credentials and requests server logout.
[Terminal login](../operations/development-loop.md#receiving-password-login) describes configuration and prompts.
External federation and unbuilt identity design remain in the [identity plan](../plan/identity.md).

### Consistent session access

Sessions and PATs use the same operation permission checks, including the folded grant of an entry.
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

`/password/session`, `/password/renew`, and `/password/logout` accept an optional `carrier`, `"bearer"` by default or `"cookie"`.
With `"cookie"`, the reply body holds only `expires_at` and `login_expires_at`, and the tokens travel in three cookies.
`__Host-wamn-session` holds the session token. It is HttpOnly, with `Path=/`.
`__Host-wamn-csrf` holds a new random CSRF token. The page reads it, so it is not HttpOnly.
`__Secure-wamn-renewal` holds the renewal token. It is HttpOnly, with `Path=/password`.
All three are `Secure` and `SameSite=Strict` and carry no `Domain`.
The session and CSRF cookies live as long as the token. The renewal cookie lives as long as the login.
Renew and logout read the renewal token from its cookie and refuse a `renewal_token` in the body.
Logout clears all three cookies with `Max-Age=0`. `/password/logout-all` accepts only the bearer carrier.

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

The [screen plan](../../crates/schema/generator/src/client_plan.rs) holds the screen rules that every UI target applies.
It gives each callable operation one role: table, detail, form, delete, or none with a reason.
It also states the columns, the operator inputs, the row source, the paging, and the screens that one row opens.
It carries interaction meaning only, so no layout, styling, or framework concept enters it.
The generated terminal emitter reads the plan.
The terminal run time in `crates/client/tui` keeps its own copy of the same rules until a later epic moves it onto the plan.

A package can also generate [TypeScript bindings](../../crates/schema/generator/src/client_ts.rs) for a browser.
It opts in with a `client_package` name in its manifest, and a package that declares none generates no TypeScript. Nothing is published to a registry, and the name is what a workspace imports.
The bindings carry a request type, a result type, the published route and one function for each public operation.
A private event handler is absent, and an operation the release does not publish carries its types without a function.
The application supplies a transport that owns the URL and the credential.
That transport also classifies one response into a completion, a partial completion, a refusal or an uncertainty.
The contract stays snake_case on the wire, and the TypeScript members are camelCase.
The generator decides every member name and emits a field map beside each operation, so the browser holds no name rule.
A key that no map declares keeps its spelling, so the inside of a `json` value is never renamed.
A bounded list returns its rows under `rows`, and a page returns them under `item` beside a `nextCursor`.
An `int64` and a `numeric` are strings in TypeScript, because that is what the wire carries and a number loses precision above 2^53.
A field that declares a closed value domain types as a union of its literals.

The [web runtime](../../web/runtime/) is the hand-written package that the bindings import.
It is TypeScript source, it builds nothing, and a generated package declares it as its one dependency.
It implements the transport: the URL, the credential the application supplies, the request envelope, and the classification of one reply.
The rule it follows is `classify()` in `crates/client/tui/src/submission.rs`, which the terminal uses.
Both clients read one case table at `crates/client/tui/tests/data/classification-cases.json`.
The browser trusts the platform for the values inside a reply, so it holds no schema validator and no copy of the wire spelling rules.
A reply whose value violates its own field contract therefore reads as completed in the browser and as uncertain in the terminal.
The runtime also holds what a generated component calls: the state of one read, the members of a draft, and the display text of a declared value.

A package that generates TypeScript also generates [SolidJS components](../../crates/schema/generator/src/client_component.rs), one for each operation the plan gives a role.
A table renders the plan's columns over TanStack Table, with one control for each page control the plan names.
It appends the next page while the last reply carried a cursor.
A table that holds more than 100 rows renders only the rows in view of its fixed-height box. Paging does not change, and the table still holds every row it read.
A detail reads one record and shows its fields.
A form renders what the operator fills over TanStack Form, and it checks that input with an emitted `zod` schema.
It writes the reserved inputs from the runtime at submit time.
A delete asks for a confirmation first.
A form whose plan binds a revision reads the record when it opens and sends the revision of that read.
It reads nothing at submit, so a change that another writer makes in between refuses as a conflict.
A delete whose plan binds a revision takes the key and the revision of the record that the page displayed, and it reads nothing.
A command revision input can state `revision_of`, the input whose record it guards. The form then sends the revision of the row that carries the held key in that selector: the row the operator chose, a listed row that carries a filled key, or the record read for a key off the list. If no row carries the key, the form refuses locally and marks that selector.
A table row fills a form input only when that input is the one input of the form that names the row's model. If two inputs name it, the row fills neither.
A table column that names a record shows the record's text, not its key.
A generated result field states `references` from its column's foreign key, and an authored result field can declare it.
The plan binds that column to the model's served list, which states the display field, and to the record read that the list's rows open.
The table reads each key once, shows nothing while the read runs, and shows the key when no record comes back.
A column whose model serves no such list and read shows the key, and the emitted index names it.
A selector that holds a key its list did not return, such as one a row action filled from a later page, reads that record through the same read and shows its text.
A screen shows a refusal as a sentence, never as its code. The runtime holds one sentence for each platform and ingress code, and an application code reads as its words.
A refusal that names a field marks that control. A unique or foreign key over one field that the operation writes names that field in the refusal detail, beside the constraint.
A form shows a completed line beside its buttons after its command completes.
An operation whose shape has no role gets no component, and the emitted index names it with the reason.
A request type declares writable members, because a caller builds a request and a form library writes into it. A result type keeps its own read only.
Components state no route and no navigation: a row link is a callback, and the application decides what to open.

The [app shell](../../web/shell/README.md) places the components on routes, and the generator does not write it.
An application web page in `apps/<app>/web/` gives the shell a hand-written route table of screens, grouped by model.
The navigation names a model with one screen by the model alone, and it lists the screen labels only below a model with more than one screen.
A record page carries its key in the address, for example `pallets/<id>`, and a table row opens it through the row callback.
The shell owns the router. A screen gets the address values, the query values, `open(path)` and `close()` as props, beside the transport.
Each command form has its own route. A create or merge form opens from a button above its table, and an update form from a button above its record page.
A row that fills a form opens it with the filled values in the query, so a reload keeps them. A completed submission returns to the page that opened the form, and a refusal stays on the form.
A form whose revision no read supplies, such as `inventory.move`, reads the pallet that the address names and sends the revision that it read.
The first segment of every address is the environment audience, so a reload renews the cookie session with nothing in browser storage.
A screen address with no session asks for the password on that address, and it shows the screen after sign in.
The page calls the release under `/api`, and the proxy in front of it strips that prefix, so no page path meets a route template.

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
The normal operation checks also apply to the folded grant of each entry.
The legacy `fresh-only` restriction still refuses queued service callers. Human session support does not widen queued automation.
SIGTERM and SIGINT share the host cleanup budget. The budget retains the native drain and plugin shutdown allowances.
An aborted call loses its invocation authority, and native teardown releases its store.
After the lease expires, recovery takes a new lease generation. The old generation cannot complete the run.
The [runtime tests](../operations/running-tests.md#host-and-package-runtime) cover the retained host and package paths.

OpenTelemetry records invocation and effect spans, request timing, and delivery outcomes.
Operational logs use INFO, and detailed request traces go to Tempo.
The bounded router tap supplies the live view without durable node histories.
Each tap record names a route or a wiring. The host writes only version 2 of the record, and readers still read version 1.

The [deployment guide](../operations/deployment.md) owns probe ports, grace periods, and rollout commands.
The [native alignment page](native-alignment.md#current-limits) retains the unresolved operator retry and liveness limits.
