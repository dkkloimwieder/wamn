# Deployment

The current path uses `wamn-ctl`, OCI artifacts, Helm, and Kubernetes commands.
An operator selects the release and applies its workload configuration.
The [repository delivery commands](delivery.md) qualify, publish, select, and deploy exact releases without a CI-provider API.
CI-provider configuration remains [delivery work](../plan/delivery.md).
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
The retained runtime-operator installation uses chart `2.10.0`.
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
Then run the verbs in this order: `provision-org`, `provision-project-env`, `provision-identity-issuer`, `reconcile-run-plane`, `apply-package`, `reconcile-package-data-access`, `push-component`.

The control host of an org holds the administration login of each environment of the org ([host-run routes](../architecture/execution.md#host-run-routes)).
`provision-org --emit-control-administration-secret PATH` writes the empty Secret `wamn-control-administration-<org>`. Apply it before the control host starts, because the host mounts it.
Each administration prepare of `provision-project-env` needs `--emit-control-administration-patch PATH`, and each rotation writes the patch again.
Apply the patch to that Secret with `kubectl patch secret wamn-control-administration-<org> --type merge --patch-file PATH`.
`delete-project-env --emit-control-administration-patch PATH` writes the patch that removes the key. Apply it before the run.

If a package declares a registration condition that reads the old row, run `reconcile-replica-identity` after `apply-package`.
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

Every tenant database needs its platform principal rows before any stamped write.
`wamn-ctl reconcile-run-plane` writes them, together with `app-schema.sql` and a service row for each service principal of the project.
It also writes a user row for each member of the environment, removes the user row of a user whose membership is gone, and writes the `admin` row of each member who holds `project-admin` in the project.
It takes the email domain of those rows from `registry.meta.platform_domain` in the system database.
`provision-system` sets that value once per deployment.

If the value is unset, `reconcile-run-plane` refuses with `platform-domain-unset`.
`wamn-ctl print-platform-principals --tenant "$TENANT" --platform-domain "$PLATFORM_DOMAIN"` prints the same SQL for an operator who applies it by hand.

After `provision-project-env` and `provision-identity-issuer`, and before any package step, reconcile the target run schema and its environment policy:

```bash
wamn-ctl reconcile-run-plane \
  --system-database-url "$SYSTEM_ADMIN_URL" \
  --admin-database-url "$TARGET_ADMIN_URL" \
  --org "$ORG" --project "$PROJECT" --tenant "$TENANT" \
  --env "$ENVIRONMENT" --schema "$RUN_SCHEMA"
```

`publish-release` refuses an absent or mismatched environment policy.
The reconciler requires the registry-derived target and administrative database authority.
Its `--dry-run` form prints the proposed changes without applying them.
The same run installs the catalog schema, which `apply-package` writes into, and then `app-schema.sql` and the tenant identity rows, whose triggers call the record-history functions of the catalog schema.
For an installed package successor, follow [package upgrades](#package-upgrades) before changing its application schema.

Apply the selected package migrations to a fresh target with `wamn-ctl apply-package`.
apply-package owns the grants of the `wamn_audit_retention` role on history tables.
It grants and revokes them in the transaction that writes the log triggers.
Reconcile generated data privileges with `wamn-ctl reconcile-package-data-access`.
Publish the selected component bytes with `wamn-ctl push-component`.
If the registry certificate chains to a private CA, pass that CA with `--oci-ca-path` or `WASH_OCI_CA_PATHS`.
Submit their wiring with `wamn-ctl author-wiring`, and bind declared connection aliases with `wamn-ctl bind-connection`.
Repeating `bind-connection` with identical inputs changes no records.
Changed definitions or credential references create an immutable generation and activate it in the same transaction.
The command prints the previous and new generation.
Invalid configuration or a concurrent change leaves the current selection untouched.
The command reports “configuration valid; connectivity and credentials not tested.”
It does not contact the external service or read its credentials.

Read each command's current required arguments with `--help`.

Schedule record history retention for each tenant database that has a relation with a `"P<n>D"` retention.
Prepare an audit retention credential generation with `wamn-ctl provision-project-env --prepare-audit-retention-generation`.
Apply its Secret and a CronJob like [`audit-retention.example.yaml`](../../deploy/platform/audit-retention.example.yaml).
The CronJob runs `wamn-ctl-ops prune-record-history --tenant "$TENANT"` once a day.

## Publish and select a release

The example below selects Receiving.
For an overlay, repeat package, attachment, and manifest arguments for the exact selected package set.
Use the package manifests as the source for that selection.
Add one `--wiring` argument for each wiring that the release keeps. Receiving has none, because every Receiving attachment targets a route.

```bash
wamn-ctl publish-release \
  --database-url "$OWNER_URL" --control-database-url "$CONTROL_URL" \
  --org "$ORG" --project "$PROJECT" \
  --tenant "$TENANT" --effective-release-id "$EFFECTIVE_RELEASE_ID" \
  --environment "$ENVIRONMENT" \
  --verified-publisher-principal "$PUBLISHER_PRINCIPAL" \
  --run-schema "$RUN_SCHEMA" \
  --package "$PACKAGE_ID@$PACKAGE_VERSION" \
  --attachments apps/wamn_receiving/publication/attachments.json \
  --route-host "$ROUTE_HOST" \
  --package-manifest apps/wamn_receiving/generated/wamn.json

wamn-ctl publish-qualified-release \
  --qualification "$DELIVERY_QUALIFICATION" \
  --database-url "$OWNER_URL" --control-database-url "$CONTROL_URL" \
  --tenant "$TENANT" --effective-release-id "$EFFECTIVE_RELEASE_ID" \
  --org "$ORG" --project "$PROJECT" \
  --artifact-base "$PUSH_BASE" --registry-auth-file "$PUSH_DOCKERCONFIG"

wamn-ctl print-release-env \
  --database-url "$OWNER_URL" --tenant "$TENANT" \
  --effective-release-id "$EFFECTIVE_RELEASE_ID" \
  --artifact-base "$PULL_BASE"
```

Publication freezes the effective release and its canonical manifest digest.
[Qualify the exact candidate](delivery.md#candidate-qualification) before publication.
The push requires that result, reads the frozen snapshot, and refuses conflicting artifact bytes.
It takes the same `--oci-ca-path` input as `push-component`.
`print-release-env` prints the host release settings without editing files.
A deployment that is not a qualification pushes the published manifest with `wamn-ctl push-release-manifest`, which takes the same arguments as `publish-qualified-release` without `--qualification`. It attests the push in the control database and records no source commit.

Copy its output into the selected complete host overlay.
Commit those configuration changes together.
The publisher and workloads can reach the registry through different hostnames.
Their artifact repository path must identify the same bytes.

## Activate the selected artifacts

Use the environment's explicit Kubernetes context and the matching application overlay.
For the retained Receiving example:

```bash
helm upgrade --install -n wamn-system wamn-host \
  oci://ghcr.io/wasmcloud/charts/runtime-operator --version 2.10.0 \
  -f deploy/platform/values-host-default.yaml \
  -f deploy/platform/values-host-receiving-pat.yaml
kubectl -n wamn-system rollout status deployment/hostgroup-default --timeout=150s
```

The host loads the manifest selected by its deployment configuration.
It refuses missing or inconsistent release artifacts and bindings.
A ready Pod does not establish successful authenticated application execution.
Exercise a meaningful operation and inspect its expected state or outcome before reporting deployment success.
Record the selected source, artifact identity, release identity, command exits, and actual observed outcome in one run directory.

An operator runs `wamn-ctl promote` by hand.
No deploy manifest runs it, and that is deliberate.
The verbs that manifests run are `wamn-ctl-ops prune-record-history`, `wamn-ctl-ops prune-run-history`, and `wamn-ctl reconcile-run-plane`.

`wamn-ctl promote` copies portable release facts only after the target's package and migration records match exactly.
It pulls each component artifact of the source release from the registry and verifies its bytes.
If the registry certificate chains to a private CA, pass that CA with `--oci-ca-path` or `WASH_OCI_CA_PATHS`.
It does not apply migrations to the target.
The [repository delivery commands](delivery.md) serialize activation against the existing selected release.
A CI completion order does not set deployment precedence.

## Web client files

`wamn web upload` builds an application's web client with Vite and writes it to a bucket.
Pass the manifest digest of the release that the client belongs to, and the project-environment database that holds the release head.
The command refuses a release that is not the head, before the build. `select-release` sets the head.

```bash
AWS_ENDPOINT=<object store URL> AWS_ACCESS_KEY_ID=<key> AWS_SECRET_ACCESS_KEY=<secret> \
  wamn web upload apps/wamn_receiving --release sha256:<manifest digest> --bucket s3://<bucket>/<prefix> \
    --org <org> --database-url <project-environment database URL>
```

On Google Cloud, pass a `gs://` sink. The command then uses Application Default Credentials, which `gcloud auth application-default login` writes as a user OAuth token. No HMAC key exists:

```bash
wamn web upload apps/wamn_receiving --release sha256:<manifest digest> --bucket gs://<bucket>/<prefix> \
  --org <org> --database-url <project-environment database URL>
```

The files go to `<prefix>/<package id>/<digest hex>/`, so each release keeps its own path.
The build carries no org or project, so the built files are the same bytes for every deployment.
The command writes `config.json` beside `index.html`, with the `--org` value and the project of the client package: `{"org":"<org>","project":"<project>"}`.
The shell reads `/config.json` before sign-in. The local dev server answers it from `dev.json`.
Each object carries its Cache-Control: `assets/` is `public, max-age=31536000, immutable`, and `index.html` and `config.json` are `no-cache`.
The command writes `index.html` last. The command refuses a built file that has no declared cache rule or content type.
The command writes each object create-only. An object that already exists refuses the upload, and the command never replaces it.
The bucket must allow reads without credentials.
The edge chart in `deploy/platform/edge` serves a list of `applications`. Each entry names its public `host`, the route ingress `api` of its host group, and its upload path `bucketPath`. The edge passes the headers of each object on.
In kind, the Receiving edge case uses the command. [Cluster tests](cluster-tests.md) describes that case.

## Google Cloud edge

On Google Cloud, a global load balancer sends `/api/` and `/password/` to the edge and serves every other path from a Cloud CDN backend bucket, as the [web deployment plan](../plan/web-deployment.md) describes.
Set `neg: true` in the edge values. The edge Service then carries the annotation that makes GKE create a network endpoint group, and the chart needs no `bucket.endpoint`, because the load balancer serves the bucket.
The chart renders no Google Cloud resource. [Google Cloud operations](gcp.md) creates the bucket access, the load balancer and the DNS record with `gcloud`, and lists their deletion.
The edge itself still removes `/api` and checks the identity certificate, as it does in kind.

## Identity password configuration

`wamn-identity` loads `.env` from its working directory before it starts the runtime.
Existing environment variables take precedence over `.env`; explicit command flags take precedence over both.
A missing `.env` is allowed. An unreadable or malformed file stops startup without printing its contents.
The root `.env.example` documents `RESEND_API_KEY` and `RESEND_FROM`.
The private `.env` is ignored by Git and needs owner-only read and write permissions (`0600`).
No home-directory path is required.

For local email configuration, copy `.env.example` to `.env` before adding the key and verified sender.
Keep the existing issuer, database, TLS, operator CA, and session target configuration.
Both Resend values are required to enable the password routes.
The CLI also accepts `--resend-api-key` and `--resend-from`; prefer environment input for the secret.

For Kubernetes, create a Secret with the `api-key` entry in the identity namespace.
Set the identity chart's `resendSecret` to that Secret and `resendFrom` to the verified sender.
Set `operatorCaSecret` to permit operator invitation requests.
Set `inviteUrl` to the web client base, for example `https://receiving.wamn.dev`. The invitation mail then carries one link to `<inviteUrl>/invite#<code>`, and the reset mail one link to `<inviteUrl>/reset#<secret>`. If `inviteUrl` is empty, each mail shows the code to paste into the terminal client.
An operator invites a user to an org with `wamn-ctl invite`, which uses the operator certificate flags of the PAT mint.
The command runs three steps:

1. Identity creates the user of the email, or reuses the user that has it. Identity refuses a disabled user.
2. One system database transaction, as `wamn_system` stamped `wamn:provisioning`, writes the active org membership and the requested grants.
3. Identity mails the invitation only when the user has no password. The command prints only identity's reply.

`--org-admin` and each `--project-admin` write the rows that `org_admin.grant` and `project_admin.grant` write (docs/plan/platform-ui.md §4.4).
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
Use the [Receiving password login](development-loop.md#receiving-password-login) flow after provisioning the identity target and environment membership.

## User roles

`apply-package` writes the built-in role `admin` in each tenant.
`admin` has no permission rows and holds every operation that the serving release serves.
Every other role is authored in the tenant, and it holds the stable operation references `<package>:<interface>/<operation>` selected for it, with the operations that each selection requires (docs/plan/platform-ui.md §2.3).
A user or a service reaches no route and no environment at sign-in until it holds a role.

Give a role with `wamn-ctl grant-role`, and take it away with `wamn-ctl revoke-role`.
Both verbs name the environment in the registry, as `reconcile-run-plane` does, and the user by email:

```bash
wamn-ctl grant-role --system-database-url "$WAMN_SYSTEM_ADMIN_URL" --admin-database-url "$WAMN_PG_ADMIN_URL" \
  --org <org> --project <project> --env <env> --tenant <tenant> --user <email> --role admin
```

The admin URL must be SUPERUSER or BYPASSRLS.
The verbs refuse a role that does not exist in the tenant, an unknown email, and an email that names more than one user.
The user needs its tenant `users` row, which `reconcile-run-plane` writes, so run that verb first after a new principal.
The operator service of an environment, `wamn-operator-<org>--<project>--<env>`, has the email `<subject>@<platform domain>`.
Its PAT comes from `provision-project-env --emit-operator-pat-secret`.

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

The operation must be served by the current serving release of the environment, which `select-release` or `promote` sets.
The grant also writes each operation that the selected operation requires in that release, and it prints them.
`wamn-ctl revoke-permission` removes the selection and the operations that it required.
An operation that another selected operation of the role requires stays effective, and the verb names that operation.
The verb refuses an operation that the role holds only because another selected operation requires it.

Before a release becomes current, `promote`, `select-release`, and the dev loop update these rows from the new release.
A selection that the new release does not serve is removed.

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
Use `R` in the [Receiving sign-in screen](development-loop.md#receiving-password-login) to request recovery.
Keep reset secrets and passwords out of command arguments and logs.
After reset, require normal login with the new email and password.
Run the existing `reconcile-run-plane` procedure for affected environments to update their copied user rows.

## Identity target credentials

After replacing the identity service target Secret, restart the identity service.
Its database target connections are selected at process startup.
Use the environment's identity Deployment and require its readiness before issuing new sessions.

## Platform schema upgrades

`wamn-ctl upgrade-schema` applies a platform schema change to an installed database.
A platform schema change is a numbered file under `deploy/sql/migrations/system/` or `deploy/sql/migrations/project/`, which lands in the commit of its full-file change.
The design is in [schema upgrade](../plan/schema-upgrade.md).
Package migrations of an application are not platform migrations.

Upgrade the system database first, then each project-environment database:

```bash
wamn-ctl upgrade-schema --system-database-url "$SYSTEM_ADMIN_URL"
wamn-ctl upgrade-schema --system-database-url "$SYSTEM_ADMIN_URL" --confirm
wamn-ctl upgrade-schema --system-database-url "$SYSTEM_ADMIN_URL" --admin-database-url "$TARGET_ADMIN_URL" --confirm
```

Without `--confirm`, the verb prints the pending files and changes nothing.
One run applies the pending files in one transaction and records them in `registry.schema_migrations` or `app_system.schema_migrations`.
A failure leaves the database as it was.
The verb refuses a recorded file that differs from the file of the binary, with `schema-migration-drift`.
It runs system files as `wamn_system` and project files as the admin connection.

`provision-system` and `reconcile-run-plane` record every file in a fresh install, so a new target needs no upgrade run.
A database installed before its record table existed takes `--baseline <ordinal>` on its first run only.
The verb then records the files up to that ordinal without running them, and applies the rest in the same run.
If a change breaks a running binary, stop that binary before the run.

## Package upgrades

This path advances one installed package to its direct successor while retaining its rows.
The successor names the installed version as `predecessor_version` and preserves its migration stream as an exact byte prefix.
An installed overlay that pins the base requires a coordinated upgrade with every affected overlay successor.
An overlay is a package that consumes a pinned base.
Each overlay successor changes only its package coordinate and exact base version/digest pin.
Every consumed operation contract stays unchanged. Changed or removed contracts remain named Epic 3 work.

Use a patch version for an internal correction without an application migration suffix.
Use a minor version for predecessor-compatible additive schema or operation changes.
Use a major version for an incompatible contract or migration that requires client changes.
A nonempty application migration suffix requires at least a minor version, such as `2.0.0 → 2.1.0`.

The shared upgrade policy permits new ordinary tables, nullable modeled columns without defaults, and two non-null constant defaults: `boolean DEFAULT false` and `text DEFAULT 'not_required'`.
It refuses new constraints on existing relations, type changes, backfills, and destructive changes.
Migration-specific exceptions, including a whole-row query that loses access to an added column, remain in Epic 3 of [package upgrade](../plan/package-upgrade.md).

`qualify-upgrade` proves the database transition before production mutation.
It copies the installed predecessor database, preserves object ownership, restores predecessor grants, and applies the candidate suffix on that copy.
It plans predecessor SQL under both predecessor grants and the complete candidate grants after reconciliation.
A failure before reconciliation refuses the upgrade even if candidate grants restore access.
It also checks candidate generated artifacts and statements against the upgraded copy.

`qualify-release` remains the single release-qualification path defined by [release qualification](../plan/release-qualification.md).
It proves the exact published release, source, and deployment artifacts after the package upgrade.
An upgrade result cannot satisfy `publish-qualified-release` or replace ordinary release qualification.

### Upgrade prerequisites

Use one deployment writer for the environment. Resolve any mismatch between the selected release and serving workloads before qualification.
The observer compares the selected manifest digest with `--release-manifest-digest` on fully rolled-out, ready host pods.
It follows each selected `WorkloadDeployment` through its current replica set to the ready, correctly placed `Workload` objects.
The Kubernetes arguments select those resources. They do not assert application schemas.

Each SQL-bearing package must have one actual serving `wamn.schema`, with the expected tenant and environment.
The successor keeps that schema. Predecessor and candidate SQL use the same exact `search_path`.
Absent or ambiguous workloads, multiple schema surfaces, conflicting replica schemas, and schema changes refuse qualification.
There are no predecessor-schema or candidate-schema flags.
Packages with no SQL need no application schema or `--package-workload` selector. Host convergence still applies.
Schema relocation or multiple-schema support requires a future persisted deployment fact outside the additive upgrade path.

Apply pending [platform schema migrations](#platform-schema-upgrades) before application upgrade, including the `catalog.package_upgrade_qualifications` carrier.
Keep the exact predecessor release available for recovery.
`upgrade-environment` reads installed package artifacts from the environment's `registry` value plus `/packages`.
It uses the same login as component publication. Immutable package tags identify each package ID and version.
The operator pushes the artifacts of older installed versions once from their attested commits before an upgrade.
The package stage calls `push-package` for each newly installed version before application, with the run's commit and component index.
An existing package tag with different bytes refuses before application.
The verb fetches each predecessor by its recorded digest and compares its frozen manifest, migrations, and SQL with installed and serving facts.
It does not rebuild predecessor SQL with the current generator.

The package stage qualifies the complete successor set before application.
An additive base upgrade and all affected overlay successors apply in one transaction, with the base first.
A retry requires the saved qualification to match the exact environment, complete package roots, and selected base component bytes.
The verb refuses multiple independent base transitions in one run.
Prepare the complete candidate root set, including generated artifacts. Ordinary release qualification must support the environment's candidate shape.
The completed Epic 1 WMS proof is recorded in [Google Cloud operations](gcp.md).

Run qualification on a machine authorized to hold copied production data, with PostgreSQL 18 tools and enough temporary storage.
The command owns its temporary PostgreSQL server and private dump directory and removes them after use.
It copies application data, preserves required ownership, and reconstructs effective privileges without copying cluster login-role secrets.
Keep database credentials outside committed files. Retain the qualification result through application and resolve any reported cleanup failure.

### Upgrade order

Set the paths and scope below for the chosen environment. `UPGRADE_RESULT` must name a new file.
For WMS, `CANDIDATE_PACKAGE` is the prepared `wamn_wms@2.1.0` root and `WORKLOAD_DEPLOYMENT` identifies its serving HTTP workload.
Repeat `--presented-package` for every installed lineage and `--package-workload PACKAGE=NAME` for each SQL-bearing package in the proof.

```bash
wamn-ctl qualify-upgrade \
  --database-url "$OWNER_URL" --tenant "$TENANT" --environment "$ENVIRONMENT" \
  --package "$CANDIDATE_PACKAGE" --presented-package "$CANDIDATE_PACKAGE" \
  --kubeconfig "$KUBECONFIG_FILE" --context "$KUBE_CONTEXT" --namespace "$NAMESPACE" \
  --host-deployment "$HOST_DEPLOYMENT" \
  --package-workload "$PACKAGE_ID=$WORKLOAD_DEPLOYMENT" \
  --result "$UPGRADE_RESULT"

wamn-ctl apply-package \
  --database-url "$OWNER_URL" --tenant "$TENANT" \
  --package "$CANDIDATE_PACKAGE" --upgrade-qualification "$UPGRADE_RESULT"

wamn-ctl reconcile-package-data-access \
  --database-url "$OWNER_URL" --tenant "$TENANT" --package "$CANDIDATE_PACKAGE"

wamn-ctl reconcile-replica-identity \
  --admin-database-url "$OWNER_URL" --package "$CANDIDATE_PACKAGE"
```

Repeat reconciliation's `--package` arguments for the same complete root set that qualification used.
Run replica-identity reconciliation for each package whose registrations require old-row fields.
Continue through the existing [release commands](delivery.md) in this order:

```text
push-component → publish-release → prepare-release → qualify-release
→ publish-qualified-release → select-release
→ host/workload deployment → readiness and authenticated operation
→ optional wamn web upload
```

Before mutation, application rechecks the predecessor head, package bytes, privileges, and observed workload identities, specifications, and schemas.
It stores accepted canonical evidence and its digest in `catalog.package_upgrade_qualifications` within the package application transaction.
An exact retry changes nothing. Conflicting evidence for the same coordinate refuses.
Data-access reconciliation consumes the persisted evidence and refuses a changed root set or derived privilege state.
Reconciliation requires a qualification that matches the complete current roots, candidate transition, and derived privileges.
It does not select by timestamp. Earlier qualifications remain immutable history and do not independently constrain later package sets.
Each subsequent qualification starts from the complete installed set and proves the complete successor set.

Kind delivery uses `deploy-release` after selection.
wamn-dev/GCP uses generated host values, `helm upgrade`, and `kubectl apply` of the released workloads, as [Google Cloud operations](gcp.md) records.
Wait for readiness and prove a meaningful authenticated application operation before reporting success.
Upload web files only after selecting their release.

### Coordinated base and overlay upgrade

Epic 2 permits additive base migrations and metadata-only overlay successors with exact base version and component digest pins.
Qualification compares predecessor and successor declarations of every consumed operation and refuses changed or removed contracts.
It hashes the exact built base artifact and requires every overlay to pin that digest.
The successor root set includes every affected overlay and every unchanged installed package.
The original root set must match every currently installed package.
Qualification records format 2 evidence for this coordinated transition. Existing format 1 evidence remains unchanged.

The application transaction applies the base first, then affected overlay successors sorted by package ID.
It commits all registrations, migrations, and accepted evidence together, or rolls them all back.
SQL that cannot run inside a transaction is refused as `NontransactionalOperation`, with `nontransactional SQL is forbidden`.
This includes `CREATE INDEX CONCURRENTLY`. There is no separate application step for that SQL.
The same actual serving schema rule applies to the base and overlays.

Set `BASE_PREDECESSOR` and `OVERLAY_PREDECESSOR` to the installed package roots.
Set `BASE_SUCCESSOR` and `OVERLAY_SUCCESSOR` to their prepared direct successor roots.
Set `BASE_COMPONENT` to the exact built successor base artifact named by each overlay pin.
Use the owner connection and Kubernetes scope from [upgrade prerequisites](#upgrade-prerequisites).
Repeat `--predecessor-package` for the complete original root set, including unchanged packages.
Repeat `--presented-package` for the complete successor root set in both commands.
Repeat `--package-workload PACKAGE=NAME` for every SQL-bearing package in the proof.

```bash
wamn-ctl qualify-upgrade \
  --database-url "$OWNER_URL" --tenant "$TENANT" --environment "$ENVIRONMENT" \
  --package "$BASE_SUCCESSOR" --base-component "$BASE_COMPONENT" \
  --predecessor-package "$BASE_PREDECESSOR" --predecessor-package "$OVERLAY_PREDECESSOR" \
  --presented-package "$BASE_SUCCESSOR" --presented-package "$OVERLAY_SUCCESSOR" \
  --kubeconfig "$KUBECONFIG_FILE" --context "$KUBE_CONTEXT" --namespace "$NAMESPACE" \
  --host-deployment "$HOST_DEPLOYMENT" \
  --package-workload "$BASE_PACKAGE_ID=$BASE_WORKLOAD_DEPLOYMENT" \
  --package-workload "$OVERLAY_PACKAGE_ID=$OVERLAY_WORKLOAD_DEPLOYMENT" \
  --result "$UPGRADE_RESULT"

wamn-ctl apply-package \
  --database-url "$OWNER_URL" --tenant "$TENANT" \
  --package "$BASE_SUCCESSOR" --upgrade-qualification "$UPGRADE_RESULT" \
  --presented-package "$BASE_SUCCESSOR" --presented-package "$OVERLAY_SUCCESSOR"
```

The coordinated application requires accepted upgrade evidence. Supplying roots does not bypass qualification.
After application, reconcile the complete successor root set and continue through the ordinary release commands above.
Ordinary overlay admission and release qualification remain separate requirements.

PostgreSQL tests with retained predecessor data provide the Epic 2 exit evidence.
When Epic 2 lands, run one live forward upgrade through `upgrade-environment` on `wamn-dev`.
Record its command and result on `wamn-orb5`.
Epic 2 does not require a scheduled live rollback and re-forward cycle.

### Upgrade failures

| Failure point | Installed state and recovery |
| --- | --- |
| Qualification | Production is unchanged. Correct the candidate or observed deployment and qualify again. |
| Package application | The transaction rolls back, including candidate registration and accepted evidence. Correct the cause and retry. |
| Application committed, data access not reconciled | The candidate schema remains with predecessor grants. Qualification proved predecessor SQL in this state. Repair reconciliation using the qualified roots. |
| Data access committed, replica identity or later publication/qualification failed | The predecessor serves on candidate schema and grants. Repair and continue, or abandon that release attempt. |
| Selection or host deployment | The head and serving release can differ. Complete deployment or select the qualified immediate predecessor and restore its workloads. Restore convergence before another upgrade. |

Never reverse a committed package migration for recovery.
Any replacement package candidate names the installed leaf as its predecessor, including after a later deployment failure.

## Rollback and maintenance

Select the qualified previous release, then restore its host and workload configuration through the environment's normal deployment procedure.
A release digest selects exact bytes, but a mutable image tag does not.
The installed package and its committed migrations remain in place.
Reverse migration is not rollback.

After an additive package upgrade, rollback supports only the exact immediate predecessor release recorded in accepted upgrade evidence.
`select-release` and kind `deploy-release` use the same compatibility rule: exact installed migration signatures, or persisted predecessor compatibility with unchanged relevant live state.
They refuse missing evidence, a changed installed leaf, a non-prefix history, a different predecessor manifest, or changed qualified data privileges.
Rollback needs no operator-local upgrade result file. Ordinary release qualification and publication requirements still apply.
On wamn-dev/GCP, select the predecessor, render its host values, run `helm upgrade`, apply its workloads, and prove readiness and authenticated operation success.
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
