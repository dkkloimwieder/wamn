# WAMN architecture

WAMN runs application operations as WebAssembly components on wasmCloud.
A wiring connects those operations into a route.
This file is the single current architecture overview.
Beads and Git own work status.
The [build and test runbook](operations/build-and-test.md) owns runnable commands.
The [deployment runbook](../deploy/README.md) owns installation and rollout procedures.

## Applications

Each application lives under `apps/<exact-package-id>/`.
Its home contains its manifest, migrations, authored SQL, generated output, components, operator application, and tests.
Receiving, WMS, and the Acme Receiving overlay have separate homes.
Each application README maps its declared package to its crates and components.
The [application naming contract](architecture/application-naming.md) defines package, operation, schema, and generated identifier spelling.

The repository root owns the native Cargo workspace.
Native application UI and test crates belong to that workspace through explicit members.
`apps/Cargo.toml` owns the guest workspace, and `apps/platform/` holds shared guests and guest libraries.
The separate `apps/platform/no-std/` workspace keeps its feature requirements isolated.
Cargo owns package membership, dependencies, features, and targets.
Application manifests own component declarations.

The Receiving operator combines generated screens with application-owned Rust.
Its UI crate owns the entrypoint, and its generated UI crate supplies a library.
The generator does not create a second Receiving launcher.
WMS still declares the `wamn-wms-tui` binary in its generated UI crate.
The native development command selects operators from their Cargo declarations.

Application-specific generation, SQL, terminal, and deployed tests stay with their application.
Shared native tests live under `tests/`, with common setup in `test-support/`.

Developers publish components with declared operations, ports, effects, and connections.
Users compose admitted components into versioned wirings.
Admission makes sure that every imported capability belongs to the platform's explicit supported set.
It permits only the named WASI interfaces and grants no `wasi:*` wildcard.
A component's effect permissions include effects reachable through its declared operation dependencies.
The [component artifact contract](architecture/component-artifact-boundary.md) defines the publication boundary.

## Generation

Migration SQL defines the application schema.
PostgreSQL applies the selected base and overlay migrations.
`wamn-schema-introspection` reads that database into a normalized schema description.
`wamn-schema-generator` produces package artifacts from that description, the exact manifest and SQL bytes, and declared provenance.
Its core transformation performs no filesystem, database, clock, or environment access.
The generated schema description is not an editable alternative to migrations.

Generated artifacts include typed accessors, registered operations, static SQL, client contracts, and UI screens.
The same SQL files feed native SQLx compilation and guest execution.
Native application tests keep their SQLx metadata under their own `tests/.sqlx/` directories.
Guests send statement identities and arguments through `wamn:postgres`.
The host resolves those identities to the exact admitted SQL bytes.
The [SQLx data-access contract](sqlx-data-access-spec.md) defines the shared SQL and type requirements.

Commands keep one transaction on one PostgreSQL connection.
A transaction resource does not cross a wiring edge.
For operations declared `per_input`, each outer input item owns its transaction.
The host owns statement authority, execution, commit, rollback, and connection cleanup.
Cancellation destroys an unfinished connection before another request can acquire it.
Guests receive no database socket or credential.

Generated package metadata binds schema requirements, platform policy requirements, SQL identity, and generator provenance.
Admission retains those facts with the component's immutable operation declarations.
Generated operations pass the same admission and caller-permission rules as authored operations.
Tenant-authored arbitrary SQL remains subject to runtime authorization and execution limits.

## Control and releases

`wamn-control-registry` owns organization, project, and environment declarations.
`wamn-control-provision` builds provisioning statements and native broker declarations.
`wamn-schema-control` owns package migration decisions and run storage declarations.
`wamn-catalog` owns component, wiring, and release facts.
`wamn-ctl` performs database and broker operations through these libraries.
`wamn-scenario-worker` serves the authoring admission API.

