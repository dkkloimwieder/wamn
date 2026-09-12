# Platform capabilities

A capability is an interface through which a component accesses platform resources.
A binding connects a declared requirement to an authorized environment resource.
The guest names its requirement and logical input. The host owns endpoints, credentials, and deployment scope.

## Import admission

The code-owned [capability declaration](../../crates/platform/component-policy/src/lib.rs) defines the closed tenant import set.
It admits exact package and version pairs.
An absent pair refuses, without version ranges, normalization, or namespace guesses.
The platform's own native workloads use their separate publication authority.

| Package | Version | Security class |
| --- | --- | --- |
| `wamn:node` | `0.1.0` | Ambient |
| `wamn:postgres` | `0.1.0` | Effect |
| `wamn:connection` | `0.1.0` | Effect |
| `wasmcloud:blobstore` | `0.1.0` | Effect |
| `wasi:logging` | `0.1.0-draft` | Ambient |
| `wasi:io` | `0.2.12` | Ambient |
| `wasi:clocks` | `0.2.12` | Ambient |
| `wasi:random` | `0.2.12` | Ambient |

Ambient interfaces support execution without a declared external effect.
Effect interfaces require the admitted effect and binding facts.
Both admission and component modeling classify platform packages from this same declaration.
They classify the package independently of version, then enforce the exact admitted version.
A foreign namespace alone does not make an import an application dependency.

A guest-visible capability requires one pinned WIT contract and its corresponding host world.
The implementation translates failures to that contract's error types.
Its connection descriptor, publication mapping, binding coordinates, and plugin registration must agree.
Per-effect spans and latency series belong to the implementation.
The [native alignment page](native-alignment.md) records retained differences and their removal conditions.

Capabilities used only by the platform need no guest import or tenant admission row.
A new security class or unsupported method needs an explicit owner decision.
The existing code declaration records that decision and does not infer it from a guest's imports.

## Invocation and connection authority

The host records the original wiring, node occurrence, component digest, operation, and exact dependency path.
A guest cannot replace that identity through request fields.
The loaded release binds the admitted component and connection facts.
Nested calls retain their original caller and require the destination's authority.

The environment owns concrete endpoints, schemas, container names, prefixes, and credentials.
Authors declare portable requirement aliases and supported connection types.
The platform supplies the complete connection semantics.
Publication and activation refuse absent or incompatible bindings before executing effects.

Guest SQL, executor work, HTTP admission, and event materialization use distinct authority classes.
Private event binding files remain outside guest configuration, and workload configuration cannot replace them.
Credential generations remain separate from invocation identity.
A replacement generation must demonstrate live use before the previous generation can retire.
A resource or connection cannot silently cross a tenant, environment, or generation boundary.
The [database contract](data-access.md) owns SQL-specific authorization and transaction behavior.

## Outbound HTTP

The public contract is [`wamn:connection/http@0.1.0`](../../crates/platform/runtime/wit/deps/wamn-connection/package.wit).
A request names a requirement, method, relative path and query, headers, optional body, and optional idempotency key.
The guest does not supply an endpoint or credential.
The host resolves the admitted requirement and confines the request to that connection.

The transport pins the approved peer and compares the connected peer.
Its client identity includes the connection scope and credential generation.
It does not follow redirects or retry a request automatically.
Requests share bounded client, socket, and concurrent-request capacity.

The transport bounds request and response bodies to eight MiB.
It bounds headers to 100 entries and 32 KiB.
One request has a 30-second deadline.
The host preserves the distinction between refusal before dispatch and a lost response after dispatch.
An idempotency key alone creates no remote same-result guarantee.

Contract failures remain `unbound`, `incompatible`, `authority-denied`, `attestation-invalid`, `credential-unavailable`, `timeout`, or `transport`.
A lost response cannot become a successful result or an editable application refusal.
The [transport owner](../../crates/platform/runtime/src/plugins/connection_http/transport.rs) defines aggregate capacity and intake bounds.
Raw-socket admission does not define this capability's separate external address policy.

## Object storage

The current contract is `wasmcloud:blobstore@0.1.0`, implemented through `object_store`.
The environment owns endpoint, container, and prefix.
The guest supplies a key relative to that prefix.
Path confinement refuses a `..` segment without refusing a harmless substring.
Key construction does not double a separator.

A write commits only after bounded intake establishes a complete body.
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

PostgreSQL change capture publishes the frozen [event contract](../../apps/platform/events/wire/src/lib.rs).
Subjects retain `evt.<org>.<project>.<env>.<entity>.<op>`.
The declared organization, project, and environment jointly define stream isolation.
Length-prefixed names distinguish triples that plain underscore joining can confuse.
Source and advisory streams belong to that complete triple.

Provisioning credentials create the declared streams and durable consumers.
Provisioning compares existing configuration and refuses a mismatch.
Runtime publishers and materializers can publish, subscribe, and attach to declared consumers.
They cannot create, update, or delete streams or consumers.
Activation reads and compares complete stream and consumer configuration, including foreign sources, mirrors, filters, and delivery policy.

The event broker remains separate from the scheduler broker.
NATS replication remains separate from Kubernetes workload replicas.
The host carries the declared stream replica count and duplicate window into activation.
No runtime default substitutes for a missing declaration.
The [native declaration owner](../../crates/control/provision/src/events.rs) supplies the configurations used by provisioning and activation.

A materializer pulls bounded batches with explicit acknowledgements.
The declared acknowledgement wait and delivery limit govern broker redelivery.
The current consumer allows 64 pending acknowledgements, 64 messages per pull, four MiB per pull, and one waiting pull.
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
The guest cannot supply tenant, project, or environment authority.
Causation depth bounds recursion. The current materializer limit is 16.
These delivery rules do not create exactly-once application effects.

## Session role reader

`POST /session` exchanges a current human PAT for a bounded session token.
The request supplies the audience and cannot supply roles, tenant identity, or a database URL.
A mounted target declaration selects the exact audience, tenant, database, and dedicated `SessionRoleReader` credential.

That role reads active users and their role assignments within the selected tenant.
It cannot write application data, read permission tables, or use catalog and execution authority.
The query binds both tenant and principal explicitly.
The service reads roles afresh for every exchange.
An inactive user or empty role set refuses exchange.
The request permits five seconds and a 1,024-byte body.

Connections are lazy and retained only for active configured targets.
An unused target opens no connection.
A retained connection never transfers to another audience or tenant.
The [execution page](execution.md#caller-authentication) owns token acceptance and revocation bounds.

## PAT issuance

`POST /pats` requires a client certificate under the dedicated operator CA.
An accepted operator certificate can mint for an existing active human or service principal.
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

The [PAT client](../../services/ctl/src/pat_client.rs) requires HTTPS and complete explicit TLS inputs before resource writes.
Its base URL cannot contain user information, a query, or a fragment.
The request permits five seconds and a 4,096-byte response, with no redirect or retry.
If the connection is lost after issuance, the token can exist without a recoverable raw value.
The service retains only its hash.

## Registry credentials

Server pulls read one explicitly configured projected credential file.
The lookup requires an exact registry authority and a plaintext username and password entry.
It does not consult `DOCKER_CONFIG`, invoke helpers, normalize Docker Hub names, or accept identity tokens and base64-only entries.
The [reader](../../crates/platform/runtime/src/registry_credentials.rs) owns that narrow accepted form.

Client certificate loading and OCI transfer retain their WAMN artifact contracts.
The [native alignment page](native-alignment.md) names the conditions for replacing those native adapters.
