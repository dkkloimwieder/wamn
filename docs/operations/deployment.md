# Deployment

`wamn platform deploy` installs the platform in a local kind cluster from a published chart digest.
A chart is a Helm package of Kubernetes resources.
Deployment consumes pinned artifacts, whose digests identify exact bytes.
It does not compile source, build JavaScript, publish packages, or select application releases.
A fresh platform contains zero application environments and zero application releases.

[Building](building.md#local-platform-artifacts) owns artifact production.
[The deployment plan](../plan/deploy.md) owns application schema rules and the deferred GCP work.
[Google Cloud prerequisites](gcp.md#cloud-credentials-and-prerequisites) remain external inputs. GCP deployment integration is unfinished.

## Local platform

Use Docker, kind, Helm, kubectl, OpenSSL, and the pinned KCL compiler on the deployment host.
Use the `wamn` and `wamn-ctl` binaries from the selected build.
Keep registry, database, and broker credentials outside committed files.
Certificate subjects must match the endpoint names that their clients verify.

From the source checkout, prepare the registry and artifacts as [building](building.md#local-platform-artifacts) describes.
The example uses `localhost:5001`, as `deploy/kind/cluster.k` does.
The published chart includes the pinned infrastructure, platform, org, and environment charts.
On kind, `wamn platform deploy` reads `WAMN_REGISTRY_AUTH_FILE` to create the org namespace’s `wamn-registry-pull` Secret.
Set this variable to the platform registry credential file before deployment.
Deploy the chart reference that the build command prints:

```bash
wamn platform deploy deploy/kind/cluster.k --chart oci://localhost:5001/wamn/charts/release@sha256:<digest>
wamn platform status deploy/kind/cluster.k
```

Deployment creates or reuses `wamn-local` and connects its registry to the kind network.
Helm first installs cert-manager, CloudNativePG, and the TLS ingress controller.
The platform chart then installs certificates, NATS, the wasmCloud operator, and the collector.
The org chart creates PostgreSQL in the org namespace.
Bootstrap Jobs initialize SQL and credentials before the service components start.
Identity, control, and the edge belong to the platform Helm release.
Without a recorded org control release, deployment initializes `wamn_system`, the organization, identity, and control.
Once control exists, authenticated platform updates select an existing owner with `--only`.
Reapply does not refresh credentials.
It does not rewrite the registry pull Secret, the Resend key Secret, the control operator Secret, or the identity signing keys.
The command returns success only when every required component is ready.
An error preserves a nonzero exit and prints the status that remains observable.

Deployment also works outside the source checkout.
Copy the cluster document to the deployment host.
The host fetches the published chart by digest.
The GCP collector exports traces and logs to the declared telemetry bucket with a persistent queue.
Its chart uses the existing collector workload identity. Local clusters use debug export.
If the checkout is absent, set the existing `WAMN_KCL` variable to the pinned compiler path.
[Building](building.md#package-manifests) describes that compiler.
Supply the document path and the chart reference, with the required tools available.
The deployment host must reach the selected registry and its pinned artifacts.
A loopback registry refers to that host, so its artifacts must exist there before deployment.

Helm records the chart reference for later operations.
Status reads that reference and reports selected artifacts separately from running images.
It reads Helm, Kubernetes, certificate, service, and system database state.
Its categories are `ready`, `incomplete`, `failed`, and `unknown`.
A failed query reports `unknown`. Status never creates a missing cluster.
Unselected UI, external mail, or telemetry does not prevent platform readiness.

If bootstrap stops before control exists, repeat it with the same document and chart:

```bash
wamn platform deploy deploy/kind/cluster.k --chart oci://localhost:5001/wamn/charts/release@sha256:<digest>
```

Kubernetes and its operators own running resources. Helm owns release revisions.
WAMN stores no generic installation-completion ConfigMaps and has no numbered resume steps.
The database determines whether system initialization completed.
A repeat preserves initialized system data. Incomplete initialization reports a repair action and does not reset the database.
If the system baseline is missing from existing state, restore a trusted system database backup before repeating initialization.
If the baseline differs, use the matching system artifact. Do not replay bootstrap SQL against that database.
For a disposable local platform, use the separate cluster rebuild procedure under [cleanup](#cleanup).

### Local access and owner invitation

Kind uses identity's local mail mode. It writes invitations inside the identity pod and sends no external email.
Deployment invites the document's owner as administrator of the initial organization.
It sends no new invitation when that owner has a password or an unexpired pending invitation.
Deployment prints the exact `kubectl exec` command that reads the local invitation file. It never prints the invitation code.
Use that command to read the invitation privately.
The local outbox is temporary pod storage. Keep invitation codes and passwords out of logs.

Deployment also prints the port-forward command and required HTTP `Host` value.
A port-forward connects a local port to a pod.
The control Service has no selector, because the wasmCloud operator manages its endpoints.
Forward `deployment/hostgroup-c-<org>` in the org namespace.
Use `route_hosts.control` as the HTTP host.
For the committed document, the Deployment is `hostgroup-c-local` and the host is `control.wamn.localhost`.
Kind application hosts use `<application>.wamn.localhost`, covered by the edge certificate for `*.wamn.localhost`.
Export the cluster access file to a private directory:

```bash
umask 077
mkdir -p /tmp/wamn-local-access
kind export kubeconfig --name wamn-local --kubeconfig /tmp/wamn-local-access/kubeconfig
kubectl --kubeconfig /tmp/wamn-local-access/kubeconfig -n local \
  port-forward deployment/hostgroup-c-local 8080:80
```

Keep that command running while you use the service.
Send control requests to `http://127.0.0.1:8080` with `Host: control.wamn.localhost`.
From another terminal, read the unauthenticated control response:

```bash
curl -i -H 'Host: control.wamn.localhost' http://127.0.0.1:8080/wamn_control/control/mine
```

This request must return HTTP 401.
A permitted control query requires a password-login session for the control audience.
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

To rebuild a disposable local platform, delete `wamn-local` with the command above.
This removes its system database, every project database, and all cluster-local application state.
Keep the shared registry and prepare current artifacts through [building](building.md#local-platform-artifacts).
Run `wamn platform deploy deploy/kind/cluster.k --chart oci://localhost:5001/wamn/charts/release@sha256:<digest>` again.
The new platform starts with zero applications. Redeploy each required application through `wamn deploy`.
This procedure does not upgrade a persistent system database.

Successful deployment removes its private work directory unless a new kind cluster mounts its registry files.
Keep that mounted directory while the cluster exists. Reapply updates the original registry mount.
Failed deployments without `--only` retain their named work directory for diagnostics.
Remove only the retained files and directory that this run created after they are no longer needed.

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
It selects and deploys that release's declared application.
`POST /wamn_control/application/rollback` takes `application` and an optional `release` digest.
Both operations change code selections without running SQL.
Initialize application SQL through the explicit schema operation before the first code deployment.

`POST /wamn_control/schema/diff` and `POST /wamn_control/schema/apply` accept optional `application` and `release` members.
Without `application`, they process the environment's selected applications.
An explicit release requires an application.
`POST /wamn_control/schema/reset` resets the whole disposable environment.
These routes expose no system SQL operation.

Each start response returns `job`, the Kubernetes Job name.
`GET /wamn_control/operation/status` takes `project`, `env`, and that `job` reference.
It returns `state` and a log tail of at most 100 lines and 32 KiB.
States are `pending`, `running`, `succeeded`, and `failed`.
Every read repeats project authorization and compares the Job's target with the request.
Jobs stop after 15 minutes.
Kubernetes removes completed Jobs after one hour.
There is no operation history database.

The server selects the image, command, namespace, actor, chart, and credential references.
Deploy, rollback, and diff use the existing apply, registry, and NATS credentials.
Apply and reset also receive the project platform-migration, package-migration, and capture credentials.
Reset advances replication through the capture login and removes package objects through their migration login.
SQL steps run inside the authorized operation Job with their owning role credentials.
The control image runs workers and reads the installed chart reference.
The installed control configuration names the pinned worker image and its configuration.
Environment provisioning creates separate code and SQL ServiceAccounts and namespace Roles.
Reset uses the existing lifecycle worker account to stop and resume the shared environment host.
Each project's CDC account reads only its replication credential and database CA.


Set `WAMN_CONTROL_URL` to the deployed control host and `WAMN_TOKEN` to your password session token.
`wamn deploy` and `wamn rollback` require these control credentials.
The published `wamn schema diff`, deployed `schema apply`, and `schema reset` commands also use those routes.
The CLI starts the Job and polls `operation.status`.
It prints one line when the state changes and prints the log tail once when the Job ends.
A refused route or failed Job exits nonzero.
The CLI never retries a refused route through direct access.
Remote operations need no kubeconfig, target database URL, or local chart.
The environment file supplies the project and environment names.
The server reads the canonical document and selects all runtime inputs.

Source publication stays local and submits only its release digest.
Source builds require `DATABASE_URL` for disposable build databases and the existing registry inputs.
Set `WAMN_PACKAGE_ARTIFACT_BASE`, `WAMN_COMPONENT_ARTIFACT_BASE`, `WAMN_RELEASE_ARTIFACT_BASE`, and `WAMN_REGISTRY_AUTH_FILE` for publication.
`WAMN_OCI_CA_PATHS` supplies optional registry certificates.
For an HTTP registry, the existing `WAMN_RELEASE_HOST_GROUP` document supplies `--allow-insecure-registries` in `extraArgs`.
`wamn schema publish` only publishes and starts no Job.

Application, schema and administration commands require `WAMN_CONTROL_URL` and `WAMN_TOKEN`.
Control selects the installed database, chart, registry and broker configuration.
The CLI has no direct database fallback. Source publication still uses a disposable build database.
See [Native control](../architecture/control.md) for the service and worker configuration.

The applied environment configuration lives in `registry.environment_projects.configuration`.
`wamn-registry` defines its type. Unconfigured deployments hold NULL.
The configuration contains hostnames, policy, schema mode, clients, connections and integrations.
It contains no release digest or package membership. Old document shapes are refused.
Set `revocation_seconds` for the project and each application.
These nonnegative values bound credential revocation delay when an invalidation is lost.
Zero disables auth caching. Restart hosts after changing a bound.
`wamn env apply environment.k` submits the configuration to authenticated control.
The installer reconciles the database and role declarations. The environment library saves the configuration after provisioning succeeds.
A new database receives its initial SQL through an installer bootstrap Job. That worker refuses an existing baseline.
Later configuration updates use restricted connections and never read a superuser credential.
Use `--disposable` only when first recording an environment that can be reset.
Use `wamn env delete ORG/ENV/PROJECT` to remove an environment. Add `--data` and confirm the coordinate in a terminal to delete its database.
These lifecycle requests return Jobs. The CLI polls `environment.operation_status`.
Use `wamn revoke-pat --prefix PREFIX` to revoke a PAT within the authenticated org authority.
Other lifecycle operations take the coordinate and read the registry.
Helm history records the applied release. Each release manifest defines its packages.
The configuration export contains no code selection.
Use a source package directory for source deployment. Use an application release digest for deployment without source:

```bash
wamn deploy apps/sample_app --application sample_app --env environments/dev/environment.k
wamn deploy --release sha256:<release-digest> --application sample_app --env environments/dev/environment.k
wamn rollback sample_app --env environments/dev/environment.k
wamn rollback sample_app --release sha256:<release-digest> --env environments/dev/environment.k
wamn status --env environments/dev/environment.k
```

Publication needs no destination database.
Run `wamn env apply environments/dev/environment.k` before applying application SQL and deploying code.
Each application owns its Helm history, workloads, Services, and route ConfigMaps.
The environment chart owns one host for each org and environment. All projects share it.
Application workloads and file Services select that host. Database credentials and event bindings remain separate for each project.
Status reports loaded application releases and the unfinished runs that pin them.
The route ConfigMaps carry `wamn.route=true`. The edge reads that label.
Code-only deploy, reapply, and rollback preserve installed SQL and other applications' data, permissions, routes, and workloads.
Helm is the only owner of release history. Deploy names the application and release digest.
After a failed head, rollback restores the latest successful revision, including its web client pin.
After a successful head, rollback restores the preceding distinct release and client pair.
An explicit rollback digest selects that release. Rollback preserves installed SQL.
Failed Helm upgrades keep their revision and pods for inspection. Rollback is an explicit operation.

Deployment prepares bindings and grants before Helm changes the application.
Readiness precedes a bounded drain wait, which lets old hosts finish their work.
The policy's `drain_bound_seconds` supplies the pod termination grace period. Observation allows another 15 seconds for pod removal.
Rollout retains older bindings, release facts and generated permissions.
A separate retention operation removes facts that no live host, current selection or unfinished run needs.
If host observation fails, retention refuses cleanup.
Environment status reports unfinished runs with no live host.
If the lifecycle lock session fails, the operation stops and returns an error.

### Select SQL independently

SQLx migration versions and checksums identify applied SQL.
Schema diff reports pending files, missing applied files, and checksum mismatches.
`schema.mode` accepts `reset` and `migrate`.
If omitted, the mode is `reset` only when `registry.environments.disposable` is true. Otherwise, it is `migrate`.
An environment name or explicit `reset` mode grants no authority to discard data.
Editing the mode alone executes no SQL and changes no application selection.

```bash
wamn schema publish apps/sample_app --env environments/dev/environment.k
wamn schema diff sample_app --env environments/dev/environment.k
wamn schema apply sample_app --env environments/dev/environment.k
wamn schema apply sample_app --env environments/dev/environment.k --release sha256:<release-digest>
wamn schema diff apps/sample_app --env environments/dev/environment.k
wamn schema apply apps/sample_app --env environments/dev/environment.k
wamn schema reset --env environments/dev/environment.k
```

An application name selects its latest successful Helm release. `--release` selects another application release for SQL without selecting its code.
A source diff requires current build output and performs no publication or mutation.
A deployed source apply builds and publishes first, then executes those immutable bytes through the same SQL Job.
`schema publish` prints the application release digest without installing SQL or selecting code.
Development uses the same control routes for schema diff and apply. See [development loop](development-loop.md).

In `migrate`, explicit apply executes pending files in dependency order and preserves existing rows.
The watcher initializes absent packages but does not advance installed SQL.
Code deployment preserves installed SQL.
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

Every schema apply in `reset` mode resets application state in the existing project database.
An explicit schema reset does the same work.
The registry must authorize data loss by recording the environment as disposable.
Neither operation resets `wamn_system` or another environment.

Before stopping users, WAMN resolves all retained package artifacts and validates their migration files.
The lifecycle lock excludes application deployment during reset.
Reset stops application hosts and event users.
It drops the objects owned by the migration login and restores that login's grants.
It clears application catalog state, runs, queued events, and authored workflows.
It then applies every selected package's migrations in dependency order.

Platform-owned schemas, database credentials, CDC publications, and replication slots remain.
The platform owns application schemas. The migration login owns the objects inside them.
Schema publications therefore include recreated tables without rebuilding CDC.
The database keeps its identity.
After migrations succeed, WAMN resumes the existing application selections.
If a reset fails, users remain stopped. Repair the failure and repeat the same schema apply with its release.

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
Without it, the host uses the live release's bound version, or the newest published version when nothing is bound.
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
[Integration and connection schema](../plan/integration-schema.md) defines administration, bindings, and version selection.

The control routes use project administrator authority.
The `external_system.*`, `integration.*`, and `connection.*` routes manage their respective identities.
The matching `wamn-ctl` verbs call these routes with a session token.
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
`environment::apply` provisions shared environment resources.
Apply application SQL before `wamn deploy` selects the targeted code release.
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

The operation worker holds the targeted application lock and a shared environment lock.
Different applications can deploy concurrently. A whole-environment reset takes the exclusive environment lock.
A CI completion order does not set deployment precedence.

The deployed capture owner sets replican identity from the installed published package artifacts.
The development loop reconciles it directly from local source on each activation.
Captured tables use REPLICA IDENTITY FULL so that events include the old row.

Provisioning owns database schema, privileges, environment bindings, and broker stream configuration.
Runtime uses credentials bound to that environment.
Declare stream replicas and the duplicate window in the environment configuration.
Activation compares declared configuration with the provisioned resources and refuses disagreement.
It does not grant runtime credentials permission to reconfigure streams.

A physical schema change follows the selected mode and recorded disposable authorization described above.

Read each command's current required arguments with `--help`.

## Publish a release

Publish a source package through the product command:

```bash
wamn schema publish apps/wamn_receiving --env environments/dev/environment.k
```

Publication verifies package and component artifacts and prints the immutable release digest.
The environment document selects the application package closure.

Source publication records the release by its canonical manifest digest.
If the release has an HTTP attachment, publication composes the pinned route guest with the application.
Set `WAMN_COMPONENT_ARTIFACT_BASE` for this step. `WAMN_FLOW_HTTP_COMPONENT` can name another route guest file.
It pushes the entry to `<component-artifact-base>/entries`, beside the members.
The tag of the entry is its content digest. If that tag is already present with the same bytes, publication reuses it and pushes nothing.
The manifest records the entry as `entries@sha256:<manifest-digest>`, a path relative to the component artifact base.
It also records the member byte digests.
Each host joins the entry path with its own `WAMN_COMPONENT_ARTIFACT_BASE`, as it does for the members.
The manifest names no registry, so a release copied with its components and entry starts in the new registry.
The release chart starts that entry with `poolSize: 0`.
The host loads application releases from verified catalog bytes when workloads or unfinished runs need them.
It also restores the releases pinned by unfinished runs after restart.
A release preparation failure appears in the host log with its digest and pinned run ids.
It does not stop queues belonging to other releases.
The host takes no process release digest or release registry argument.
It pushes the manifest to the release repository that `WAMN_RELEASE_ARTIFACT_BASE` names, and it prints the digest.
The credential of `--registry-auth-file` must pull the packages and push to the release repository.
Each component of the release carries its admitted descriptor from the package artifact.
Set `RELEASE_DIGEST` to it.
The manifest names no environment key and no environment, so one release has one digest in every environment.
The composed `project_env_key` is `<org>--<environment>--<project>`.
The separate `environment` field keeps the short name.
The release also names no route host.
The route host is only the `host` of the http workload config. The router matches routes under it.
`publish-release` takes the same `--oci-ca-path` input as `push-package`.
Use `wamn deploy --release` to select the published application.

## Web client files

Use [Web upload](web-upload.md#package-clients) to copy the pinned web artifact to a bucket.
Pass the environment document’s `client.web` digest and the registry inputs.
The uploader reads the package id and client name from `web.json`.
It writes `clients/<package>/<web digest hex>/` and skips all writes when the index already exists.
That page owns the command, required inputs, destination paths, output, and errors.
The shell reads `/config.json` from the installed web service before sign-in.
The bucket must allow reads without credentials.
Edge deployment remains outside the local platform milestone.

Release hosts expose workload HTTP on port 80, probes on port 8081, and native file transfers on port 8082.
The `<release>-files` Service selects the shared environment host and maps port 80 to the named `files` port.
The edge sends `/api/<application>/files` and its child paths there before the general `/api/` route.
File requests keep the full path. Other API requests lose the `/api` prefix.

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

Use [Web upload](web-upload.md#platform-ui) with the artifact digest that the prepared chart pins in `platformUi`.
The command pulls the artifact from the platform registry and makes sure that its bytes match the digest before any bucket write.
The default destination is `gs://wamn-dev-web/platform-ui/sha256-<hex>/`.
Organization configuration lives separately at `platform-ui/orgs/<org>/config.json`.
The chart writes the UI digest and organization into `release.json`.
The edge selects shared files by that digest and reads configuration from the organization path.
Deployment needs no source checkout, pnpm installation, or local UI output.
Kind keeps an empty web bucket and selects no platform UI.

The control root is `https://control.wamn.dev`.
The org chart runs `wamn-control --config /etc/wamn/control/config.json` in the org namespace.
The native service has no route guest or application release.
The control Service selects that Deployment.
Local platform deployment installs control without a UI upload. GCP integration for public control access remains unfinished.

The edge values can contain a separate `control` entry with `host` and `api`.
The control host serves no native file routes, so its entry has no `files`.
The edge chart forwards `/api/` to the control Service and `/password/` to identity.
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

`wamn-identity` loads `.env` from its working directory before it starts the runtime.
Existing environment variables take precedence over `.env`. Explicit command flags take precedence over both.
A missing `.env` is allowed. An unreadable or malformed file stops startup without printing its contents.
The root `.env.example` documents `RESEND_API_KEY` and `RESEND_FROM`.
The private `.env` is ignored by Git and needs owner-only read and write permissions (`0600`).
No home-directory path is required.

For local email configuration, copy `.env.example` to `.env` before adding the key and verified sender.
Keep the existing issuer, database, TLS, operator CA, and session target configuration.
External delivery requires both Resend values. Kind deployment instead selects local mail as described in [local access](#local-access-and-owner-invitation).
The CLI also accepts `--resend-api-key` and `--resend-from`. Prefer environment input for the secret.

For Kubernetes, create a Secret with the `api-key` entry in the identity namespace.
Set the identity chart's `resendSecret` to that Secret and `resendFrom` to the verified sender.
Set `operatorCaSecret` to permit operator invitation requests.
Set `inviteUrl` to the web client base, for example `https://receiving.wamn.dev`.
The invitation mail links to `<inviteUrl>/invite#<code>`.
The reset mail links to `<inviteUrl>/reset#<secret>`.
If `inviteUrl` is empty, each mail shows the code.
An operator provisions the first admin of an org with `wamn-ctl invite`, which uses the operator certificate flags of the PAT mint.
It is the one SQL membership path, and it runs as the Provisioning platform principal.
If the org already has an active org admin, the command refuses and names `user.invite`, the route for every later invitation.
The command runs three steps:

1. Identity creates the user of the email, or reuses the user that has it. Identity refuses a disabled user.
2. One system database transaction, as `wamn_system` stamped `wamn:provisioning`, writes the active org membership and the requested grants.
3. Identity mails the invitation only when the user has no password. The command prints only identity's reply.

`--org-admin` and each `--project-admin` write the rows that `org_admin.grant` and `project_admin.grant` write.
Each `--membership <project>/<env>` writes one environment membership.
`--org-admin` is required. `--project-admin` and `--membership` are optional and can repeat:

```bash
wamn-ctl invite --email <email> --display-name <name> --org <org> \
  --org-admin [--project-admin <project>]... [--membership <project>/<env>]... \
  --system-database-url "$WAMN_SYSTEM_ADMIN_URL" --pat-issuer <identity URL> \
  --pat-server-ca <identity CA> --pat-client-cert <operator certificate> --pat-client-key <operator key>
```

Restart the Deployment after changing the credential, sender, or operator CA.
CloudNativePG manages the issuer login and its basic-auth Secret. `provision-identity-issuer` publishes its public connection configuration.
Use the current system schema on a fresh database, following the repository's schema installation contract.

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
The target needs an existing application user or service row.
The HTTP owner refuses unknown users. A grant also refuses a missing role.
The operator service of an environment, `wamn-operator-<org>--<env>--<project>`, has the email `<subject>@<platform domain>`.
`environment::apply` writes its PAT Secret when it can reach identity. GCP deployment integration for a missing operator PAT remains unfinished.

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
SELECT set_config('app.user_id', :'administrator_id', true);
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

After changing the identity service connection ConfigMap, restart the identity service.
Its database target connections are selected at process startup.
Use the environment's identity Deployment and require its readiness before issuing new sessions.

## Platform schema upgrades

Set `WAMN_CONTROL_URL` and `WAMN_TOKEN` for platform SQL operations.
Control selects its installed chart and pinned worker image.
System target reads its managed migration login's basic-auth Secret in the org namespace.
Both targets execute SQL in a bounded Job in that namespace.

```bash
wamn platform schema diff --target system
wamn platform schema apply --target system
```

Both commands run the installed immutable `control` image.
Both read the migration bytes embedded in that image.
Diff writes no SQL state.
It reports installed and pending SQLx versions, checksums, and missing files.
If the image lacks an applied file, apply refuses.
Each transactional file commits with its SQLx history row.
A later failure keeps earlier commits.
Files that begin with `-- no-transaction` need safe retries after partial execution.

Fresh platform installation initializes an empty system database, then runs every embedded system migration.
If migration execution stops, resume with `platform schema apply --target system` with the same installed worker image.
Repeated installation preserves initialization and runs no pending migration.
Ordinary platform reapply also runs no migration.

System schema apply changes only system SQL objects and migration records.
It changes no service release, application selection, credential, or Helm release.
Deploy compatible control code separately with `platform deploy ... --only control`.
For one environment, select project target and its environment document.
Project target requires `--env`, and system target refuses it.
Control reads the environment's recorded database instance and installed connection projection.
The SQL Job uses the scope's own CNPG migration login and basic-auth Secret.
The platform owner roles stay `NOLOGIN`. Migration logins inherit their privileges through membership.
Platform migration execution refuses administrator sessions.

```bash
wamn platform schema diff --target project --env environments/dev/environment.k
wamn platform schema apply --target project --env environments/dev/environment.k
```

The Job holds the environment's exclusive lifecycle lock for the whole operation.
It waits for an active reset or application schema operation.
It changes only WAMN substrate SQL and `platform._sqlx_migrations` in that environment's recorded database.
Application tables, rows, code selections, Helm releases, and other environments stay unchanged.
The operation runs no environment provisioning or application activation.

Fresh environment provisioning installs the catalog and project substrate, then runs every embedded project migration.
Repeated provisioning runs no pending migration.
If execution stops, the error reports completed and remaining files.
Resume with `wamn platform schema apply --target project --env FILE` with the same installed worker image.
Application package migrations keep their own streams.

## Rollback and maintenance

Rollback restores the latest successful revision after a failed head. Otherwise, it restores the preceding distinct release and client pin.
Use `--release <digest>` to select an explicit published release.
Both forms use the same selection and application deployment owner.

A release digest selects exact bytes, but a mutable image tag does not.
The installed package and its committed migrations remain in place.
Reverse migration is not rollback.

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
Run `wamn-ctl provision-system --platform-domain DOMAIN` in the org namespace.
For explicit PostgreSQL access, also pass the system migration login's connection through `--migration-database-url`.
It declares the restricted apply login through CloudNativePG and publishes its connection data in the `wamn-apply` ConfigMap.
The installer supplies that public apply connection object in the native control configuration.
The other apply phases use the restricted credential. The SQLx constructor reads its native basic-auth Secret and cluster CA.

## PostgreSQL client TLS

Runtime pools, OAuth transactions, and resource leases use SQLx with verified TLS.
Component configuration names the database host, port, database, login, basic-auth Secret, and CNPG CA Secret.
Both Secrets and the consumer belong to the org namespace.
Explicit operator connection strings must name a readable CA file through `sslrootcert`.
The database hostname must match the server certificate.
The provisioned CNPG clusters accept client connections only over TLS.

## Kubernetes credentials

Kubernetes Secrets hold connection credentials. Each environment has one Secret with one entry per credential UUID.
The database stores the Secret name and entry name. It accepts only `k8s` references.
File-backed credential storage and `WAMN_CREDENTIAL_DIR` are removed.
Deployed hosts read connection credentials through their in-cluster ServiceAccount.
Local development hosts have no connection credential store.
A credential read returns `no credential store in local mode` and logs that reason.
Local startup still provisions the disposable databases and starts identity, control and web services.
Those processes use explicit operator connection URLs with verified TLS.
They do not supply a local connection credential store.
Session target files accept an explicit role URL for these local processes.
Cluster session targets use native credential and CA references.
Each `sessionTargetSecrets` chart entry contains its ConfigMap `name` and its public `connection` configuration.
The chart grants identity access to the credential and CA named by that configuration, including dedicated cluster CAs.

The HTTP interface keeps its `credential-unavailable` result.

`wamn-kubernetes` owns Secret reads and writes. Each update preserves `resourceVersion` and retries concurrent changes.
`wamn-broker` owns the NATS authorization Secret. Platform install and environment provisioning call the same writer.
