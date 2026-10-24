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

### WMS object-store credentials

Before applying the WMS overlay, create `wamn-object-store-credentials-acme--wms--dev` in the host namespace.
Store the host credential map under its `credentials.json` key.
The map uses project `wms` and connection handle `labels-store`, with the object-store credential JSON encoded as that handle's string value.
The [WMS cluster setup](../../apps/wamn_wms/tests/cluster/deployment.rs) creates this Secret for its tests.
Production deployment supplies its own credentials through the same Secret contract.

### HTTP connection credentials

Before starting the host, create a connection Secret in its namespace.
Store the credential map under `credentials.json`.
The `projects` map uses the project as its key.
The inner key names the connection instance, which `env apply` also uses as its credential handle.
Each inner value is a JSON string with a `headers` object:

```yaml
apiVersion: v1
kind: Secret
metadata:
  name: wamn-http-credentials-acme--orders--dev
  namespace: wamn-system
type: Opaque
stringData:
  credentials.json: |
    {"projects":{"orders":{"upstream":"{\"headers\":{\"authorization\":\"Bearer <token>\"}}"}},"orgs":{}}
```

Replace `<token>` with the endpoint credential before creating the Secret.
Keep the Secret manifest outside committed files.
For an endpoint that needs no credential, use `"{\"headers\":{}}"` as the inner value.

Mount this Secret at `/etc/wamn/credentials` in the host values.
Map its `credentials.json` key to the file of the same name.
Set `WAMN_CREDENTIALS_FILE=/etc/wamn/credentials/credentials.json`.
Set `WAMN_ALLOWED_HOSTS` to the endpoint origin, such as `https://api.example.com:443`.
The [WMS host values](../../deploy/platform/values-host-wms-pat.yaml) show the volume and mount structure.
`env apply` does not mount connection credentials.
The operator supplies these host values through `WAMN_RELEASE_HOST_GROUP`.

For a shared org connection, set `WAMN_CONTROL_DATABASE_URL` to the control database URL.
Apply system migration `0027_org_connections.sql` with `upgrade-schema` before declaring the connection.
Write the non-secret definition to `erp-definition.json`:

```json
{"primary-authority":"https://api.example.com/","tls-verification":"verify-authority"}
```

Declare the connection:

```bash
wamn-ctl connection declare --org acme --name erp \
  --requirement-type http --definition erp-definition.json
```

The command accepts only HTTP connections and never accepts credential bytes.
A repeated declaration updates only a changed definition.
The connection type cannot change under an existing name.

Use the `orgs` map to share one org secret:

```json
{
  "projects": {},
  "orgs": {
    "acme": {
      "erp": "{\"headers\":{\"authorization\":\"Bearer <token>\"}}"
    }
  }
}
```

Mount this file into each host that uses the connection.
Keep project credentials under `projects` when both forms are needed.
Each host resolves `org:acme:erp` only when its startup org is `acme`.
An absent org entry never uses a project entry as a fallback.
Restart each host after changing secret bytes.

Use `connections = { upstream = { org = "erp" } }` in each environment document.
Each `env apply` uses one snapshot of the org definition.
It applies project migration `0023_org_connection_source.sql` before connection writes, including when `release = "none"`.
A changed definition creates one generation at the next apply of each environment.
A repeated apply with the selected definition creates no generation.
The command tests neither endpoint connectivity nor secret validity.
`env show` preserves the org reference, and rollback uses the current org definition.

Bind the release's `upstream` HTTP requirement with an `environment.k` document:

```python
import environment

environment.Environment {
    org = "acme"
    project = "orders"
    env = "dev"
    release = "sha256:<release digest>"
    route_host = "orders.example.com"
    policy = "dev"
    applications = {
        orders = {packages = ["orders"]}
    }
    connections = {
        upstream = {
            type = "http"
            definition = {
                "primary-authority" = "https://api.example.com/"
                "tls-verification" = "verify-authority"
            }
        }
    }
}
```

Replace the release placeholder with the published digest.
`applications` is required. It names each application of the environment and the packages of the release that belong to it ([platform configuration](../plan/platform-config.md)).
With a release, `env apply` refuses a release package that no application lists, a package that two applications list, and a listed package that the release does not carry.
It also refuses an empty application name, an application with no packages, and an empty package id.
Workflows belong to an application by its name.
Run `wamn-ctl env apply environment.k`.
The host reads the credential and adds its headers to the outgoing request.
The guest receives the response and never receives the credential.

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

