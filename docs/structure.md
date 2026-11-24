# Crate structure

These rules cover the root workspace and the `apps/` workspace. Crate boundaries and `pub(crate)` limit what other crates can use. Code review enforces the rules.

## Roles

| Role | Crates | Dependencies and responsibilities |
| --- | --- | --- |
| Portable | digest, registry, package, role, principal, scalar, routes, identity-protocol, connection-type, component-policy, event, workflow-graph, schema, number-pattern, oci-reference, application, edge-document, envelope, jsonata, guest, record-history | Portable crates can link other portable crates. They must link no input or output library. They must never access files or the network. |
| Infrastructure | postgres kernel, kubernetes, oci, broker, http-client, storage, gcp-metadata, key, identity-client | Infrastructure can link portable crates, other infrastructure crates, and its own input or output library. It can hold WAMN identifiers and rules for storage or protocols. It must leave domain decisions and lifecycle steps to domain crates. |
| Runtime domain | engine, plugin, authority, http, blobstore, connection, authentication, run-state | Runtime domain crates can call portable, infrastructure, and runtime domain crates directly. They can link SQLx and the host libraries `wash-runtime`, `wasmtime`, `hyper`, and `reqwest`. They own their SQL. |
| Deployment domain | environment, platform-install, release, build, codegen, identity, provision, application-chart, dev | Deployment domain crates can call any crate except a binary. They can link SQLx and own their SQL. |
| Binary | Runtime binaries are host, edge, web, and cdc-reader. Deployment binaries are control, cli, identity-service, and xtask. | Binaries set up the process and connect its parts. They can own SQL that only they use. No crate can depend on a binary. |

The apps workspace links only portable crates. Runtime binaries and runtime domain crates must have no dependency path to a deployment domain crate. Deployment binaries can link runtime domain crates.

Among library crates, only infrastructure crates link Kubernetes, OCI, and NATS client libraries directly. Binaries can link these libraries for their own wiring.

The authentication crate owns token checks, authorization reads, route authentication, and project administration. The identity crate owns sessions, passwords, account OAuth, service principals, workload identity, platform identity, logins, and NATS signing. The connection crate owns connection OAuth token refresh.

`HostRouteSet::contract_files` can read files until Epic 2 removes the authored JSON shapes and their loader.

## Traits

Use a trait only if one of these conditions applies.

- It has two production implementations.
- A test uses its own implementation.
- It prevents a dependency cycle.
- It keeps Wasmtime and the engine out of a crate that does not need them.

Use a direct call in other cases. Before you remove a trait, name the location of the call that replaces it. Keep `HostPolicy` to prevent a dependency cycle and share the upstream host matching rules.

## SQL and rows

- The Postgres kernel holds SQL for shared database mechanics and persistence conventions. These include transaction identity binding and the migration runner. Keep domain SQL in the runtime domain crate, deployment domain crate, or binary that owns it.
- Keep the SQLite store private in `wamn-edge`.
- Give each logical operation one transaction owner. That owner commits or rolls back the transaction. Helpers can borrow the transaction. The host can keep the transaction across guest callbacks.
- When you replace a transaction trait with direct SQLx calls, keep the existing transaction, lock, cancellation, and commit boundaries.
- Session exchange must keep membership checks, signing, and commit in one transaction. Session renewal must still commit revocation in the same refusal paths.
- Bind and read plain values, such as `String`, `Uuid`, or arrays. Use the constructor to make the domain value.
- If you map a record often or the mapping can fail, use a private row struct in the crate that owns the SQL.

## Jobs and names

- A crate has one job. Its name fits everything that the crate holds.
- A crate name is singular and in plain words, with no abbreviations. A test crate can end in `-tests`.
- A directory name equals the crate name without the `wamn-` prefix. Inside `test-support/`, it also drops the `test-` prefix.
- The members of a workspace live under the directory of that workspace. No member sits inside the directory of another workspace.

## Code

- Do not copy code. If two crates need the same code, it goes to the crate that owns the job.
- A crate declares no dependency that it does not use. `cargo xtask machete` finds them. An ignore entry carries a comment that gives the reason.
- No public item exists only for tests.
