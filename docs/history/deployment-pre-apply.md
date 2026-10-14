# Deployment before `env apply`

This file is the pre-apply manual path of [deployment](../operations/deployment.md), which `env apply` replaced (`wamn-g18lv`).
It is kept as history and is not run.

## Deployment ordering

Then run the verbs in this order: `provision-org`, `provision-project-env`, `provision-identity-issuer`, `reconcile-run-plane`, `push-package`, `apply-package`, `reconcile-package-data-access`.

The control host of an org holds the administration login of each environment of the org ([host-run routes](../architecture/execution.md#host-run-routes)).
`provision-org --emit-control-administration-secret PATH` writes the empty Secret `wamn-control-administration-<org>`. Apply it before the control host starts, because the host mounts it.
Each administration prepare of `provision-project-env` needs `--emit-control-administration-patch PATH`, and each rotation writes the patch again.
Apply the patch to that Secret with `kubectl patch secret wamn-control-administration-<org> --type merge --patch-file PATH`.
`env delete <org/project/env>` removes the key itself.

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

Apply the selected package migrations to a fresh target with `wamn-ctl apply-package --package-artifact <package_id>-<version>` and the registry flags of `push-package`.
apply-package takes only the package artifact, never a checkout directory, and records its artifact digest in `catalog.packages`.
A coordinate that `wamn dev` applied from a working tree records no artifact digest, and every release verb refuses it.
apply-package owns the grants of the `wamn_audit_retention` role on history tables.
It grants and revokes them in the transaction that writes the log triggers.
Reconcile generated data privileges with `wamn-ctl reconcile-package-data-access --package-artifact <package_id>-<version>`, one for each applied package.
If the registry certificate chains to a private CA, pass that CA with `--oci-ca-path` or `WASH_OCI_CA_PATHS`.
`author-wiring` and the management gate read the component facts of the package from the descriptors of its artifact, the way `publish-release` reads them.
`author-wiring` takes the same registry arguments as `publish-release`: `--artifact-base`, `--registry-auth-file`, `--insecure-registry` and `--oci-ca-path`.
The scenario worker reads the artifact with a pull-only registry credential scoped to the package repositories.
It mounts that credential from the Secret `wamn-registry-package-pull` the same way the host mounts `wamn-registry-pull` ([`scenario-worker.yaml`](../../deploy/platform/scenario-worker.yaml), [`registry-credentials.example.yaml`](../../deploy/platform/registry-credentials.example.yaml)).
It pulls each package by tag from the registry only; no database holds a package row.
Submit their wiring with `wamn-ctl author-wiring`, and bind declared connection aliases with `wamn-ctl bind-connection`.
Repeating `bind-connection` with identical inputs changes no records.
Changed definitions or credential references create an immutable generation and activate it in the same transaction.
The command prints the previous and new generation.
Invalid configuration or a concurrent change leaves the current selection untouched.
The command reports “configuration valid; connectivity and credentials not tested.”
It does not contact the external service or read its credentials.

Schedule record history retention for each tenant database that has a relation with a `"P<n>D"` retention.
Prepare an audit retention credential generation with `wamn-ctl provision-project-env --prepare-audit-retention-generation`.
Apply its Secret and a CronJob like [`audit-retention.example.yaml`](../../deploy/platform/audit-retention.example.yaml).
The CronJob runs `wamn-ctl-ops prune-record-history --tenant "$TENANT"` once a day.

## Publish and select a release

Set the host variables `WAMN_ORG`, `WAMN_PROJECT` and `WAMN_DRAIN_BOUND_SECONDS` on the host group; a host without a drain bound refuses to start.
The operator chart sets `WASMCLOUD_HOST_ENVIRONMENT` to the pod namespace, and Kubernetes refuses a second variable of one name.
Add `--environment=<environment>` to the host group's `extraArgs`; the host keeps the last `--environment`.

The host release settings are `--release-artifact-base=<release repository>` and `--release-manifest-digest=<release digest>` in `hostGroups[].extraArgs`.

Write them into the selected complete host overlay.
Commit those configuration changes together.
The publisher and workloads can reach the registry through different hostnames.
Their artifact repository path must identify the same bytes.

## Package upgrades

### Upgrade order

Set the paths and scope below for the chosen environment. `UPGRADE_RESULT` must name a new file.
Push the candidate with `push-package` first. For WMS, `CANDIDATE_PACKAGE` is the artifact `wamn_wms-2.1.0`.
`CANDIDATE_ROOT` is its package root, which `reconcile-replica-identity` reads.
`REGISTRY_FLAGS` holds the registry flags of `push-package`: `--artifact-base` and `--registry-auth-file`.
Repeat `--presented-package` for every installed lineage.

```bash
wamn-ctl qualify-upgrade \
  --database-url "$OWNER_URL" --tenant "$TENANT" --environment "$ENVIRONMENT" \
  --predecessor-release "$PREDECESSOR_RELEASE" $REGISTRY_FLAGS \
  --package "$CANDIDATE_PACKAGE" --presented-package "$CANDIDATE_PACKAGE" \
  --result "$UPGRADE_RESULT"

wamn-ctl apply-package \
  --database-url "$OWNER_URL" --tenant "$TENANT" $REGISTRY_FLAGS \
  --package-artifact "$CANDIDATE_PACKAGE" --upgrade-qualification "$UPGRADE_RESULT"

wamn-ctl reconcile-package-data-access \
  --database-url "$OWNER_URL" --tenant "$TENANT" $REGISTRY_FLAGS \
  --package-artifact "$CANDIDATE_PACKAGE"

wamn-ctl reconcile-replica-identity \
  --admin-database-url "$OWNER_URL" --package "$CANDIDATE_ROOT"
```

An expand stage with `whole_row_grants` applies nullable column additions and derived grants in one transaction.
The same transaction records the accepted qualification after postconditions and serving SQL planning succeed.
If the qualified root set contains other packages, pass every root with `--presented-package`.
A failed condition or statement leaves the schema, grants, and qualification records unchanged.

If a backfill stops, correct the reported cause before you repeat the same `apply-package` command.
Use the original package bytes and qualification file.
Committed batches and their cursor remain recorded while the predecessor version stays installed.
The final batch, postconditions, new package version, and accepted evidence commit together.
To abandon an incomplete backfill, repeat its `apply-package` command with `--abandon-stage` and the original `--upgrade-qualification` file.
This command records abandonment and prints the retained cursor, completed batch count, and unchanged installed version.
It does not reverse committed batches. The abandoned stage cannot resume.
The command refuses a completed stage, changed package bytes, changed qualification bytes, or a changed installed predecessor.
The current backfill grammar and its limits are in [application declarations](../architecture/data-access.md#application-declarations).

Repeat reconciliation's `--package-artifact` arguments for the same complete artifact set that qualification used.
Run replica-identity reconciliation for each package whose registrations require old-row fields.
Continue through the existing [release commands](../operations/delivery.md) in this order:

```text
publish-release → qualify-release → wamn-ctl env apply
→ optional wamn web upload
```

Before mutation, application rechecks the installed predecessor packages, package bytes, and privileges.
It stores accepted canonical evidence and its digest in `catalog.package_upgrade_qualifications` within the package application transaction.
An exact retry changes nothing. Conflicting evidence for the same coordinate refuses.
Data-access reconciliation consumes the persisted evidence and refuses a changed root set or derived privilege state.
Reconciliation requires a qualification that matches the complete current roots, candidate transition, and derived privileges.
It does not select by timestamp. Earlier qualifications remain immutable history and do not independently constrain later package sets.
Each subsequent qualification starts from the complete installed set and proves the complete successor set.

Deployment is `wamn-ctl env apply` of a document that names the release digest; `upgrade-environment` runs the same apply.
Wait for readiness and prove a meaningful authenticated application operation before reporting success.
Upload web files only after `apply` installed their release.

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
The same application schema rule applies to the base and overlays.

Set `BASE_PREDECESSOR` and `OVERLAY_PREDECESSOR` to the installed package roots.
Set `BASE_SUCCESSOR` and `OVERLAY_SUCCESSOR` to their prepared direct successor roots.
Set `BASE_COMPONENT` to the exact built successor base artifact named by each overlay pin.
Use the owner connection from [upgrade prerequisites](../operations/deployment.md#upgrade-prerequisites).
Repeat `--predecessor-package` for the complete original root set, including unchanged packages.
Repeat `--presented-package` for the complete successor root set in both commands.

```bash
wamn-ctl qualify-upgrade \
  --database-url "$OWNER_URL" --tenant "$TENANT" --environment "$ENVIRONMENT" \
  --predecessor-release "$PREDECESSOR_RELEASE" \
  --package "$BASE_SUCCESSOR" --base-component "$BASE_COMPONENT" \
  --predecessor-package "$BASE_PREDECESSOR" --predecessor-package "$OVERLAY_PREDECESSOR" \
  --presented-package "$BASE_SUCCESSOR" --presented-package "$OVERLAY_SUCCESSOR" \
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
