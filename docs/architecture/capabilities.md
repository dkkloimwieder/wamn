# Platform capabilities

A capability is an interface through which a component accesses platform resources.
A binding connects a declared requirement to an authorized environment resource.
The guest names its requirement and logical input. The host owns endpoints, credentials, and deployment target.

## Import admission

The code-owned [capability declaration](../../crates/platform/component-policy/src/lib.rs) defines the closed project import set.
It admits package and version pairs.
A `wamn:*`, `wasmcloud:*` or `wasi:logging` row names an exact version.
A `wasi:io`, `wasi:clocks` or `wasi:random` row names the family `0.2`.
The family admits `0.2.N` up to the ceiling, the highest `0.2` patch that the host's Wasmtime links.
The build reads the ceiling from the WIT of the linked `wasmtime-wasi`.
An absent pair refuses, without other version ranges, normalization, or namespace guesses.
An application component cannot import `wamn:postgres/client`, even with a grant, because it runs raw SQL outside the admitted statements.
The platform's own native workloads use their separate publication authority.

| Package | Version | Security class |
| --- | --- | --- |
| `wamn:node` | `0.1.0` | Ambient |
| `wamn:postgres` | `0.1.0` | Effect |
| `wamn:postgres` | `0.4.0` | Effect |
| `wamn:connection` | `0.1.0` | Effect |
| `wasmcloud:blobstore` | `0.1.0` | Effect |
| `wasi:logging` | `0.1.0-draft` | Ambient |
| `wasi:io` | family `0.2` | Ambient |
| `wasi:clocks` | family `0.2` | Ambient |
| `wasi:random` | family `0.2` | Ambient |

Ambient interfaces support execution without a declared external effect.
Effect interfaces require the admitted effect and binding facts.
Both admission and component modeling classify platform packages from this same declaration.
They classify the package independently of version, then enforce the admitted version or family.
A foreign namespace alone does not make an import an application dependency.

A guest-visible capability requires one pinned WIT contract and its corresponding host world.
The implementation translates failures to that contract's error types.
Its connection descriptor, publication mapping, binding coordinates, and plugin registration must agree.
Per-effect spans and latency series belong to the implementation.
The [native alignment page](native-alignment.md) records retained differences and their removal conditions.

Capabilities used only by the platform need no guest import or project admission row.
A new security class or unsupported method needs an explicit owner decision.
The existing code declaration records that decision and does not infer it from a guest's imports.

## Invocation and connection authority

The host records the original wiring, node occurrence, component digest, operation, and exact dependency path.
A guest cannot replace that identity through request fields.
`InvocationScopes` holds the trusted authority of each workload and invocation.
Each record selects an immutable release and the connection reader of its project deployment.
HTTP, blob storage, route authentication, and integration imports use that selected authority.
A shared component digest shares code only. It grants no credential or project access.

Each project deployment owns one `ConnectionFacts` instance across its loaded releases.
Its generation, secret, and storage client maps stay separate from those of other projects.
Each release keeps its own binding facts under its digest.
A missing or revoked scope refuses the call before an external effect.
Native file handlers and file collection use the same trusted deployment context.
Nested calls retain their original caller and require the destination's authority.

The environment owns concrete endpoints, schemas, container names, prefixes, and credentials.
Authors declare portable requirement aliases and supported connection types.
Wiring validation refuses string values with the existing secret prefixes: `Bearer `, `-----BEGIN`, and `AKIA`.
It inspects nested values, including case inputs, without copying rejected values into errors.
Reference names remain valid. This pattern test cannot identify every arbitrary secret.
The platform supplies the complete connection semantics.
Publication and activation refuse absent or incompatible bindings before executing effects.

An administrator creates each project integration and its environment connection in the system database.
The connection stores its active generation of parameters and a reference to private credentials.
The environment database holds a projection of each integration and connection.