`apply-package` is the sole migration applier.
Rust decides package registration and wiring activation from the stored facts within the caller's transaction.
PostgreSQL enforces uniqueness, row isolation, immutable records, release sealing, and the required locks.
Package registration retains its lineage lock, exact retry behavior, and first timestamp.
Platform schema installation targets fresh databases, including the control database.
Application migrations remain ordered package inputs.

`wamn-schema-control` compares projected release identities and deployment attestations.
`wamn-ctl` writes them and compares the stored winner within the same transaction.
Exact retries retain the first timestamp and nullable source provenance.
The provisioning owner locks and compares environment identity before it refreshes the tenant projection.
That refresh clears the old instance claim, including when the suffix stays unchanged.
The developer coordinator claims an instance through one update and refuses a missing tenant projection.

`wamn-run-state` supplies the query for actual `CURRENT_USER` role membership.
The native runtime refuses executor operations when that membership is absent.
It translates those refusals at the existing guest error boundary.

A release binds exact package versions, component digests, operation dependencies, statements, connections, and wiring versions.
Sealing prevents later changes to that release's content.
Promotion compares target package manifests and ordered migration records, copies admitted facts, and admits the target wirings.
It records activation without changing the immutable release content.
Exact retries preserve their original facts, and conflicting content refuses.

Publication produces an immutable release manifest addressed by its digest.
Deployment supplies that digest and artifact location to the host and executor.
A deployment rollout changes the serving release.
Runtime requests resolve wiring facts from that supplied release and do not follow mutable database activation pointers or PostgreSQL notifications.
Frozen candidate execution retains its own exact admitted facts for the full traversal.
The [deployment runbook](../deploy/README.md) defines publication, rollout, and rollback.

Runtime database credentials belong to a tenant and project environment.
PostgreSQL uses the authenticated database role for application row isolation.
The host selects credentials for guest SQL, executor work, HTTP admission, and event materialization through their distinct authority classes.
Guest input cannot select a database identity.
Control-store projections retain their separate tenant-scoped row policies.

## Request execution

`wamn-host` accepts HTTP traffic, and `wamn-executor` processes queued work.
Both use `wamn-execution-host` and the same native router.
A hot HTTP request creates no run or queue row.
Route concurrency limits refuse excess work with HTTP 429.
The executor retains queue claim, lease, and expiry handling.
Production automation admission remains unimplemented.

The router walks the admitted wiring and routes each output to another node, a response, an emission, or discard.
A hop limit bounds the walk.
Edges carry arrays of request identifiers with values or errors.
Connected ports must have identical canonical schema digests.
Nodes transform values and preserve error items.
Multiple-input targets require an explicit `to_port`.

Synchronous nodes export `wamn:node/handler`.
Nodes that await asynchronous capabilities export `wamn:node/async-handler@0.1.0` with an asynchronous lift.
The P3 HTTP shell separately exports `wasi:http/handler@0.3.0`.
Native wasmCloud loading, linking, and dispatch serve released, nested, and candidate calls.
Each imported operation interface has one admitted provider across the component closure, including its interface version.
Repeated export-only interfaces do not create that uniqueness requirement.

An admitted release retains one native application for its component set.
A candidate retains one for its traversal.
Native code reuses compiled component bytes, while every invocation receives a fresh store.
Complete admitted authority facts remain separate from compiled bytes.
Positive `poolSize` remains refused.
One absolute deadline covers initialization, execution, and nested calls, and children cannot extend it.

After initialization, the host registers an invocation scope containing the caller, statements, claims, causation, and effect authority.
Native callbacks use that scope to restore the host trace context.
Cleanup revokes the scope and clears bindings after success, failure, cancellation, or owner shutdown.
Nested effects require the released root's exact declared dependency path and the executing operation's authority.
Release membership alone grants no nested access.
Candidate execution still refuses nested calls.

