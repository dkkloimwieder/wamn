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
| `wamn-registry` | Org, environment and project coordinates, resource names, and registry SQL |
| `wamn-control-provision` | Provisioning SQL and credential roles |
| `wamn-broker` | NATS connections, streams, shared memory buckets, and authorization |
| `wamn-kubernetes` | Kubernetes HTTP client, Secrets, Job execution, and command runners |
| `wamn-catalog` | Component, wiring, and release facts |
| `wamn-package-artifact` | Reading package artifacts from the registry and their component descriptors |
| `wamn-package-control` | Package admission and artifacts |
| `wamn-release-control` | Release publication and charts |
| `wamn-environment` | Environment provisioning, package application, and schema lifecycle |
| `wamn-platform-install` | Helm installation, platform configuration, and explicit bootstrap Jobs |
| `wamn-dev-control` | Development orchestration and process supervision |
| `wamn-ctl` | Command-line client of the four control crates |

The CLI and test support call each control crate directly.
The dependency order is package, release, environment, then development.
Each crate depends only on earlier control crates and the lower libraries.
The control crates never depend on `wamn-ctl`.
`wamn-package-control` owns standalone web artifacts in `web_artifact`.
Package artifacts contain three tar layers and no browser files.
`wamn build` compiles package components without Vite or a web output digest.
`wamn web build` binds browser files to the compiled package manifest digest recorded in `build.json`.
`wamn web push` publishes a deterministic OCI artifact under an immutable tag.
[Web clients](../operations/web-upload.md#build-and-push) defines its layout and commands.
A deployed service reads package facts through `wamn-package-artifact`.
A control operation takes ordinary inputs, keeps its own validation, and returns its result without printing.

`wamn-platform-install` lives under `crates/control/platform-install`.
Helm records infrastructure in `platform/wamn-infrastructure` and platform services in `platform/wamn`.
The org chart owns PostgreSQL clusters and backup resources in the org namespace.
The environment chart owns databases, publications, worker accounts, hosts, and materializers in that same namespace.
Credential Secrets and bootstrap SQL Jobs stay outside Helm values.
Each org chart update derives its complete role list from registry facts and fixed platform definitions.
Bootstrap declares only the fixed platform roles. Helm stores the derived list.
Each worker fetches the selected chart by its immutable OCI digest.
The installed-platform reader supplies registry and chart configuration to later operations.
Host groups reference shared configuration through a ConfigMap.
Cloud project, node pool, and OpenTofu tools live in xtask.

The other control crates live under `crates/control/{package,release,environment,dev}`.
They run the `git`, `docker`, `kubectl`, and `cargo` processes that their work needs.
`wamn-dev-control::owned_command` owns the child process groups.
The environment crate owns history retention. Its library interfaces enforce the retention login and coordinate.
[Native control](control.md) owns the org control service and its operation worker.

`wamn` lives at `services/ctl` and owns the user command line.
`wamn-ctl` is an internal bootstrap worker.
It holds the clap argument definitions, the printed output, and the exit codes.
Each verb parses its arguments, calls its owner, and prints the result.
The `<area>_verbs.rs` modules hold that surface.
The development commands in `services/ctl/src/dev/` own argument parsing, output, and process signals.
`wamn-dev-control::dev` owns the watcher, session orchestration, environment setup, target reset, and child-process supervision.
Clients request cooperative shutdown through the session control handle.
The engine finishes active work and cleans up its owned processes.

`apply-package` applies package SQL through SQLx `Migrator`.
Authored and generated history migrations share each package's version sequence.
The migration connection authenticates directly as the restricted migration login.
A separate connection writes trusted catalog facts.
Session locks and actor bindings last across the commits of individual migrations.
Each package and environment has its own SQLx history table.
System and project platform scopes use separate SQLx histories under their platform owners.
Project initialization installs the history helpers and a privilege event trigger.
The trigger refuses `GRANT`, `REVOKE`, and `ALTER DEFAULT PRIVILEGES` from the authenticated migration login.
SQLx checksums refuse changes to applied migrations.

Identity fills application user rows at provisioning and updates them when grants change.
Project initialization installs platform SQL once. SQLx applies later platform changes.
The provisioning owner locks and compares environment identity before refreshing its `project_env_key` projection.
That refresh clears the instance assignment even when the suffix stays unchanged.
The developer coordinator assigns an instance with one update and refuses a missing projection.

Workflow authorship validates the graph against the environment’s serving release.
The application tests exercise effects through runtime interfaces.

## Release identity

A release fixes package versions, component digests, base pins, SQL, connections, routes, and wiring versions.
Sealing prevents later changes to its content.
Conflicting content refuses, and exact retries retain their original facts.

The compiler creates the final publication documents once from typed operation descriptions.
It writes routes with inline input schemas and computed definition hashes to `publication/attachments.json` in the build output.
It writes component declarations with inline request and result schemas to `publication/components.json`.
The compiled `wamn.json` carries operation types and described statement facts.
Publication, admission, and clients consume these final documents. They do not merge authored files or resolve schema file references.
Workflows are authored for an environment and name sealed operations from its release.
Publication refuses a wiring with no edges, because a graph with no edges is a route.
A registration or schedule can name a one-node wiring.

Publication produces an immutable release manifest addressed by its digest.
Deployment supplies that digest and artifact location to the host.
A rollout changes the serving release.
Runtime requests resolve wiring from the supplied release, independently of mutable activation pointers and PostgreSQL notifications.
Frozen candidate execution retains its own admitted facts throughout the traversal.

## Environment lifecycle

An environment is one Helm release of the `release` chart in `deploy/platform/release`.
The release name is `r-<slug>-<hash>`, derived from the coordinate `org/env/project`.
The name is also the host group of the environment.
The chart holds the pinned runtime-operator subchart in host-only mode, which renders only the host.
It adds one WorkloadDeployment for the `http` role and one for the `materializer` role.
The `http` role starts the separate route component with `poolSize` 0. A Service of the same name routes to it.

When the environment has a route host, the chart also writes a ConfigMap.
Its `release.json` names the host, the release digest, client packages in `clients`, and pinned web digests in `webs`.
The platform image set lives only in the default values of the chart. `cargo xtask build-platform` stamps it.
The release is named by its manifest digest alone.

An environment is declared in one `environment.k` document.
It names the coordinate, application hostnames, the policy, schema mode, clients, connections and integrations.
The applied configuration lives in `registry.environment_projects.configuration`. `wamn-registry` defines its type.
Helm history owns release selections. Package membership comes from each release manifest.

Its optional `client` names a package id and an immutable web artifact digest.
Analysis makes sure that the artifact matches its digest and its package coordinate appears in the release.
Without `client`, the host serves no application web client.
Revision values keep `webClients` for package ids and `webDigests` for web artifact digests.
Apply reads the web artifact without owning it, so environments can share one artifact.
`wamn web upload` copies that artifact’s `dist/` to the shared digest root.
It writes files create-only, then the index.
An existing index marks completion, so another upload writes nothing.

The Rust edge reads `release.json` for each request.
It serves application files from `clients/<package>/<web digest hex>/` in the web bucket.
The web digest must use `sha256:` and 64 lowercase hex digits.
A host without a client package or web digest refuses with `the host has no web client`.
The release digest does not select browser files.

The Rust web front generates application `config.json` from the live release.
It supplies `org`, `project`, `release` and `web`.
A web-only apply changes the bundle and `web`, while `release` stays the same.
The web front reads other files from the bucket without a process cache and serves `config.json` with `no-cache`.
The platform UI uses `platform-ui/sha256-<digest>/`.
The web front generates its org configuration from the live release.
`wamn-web-front` owns response headers and proxies `/password/` to identity over HTTPS.
TLS for browser traffic terminates at ingress.

`wamn deploy`, `wamn rollback`, `wamn status`, and the schema commands call authenticated control routes.
The CLI requires `WAMN_CONTROL_URL` and `WAMN_TOKEN` for these commands.
It does not fall back to direct database or Kubernetes access.
Build, dev and eject run locally. Bootstrap runs locally before control exists.

The native service reads one public JSON configuration file through `--config`.
Database connection objects refer to CNPG Secrets. The configuration holds no database passwords.
Long application operations run in bounded Jobs through `wamn-kubernetes`.
A Job receives its authenticated actor and environment coordinate from the service.
SQL operations mount the owning project SQL and package migration credentials.
They never mount a superuser credential or start another SQL Job.
The registry owns the applied environment configuration. Helm owns release history.

`apply` runs these steps:

1. Analyse: Read each authority once. Compare the document with them. Return the plan or the refusals.
2. Ensure: Create the missing substrate: the environment row, the `Database` resource, the project database, the run plane, the credential Secrets, and the admin grants.
3. Expand: Write the policy and route host. Run the package stage. Enable each declared connection and bind it to the components of the release.
4. Write: Recover any pending Helm operation. Always run `helm upgrade --install` with `--wait`, the readiness budget of the policy, and no automatic rollback. A failed revision and its pods remain available for inspection.
5. Readiness: Wait until each WorkloadDeployment of the release reports `Ready`.
6. Drain: Wait until the live set is exactly the declared release. Name each stranded run and stop.
7. Retain: Keep older release facts and permissions. Rollout performs no release cleanup.

A separate retention operation removes unused release facts and generated permissions under the environment lock.
Live hosts, current Helm selections and unfinished runs keep their release facts.
Status reports unfinished runs with no live host across the environment.

During analysis, apply reads each bound connection's type and the target type from the registry.
For an exclusive connection on a cloud target, the body from `WAMN_RELEASE_HOST_GROUP` must name `replicas: 1`.
Apply refuses another count or a missing key and names the connection.
It never uses the chart default for this requirement.
Local targets are exempt.

The package stage runs in the project database before any Kubernetes write.
It checks applied migration versions and checksums through SQLx and installs each new package from its verified artifact.
It applies the stage of each successor and writes the release cache row.
It also creates the number sequence row of each package default that the environment lacks.
With `release = none`, `apply` uninstalls the release chart instead of writing it.
Every step is an ensure that is judged by observation, so applying again repairs a crash.
Initialization installs the platform, catalog and run schemas once.
Explicit platform schema apply runs new SQLx migrations and refuses changed checksums.
Application schema reset stops users, removes migration-owned objects and clears application state.
It discards old replication changes and queued events, reapplies migrations, restores selected releases and resumes users.
The database, credentials, platform schemas and CDC objects stay.

After a failed or pending head, `rollback` restores the latest successful Helm revision.
After a successful head, `rollback` compares successful revisions with the current release digest and client pin.
It selects the newest revision with a different pair and status `deployed` or `superseded`.
It restores that revision's release digest and client through `apply` and keeps the reason in the Helm description.
If that revision has no client, rollback clears the current client.

After a web-only apply, rollback restores the previous web digest with the same release digest.
The web maps reach only the edge's release ConfigMap, so a web-only apply or rollback leaves host pod templates unchanged.

Rollback never runs `helm rollback`.
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

The platform UI reads status through a control operation with a read-only ServiceAccount over Pods, Deployments and WorkloadDeployments in the release namespace `hosts`: the digest from the host Deployment's pod-template annotations, the rollout from its status, the workload conditions, the live set. Who selected and who approved are not there: they are in the Helm revision (§11.3), read with `helm history`, not by the UI. Current state only; no history of tests; no Secrets; no verb runs from a browser. The project screen that reads the saga today reads this instead.

- Every step is safe to run twice. A missing object is a finished step, not a refusal.

- The registry rows go last. Until they go, a second run can still read the instance suffix and the project.

- The database and its CDC objects go before any role. The control rows go before the registry rows.

- The verb turns off an immutability trigger for its one delete statement only, inside one transaction, and turns it on again before the commit. No other session sees the trigger off.

- The source stream and the advisory stream are deleted by name, not purged. No NATS permission is added: the provisioning user of the stream already holds `STREAM.DELETE` and `CONSUMER.DELETE` (`test-support/infrastructure/event_broker.rs:101-131`).

- The verb prints each step with its result on one line. It prints no credential.

### Control rows during deletion

6. Delete the environment’s control rows in one transaction.

### Deletion refusals

- The replication slot is active: a reader still streams from it.

### Resources kept after deletion

- The org, the project, `identity.project_roles` and every user principal.
- The control-family login roles and their `author_login_projects` rows. They belong to the triple, not the instance, and a new provision of the triple uses the same names.

## Registry hierarchy

The hierarchy is org, environment, project, application.
`registry.environments` owns each environment's policy, status, disposable setting, and target.
`registry.projects` holds org-level project definitions.
`registry.environment_projects` places a project under an environment and owns its database Secret, instance suffix, and route host.
Applications are releases under those project deployments.
Environment coordinates contain org and environment.
Project database coordinates also contain the project.
The catalog holds no second environment identity table.
Postgres row security restricts each control login to the org encoded by its authenticated login.
Request settings cannot select a different org.

## Organization membership and administration

A principal is global. Membership of an org is explicit.
`identity.org_memberships` holds one row for each principal and org, with the status `active` or `inactive`.
An invitation creates or reuses the principal and activates its membership.
An active member can hold no project access, a membership of environments, application roles, `project-admin`, or `org-admin`.
A valid account with no effective access is not an authentication error.

Authorization derives administration from the current explicit grants.
An org admin administers every environment, project and application in the org.
An environment admin administers every project and application in that environment.
An environment grant gives no org authority.
A project admin administers the applications and connections of that project.
Each grant writes only its own row in `identity.org_roles`, `identity.environment_roles`, or `identity.project_roles`.
The platform copies no inherited roles or memberships.

Session issuance and host permission reads derive application administration from these same grants.
A revoke removes only the explicit grant that it names.
Other explicit grants and ordinary memberships stay.
Inactive org membership grants no access.
Application databases retain user identity and authored roles, but they do not store inherited user administration.
Services retain their explicit application roles.

Deactivation removes the user's application identity and explicit grants within that org.
It then marks the org membership inactive.
The global principal and access in other orgs stay.
Reactivation restores the org membership and no removed grants.

A package schema change uses immutable SQLx migrations from a published release.
Migrate mode preserves data. Reset mode clears application state before it reapplies those migrations.

## Runtime owners

Five layers run a component, and each layer depends only on the layers below it:

1. [`wamn-run-state`](../../crates/execution/run-state/src/lib.rs) owns run admission, leases, settlement and pruning on authorized connections. Concrete operation owners hold their own claim storage.
2. [`wamn-engine`](../../crates/platform/engine/src/lib.rs) holds the Wasmtime and wash-runtime host, component admission and artifacts, lifecycle, `invoke_operation`, and the host side of the `wamn:operation` binding that the separate route component calls. It links no Postgres, OCI, or `object_store` crate. A test pins what it links through wash-runtime.
3. [`wamn-runtime`](../../crates/platform/runtime/src/lib.rs) holds the plugin set: Postgres, JetStream, blobstore, HTTP connections, credentials, route authentication, and the component registry adapter. `WamnPostgres` supplies authorized transaction transport for run persistence.
4. [`wamn-execution-host`](../../crates/execution/host/src/lib.rs) holds `OperationHost`, native platform endpoints and release readiness.
5. [`wamn-workflow`](../../crates/execution/workflow/src/lib.rs) owns the private graph walk, the node invocation driver, the durable queue, and the workflow contract (start, park, release, list). It indexes catalog graphs directly. A route never enters it.

`wamn-package-artifact` owns shared registry clients, release artifact transfer and the pod metadata service.
The host uses wash-runtime's logger and its own tracing exporter.

The services compose these layers.

A route calls one component export through the route path. A wiring and queued work use the [execution driver](execution.md) in `wamn-workflow`.
The driver coordinates graph decisions. The engine owns dispatch and workload loading, and the host owns invocation authority.
The workflow crate resolves and drives runs. The run-state crate owns their persistence.
The [capability owners](capabilities.md) supply database, HTTP, object storage, and event access without guest credentials.

The developer host also loads unpublished components and admitted facts from local files.
It requires the expected file digests and a database marker tied to the `project_env_key`, environment, and physical database creation.
Local facts supply released wiring, connection requirements, and binding selections without changing the frozen catalog records.
The normal runtime still owns project authority, run leases, credential generations, and effects.

## Cloud providers

A provider-specific piece is a named implementation behind one platform interface, never a switch inside the platform.
The registry token source and the blob provider use this pattern.
On Google Cloud they are the metadata server token of [registry credentials](capabilities.md#registry-credentials) and the `gcs` blob provider.
A second provider adds one implementation of each and a values file.
Nothing in the router, the generator, or the applications changes, and no piece uses a service account key.
The kind clusters run the platform with none of these implementations.