A package declares each connection name, WIT interfaces, and access mode in `wamn.k`.
Install refuses a declared interface that the component does not import.
An environment document maps these names to integrations and selects each integration version.
Several declared names can use the same integration.
Apply refuses missing or inactive connections and versions that do not export every required interface.
Interface versions follow component-model compatibility rules.
Apply writes no connection generation and changes no connection lifecycle.

The host reads the loaded release's names, modes, and versions from the environment catalog.
The system database supplies each selected Wasm digest and the active connection parameters.
The host refuses a name that the calling component did not declare.
HTTP method authorization and object-store access enforce a declaration's read mode.

The host resolves private credentials through the environment store.
Each lookup requires the trusted organization, project, and environment target.
Credential changes use the current generation without a host restart.
The existing refresh behavior remains in place.

Guest SQL, queue maintenance, HTTP admission, and event materialization use distinct authority classes.
Private event binding files remain outside guest configuration, and workload configuration cannot replace them.
Credential generations remain separate from invocation identity.
A replacement generation must demonstrate live use before the previous generation can retire.
A resource or connection cannot silently cross a project, environment, or generation boundary.
The [database contract](data-access.md) owns SQL-specific authorization and transaction behavior.

`wamn:postgres` binds no connection instance, so it has no connection generation.
The host selects the one project-environment database from its trusted project.
Each authority class uses one login and one basic-auth Secret. CloudNativePG manages the login. The cluster, its consumers and its Secrets share the org namespace. Component configuration names the database, login, host and Secret references. The shared SQLx constructor verifies the server certificate with the cluster CA.

## Outbound HTTP

The public contract is [`wamn:connection/http@0.1.0`](../../wit/wamn-connection/package.wit).
A request names a requirement, method, relative path and query, headers, optional body, and optional idempotency key.
The guest does not supply an endpoint or credential.
HTTP and object storage use one authority resolver.
It reads component declarations and connection modes from the loaded release.
A configured palette declaration keeps its parameter reference.
Publication seals its modes by package and alias beside that declaration.
The resolver considers bindings within the admitted application.
Shared aliases must select the same connection, version and mode.
It reads the workflow position and node configuration from the admitted invocation.
Connection memory supplies the binding, status and current generation.
A configured alias must match the node configuration.
The host confines the request to that connection.

The `http-request` palette node sends one request through this contract.
Its `method` stays a fixed parameter.

The optional `path-input-pointer` selects the request path from the input through a JSON pointer, a path into a JSON value.
The required `path-and-query` parameter stays declared. The pointer overrides its value for that request.
Without the pointer, `path-and-query` supplies the path.
Both forms require a relative absolute-path that starts with `/`, but not `//`.
A missing target or a target that is not a string returns a terminal error.

The optional `body-input-pointer` selects the JSON body from the input.
A missing body target returns a terminal error. JSON null is a valid body.
Without that parameter, the node sends the original input bytes as its body.
Headers retain their configured values in both forms.

The `jsonata` node accepts optional `parameters`, an object bound to `$parameters` during evaluation.
The input remains unchanged.
Its optional `port-input-pointer` selects `main` or `items` from the expression result through a JSON pointer.
A missing target, a non-string target, or an unknown port returns a terminal error.
Without that pointer, the existing `port` parameter supplies the output port.
The `items` output still requires an array.
The `items` input port accepts an array with the same handler as `input`.
Edges into either input port name that port explicitly.

