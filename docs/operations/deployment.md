# Deployment

`wamn platform deploy` installs the platform in a local kind cluster from a prepared chart.
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
The prepared directory must include its `platform/` resources and operator chart archive.
On kind, `wamn platform deploy` reads `WAMN_REGISTRY_AUTH_FILE` to create the `hosts/wamn-registry-pull` Secret.
Set this variable to the platform registry credential file before deployment.
Deploy that directory explicitly:

```bash
wamn platform deploy deploy/kind/cluster.k --chart /tmp/wamn-platform-chart
wamn platform status deploy/kind/cluster.k
```

Deployment creates or reuses `wamn-local` and connects its registry to the kind network.
It applies namespaces, certificates, NATS, the wasmCloud operator, and CloudNativePG PostgreSQL.
It initializes `wamn_system` and the initial organization, then starts identity and control.
The command returns success only when every required component is ready.
An error preserves a nonzero exit and prints the status that remains observable.

Deployment also works outside the source checkout.
Copy the cluster document and the entire prepared chart to the deployment host.
If the checkout is absent, set the existing `WAMN_KCL` variable to the pinned compiler path.
[Building](building.md#package-manifests) describes that compiler.
Supply their paths to the same command, with the required tools available.
The deployment host must reach the selected registry and its pinned artifacts.
A loopback registry refers to that host, so its artifacts must exist there before deployment.

Keep the prepared chart at its deployed path for later status queries.
Status reads the selected chart path from Helm and reports selected artifacts separately from running images.
It reads Helm, Kubernetes, certificate, service, and system database state.
Its categories are `ready`, `incomplete`, `failed`, and `unknown`.
A failed query reports `unknown`. Status never creates a missing cluster.
Unselected UI, external mail, or telemetry does not prevent platform readiness.

After an interruption, repeat the deployment command with the same document and chart:

```bash
wamn platform deploy deploy/kind/cluster.k --chart /tmp/wamn-platform-chart
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
Forward `deployment/hostgroup-c-<org>` in namespace `hosts`.
Use `route_hosts.control` as the HTTP host.
For the committed document, the Deployment is `hostgroup-c-local` and the host is `control.wamn.localhost`.
Kind application hosts use `<application>.wamn.localhost`, covered by the edge certificate for `*.wamn.localhost`.
Export the cluster access file to a private directory:

```bash
umask 077
mkdir -p /tmp/wamn-local-access
kind export kubeconfig --name wamn-local --kubeconfig /tmp/wamn-local-access/kubeconfig
kubectl --kubeconfig /tmp/wamn-local-access/kubeconfig -n hosts \
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
The local platform does not provide a public ingress or hosted UI.

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
Run `wamn platform deploy deploy/kind/cluster.k --chart /tmp/wamn-platform-chart` again.
The new platform starts with zero applications. Redeploy each required application through `wamn deploy`.
This procedure does not upgrade a persistent system database.

Remove only the private access files and deployment work directory that this run created.
The deployment output names its work directory. Keep it while the cluster exists because kind can mount files from it.

## Application and schema operations

Use direct kubeconfig and database access for trusted development and bootstrap.
Authenticated WAMN control routes are the product target for both the CLI and UI.
That target shares the operation owners below. This document does not claim that deployment control routes are complete.
GCP coverage and upgrades of persistent system databases remain outside this rebuild.

Prepare the platform first, as [building](building.md#local-platform-artifacts) describes.
Use the selected kubeconfig and its current context.
Supply the existing application inputs before these commands:

- Set `WAMN_APPLY_DATABASE_URL` for restricted system access during mutations.
- Set `WAMN_CONTROL_DATABASE_URL` for read-only schema inspection and status.
- Set `WAMN_SYSTEM_ADMIN_URL` for provisioning, status, and SQL Jobs.
- Set `WAMN_RELEASE_CHART`, `WAMN_RELEASE_ARTIFACT_BASE`, `WAMN_PACKAGE_ARTIFACT_BASE`, and `WAMN_COMPONENT_ARTIFACT_BASE` to the prepared artifacts.
- Set `WAMN_REGISTRY_AUTH_FILE` and the existing certificate and broker inputs for that platform.
- Set `DATABASE_URL` to a separate disposable PostgreSQL server when building source.

`environment.k` declares each application's package membership, hostname, and `current` and `previous` release selections.
Use a source package directory for source deployment. Use an application release digest for deployment without source:

```bash
wamn deploy apps/sample_app --env environments/dev/environment.k
wamn deploy --release sha256:<release-digest> --env environments/dev/environment.k
wamn rollback sample_app --env environments/dev/environment.k
wamn rollback sample_app --release sha256:<release-digest> --env environments/dev/environment.k
wamn status --env environments/dev/environment.k
```

Publication needs no destination database. Initial deployment provisions the environment and initializes missing SQL through a Kubernetes Job.
Each application owns its Helm release, host group, workloads, Services, and route ConfigMaps.
The route ConfigMaps carry `wamn.route=true`. The edge reads that label.
Code-only deploy, reapply, and rollback preserve installed SQL and other applications' data, permissions, routes, and workloads.
Under the application lock, selection derives `previous` from the last successful Helm apply.
An explicit rollback release resolves an ambiguous prior selection. Rollback never reverses SQL.

Deployment prepares bindings and grants before Helm changes the application.
Readiness precedes a bounded drain wait, which lets old hosts finish their work.
The policy's `drain_bound_seconds` supplies the pod termination grace period. Observation allows another 15 seconds for pod removal.
If old hosts remain or pinned work still needs a binding, WAMN retains the binding and its required grants.
If observation fails, WAMN retains those facts. Retry the same selection after repairing the reported failure.

### Select SQL independently

A schema fingerprint identifies the complete physical SQL, including authored and generated definitions.
It excludes code versions, release digests, ordinary queries, and web files.
`schema.mode` accepts `reset` and `migrate`.
If omitted, the mode is `reset` only when `registry.project_envs.disposable` is true. Otherwise, it is `migrate`.
An environment name or explicit `reset` mode grants no authority to discard data.
Editing the mode alone executes no SQL and changes no application selection.

```bash
wamn schema publish apps/sample_app --env environments/dev/environment.k
wamn schema diff sample_app --env environments/dev/environment.k
wamn schema apply sample_app --env environments/dev/environment.k
wamn schema apply sample_app --env environments/dev/environment.k --release sha256:<release-digest>
wamn schema diff apps/sample_app --env environments/dev/environment.k
wamn schema apply apps/sample_app --env environments/dev/environment.k
wamn schema adopt --env environments/dev/environment.k
wamn schema reset --env environments/dev/environment.k
```

An application name selects its configured `current` release. `--release` selects another application release for SQL without selecting its code.
A source diff requires current build output and performs no publication or mutation.
A deployed source apply builds and publishes first, then executes those immutable bytes through the same SQL Job.
`schema publish` prints the application release digest without installing SQL or selecting code.
For adjacent `dev.json` and a source directory, diff and apply use the local connection described in [development loop](development-loop.md).

In `migrate`, explicit apply executes pending files in dependency order and preserves existing rows.
The watcher and code deployment initialize absent packages but do not advance installed SQL.
An ordinary migration file and its completion record commit together.
`catalog.packages` stores each package's baseline, current fingerprint, version, and artifact source.
`catalog.package_migrations` stores ordered paths, checksums, outcomes, and completion timestamps across code versions.
`platform.schema_baseline` records the project substrate and initialization identity.
Status reports unreadable state as unknown. An adopted record does not prove that SQL ran.

`schema adopt` asserts the configured sources without executing SQL or selecting code.
Matching records remain unchanged. New or changed asserted files receive `adopted` with no `applied_at` timestamp.
Resolve an uncertain `started` record before adoption.

### Reset scope and recovery

If selected physical SQL changes in authorized `reset` mode, deploy or schema apply replaces the whole project-environment database.
Every application in that database loses its data, including stopped applications with retained schemas.
Manual workflow definitions, queued runs, and other database-local state also disappear.
An explicit schema reset performs that replacement even when the fingerprints match.
Neither operation resets `wamn_system` or changes another environment's database.

Before stopping writers, WAMN resolves every retained schema, permission, and required published artifact.
Missing sources, omitted installed applications, or conflicting definitions stop preparation before destruction.
The environment lock excludes application deployment during reset.
`registry.project_envs.pending_database` pins the complete inputs and a new initialization identity outside the database being replaced.
CNPG sets the existing Database to `ensure: absent`, waits, then sets `ensure: present` under the same name and waits again.
The SQL Job restores the pinned schema. Existing owners restore grants, bindings, release caches, and CDC.
The broker discards only that environment's obsolete events while writers remain stopped.
WAMN clears the pending record before resuming unchanged application selections.

After an interruption, repeat the same reset or schema-changing deploy.
Before the recreated database carries the pinned initialization identity, retry can repeat deletion and creation.
After that identity exists, retry preserves the recreated database and resumes SQL restoration and cleanup.
If workload resume fails after the pending record clears, reapply the affected application's release.
Inspect SQL records and the named failed resource before retrying. Do not clear pending inputs or manually drop application tables.
Stale-event admission remains untested under `wamn-alrvy.19`.

### Nontransactional SQL repair

A file whose first line is exactly `-- wamn:no-transaction` must contain one standalone statement.
The checksum includes the marker. WAMN commits `started`, executes outside a transaction, then records `applied` after success.
This sequence provides neither atomicity nor exactly-once execution.
An uncertain `started` record blocks silent replay. Inspect the actual database effect first.
Use the package and relative path in the explicit resolution:

```bash
wamn schema apply sample_app --env environments/dev/environment.k --resolve sample_pkg:migrations/0002_index.sql=applied
wamn schema apply sample_app --env environments/dev/environment.k --resolve sample_pkg:migrations/0002_index.sql=retry
```

`applied` asserts completed execution and skips the statement.
`retry` permits the same checksummed bytes after manual repair, such as removing an invalid concurrent index.
Changed bytes refuse resolution. WAMN performs no automatic repair.
Execution and resolution share session advisory locks across commits, so resolution cannot race a worker that still runs.
Committed SQL stays after a failed rollout or executable rollback.

## Existing application operations

Use an explicitly selected cluster, namespace, and environment for the commands below.
[Repository delivery commands](delivery.md) describe the existing qualification, publication, and release-selection interfaces.
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
wamn-ctl integration write --file /private/integration.json
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
wamn-ctl connection test --file /private/connection.json --credential-file /private/credential.json
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

Write the connection with its first credential:

```bash
wamn-ctl connection write --file /private/connection.json --credential-file /private/credential.json
```

For later credential changes, create `/private/target.json` with the project, `env`, and integration name:

```json
{"project":"wms","env":"dev","name":"orders-api"}
```

Set the replacement credential:

```bash
wamn-ctl connection set-credential --file /private/target.json --credential-file /private/credential.json
```

For `oauth2_authorization_code`, set `parameters.auth` to `client_id`, `authorize_url`, `token_url`, and a `scopes` array.
Write the client secret through `--credential-file` as `{"client_secret":"..."}`.
After the write, read the connection metadata:

```bash
wamn-ctl connection get --file /private/target.json
```

Register its `redirect_uri` with the provider.
The deployed control host uses `https://<route_hosts.control>/oauth/callback`.
The local control host uses its local public URL with `/oauth/callback`.
Start authorization:

```bash
wamn-ctl connection authorize --file /private/target.json
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
For Modbus, supply `--write-interface wamn:modbus/write@1.0.0` to `wamn-ctl integration publish modbus`.
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
`wamn deploy` provisions missing shared resources, initializes absent application SQL, and then applies the targeted application release.
Platform deployment performs no application pass.

The optional `client` field pins an application web artifact: `client = {package = "wamn_receiving", web = "<full web digest>"}`.
The web digest is `sha256:` and 64 lowercase hexadecimal digits, as `wamn web push` prints it.
Without `client`, the host serves no application web client, even when the release contains one client package.
Apply pulls the artifact from `WAMN_PACKAGE_ARTIFACT_BASE` with the existing registry credentials and CA paths.
It refuses a web artifact whose package id differs from `client.package` or whose package version is absent from the release.
The revision values keep the host-to-package map in `webClients` and the host-to-web-digest map in `webDigests`.

The same immutable web artifact can serve more than one environment.
The verb reads its platform inputs from environment variables (see the selected command's `--help` for its required inputs):

```bash
wamn-ctl env apply <document> --application <application> --dry-run
wamn-ctl env apply <document> --application <application>
```

`--dry-run` prints the plan and changes nothing.
`wamn-ctl env apply` holds the targeted application's lock and a shared environment lock.
Different applications can deploy concurrently. A whole-environment reset takes the exclusive environment lock.
A CI completion order does not set deployment precedence.

The deployed capture owner sets replica identity from the installed published package artifacts.
The development loop reconciles it directly from local source on each activation.
Captured tables use REPLICA IDENTITY FULL so that events include the old row.

Provisioning owns database schema, privileges, environment bindings, and broker stream configuration.
Runtime uses credentials scoped to that environment.
Declare stream replicas and the duplicate window in the environment configuration.
Activation compares declared configuration with the provisioned resources and refuses disagreement.
It does not grant runtime credentials permission to reconfigure streams.

A physical schema change follows the selected mode and recorded disposable authorization described above.

Read each command's current required arguments with `--help`.

## Publish a release

The example below selects Receiving.
`publish-release` composes package artifacts.
Push the package with `wamn-ctl push-package` first.
It admits each component, pushes it under its digest, and then pushes the package artifact with the component descriptors.
Before it pushes anything, it reads the tag `<package_id>-<version>`.
If the tag holds another digest, it refuses, names both digests, and pushes nothing. Tags are immutable.
If the tag holds the same digest, it reports `already present` and pushes nothing again.
No database records the package: the artifact in the registry is the proof of its checks.
Each `--package ID@VERSION` is the artifact under the tag `ID-VERSION` in `--artifact-base`, read from the registry only.
A package with no artifact in the registry refuses, and the publish writes nothing.
The release takes each package's `wamn.json`, attachments and component descriptors from its artifact.
For an overlay, repeat `--package` for the exact selected package set.
`publish-release` reads the file argument, compiles it, and takes its `applications` map.
The document supplies the organization, project, and environment.
A map with one application needs no `--application` selector.
For a map with multiple applications, pass the application name through `--application`.
Publication derives the selected application closure from the declared package membership and verified package artifacts.

```bash
WAMN_RELEASE_ARTIFACT_BASE="$RELEASE_BASE" wamn-ctl publish-release \
  environments/dev/environment.k \
  --package "$PACKAGE_ID@$PACKAGE_VERSION" \
  --artifact-base "$PACKAGE_BASE" --registry-auth-file "$PUSH_DOCKERCONFIG"

wamn-ctl qualify-release \
  --release-digest "$RELEASE_DIGEST" --release-chart "$WAMN_RELEASE_CHART" \
  --artifact-base "$RELEASE_BASE" --registry-auth-file "$PUSH_DOCKERCONFIG"
```

`publish-release` records the release by its canonical manifest digest.
If the release has an HTTP attachment, publication composes the pinned route guest with the application.
Set `WAMN_COMPONENT_ARTIFACT_BASE` for this step. `WAMN_FLOW_HTTP_COMPONENT` can name another route guest file.
It pushes the entry to `<component-artifact-base>/entries`, beside the members.
The tag of the entry is its content digest. If that tag is already present with the same bytes, publication reuses it and pushes nothing.
The manifest records the entry as `entries@sha256:<manifest-digest>`, a path relative to the component artifact base.
It also records the member byte digests.
Each host joins the entry path with its own `WAMN_COMPONENT_ARTIFACT_BASE`, as it does for the members.
The manifest names no registry, so a release copied with its components and entry starts in the new registry.
The release chart starts that entry with `poolSize: 0`.
It pushes the manifest to the release repository that `WAMN_RELEASE_ARTIFACT_BASE` names, and it prints the digest.
The credential of `--registry-auth-file` must pull the packages and push to the release repository.
Each component of the release carries its admitted descriptor from the package artifact.
Set `RELEASE_DIGEST` to it.
The manifest names no environment key and no environment, so one release has one digest in every environment.
The composed `env_key` is `<org>--<project>--<environment>`.
The separate `environment` field keeps the short name.
The release also names no route host.
The route host is only the `host` of the http workload config. The router matches routes under it.
`publish-release` takes the same `--oci-ca-path` input as `push-package`.
[Release qualification](delivery.md#release-qualification) remains a separate optional operation.
Each package selects its cases through its own `tests/qualification.json` declaration.
`qualify-release` pushes the qualification artifact after its cases pass. Deployment reports missing qualification as a warning.
Use `wamn deploy --release` to select the published application.

## Web client files

Use [Web upload](web-upload.md#package-clients) to copy the pinned web artifact to a bucket.
Pass the environment document’s `client.web` digest, the org, and the registry inputs.
The uploader reads the package id and client name from `web.json`.
It writes `clients/<package>/<web digest hex>/` and skips all writes when the index already exists.
That page owns the command, required inputs, destination paths, output, and errors.
The shell reads `/config.json` before sign-in. The local dev server answers it from `dev.json`.
The bucket must allow reads without credentials.
Edge deployment remains outside the local platform milestone.

Release hosts expose workload HTTP on port 80, probes on port 8081, and native file transfers on port 8082.
The `<release>-files` Service selects the host group pods and maps port 80 to the named `files` port.
The edge sends `/api/<application>/files` and its child paths there before the general `/api/` route.
File requests keep the full path. Other API requests lose the `/api` prefix.

The Rust edge reads the live release and serves its bucket files.
For an application, it selects `clients/<package>/<web digest hex>/` from the host's pinned package and web digest.
The digest uses `sha256:` and 64 lowercase hex digits. A missing or invalid web digest refuses.

The edge adds `release` and `web` to the uploaded `config.json`, which supplies `org` and `project`.
Both fields contain full `sha256:` digests. The edge serves this file with `no-cache`.
A web-only apply changes the bundle and `web`, while `release` stays the same.

In kind, the Receiving edge case uses the command. [Cluster tests](cluster-tests.md) describes that case.

- An application never learns a deployment fact at build time. The same static files serve every environment.

Ruled: keep `max-age=10, stale-while-revalidate=60`, because no shared cache stores an authenticated list under option 3.3 A. Revisit them with a measurement from the real deployment.

Ruled: the files under `assets/` carry a content hash in their names, so they are `public, max-age=31536000, immutable`. `index.html` is `no-cache`, so a new release reaches the browser at the next load.

## Platform UI files

Use [Web upload](web-upload.md#platform-ui) to upload the local platform UI with its matching stamped chart.
The default destination is `gs://wamn-dev-web/platform-ui/<platform version>/`.
The upload page owns the command, build comparison, organization configuration, and refusal of existing objects.

The control root is `https://control.wamn.dev`.
Its host group runs `wamn-host host --control --org=dkk` in namespace `hosts`.
Its platform HTTP role contains the pinned route guest and an empty dispatcher, with `poolSize: 0` and no release record.
The HTTP transport workload selects that host group and supplies endpoints for the fixed Service `c-dkk-http`.
Local platform deployment installs control without a UI upload. GCP integration for public control access remains unfinished.

The edge values can contain a separate `control` entry with `host` and `api`.
The control host serves no native file routes, so its entry has no `files`.
The edge chart forwards `/api/` to the control Service and `/password/` to identity.
The Rust edge serves the platform UI files behind nginx.
Every other page reaches that prefix's `index.html`, including `/`, `/invite`, `/recover`, `/reset`, and control or project-environment audience paths.

## Google Cloud edge

On Google Cloud, a global HTTPS load balancer with Cloud CDN sends all traffic to a network endpoint group, and the group sends it to nginx.
The load balancer presents a Google-managed certificate and re-encrypts to nginx, which keeps its own TLS as in kind.
The CDN caches only what the origin marks as cacheable.
Set `neg: true` in the edge values. The edge Service then carries the annotation that makes GKE create the network endpoint group.
The chart renders no Google Cloud resource and no other difference from kind. [Google Cloud prerequisites](gcp.md#cloud-credentials-and-prerequisites) describe the external inputs. Deployment integration remains unfinished.
The edge keeps `/api` for native file requests and removes it for other API requests.
It checks the identity certificate, as it does in kind.

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
The issuer credential needs the current grants from `provision-identity-issuer --prepare-generation`.
Use the current system schema on a fresh database, following the repository's schema installation contract.

The route bodies and limits are in [password enrollment](../architecture/execution.md#password-enrollment-foundation).

## User roles

`apply-package` writes the built-in role `admin` in each tenant.
`admin` has no permission rows and holds every operation that the serving release serves.
Every other role is authored in the tenant, and it holds the stable operation references `<package>:<interface>/<operation>` granted to it.
A user or a service reaches no route and no environment at sign-in until it holds a role.

Give a role with `wamn-ctl grant-role`, and take it away with `wamn-ctl revoke-role`.
Both verbs use the application host URL and the caller's session token.
The target user is named by principal UUID:

```bash
wamn-ctl grant-role --host-url "$WAMN_CONTROL_URL" --token "$WAMN_TOKEN" \
  --user-id <principal UUID> --role <authored role>
```

The verbs refuse direct assignment or removal of `admin`.
Grant or revoke system project-admin or org-admin to change application administration.
The target needs an existing application user or service row.
The HTTP owner refuses unknown users. A grant also refuses a missing role.
The operator service of an environment, `wamn-operator-<org>--<project>--<env>`, has the email `<subject>@<platform domain>`.
`env apply` writes its PAT Secret when it can reach identity. GCP deployment integration for a missing operator PAT remains unfinished.

### Authored roles

Create an authored role with `wamn-ctl create-role`, and delete it with `wamn-ctl delete-role`.
The delete also removes the role's assignments and permissions.
The verbs take the same host and session arguments as `grant-role`.
The `--role` argument names the role.
A role name has lowercase letters, digits, and hyphens after the first character, and at most 64 bytes.
The verbs refuse `admin`.

Select an operation for an authored role with `wamn-ctl grant-permission`:

```bash
wamn-ctl grant-permission --host-url "$WAMN_CONTROL_URL" --token "$WAMN_TOKEN" \
  --role <role> \
  --operation <package>:<interface>/<operation>
```

The grant stores the operation even when the serving release does not serve it.
`wamn-ctl revoke-permission` removes the operation, and it refuses an operation that the role does not hold.


## Mailbox-loss recovery

An authorized administrator approves the replacement email and updates the existing user principal.
There is no separate trusted-contact channel or self-service email change.
The user then uses normal password recovery at the replacement address.

Use an authorized administrative connection to the identity service's `wamn_system` database.
The connection must permit `SET ROLE wamn_system`.
Do not use the scoped identity issuer credential, which cannot edit principals.
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
    WHERE id = :'principal_id'::uuid AND kind = 'user'
    FOR UPDATE
) AS account_found \gset
\if :account_found
UPDATE identity.principals
SET email = lower(btrim(:'replacement_email'))
WHERE id = :'principal_id'::uuid AND kind = 'user';
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
Run `wamn-ctl env apply` for affected environments to update their copied user rows.

## Identity target credentials

After replacing the identity service target Secret, restart the identity service.
Its database target connections are selected at process startup.
Use the environment's identity Deployment and require its readiness before issuing new sessions.

## Platform schema upgrades

Select the prepared chart explicitly for each platform SQL operation.
Set `KUBECONFIG` to the target cluster file and select its current context first.
Without `KUBECONFIG`, the command uses `$HOME/.kube/config`.
System scope reads the existing `platform/wamn-pg-superuser` Secret.
Both scopes execute SQL in a bounded Job in `platform`.

```bash
wamn platform schema diff --scope system --chart /prepared/platform
wamn platform schema apply --scope system --chart /prepared/platform
```

Both commands run the selected chart's immutable `ctl` image and print its digest.
Both read the migration bytes embedded in that image.
Diff writes no SQL state.
It reports adoption, completed files, pending SQL, checksums, and uncertain `started` files.
For recognized removals, it warns about columns and known SQL dependencies.
It also names control and identity code as possible consumers.

If the image lacks recorded files, diff warns and apply refuses.
Adoption creates an empty migration history for an existing matching initialization.
Only apply performs adoption and runs pending files.

Each transactional file commits with its history row.
Apply skips completed files and refuses changed checksums or a different initialization digest.
A failure reports completed and remaining files.
Committed SQL stays committed if a later file fails.

A file with the `-- wamn:no-transaction` marker runs outside a transaction.
If its outcome stays `started`, inspect the database before resolution.
Use `applied` to record that the statement completed.
After any manual cleanup, use `retry` to run the same bytes again.

```bash
wamn platform schema apply --scope system --chart /prepared/platform \
  --resolve migrations/system/0001_example.sql=applied
wamn platform schema apply --scope system --chart /prepared/platform \
  --resolve migrations/system/0001_example.sql=retry
```

Fresh platform installation initializes an empty system database, then runs every embedded system migration.
If migration execution stops, resume with `platform schema apply --scope system` and the same `--chart`.
Repeated installation preserves initialization and runs no pending migration.
Ordinary platform reapply also runs no migration.

System schema apply changes only system SQL objects and migration records.
It changes no service release, application selection, credential, or Helm release.
Deploy compatible control code separately with `platform deploy ... --only control`.
For one environment, select project scope and its environment document.
Project scope requires `--env`, and system scope refuses it.
Set `WAMN_APPLY_DATABASE_URL` to the installed system registry before project operations.
The command reads the environment's recorded database instance and PostgreSQL cluster from that registry.
The SQL Job uses the existing CNPG administrator and `platform/wamn-apply` credentials.

```bash
wamn platform schema diff --scope project --env environments/dev/environment.k --chart /prepared/platform
wamn platform schema apply --scope project --env environments/dev/environment.k --chart /prepared/platform
wamn platform schema apply --scope project --env environments/dev/environment.k --chart /prepared/platform \
  --resolve migrations/project/0001_example.sql=retry
```

The Job holds the environment's exclusive lifecycle lock for the whole operation.
It waits for an active reset or application schema operation.
It changes only WAMN substrate SQL and `platform.schema_migration` in that environment's recorded database.
Application tables, rows, code selections, Helm releases, and other environments stay unchanged.
The operation runs no environment provisioning or application activation.

Fresh environment provisioning installs the catalog and project substrate, then runs every embedded project migration.
Repeated provisioning runs no pending migration.
If execution stops, the error reports completed and remaining files.
Resume with `wamn platform schema apply --scope project --env FILE --chart DIR` and the same chart.
Application package migrations keep their own streams.

## Rollback and maintenance

Run `wamn rollback <application> --env <environment.k>` to restore the prior application release.
If the prior selection is ambiguous, select the target with `--release <digest>`.
The low-level form is `wamn-ctl env rollback <environment.k> --application <application> [--release <digest>]`.
Both forms use the same selection and application deployment owner.

A release digest selects exact bytes, but a mutable image tag does not.
The installed package and its committed migrations remain in place.
Reverse migration is not rollback.

`wamn-ctl-ops` contains copy, run-history pruning, event advisory, and cluster recovery commands.
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

For separate application setup, set `WAMN_SYSTEM_ADMIN_URL` to the system administrator URL.
Run `wamn-ctl provision-system --platform-domain DOMAIN --emit-apply-secret PATH --db-host HOST` to write the restricted apply Secret.
Platform deployment does not emit this application credential Secret.
Set `WAMN_APPLY_DATABASE_URL` from the Secret URL before `wamn-ctl env apply`.
Set `WAMN_SYSTEM_ADMIN_URL` for its ensure phase.
The other apply phases use the restricted credential.
