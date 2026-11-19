# Deployment

A chart is a Helm package of Kubernetes resources.
Deployment consumes pinned artifacts, whose digests identify exact bytes.
It does not compile source, build JavaScript, publish packages, or select application releases.
A fresh platform contains zero application environments and zero application releases.

[Building](building.md#platform-artifacts) explains how to prepare the chart bundle.
Run `wamn platform deploy <values>` on a developer machine with Helm 4, kubectl, and an administrator kubeconfig.
The command uses the current Kubernetes context. Helm validates the operator's values against the platform chart schema.
Set `WAMN_RELEASE_CHART` to the prepared bundle directory or its published OCI digest reference. You can also pass it with `--chart`.

The command installs or upgrades infrastructure first, then the platform. At the end it waits for the SQL Job to complete and the workloads to roll out. Identity reports readiness through its `/readyz` probe.
Use `--only infrastructure` for cert-manager, CloudNativePG, and Traefik when enabled. Use `--only platform` for the shared platform workloads on an existing infrastructure install.
Use `wamn env apply` to roll out a host image.

## Local platform

Use Docker, kind 0.32, Helm, kubectl, OpenSSL, and the pinned KCL compiler on the deployment host.
Use the `wamn` binary from the selected build.
Keep registry, database, and broker credentials outside committed files.
Certificate subjects must match the endpoint names that their clients verify.

From the source checkout, create the local cluster and OCI registry.

```bash
cargo xtask kind-cluster-create
```

The command reuses the `wamn-local` cluster and the registry at `localhost:5001` if they already exist.
Select the `kind-wamn-local` Kubernetes context for the local deployment.
Copy the kind values file to `platform-kind.yaml` and add the printed `registryEndpoint` block. The kind values file sets the registry to `localhost:5001`. [Kind values](../../deploy/charts/platform/values-kind.yaml)
Prepare the artifacts as [building](building.md#platform-artifacts) describes.

```bash
kubectl config use-context kind-wamn-local
wamn platform deploy platform-kind.yaml --chart /var/tmp/wamn-platform-chart
```

The published application bundle includes the infrastructure, platform, org, environment, and project charts.
Helm installs cert-manager, CloudNativePG, and the ingress controller before the platform.
The platform chart installs the system PostgreSQL Cluster, its SQL Job, shared identity, edge, NATS, the wasmCloud runtime operator, and the collector.
The org chart owns control, the namespace quota, RBAC, and network policies.
Run `wamn org create <org> --owner <email>` with the admin kubeconfig.
The command installs the org chart with public platform settings. Its platform Job writes the control and apply passwords to their org Secrets before the invitation Job runs. [Org chart](../../deploy/charts/org/README.md)

Set `global.maxProjects` to the project stream limit for the installation. Each project in an environment uses one slot.
`wamn env apply` refuses new reservations if their total across all organizations and environments would exceed the limit.
The system SQL Job records the limit in the system database on each platform deploy.
Each NATS pod uses 256 MiB per project for streams and 64 MiB per project for storage overhead.
The resolver has a separate claim when operator mode is enabled. [Platform chart](../../deploy/charts/platform/README.md)

The local collector uses debug export.

Kubernetes and its operators own running resources. Helm owns release revisions.
WAMN stores no generic installation-completion ConfigMaps and has no numbered resume steps.
The database determines whether system initialization completed.
A repeat preserves initialized system data. Incomplete initialization reports a repair action and does not reset the database.
If the system baseline is missing from existing state, restore a trusted system database backup before repeating initialization.
If the baseline differs, use the matching system artifact. Do not replay bootstrap SQL against that database.
For a disposable local platform, use the separate cluster rebuild procedure under [cleanup](#cleanup).

### Local access and owner invitation

When identity uses local mail, it writes invitations inside the identity pod.
An administrator sends invitations through the `user.invite` control route.
The local outbox is temporary pod storage. Keep invitation codes and passwords out of logs.

A port-forward connects a local port to a pod.
The control Service selects the org's control Deployment, `c-<org>`.
Use `<org>.<base>`, where `<base>` comes from the platform wildcard.
The example uses org `local` and host `control.wamn.localhost`.
The edge certificate covers `*.wamn.localhost`, including application hosts.
Export the cluster access file to a private directory:

```bash
umask 077
mkdir -p /tmp/wamn-local-access
kind export kubeconfig --name wamn-local --kubeconfig /tmp/wamn-local-access/kubeconfig
kubectl --kubeconfig /tmp/wamn-local-access/kubeconfig -n local \
  port-forward deployment/c-local 8080:80
```

Keep that command running while you use the service.
Send control requests to `http://127.0.0.1:8080` with `Host: control.wamn.localhost`.
From another terminal, read the unauthenticated control response:

```bash
curl -i -H 'Host: control.wamn.localhost' http://127.0.0.1:8080/wamn_control/control/mine
```

This request must return HTTP 401.
A permitted control query requires a password-login token for the org audience.
The local TLS ingress listens on host port 443.
Port 80 reaches the same ingress controller.
The edge certificate covers the application hostnames.
Trust the local certificate explicitly when using these endpoints.
Kind selects no hosted UI artifact.

### Cleanup

Stop the port-forward with Ctrl-C.
Delete the local cluster when its databases are no longer needed. This deletes its platform data.
Keep the shared registry container and its volume for later builds.

```bash
kind delete cluster --name wamn-local
```

To rebuild a disposable platform, repeat the local platform steps.
A new platform starts with zero applications. Redeploy each required application through `wamn deploy`.

## Google Cloud platform

Provision the GKE cluster and select its administrator context. [Google Cloud prerequisites](gcp.md#cloud-credentials-and-prerequisites)
Copy the GCP values file to `platform-gcp.yaml`. Set the registry, project, zone, domain, and buckets for your installation. [GCP values](../../deploy/charts/platform/values-gcp.yaml)
Use the chart bundle published to your registry.

```bash
wamn platform deploy platform-gcp.yaml
```

The GCP collector uses its workload identity to export traces and logs to the telemetry bucket. It uses a persistent queue.

## Application and schema operations

The native control service starts application operations as Kubernetes Jobs.
A Job runs one command to completion.
These routes require a password session and administrator authority for the requested project.
The host supplies the organization.
Every request supplies `project` and `env`.
POST requests put those members under `value`.
GET requests use the existing read query parameters.
POST requests also supply `request_id` in the existing request envelope.

`POST /wamn_control/application/deploy` takes `application` and a published `release` digest.
The worker checks enabled workflows, applies the package SQL, then upgrades the application chart.
`POST /wamn_control/application/rollback` takes `application` and an optional `revision`.
It checks enabled workflows against the target and restores the chart content.

`POST /wamn_control/schema/diff` requires `application` and accepts an optional `release` digest.
`POST /wamn_control/schema/apply` requires both.
`POST /wamn_control/schema/reset` resets the selected project in a disposable environment.
These routes expose no system SQL operation.

Each start response returns `job`, the Kubernetes Job name.
`GET /wamn_control/operation/status` takes `project`, `env`, and that `job` reference.
It returns `state` and a log tail of at most 100 lines and 32 KiB.
States are `pending`, `running`, `succeeded`, and `failed`.
Every read repeats project authorization and compares the Job's target with the request.
Jobs stop after 15 minutes. The CLI polls for up to 16 minutes.
Kubernetes removes completed Jobs after one hour.
There is no operation history database.

The server selects the image, command, namespace, actor, chart, and credential references.
Operation workers stage the org's apply login and system database CA in the Job's in-memory volume.
They stage project logins and the project CA as needed.
Reset advances replication through the capture login and removes package objects through their migration login.
SQL steps run inside the authorized operation Job with their owning role credentials.
The control image runs workers and reads the installed chart reference.
The installed control configuration names the pinned worker image and its configuration.
Deploy, rollback, stop, schema apply, reset, and capture-gap recovery and close all use the org worker ServiceAccount because they run Helm.
Schema diff alone uses the project operation ServiceAccount. It can read declared application workloads and stage named migration credentials and database CAs in the Job's database volume.
Configure creates the CDC reader's ServiceAccount from `operations.capture`. The Deployment mounts the replication login and the project's database CA certificate. Delete removes the account with the reader.


Set `WAMN_CONTROL_URL` to the deployed control host and `WAMN_TOKEN` to your password session token.
`wamn deploy` and `wamn rollback` require these control credentials.
The published `wamn schema diff`, deployed `schema apply`, and `schema reset` commands also use those routes.
The CLI starts the Job and polls `operation.status`.
It prints one line when the state changes and prints the log tail once when the Job ends.
A refused route or failed Job exits nonzero.
The CLI never retries a refused route through direct access.
Remote operations need no kubeconfig, target database URL, or local chart.
The environment file names the environment, its projects, and their applications.
An application command finds the project from the application name.
Control selects the runtime inputs.

Source publication stays local and returns the top package OCI digest.
Source builds use [committed PostgreSQL describe evidence](building.md#wamn-build) and the existing registry inputs.
To refresh the evidence during a source deploy, pass `--refresh-describe` with `--database-url` or set `DATABASE_URL`.
Set `WAMN_PACKAGE_ARTIFACT_BASE`, `WAMN_COMPONENT_ARTIFACT_BASE` and `WAMN_REGISTRY_AUTH_FILE` for publication.
`WAMN_OCI_CA_PATHS` supplies optional registry certificates.
Control derives HTTP registry access from the installed registry address using the loopback rule.
`wamn publish` uses the existing build output and starts no Job.

Application, schema and administration commands require `WAMN_CONTROL_URL` and `WAMN_TOKEN`.
Control selects the installed database, chart, registry and broker configuration.
The CLI sends database operations to the control service.
See [Native control](../architecture/control.md) for the service and worker configuration.

The registry stores applied project settings, application hostnames, client pins, and connection aliases.
The shared environment owns its target, policy, and connection versions.
The top package pins its base packages by OCI digest.
Helm records the release history. A WorkloadDeployment exposes the applied package digest and Helm revision.

Use one `environment.yaml` file for each environment. Each application name must be unique in the file.
The [environment schema](../../crates/environment/environment.schema.json) describes the YAML fields.
An application connection alias names a connection from the shared `connections` map.
Set `revocation_seconds` to an integer from 0 to 3600 for the environment.
Deploy sets this grant cache lifetime on each application workload. Zero disables the cache.

```yaml
org: acme
env: dev
target: cloud
policy: dev
revocation_seconds: 60
database:
  instances: 1
  storage: 2Gi
  cpu: 200m
  memory: 256Mi
connections:
  orders-api: {version: 2}
projects:
  - name: orders
    applications:
      shop:
        hostname: shop.example.com
        connections:
          orders: orders-api
  - name: reporting
    applications:
      reports:
        hostname: reports.example.com
        connections:
          orders: orders-api
```

Helm installs, upgrades, and rollbacks use `--server-side=true`.
Environment and application installs and upgrades pass `--skip-crds`. The installation release provides the runtime resource types.
Org release records go in `wamn-helm-<org>`. The charts use an explicit org namespace value for their workloads and credentials.
`wamn org create` creates the storage namespace before installing the org chart.
Use this layout for new installs. Existing Helm records and their resource ownership stay in place.
The kstatus watcher retains `list` and `watch` on the non-Secret kinds that environment and application charts render.

`wamn env apply environment.yaml` holds one environment lock.
The worker reserves the selected projects, asks identity to publish the environment account, and writes the host and admin credential Secrets. It starts one environment chart upgrade with `--wait=watcher` before it configures the projects.
It upgrades one project chart per project and runs that project's SQL in file order.
After the last project succeeds, the worker checks the environment chart's Helm result and removes unused connection pins on a full apply.
If a project fails, the worker stops and names the project. Fix the error and run the command again to continue.
New projects later in the file stay reserved without applied configuration.
An omitted project schema mode follows the environment's disposable setting.
Use `--disposable` only when first recording an environment that can be reset.
The project chart declares one PostgreSQL Cluster and five managed logins per project.
The environment YAML sets the database size for all projects. The installation values pin the PostgreSQL image.
Project upgrades keep the login passwords and omit the Helm wait flag.
See [Backup and recovery](backup-and-recovery.md) for backup settings.
The worker reads each required PostgreSQL cluster, Database, and reader Deployment by name.
The project chart creates the PostgreSQL publication for change data capture (CDC).
For each project, the worker waits for the database to be applied and allows up to 120 seconds for it to accept a SQL connection.
CloudNativePG creates the database with `wamn_migration` as owner. The worker installs the project schema before restoring platform migrations. It checks the publication in SQL before it creates the replication slot.
The CDC reader waits up to 120 seconds for its slot before it starts replication.
The worker creates the stream with the environment's `stream-admin` credentials and saves the project's applied configuration.
The project chart mounts the YAML document in later project Jobs.
The reader stays paused while a capture gap is open.
The shared host group uses the runtime operator chart at version 2.12.2. The host reads NATS user credentials from `/etc/wamn/nats/` and trusts the broker CA at `/etc/wamn/nats-ca/ca.crt`.

Use `wamn env delete ORG/ENV/PROJECT` to remove a project deployment. Helm keeps the Cluster and ObjectStore by default. Add `--data` and confirm the coordinate to remove both resources.
Delete stops the reader before it removes the stream, then removes the replication slot and publication.
Delete removes the project’s CDC and recovery credential Secrets. For the last project it also removes the host group, the shared credentials, and the environment account.
These lifecycle requests return Jobs. The CLI polls `environment.operation_status`.
Use `wamn revoke-pat --project PROJECT --env ENV --prefix PREFIX` from an org admin session.
Other lifecycle operations take the coordinate and read the registry.
Helm history records the applied release. The top package's deploy section pins its dependency closure.
The configuration export contains no code selection.
Use a source package directory for source deployment. Use an application release digest for deployment without source:

```bash
wamn deploy apps/sample_app --application sample_app --env environments/dev/environment.yaml
wamn deploy --release sha256:<release-digest> --application sample_app --env environments/dev/environment.yaml
wamn rollback sample_app --env environments/dev/environment.yaml
wamn rollback sample_app --revision 3 --env environments/dev/environment.yaml
wamn status --env environments/dev/environment.yaml
wamn status sample_app --env environments/dev/environment.yaml
```

Status reports each project in the file. An application name selects its project.
For each workload, status reports the deployed package digest, content revision, and workload conditions.

Publication needs no destination database.
Run `wamn env apply environments/dev/environment.yaml` before deploying the application.
Each application owns its Helm history, workloads, Services, and route ConfigMaps.
The environment chart owns one host for each org and environment. All projects share it.
Application workloads and file Services select that host. Database credentials and event bindings remain separate for each project.
Status reports application release state.
The route ConfigMaps carry `wamn.route=true`. The edge reads that label.
A source deploy builds and publishes once. A digest deploy starts with the published package.
Both forms submit one Job. The worker resolves the package closure and environment bindings under the lifecycle lock.
It refuses if an enabled workflow calls a sibling or export absent from the candidate.
It allows up to 120 seconds for the project database to accept a SQL connection.
It applies pending SQL and creates the application principals before running `helm upgrade --wait=watcher`.
Deploy preserves installed data. Write migrations that remain compatible with the code already running.
If the Helm upgrade fails, the SQL that committed remains installed. Retry with the same package digest after fixing the failure.
Environment apply refuses unfinished runs in the selected application.
Environment delete checks all applications in the project environment unless you pass `--data`.
If the lifecycle lock session fails, the operation stops and returns an error.

### Select SQL independently

SQLx migration versions and checksums identify applied SQL.
Schema diff reports pending files, missing applied files, and checksum mismatches.
`schema.mode` accepts `reset` and `migrate`.
If omitted, the mode is `reset` only when `registry.environments.disposable` is true. Otherwise, it is `migrate`.
An environment name or explicit `reset` mode grants no authority to discard data.
Editing the mode alone executes no SQL and changes no application selection.

```bash
wamn publish apps/target/wamn/sample_app
wamn schema diff sample_app --env environments/dev/environment.yaml
wamn schema apply sample_app --env environments/dev/environment.yaml --release sha256:<package-digest>
wamn schema reset --project sample_project --env environments/dev/environment.yaml
```

Schema diff uses the live package digest unless you supply `--release`.
Schema apply uses the supplied package digest and applies migrations in dependency order without upgrading the application chart.
An ordinary migration file and its completion record commit together.
`catalog.packages` stores the current package version and artifact source.
A separate SQLx history table records each package and environment's applied versions and checksums.
Authored and generated history migrations run through the same SQLx sequence.
The restricted migration connection keeps its actor binding across commits.
The operation worker selects the migration login from its server-owned connection configuration.
Both connections retain their own password and TLS parameters.
Kubernetes callers use the managed role's basic-auth Secret.
A separate trusted connection records package completion and grants.
SQLx refuses edited or missing applied migrations.
A failed transactional file rolls back its effects and its history row together.
Earlier committed files remain applied.

### Reset target and recovery

An explicit schema reset selects the whole project database.
Type the project name at the terminal prompt. Every application in that project is affected.
The registry must authorize data loss by recording the environment as disposable.
Reset stays within the selected project database.

Before stopping users, WAMN resolves all retained package artifacts and validates their migration files.
The lifecycle lock excludes application deployment during reset.
Reset stops application hosts and event users.
It drops the objects owned by the migration login and restores that login's grants.
It clears application catalog state, runs, queued events, and authored workflows.
It then applies every selected package's migrations in dependency order.

Platform-owned schemas, database credentials, CDC publications, and replication slots remain.
The platform owner owns application schemas. The migration login owns the objects inside them.
Reset keeps model schemas and their publication bindings, and removes the migration-owned objects inside them.
Apply and reset use one helper to grant USAGE to both runtime roles and USAGE and CREATE to the migration login.
These grants name each model schema explicitly. Reset excludes the separate history schema from its model schema selection.
CDC preparation restores capture after package apply.
The database keeps its identity.
After migrations succeed, WAMN resumes the existing application selections.
If a reset fails, users can remain stopped. Inspect the Job log before attempting recovery.

### Nontransactional SQL

A file that begins with `-- no-transaction` runs outside a transaction.
SQLx records it after it succeeds.
A crash before that record causes a retry to execute the file again.
Inspect and repair partial effects before retrying a failed nontransactional file.
WAMN has no separate adoption, started, or resolution state.
Session advisory locks exclude concurrent schema operations across commits.

## Existing application operations

Use an explicitly selected cluster, namespace, and environment for the commands below.
[Repository delivery commands](delivery.md) describe the publication and release-selection interfaces.
[Components](../architecture/components.md) defines their artifact boundary.

### Project integrations and environment connections

Set `WAMN_CONTROL_URL` to the control host URL for the organization.
Set `WAMN_TOKEN` to a session token with project administrator authority.
Create an integration request in `/private/integration.json`:

```json
{"project":"wms","name":"orders-api","integration_type":"http","auth_type":"api_key"}
```

Write the integration:

```bash
wamn integration write --file /private/integration.json
```

Create its environment request in `/private/connection.json`:

```json
{
  "project":"wms",
  "env":"dev",
  "name":"orders-api",
  "parameters":{
    "primary-authority":"https://upstream.example",
    "tls-verification":"verify-authority",
    "auth":{"in":"header","name":"Authorization"}
  }
}
```

Store the credential JSON in `/private/credential.json`:

```json
{"key":"<replacement authorization value>"}
```

Restrict access to the credential file:

```bash
chmod 600 /private/credential.json
```

For authentication types other than `oauth2_authorization_code`, test the candidate before the first write:

```bash
wamn connection test --file /private/connection.json --credential-file /private/credential.json
```

The command uses project administrator authority and answers `{"ok":true}` or `{"ok":false,"error":"..."}`.
It writes no connection generation or credential and stops after ten seconds.

An optional integer `version` in the request selects a published version.
Without it, control uses the version pinned in the environment.
If the connection has no environment pin, control uses the latest published version.
If you omit `--credential-file`, the host uses the existing credential when one exists.
Candidate parameters must pass the same rules as a connection write, including immutable fields and live versions.

An HTTP candidate needs an HTTPS authority and `tls-verification` set to `verify-authority`.
Responses 401, 403 and 5xx fail with their status as the error. Other HTTP responses are healthy.
The control host needs its existing component registry configuration for wasm and native candidates.
It reads `WAMN_COMPONENT_ARTIFACT_BASE` with `WAMN_REGISTRY_AUTH_FILE` or registry token metadata.
Registry, GCS and KMS share the pod metadata token service.
The service refreshes tokens thirty seconds before expiry. It reads no local Google credential file.

Write the connection with its first credential:

```bash
wamn connection write --file /private/connection.json --credential-file /private/credential.json
```

For later credential changes, create `/private/target.json` with the project, `env`, and integration name:

```json
{"project":"wms","env":"dev","name":"orders-api"}
```

Set the replacement credential:

```bash
wamn connection set-credential --file /private/target.json --credential-file /private/credential.json
```

For `oauth2_authorization_code`, set `parameters.auth` to `client_id`, `authorize_url`, `token_url`, and a `scopes` array.
Write the client secret through `--credential-file` as `{"client_secret":"..."}`.
After the write, read the connection metadata:

```bash
wamn connection get --file /private/target.json
```

Register its `redirect_uri` with the provider.
The deployed control host uses `https://<route_hosts.control>/oauth/callback`.
Development uses the installed control service and its public `/oauth/callback` URL.
Start authorization:

```bash
wamn connection authorize --file /private/target.json
```

Open the returned `authorization_url` within ten minutes.
The provider returns to `GET /oauth/callback` on the control host.
A success page means that the connection can serve calls.
Until then, calls return `unauthorized`.
The callback uses its random state as authority and needs no session.

A failed provider exchange leaves the state available for another attempt.
A replay, an expired state, or a generation change returns an error page.
After expiry or a generation change, start authorization again.
A secret-only `set-credential` retains the refresh token and forces refresh with the new secret.

The control host writes credential values through the environment store.
Postgres keeps their references and display hints.
The baseline schema owns fresh installations.

The control routes use project administrator authority.
The `external_system.*`, `integration.*`, and `connection.*` routes manage their respective identities.
The matching `wamn` verbs call these routes with a session token.
Each verb reads its JSON request from `--file`.
The request names the project, and connection requests also name the environment in `env`.
The first connection write reads the credential value from `--credential-file`, except for authentication type `none`.
`connection set-credential` reads later values from that flag too.

### Integration write interfaces

Publish a typed integration with one `--write-interface <interface>` flag for each export that permits writes.
For Modbus, supply `--write-interface wamn:modbus/write@1.0.0` to `wamn integration publish modbus`.
Publication refuses an interface that the component does not export.
Host types use an empty list and enforce the declared mode at run time.
Environment apply refuses a read requirement that imports a declared write interface.

For an exclusive cloud binding, set `replicas: 1` in the body from `WAMN_RELEASE_HOST_GROUP`.
Apply refuses another count or a missing key and names the connection.
The chart default does not satisfy this requirement.
Local targets are exempt.

## Deployment ordering

Build and exercise the exact selected source and artifacts before publication.
Compilation and a successful upload do not establish deployed behavior.
Select the relevant tests from [running tests](running-tests.md) and interpret them through [test results](../testing/evidence.md).

Deploy the platform before using the separate application commands.
Platform deployment owns system initialization, the initial organization, and the identity issuer.
Do not repeat the removed numbered installer or its initial application pass.

Application publication is independent of the destination database.
`wamn env apply` provisions shared environment resources.
The deploy Job applies SQL before upgrading the application release.
Platform deployment performs no application pass.

The optional `client` field pins an application web artifact: `client = {package = "wamn_receiving", web = "<full web digest>"}`.
The web digest is `sha256:` and 64 lowercase hexadecimal digits, as `wamn web push` prints it.
Without `client`, the host serves no application web client, even when the release contains one client package.
Apply pulls the artifact from `WAMN_PACKAGE_ARTIFACT_BASE` with the existing registry credentials and CA paths.
It refuses a web artifact whose package id differs from `client.package` or whose package version is absent from the release.
The revision values keep the host-to-package map in `webClients` and the host-to-web-digest map in `webDigests`.

The same immutable web artifact can serve more than one environment.
Deploy a published release through control:

```bash
wamn deploy --env <document> --application <application> --release <digest>
```

The operation worker holds the environment lock through SQL and the application rollout.
A CI completion order does not set deployment precedence.

The deployed capture owner sets replican identity from the installed published package artifacts.
The development loop reconciles it directly from local source on each activation.
Captured tables use REPLICA IDENTITY FULL so that events include the old row.

The configure Job reads the replica count and duplicate window from the installed event policy.
It compares an existing stream with the source declaration before it starts the reader.
The release chart scopes application access to the project stream and the `evt.<org>.<env>.<project>.>` subjects. See [broker accounts and retention](../architecture/platform-services.md#events).

A physical schema change follows the selected mode and recorded disposable authorization described above.

Read each command's current required arguments with `--help`.

## Publish a package

Run this command after building the package and its bases.

```bash
wamn publish apps/target/wamn/wamn_receiving
```

Set `WAMN_PACKAGE_ARTIFACT_BASE` and `WAMN_COMPONENT_ARTIFACT_BASE` to the package and component repositories.
Set `WAMN_REGISTRY_AUTH_FILE` to a credential file with access to both repositories.
Publication pushes the bases first and records their OCI digests in the dependent packages.
The command prints the digest of the top package. [Package and component formats](building.md#reading-a-pushed-package)


## Web client files

Use [Web upload](web-upload.md#package-clients) to copy the pinned web artifact to a bucket.
Pass the environment document’s `client.web` digest and the registry inputs.
The uploader reads the package id and client name from `web.json`.
It writes `clients/<package>/<web digest hex>/` and skips all writes when the index already exists.
That page owns the command, required inputs, destination paths, output, and errors.
The shell reads `/config.json` from the installed web service before sign-in.
Browser sign-in uses the web host over HTTPS.
The platform chart mounts the `key` entry of `hosts/wamn-web-csrf` in the web Deployment.
Helm generates 32 random bytes on first install and reuses the Secret on upgrades.
The bucket must allow reads without credentials.
Edge deployment remains outside the local platform milestone.

Release hosts expose workload HTTP on port 80, probes on port 8081, and native file transfers on port 8082.
The `<release>-files` Service selects the shared environment host and maps port 80 to the named `files` port.
The edge sends `/api/<application>/files` and its child paths there before the general `/api/` route.
File and workflow requests keep the full path. Other API requests lose the `/api` prefix.

The Rust edge reads the live release and serves its bucket files.
For an application, it selects `clients/<package>/<web digest hex>/` from the host's pinned package and web digest.
The digest uses `sha256:` and 64 lowercase hex digits. A missing or invalid web digest refuses.

The Rust web front generates `config.json` with `org`, `project`, `release` and `web` from the live release.
Both fields contain full `sha256:` digests. The edge serves this file with `no-cache`.
A web-only apply changes the bundle and `web`, while `release` stays the same.

In kind, the Receiving edge case uses the command. [Cluster tests](cluster-tests.md) describes that case.

- An application never learns a deployment fact at build time. The same static files serve every environment.

Ruled: keep `max-age=10, stale-while-revalidate=60`, because no shared cache stores an authenticated list under option 3.3 A. Revisit them with a measurement from the real deployment.

Ruled: the files under `assets/` carry a content hash in their names, so they are `public, max-age=31536000, immutable`. `index.html` is `no-cache`, so a new release reaches the browser at the next load.

## Platform UI files

Use [Web upload](web-upload.md#platform-ui) with the artifact digest that the prepared org chart pins in `control.platformUi`.
The command pulls the artifact from the platform registry and makes sure that its bytes match the digest before any bucket write.
The default destination is `gs://wamn-dev-web/platform-ui/sha256-<hex>/`.
Organization configuration lives separately at `platform-ui/orgs/<org>/config.json`.
The chart writes the UI digest and organization into `release.json`.
The edge selects shared files by that digest and reads configuration from the organization path.
Deployment needs no source checkout, pnpm installation, or local UI output.
Kind keeps an empty web bucket and selects no platform UI.

The control root is `https://control.wamn.dev`.
The org chart runs `wamn-control --settings /etc/wamn/control/settings.json platform` in the org namespace.
The native service has no route guest or application release.
The control Service selects that Deployment.
Local platform deployment installs control without a UI upload. GCP integration for public control access remains unfinished.

The org chart writes the control host and Service address to a route ConfigMap.
The edge forwards `/api/` to the control Service and `/password/` to identity.
The Rust web front serves platform UI files and generates its org configuration.
Every other page reaches that prefix's `index.html`, including `/`, `/invite`, `/recover`, `/reset`, and control or project-environment audience paths.

## Google Cloud edge

On Google Cloud, the HTTPS ingress sends traffic through a network endpoint group to the Rust HTTP Service on port 80.
TLS stays at ingress. A cluster ingress uses the serving certificate that cert-manager owns.
The infrastructure chart installs a default TLS ingress controller on non-GCP clusters.
The Rust front owns response headers and proxies password requests to identity over HTTPS.
The CDN caches only what the origin marks as cacheable.
Set `neg: true` in the edge values. The edge Service then carries the annotation that makes GKE create the network endpoint group.
The chart renders no Google Cloud resource and no other difference from kind. [Google Cloud prerequisites](gcp.md#cloud-credentials-and-prerequisites) describe the external inputs. Deployment integration remains unfinished.
The edge keeps `/api` for native file requests and removes it for other API requests.
It makes sure of the identity certificate, as it does in kind.

## Identity password configuration

`wamn-identity serve --settings <path>` reads one JSON file. The chart renders the issuer, public database connection, envelope provider, and mail mode into that file.
The pod mounts the database credentials, TLS keys, local wrapping key, and Resend token as files.
When `identity.authNats.tlsSecret` is set, the chart adds `global.nats.schedulerUrl` and the CA path to identity settings. Identity mounts `ca.crt` from that Secret.
Identity creates the first session signing key at startup. CloudNativePG manages the issuer login. Identity uses the client certificate and key from `wamn-identity-db-tls`.

Create a Secret in `platform` with an `api-key` entry.
Set `identity.resendSecret` to that Secret and `identity.resendFrom` to the verified sender in the platform chart.
Set `identity.inviteUrl` to the web client base, for example `https://receiving.wamn.dev`.
The invitation mail links to `<inviteUrl>/invite#<code>`.
The reset mail links to `<inviteUrl>/reset#<secret>`.
If `inviteUrl` is empty, each mail shows the code.
For kind, use the local mail file described in [local access](#local-access-and-owner-invitation).

Use `user.create` to add an org member without sending mail. Use `user.invite` to send an invitation.
Restart the Deployment after changing the mounted Resend token or sender.

Control reads its projected service account token from `/var/run/secrets/wamn-identity/token`.
The org chart projects the token with audience `wamn-identity`. Identity uses Kubernetes TokenReview through the `wamn-token-reviewer` ClusterRole.

The route bodies and limits are in [password enrollment](../architecture/execution.md#password-enrollment-foundation).

## User roles

`apply-package` writes the built-in role `admin` in each project.
`admin` has no permission rows and holds every operation that the serving release serves.
Every other role is authored in the project, and it holds the stable operation references `<package>:<interface>/<operation>` granted to it.
A user or a service reaches no route and no environment at sign-in until it holds a role.

Give a role with `wamn grant-role`, and take it away with `wamn revoke-role`.
Both verbs use the application host URL and the caller's session token.
The target user is named by principal UUID:

```bash
wamn grant-role --host-url "$WAMN_CONTROL_URL" --token "$WAMN_TOKEN" \
  --user-id <principal UUID> --role <authored role>
```

The verbs refuse direct assignment or removal of `admin`.
Grant or revoke system project-admin or org-admin to change application administration.
Control records `admin` in each project environment that the grant covers.
Revoking the last grant that covers the user removes that assignment.
The target needs an existing application user or service row.
The HTTP owner refuses unknown users. A grant also refuses a missing role.
Service principal email addresses use `<subject>@<platform domain>`.
Create an API service and its PAT through control with `wamn create-api-service`.

### Authored roles

Create an authored role with `wamn create-role`, and delete it with `wamn delete-role`.
The delete also removes the role's assignments and permissions.
The verbs take the same host and session arguments as `grant-role`.
The `--role` argument names the role.
A role name has lowercase letters, digits, and hyphens after the first character, and at most 64 bytes.
The verbs refuse `admin`.

Select an operation for an authored role with `wamn grant-permission`:

```bash
wamn grant-permission --host-url "$WAMN_CONTROL_URL" --token "$WAMN_TOKEN" \
  --role <role> \
  --operation <package>:<interface>/<operation>
```

The grant stores the operation even when the serving release does not serve it.
`wamn revoke-permission` removes the operation, and it refuses an operation that the role does not hold.


## Mailbox-loss recovery

An authorized administrator approves the replacement email and updates the existing user principal.
There is no separate trusted-contact channel or self-service email change.
The user then uses normal password recovery at the replacement address.

Use an authorized administrative connection to the identity service's `wamn_system` database.
The connection must permit `SET ROLE wamn_system`.
Do not use the targeted identity issuer credential, which cannot edit principals.
Use the administrator's existing principal UUID as the audit actor.
Use the affected user's existing principal UUID as the target.
Do not create another principal or change its subject, memberships, or roles.

Run this transaction in `psql` on that administrative connection:

```sql
\set ON_ERROR_STOP on
\prompt 'Administrator principal UUID: ' administrator_id
\prompt 'Affected user principal UUID: ' principal_id
\prompt 'Replacement email: ' replacement_email
BEGIN;
SET LOCAL ROLE wamn_system;
INSERT INTO platform.tx_identity(principal_id, role, application, operation)
VALUES (:'administrator_id'::uuid, '', '', 'admin:change-email');
SELECT EXISTS (
    SELECT 1 FROM identity.principals
    WHERE id = :'principal_id'::uuid AND type = 'user'
    FOR UPDATE
) AS account_found \gset
\if :account_found
UPDATE identity.principals
SET email = lower(btrim(:'replacement_email'))
WHERE id = :'principal_id'::uuid AND type = 'user';
UPDATE identity.password_tokens
SET consumed_at = clock_timestamp()
WHERE principal_id = :'principal_id'::uuid AND consumed_at IS NULL;
UPDATE identity.password_logins
SET revoked_at = clock_timestamp()
WHERE principal_id = :'principal_id'::uuid AND revoked_at IS NULL;
COMMIT;
\else
ROLLBACK;
\echo 'No matching user principal. Nothing changed.'
\endif
```

The database enforces email format and uniqueness. A failed statement leaves the transaction uncommitted.
If a statement fails, run `ROLLBACK` before correcting the input.
The principal lock serializes this update with password login, enrollment, reset, and renewal.
Old email secrets and renewal credentials cannot survive a successful correction.
Issued password tokens fail new admission after this transaction commits. PATs retain their separate revocation procedure.
The correction does not reactivate a disabled account or replace its password.

After commit, ask the user to request normal recovery with the replacement email.
The recovery endpoint and reset fields are in [password enrollment](../architecture/execution.md#password-enrollment-foundation).
Keep reset secrets and passwords out of command arguments and logs.
After reset, require normal login with the new email and password.
Control membership routes project committed authority into affected application databases.

## Identity target credentials

The identity Deployment in `platform` reads its database connection from the mounted settings file at startup.
The chart rolls the Deployment when the settings file changes.
The readiness probe checks that the database answers and that the issuer has an active signing key.

## Platform schema upgrades

The `provision-system` Job in `platform` initializes an empty system database and applies pending system migrations from the control image.
Each platform upgrade runs the Job again. It preserves the baseline and skips migrations recorded in SQLx history.

Set `WAMN_CONTROL_URL` and `WAMN_TOKEN` for project SQL operations.
Supply the environment document with `--env`.
Control selects its installed chart and pinned worker image and runs a bounded Job in the org namespace.

```bash
wamn platform schema diff --target project --env environment.yaml
wamn platform schema apply --target project --env environment.yaml
```

Both commands run the installed immutable `control` image.
Both read the migration bytes embedded in that image.
Diff writes no SQL state.
It reports installed and pending SQLx versions, checksums, and missing files.
If the image lacks an applied file, apply refuses.
Each transactional file commits with its SQLx history row.
A later failure keeps earlier commits.
Files that begin with `-- no-transaction` need safe retries after partial execution.

Control reads the environment's recorded database instance and installed connection projection.
The project SQL Job uses the project's migration login.
The platform owner roles stay `NOLOGIN`. Migration logins inherit their privileges through membership.
Platform migration execution refuses administrator sessions.

```bash
wamn platform schema diff --target project --env environments/dev/environment.yaml
wamn platform schema apply --target project --env environments/dev/environment.yaml
```

The Job holds one exclusive environment lock for the whole operation.
It visits each project in file order and stops on the first error.
It changes WAMN substrate SQL and `platform._sqlx_migrations` in each selected project database.
Application tables, rows, code selections, Helm releases, and other environments stay unchanged.
The operation runs no environment provisioning or application activation.

Fresh environment provisioning installs the catalog and project substrate, then runs every embedded project migration.
Repeated provisioning runs no pending migration.
If execution stops, the error reports completed and remaining files.
Resume with `wamn platform schema apply --target project --env FILE` with the same installed worker image.
Application package migrations keep their own streams.

## Rollback and maintenance

Use `wamn rollback APP --revision N --env FILE` to restore the content of revision N.
Without `--revision`, the worker finds the newest earlier deploy. It skips revisions that record a rollback.
If no older deploy exists, the command reports the current content revision and refuses.
The worker reads the target package digest from Helm, checks enabled workflows, and runs `helm rollback --wait=watcher` under the lifecycle lock.
Rollback keeps installed SQL migrations.

The WorkloadDeployment annotation `wamn.release-revision` names the revision whose content is live.
Helm restores the saved manifest when it creates a rollback revision. After a rollback to N, status still reports N.
Use the same revision numbers with `--revision`.

Retention runs through its owning library interfaces.
For backup and recovery, use CloudNativePG backup and recovery on the cluster.
The procedure is in [backup and recovery](backup-and-recovery.md).
Use its `--help` output for the selected action and required credentials.
These maintenance verbs do not authorize shared development targets or a new schema lifecycle.

## Trusted component reuse

The deployment owner selects exact reviewed component digests on `wamn-host`.
Keep the selection empty when guest-code correctness is not a sufficient isolation boundary.
Review and test the component before granting trust.
Require alternating callers on an observed reused instance, request-local data, and no unfinished guest tasks after return.
[Native dispatch](../architecture/execution.md#native-dispatch) defines the host and component responsibilities.

Pass each selected digest with `--trusted-warm-component-digest sha256:<hex>`.
Alternatively, set `WAMN_TRUSTED_WARM_COMPONENT_DIGESTS` to a comma-separated list of exact digests.
Do not copy this selection into application manifests.
A changed artifact requires a new review and a new deployment selection.

Set `--component-pool-size` or `WAMN_COMPONENT_POOL_SIZE` to a positive count. The default is one instance per eligible component.
Set `--component-reclaim-window-seconds` or `WAMN_COMPONENT_RECLAIM_WINDOW_SECONDS` to a positive number of seconds. The default is 60 seconds.
The native pool uses `maxConcurrency = 1`, no retained minimum, and a 1,000-call instance limit.
Pool overflow uses fresh stores and remains subject to the existing admission and memory limits.
Restart the host to change trust or pool configuration. The replacement process creates new pools.

### Environment apply credentials

Set `WAMN_SYSTEM_ADMIN_URL` to the administrator connection configuration for the ensure phase.
CloudNativePG declares the restricted apply login. The platform SQL Job grants its system database privileges.
The installer supplies that public apply connection object in the native control configuration.
The other apply phases use the restricted credential from the worker's database volume.

## PostgreSQL client TLS

Runtime pools, OAuth transactions, and resource leases use SQLx with verified TLS.
Component configuration names the database host, port, database, login, login Secret, and CNPG CA Secret.
The login and CA Secrets live in the consumer’s namespace. Shared identity and the platform SQL Job mount them in `platform`. Hosts, CDC readers, and control use read-only mounts in their org namespace.
Operation workers copy the selected Secret keys to a memory-backed volume as the database names become known.
The database client reads `ca.crt` from `/etc/wamn/db/<login-secret>/` to verify the server.
For a password login, the client reads `username` and `password` from that directory.
It makes sure that the username matches the configured login.
For identity's system database login, SQLx reads `tls.crt` and `tls.key` on each new connection. PostgreSQL matches the certificate's common name to the configured login.
The control administration directory supplies the password login files and `ca.crt` for project connections.
Explicit operator connection strings must name a readable CA file through `sslrootcert`.
The database hostname must match the server certificate.
The provisioned CNPG clusters accept client connections only over TLS.

## Kubernetes network policies

Org network policies deny ingress and egress by default. Supply the installation's API server and registry destinations as CIDR and TCP port pairs.
The environment and project charts allow connections to their host group and database. The edge accepts ingress from Traefik on installations that use Traefik.
Application deploy adds a public IPv4 egress policy for the TCP ports of that release's wasm integration authorities.
The rule excludes fixed private and special-use ranges. The runtime still checks each integration's allowed hosts.

## Kubernetes credentials

The project chart creates an empty `wamn-credential-<org>--<env>--<project>` Secret with one key per credential UUID.
Control reads and patches that Secret by name. Each merge patch updates one credential key and the labels control owns.
Use Helm server-side apply for the project chart so the Secret keeps its credential keys on upgrade.
Helm retains the Secret during uninstall.
The database stores the Secret name and entry name as a `k8s` reference.
File-backed credential storage and `WAMN_CREDENTIAL_DIR` are removed.
Deployed hosts use their in-cluster ServiceAccount to read the Secret by name.
Each host process shares one reader, which caches successful reads by project coordinate and credential UUID.
Concurrent misses share one API request. An HTTP API response of 401 or 403 refreshes the credential before one retry.
Environment teardown deletes the Secret for each project release in that environment.
Local development hosts have no connection credential store.
A credential read returns `no credential store in local mode` and logs that reason.
Local startup still provisions the disposable databases and starts identity, control and web services.
Those processes use explicit operator connection URLs with verified TLS.
They do not supply a local connection credential store.
Identity login uses the org audience `urn:wamn:org:<org>`.
The identity Deployment mounts the system login and CA certificate.

The environment chart renders a settings ConfigMap for each host group using the endpoints from the org control settings.
Hosts and control derive the JWKS URL and org audience, and read the public platform identity CA from the org's `identity-ca` ConfigMap. Control sends identity a projected service account token for the `wamn-identity` audience.

The HTTP interface keeps its `credential-unavailable` result.

Connection administration passes the registry's credential Secret name and the coordinate values to `wamn-kubernetes`.
Separate credential keys can change independently. The project chart must create the Secret before control writes credentials.

The broker retains the static `PLATFORM` account and authentication through client certificates. Org account provisioning requires the migration to operator mode in Epic 10.2.

### Transaction identity retention

The control service removes expired transaction identities every minute in a separate task.
Cleanup runs do not overlap. Each database operation has a five-second limit.
Shutdown cancels the task before the service drains HTTP connections.
It connects to each configured project as `wamn_platform_login`.
It deletes rows with `xid < pg_snapshot_xmin(pg_current_snapshot())`.
The UNLOGGED table `platform.tx_identity` stays outside CDC publications. A crash discards its rows.

Before merging an identity change, measure read COMMIT latency with the deployed Postgres major version. The private test server is sufficient:

```sh
wamn-test-postgres --database identity_measurement --url-env DATABASE_URL -- sh -ec '
  psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -f deploy/sql/record-history.sql
  python3 test-support/measure-tx-identity.py "$DATABASE_URL" --samples 200
'
```

The script needs psycopg 3 and a login that can insert identity rows.
It reports COMMIT round trips with and without the insert, WAL counters from `pg_stat_wal` and `pg_stat_io`, and the flushed WAL position. It creates no schema and changes no server configuration.