The `http-page` node emits one array on `main` after it collects all pages.
Its parameters are `requirement`, `path-and-query`, optional `headers`, `page`, `items`, and optional `carry-input-pointer`.
The optional `method` defaults to GET and accepts GET, HEAD, POST, PUT, DELETE, OPTIONS, and PATCH, as `http-request` does.
The optional `body-input-pointer` selects a value from the node input through a JSON pointer, a path into a JSON value.
Each request sends that value as JSON with `Content-Type: application/json`, which replaces any configured content type.
A missing body target returns a terminal error. A selected JSON null is a valid body.
When `carry-input-pointer` is set, the output is `{"items":[…],"carry":<value>}`.
The JSON pointer selects the carried value from the node input, including for an empty item array.
A missing target returns a terminal error.
Without that parameter, the output remains the plain item array.
The `items` parameter is a JSON pointer to each response's item array.
Offset paging uses `page = {"style":"offset","limit":"limit","offset":"offset"}` and reads the positive limit from `path-and-query`.
A short page ends offset paging.
The offset and limit remain query parameters for POST requests, including an initial `offset=0` when no offset is supplied.
An optional `page.has-more`, for example `"/hasMore"`, replaces the short-page rule with a boolean from each response.
False ends the loop after that page, without another request. True continues even after a short page.
A missing or nonboolean value returns an `invalid-page` terminal error.
Cursor paging uses `{"style":"cursor","pointer":"/next","param":"cursor"}` and stops on an absent, null, or empty cursor.
Link paging uses `{"style":"link-header"}` and follows relative links with `rel="next"` unchanged.
Absolute next links return a terminal error, because the guest does not see the binding's origin (`wamn-62h0t`).
A 429 returns `rate-limited` with numeric `Retry-After` seconds converted to milliseconds.
A 5xx returns `retryable`. Other 4xx responses, including 408, return `terminal`.
The [REST connector](../../apps/rest_connector/README.md) carries the schedule due time and maps items with JSONata.
It calls the registered command of the separate consumer application, then advances its checkpoint after success.

The integration selects its authentication scheme with `auth_type`.
The credential store holds secret fields. The connection generation holds public authentication fields under `parameters.auth`.
The host reads both objects and applies the selected scheme.

| `auth_type` | Credential | `parameters.auth` |
| --- | --- | --- |
| `http_basic` | `{"username":"…","password":"…"}` | `{}` |
| `api_key` | `{"key":"…"}` | `{"in":"header","name":"x-api-key"}` or `{"in":"query","name":"api_key"}` |
| `hmac` | `{"secret":"…"}` | `{"algorithm":"sha256","header":"x-signature"}` |
| `oauth2_client_credentials` | `{"client_secret":"…"}` | `{"client_id":"…","token_url":"…","scopes":["…"]}` |
| `oauth2_client_credentials_jwt` | `{"private_key":"PEM"}` | `{"client_id":"…","token_url":"…","scopes":["…"],"key_id":"…","algorithm":"PS256","scope_separator":","}` |
| `aws_sigv4` | `{"access_key_id":"…","secret_access_key":"…"}` | `{"region":"…","service":"…"}` |
| `custom` | An object defined by the integration implementation | `{}` |
| `none` | No credential | `{}` |

HTTP Basic authentication sends the username and password in `Authorization`.
An API key uses the declared header or query parameter.
AWS SigV4 signs the request for the declared region and service.
The `custom` scheme gives the credential to the integration implementation through `bound.credential()`.
Other schemes keep credentials inside the host. The `none` scheme applies no authentication.

HMAC is a signature that uses a shared secret.
The host signs the exact outbound body bytes and injects `sha256=<lowercase hex>` in the declared header.
The secret and header name must be nonempty, and the name must be valid and not reserved by the host.
The HMAC shape refuses unknown fields and algorithms other than `sha256`.

The `oauth2_client_credentials` type uses a client secret from the store and public fields from `parameters.auth`.
The `scopes` field is an array of strings.
The host requests a client-credentials token with HTTP Basic authentication and a form body.
The token endpoint stays inside the bound authority and base path and uses the same transport and address policy.
The host caches tokens for the connection and active credential generation until `expires_in` minus 60 seconds.
Missing expiry, expiry at most 60 seconds, or an unrepresentable expiry permits only the current send.

The `oauth2_client_credentials_jwt` type authenticates with a signed JWT, a JSON Web Token, instead of a client secret.
The credential holds a PEM private key. RSA accepts PKCS#8 or PKCS#1, and P-256 accepts PKCS#8.
The host supports PS256, RS256, and ES256. ES256 uses the fixed 64-byte signature format.
The assertion header carries `alg`, `kid` from `key_id`, and `typ` set to `JWT`.

