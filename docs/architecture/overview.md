# Architecture overview

WAMN runs application operations as WebAssembly components on wasmCloud.
A wiring is a graph that connects admitted operations.
Applications own business behavior, while native services own authority, deployment, and external resources.
This page describes the main owners. The [architecture index](README.md) links each detailed contract.

## Application ownership

Each application lives under `apps/<exact-package-id>/`.
Its home contains the manifest, migrations, authored SQL, generated output, components, operator application, and tests.
Receiving, WMS, and the Acme Receiving overlay have separate homes.
The application README maps its package to its Cargo crates and components.

A package owns its version and schema contribution.
An overlay declares its exact base dependency and owns its added definitions and operations.
A module organizes implementation within a package and creates no independent public identity.
The [naming contract](naming.md) defines operation spelling, and [data access](data-access.md) defines ownership on shared relations.

The repository root owns the native Cargo workspace, including native application UIs and tests.
`apps/Cargo.toml` owns the guest workspace.
`apps/platform/` holds shared guests and guest libraries.
The separate `apps/platform/no-std/` workspace isolates its dependency features.
Cargo owns membership, dependencies, features, and targets. Application manifests own component declarations.

The Receiving operator combines generated screens with application-owned Rust.
Its UI crate owns the entrypoint, and its generated crate supplies a library.
The generator creates no second Receiving launcher.
WMS still declares `wamn-wms-tui` in its generated UI crate.
The development command selects the operator from its Cargo declaration.

## Control and publication

| Owner | Responsibility |
| --- | --- |
| `wamn-control-registry` | Organization, project, and environment declarations |
| `wamn-control-provision` | Provisioning SQL, credential roles, and native broker declarations |
| `wamn-schema-control` | Package migration decisions and run storage declarations |
| `wamn-catalog` | Component, wiring, and release facts |
| `wamn-ctl` | Database and broker operations through these libraries |
| `wamn-scenario-worker` | The authoring admission API |

`apply-package` is the sole application migration applier.
Rust decides package registration and wiring activation from stored facts within the caller's transaction.
PostgreSQL enforces uniqueness, row isolation, immutable records, release sealing, and required locks.
Package registration retains its lineage lock, exact retry behavior, and first timestamp.
Platform schema installation targets fresh databases, including the control database.

`wamn-schema-control` compares projected release identities and deployment attestations.
The CLI writes them and compares a concurrent winner within the same transaction.
Exact retries retain the first timestamp and nullable source provenance.
The provisioning owner locks and compares environment identity before refreshing its tenant projection.
That refresh clears the instance claim even when the suffix stays unchanged.
The developer coordinator claims an instance with one update and refuses a missing projection.

Authoring cases judge a wiring document and permit no effects.
The case runner refuses a reachable component with admitted effects, including effects reached through operation dependencies.
Reports are immutable and keyed by the wiring hash within the tenant.
Concurrent attempts return the stored winner for that hash.
The service keeps no durable table for resuming individual cases.
Effectful application tests remain separate from this authoring contract.

## Release identity

A release fixes package versions, component digests, operation dependencies, SQL, connections, and wiring versions.
Sealing prevents later changes to its content.
Promotion compares target manifests and ordered migrations, copies admitted facts, and admits target wirings.
Conflicting content refuses, and exact retries retain their original facts.

Publication produces an immutable release manifest addressed by its digest.
Deployment supplies that digest and artifact location to the host and executor.
A rollout changes the serving release.
Runtime requests resolve wiring from the supplied release, independently of mutable activation pointers and PostgreSQL notifications.
Frozen candidate execution retains its own admitted facts throughout the traversal.

An additive base can satisfy an unchanged overlay's declared schema and operation requirements.
This compatibility does not authorize an in-place upgrade or a general migration lifecycle.
The remaining upgrade design belongs in [upgrade plans](../plan/upgrades.md).
The [operations pages](../operations/README.md) own publication, deployment, and rollback procedures.

## Runtime owners

The host and executor share the [execution driver](execution.md).
The driver coordinates graph decisions, while private native modules own dispatch, invocation authority, and workload loading.
The executor and existing run-state libraries own durable queue claims and settlement.
The [capability owners](capabilities.md) supply database, HTTP, object storage, and event access without guest credentials.
