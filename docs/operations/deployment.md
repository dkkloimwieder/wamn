# Deployment

A chart is a Helm package of Kubernetes resources.
Deployment consumes pinned artifacts, whose digests identify exact bytes.
It does not compile source, build JavaScript, publish packages, or select application releases.
A fresh platform contains zero application environments and zero application releases.

[Building](building.md#platform-artifacts) explains how to prepare the chart bundle.
Run `wamn platform deploy <values>` on a developer machine with Helm 4 and an administrator kubeconfig.
The command uses the current Kubernetes context. Helm validates the operator's values against the platform chart schema.
Set `WAMN_RELEASE_CHART` to the prepared bundle directory or its published OCI digest reference. You can also pass it with `--chart`.

The command installs or upgrades infrastructure first, then the platform. At the end it waits for the SQL Job to complete and the workloads to roll out. Identity reports readiness through its `/readyz` probe.
Use `--only infrastructure` for cert-manager, trust-manager, CloudNativePG, and Traefik when enabled. Use `--only platform` for the shared platform workloads on an existing infrastructure install.
Use `wamn env apply` to roll out a host image.

Set `registryEgress` to the registry's CIDR blocks and TCP ports. If the list is empty, the command uses the IP address and port from `registryEndpoint`.
The platform install requires one of these inputs and records the resolved list in `wamn-org-inputs`.

## Upgrade the runtime operator to 2.12.3

On an existing cluster, apply the four updated workload custom resource definitions (CRDs) before you run `wamn platform deploy`.
Helm installs these definitions on a fresh cluster and skips existing definitions in the chart's `crds/` directory. [Helm CRD handling](https://helm.sh/docs/chart_best_practices/custom_resource_definitions/)

Run this command from the repository root with the target Kubernetes context selected.

```bash
tar -xOzf deploy/charts/platform/charts/runtime-operator-2.12.3.tgz \
  runtime-operator/crds/runtime.wasmcloud.dev_workloaddaemonsets.yaml \
  runtime-operator/crds/runtime.wasmcloud.dev_workloaddeployments.yaml \
  runtime-operator/crds/runtime.wasmcloud.dev_workloadreplicasets.yaml \
  runtime-operator/crds/runtime.wasmcloud.dev_workloads.yaml |
  kubectl apply --server-side --force-conflicts -f -
```

## Local platform

Use Docker, kind 0.33.0, Helm, kubectl, OpenSSL, and the pinned KCL compiler on the deployment host.
Install the pinned kind binary with `mise install` from the repository root. [Tool installation](building.md#repository-tasks-and-tools)
Keep registry, database, and broker credentials outside committed files.
Certificate subjects must match the endpoint names that their clients verify.

Set `CARGO_TARGET_DIR` before you build. [Checkout setup](building.md#building)
Build the `wamn` command in debug mode.
Add `$CARGO_TARGET_DIR/debug` to `PATH`.
`cargo xtask install-kcl` installs the pinned KCL compiler under `.tools/kcl/`.

```bash
cargo build --locked --offline -p wamn-cli
export PATH="$CARGO_TARGET_DIR/debug:$PATH"
cargo xtask install-kcl
```

From the source checkout, create the local cluster and OCI registry.

```bash
cargo xtask kind-cluster-create
```

The command reuses the `wamn-local` cluster and the registry at `localhost:5001` if they already exist.
It uses the pinned node image when it creates a cluster. [Kind configuration](../../deploy/kind/kind-config.yaml)
Copy the kind values file to `platform-kind.yaml` and add the printed `registryEndpoint` block. The kind values file sets the registry to `localhost:5001`. [Kind values](../../deploy/charts/platform/values-kind.yaml)
Prepare the artifacts as [building](building.md#platform-artifacts) describes.

Select the `kind-wamn-local` Kubernetes context.
On a fresh cluster, create the `platform` namespace before you create the Secret.
`wamn platform deploy` sets Pod Security enforcement and warnings to `restricted` on that namespace before installing either chart.
Identity uses the `key` entry in `platform/wamn-oauth` to encrypt its signing keys.
Create the Secret with a random 32-byte key if it is missing.

```bash
(
  set -e
  kubectl config use-context kind-wamn-local
  namespace=$(kubectl get namespace platform --ignore-not-found -o name)
  if [ -z "$namespace" ]; then
    kubectl create namespace platform
  fi

  secret=$(kubectl -n platform get secret wamn-oauth --ignore-not-found -o name)
  if [ -z "$secret" ]; then
    key_file=$(mktemp)
    trap 'rm -f "$key_file"' EXIT
    openssl rand -out "$key_file" 32
    kubectl -n platform create secret generic wamn-oauth --from-file="key=$key_file"
  fi
)
```

Install the platform.

```bash
wamn platform deploy platform-kind.yaml --chart /var/tmp/wamn-platform-chart
```

`cargo xtask build-platform` writes the bundle's digest reference to `chart.ref` in the bundle folder.
`wamn platform deploy` records the reference in `wamn-org-inputs`, and `wamn org create` uses it for control's `chart` setting.

The published application bundle includes the infrastructure, platform, org, environment, and project charts.
Helm installs cert-manager, trust-manager, CloudNativePG, and the ingress controller before the platform.
On a fresh cluster, `wamn platform deploy` starts cert-manager before it adds trust-manager to the infrastructure release.
It waits for the trust-manager webhook before it installs the platform chart.
The platform chart installs the system PostgreSQL Cluster, its SQL Job, shared identity, edge, NATS, the wasmCloud runtime operator, and the collector.
The org chart owns control, RBAC, and network policies.
On kind, create a registry auth file with `{"auths":{"localhost:5001":{}}}`.
On kind, set `WAMN_REGISTRY_AUTH_FILE` to the path of that file.
If the platform uses registry token metadata, skip these two steps.
A user can belong to only one org. Use an owner email that does not belong to a user in another org.
An inactive membership in another org also blocks the invitation.
Run `wamn org create <org> --owner <email>` with the admin kubeconfig.

Use the same kubeconfig to run `wamn org deactivate <org>` or `wamn org activate <org>`.
Deactivation refuses new commands and logins for the org. Its workloads keep running.
Activation restores admission under the existing grants.
On kind, the command copies the registry entry into the org's `wamn-registry-pull` Secret.
The command installs the org chart with public platform settings. Its platform Job writes the control password to the org Secret before the invitation Job runs.
The Job records its start and result as `org_create` in the system journal, including failures before the org is registered.
[Org chart](../../deploy/charts/org/README.md)

If the owner email belongs to another org, control refuses the invitation with HTTP 409.
The command exits with `Error: Job <org>/wamn-org-invitation-<suffix> failed`. The invitation Job's log shows the HTTP 409.

The platform chart sets one JetStream file store to 16 GiB on a 20 GiB volume. The chart value `global.natsFileStoreBytes` gives NATS and identity the same byte limit.
The full resolver has a separate volume. [Platform chart](../../deploy/charts/platform/README.md)

On kind, run `wamn env create <org>/<env>` with the same admin kubeconfig and registry auth file before the first `wamn env apply`.
Add `--prod` to mark the environment as prod. An org can have one prod environment or none.
The first commit of the environment fixes its choice in `registry.env.prod`.
The command checks that the names fit and commits the env and quota through a SQL Job in `platform`.
It then creates the namespaces and adds `wamn.dev/prod=true` to the prod workload namespace.
After the command prepares the worker Secret, a second Job creates the login in SQL and publishes its credentials after commit.
For each new environment on kind, the chart sets the default container CPU request to `10m` (10 millicores).
If the chart supplies no value, the database sets the request to `100m`.
The command applies the env account, worker configuration, network policies, quotas, and local registry credentials.
Local OAuth encryption uses a separate `wamn-oauth` key in each env namespace. Control reads that key for authorize and callback. A retry keeps the key and publishes the worker’s new password.
If provisioning fails partway through, retry with the original `--prod` choice.
If the quota changes in the system database, run the command again to apply it.

The system database stores each environment's NATS limits in `registry.env`.
The defaults are 64 connections, 2,000 consumers per stream, 10,000 subscriptions per connection, and 1 MiB per message.
Each project adds room for two streams and 512 MiB of file storage to the environment's account.
Identity refuses storage increases that would exceed the platform limit.

Use the system database's administrative connection to update the `nats_*` columns.
Bind the provisioning actor in the same transaction so that PostgreSQL can record the edit.

```sql
BEGIN;
SET LOCAL ROLE wamn_system;
INSERT INTO platform.tx_identity (principal_id, role, app, operation)
SELECT id, '', '', 'wamn:provisioning'
FROM identity.principal WHERE type = 'platform' AND subject = 'wamn:provisioning';
UPDATE registry.env SET nats_connections = 96 WHERE org = 'acme' AND env = 'dev';
COMMIT;
```

Identity scans active environments every 30 seconds and retries account revisions until NATS acknowledges them.
Identity refuses to publish a decrease that conflicts with live use. The previous revision continues to apply until the new limits fit.
A decrease in the payload limit requires no active connections or stored messages.

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
Use `<org>.<base>`, where `<base>` is the platform chart's `platformDomain`.
The example uses org `local` and host `local.wamn.localhost`.
Export the cluster access file to a private directory:

```bash
umask 077
mkdir -p /tmp/wamn-local-access
kind export kubeconfig --name wamn-local --kubeconfig /tmp/wamn-local-access/kubeconfig
kubectl --kubeconfig /tmp/wamn-local-access/kubeconfig -n local \
  port-forward deployment/c-local 8080:8080
```

Keep that command running while you use the service.
Send control requests to `http://127.0.0.1:8080` with `Host: local.wamn.localhost`.
From another terminal, read the unauthenticated control response:

```bash
curl -i -H 'Host: local.wamn.localhost' http://127.0.0.1:8080/wamn_control/control/mine
```

This request must return HTTP 401.
A permitted control query requires a password-login token for the org audience.
The local TLS ingress listens on host port 443.
Port 80 reaches the same ingress controller.
For the same control call through web on port 80, use `http://local.wamn.localhost/wamn_control/control/mine`.
On port 443, the org certificate covers `local.wamn.localhost` and `*.local.wamn.localhost`.
Use port 80 for the current app hosts, such as `receiving--local.wamn.localhost`.
To use HTTPS, export the public CA from the kind cluster and pass it to curl.
If you use HTTPS with `wamn` or a browser, add the CA to the client's trust store.

```bash
kubectl --context kind-wamn-local -n hosts get secret wamn-edge-ca \
  -o 'jsonpath={.data.tls\.crt}' | base64 --decode > /tmp/wamn-local-access/edge-ca.crt
curl -i --cacert /tmp/wamn-local-access/edge-ca.crt \
  https://local.wamn.localhost/wamn_control/control/mine
```

Kind selects no hosted UI artifact.

Call an application route through web at `/api/<route path>` on `<label>--<org>.<base>`.
Web strips `/api` before it forwards the request for a route declared by the application.
Find the route paths in `wamn.http.routes` in the application's HTTP WorkloadDeployment.
This key is under `localResources.config` for the `ingress` component.
The Receiving [manifest](../../apps/wamn_receiving/wamn.k) declares `/location/list` in `routes.operations`.

After you deploy Receiving, call its location list route without a token. This request returns HTTP 401.

```bash
curl -i http://receiving--local.wamn.localhost/api/location/list
```

Set `WAMN_TOKEN` to an authorized token for the org. Send it as a bearer token to get HTTP 200.

```bash
curl -i -H "Authorization: Bearer $WAMN_TOKEN" http://receiving--local.wamn.localhost/api/location/list
```

`GET /` on an application host serves the pinned web client for that application.
If the host has no pinned web client, `GET /` and `HEAD /` return HTTP 404 with an empty body.

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
Set `registryEgress` to the address ranges and TCP ports your org pods use to reach Artifact Registry.
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

`POST /wamn_control/app/deploy` takes `app` and a published `release` digest.
The worker checks enabled workflows, applies the package SQL, then upgrades the application chart.
`POST /wamn_control/app/rollback` takes `app` and an optional `revision`.
It checks enabled workflows against the target and restores the chart content.

`POST /wamn_control/schema/diff` requires `app` and accepts an optional `release` digest.
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
The system database keeps an append-only journal for environment apply and project deactivation, deploy, rollback, schema apply and reset, and capture-gap repair.
Control records the authenticated caller and command start.
Control records dispatch failures and refusals, and workers record the results of operations they run.
The journal remains after Kubernetes removes the Job and its logs.

The server selects the image, command, namespace, actor, chart, and credential references.
Operation workers stage the environment's apply login and system database CA in the Job's in-memory volume.
They stage project logins and the project CA as needed.
Reset advances replication through the capture login and removes package objects through their migration login.
SQL steps run inside the authorized operation Job with their owning role credentials.
The control image runs workers and reads the installed chart reference.
The installed control configuration names the pinned worker image and its configuration.
Project operations use the project operation ServiceAccount. It can read declared application workloads and stage named database credentials and CAs in the Job's database volume.
Deploy, schema apply, reset, and capture-gap recovery and close use a companion Job under the environment worker ServiceAccount for project and host chart changes.
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
Set `WAMN_REGISTRY` to the org registry origin for publication.
On kind, set `WAMN_REGISTRY` to `localhost:5001`.
Set `WAMN_REGISTRY_AUTH_FILE` to the path of the registry auth file you created for `wamn org create`.
`WAMN_OCI_CA_PATHS` supplies optional registry certificates.
The worker uses the org origin for packages and application components. It uses the platform origin for integrations.
On kind, both origins use the local registry. The worker uses HTTP for its local registry reads.
`wamn publish` uses the existing build output and starts no Job.

Application, schema and administration commands require `WAMN_CONTROL_URL` and `WAMN_TOKEN`.
Control selects the installed database, chart, registry and broker configuration.
The CLI sends database operations to the control service.
See [Native control](../architecture/control.md) for the service and worker configuration.

The registry stores applied project settings, application host labels, client pins, and connection aliases.
The shared environment owns its target, readiness budget, drain bound, project fact cache lifetime, and connection versions.
The top package pins its base packages by OCI digest.
Helm records the release history. A WorkloadDeployment exposes the applied package digest and Helm revision.

Use one `env.yaml` file for each environment. Each application name must be unique in the file.
Use 1 to 40 lowercase ASCII letters, digits, and single hyphens for application and project names.
Put hyphens between letters or digits.
See [Naming](../architecture/naming.md#environment-names) for the reserved project names.
The [environment schema](../../crates/env/env.schema.json) describes the YAML fields.
Set each application's `label` to a value unique within the org, across all its environments and projects.
Use 1 to 21 lowercase letters, digits, and single hyphens between letters or digits.
Deploy uses `<label>--<org>.<base>` as the host name.
The example uses `receiving--local.wamn.localhost`.
The platform's `platformDomain` value supplies the base for both deploy and web.
An application connection alias names a connection from the shared `connections` map.
Set `revocation_seconds` to an integer from 0 to 3600 for the environment.
Deploy takes this applied value from the registry and sets the project fact cache lifetime on each application workload. Zero disables the cache. Each call reads role activity from the database.
The optional `readiness_budget_seconds` field caps the wait for the selected application template after Helm returns. The wait also shares the Helm command's time budget.
It defaults to 600 seconds.
The optional `drain_bound_seconds` field sets how long the host can drain before shutdown.
It defaults to 300 seconds.
Both fields accept positive integers.

Save this kind example as `env.yaml` in the repository root.
The platform registers the target named `cloud` on kind too.

```yaml
org: local
env: dev
target: cloud
revocation_seconds: 60
database:
  instances: 1
  storage: 2Gi
  cpu: 200m
  memory: 256Mi
projects:
  - name: receiving
    apps:
      wamn-receiving:
        label: receiving
```

Helm installs, upgrades, and rollbacks use `--server-side=true`.
Environment and application installs and upgrades pass `--skip-crds`. The installation release provides the runtime resource types.
Org release records go in `wamn-helm-<org>`.
Host, project, and app release records go in `wamn-helm-<org>--<env>`.
Their workloads and credentials go in `<org>--<env>`. This layout requires a fresh installation.
The kstatus watcher retains `list` and `watch` on the non-Secret kinds that environment and application charts render.

`wamn env apply env.yaml` holds one environment lock.
The worker requires an existing environment registration before it reserves projects.
The worker reserves the selected projects, asks identity to publish the environment account, and writes the host and admin credential Secrets.
It upgrades one project chart per project and runs that project's SQL in file order.
If the file no longer lists an application of a project, the worker stops that application. When one of these applications has unfinished runs, the worker refuses before it stops any of them, and names each unfinished run. Otherwise it prints `stopping <app> (release <name>)` and uninstalls each release. The project database and its data stay.
After the last project succeeds, the worker stages the three timing settings in the registry and upgrades the environment chart with `--wait=watcher`, using the stored drain bound.
The worker commits the settings and, on a full apply, removes unused connection pins after Helm succeeds.
A failed apply keeps the previous environment timing settings. If a project fails, the worker stops and names it. Fix the error and run the command again to continue.
New projects later in the file stay reserved without applied configuration.
An omitted project schema mode follows the environment's disposable setting.
Use `--disposable` on the first apply for an environment that can be reset.
The project chart declares one PostgreSQL Cluster and five managed logins per project.
The environment YAML sets the database size for all projects. The installation values pin the PostgreSQL image.
Project upgrades omit the Helm wait flag. Each project Cluster has a CA that signs its five login certificates and its replication certificate.
See [Backup and recovery](backup-and-recovery.md) for backup settings.
The worker reads each required PostgreSQL cluster, Database, and reader Deployment by name.
The project chart creates the PostgreSQL publication for change data capture (CDC).
Environment apply reads the schemas for that publication from the package digests recorded in the project database.
For each project, the worker waits for the database to be applied and allows up to 120 seconds for it to accept a SQL connection.
CloudNativePG creates the database and assigns ownership to `wamn_migration`. The worker installs the project schema before restoring platform migrations. It checks the publication in SQL before it creates the replication slot.
The CDC reader waits up to 120 seconds for its slot before it starts replication.
The worker obtains a short-lived stream administration JWT from identity and creates the stream. It then saves the project's applied configuration.
The reader stays paused while a capture gap is open.
The shared host group uses the runtime operator chart at version 2.12.3.
At startup, the host uses `POD_NAME` as its host ID and generates a scheduler user key in memory.
It sends the public key and its projected service account token to identity over HTTPS using `/etc/identity-ca/ca.crt`.
The host reads its other NATS credentials from `/etc/wamn/nats/` and trusts the broker CA at `/etc/wamn/nats-ca/ca.crt`.

Use `wamn env deactivate-project ORG/ENV/PROJECT` to deactivate a project deployment. The registry keeps the inactive deployment and removes its project env memberships.
By default, deactivation keeps the Cluster, ObjectStore, and project certificate resources for recovery.
The next `wamn env apply` creates a new database generation with a new Cluster.
Run `wamn env deactivate-project ORG/ENV/PROJECT --data` to remove resources kept by an earlier deactivation. The command lists each one it deletes.
Type the same `ORG/ENV/PROJECT` at the terminal prompt, or pass `--confirm ORG/ENV/PROJECT` with `--data`.
See [Recover after deactivation](backup-and-recovery.md#recover-after-deactivation) for cleanup when the Cluster is already gone.
Deactivation stops the reader before it removes the stream, then removes the replication slot and publication.
It removes the project's CDC credential Secret. Identity revokes the project's NATS users and keeps the environment account.
For the last active project, the worker removes the host group and the shared host credentials.
If you reactivate a project with a retained deployment, run `wamn env apply` to issue a new CDC credential and restart the reader.
These lifecycle requests return Jobs. The CLI polls `env.operation_status`.
Use `wamn revoke-pat --project PROJECT --env ENV --prefix PREFIX` from an org admin session.
Other lifecycle operations take the coordinate and read the registry.
Helm history records the applied release. The top package's deploy section pins its dependency closure.
The configuration export contains no code selection.
Run `wamn env apply env.yaml` before deploying the application.
Use the source directory or set `RELEASE_DIGEST` to the full digest of a published release.

```bash
wamn deploy apps/wamn_receiving --app wamn-receiving --env env.yaml
wamn deploy --release "$RELEASE_DIGEST" --app wamn-receiving --env env.yaml
wamn rollback wamn-receiving --env env.yaml
wamn rollback wamn-receiving --revision 3 --env env.yaml
wamn status --env env.yaml
wamn status wamn-receiving --env env.yaml
```

Status reports each project in the file. An application name selects its project.
For each workload, status reports the deployed package digest, content revision, and workload conditions.

Publication needs no destination database.
Each application owns its Helm history, workloads, and Services.
The environment chart owns one host for each org and environment. All projects share it.
Application workloads and file Services select that host. Database credentials and event bindings remain separate for each project.
Status reports application release state.
The application HTTP Service carries its route and browser pins in the `wamn.dev/web-release` annotation.
A source deploy builds and publishes once. A digest deploy starts with the published package.
Both forms submit one operation. The project Job resolves the package closure and environment bindings. The environment Job holds the lifecycle lock and updates the project chart.
Before deployment or explicit rollback, the worker checks that the deployed Helm revision is serving. If it is unready, the worker restores the newest earlier successful Helm revision before it proceeds. The worker uninstalls an interrupted first install.
It refuses if an enabled workflow calls a sibling or export absent from the candidate.
It allows up to 120 seconds for the project database to accept a SQL connection.
It applies pending SQL and creates the application principals before running `helm upgrade --wait=watcher --rollback-on-failure`.
The worker starts the Helm upgrade only if at least 120 seconds remain in the Job. The Helm command and the worker's check for the new template share at most half the remaining time. Helm uses the same timeout for its own rollback.
If Helm returns success, the worker verifies that each workload from the release is serving its selected template. The current replica set must report Ready for its current generation.
If that check fails, the worker restores the previous release and checks that it serves again. If this was the first install, the worker uninstalls it. The operation reports the original failure and any rollback or cleanup failure.
Committed migrations remain installed. Application developers choose schema compatibility and migrate forward if the restored code cannot use the migrated schema. See [Application schema changes](../application-schema-changes.md).
Environment apply refuses unfinished runs in the selected application.
When the env file omits an app, `wamn env apply` stops its release and marks the app inactive.
The app keeps its host label and its data. Add it back to the file and apply again to reactivate it.
Run `wamn deploy` to serve the app again.
Project deactivation checks all apps in the project env unless you pass `--data`.
If the lifecycle lock session fails, the operation stops and returns an error.

### Select SQL independently

SQLx migration versions and checksums identify applied SQL.
Schema diff reports pending files, missing applied files, and checksum mismatches.
`schema.mode` accepts `reset` and `migrate`.
If omitted, the mode is `reset` only when `registry.env.disposable` is true. Otherwise, it is `migrate`.
An environment name or explicit `reset` mode grants no authority to discard data.
Editing the mode alone executes no SQL and changes no application selection.

Publish the package and save its digest for `wamn schema apply`.
To use the reset example, pass `--disposable` the first time you apply the environment.

```bash
wamn build apps/wamn_receiving
PACKAGE_DIGEST=$(wamn publish apps/target/wamn/wamn_receiving)
wamn schema diff wamn-receiving --env env.yaml
wamn schema apply wamn-receiving --env env.yaml --release "$PACKAGE_DIGEST"
wamn schema reset --project receiving --env env.yaml
```

Schema diff uses the live package digest unless you supply `--release`.
Schema apply uses the supplied package digest and applies migrations in dependency order without upgrading the application chart.
An ordinary migration file and its completion record commit together.
`catalog.package` stores the current package version and artifact source.
A separate SQLx history table records each package and environment's applied versions and checksums.
Authored and generated history migrations run through the same SQLx sequence.
The restricted migration connection keeps its actor binding across commits.
The operation worker selects the migration login from its server-owned connection configuration.
Both connections retain their own credentials and TLS parameters.
Kubernetes callers use the managed migration login's client certificate.
A separate trusted connection records package completion and grants.
SQLx refuses edited or missing applied migrations.
A failed transactional file rolls back its effects and its history row together.
Earlier committed files remain applied.

### Reset target and recovery

An explicit schema reset selects the whole project database.
Type the project ID at the terminal prompt, or pass `--confirm PROJECT`. The reset affects every application in that project.
The registry must authorize data loss by recording the environment as disposable.
Reset stays within the selected project database.

Before stopping users, WAMN resolves all retained package artifacts and validates their migration files.
The lifecycle lock excludes application deployment during reset.
Reset stops application hosts and event users.
It drops the objects owned by the migration login and restores that login's grants.
It clears application catalog state and its history, runs, queued events, and authored workflows.
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

Control saves each new credential version in the shared project Secret before it commits the reference in Postgres.
If control stops between these steps, the Secret can contain an unused version. A retry writes a fresh version.
Connection responses and CLI output include credential creation metadata.
The baseline schema owns fresh installations.

The control routes use project administrator authority.
The `external_system.*`, `integration.*`, and `connection.*` routes manage their respective identities.
The matching `wamn` verbs call these routes with a session token.
Each verb reads its JSON request from `--file`.
The request names the project, and connection requests also name the environment in `env`.
The first connection write reads the credential value from `--credential-file`, except for authentication type `none`.
`connection set-credential` reads later values from that flag too.
`wamn connection deactivate --file /private/target.json` refuses new calls, removes the connection's OAuth records, and erases its credential keys from the shared project Secret.
The connection, its bindings, and its credential and generation history stay.
If a Secret update fails, the connection stays inactive. Repeat deactivation to finish removing its credential keys.
`wamn connection activate --file /private/target.json --credential-file /private/new-credential.json` creates a generation with a fresh credential.
Omit `--credential-file` for authentication type `none`.

### Integration write interfaces

The system schema seeds integration types and their write interfaces through `wamn platform deploy`.
The Modbus type declares `wamn:modbus/write@1.0.0` as its write interface.
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
The deploy Job reads each workload image manifest before it applies SQL.
If a manifest is missing, the Job reports the image reference and stops.
After SQL applies, the Job upgrades the application release.
Platform deployment performs no application pass.

The optional `client` field pins an application web artifact: `client = {package = "wamn_receiving", web = "<full web digest>"}`.
The web digest is `sha256:` and 64 lowercase hexadecimal digits, as `wamn web push` prints it.
Without `client`, the host serves no application web client, even when the release contains one client package.
`wamn web upload --web` pulls the artifact by digest from the repository in `--artifact-base` and writes its files to the bucket.
The revision values keep the host-to-package map in `webClients` and the host-to-web-digest map in `webDigests`.

The same immutable web artifact can serve more than one environment.
Deploy a published release through control:

```bash
wamn deploy --env env.yaml --app wamn-receiving --release "$RELEASE_DIGEST"
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

Set `WAMN_REGISTRY` to the org registry origin. Publication adds `/wamn/packages` and `/wamn/components` to this origin.
Set `WAMN_REGISTRY_AUTH_FILE` to a credential file with write access to the org repository.
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
For unsafe requests with a session cookie, web accepts `Sec-Fetch-Site: same-origin` or `Sec-Fetch-Site: none`.
If that header is absent, web requires an HTTPS `Origin` that matches the incoming `Host`.
The bucket must allow reads without credentials.
Edge deployment remains outside the local platform milestone.

Release hosts expose workload HTTP on port 80, probes on port 8081, and native file transfers on port 8082.
The `<release>-files` Service selects the shared environment host and maps port 80 to the named `files` port.
The edge sends `/api/<app>/files` and its child paths there before the general `/api/` route.
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
The chart puts the UI digest and org in the `wamn.dev/web-release` annotation on the control Service.
The edge selects shared files by that digest and generates the org configuration.
Deployment needs no source checkout, pnpm installation, or local UI output.
Kind keeps an empty web bucket and selects no platform UI.

The control root is `https://<org>.<base>`, where `<base>` is the platform chart's `platformDomain`.
The org chart runs `wamn-control --settings /etc/wamn/control/settings.json platform` in the org namespace.
The native service has no route guest or application release.
The control Service selects that Deployment.
Local platform deployment installs control without a UI upload. GCP integration for public control access remains unfinished.

The control Service annotation contains the control host and Service address.
The edge forwards `/api/` to the control Service and `/password/` to the identity password listener.
The Rust web front serves platform UI files and generates its org configuration.
Every other page reaches that prefix's `index.html`, including `/`, `/invite`, `/recover`, `/reset`, and control or project-environment audience paths.

## Google Cloud edge

On Google Cloud, the HTTPS ingress sends traffic through a network endpoint group to the Rust HTTP Service on port 80.
TLS stays at ingress. A cluster ingress uses the serving certificate that cert-manager owns.
The infrastructure chart installs a default TLS ingress controller on non-GCP clusters.
The Rust front owns response headers and proxies password requests to identity over HTTPS.
The CDN caches only what the origin marks as cacheable.
Set `neg: true` in the edge values. The edge Service then carries the annotation that makes GKE create the network endpoint group.
The chart renders Kubernetes resources. Edge egress remains open on GCP so the proxy can reach its web bucket. Deployment integration remains unfinished. [Google Cloud prerequisites](gcp.md#cloud-credentials-and-prerequisites)
The edge keeps `/api` for native file requests and removes it for other API requests.
It makes sure of the identity certificate, as it does in kind.

## Identity password configuration

`wamn-identity serve --settings <path> --password-port <port>` reads one JSON file. The chart renders the issuer, public database connection, envelope provider, and mail mode into that file.
The chart gives the web proxy access to the password port. Configure it with `identity.passwordPort`, which defaults to 8444.
Identity accepts a forwarded client IP on that port. The general listener on 8443 uses the socket IP and returns 404 for password requests.
The pod mounts the database credentials, TLS keys, local wrapping key, and Resend token as files.
When `identity.authNats.tlsSecret` is set, the chart adds `global.nats.schedulerUrl`, the CA path, and the NATS limits to identity settings.
Identity mounts `ca.crt` from that Secret.
Identity creates the first session signing key at startup. CloudNativePG manages the issuer login. Identity uses the client certificate and key from `wamn-identity-db-tls`.

Create a Secret in `platform` with an `api-key` entry.
Set `identity.resendSecret` to that Secret and `identity.resendFrom` to the verified sender in the platform chart.
Set `identity.inviteUrl` to the web client base, for example `https://receiving--acme.wamn.dev`.
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
Every other role is authored in the project and grants its stable operation references `<package>:<interface>/<operation>` while active.

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
The HTTP owner refuses unknown and disabled users. A grant also refuses a missing or inactive role.
Service principal email addresses use `<subject>@<platform domain>`.
Create an API service and its PAT through control with `wamn create-api-service`.

### Authored roles

Create an authored role with `wamn create-role`.
Use `wamn deactivate-role` to suspend its grants and `wamn activate-role` to restore them.
Deactivation takes effect on the next call and keeps the role's assignments and permissions.
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
    SELECT 1 FROM identity.principal
    WHERE id = :'principal_id'::uuid AND type = 'user'
    FOR UPDATE
) AS account_found \gset
\if :account_found
UPDATE identity.principal
SET email = lower(btrim(:'replacement_email'))
WHERE id = :'principal_id'::uuid AND type = 'user';
UPDATE identity.password_token
SET consumed_at = clock_timestamp()
WHERE principal_id = :'principal_id'::uuid AND consumed_at IS NULL;
UPDATE identity.password_login
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
Control selects its installed chart and pinned worker image. It reads the environment mapping, then starts a Job in that namespace.

```bash
wamn platform schema diff --target project --env env.yaml
wamn platform schema apply --target project --env env.yaml
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
The SQL Job runs as `wamn-env` and uses each project's migration login.
The platform owner roles stay `NOLOGIN`. Migration logins inherit their privileges through membership.
Platform migration execution refuses administrator sessions.

The Job holds one exclusive environment lock for the whole operation.
It visits each project in file order and stops on the first error.
It changes WAMN substrate SQL and `platform._sqlx_migrations` in each selected project database.
Application tables, rows, code selections, Helm releases, and other environments stay unchanged.
The operation runs no environment provisioning or application activation.

Fresh environment provisioning installs the catalog and project substrate, then runs every embedded project migration.
Repeated provisioning runs no pending migration.
If execution stops, the error reports completed and remaining files.
Resume with `wamn platform schema apply --target project --env env.yaml` with the same installed worker image.
Application package migrations keep their own streams.

## Rollback and maintenance

Use `wamn rollback APP --revision N --env env.yaml` to restore the content of revision N.
Without `--revision`, the worker finds the newest earlier deploy. It skips revisions that record a rollback.
If no older deploy exists, the command reports the current content revision and refuses.
The worker refuses revisions whose application Service lacks the route annotation.
It reads the target package digest from Helm, checks enabled workflows, and runs `helm rollback --wait=watcher` under the lifecycle lock.
Before reporting success, the worker verifies that the restored templates are serving. The Helm command and the readiness check share the rollback phase budget within the Job deadline.
Rollback keeps installed SQL migrations.

The WorkloadDeployment annotation `wamn.release-revision` names the revision whose content is live.
Helm restores the saved manifest when it creates a rollback revision. After a rollback to N, status still reports N.
Use the same revision numbers with `--revision`.

Retention runs through its owning library interfaces.
For backup and recovery, use CloudNativePG backup and recovery on the cluster.
The procedure is in [backup and recovery](backup-and-recovery.md).
Use its `--help` output for the selected action and required credentials.
These maintenance verbs do not authorize shared development targets or a new schema lifecycle.

## Environment apply credentials

Set `WAMN_SYSTEM_ADMIN_URL` to the administrator connection configuration for the ensure phase.
CloudNativePG declares the restricted apply login. The platform SQL Job grants its system database privileges.
The installer supplies that public apply connection object in the native control configuration.
The other apply phases use the restricted credential from the worker's database volume.

## PostgreSQL client TLS

On a fresh install, cert-manager issues the identity, NATS, and system database CAs in `platform`. trust-manager runs in `cert-manager` and copies the public roots to org ConfigMaps. It also gives the web proxy the public identity CA in `hosts`.
Before you replace a signing root, plan a coordinated rollout.
Include trust bundle updates, leaf certificate reissuance, and restarts for clients that load trust at startup.

Runtime pools, OAuth transactions, and resource leases use SQLx with verified TLS.
Component configuration names the database host, port, database, login, login Secret, and CNPG CA Secret.
The login and CA Secrets live in the consumer’s namespace. Shared identity, the platform SQL Job, and the org provisioning Jobs mount them in `platform`. Hosts and CDC readers mount their database credentials in the environment namespace. Control mounts its system login in the org namespace and reads project logins through Kubernetes.
Operation workers copy the selected Secret keys to a memory-backed volume as the database names become known.
The database client reads `ca.crt` from `/etc/wamn/db/<login-secret>/` to verify the server.
For a password login, the client reads `username` and `password` from that directory.
It makes sure that the username matches the configured login.
For project logins and platform logins on the system database, SQLx reads `tls.crt` and `tls.key` on each new connection. PostgreSQL matches the certificate's common name to the configured login.
The namespaced system database issuer signs the platform certificates. The platform SQL Job mounts separate certificates for `postgres` and its restricted migration login. Org provisioning Jobs mount only the `postgres` certificate.
Project clients use `ca.crt` from the CloudNativePG server CA Secret to verify the server.
Explicit operator connection strings must name a readable CA file through `sslrootcert`.
The database hostname must match the server certificate.
The provisioned CNPG clusters accept client connections only over TLS.

## Kubernetes network policies

Org network policies deny ingress and egress by default.
`wamn org create` finds the API server destinations and copies `registryEgress` from `wamn-org-inputs` into `networkPolicy.registry`.
The registry rules allow control, the environment worker, and hosts to reach those CIDR blocks and TCP ports.
The environment and project charts allow connections to their host group and database. The edge accepts ingress from Traefik on installations that use Traefik.
The platform chart denies ingress by default and allows the specific callers of shared services.
`wamn platform deploy` discovers Kubernetes API addresses for the CloudNativePG webhook and for edge egress on kind.
On kind, edge egress allows org and env host groups, org control, identity, DNS, and the Kubernetes API.
Application deploy adds a public IPv4 egress policy for the TCP ports of that release's wasm integration authorities.
The rule excludes fixed private and special-use ranges. The runtime still checks each integration's allowed hosts.

## Kubernetes credentials

The project chart creates an empty `wamn-credential-<org>--<env>--<project>` Secret with one key per credential UUID.
Control reads and patches that Secret by name in the recorded environment namespace. Each merge patch updates one credential key and the labels control owns.
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

Connection administration gives the registry's credential Secret name and coordinate values to `KubernetesStore` in `wamn-connection`.
The store uses `wamn-kubernetes` to read and update the Secret.
Separate credential keys can change independently. The project chart must create the Secret before control writes credentials.

NATS runs in operator mode and authenticates clients with user JWTs.
Identity signs the `PLATFORM` account and one account for each environment. [NATS accounts](../architecture/platform-services.md#events)

### Transaction identity retention

Identity prunes system transaction identities in its one-minute maintenance loop.
It calls `platform.prune_tx_identity()` with a five-second statement timeout and logs any error.

Each org's control service removes expired project transaction identities every minute in a separate task.
Maintenance runs do not overlap. For each project database, connection and cleanup share a five-second limit.
The same task then has five seconds to copy current authority from the system database.
It logs a failed target and proceeds to the next project database.
Shutdown cancels the task before the service drains HTTP connections.
It connects to each configured project as `wamn_platform_login`.
It deletes rows with `xid < pg_snapshot_xmin(pg_current_snapshot())`.
The UNLOGGED table `platform.tx_identity` stays outside CDC publications. A crash discards its rows.

Before merging an identity change, measure read transaction COMMIT latency on one private PostgreSQL 18 server.
Set `DATABASE_URL` to the URL for a private database. Use a login that can install the schema and insert identity rows.

```sh
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -f deploy/sql/record-history.sql
cargo xtask measure-tx-identity "$DATABASE_URL" --samples 200
```

The command also reads `DATABASE_URL` if you omit the URL argument.
It compares COMMIT times with and without the insert on the same server.
It reports synchronous replication settings, WAL counters, and the flushed WAL position.
It commits the measurement identity rows.