The JWT claims carry `iss` from `client_id`, `aud` from `token_url`, and `iat` from the host clock.
The expiry, `exp`, is 300 seconds later. Each assertion carries a fresh random 256-bit `jti`.
Optional `scopes` form the `scope` JWT claim and the form field. Empty scopes omit both.
The `scope_separator` accepts a space or comma and defaults to a space.

The token form carries `grant_type=client_credentials`, the RFC 7523 `client_assertion_type`, and `client_assertion`.
Malformed keys or keys that do not match the algorithm return `credential-unavailable` before dispatch.
Connection health uses the same assertion request and token response rules.
Both client credentials types use the shared encrypted token row and locked refresh path described in [execution](execution.md#connection-memory).

The host injects `Authorization: Bearer` into the API request.
An API 401 removes the cached token and causes one fresh token request and one API retry.
A second 401 returns to the guest and removes that token from the cache.
If the fresh token request fails, the guest receives the first API 401.
For every authentication type, an API 401 or 403 drops the secret and the connection fact, and the host reads both again.
The host sends once more only when the new fact names another generation or credential, so a repeated send never carries the refused credential.
A call makes at most one extra send, so an OAuth2 retry after a changed fact is that one send.
A token failure before any API request returns `credential-unavailable` without the token endpoint's response body.
The guest receives no credential. The workflow queue owns other retries.

The transport refuses unspecified, loopback and link-local addresses, including mapped IPv4 addresses.
It pins the approved peer and compares the connected peer.
Its client identity includes the connection target and credential generation.
It does not follow redirects or retry a request automatically.
Requests share bounded client, socket, and concurrent-request capacity.

The transport bounds request and response bodies to eight MiB.
It bounds headers to 100 entries and 32 KiB.
One request has a 30-second deadline.
The host preserves the distinction between refusal before dispatch and a lost response after dispatch.
An idempotency key alone creates no remote same-result guarantee.

Contract failures remain `unbound`, `incompatible`, `authority-denied`, `attestation-invalid`, `credential-unavailable`, `timeout`, or `transport`.
A lost response cannot become a successful result or an editable application refusal.
The [transport owner](../../crates/platform/http-capability/src/transport.rs) defines aggregate capacity and intake bounds.

ConnectionHttp confines requests through administrator-bound connections.
RFC1918 and cluster service addresses are allowed because the administrator bound the target.
The host denies IPv4 and IPv6 link-local addresses and the IPv6 cloud metadata address `fd00:ec2::254`.
The IPv4 link-local range includes cloud metadata at `169.254.169.254`.
The same rules apply to IPv4 addresses mapped into IPv6.

After DNS resolution, the host selects an allowed address and pins that exact peer before transport use.
A later request resolves again and refuses if no allowed address remains.
Redirect responses return to the guest without a second request, including redirects into denied ranges.
Raw-socket admission owns a separate, stricter address policy.

## Object storage

The current contract is `wasmcloud:blobstore@0.1.0`, implemented through `object_store`.
The environment owns endpoint, container, and prefix.
The guest supplies a key relative to that prefix.
Path confinement refuses a `..` segment without refusing a harmless substring.
Key construction does not double a separator.

A write commits only after bounded intake establishes a complete body.
Guest writes and native file uploads accept at most 64 MiB.
Native uploads stream through multipart storage and abort unfinished uploads on cancellation.
A storage handle retains its connection generation.
Each operation refuses a disabled connection or a changed generation before calling storage.
The host shares storage clients by connection and generation in a bounded cache.
Range reads fetch only the requested bytes.
A partial stream, over-limit body, or cancellation cannot publish a partial object.
The contract's own error variants carry backend failures.

`copy-object` and `move-object` refuse because their bare object identities cannot preserve cross-binding confinement.
Container creation and deletion remain environment policy.
The current backend cannot supply the creation time required by `info`.
It refuses instead of inventing a timestamp.

The shared blob writer requires the caller's deterministic key.
Repeated delivery overwrites that same object and does not invent another key.
Its asynchronous import requires the asynchronous node export.
The [components page](components.md#shared-label-renderer) describes its composition with label rendering.

## Events

PostgreSQL change capture publishes the [event contract](../../crates/events/wire/src/lib.rs).
The wire owner decodes records and verifies their source identity.
Records carry `"type": "cdc"` or `"type": "derived"` and use underscore field names.
Subjects use `evt.<org>.<env>.<project>.<entity>.<op>`.
The declared organization, project, and environment jointly define stream isolation.
Length-prefixed names distinguish triples that plain underscore joining can confuse.
Source and advisory streams belong to that complete triple.

Provisioning credentials create the declared streams and durable consumers.
Provisioning compares existing configuration and refuses a mismatch.
Runtime publishers and event consumers can publish, subscribe, and attach to declared consumers.
They cannot create, update, or delete streams or consumers.
The constructor compares declared stream fields, including foreign sources, mirrors, filters, and delivery policy.
The consumer test compares only the fields wamn sets; the broker owns every other consumer field, and the test does not compare it.

The platform runs one NATS for the scheduler, the events and the host memories.
Each client presents a certificate, and the server maps it to a NATS user.
An environment's user reaches only its own events, stream consumers and memory keys.
Only the provisioning client creates, updates or deletes streams and consumers.
NATS replication remains separate from Kubernetes workload replicas.
The host carries the declared stream replica count and duplicate window into activation.
No runtime default substitutes for a missing declaration.
The [native declaration owner](../../crates/broker/src/events.rs) supplies the configurations used by provisioning and activation.

The native event transport awaits bounded fetches and acknowledges each message after workflow admission commits.
Application consumers retry deliveries without a limit, with a 30 second acknowledgement wait.
Each consumer permits 64 pending acknowledgements, 64 messages per pull, four MiB per pull, and 64 waiting pulls.
Successful settlement acknowledges delivery. Permanent rejection terminates it.
A disconnect before acknowledgement permits redelivery and requires application idempotency.

The broker retains exhaustion and termination advisories for seven days, with 1,000 records per advisory subject.
Payload bytes remain in the separately retained source stream.
Source expiry can remove payload while its advisory remains.
An advisory observation does not pretend that an expired payload is available.

An observer connects with that environment's credentials.
It can read only that environment's advisory metadata and any separately authorized source payload.
Observer, runtime, and provisioning credentials remain distinct.
The unauthenticated NATS HTTP monitor binds loopback and has no Service exposure.
Administrator process access remains separate from environment observation.

Derived events use the host's admitted terminal, logical deduplication input, and bound causation.
The guest cannot supply `project_env_key`, project, or environment authority.
Causation depth bounds recursion. Workflow refuses an event above depth 16.
These delivery rules do not create exactly-once application effects.

## Session role reader

`POST /password/session` signs in a user and returns a bounded session token.
The request supplies the audience and cannot supply roles, an `project_env_key`, or a database URL.
A mounted target declaration selects the exact audience, `project_env_key`, database, and dedicated `SessionRoleReader` credential.

That role reads active users and their role assignments within the selected project.
It cannot write application data, read permission tables, or use catalog and execution authority.
The query binds the principal explicitly. The target selects its environment database.
The service reads roles for each login.
An inactive user or empty role set refuses login.

Connections are lazy and retained only for active configured targets.
An unused target opens no connection.
A retained connection never transfers to another audience or `project_env_key`.
The [execution page](execution.md#caller-authentication) owns token acceptance and revocation bounds.

## PAT issuance

`POST /pats` requires a client certificate under the dedicated operator CA.
An accepted operator certificate can mint for an existing active service principal.
A PAT, session JWT, or caller identity header cannot grant issuance authority.
Absent operator trust roots refuse issuance.

The request contains exactly `principal_id`, `label`, and integer `lifetime_seconds`.
Unknown or repeated fields and bodies above 1,024 bytes refuse.
A trimmed label contains 1 through 200 bytes.
The lifetime is one second through 365 days.
Issuance creates no principal, membership, or role.

The 201 response contains `token`, `token_prefix`, `principal_id`, `created_at`, and `expires_at`.
Times use UTC RFC 3339 and the response uses `Cache-Control: no-store`.
The raw token appears only in that response and never in logs.

Provisioning uses this service even for the first PAT.
It requests 30 days and authenticates the returned token as the expected principal.
It writes the credential atomically to a mode-0600 file.
There is no database issuance fallback or credential output on stdout.

The [PAT client](../../crates/identity/client/src/lib.rs) requires HTTPS and complete explicit TLS inputs before resource writes.
Its base URL cannot contain user information, a query, or a fragment.
The request permits five seconds and a 4,096-byte response, with no redirect or retry.
If the connection is lost after issuance, the token can exist without a recoverable raw value.
The service retains only its hash.

## Registry credentials

A release-backed host takes exactly one registry credential: `--registry-auth-file` or `--registry-token-metadata`.
With `--registry-auth-file`, server pulls read one explicitly configured projected credential file.
The lookup requires an exact registry authority and a plaintext username and password entry.
It does not consult `DOCKER_CONFIG`, invoke helpers, normalize Docker Hub names, or accept identity tokens and base64-only entries.
With `--registry-token-metadata`, the host reads no credential file.
Each pull gets its credential from the shared pod metadata service and sends it as the password of `oauth2accesstoken`.
Registry, GCS and KMS share that service's token and account email.
The service serializes token refreshes and stops reusing a token thirty seconds before expiry.
Each pull builds a new upstream registry client because that client remembers its first credential.
The server's host is `GCE_METADATA_HOST` when it is set, as Google's client libraries read it, and `metadata.google.internal` otherwise.
The [reader](../../crates/catalog/package-artifact/src/registry_credentials.rs) owns both accepted forms.
The artifact crate also owns the shared registry client configuration, trust roots and release artifact transfer.
Platform endpoint calls share its Hyper client. They use explicit trust, bounded bodies and request deadlines, without proxies or redirects.
The upstream transports inside `oci-client` and `object_store` stay with those libraries.

The host image also carries [`docker-credential-wamn`](../../crates/platform/component-artifact/src/bin/docker-credential-wamn.rs), a Docker credential helper for wash-runtime's own pulls.
It answers `get` with the same token function, and supports no other command.
A `config.json` that names it under `credHelpers` holds no credential.

Client certificate loading and OCI transfer retain their WAMN artifact contracts.
The [native alignment page](native-alignment.md) names the conditions for replacing those native adapters.

- The kind clusters keep the projected docker configuration file for their in-cluster registry. Only the GKE host changes.

- The operator verbs of `wamn-ctl` (`push-package`, `publish-release`, the delivery verbs) keep `--registry-auth-file`. They run by hand, outside the cluster.

- A host that is set up for the metadata server and cannot get a token refuses the pull with a named outcome. It never falls back to an anonymous pull.

The host gets its registry token from the GKE metadata server: `GET http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token` with `Metadata-Flavor: Google`. The token is the `access_token` field. The registry credential is `oauth2accesstoken` and that token, the form Artifact Registry accepts and the CronJob writes today. The shared metadata service supplies the token. It never writes tokens to a file or a log.

A ConfigMap mounted at `DOCKER_CONFIG` holds `{"credHelpers":{"us-central1-docker.pkg.dev":"wamn"}}`. The ConfigMap holds no credential. wash-runtime runs the helper at each pull.

- The host's existing Google service account, `wamn-blob`, gets `roles/artifactregistry.reader` on repository `wamn`. A pod has one Workload Identity binding, and the host needs both roles. The name `wamn-blob` now understates the account. Renaming it is not this finding.

- `wamn-edge`. It pulls nothing from a registry.
