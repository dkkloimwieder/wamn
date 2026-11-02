# Deployment

Deployment has one path: `wamn-ctl env apply` of one environment document.
The document names the environment and its release.
The verb creates the environment and installs the release chart of the release.
[Google Cloud install](gcp.md#4-install) is the runbook of a full install.
The [repository delivery commands](delivery.md) qualify, publish, select, and deploy exact releases without a CI-provider API.
[Components](../architecture/components.md) defines the artifact boundary.

## Prerequisites

Run commands from the repository root.
Use an explicitly selected cluster, namespace, and environment.
Apply repository-wide resource restrictions from [AGENTS.md](../../AGENTS.md).
[Building](building.md) gives native and guest build commands.
[Cluster tests](cluster-tests.md) gives the existing deployed application cases.

The [deploy tree](../../deploy/README.md) contains infrastructure, platform manifests, test Jobs, and SQL.
Use the direct upstream revision pinned in `Cargo.toml`.
Use the runtime-operator chart version in the install command in `deploy/infra/values-wamn.yaml`.
The retained runtime-operator installation uses chart `2.11.0`.
Install its CRDs before the operator release.
Helm does not update existing CRDs during a chart upgrade.

Keep registry, database, and broker credentials outside committed files.
Supply real values for example Secrets and release placeholders before applying them.
Certificate subjects must match the endpoint names that their clients verify.

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

Provision the target before publishing its release.
First install the control store into the empty system database, once per deployment.
The database and its owner role `wamn_system` must exist, for example from the CloudNativePG `initdb` bootstrap:

```bash
wamn-ctl provision-system --system-url "$SYSTEM_ADMIN_URL" --platform-domain "$PLATFORM_DOMAIN"
```

The verb installs the record history, the system schema and the portable store as `wamn_system`, and closes the PUBLIC `CONNECT` and `TEMPORARY` floors.
It refuses a database that already has the schema `registry`.
Then run these steps in this order:

1. `provision-org --org <org> --template trials|standard|dedicated --system-database-url "$SYSTEM_ADMIN_URL" --admin EMAIL`.
2. `provision-identity-issuer --issuer <issuer URL>`.
3. Write one environment document. Run `wamn-ctl env apply` of it.

`provision-org --admin EMAIL` makes that user the org's admin. When the identity store has no user, it creates the user in the same transaction as the org, so a new install needs no other verb for its first user. When users exist, the email must name one of them, or the verb refuses and writes nothing.

`env apply` creates the environment row, its `Database`, its roles, its run plane, and its credential Secrets.
The first apply names `release = "none"`, because `publish-release` needs the environment's database.
A later apply names the release digest and installs the release chart.
The verb reads its platform inputs from environment variables ([Google Cloud platform file](gcp.md#410-platform-file)):

```bash
wamn-ctl env apply <document> --dry-run
wamn-ctl env apply <document>
```

`--dry-run` prints the plan and changes nothing.
`wamn-ctl env apply` serializes deployment under the lifecycle lock of the environment.
A CI completion order does not set deployment precedence.

If a package declares a registration condition that reads the old row, run `reconcile-replica-identity` after the package is applied.
The verb sets REPLICA IDENTITY FULL on each table that such a registration names. WMS needs it for `wms.packaging`:

```bash
wamn-ctl reconcile-replica-identity --admin-database-url "$PG_ADMIN_URL" --package apps/wamn_wms
```

The development loop applies this rule itself. After the package data access, it reconciles the replica identity of every package on each activation.

Without it, the old row carries only the key, so the condition cannot compare the old location.

Provisioning owns database schema, privileges, environment bindings, and broker stream configuration.
Runtime uses credentials scoped to that environment.
Declare stream replicas and the duplicate window in the environment configuration.
Activation compares declared configuration with the provisioned resources and refuses disagreement.
It does not grant runtime credentials permission to reconfigure streams.

A package schema change requires a new release and a reset of the application tables.

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
Platform release Jobs mount the selected authored document at `/inputs/environment.k`.
`publish-release` reads the file argument, compiles it, and takes its `applications` map.
The document supplies the organization, project, and environment.
A map with one application needs no `--application` selector.
For a map with multiple applications, pass the application name through `--application`.
Add one `--wiring` argument for each wiring that the release keeps. Receiving has none, because every Receiving attachment targets a route.

```bash
ENV_KEY=dkk--receiving--dev
WAMN_RELEASE_ARTIFACT_BASE="$RELEASE_BASE" wamn-ctl publish-release \
  deploy/gcp/environments/"$ENV_KEY".k \
  --database-url "$OWNER_URL" --control-database-url "$CONTROL_URL" \
  --verified-publisher-principal "$PUBLISHER_PRINCIPAL" \
  --run-schema "$RUN_SCHEMA" \
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
[Qualify the release](delivery.md#release-qualification) from the repository checkout before deployment.
Each package selects its cases through its own `tests/qualification.json` declaration.
`qualify-release` pushes the qualification artifact beside the release only when every case passes, and `env apply` reads it from the registry.
Name the release digest in the environment document, and run `env apply` of it ([deployment ordering](#deployment-ordering)).

## Web client files

Use [Web upload](web-upload.md#package-clients) to write the clients from the served release to a bucket.
That page owns the command, required inputs, destination paths, output, and errors.
The shell reads `/config.json` before sign-in. The local dev server answers it from `dev.json`.
The bucket must allow reads without credentials.
Platform apply renders each application host and API Service in the edge chart.

Release hosts expose workload HTTP on port 80, probes on port 8081, and native file transfers on port 8082.
The `<release>-files` Service selects the host group pods and maps port 80 to the named `files` port.
The edge sends `/api/<application>/files` and its child paths there before the general `/api/` route.
File requests keep the full path. Other API requests lose the `/api` prefix.

The Rust edge reads the live release and serves its bucket files.
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
[Google Cloud section 4](gcp.md#4191-control-host-and-platform-ui) owns its installation and credentials.

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
The chart renders no Google Cloud resource and no other difference from kind. [Google Cloud section 4.20](gcp.md#420-edge-and-load-balancer) creates the load balancer.
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
Both Resend values are required to enable the password routes.
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
`env apply` writes its PAT Secret when it can reach identity. When it cannot, mint the PAT with the Job of [Google Cloud releases](gcp.md#414-releases).

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

The full schema files under `deploy/sql/` own each platform schema and build fresh installations.
Application package migrations retain their own streams.

## Rollback and maintenance

Run `wamn-ctl env rollback <coordinate> --reason <text>`, which applies the previous intended release of the release chart.
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

For initial installation, set `WAMN_SYSTEM_ADMIN_URL` to the system administrator URL.
Run `wamn-ctl provision-system --platform-domain DOMAIN --emit-apply-secret PATH --db-host HOST` to write the restricted apply Secret.
The platform install Job supplies these arguments.
Set `WAMN_APPLY_DATABASE_URL` from the Secret URL before `wamn-ctl env apply`.
Set `WAMN_SYSTEM_ADMIN_URL` for its ensure phase.
The other apply phases use the restricted credential.
