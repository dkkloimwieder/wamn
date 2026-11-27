# Architecture overview

WAMN runs application operations as WebAssembly components on wasmCloud.
Applications declare data and operations in `wamn.k`.
Native services own authority, deployment, and external resources. PostgreSQL stores durable platform state.
The [architecture index](README.md) links the contracts for each part.

## Application ownership

The root `Cargo.toml` owns native libraries, services, repository tasks, and native tests.
`apps/Cargo.toml` owns application guests, platform guests, guest libraries, and guest fixtures.
Each workspace keeps its members below its own directory. Cargo owns membership, dependencies, features, and targets.

Applications live directly under `apps/<exact-package-id>/`.
Each application keeps its manifest, migrations, authored SQL, component, and browser code together.
The Receiving and WMS native tests live under `tests/`.
`wamn_integration` is an application base package beside its consumers.
Overlays declare exact base versions and digests.

`wamn build` writes generated application output to `apps/target/wamn/<exact-package-id>/`.
Moving source directories does not move this output.
Cargo writes compiled artifacts to `CARGO_TARGET_DIR`.
The platform build publishes shared palette components separately from applications.
[Components](components.md#guest-build-identity) defines guest identity and admission.
[Building](../operations/building.md) owns commands and output formats.

## Directory owners

Each library has one concern. Directory names follow crate names without the `wamn-` prefix.

| Directory | Owner and responsibility |
| --- | --- |
| `crates/codegen` | `wamn-codegen`: authored model, document rules, SQL descriptions, and generated outputs |
| `crates/schema` | `wamn-schema`: normalized catalog records |
| `crates/digest` | `wamn-digest`: canonical JSON and SHA-256 identities |
| `crates/scalar` | `wamn-scalar`: scalar formats and SQL value types |
| `crates/number-pattern` | `wamn-number-pattern`: document number pattern parsing |
| `crates/registry` | `wamn-registry`: hierarchy coordinates, resource names, and registry values |
| `crates/release` | `wamn-release`: release plans, admitted components, publication contracts, and web artifact uploads |
| `crates/oci` | `wamn-oci`: registry transport, artifact reads, and registry credentials |
| `crates/env` | `wamn-env` owns environment lifecycle, application chart values, package apply, retention, and CDC reader lifecycle and recovery |
| `crates/platform-install` | `wamn-platform-install`: chart installation and system and org database bootstrap |
| `crates/kubernetes` | `wamn-kubernetes`: Kubernetes transport, Secrets, and Job execution |
| `crates/broker` | `wamn-broker`: NATS connections, streams, and shared memory buckets |
| `crates/event` | `wamn-event`: event records, source identity, and CDC reader configuration |
| `crates/component-policy` | `wamn-component-policy`: import admission and platform import declarations |
| `crates/build` | `wamn-build` owns package builds, overlay and base composition, source publication, and watched deployment |
| `crates/engine` | `wamn-engine`: native loading, dispatch, and the invocation scope registry |
| `crates/workflow-graph` | `wamn-workflow-graph` owns authored graphs, validation, and canonical identity |
| `crates/authority` | `wamn-authority`: shared invocation identity and effect tracing |
| `crates/postgres` | `wamn-postgres`: PostgreSQL connection configuration and transport |
| `crates/plugin` | `wamn-plugin`: host operation transactions, workflow runs, and file catalogue access |
| `crates/http` | `wamn-http`: authorized outbound HTTP and OAuth |
| `crates/envelope` | `wamn-envelope` owns envelope encryption and the wrapping key interface |
| `crates/gcp-metadata` | `wamn-gcp-metadata` reads access tokens and account email from the Google Cloud metadata server |
| `crates/key` | `wamn-key` wraps data keys with a local key or through Cloud KMS |
| `crates/connection` | `wamn-connection`: connection facts, credentials, resource leases, invocation context, and bound Wasm import dispatch |
| `crates/blobstore` | `wamn-blobstore`: connection-authorized file and object storage |
| `crates/storage` | `wamn-storage`: S3 and Google Cloud Storage transport |
| `crates/authentication` | `wamn-authentication`: token verification, route authentication, authorization reads, and project administration |
| `crates/identity` | `wamn-identity`: platform identities, sessions, passwords, and NATS signing |
| `crates/identity-client` | `wamn-identity-client`: HTTPS client for identity administration |
| `crates/identity-protocol` | `wamn-identity-protocol`: principal IDs, database login names, roles, credential formats, and public key documents |
| `apps/crates/guest` | `wamn-guest`: guest cursors, pages, scalar conversion, operation SQL, and document numbering |
| `apps/crates/record-history` | `wamn-record-history`: reconstruction of a row from its history |
| `apps/guests/ingress` | HTTP matching and JSON invocation of typed application operations |
| `apps/guests/blob-put` | Palette node that writes one object |
| `crates/jsonata` | `wamn-jsonata`: expression evaluation for the JSONata guest |
| `apps/guests/jsonata-expression` | Palette node that evaluates a JSONata expression |
| `apps/guests/http-page` | Palette node that reads HTTP pages |
| `apps/guests/http-request` | Palette node that makes an HTTP request |
| `apps/guests/label-render` | Palette node that renders label text |
| `apps/guests/modbus` | Bound Modbus register operations |
| `apps/guests/transform` | Palette node that transforms its input |
| `services/host` | `wamn-host`: shared environment host for HTTP, workflow lifecycle and workflow dispatch |
| `services/control` | `wamn-control`: authenticated org control service |
| `services/identity-service` | `wamn-identity-service` (binary `wamn-identity`): session issuer, passwords, user management, and public verification keys |
| `services/web` | `wamn-web`: browser files, runtime configuration, and password proxy |
| `services/edge` | `wamn-edge`: local application host and device loop |
| `services/cdc-reader` | `wamn-cdc-reader`: PostgreSQL change capture and broker acknowledgement |
| `services/cli` | `wamn-cli`: user CLI and internal operation worker; binary `wamn` |
| `test-support/infrastructure` | Temporary external resources, HTTP fixtures, and shared test adapters |
| `test-support/grant-tests` | `wamn-grant-tests`: grant refusal tests of every restricted deployed login |
| `test-support/fixtures/package` | Platform fixture application paths and inputs |
| `apps/fixtures/guests` | Guest components used by interface tests |
| `tests/journeys` | Tests across services through product commands and routes |
| `xtask` | Repository tasks, build orchestration, and test selection |
| `crates/build` | `wamn-build`: shared package and guest build implementation |
| `deploy/charts/infrastructure` | Cluster infrastructure chart |
| `deploy/charts/platform` | Shared identity, edge, NATS, system PostgreSQL, and operators |
| `deploy/charts/org` | Org resources |
| `deploy/charts/env` | Shared host group for each environment |
| `deploy/charts/project` | Project PostgreSQL Cluster, backups, accounts, certificates, and change capture |
| `deploy/charts/app` | Application release and web selection |
| `deploy/sql` | Platform schemas and shared SQL definitions |
| `deploy/gcp` | Google Cloud deployment inputs |
| `deploy/kind` | Local cluster deployment inputs |
| `web` | Browser runtime, wfe packages, and shared browser tooling |

Callers pass registry resource names and coordinate values to `wamn-kubernetes`.
The control service selects the resources for environment status and the registry credentials for operation Jobs.

## Release identity

The compiler writes route policy and component declarations to the `deploy` section of `wamn.json`.
Publication checks the component bytes, pushes stock wasmCloud images and records their OCI digests.
The top package digest identifies the application content. The package records the exact OCI digest of each base.
The package carries its SQL migrations and history in a separate tar layer.
The HTTP route component calls application components through the host.
Only overlays compose with their pinned bases.


## Environment lifecycle

One YAML file declares the projects, applications, and connection pins of an environment.
The registry stores the applied settings and one version per connection name in the environment.
Helm stores the org release in `wamn-helm-<org>`.
Helm stores host, project, and application releases in `wamn-helm-<org>--<env>`. The environment and project workers can read and write the records in their environment. Projects in the same environment share that access.
The charts place runtime objects in the environment namespace recorded in `registry.env`.
The org chart declares the namespace, base RBAC, and default-deny network policy.
On kind, `wamn env create` applies quotas and container defaults in each env namespace.
The environment chart allows traffic for its host group. The project chart allows traffic for its database and change reader.
Each application release adds its integration ports to the host group's public egress. The runtime enforces the authorities on each wasm integration.
The platform chart owns the system PostgreSQL Cluster in namespace `platform`.
Control and stateless worker SQL use separate PgBouncer transaction Poolers. The environment worker uses its env login on both its transaction Pooler and the session Pooler for lifecycle locks.
Clients check the pooler hostname and TLS certificate against the platform CA.
The project chart owns each project's PostgreSQL Cluster, database, managed logins, backups, and change capture resources.
The environment chart owns the shared host group.
The application chart sets `wamn.package-digest` and `wamn.release-revision` on the WorkloadDeployment.
The worker upgrades each project chart once in file order. It upgrades the environment chart after the projects succeed.
Each project keeps its own database authority, connection facts, and loaded release selections.
A workflow run keeps its workflow version and uses the live application workload.
Environment apply and project deactivation check for unfinished application runs.

The control service authenticates each request and selects the operation worker, chart, credentials, and target.
Each apply Job holds the environment advisory lock on a dedicated system database session.
It writes and commits the environment timing settings through the transaction pooler after the host upgrade succeeds.
Workers apply package migrations through SQLx while logged in as the restricted migration role.
Trusted catalog writes use a separate connection.
The project chart creates each credential store Secret. Control writes individual credential keys through the Kubernetes API, so credential values stay outside Helm values.
[Native control](control.md) defines the request boundary.
[Deployment](../operations/deployment.md) defines lifecycle ordering, rollback, reset, and deletion.

Development builds and publishes immutable source snapshots, then uses the same authenticated control routes as other deployments.
It uses an installed `kind` platform.
The watcher stops after its current deployment finishes. The deployed release stays available.
[Development loop](../operations/development-loop.md) owns the procedure.

## Execution and authority

A route invokes one component export. A workflow traverses its graph and invokes each selected node through the same host boundary.
The engine dispatches Wasm. The host supplies invocation authority. The host services authorize access to external resources.
A component digest shares code only. It grants no access to another project or environment.
[Execution](execution.md) defines routing, authentication, queues, and service lifecycle.
[Platform services](platform-services.md) defines imports, bindings, credentials, and external effects.
[Data access](data-access.md) defines SQL, transactions, migrations, and record history.
[Edge](edge.md) defines local operation storage and device status.

## Registry hierarchy

The hierarchy is org, environment, project, application.
`wamn-registry` defines the values that registry readers return.
`wamn-env` owns SQL for deployment records, applied configuration, and lifecycle locks.
`wamn-platform-install` owns SQL for system and org setup. `wamn-connection` holds the project connection store.
`registry.env` stores each environment's active flag, disposable setting, target, readiness budget, drain bound, and project fact cache lifetime.
`registry.project` holds org-level project definitions.
`registry.project_env` records whether a project deployment is active, along with its database Secret and instance suffix. Deactivation keeps the row and removes its explicit memberships. Re-applying the project creates a new database generation.
Applications are releases under those project deployments.
`registry.env_app` stores each app's active flag and host label.
Each label stays unique within the org, including labels held by inactive apps.
Deploy derives `<label>--<org>.<base>` using the base in `registry.meta.platform_domain`.
Web gets the org from the Service's Namespace labels. The app host must name that org and match the platform base in the web settings.
Environment coordinates contain org and environment.
Project database coordinates also contain the project.
`wamn_registry::OrgId`, `ProjectId`, and `EnvName` own the three coordinate names.
`wamn_registry::ProjectEnvKey` owns composed deployment keys.
Their constructors parse external strings. Internal calls carry the types, and wire formats keep strings.
SQL decoding uses the same constructors and refuses invalid stored values.
The catalog holds no second environment identity table.
Postgres row security restricts each control and apply login to the org encoded by its authenticated login.
Request settings cannot select a different org.

Org and project records have their own active flags.
An inactive org refuses new commands and logins.
An inactive project refuses admission while other projects in the same env can still serve.
Membership and grant records stay available for activation.

## Organization membership and administration

A principal is global. Membership of an org is explicit.
`identity.org_membership` holds one row for each principal and org, and uses a boolean `active` flag.
`user.create` and `user.invite` create or reuse the principal and activate its org membership.
Control refuses a new or reactivated membership if the user belongs to another org, including an inactive membership.
An active member can hold no project access, a membership of environments, application roles, `project-admin`, or `org-admin`.
A valid account with no effective access is not an authentication error.

An org admin administers every environment, project and application in the org.
A project admin administers the applications and connections of that project.
The system stores each explicit grant in `identity.org_role` or `identity.project_role`.
`project_authority` records inherited administration as an `admin` assignment in each project environment database that the grant covers.

Org token issuance requires active org membership.
A revoke removes only the explicit system grant that it names.
Other system grants and ordinary memberships stay.
The `admin` assignment stays while an org or project grant still covers the user.
Inactive org membership grants no access.
Services retain their explicit application roles.

Deactivation removes the user's system grants within that org and marks the org membership inactive.
Projection disables the retained user row in each affected project environment, revokes its PATs, and removes its application roles.
The global principal and access in other orgs stay.
Reactivation restores the org membership and no removed grants.

Disabling a user clears the global principal’s active flag and revokes password logins.
Control refuses if another org has an active membership for the user.
Project databases keep their user rows. Control's maintenance loop projects the disabled authority into them.
Already issued access tokens have a 900-second lifetime. The disabled user cannot renew.

Identity records password, invitation, session and NATS credential decisions in `identity.authentication_event` in the system database.
Platform operators can read the table. Identity can add rows.
The bootstrap creates the table on a fresh install.
PostgreSQL refuses row updates and deletions. The system keeps every row.

## Native and cloud boundaries

wasmCloud owns compilation, linking, stores, interruption, and cancellation.
The host uses its guest logger with the host tracing exporter.
WAMN retains its publication formats, invocation authority, and resource policies.
[Native alignment](native-alignment.md) records retained implementations and their replacement conditions.

Provider implementations supply registry tokens and object storage through the same platform interfaces.
Google Cloud uses metadata credentials. Local clusters use their declared local resources.
Provider selection does not change application operation contracts.
