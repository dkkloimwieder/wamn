# Architecture overview

WAMN runs application operations as WebAssembly components on wasmCloud.
A wiring is a graph that connects admitted operations.
Applications own business behavior, while native services own authority, deployment, and external resources.
This page describes the main owners. The [architecture index](README.md) links each detailed contract.

## Application ownership

Each application lives under `apps/<exact-package-id>/`.
The shared integration base lives under `apps/platform/wamn_integration/` and uses the same application workspace and output root.
Its home contains the manifest, migrations, authored SQL, components, operator application, and tests.
`wamn build` writes its generated output to `apps/target/wamn/<exact-package-id>/`, and Git holds none of it.
Receiving, WMS, and the Acme Receiving overlay have separate homes.
The application README maps its package to its Cargo crates and components.

A package owns its version and schema contribution.
An overlay declares its exact base dependency and owns its added definitions and operations.
A module organizes implementation within a package and creates no independent public identity.
The [naming contract](naming.md) defines operation spelling, and [data access](data-access.md) defines ownership on shared relations.

The repository root owns the native Cargo workspace, including native application tests.
`apps/Cargo.toml` owns the guest workspace.
`apps/platform/` holds shared guests and guest libraries.
The separate `apps/platform/no-std/` workspace isolates its dependency features.
Cargo owns membership, dependencies, features, and targets. Application manifests own component declarations.

## Control and publication

| Owner | Responsibility |
| --- | --- |
| `wamn-control-registry` | Organization, project, and environment declarations |
| `wamn-control-provision` | Provisioning SQL, credential roles, and native broker declarations |
| `wamn-schema-control` | Package migration decisions and run storage declarations |
| `wamn-catalog` | Component, wiring, and release facts |
| `wamn-package-artifact` | Reading package artifacts from the registry and their component descriptors |
| `wamn-control` | Control operations, delivery, development orchestration, and process supervision |
| `wamn-ctl` | Command-line client of `wamn-control` |
| `wamn-scenario-worker` | The authoring admission API |

The CLI, the dev loop, and other callers call `wamn-control`, and `wamn-control` calls the lower libraries.
`wamn-control` never depends on `wamn-ctl`.
A deployed service never depends on `wamn-control`. The scenario worker reads component facts through `wamn-package-artifact`, as publish does.
A `wamn-control` operation takes ordinary inputs, keeps its own validation, and returns its result without printing.

`wamn-control` lives at `crates/control/lib` and owns every control verb.
It also owns the delivery code, which qualifies a release, publishes it, and applies it to an environment.
That code runs the `git`, `docker`, `kubectl`, and `cargo` processes its work needs.
It owns the child process group those processes run in.
The `ops` feature adds the user-creation, broker-advisory reporting, and history-pruning verbs.
`wamn-ctl` turns that feature on for its separate `wamn-ctl-ops` binary.

`wamn-ctl` lives at `services/ctl` and owns the command line.
It holds the clap argument definitions, the printed output, and the exit codes.
Each verb parses its arguments, makes one library call, and prints the result.
The `<area>_verbs.rs` modules hold that surface.
The development commands in `services/ctl/src/dev/` own argument parsing, output, and process signals.
`wamn-control::dev` owns the watcher, session orchestration, environment setup, target reset, and child-process supervision.
Clients request cooperative shutdown through the session control handle.
The engine finishes active work and cleans up its owned processes.

`apply-package` is the sole application migration applier.
Rust decides package registration and wiring activation from stored facts within the caller's transaction.
PostgreSQL enforces uniqueness, row isolation, immutable records, release sealing, and required locks.
Package registration retains its lineage lock, exact retry behavior, and first timestamp.
A fresh install creates the platform schema, including the control database.
`wamn-ctl upgrade-schema` applies the pending numbered migrations to an installed database.
One run is one transaction.
It makes sure that the recorded files match the files of the binary.
Then it applies the pending files in order and records them.
Without `--confirm`, it prints the pending files and rolls back.

The provisioning owner locks and compares environment identity before refreshing its tenant projection.
That refresh clears the instance claim even when the suffix stays unchanged.
The developer coordinator claims an instance with one update and refuses a missing projection.

Authoring cases judge a wiring document and permit no effects.
The case runner refuses a reachable component with admitted effects, including the effects of each base composed into it.
Reports are immutable and keyed by the wiring hash within the tenant.
Concurrent attempts return the stored winner for that hash.
The service keeps no durable table for resuming individual cases.
Effectful application tests remain separate from this authoring contract.

## Release identity

A release fixes package versions, component digests, base pins, SQL, connections, routes, and wiring versions.
Sealing prevents later changes to its content.
Conflicting content refuses, and exact retries retain their original facts.

