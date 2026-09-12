# Deployment

The current path uses `wamn-ctl`, OCI artifacts, Helm, and Kubernetes commands.
An operator selects the release and applies its workload configuration.
Provider-independent CI automation remains [delivery work](../plan/delivery.md).
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

Apply the selected package migrations to a fresh target with `wamn-ctl apply-package`.
Reconcile generated data privileges with `wamn-ctl reconcile-package-data-access`.
Publish the selected component bytes with `wamn-ctl push-component`.
Submit their wiring with `wamn-ctl author-wiring`, and bind declared connection aliases with `wamn-ctl bind-connection`.
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
Existing reconciliation code does not establish support for post-install application schema upgrades.
That design remains in [upgrades](../plan/upgrades.md).

## Publish and select a release

The example below selects Receiving.
For an overlay, repeat package, wiring, attachment, and manifest arguments for the exact selected package set.
Use the package manifests as the source for that selection.

```bash
wamn-ctl publish-release \
  --database-url "$OWNER_URL" --control-database-url "$CONTROL_URL" \
  --org "$ORG" --project "$PROJECT" \
  --tenant "$TENANT" --effective-release-id "$EFFECTIVE_RELEASE_ID" \
  --environment "$ENVIRONMENT" \
  --verified-publisher-principal "$PUBLISHER_PRINCIPAL" \
  --run-schema "$RUN_SCHEMA" \
  --package "$PACKAGE_ID@$PACKAGE_VERSION" \
  --wiring "$PACKAGE_ID@$PACKAGE_VERSION::$WIRING_ID=$WIRING_VERSION" \
  --attachments apps/wamn_receiving/publication/attachments.json \
  --route-host "$ROUTE_HOST" \
  --package-manifest apps/wamn_receiving/wamn.json

wamn-ctl push-release-manifest \
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
The push reads that frozen snapshot and refuses conflicting artifact bytes.
`print-release-env` prints the executor environment entries and host flags without editing files.

Copy its output into the executor manifest and the selected complete host overlay.
Commit those configuration changes together.
The publisher and workloads can reach the registry through different hostnames.
Their artifact repository path must identify the same bytes.

## Activate the selected artifacts

Use the environment's explicit Kubernetes context and the matching application overlay.
For the retained Receiving example:

```bash
kubectl -n wamn-system apply -f deploy/platform/executor.yaml
kubectl -n wamn-system rollout status deployment/executor --timeout=300s
helm upgrade --install -n wamn-system wamn-host \
  oci://ghcr.io/wasmcloud/charts/runtime-operator --version 2.9.0 \
  -f deploy/platform/values-host-default.yaml \
  -f deploy/platform/values-host-receiving-pat.yaml
kubectl -n wamn-system rollout status deployment/hostgroup-default --timeout=150s
```

The host and executor load the manifest selected by their deployment configuration.
They refuse missing or inconsistent release artifacts and bindings.
A ready Pod does not establish successful authenticated application execution.
Exercise a meaningful operation and inspect its expected state or outcome before reporting deployment success.
Record the selected source, artifact identity, release identity, command exits, and actual observed outcome in one run directory.

`wamn-ctl promote` copies portable release facts only after the target's package and migration records match exactly.
It does not apply migrations to the target.
Current operation is manual, so a CI completion order is not a deployment-ordering mechanism.
The future automation requirements remain in [delivery](../plan/delivery.md).

## Identity target credentials

After replacing the identity service target Secret, restart the identity service.
Its database target connections are selected at process startup.
Use the environment's identity Deployment and require its readiness before issuing new sessions.

## Rollback and maintenance

Revert the workload configuration change and apply the previous release selection to roll back its artifact pointer.
A release digest selects exact bytes, but a mutable image tag does not.
A code rollback does not reverse database changes.
For schema changes, use a fresh target instead of implying an in-place upgrade path.

`wamn-ctl-ops` contains dump, restore, copy, run-history pruning, and event advisory commands.
Use its `--help` output for the selected action and required credentials.
These maintenance verbs do not authorize shared development targets or a new schema lifecycle.
