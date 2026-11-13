# WAMN deployment and upgrade simplification

This plan describes the shipped deployment and explicit SQL operations.
[Deployment](../operations/deployment.md) owns operational inputs, repair, and cleanup.
[Google Cloud](../operations/gcp.md#4-platform-deployment-boundary) owns cloud prerequisites and infrastructure operations.
Section 9 records the retained run evidence. The epic closed without a final GKE run, by owner ruling.

The command definitions live in [services/ctl](../../services/ctl/src/bin/wamn.rs).
[Application commands](../../services/ctl/src/application_verbs.rs), [schema commands](../../services/ctl/src/schema_verbs.rs), and [platform commands](../../services/ctl/src/platform_verbs.rs) call their control owners.
[Environment operations](../../crates/control/environment/src/environment/mod.rs) and [SQL operations](../../crates/control/environment/src/environment/operation.rs) implement the deployment boundary.
[Control routes](../../crates/execution/host/src/control_route/operation_job.rs) authorize and create the Jobs.

## 1. Primary decision: remove WAMN's deployment controller

WAMN selects artifacts, renders configuration, invokes Helm or the Kubernetes API, and reports running state.
Kubernetes and its operators manage resources after the command exits.
The numbered installer and installation completion ConfigMaps are deleted.
Keep one implementation for each operation. Prefer deleting an operation to caching its completion.

These decisions control deployment:

1. KCL supplies authored configuration. Helm and Kubernetes record requested resources. The wasmCloud operator manages Wasm workloads. CloudNativePG manages PostgreSQL clusters and databases.
2. WAMN owns component builds, package composition, immutable publication, artifact selection, initial SQL, and migrations.
3. Platform deployment, application deployment, and SQL execution are independent operations. Application deployment never initializes or advances SQL.
4. Compatibility differences produce notices. Missing inputs, authorization failures, SQL errors, and failed resource operations produce failures.
5. Kind and GCP use the same release, SQL, and Helm owners. OpenTofu owns the external GCP infrastructure.

### Deployment ownership

An application release is the unit of publication, deployment, and rollback.
It holds that application's packages, composed entry, routes, and component artifacts.
A composed entry is the application's executable component.
Each application owns its Helm release, host group, HTTP workload, Service, hostname, and route ConfigMaps.
The route ConfigMaps carry `wamn.route=true`. The edge reads that selector.

Platform and environment provisioning own shared database infrastructure, roles, credentials, and environment configuration.
Environment provisioning also owns the event stream, change data capture (CDC), and the shared materializer.
Application releases consume these resources without owning them.
Code deployment does not duplicate or restart shared event processing.
A database reset stops and reconnects every affected writer through these existing owners.

Deploy and rollback change only the target application's code selection and Helm release.
They run no schema changes, migrations, resets, seeds, or application data mutations.
They can read control state and prepare release caches, application release bindings, and application runtime grants.
These writes are repeatable and scoped to the target application.
Shared immutable cache entries can be added or reused.
Other applications keep their records, effective permissions, selections, routes, and workloads.

Deployment prepares the new release's bindings and grants alongside those of the running release.
Readiness precedes the bounded drain wait.
The policy's `drain_bound_seconds` sets the pod termination grace period. Observation allows another 15 seconds for pod removal.
Bindings stay when old pods remain, pinned work needs them, or observation fails.
A failed deployment retains the facts that running code needs.
Helm records applied operations. Control records do not select releases or track installation steps.

## 3. Small deployment commands, not a scripted runbook

`--env` takes an environment file path.
[Deployment](../operations/deployment.md#application-and-schema-operations) owns prerequisites and trusted administration inputs.

| Command | Responsibility |
|---|---|
| `wamn platform deploy deploy/kind/cluster.k --chart /prepared/platform` | Install a platform from prepared artifacts with zero applications. |
| `wamn platform deploy deploy/gcp/cluster.k --chart /prepared/platform` | Apply the same platform owners to the GKE target. OpenTofu owns external cloud resources. |
| `wamn platform status deploy/kind/cluster.k` | Read platform selections, SQL state, and observed readiness. |
| `wamn-ctl env apply environments/dev/environment.k --application sample_app` | Provision the environment, record configuration, and reapply the selected application code without application SQL. |
| `wamn deploy apps/sample_app --env environments/dev/environment.k` | Build and publish one application, then deploy its code. Provision the environment and apply SQL separately first. |
| `wamn deploy --release sha256:<digest> --env environments/dev/environment.k` | Deploy published application bytes without a source build. Resolve the application from its declared package membership. |
| `wamn schema publish apps/sample_app --env environments/dev/environment.k` | Build and publish without installing SQL or selecting code. |
| `wamn schema diff sample_app --env environments/dev/environment.k [--release <digest>]` | Inspect one application's selected published SQL without mutation. |
| `wamn schema apply sample_app --env environments/dev/environment.k [--release <digest>]` | Execute selected published SQL without selecting code. |
| `wamn schema diff apps/sample_app --env environments/dev/environment.k` | Inspect current source build output without publication or mutation. |
| `wamn schema apply apps/sample_app --env environments/dev/environment.k` | Publish source first, then execute those immutable SQL bytes. An adjacent `dev.json` selects direct local source execution. |
| `wamn schema reset --env environments/dev/environment.k` | Reset the whole recorded disposable environment database, even when fingerprints match. |
| `wamn schema adopt --env environments/dev/environment.k` | Assert schema sources without executing SQL or selecting code. |
| `wamn rollback sample_app --env environments/dev/environment.k [--release <digest>]` | Deploy the prior successful release or an explicit target. Never reverse SQL. |
| `wamn status --env environments/dev/environment.k` | Report each application's selected and observed releases, hostname, readiness, schema state, and notices. |

### Release selection

One non-secret ConfigMap holds the canonical environment document for deployed writers.
Its name is `e-<env-slug>-<coordinate-hash>` in namespace `hosts`.
Its `data.environment.json` holds the JSON document.
Checkout files are configuration inputs or exports. They do not select deployed releases.
Explicit environment configuration updates preserve recorded application release pairs.

Each application records `current`, the requested release, and `previous`, the prior successful release.
First installation has no previous release.
Under the application lock, deploy and rollback derive `previous` from successful Helm history.
Reapplying the same selection preserves both fields.
The `--env` file names the target. Stale input produces a warning with both values.

After preparation, deployment writes only the target application's release pair, then applies its Helm release.
The write supplies the ConfigMap's resourceVersion, the Kubernetes revision read with the document.
A conflict causes a reread and another merge of only that application's pair. Retries have a fixed bound.
Different applications can deploy concurrently under their separate locks.
An environment reset excludes all application deployments in that environment.

A failed deployment retains its requested `current`. A retry preserves the prior successful release.
Rollback requires `--release` when the previous selection is ambiguous.
Use `wamn rollback`, not `helm rollback`, to select application code.
`wamn-ctl env show <environment.k>` exports the canonical document as KCL.
Deploy and rollback never write the checkout file.

### Authorization

With `WAMN_CONTROL_URL`, the CLI uses authenticated control routes.
Set `WAMN_TOKEN` to the password session token.
Each request names `project` and `env`. The host supplies the organization and requires project administrator authority.
The server selects the image, command, namespace, actor, chart, and credential references.
The current trusted-worker boundary remains accepted. Section 8 defers tighter credential scopes and publication restrictions.

| Route | Input and behavior |
|---|---|
| `POST /wamn_control/application/deploy` | Required published `release` selects its declared application. No SQL runs. |
| `POST /wamn_control/application/rollback` | Required `application` and optional `release` select code. No SQL runs. |
| `POST /wamn_control/schema/diff` | Required `application` and optional `release` select one application's SQL for inspection. |
| `POST /wamn_control/schema/apply` | Required `application`, optional `release`, and optional `resolve` select explicit SQL execution. |
| `POST /wamn_control/schema/reset` | Reset the whole recorded disposable environment. |
| `POST /wamn_control/schema/adopt` | Assert schema sources for the optional application or the whole environment. |
| `GET /wamn_control/operation/status` | Read the authorized Job's state and bounded log tail. |

A start request returns a Job name. The CLI polls until the Job succeeds or fails.
States are `pending`, `running`, `succeeded`, and `failed`.
Jobs stop after 15 minutes. Kubernetes removes completed Jobs after one hour.
The log tail contains at most 100 lines and 32 KiB.
Each status read repeats authorization and compares the Job's target with the request.
There is no system SQL route or operation history database.

Deploy, rollback, and diff Jobs use the existing apply, registry, and NATS credentials.
Apply, reset, and adopt also receive the CNPG administrator credential. These SQL Jobs are privileged.
Code deploy and rollback Jobs receive no administrator credential.
Environment provisioning installs separate code and SQL ServiceAccounts and namespace Roles.

Without `WAMN_CONTROL_URL`, the CLI uses direct trusted administration.
Status, provisioning, and platform SQL commands remain direct.
Source publication stays local and submits only the resulting release digest.
A refused remote request never falls back to direct administration.
Publication accepts no caller-supplied publisher identity and records no human publisher.
Authenticated deployment stamps the applied release with the request actor. The direct path uses the provisioning principal.

## 4. Schema modes: `reset` and `migrate`

Schema mode belongs to the environment and does not depend on package SemVer.
Rust and KCL accept `reset` and `migrate`.
An omitted mode resolves to `reset` only when `registry.project_envs.disposable` is true.
An absent record or a false marker resolves to `migrate`.
An environment name or explicit mode grants no authority to discard data.
`wamn dev up` records its environment as disposable.

```kcl
schema = { mode = "reset" }  # or "migrate"
```

| Operation | `reset` | `migrate` |
|---|---|---|
| Code deployment or rollback | Preserve installed SQL and data | Preserve installed SQL and data |
| Explicit schema apply | Replace the disposable database when physical SQL changes | Initialize absent packages and execute pending migrations |
| Mode edit | Run no SQL and change no application selection | Run no SQL and change no application selection |
| Explicit adoption | Assert sources without SQL | Assert sources without SQL |

### The one reset rule

Schema apply resets automatically only when all these conditions hold:

- The selected physical SQL differs from installed state.
- The mode is `reset`.
- The environment is recorded as disposable.
- WAMN can reconstruct every retained package schema and the installed project substrate.

This rule applies equally to source and published inputs to schema apply.
Deployed source apply publishes first, then executes those immutable bytes.
Deploy, reapply, and rollback never infer a reset or execute migrations.
Explicit `schema reset` uses the same authorization and reconstruction rules even when fingerprints match.

A physical fingerprint hashes the complete authored and generated SQL for installation.
The environment fingerprint includes each installed package's identity and physical SQL hash, plus the project baseline.
The decision substitutes the target application's candidate SQL for its installed SQL.
Code versions, release digests, web files, and ordinary queries do not trigger reset.
Missing inputs, omitted installed applications, and conflicting relation definitions stop reset before the database changes.

### Reset implementation

Reset replaces the whole project-environment database under its existing name.
It does not allocate another database name or retain the old database.
Every application in that database loses its data, including stopped applications with retained schemas.
The command states the affected applications, downtime, and irreversible data loss before execution.
Application rows, manually created workflows, queued runs, and obsolete queued events are lost.
An application reset never resets `wamn_system` or another environment's database.

The deployed reset follows these steps:

1. Acquire the existing exclusive environment lifecycle lock. Wait for active application deployments to release their shared locks.
2. Resolve complete SQL, permissions, and artifacts for every retained schema before stopping writers. Keep unchanged applications' recorded schema sources.
3. Resolve the project substrate from the installed platform SQL image. Require matching checksums for every recorded applied file and resolve uncertain files first.
4. Pin the validated inputs and a new initialization identity in `registry.project_envs.pending_database`, outside the database that reset replaces.
5. Stop affected application workloads and event writers through their existing owners.
6. Set the existing CNPG Database to `ensure: absent`. Wait for deletion. Then set `ensure: present` under the same name and wait for creation.
7. Restore the pinned SQL and permissions through the SQL Job. Write the pinned initialization identity in the recreated database.
8. Rebuild bindings, caches, and CDC through their existing owners. Discard only this environment's obsolete event backlog while writers remain stopped.
9. Clear the pending record. Then resume workloads with unchanged code selections.

Reset restores initialization SQL and the recorded applied project files through the shared executor.
Pending files stay pending. Missing files or changed checksums refuse each attempt before database replacement.
The reset keeps every application's release selection. It runs authored seeds only when the initialization recipe includes them.
Local `wamn dev` uses its existing template reset and direct database connection with the same schema planner and package SQL executor.

### Reset recovery

Recovery uses pinned inputs, initialization identity, and observed resource and SQL state.
There is no per-step progress store, generic retry engine, or background deployment controller.
Repeat `wamn schema reset` or the interrupted schema apply after repairing its reported failure.

If preparation fails before the pending record, the database and workloads remain unchanged.
If writer shutdown fails, the old database remains, but stopped applications do not serve requests.
If deletion starts, old data is lost. A retry repeats recreation until the database carries the pinned initialization identity.
Once that identity exists, retry resumes restoration without deleting restored data.
Committed SQL records prevent repeated execution.
A retry does not create another candidate or rotate any application's previous release.

Event disposal runs only while the pending record exists and writers remain stopped.
New events follow completion, so a retry cannot discard newly produced events.
If workload resume fails after the pending record clears, the reset is complete.
Reapply the named application's unchanged selection without another reset.
[Deployment cleanup](../operations/deployment.md#cleanup) owns disposable platform rebuild commands and their data-loss scope.

### Switching modes

Mode edits execute no SQL, write no migration history, and change no code selections.
Matching package baselines and migration records supply the starting point for a later schema operation.
A baseline mismatch requires explicit adoption, manual repair, or an authorized reset.
Changing to `reset` permits replacement only at an explicit schema operation on a recorded disposable target.
Repeated transitions preserve the same rules. Version 1.0 does not change mode.

`wamn schema adopt` records an explicit user assertion in the existing package records.
Matching migration rows keep their checksums and outcomes.
New or changed asserted files receive `outcome = 'adopted'` and no `applied_at` timestamp.
Adoption executes no SQL and makes no claim about the live schema.
Later apply skips adopted files and executes the remaining continuation.
An uncertain `started` file requires `--resolve` before adoption.

### SQL execution

Package migrations and checksums belong to immutable package artifacts.
An application name selects its configured current release. `--release` selects different published SQL without changing code selection.
A source diff reads current build output without mutation.
Deployed source apply publishes first. An adjacent `dev.json` keeps direct local source execution.
Deployed operations run the SQL executor in a Kubernetes Job without a laptop port-forward.
A successful explicit apply also configures CDC through its existing owner.

Each ordinary migration file commits with its completion record.
A statement that cannot run in a transaction needs its own file.
The first line must be exactly `-- wamn:no-transaction`. The file must contain exactly one standalone statement.
The marker belongs to the checksummed bytes.

For a marked file, the executor commits `started` before execution.
It runs the statement outside a transaction and records `applied` only after success.
An unresolved `started` file does not count as installed SQL. A later apply refuses and names that file.
Resolve it after database inspection:

```bash
wamn schema apply sample_app --env environments/dev/environment.k --resolve sample_pkg:migrations/0002_index.sql=applied
wamn schema apply sample_app --env environments/dev/environment.k --resolve sample_pkg:migrations/0002_index.sql=retry
```

`applied` asserts that the statement completed and skips execution.
`retry` permits the same bytes after manual cleanup, such as removal of an invalid concurrent index.
A changed checksum refuses resolution. WAMN performs no automatic repair.
Execution and resolution share a session-level PostgreSQL advisory lock across transaction boundaries.
The lock prevents resolution while a worker continues after the CLI disconnects.
Marked files have no atomicity or exactly-once guarantee.

`catalog.packages` keeps each package's baseline, current fingerprint, version, and schema source.
`catalog.package_migrations` keeps migration checksums and outcomes across code versions.
Explicit data migrations admit `UPDATE`, `INSERT`, and `DELETE` on schema-qualified tables that the package owns.
Existing refusals of role switches, grants, and role operations remain.
Publication and apply share migration validation. Target database checks stay at apply.

## 5. Concrete deployment and upgrade use cases

### 5.1 Fresh platform installation, with zero applications

Prepare the chart separately, then deploy it:

```bash
wamn platform deploy deploy/kind/cluster.k --chart /prepared/platform
wamn platform status deploy/kind/cluster.k
```

Kind uses Docker, kind, a local OCI registry, and the pinned tools and images.
It needs no GCP project or cloud credentials. The platform command creates or reuses the local cluster.
It applies operator prerequisites before their custom resources, then installs platform resources and initializes the empty system database.
The system initializer commits the control store and baseline together, then runs embedded migrations.
Initialization also establishes the organization, identity, and control credentials.
It installs no application SQL or release and compiles no application or platform UI source.
Kind selects no platform UI. GCP uses a separately uploaded immutable UI artifact.

Once the org control release exists, reapply runs no initialization Job and writes no initialization Secret.
It preserves signing keys and credentials and executes no pending SQL.
After failure, rerun the same deployment and inspect the reported resource and SQL state.
Incomplete SQL initialization reports a repair action and never resets the system database automatically.

GCP uses the same prepared chart and `wamn platform deploy deploy/gcp/cluster.k --chart /prepared/platform`.
OpenTofu creates the cluster and node pools before platform deployment.
The edge Service creates the network endpoint group. A later full OpenTofu apply creates the load balancer and certificate.
The DNS owner publishes the route records and certificate authorization record.
[Google Cloud](../operations/gcp.md#4-platform-deployment-boundary) owns the complete sequence.

### 5.2 Fresh application installation, with explicit initial SQL

The environment document declares the application and its package membership.
For initial provisioning, set its `current` selection to `"none"`.
Provision the environment and initialize SQL before code deployment:

```bash
wamn-ctl env apply environments/dev/environment.k --application sample_app
wamn schema apply apps/sample_app --env environments/dev/environment.k
wamn deploy apps/sample_app --env environments/dev/environment.k
```

Environment provisioning creates the database, shared roles, credentials, and canonical document.
Schema apply publishes source and installs that immutable SQL without selecting application code.
Deploy builds and publishes source or consumes `--release`, then applies only that application's Helm release.
Publication needs no destination application database. Source builds use separate disposable build databases.

Adding an application requires its explicit SQL operation.
In `migrate`, absent package initialization preserves existing rows.
In authorized `reset`, changed physical SQL follows the whole-database rule in section 4.
A later rollout failure leaves committed SQL in place. Retry the code selection without repeating SQL.

### 5.3 Platform upgrades: behavior, WAMN SQL, and infrastructure

#### 5.3.1 Behavior-only or configuration-only change

Select one existing service or host group from a prepared chart:

```bash
wamn platform deploy deploy/kind/cluster.k --chart /prepared/platform --only control
wamn platform deploy deploy/kind/cluster.k --chart /prepared/platform --only identity
wamn platform deploy deploy/kind/cluster.k --chart /prepared/platform --only host:alpha/alpha
```

`--only` accepts `control`, `identity`, or `host:<release>/<group>`.
A host target names its owning Helm release in `hosts` and its group.
Every targeted update requires `--chart` and an existing owner release.
Missing owners, ambiguous groups, and unavailable inputs refuse before upgrade.
Targeted updates run no bootstrap, migration, application build, or credential rotation.
They change only the selected release and report that target after success or failure.

Control and identity record `platform.chart` in their own Helm release values.
An existing service without that value inherits the default from `platform/wamn`.
Service configuration comes from the selected chart and cluster document.
Control retains only matching named host image overrides. Identity carries over no installed configuration.
Credential Secrets and signing keys remain unchanged.
Only identity inspects active signing state through the existing PostgreSQL primary.
A five-second SQL timeout and a ten-second complete inspection limit bound that read.
Missing or unreadable signing state refuses before resource changes and never triggers initialization.

A host update uses the installed owner chart and values. The incoming chart supplies only the immutable host image.
The image override persists as `runtime-operator.runtime.hostGroups[].image` in the owning release.
Application deploy and shared-host reapply retain matching named image overrides.
They leave other installed configuration behind and retain the platform default for groups without overrides.
The host update uses existing application or exclusive shared-host environment locks and recovers pending operations before reading upgrade values.

Untargeted platform reapply retains the default chart, service selections, and explicit host image overrides.
It needs no `--chart`. A supplied chart produces a warning and does not replace recorded selections.
The command resolves recorded charts before any resource change. An unavailable recorded chart refuses.
Restore that path or select a new service chart through an explicit targeted update.
Targeted updates never replace the global default.

Status separates recorded selections from workload readiness, resource images, and running image IDs.
It reads these facts without requiring a healthy control or identity service.
Host readiness includes its Deployment, active pods, and assigned WorkloadDeployments.
The Deployment must observe its current generation and reach its desired updated, ready, and available counts.
Enough active pods and all assigned WorkloadDeployments must be ready.
Draining pods remain in diagnostics but do not block success when those conditions hold.
Unreadable assigned workloads produce unknown readiness. Other host groups do not affect the target.

Selecting an earlier compatible chart uses the same targeted update operation.
Retry performs no installation or SQL reversal. Application release digests and data remain unchanged.

#### 5.3.2 WAMN system or project schema change

Platform SQL commands select the prepared chart explicitly:

```bash
wamn platform schema diff --scope system --chart /prepared/platform
wamn platform schema apply --scope system --chart /prepared/platform
wamn platform schema diff --scope project --env environments/dev/environment.k --chart /prepared/platform
wamn platform schema apply --scope project --env environments/dev/environment.k --chart /prepared/platform
```

Both commands run the chart's immutable `ctl` image and print its digest.
That image supplies the SQL. Project scope requires `--env`. System scope refuses it.
Both scopes use bounded Jobs in `platform`.
Project scope resolves the database and cluster from the system registry and holds the environment's exclusive lifecycle lock.
It uses the existing CNPG administrator and apply credentials.

| Scope | Frozen initialization SQL | Append-only files | Records |
|---|---|---|---|
| System | `CONTROL_BOOTSTRAP_SQL` | `deploy/sql/migrations/system/NNNN_name.sql` | `registry.schema_baseline`, `registry.schema_migration` |
| Project | `CATALOG_SCHEMA_SQL`, then `APP_SCHEMA_SQL` | `deploy/sql/migrations/project/NNNN_name.sql` | `platform.schema_baseline`, `platform.schema_migration` |

The build embeds each migration directory from its listing.
A file ID is its path below `deploy/sql/`. Its four-digit prefix orders execution. Its checksum hashes the exact bytes.
Initialization identity contains the frozen SQL digest and `installed_at`.
Migration history records ordinal, path, checksum, outcome, and completion time.
Applied history is immutable. A `started` outcome can advance to `applied`.
Installed state derives from initialization and history. There is no third state record or code selection dependency.

Fresh installation runs initialization and every embedded migration through the shared executor.
Repeated provisioning and platform reapply execute no pending migrations.
Resume an interrupted installation with the explicit platform schema apply command and the same chart.
A different initialization digest refuses. A new baseline belongs only to an empty database.
Application schema mode never authorizes system database reset.

For an existing matching baseline without migration history, apply creates the empty history table before executing files.
It runs no initialization SQL and claims no completed file through adoption.
Diff reports this adoption without mutation.
Transactional files commit with their history rows. Retry skips committed files and refuses changed checksums.
Both platform scopes and package SQL use one shared executor for ordered prefixes, gaps, drift, locks, and nontransactional outcomes.
Platform scopes use their own record tables without package authorization or package keys.

The [SQL execution](#sql-execution) rules also apply to platform files.
A marked file has the same `started` uncertainty. Resolution uses a path without the package prefix:

```bash
wamn platform schema apply --scope system --chart /prepared/platform --resolve migrations/system/0001_example.sql=applied
wamn platform schema apply --scope project --env environments/dev/environment.k --chart /prepared/platform --resolve migrations/project/0001_example.sql=retry
```

System scope holds its own session-level advisory lock. Project scope uses the environment's schema exclusion.
Apply changes only SQL objects and records in the selected scope.
It runs no environment provisioning, application activation, service update, credential rotation, or SQL reversal.
No command migrates every environment.
Developers choose the order of SQL and compatible service code updates.
A later service failure leaves committed SQL in place.

Status reports initialization identity, installed files, pending image files, and uncertain files separately from executable selections.
Diff warns when an image lacks recorded files. Apply refuses unavailable recorded files or changed checksums.
Authorized reset reconstructs the recorded applied project files and leaves pending files pending.

#### 5.3.3 PostgreSQL and operator upgrades

CloudNativePG owns PostgreSQL engine changes.
The existing Helm and infrastructure owners manage operators and custom resource definitions.
These operations imply no application migration. Broader platform work remains outside this plan's closure.

### 5.4 Application schema change, including rolling SQL and code changes

Apply published SQL independently of running code:

```bash
wamn schema diff sample_app --env environments/dev/environment.k
wamn schema apply sample_app --env environments/dev/environment.k --release sha256:<digest>
```

In `migrate`, apply executes pending files in dependency order and preserves rows.
Failed SQL identifies committed files and stops the operation. It attempts no application rollback.
A changed fingerprint alone does not authorize data loss.
In `reset`, changed physical SQL replaces the whole recorded disposable database as section 4 describes.
Both operations preserve code selections.

For a field rename, developers can apply additive SQL, deploy transitional code, backfill data, remove old SQL, and deploy final code.
WAMN does not synchronize columns or choose when old workflows no longer need a field.
Each SQL and code operation uses its separate owner.

### 5.5 Application code upgrade, hotfix and rollback

Deploy source or a published release after the required SQL exists:

```bash
wamn deploy --release sha256:<digest> --env environments/dev/environment.k
wamn rollback sample_app --env environments/dev/environment.k
```

Only the target application's code selection, Helm resources, and necessary control facts change.
SQL, other applications' permissions, Helm revisions, pod identities, and data stay unchanged.
Schema differences produce notices and do not turn deploy into SQL execution.
A failed build or publication changes no deployed selection.
After a failed Helm apply, the ConfigMap retains the requested current release and prior successful release.
Retry preserves that previous release. Rollback requires an explicit digest when history is ambiguous.

A stale checkout cannot replace the prior successful release with its own stale value.
For example, after A1 succeeds, a later deployment of A2 records A1 as previous even if the input file still selects A0.
Rollback of that application selects A1 and leaves another application's release unchanged.

### 5.6 Local edit loop is not deployed kind

`wamn dev` watches selected source, regenerates changed inputs, and keeps the last running application after a candidate build failure.
The watcher initializes absent packages but does not advance installed SQL in `migrate`.
A developer applies SQL explicitly. Disposable local schema changes use the shared reset decision.
Code and ordinary query edits preserve rows.
A passing local loop does not establish deployed Kubernetes behavior.

### 5.7 Mode changes are independent of schema execution

Changing `schema.mode` alone performs no SQL and changes no application release.
Matching baselines supply migration starting points without replay.
An explicit adoption asserts records without claiming execution.
A later reset needs the recorded disposable designation and complete retained SQL.
Repeated transitions retain these rules and never reset the system database.

### 5.8 Cross-cutting recovery and status

Status separates requested code, observed running artifacts, readiness, shared resources, and installed SQL.
Unreadable state is unknown with its cause.
A failed rollout leaves committed SQL in place.
After an interrupted Helm operation, inspect the application's resources and reapply its persisted selection.
After interrupted SQL, inspect completion records and resolve uncertain files explicitly.
Do not use installer completion markers as evidence.

## 6. Warnings and operation results

Each notice names its application or database, evidence, and next action.
Schema mismatch and uncertain compatibility produce notices. Actual execution and authorization failures remain errors.
Status and analysis report initialization digest differences and changed recorded checksums as warnings.
Mutating SQL operations refuse those differences.
After a failed apply, report current, previous, and observed running releases separately.
Output must expose no secrets.

## 8. Explicitly deferred

Each bead below remains open and deferred. None blocks the accepted trusted-worker boundary.
Use the target's configured artifact repositories.

- `wamn-b26q0`: Deferred credential-scope restrictions. The owner accepted the current trusted-worker boundary on 2026-11-10.
- `wamn-pbh27`: Deferred publication restrictions under the same owner ruling.
- `wamn-yen7r`: Deferred product project delete operation.
- `wamn-aasc7`: Deferred Acme deployment in `wamn-dev` for the `acme_record_receipt` benchmark.
- `wamn-nm23n`: A package push of new bytes under an unchanged version needs a registry tag deleted.
- `wamn-pzddj`: Measure the Kubernetes calls Helm makes for env apply and narrow the release Roles.
- `wamn-2oqx6`: Replace the platform build source-based reuse analysis with explicit artifact selection.
- `wamn-31x2l`: Identity cannot be reinitialized on an installed platform.
- `wamn-d3ko5`: An image copied by input hash keeps its old base and apt layers.
- `wamn-t66vo`: Deferred `native_lifecycle_live` failure.

New stop and delete features, automatic rollback, and broader platform work remain outside this closure.
This plan adds no deployment operator, GitOps requirement, compatibility solver, automatic SQL reversal, or system reset policy.

## 9. Measured evidence

The retained evidence lives in `/var/tmp/wamn-dep7/gke`.
These logs describe the earlier GKE attempts on 2026-11-11, before the fixes below.
They do not establish final acceptance or a before-and-after performance improvement.

### Build and infrastructure

| Log | Recorded measurement or result |
|---|---|
| `build.log` | UI build: 1.693s. Input hashes: 1.694s. Input lookups: 1.560s. Cloud submission: 2638.115s. |
| `build.log` | Copy ctl: 2.249s. Copy edge-web: 2.151s. Copy host: 2.162s. Copy identity: 2.082s. |
| `build.log` | HTTP reuse check: 1.738s. Materializer reuse check: 1.484s. Both layers differ from 0.1.141. |
| `build.log` | Registry reads: 8.938s. Stamp: 0.011s. Output: the prepared `chart` directory. |
| `apply-pools.log` | Cluster creation: 7m49s. Node pool creation: main 1m22s, bench 1m33s, pg 1m42s. Apply adds 4 resources. |
| `apply-full.log` | Address creation: 12s. Certificate map: 11s. Certificate: 11s. Backend service: 32s. Forwarding rule: 22s. Apply adds 10 resources. |

The resource durations overlap. They are not a total installation time.
`build.log` marks ctl, edge-web, host, and identity inputs as `copy`.
It records no unrelated-artifact rebuild count.
`buildbins.sh` builds the local `wamn` and `wamn-ctl` binaries.
`buildinputs.sh` builds the Wasm HTTP route input and the local PostgreSQL test helper.
`buildtest.sh` and `fixhost.sh` prepare the acceptance executable without running it.
These scripts record rebuild commands without elapsed results.
`fix-resend.py` edits identity mail setup so that deployment prepares it before Helm waits for identity readiness.
`test10.log` records a 16.90s test compilation after environment, development, and harness crates compile.

### GKE attempts

Each `test*.log` identifies head `afc8c7cfa5d78bfe1d31a18a03fd718fa68b0e2f`.
The table keeps the separately printed platform command, test body, and whole invocation durations.
Every listed acceptance attempt fails. Platform duration alone does not prove readiness.

| Log | Platform command | Test body | Whole invocation | Reported failure |
|---|---|---|---|---|
| `test.log` | 81.59s | 104.51s | 106s | Owner login returns 401. |
| `test2.log` | 79.61s | 104.56s | 105s | Project creation receives an HTTP/2 stream error. |
| `test3.log` | 58.47s | 59.80s | 61s | Platform deployment fails. |
| `test4.log` | 76.59s | 100.22s | 101s | Project creation returns 405. |
| `test5.log` | 87.36s | 159.66s | 161s | Alpha publication fails. |
| `test6.log` | 88.79s | 111.07s | 112s | The journey project already exists. |
| `test7.log` | 86.00s | 148.28s | 150s | Route layer bytes differ from the pinned digest. |
| `test8.log` | 83.81s | 421.21s | 422s | Environment provisioning fails. |
| `test9.log` | 82.77s | 383.97s | 385s | Environment provisioning fails. |
| `test10.log` | 86.17s | 291.90s | 309s | Environment provisioning reports an app ACL role mismatch. |

`test8.log`, `test9.log`, and `test10.log` time failed provisioning at 93.47s, 90.10s, and 24.15s.
Their environment deletion times are 71.05s, 48.68s, and 16.68s.
Their PAT revocation times are 0.58s, 0.56s, and 0.55s.
These are failed-run setup and cleanup observations, not successful application installation times.

`deploy-1.log` and `deploy-2.log` report a refused first system database connection.
`deploy-3.log` reports an identity readiness timeout.
The retained `deploy.log` reports platform readiness with version 0.1.143.
These deployment logs contain no complete command duration.
The retained evidence contains no successful first application install, code edit-to-ready, SQL-only update, or interrupted-operation recovery measurement.

### Operator work and provisioning fixes

The retained scripts record operator work outside the deploy command:

- `plan1.sh` and `apply1.sh` prepare and apply the targeted cluster and node pool plan.
- `dockerauth.sh` prepares private registry credentials. `build.sh` submits the cloud platform build.
- `deploy.sh` installs the pinned KCL compiler and invokes platform deployment. `watch.sh` preserves earlier deployment output and watches initialization.
- `owner-grants.sh` supplies registry reader grants for control, application, code, and SQL workers. It also supplies bucket grants for edge and upload.
- `plan2.sh` and `apply2.sh` prepare and apply the full load balancer infrastructure plan.
- `owner-dns.sh` updates the control route and certificate authorization DNS records. `certwait.sh` waits for the managed certificate.
- `upload.sh` uploads the chart-selected platform UI artifact separately.
- `scaleup.sh` resizes the main and PostgreSQL pools. The login and HTTP probe scripts inspect identity and public control access.
- `runtest.sh` supplies the dedicated cloud login, kubeconfig, owner input, chart, compiler, and pinned HTTP component before the acceptance command.

The scripts do not supply a measured total of manual steps or their elapsed operator time.
Credential values remain outside this document.

| Finding | Reconciliation |
|---|---|
| `wamn-8h4h6.8` | `a610c128f` retries only a refused first system connection, 30 times at 1s. Other connection errors fail directly. |
| `wamn-8h4h6.9` | `90894861d` provisions the shared host from the installed platform runtime, including provider registry inputs. |
| `wamn-8h4h6.10` | The owner closed this finding as no change. Evidence: `wamn-8h4h6.10`. |
| `wamn-8h4h6.11` and `wamn-8h4h6.13` | `21566fc6d` retains GKE logs on failure and reduces acceptance to current product behavior. |
| `wamn-8h4h6.12` | `c1e646133` configures CDC after successful explicit schema apply. Code deployment stays independent of SQL. |

`c8093b985` deletes qualify-release, runner and acceptance test image values, qualification files, and the all-application schema diff and apply loop.
The Dockerfile conditions target remains for the condition suite.
Later fixes retry one Cloud Build image push after an authentication deadline (`1d0861f5c`, `wamn-33qe5`).
Environment delete refuses stranded runs before uninstalling resources (`d273a8e78`, `wamn-e70ys`).
The wash-runtime fork is removed at `45e2227f2`.
`cfdb1badd` selects wasmCloud v2.12.1, removes the declared-world linker, and defaults socket egress to Enforce.
The earlier run logs do not verify these later changes.

Final GKE verification (wamn-8h4h6.4): not run. On 2026-11-12 the owner closed the epic without it.
The kind stage passed at `b94833fd3`: registry-image-kind, control_operations_kind, platform_apply_kind, and platform_upgrade_kind.

Completion point: epic/dep7 merged to main on 2026-11-12.