Personal access tokens, or PATs, identify callers through fresh database reads.
Each PAT request reads current identity, membership or service scope, and operation permissions.
The separate `wamn-identity` service issues PATs and signed session tokens.
Session tokens carry identity and environment roles for at most 900 seconds, with 30 seconds of clock tolerance.
Hosts retain public signing keys only within the configured issuer's 300-second freshness window.
They read operation permissions fresh for every new request.

The [session authentication contract](architecture/wamn_jwt_proposal.md) defines the token and key rules.
An operation declared `fresh-only` requires a PAT, including across nested calls.
A permitted session caller receives `fresh-credential-required` for that operation.
The host does not repeat earlier committed work or retry the refusal under another credential.
Private signing keys remain with the identity service and its system database.

A declared committed result lets the router retain a successful operation result when later work fails.
The actual output must satisfy the admitted commitment contract.
Missing or ambiguous observations leave the outcome unknown.
A served partial error carries `committed_result` and `failed_outcome` without creating stored node history.
Generated clients make sure that this response matches its declared schema and request identity.
Partial completion does not authorize replay of a composed route.

## Operational boundaries

PostgreSQL change capture feeds the event plane through `wamn-cdc-reader`.
The native materializer attaches through its platform-owned `events` binding.
Provisioning creates source streams, advisory streams, and declared consumers for each organization, project, and environment.
Those three coordinates define the isolation boundary, separately from tenant and database identities.
Stream names encode each coordinate's length and value to avoid ambiguous joins.

Activation compares complete stored stream and consumer configurations with their declarations and refuses disagreement.
The declared NATS replica count remains separate from the Kubernetes workload replica count.
Runtime credentials cannot create, change, or delete streams or consumers.
Materializer credentials permit only their declared delivery, acknowledgement, and private reply subjects.
Publishing and monitoring use separate credentials limited to the same environment.
Private binding files stay outside guest configuration, and workload configuration cannot replace them.

The materializer acknowledges only after completion.
Delivery limits bound retries, pending acknowledgements, and pull sizes.
Each source stream has its own advisory stream for exhausted and terminated deliveries.
Monitoring cannot read another environment's metadata or source payload.
Source retention can remove payloads while the corresponding advisory remains available.
The unauthenticated NATS HTTP monitor listens on loopback without a Service endpoint.

Default delivery is at least once, so applications must handle repeated work safely.
Producer keys and explicit deduplication identifiers suppress repeats only at their declared boundaries.
The premium durable effect protocol remains shelved and is not a default execution guarantee.
Its rule forbids redispatch after a recorded attempt with no recorded outcome.
A configured durability class alone cannot supply that protection.

Outbound HTTP uses one bounded process-owned transport with an authorized, pinned destination address.
Each request authorizes its caller, release, binding, credentials, and destination before connection reuse.
Reusable clients remain separate by authority, credential generation, destination, and TLS identity.
The transport limits connections, concurrent requests, response sizes, and duration, and performs no internal retry.
A timeout or cancellation does not establish that the remote operation did nothing.

OpenTelemetry records invocation and effect spans, request timing, and delivery outcomes.
Operational logs use INFO, and detailed request traces go to Tempo.
Effect observations distinguish refusal before dispatch, response, timeout, cancellation, `effect-uncertain`, and `response-lost`.
An uncertain sent effect cannot be sent again under the premium protocol.
A lost response requires reading the result again.
The bounded router tap supplies the live view without durable node histories.

Platform services use OCI images and operator-managed deployments.
The deployment runbook separates infrastructure installation from each environment's host release.
The event broker remains separate from the scheduler broker.
Native WAMN contract identifiers and generated artifact identities retain their declared spelling.
The release manifest keeps its current integer format and OCI media type without compatibility layers for historical development formats.

Application tests call ordinary Rust setup and assertion functions.
Thin lifecycle scripts perform explicit container, cluster, and image operations.
Tests that require external services report missing inputs, skips, failures, and cleanup separately.
A skipped or unexecuted case is not a passing execution.
Raw run data belongs under `evidence/`, and active instructions link to the owning commands and contracts.
