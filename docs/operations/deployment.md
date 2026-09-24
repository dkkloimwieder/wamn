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
The retained runtime-operator installation uses chart `2.9.0`.
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
Provisioning owns database schema, privileges, environment bindings, and broker stream configuration.
Runtime uses credentials scoped to that environment.
Declare stream replicas and the duplicate window in the environment configuration.
Activation compares declared configuration with the provisioned resources and refuses disagreement.
It does not grant runtime credentials permission to reconfigure streams.

Every tenant database needs its platform principal rows before any stamped write.
`wamn-ctl reconcile-run-plane` writes them, together with `app-schema.sql` and a service row for each service principal of the project.
It takes the email domain of those rows from `registry.meta.platform_domain` in the system database.
Set that value once per deployment, in the same step that applies `deploy/sql/system-schema.sql`:

```sql
UPDATE registry.meta SET platform_domain = 'example.invalid';
```

If the value is unset, `reconcile-run-plane` refuses with `platform-domain-unset`.
`wamn-ctl print-platform-principals --tenant "$TENANT" --platform-domain "$PLATFORM_DOMAIN"` prints the same SQL for an operator who applies it by hand.
Person rows are not written yet, as the [record history limits](../architecture/data-access.md#limits) state.

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

Before release publication, reconcile the target run schema and its environment policy:

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
The same run installs `app-schema.sql` and the tenant identity rows, after the catalog schema that carries the record-history functions their triggers call.
Existing reconciliation code does not establish support for post-install application schema upgrades.
That design remains in [upgrades](../plan/upgrades.md).

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
  --package-manifest apps/wamn_receiving/wamn.json

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

Copy its output into the selected complete host overlay.
Commit those configuration changes together.
The publisher and workloads can reach the registry through different hostnames.
Their artifact repository path must identify the same bytes.

## Activate the selected artifacts

Use the environment's explicit Kubernetes context and the matching application overlay.
For the retained Receiving example:

```bash
helm upgrade --install -n wamn-system wamn-host \
  oci://ghcr.io/wasmcloud/charts/runtime-operator --version 2.9.0 \
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
Restart the Deployment after changing the credential, sender, or operator CA.
The issuer credential needs the current grants from `provision-identity-issuer --prepare-generation`.
Use the current system schema on a fresh database, following the repository's schema installation contract.

The route bodies and limits are in [password enrollment](../architecture/execution.md#password-enrollment-foundation).
Use the [Receiving password login](development-loop.md#receiving-password-login) flow after provisioning the identity target and environment membership.

## Mailbox-loss recovery

An authorized administrator approves the replacement email and updates the existing human principal.
There is no separate trusted-contact channel or self-service email change.
The person then uses normal password recovery at the replacement address.

Use an authorized administrative connection to the identity service's `wamn_system` database.
The connection must permit `SET ROLE wamn_system`.
Do not use the scoped identity issuer credential, which cannot edit principals.
Use the administrator's existing principal UUID as the audit actor.
Use the affected person's existing principal UUID as the target.
Do not create another principal or change its subject, memberships, or roles.

Run this transaction in `psql` on that administrative connection:

```sql
\set ON_ERROR_STOP on
\prompt 'Administrator principal UUID: ' administrator_id
\prompt 'Affected human principal UUID: ' principal_id
\prompt 'Replacement email: ' replacement_email
BEGIN;
SET LOCAL ROLE wamn_system;
SELECT set_config('app.user_id', :'administrator_id', true);
SELECT EXISTS (
    SELECT 1 FROM identity.principals
    WHERE id = :'principal_id'::uuid AND kind = 'human'
    FOR UPDATE
) AS account_found \gset
\if :account_found
UPDATE identity.principals
SET email = lower(btrim(:'replacement_email'))
WHERE id = :'principal_id'::uuid AND kind = 'human';
UPDATE identity.password_tokens
SET consumed_at = clock_timestamp()
WHERE principal_id = :'principal_id'::uuid AND consumed_at IS NULL;
UPDATE identity.password_logins
SET revoked_at = clock_timestamp()
WHERE principal_id = :'principal_id'::uuid AND revoked_at IS NULL;
COMMIT;
\else
ROLLBACK;
\echo 'No matching human principal. Nothing changed.'
\endif
```

The database enforces email format and uniqueness. A failed statement leaves the transaction uncommitted.
If a statement fails, run `ROLLBACK` before correcting the input.
The principal lock serializes this update with password login, enrollment, reset, and renewal.
Old email secrets and renewal credentials cannot survive a successful correction.
Issued password tokens fail new admission after this transaction commits. PATs retain their separate revocation procedure.
The correction does not reactivate a disabled account or replace its password.

After commit, ask the person to request normal recovery with the replacement email.
The recovery endpoint and reset fields are in [password enrollment](../architecture/execution.md#password-enrollment-foundation).
Use `R` in the [Receiving sign-in screen](development-loop.md#receiving-password-login) to request recovery.
Keep reset secrets and passwords out of command arguments and logs.
After reset, require normal login with the new email and password.
Run the existing `reconcile-run-plane` procedure for affected environments to update their copied person rows.

## Identity target credentials

After replacing the identity service target Secret, restart the identity service.
Its database target connections are selected at process startup.
Use the environment's identity Deployment and require its readiness before issuing new sessions.

## Rollback and maintenance

Revert the workload configuration change and apply the previous release selection to roll back its artifact pointer.
A release digest selects exact bytes, but a mutable image tag does not.
A code rollback does not reverse database changes.
For schema changes, use a fresh target instead of implying an in-place upgrade path.

`wamn-ctl-ops` contains copy, run-history pruning, event advisory, and cluster recovery commands.
For backup and recovery, use CloudNativePG backup and recovery on the cluster.
The procedure is in [backup and recovery](backup-and-recovery.md).
Use its `--help` output for the selected action and required credentials.
These maintenance verbs do not authorize shared development targets or a new schema lifecycle.

## Trusted component reuse

The deployment owner selects exact reviewed component digests on `wamn-host`.
Keep the selection empty when guest-code correctness is not a sufficient isolation boundary.
Review and test the entire linked unit before granting trust.
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