Publication mints one route for each attachment that targets a route.
The route carries the contract kind of its operation, which publication reads from the generated `operation.json` of the package.
Generation derives the route input schema of each generated CRUD operation from its input contract, and writes it to `generated/routes/<model>/<action>.json`.
Generation also writes the route entry and the declaration entry of each generated operation, under `generated/publication/`.
The authored `publication/attachments.json` and component declaration carry only the custom operations, and each reader reads both files, as [data access](data-access.md#generated-operations) states.
The route `input-schema` and the operation input port name the route schema file, and hold no copy.
Each one writes `{"$ref": "generated/routes/<model>/<action>.json"}`.
Publication, component admission and the client generator resolve the name against the package root, so a contract change moves the served schema and its definition hash.
An authored operation keeps the input schema that its author writes.
Publication refuses a wiring with no edges, because a graph with no edges is a route.
A registration or schedule can name a one-node wiring.

Publication produces an immutable release manifest addressed by its digest.
Deployment supplies that digest and artifact location to the host.
A rollout changes the serving release.
Runtime requests resolve wiring from the supplied release, independently of mutable activation pointers and PostgreSQL notifications.
Frozen candidate execution retains its own admitted facts throughout the traversal.

`wamn-ctl qualify-release` runs the qualification cases against a published release, which it pulls by digest.
When every case passes, it pushes a qualification artifact beside the release.
The artifact is keyed by the release digest and the platform image set of the release chart.
`env apply` refuses a release that has no qualification for that image set.
The [delivery commands](../operations/delivery.md) describe qualification.

## Environment lifecycle

An environment is one Helm release of the `release` chart in `deploy/platform/release`.
The release name is `r-<slug>-<hash>`, derived from the coordinate `org/project/env`.
The name is also the host group of the environment.
The chart holds the pinned runtime-operator subchart in host-only mode, which renders only the host.
It adds one WorkloadDeployment for the `http` role and one for the `materializer` role.
The `http` role starts the composed entry with `poolSize` 0, and a Service of the same name routes to it.
When the environment has a route host, the chart also writes a ConfigMap that names the host, the release digest and the web clients.
The platform image set lives only in the default values of the chart. `wamn-ctl build-platform` stamps it.
The release is named by its manifest digest alone.

An environment is declared in one `environment.k` document.
It names the coordinate, the release digest or `none`, the route host, the policy by name, the connection definitions, and the floors.
`wamn-ctl env` has five verbs:

- `apply <file>`: Move the environment toward its document. `--dry-run` prints the plan without the lock.
- `rollback <coordinate> --reason <text>`: Apply the previous intended release.
- `delete <coordinate> [--data]`: Delete the environment.
- `show <coordinate>`: Print the document that the authorities hold for the environment.
- `status <coordinate>`: Print the current state as JSON.

`apply`, `rollback` and `delete` take one lifecycle lock for the coordinate and hold it for the whole verb.
A second verb on the same coordinate refuses while the lock is held.
`--dry-run` and `show` take no lock.
The platform inputs come from environment variables of the verb, not from flags or document fields.

`apply` runs these steps:

1. Analyse: Read each authority once. Compare the document with them. Return the plan or the refusals.
2. Ensure: Create the missing substrate: the environment row, the `Database` resource, the project database, the run plane, the credential Secrets, and the admin grants.
3. Expand: Write the policy and route host. Run the package stage. Enable each declared connection and bind it to the components of the release.
4. Write: Run `helm upgrade --install` with `--wait`, the readiness budget of the policy, and `--rollback-on-failure`. If the chart version and values equal those of the newest successful revision, skip Helm.
5. Readiness: Wait until each WorkloadDeployment of the release reports `Ready`.
6. Drain: Wait until the live set is exactly the declared release. Name each stranded run and stop.
7. Contract: Remove the bindings of releases outside the live set. Disable instances that the document no longer names. Apply a declared floor.

The package stage runs in the project database before any Kubernetes write.
It applies the pending platform migrations and installs each new package from its verified artifact.
It applies the stage of each successor, writes the release cache row, and reconciles data access.
It also creates the number sequence row of each package default that the environment lacks.
With `release = none`, `apply` uninstalls the release chart instead of writing it.
Every step is an ensure that is judged by observation, so applying again repairs a crash.

`rollback` finds the newest Helm revision that is `deployed` or `superseded` and has another digest than the current one.
It runs `apply` on that digest and keeps the reason in the Helm description.
It never runs `helm rollback`.
If no such revision exists, it refuses and writes nothing.

`delete` is the one destructive verb, and every step runs only if its target is present.
In order, it does these steps:

1. Uninstall the release chart and wait for the drain.
2. Revoke the provisioning tokens.
3. Delete the credential Secrets and the event streams.
4. Drop the replication slot and delete the `Database` resource.
5. Delete the control rows and the environment row.

The `Database` resource keeps `databaseReclaimPolicy: retain`, so the database stays on the cluster.
With `--data`, the verb drops the database and the instance roles. It reads the coordinate again from a terminal first.

## Organization membership and administration

A principal is global. Membership of an org is explicit.
`identity.org_memberships` holds one row for each principal and org, with the status `active` or `inactive`.
An invitation creates or reuses the principal and activates its membership.
An active member can hold no project access, a membership of environments, application roles, `project-admin`, or `org-admin`.
A valid account with no effective access is not an authentication error.

Authorization never infers administration from a higher role.
Each grant writes the lower rows:

- `org-admin` writes the `identity.org_roles` row, `project-admin` in every project of the org, and a membership of every environment of the org.
- `project-admin` writes the `identity.project_roles` row and a membership of every environment of the project.
- The `admin` role row of the user is written in the database of each environment.

A new project or environment receives the rows of the existing administrators when it is created.

Revocation flows downward.
Revoking `org-admin` removes `project-admin` in the org and the application `admin` rows.
Revoking `project-admin` removes it in one project.
Neither revoke removes ordinary memberships or authored roles.
A revoke of a lower grant refuses while a higher grant covers it. The holder must revoke the higher grant first.

Deactivating a user in an org removes four kinds of access, from the leaves upward.
They are the application user rows, the environment memberships, the project roles, and the org roles of that org.
It then marks the membership `inactive`.
The global principal stays, and access in other orgs does not change.
Reactivation makes the membership active and restores nothing else.
The control host exposes these writes as the routes `user.invite`, `user.activate`, `user.deactivate`, `org_admin.grant` and `org_admin.revoke`.

An additive base can satisfy an unchanged overlay's declared schema and operation requirements.
Installed additive upgrades use a separate `qualify-upgrade` proof before `apply-package` changes production.
Qualification copies predecessor data and ownership, observes the actual runtime schema, and requires the successor to retain that schema.
It proves predecessor SQL under both predecessor and candidate grants, then records the exact candidate roots and final privileges.
Application stores accepted evidence with the successor in one transaction.
For a base upgrade, that transaction installs the base first and every affected overlay successor afterward.
Each overlay preserves its consumed operation contracts and pins the exact candidate base version and component digest.
Qualification refuses an omitted overlay, a changed contract, or a migration statement that cannot run inside the transaction.
Reconciliation requires evidence for the complete current package set and privileges, without selecting by timestamp.
The [package upgrade procedure](../operations/deployment.md#package-upgrades) owns command order and recovery.

## Runtime owners

Five layers run a component, and each layer depends only on the layers below it:

1. [`wamn-run-state`](../../crates/execution/run-state/src/lib.rs) holds pure run decisions and two storage traits. `RunStore` is the queued-run lifecycle. `IntentStore` is the per-call intent record. `wamn-runtime`, `wamn-run-state-sqlite` and the edge implement it.
2. [`wamn-engine`](../../crates/platform/engine/src/lib.rs) holds the Wasmtime and wash-runtime host, component admission and artifacts, lifecycle, `invoke_operation`, and the host side of the `wamn:operation` binding that the route guest calls. It links no Postgres, OCI, or `object_store` crate. A test pins what it links through wash-runtime.
3. [`wamn-runtime`](../../crates/platform/runtime/src/lib.rs) holds the plugin set: Postgres, JetStream, blobstore, HTTP connections, credentials, logging, route authentication, and the OCI registry source. `WamnPostgres` is the Postgres adapter of `RunStore`.
4. [`wamn-execution-host`](../../crates/execution/host/src/lib.rs) holds the route path, `OperationHost`, and the delivery bridge.
5. [`wamn-workflow`](../../crates/execution/workflow/src/lib.rs) holds everything that walks or delivers a wiring: the driver, wiring delivery, the queue, the lowering of a wiring, and the workflow contract (start, park, release, list). It depends on the pure walk, [`wamn-router`](../../crates/execution/workflow/router/src/lib.rs). A route never enters it.

The services compose these layers.

A route calls one component export through the route path. A wiring and queued work use the [execution driver](execution.md) in `wamn-workflow`.
The driver coordinates graph decisions. The engine owns dispatch and workload loading, and the host owns invocation authority.
The workflow crate owns durable queue claims and settlement through `RunStore`.
The [capability owners](capabilities.md) supply database, HTTP, object storage, and event access without guest credentials.

The developer host also loads unpublished components and admitted facts from local files.
It requires the expected file digests and a database marker tied to the tenant, environment, and physical database creation.
Local facts supply released wiring, connection requirements, and binding selections without changing the frozen catalog records.
The normal runtime still owns tenant authority, run claims, credential generations, and effects.

## Cloud providers

A provider-specific piece is a named implementation behind one platform interface, never a switch inside the platform.
The registry token source and the blob provider use this pattern.
On Google Cloud they are the metadata server token of [registry credentials](capabilities.md#registry-credentials) and the `gcs` blob provider.
A second provider adds one implementation of each and a values file.
Nothing in the router, the generator, or the applications changes, and no piece uses a service account key.
The kind clusters run the platform with none of these implementations.