For an installed package successor, follow [package upgrades](#package-upgrades) before changing its application schema.

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
Add one `--wiring` argument for each wiring that the release keeps. Receiving has none, because every Receiving attachment targets a route.

```bash
WAMN_RELEASE_ARTIFACT_BASE="$RELEASE_BASE" wamn-ctl publish-release \
  --database-url "$OWNER_URL" --control-database-url "$CONTROL_URL" \
  --org "$ORG" \
  --tenant "$TENANT" --environment "$ENVIRONMENT" \
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
The manifest names no tenant and no environment, so one release has one digest in every environment.
The tenant of an environment is `<org>--<project>--<environment>`.
The release also names no route host.
The route host is only the `host` of the http workload config. The router matches routes under it.
`publish-release` takes the same `--oci-ca-path` input as `push-package`.
[Qualify the release](delivery.md#release-qualification) before deployment.
`qualify-release` pushes the qualification artifact beside the release only when every case passes, and `env apply` reads it from the registry.
Name the release digest in the environment document, and run `env apply` of it ([deployment ordering](#deployment-ordering)).

## Web client files

Use [Web upload](web-upload.md#package-clients) to write the clients from the served release to a bucket.
That page owns the command, required inputs, destination paths, output, and errors.
The shell reads `/config.json` before sign-in. The local dev server answers it from `dev.json`.
The bucket must allow reads without credentials.
Platform apply renders each application host and API Service in the edge chart. The Rust edge reads the live release and serves its bucket files.
In kind, the Receiving edge case uses the command. [Cluster tests](cluster-tests.md) describes that case.

## Platform UI files

Use [Web upload](web-upload.md#platform-ui) to upload the local platform UI with its matching stamped chart.
The default destination is `gs://wamn-dev-web/platform-ui/<platform version>/`.
The upload page owns the command, build comparison, organization configuration, and refusal of existing objects.

The control root is `https://control.wamn.dev`.
Its host group runs `wamn-host host --control --org=dkk` in namespace `hosts`.
Its platform HTTP role contains the pinned route guest and an empty dispatcher, with `poolSize: 0` and no release record.
The HTTP transport workload selects that host group and supplies endpoints for the fixed Service `c-dkk-http`.
[Google Cloud section 4](gcp.md#4191-control-host-and-platform-ui) owns its installation and credentials.

The edge values contain a separate `control` entry with `host`, `api` and `bucketPath`.
The edge chart forwards `/api/` to the control Service and `/password/` to identity.
With a bucket endpoint, it serves the control bundle directly.
On Google Cloud, the TCP proxy sends all traffic to nginx. The Rust edge serves the platform UI files behind nginx.
Every other page reaches that prefix's `index.html`, including `/`, `/invite`, `/recover`, `/reset`, and control or project-environment audience paths.

## Google Cloud edge

On Google Cloud, a global load balancer sends `/api/` and `/password/` to the edge and serves every other path from a Cloud CDN backend bucket.
Set `neg: true` in the edge values. The edge Service then carries the annotation that makes GKE create a network endpoint group, and the chart needs no `bucket.endpoint`, because the load balancer serves the bucket.
The chart renders no Google Cloud resource. [Google Cloud section 4.20](gcp.md#420-edge-and-load-balancer) creates the load balancer.
The edge itself still removes `/api` and checks the identity certificate, as it does in kind.

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
An operator invites a user to an org with `wamn-ctl invite`, which uses the operator certificate flags of the PAT mint.
The command runs three steps:

1. Identity creates the user of the email, or reuses the user that has it. Identity refuses a disabled user.
2. One system database transaction, as `wamn_system` stamped `wamn:provisioning`, writes the active org membership and the requested grants.
3. Identity mails the invitation only when the user has no password. The command prints only identity's reply.

`--org-admin` and each `--project-admin` write the rows that `org_admin.grant` and `project_admin.grant` write.
Each `--membership <project>/<env>` writes one environment membership.
All three flags are optional, and `--project-admin` and `--membership` can repeat:

```bash
wamn-ctl invite --email <email> --display-name <name> --org <org> \
  [--org-admin] [--project-admin <project>]... [--membership <project>/<env>]... \
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
Both verbs name the environment in the registry, as `env apply` does, and the user by email:

```bash
wamn-ctl grant-role --system-database-url "$WAMN_SYSTEM_ADMIN_URL" --admin-database-url "$WAMN_PG_ADMIN_URL" \
  --org <org> --project <project> --env <env> --tenant <tenant> --user <email> --role admin
```

The admin URL must be SUPERUSER or BYPASSRLS.
The verbs refuse a role that does not exist in the tenant, an unknown email, and an email that names more than one user.
The user needs its tenant `users` row, which `wamn-ctl env apply` writes, so apply the environment after a new principal and before `grant-role`.
The operator service of an environment, `wamn-operator-<org>--<project>--<env>`, has the email `<subject>@<platform domain>`.
`env apply` writes its PAT Secret when it can reach identity. When it cannot, mint the PAT with the Job of [Google Cloud releases](gcp.md#414-releases).

### Authored roles

Create an authored role with `wamn-ctl create-role`, and delete it with `wamn-ctl delete-role`.
The delete also removes the role's assignments and permissions.
The verbs take the same environment flags as `grant-role`, and `--role` names the role.
A role name has lowercase letters, digits, and hyphens after the first character, and at most 64 bytes.
The verbs refuse `admin`.

Select an operation for an authored role with `wamn-ctl grant-permission`:

```bash
wamn-ctl grant-permission --system-database-url "$WAMN_SYSTEM_ADMIN_URL" --admin-database-url "$WAMN_PG_ADMIN_URL" \
  --org <org> --project <project> --env <env> --tenant <tenant> --role <role> \
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

`wamn-ctl upgrade-schema` applies a platform schema change to an installed database.
A platform schema change is a numbered file under `deploy/sql/migrations/system/` or `deploy/sql/migrations/project/`.
Package migrations of an application are not platform migrations.

Upgrade the system database before you apply an environment on a new platform version:

```bash
wamn-ctl upgrade-schema --system-database-url "$SYSTEM_ADMIN_URL"
wamn-ctl upgrade-schema --system-database-url "$SYSTEM_ADMIN_URL" --confirm
```

You do not upgrade a project-environment database by hand.
The package stage of `wamn-ctl env apply` applies the pending project files of its binary first, and a refusal stops the apply before any Kubernetes write.

Without `--confirm`, the verb prints the pending files and changes nothing.
One run applies the pending files in one transaction and records them in `registry.schema_migrations` or `platform.schema_migrations`.
A failure leaves the database as it was.
The verb refuses a recorded file that differs from the file of the binary, with `schema-migration-drift`.
It runs system files as `wamn_system` and project files as the admin connection.

`provision-system` and `env apply` record every file in a fresh install, so a new target needs no upgrade run.
A database installed before its record table existed takes `--baseline <ordinal>` on its first run only.
The verb then records the files up to that ordinal without running them, and applies the rest in the same run.
If a change breaks a running binary, stop that binary before the run.

## Package upgrades

This path advances one installed package to its direct successor while retaining its rows.
The successor names the installed version as `predecessor_version` and preserves its migration stream as an exact byte prefix.
An installed overlay that pins the base requires a coordinated upgrade with every affected overlay successor.
An overlay is a package that consumes a pinned base.
Each overlay successor changes only its package coordinate and exact base version/digest pin.
Every consumed operation contract stays unchanged.

Use a patch version for an internal correction without an application migration suffix.
Use a minor version for predecessor-compatible additive schema or operation changes.
Use a major version for an incompatible contract or migration that requires client changes.
A nonempty application migration suffix requires at least a minor version, such as `2.0.0 → 2.1.0`.

The ordinary upgrade policy permits new ordinary tables, nullable modeled columns without defaults, and two non-null constant defaults: `boolean DEFAULT false` and `text DEFAULT 'not_required'`.
It refuses new constraints on existing relations, type changes, backfills, and destructive changes.

`qualify-upgrade` proves the database transition before production mutation.
It copies the installed predecessor database, preserves object ownership, restores predecessor grants, and applies the candidate suffix on that copy.
It plans predecessor SQL under both predecessor grants and the complete candidate grants after reconciliation.
A failure before reconciliation refuses the upgrade even if candidate grants restore access.

`qualify-release` remains the single release-qualification path that [release qualification](delivery.md#release-qualification) describes.
It proves the published release on a platform image set after the package upgrade.
An upgrade result cannot replace release qualification.

### Upgrade prerequisites

Use one deployment writer for the environment.
`--predecessor-release` names the manifest digest of the release the environment serves.
The command refuses it when `catalog.releases` has no row for it, or when it names a package version that is not installed.
Qualification reads no Kubernetes resource.

Each SQL-bearing package must declare one application schema. Predecessor SQL plans with that schema as its exact `search_path`.
A schema change makes predecessor SQL fail to plan and refuses qualification.
Packages with no SQL need no application schema.

Apply pending [platform schema migrations](#platform-schema-upgrades) before application upgrade, including the `catalog.package_upgrade_qualifications` carrier.
Keep the exact predecessor release available for recovery.
`upgrade-environment` reads installed package artifacts from the environment's `registry` value plus `/packages`.
It uses the same login as component publication. Immutable package tags identify each package ID and version.
The operator pushes the artifacts of older installed versions once from their attested commits before an upgrade.
The package stage calls `push-package` for each newly installed version before application, with the run's commit and component index. It pushes the components under the `registry` value plus `/components` and the package artifact under `/packages`.
An existing package tag with different bytes refuses before application.
The verb fetches each predecessor by its recorded digest and compares its frozen manifest, migrations, and SQL with installed and serving facts.
It does not rebuild predecessor SQL with the current generator.

The package stage qualifies the complete successor set before application.
It takes the predecessor release from the `--predecessor-release <DIGEST>` input, the manifest digest of the release the environment serves.
Qualification refuses the digest unless `catalog.releases` holds it and its package versions equal the installed versions.
An upgrade of an installed package without this input refuses.
An additive base upgrade and all affected overlay successors apply in one transaction, with the base first.
A retry requires the saved qualification to match the exact environment, complete package roots, and selected base component bytes.
The verb refuses multiple independent base transitions in one run.
Prepare the complete candidate root set, including generated artifacts. Ordinary release qualification must support the environment's candidate shape.

Run qualification on a machine authorized to hold copied production data, with PostgreSQL 18 tools and enough temporary storage.
The command owns its temporary PostgreSQL server and private dump directory and removes them after use.
It copies application data, preserves required ownership, and reconstructs effective privileges without copying cluster login-role secrets.
Keep database credentials outside committed files. Retain the qualification result through application and resolve any reported cleanup failure.

### Upgrade order

Run `wamn-ctl upgrade-environment` as [Google Cloud operations](gcp.md#56-upgrade-an-environment) shows.

### Upgrade failures

| Failure point | Installed state and recovery |
| --- | --- |
| Qualification | Production is unchanged. Correct the candidate or observed deployment and qualify again. |
| Package application | The transaction rolls back, including candidate registration and accepted evidence. Correct the cause and retry. |
| Application committed, data access not reconciled | The candidate schema remains with predecessor grants. Qualification proved predecessor SQL in this state. Repair reconciliation using the qualified roots. |
| Data access committed, replica identity or later publication/qualification failed | The predecessor serves on candidate schema and grants. Repair and continue, or abandon that release attempt. |
| `env apply` write or readiness | The release chart serves its newest successful revision. Apply again, or run `wamn-ctl env rollback`. Restore convergence before another upgrade. |

Never reverse a committed package migration for recovery.
Any replacement package candidate names the installed leaf as its predecessor, including after a later deployment failure.

## Rollback and maintenance

Run `wamn-ctl env rollback <coordinate> --reason <text>`, which applies the previous intended release of the release chart.
A release digest selects exact bytes, but a mutable image tag does not.
The installed package and its committed migrations remain in place.
Reverse migration is not rollback.

After an additive package upgrade, rollback supports only the exact immediate predecessor release recorded in accepted upgrade evidence.
`apply` refuses a release whose packages the installed schema is not compatible with.
Rollback needs no operator-local upgrade result file. Ordinary release qualification and publication requirements still apply.
Prove readiness and authenticated operation success after the rollback.
The installed successor schema remains throughout rollback and any later re-forward deployment.

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
