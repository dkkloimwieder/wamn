# WAMN — Deployment and Upgrade Simplification

Status: Epics 1 and 2 are complete. Epic 3 is in progress on `worktree-dep3`. Section 2 lists what runs today. Every other command form is a proposal.

Decisions of 2026-11-07 control this revision: selection, environment-wide reset, SQL artifacts and fingerprints, and authorization. Section 2 records the code that each decision replaces.

## 1. Primary decision: remove WAMN's deployment controller

**The first implementation priority is architectural deletion, not improving the existing installer.** WAMN must stop coordinating long-lived Kubernetes resources through its own numbered install process, completion records, and special retry rules. The target is a thin command/API that selects artifacts, renders configuration, invokes Helm or the Kubernetes API, and reports the running state. Kubernetes and its installed operators then manage the resources.

This is a deliberate **delete-first replacement**. Once the smallest alternative can perform an operation, remove the corresponding old execution path. Do not retain parallel implementations until every previous special case works. If a needed behavior is lost, add it back to the narrow owner that needs it, with a targeted test. Removing excess machinery is preferable to preserving speculative recovery and optimization code.

The following decisions control the entire proposal:

1. **Long-lived workloads and infrastructure:** KCL is authored configuration; Helm/Kubernetes resources express what should run; the wasmCloud operator manages Wasm workloads; CloudNativePG manages PostgreSQL clusters and databases. Do not add a WAMN deployment operator or require GitOps.
2. **WAMN-specific work:** Component build, package composition, immutable publication, initial WAMN SQL, application SQL migrations, artifact selection, and useful warnings remain WAMN functions. They do not become a general deployment manager.
3. **Independent operations:** Platform deployment, application deployment, SQL execution, and code release selection can be requested independently. A friendly command can combine the minimum necessary actions for first installation but must not reintroduce a persistent step-by-step installation process.
4. **Fast iteration:** Compatibility differences and deprecated fields produce warnings by default. Do not build a compatibility solver, automatic schema-contract orchestration, compulsory qualification, or additional approval mechanism.
5. **Deployment speed:** Make commands fast by reducing the work they perform. Remove custom shortcuts installed to compensate for an unnecessarily long deployment process; do not replace them with a smarter shortcut system.
6. **Implementation sequence:** Complete the entire deployed lifecycle in a self-contained local **kind** cluster first. Only then implement GCP-specific integration. This is a development milestone, not a requirement to run kind tests before every future deployment.

### Deployment ownership

An application release is the unit of publication, deployment, and rollback. It holds that application's packages, composed entry, routes, and component artifacts. A composed entry is the application's executable component.

`environment.k` holds the requested selection for each application, plus the shared environment configuration. Helm records the applied operations. Each application has its own Helm release, composed entry, routes, HTTP workload, Service, hostname, and initially its own host group.

Shared database infrastructure has a separate owner: the CNPG cluster, environment database, and roles belong to platform and environment provisioning. Shared environment configuration belongs to the environment operation. Application releases consume these resources without owning or reapplying them.

Environment provisioning owns the shared event stream, CDC resources, and environment materializer. These resources remain outside application Helm releases. Code-only updates do not duplicate or restart them. A shared database reset stops and reconnects every writer through these existing owners.

Deploy and rollback persist only the targeted application's release selection. Code-only operations apply only that application's Helm release. Code-only deployment performs no schema changes, migrations, resets, seed execution or application-data mutations. Rollback follows the same rule. Kubernetes, wasmCloud, and CNPG own the resources. WAMN adds no deployment controller. SQL is never reversed.

Reads and necessary WAMN control writes are allowed: release-cache entries, application release bindings and application-scoped runtime grants. Writes must be repeatable and scoped to the targeted application. Shared immutable cache entries can be added or reused. Code-only operations preserve other applications' records, effective permissions, selections, and workloads. They perform no environment-wide grant or binding reconciliation. A schema-changing reset rebuilds the database-local control facts and reconnects all affected applications while preserving their release selections.

Bindings and grants for the running release stay until the new release is ready and the old release drains. The drain wait is bounded by the environment policy's `drain_bound_seconds`. If old pods are still live after the bound, or pinned work still needs a binding, the binding stays. A failed deployment leaves the running code usable. Control records are not a second release-selection authority competing with `environment.k` and Helm.

Each application's Helm release owns its route ConfigMaps. They carry the label `wamn.route=true`, and the edge reads them by that selector. A code-only deployment changes only the targeted application's route ConfigMaps. Another application's routes stay unchanged.

```text
Developer or deployment API
  -> select immutable package/platform artifacts
  -> compile KCL and render Helm/Kubernetes resources
  -> apply only the requested resources
       -> Kubernetes controllers manage ordinary workloads
       -> wasmCloud operator manages Wasm workloads
       -> CloudNativePG manages PostgreSQL infrastructure

Independent WAMN SQL operation
  -> in-cluster Job for deployed environments
  -> apply selected SQL, record its result, return actual errors

Local `wamn dev`
  -> fast selected-package edit loop, using local runtime and database
  -> not a substitute for real deployment into kind
```

**Helm nuance:** Helm is a one-shot release manager, not a continuous controller. Kubernetes and the operators manage their resources after Helm applies them. WAMN may invoke Helm again when requested; it should not store a separate history of every installation step.

## 2. Implementation status

This section records the current code. It replaces the historical list of earlier work.

| Area | Current owner and behavior |
|---|---|
| Platform deployment | `platform/deploy.rs` consumes prepared artifacts, applies platform resources, and initializes the system database without applications. `deploy/kind/cluster.k` selects kind. |
| Platform observation | `platform/status.rs` reports observed resources and SQL state without creating a cluster. |
| Installation state | The numbered installer, steps, environment-first pass, broad installer lock, and completion ConfigMaps are deleted. `platform/job.rs` keeps bounded initialization Jobs. |
| Initial system SQL | `provision_system::install_control_store` commits the control store and baseline together. Existing state determines completion. |
| Application selection | `environment/document.rs` stores `current` and `previous` per application and rotates them from the file contents. It merges concurrent changes to different applications. Section 3 replaces the rotation rule. |
| Application deployment | `environment/mod.rs`, `expand.rs`, and `write.rs` prepare targeted control facts and apply one application's Helm release. Rollback reads `previous` from the file. |
| Routes and drain | The application release renders its route ConfigMaps with `wamn.route=true`. `environment/drain.rs` waits up to the policy drain bound and keeps bindings that live pods or pinned work still need. |
| Application publication | `crates/control/release/src/application.rs` publishes from package artifacts without installing them in a target database. Source deployment and `wamn schema publish` use the same owner. |
| Schema mode | `schema.mode` is in the Rust and KCL environment document. An omitted mode resolves from `registry.project_envs.disposable`. |
| Schema records | `catalog.packages` keeps one row per package: an immutable baseline fingerprint, the current fingerprint, and the current schema source. `catalog.package_migrations` keeps checksums per package, not per code version. |
| Schema commands | `wamn schema publish`, `wamn schema diff`, and `wamn schema apply` exist. `wamn status` reports schema state. `schema diff` also reads an unpublished source directory. That read-only path stays. Section 4 requires published bytes for deployed SQL execution. |
| Environment fingerprint | `apply_package/package_version.rs::environment_fingerprint` hashes the project baseline and each package identifier with its physical-schema hash. It excludes code versions and release digests. |
| Reset preparation | `environment/sql/reset.rs` authorizes, resolves every schema input, and records the inputs in `registry.project_envs.pending_database`. It also reserves a fresh instance suffix. Section 4 keeps the inputs and replaces the new suffix with a same-name reset. |
| Publisher identity | `wamn deploy --package` and `wamn schema publish` take `--verified-publisher-principal` from the caller. Section 3 replaces this with an identity from authenticated credentials. |
| Development loop | `wamn dev` watches selected sources and keeps running code after a failed candidate. Its reset decisions still need the common mode rule. |
| Platform UI | Local kind deployment does not select the UI. The separate upload path still consumes a local bundle matching its stamped chart. |

The accepted Epic 2 record leaves `wamn-xy404`, `wamn-1riwn`, and `wamn-c361p` open.
It records `wamn-azv06` as not reproduced, not fixed.
General platform SQL upgrades, platform UI artifact publication, and GCP deployment remain outside Epic 3.

## 3. Small deployment commands, not a scripted runbook

The following forms describe the intended developer experience. Schema commands and platform SQL upgrades remain proposals. Section 2 names the implemented paths. `--env` takes an environment file path.

| Command | Responsibility |
|---|---|
| `wamn platform deploy deploy/kind/cluster.k` | Render/apply selected platform resources and report readiness. Zero applications required. |
| `wamn deploy sample_app --env environments/dev/environment.k` | Build and publish the selected application. Initialize its empty target if needed, then apply only its Helm release. |
| `wamn deploy --release sha256:... --env environments/dev/environment.k` | Select a published application release without a local source build. Resolve its application from the declared membership. |
| `wamn schema apply sample_app --env environments/dev/environment.k --release <digest>` | Apply SQL from the chosen published release without activating its code. An omitted release uses the configured application release. |
| `wamn schema diff sample_app --env environments/dev/environment.k --release <digest>` | Read and report schema differences without mutation. |
| `wamn platform schema apply --scope system` | Apply selected WAMN system SQL; independent from platform image deployment. A project schema operation selects project environments explicitly. |
| `wamn schema reset --env environments/dev/environment.k` | Explicitly reset the whole disposable project-environment database. List affected applications and state the downtime and data loss. |
| `wamn schema adopt --env environments/dev/environment.k` | Start migration tracking from an existing selected schema state. |
| `wamn rollback sample_app --env environments/dev/environment.k [--release <digest>]` | Select the release that ran before the last successful apply. Persist it and apply only that application's Helm release. Require `--release` when the previous release is ambiguous. Never reverse SQL. |
| `wamn status --env environments/dev/environment.k` | Show each application's current and previous release, observed running artifacts, hostname, readiness, schema state, and warnings. |

Each application owns one Helm release and initially one host group. Their names derive from the environment coordinate and application identity. A code-only update preserves other applications' Helm revisions, workload definitions, and pod identities. Shared database infrastructure and environment configuration remain separately owned. A schema-changing reset can restart every application that uses the shared database.

Code-only deployment and rollback perform no schema changes, migrations, resets, seed execution or application-data mutations. They can read control state and repeat targeted writes for release-cache entries, application release bindings and application-scoped runtime grants. Shared immutable cache entries can be added or reused. Other applications' records and effective permissions stay unchanged. Neither command runs environment-wide grant or binding reconciliation.

Prepare the new release's bindings and grants alongside those of the running release. Remove old bindings and obsolete grants only after readiness and the bounded drain establish that no running code or pinned work needs them. Failed or uncertain replacement retains them and leaves the running code usable. Control records supply runtime facts, not release selection.

### Release selection

`environment.k` expresses the requested selection. Each application records `current`, the release to run, and `previous`, the release that ran before it. Helm records the applied operations of each application release. On first installation, `previous` is absent.

Deploy and rollback hold the existing application lock for the targeted application. Under that lock, the command derives `previous` from the last successfully applied release in that application's Helm history. It does not derive `previous` from the `current` value in the local checkout, because that file can be stale. Reapplying the same selection leaves both fields unchanged.

If the file and the applied state diverge, the command warns and names both releases. A divergence is not a refusal. A failed deployment keeps its requested `current`. A retry applies the same bytes and does not rotate `previous` again.

Rollback selects the last successfully applied release before the current one and applies its published bytes. It publishes no replacement release. If the local pair is ambiguous, rollback requires an explicit `--release` target. Examples are a file `previous` that differs from the applied history, or a failed deploy with no successful apply after it. Do not invoke `helm rollback` to select application code.

After artifact preparation succeeds, deploy or rollback atomically writes only the targeted selection before applying that application's Helm release. Other application selections and shared configuration remain unchanged. Status reports observed artifacts, including mixed or unknown state within the targeted application.

Do not add blanket mismatch refusals, mandatory Git commits, or another selection store.

### Authorization

Direct kubeconfig and database access is the trusted developer and bootstrap path. A developer with that access can run every command in this section against their own cluster.

The product target is authenticated WAMN control routes. The CLI and the UI call the same operations through those routes. They do not get separate deployment implementations. Each operation runs as a deployment Job with only the scope that its target needs.

The publisher identity comes from the authenticated credentials of the request. A caller-supplied string, such as today's `--verified-publisher-principal`, does not set it. On the trusted direct path, the identity comes from the credentials that the developer uses for that path.

A package digest is not a release digest. `--release` always selects an application release containing that application's dependency closure. The publisher, chart, host, CLI, and status readers use the same application boundary. A host receives only its application's release and membership. The chart routes its hostname to its own HTTP workload and Service. Full reuse of a release digest across environments is not required initially.

**Warnings by default:** Missing test results, deprecated interfaces, changed schema hashes, uncertain runtime combinations, and potentially destructive SQL should produce concise notices with the relevant artifact/database names and an actionable command. Do not demand an override to proceed. Operations that cannot execute because an artifact is absent, a component will not load, the target is unauthorized, or SQL/Helm actually fails must report failure.

## 4. Schema modes: `reset` and `migrate`

Schema lifecycle is selected per environment, independently of package SemVer.
The existing disposable designation authorizes data loss. `schema.mode` supplies no additional authorization.
Rust and KCL accept only `reset` and `migrate`.

An omitted mode resolves to `reset` only when `registry.project_envs.disposable` is true.
An absent record or a false marker resolves to `migrate`, regardless of the environment name or package version.
`wamn dev up` provisions development environments through `crates/control/dev/src/dev/environment.rs::provisioning_args`, which passes `disposable: Some(true)`.
`provision_project_env/registry.rs` records that designation and projects it into `catalog.tenant_environments`.
Provisioning without a designation retains an existing marker and creates a durable new row.

Changing mode alone performs no SQL and no Kubernetes mutation. The first mutating schema operation performs any required adoption.

```kcl
# Shared environment.k field
schema = { mode = "reset" }  # or "migrate"
```

| | `reset` | `migrate` |
|---|---|---|
| Intended target | Disposable development/test database | Database retaining data |
| Fresh install | Install current complete SQL | Install baseline and pending SQL |
| Code-only deploy | Preserve existing data | Preserve existing data |
| Physical schema change | Reset **the whole disposable project-environment database**, then install the complete selected schema | Execute explicitly selected migration files. Code-only deployment performs no schema changes, migrations, resets, seed execution or application-data mutations. |
| Editing initial SQL | Normal | Warn when already applied; do not silently replay |
| Tracking | Record physical-schema hash for each package and the database instance | Record applied migration identifiers/checksums and the baseline for each package |
| Default | For explicitly disposable environments | For environments retaining data |

### The one reset rule

```text
Reset automatically when ALL are true:
  - the operation is a deploy or explicit schema apply, not ordinary reapply or rollback
  - the selected physical schema fingerprint differs from installed state
  - schema.mode == reset
  - the existing environment is recorded as disposable
  - WAMN can reconstruct the complete target, including every installed package schema

Otherwise:
  - unchanged physical schema: preserve data
  - migrate mode: do not reset; report pending changes
  - non-disposable or uncertain target: do not perform destructive replacement
```

This rule is **identical for source-based deploy and `--release` deploy**. A source-build path must not have special authority to reset. Published release artifacts must carry the information needed to compute the same decision. If required package SQL or a selected dependency is unavailable, refuse the reset *before* touching the existing database.

**Physical fingerprint:** Calculate each package fingerprint from the complete physical schema SQL selected for installation, both authored and generated definitions. Calculate the environment fingerprint from every installed application's package identifiers and physical-schema hashes, plus WAMN's project schema. For a decision, substitute the targeted schema candidate for the targeted application's installed entries. Exclude code versions and release digests. Do not use `build_record::schema_digest` as the database identity, because that build-input digest includes generator and shared interface inputs unrelated to the physical database. Changes to code, web files, ordinary queries, or SemVer alone must not reset data.

Before any destructive execution, the command detects these conditions and refuses:

- An installed application that the candidate set omits.
- Two packages whose schema definitions conflict, for example two definitions of the same relation.
- A missing schema input for any installed package.

A schema-changing deploy uses the same decision for source and published release inputs.
In `migrate`, it reports the difference and activates code without applying SQL.
In authorized `reset`, it replaces the shared database before deploying the requested code.
An unchanged schema input performs no migrations, resets, seeds, or application-data writes.
Ordinary reapply and rollback report schema differences without inferring a reset or running migrations.

Adding an application in `migrate` initializes only its missing SQL and preserves existing data.
Adding one in `reset` changes the complete schema set and follows the authorized whole-database rule.

### Reset implementation

Local `wamn dev` reuses its disposable target and template-based database reset.
Replace its narrower triggers with the common physical-schema rule.
Code and query changes without schema changes preserve data.
Disposable `wamn dev` keeps direct source iteration.

A deployed reset is environment-wide. It replaces the whole project-environment database, which every application in the environment shares. It uses CNPG ensure on the existing Database resource: first `absent`, then `present`, under the same database name. The command waits until CNPG completes each operation before it continues. It does not allocate a new instance name, and it does not keep the old database.

The reset runs these steps in order:

1. List every affected application, including stopped applications whose installed schemas the environment keeps. This preview is read-only.
2. Acquire the existing environment `LifecycleLock` in exclusive mode. Application deployments hold it in shared mode, so alpha and beta still deploy concurrently with each other. The reset waits until running deployments release it, and no new deployment starts while the reset holds it.
3. Under the lock, do the final input validation. Resolve the complete SQL and permissions for the project substrate and every installed application. Unchanged applications use their recorded schema sources, even when their running code selects an older release. The project substrate comes from the installed platform artifact set, independently of the invoking CLI version. If an input is missing, an installed application is omitted, or two schema definitions conflict, stop before any change.
4. Pin the validated inputs and a new initialization identity in `registry.project_envs.pending_database`, outside the project database. Step 7 destroys the project database and its `catalog.packages` records, so a retry reads its inputs from this record.
5. Stop the affected application workloads and the event processing that writes to the database.
6. Ensure the Database `absent`, and wait for CNPG to complete the deletion. Removing a retaining Database resource alone does not delete its database, so the reset uses the `ensure` field, not resource deletion.
7. Ensure the Database `present` under the same name, and wait for CNPG to complete the creation.
8. Restore the complete SQL and permissions through the existing SQL Job, from the pinned inputs. The first initialization transaction writes the pinned initialization identity into the recreated database.
9. Rebuild the required bindings, caches, and CDC state through their existing owners. Do not copy arbitrary old rows, and do not reconstruct manually created workflows without an existing source.
10. Discard only this environment's obsolete queued events through the existing broker owner, while every writer is still stopped. An old database event must not create a run against the recreated database.
11. Clear the pending record. The reset is complete at this point.
12. Resume the workloads with their unchanged release selections, through the ordinary reapply of each selection. Both alpha and beta can restart, but beta's selected executable stays unchanged when only alpha requests a release change.

The reset keeps every application's release selection. The initialization identity lets status and later decisions tell the recreated database from the old one.

The command states the downtime and the data loss before it starts. All affected applications stop from step 5 until step 12. The data loss cannot be reversed: every application row, workflow definition created by hand, queued run, and other database-local state is gone. The command reports these losses. Authored seeds run only when the initialization recipe explicitly includes them.

### Reset recovery

Recovery uses only three facts: the pinned inputs, the initialization identity, and the existing resource and SQL state. There is no per-step progress store, no generic retry engine, and no background controller. A retry is the same `wamn schema reset` or schema-changing deploy, run again by the developer. It acquires the lock of step 2 and reads the pending record.

- If the reset fails before step 4, nothing changed. The applications keep running on the old database.
- If it fails at step 4 or step 5, the old database and its data are intact. The applications that step 5 stopped do not serve requests. The command names them. A retry continues the reset from the pinned inputs.
- If it fails at step 6 or later, before the recreated database carries the pinned initialization identity, the old data is already gone and no restored data exists. A retry repeats the reset from step 6.
- If the recreated database carries the pinned initialization identity, a retry does not run step 6 or step 7 again. It continues at step 8, which recognizes committed SQL, and then runs steps 9 to 11. Restored data is never destroyed by a retry.
- Step 10 runs only while the pending record exists and every writer is stopped. Writers produce new events only after step 11 clears the record. So no retry discards a newly produced event.
- If step 12 fails for an application, the reset is already complete. The command names the application. Its recovery is the ordinary reapply of its unchanged selection, which runs no reset.

A retry does not create another candidate, and it does not rotate any application's `previous` selection. A later schema-changing deploy compares fingerprints as usual. After a completed reset, the installed fingerprints match the candidate, so it does not reset again.

### Reconciliation with the committed replacement-instance code

DEP3-04 (`8b7758c72`) implemented reset preparation for a fresh replacement instance. The same-name decision changes the target of the reset, not its inputs. The committed code is kept, changed, or deleted as follows:

| Committed part | Disposition | Reason |
|---|---|---|
| Authorization against `registry.project_envs.disposable` | Keep | The rule is unchanged. |
| Candidate resolution and validation (`schema::reset_candidates`, `validate_candidates`) | Keep | Step 3 needs every input before any change. |
| Check that the SQL image matches the installed project substrate | Keep | Step 3 takes the substrate from the installed platform artifact set. |
| `registry.project_envs.pending_database` with the resolved inputs | Keep, and change its meaning | Step 4 pins the inputs outside the project database for a retry. It holds the reset inputs and the new initialization identity, not a new instance name. |
| Reservation of a new instance suffix (`reserve_database`) | Delete | The database name does not change. |
| Rendering of a second candidate Database resource | Delete | CNPG `ensure` on the existing resource replaces it. |
| Active-instance checks that compare the pending and the active suffix | Delete | There is one database name. |
| `reset::initialize` through the SQL Job | Keep, and target the same name | Step 8 uses it with the recorded inputs. |
| Retired-instance cleanup (planned, not committed) | Not built | No old instance remains. |

The trade-off is explicit. A replacement instance kept the old database usable until the switch. The same-name reset loses the old data at step 6 and has a longer interruption. Steps 2 and 3 move every refusal before any change, which keeps the data while an input is missing. If kind measurements show the interruption to be unworkable, reconsider the implementation without changing developer-facing mode semantics.

Other-application isolation applies to code-only deployment. It does not apply to a shared-database reset, which affects every application in the environment by design.

Do not reset the WAMN system database during an application reset. A disposable platform reset remains a separate explicit operation.

### Switching modes

- **`reset` → `migrate`:** Editing configuration does nothing immediately. On the first mutating schema operation, compare the recorded package baseline hashes and database initialization identity. When they match the selected state, adopt the baseline without SQL or data loss. A record match confirms what WAMN last installed, not that nobody subsequently modified the database; optional introspection can warn about drift.
- **Mismatch during adoption:** Show the differences and offer explicit adopt, manual repair, or reset. Do not silently claim unexecuted SQL has run.
- **`migrate` → `reset`:** Editing configuration does nothing immediately. The next physical schema change performs a reset only when the target is already marked disposable. Current schema must be reconstructable, including the baseline and any migrations needed to produce it.
- **Repeated transitions:** Supported. A reset recreates the database under the same name and records a fresh initialization identity. Previous disposable history is not carried forward.
- **Version 1.0:** Does not silently change the environment mode. Stability is a recommendation, not a data-loss policy.

Matching `catalog.packages` and `catalog.package_migrations` records already supply the migration starting point.
Mode edits add no history record and run no SQL.
`wamn schema adopt --env <file>` records an explicit user assertion in those same records.
Matching migration rows keep their checksums and execution outcomes.
New or changed asserted files receive `outcome = 'adopted'` and no `applied_at` timestamp.
Adoption executes none of their SQL and makes no claim about the live schema.
The next schema apply skips adopted files and executes only the remaining prefix continuation.
An uncertain `started` file requires `--resolve` before adoption.
Stopped applications retain their installed schema sources.


### SQL execution

Package migrations and their checksums are part of the immutable package artifacts. Schema operations default to the configured application release. An explicit `--release` selects another published release for the schema operation without changing the code selection. A read-only `wamn schema diff` can read a source directory, because it writes nothing. Deployed SQL execution requires published bytes. A source convenience command that executes SQL publishes first, and the deployed execution then uses those exact published bytes. Disposable `wamn dev` keeps direct source iteration.

CloudNativePG owns the database, not application SQL. In deployed kind/GCP environments, **a Kubernetes Job running the WAMN SQL executor is the default**; no developer laptop port-forward is required. `wamn dev` may connect directly to its local database. Record applied SQL only after actual completion; recognize identical retries; report real SQL errors.

Ordinary migration files are transactional: each file commits with its record. A PostgreSQL statement that cannot run in a transaction goes in its own migration file, whose first line is exactly `-- wamn:no-transaction` and which holds exactly one standalone statement. The marker is part of the checksummed bytes.

For a marked file, the SQL operation runs these steps:

1. Commit a `started` record with the package, the path, and the checksum, before the statement runs.
2. Run the statement outside a transaction.
3. Record it as `applied` only after the statement succeeds.

An unresolved `started` file never counts as completed schema state. The package fingerprint, status, and later decisions treat it as not applied. The next schema apply refuses and names the package and the file.

The developer resolves it explicitly with `wamn schema apply <app> --env <file> --resolve <package>:<path>=applied|retry`. The package name keeps the resolution unambiguous when two packages use the same path.

- `applied` means that the developer inspected the database and confirms that the statement completed. WAMN records the file as applied and does not run the SQL again.
- `retry` permits WAMN to run the same checksummed bytes again, after any manual cleanup that the developer did. A failed concurrent index build can leave an invalid index, which the developer removes before a retry. `retry` does not permit a changed file, and WAMN does not repair anything automatically. If the file bytes differ from the recorded checksum, the resolution refuses.

Execution and resolution use the same schema exclusion. The database worker holds a PostgreSQL session-level advisory lock across the transaction boundaries of the operation. So a resolution cannot run while an operation continues after the CLI disconnects. WAMN never reports atomicity or exactly-once execution for a marked file, and it has no automatic retry framework.

No Atlas operator or additional SQL controller is required.

The system database's initial setup is a separate SQL operation. `provision_system::install_control_store` installs the control store and baseline in one transaction. Existing incomplete state reports a repair action. Application schema mode never authorizes a system database reset.

## 5. Concrete deployment and upgrade use cases

These scenarios are part of the implementation specification, not short examples. They define user intent, work performed, work not performed, warnings, and recovery. Commands are **proposed syntax**. They do not imply a WAMN deployment controller or a fixed sequence for future upgrades.

**Common execution model:** A CLI command selects artifacts and configuration, invokes Helm or the Kubernetes API for long-lived resources, and reports running state. Kubernetes, the wasmCloud operator, and CloudNativePG continue managing the resources after the command exits. WAMN SQL is a distinct operation executed by a short-lived Job in deployed environments. The friendly CLI can combine the minimum operations needed to initialize an empty application, but it does not store progress through a general installation process.

**Common state and safety:** Every operation identifies the target, selection, running version when reachable, and completed effects. A different code/schema version, unavailable optional test result, or possibly breaking interface produces a warning. Missing artifact bytes, authorization failure, failed SQL, invalid component instantiation, or failed Helm/Kubernetes execution produces a failed operation. A configured `reset` on an explicitly disposable environment is intentional and is reported before replacing data.

### 5.1 Fresh platform installation, with zero applications

**Intent:** Start with an empty local Kubernetes cluster and finish with a WAMN platform able to accept application installations. This must work without any application package, application release, GCP project, or cloud credentials.

```bash
# Stage 1: complete the use case locally first.
wamn platform deploy deploy/kind/cluster.k --chart /tmp/wamn-platform-chart
wamn platform status deploy/kind/cluster.k

# Stage 2: supported only after the local implementation works.
wamn platform deploy deploy/gcp/cluster.k
```

`deploy/kind/cluster.k` exists and selects `provider = "kind"`. Local work requires Docker, kind, a local OCI registry, cluster configuration, and access to required pinned images. It must not require Cloud Build, GKE, GCS, Artifact Registry, Workload Identity, cloud IAM, or public DNS. GCP-specific prerequisites remain owned by their existing infrastructure mechanism, including OpenTofu where appropriate.

**Execution:**

1. **Inspect the target.** Compile KCL, resolve the Kubernetes context, identify the selected published platform images, and report which cluster will change. No application configuration is needed.
2. **Ensure the required Kubernetes APIs exist.** Apply the chart/operator prerequisites in the limited order needed for CRD registration. Wait for the operator APIs to become available before submitting custom resources. This is a small bootstrap dependency, not a replayable numbered installation controller.
3. **Apply platform resources.** Use Helm/Kubernetes resources for NATS, certificates, wasmCloud runtime, CNPG, identity, the control service, and the other platform services included in the selected configuration. Existing controllers manage their running state.
4. **Initialize only an empty system database.** Once the CNPG database is available, run the WAMN system SQL initializer. Record completion in the database. The initializer should commit its SQL and initial schema record together where possible; if interruption leaves partial initialization, report the state and a repair action rather than silently deleting the system database.
5. **Install initial WAMN authority.** Create only the organization, identities, and credentials needed for a functioning platform. Do not publish application packages or select application releases as a side effect.
6. **Check the result.** Query Kubernetes and service endpoints for the actual running condition; report ready, failed, or incomplete components. There is no extra WAMN installation-completion ConfigMap.

**Must remain unchanged:** No application-owned database SQL, application publication, or application activation. The installer does not compile platform UI files or app source. It can deploy an already published platform UI artifact without requiring a local source checkout.

**Failure example:** CNPG and the system SQL succeed but identity does not start. The result names the successful SQL operation and the failed identity workload. A second `platform deploy` reapplies the selected Helm/Kubernetes resources and checks the system database record; it does not initialize the system database again or depend on a completed installation step number.

```text
Platform: kind / wamn-local
  CNPG database             ready
  System SQL                installed
  wasmCloud operator        ready
  Identity service          failed
  Control service           not-ready
  Application releases      none
Result: incomplete
Next: inspect identity configuration, then rerun platform deploy
```

**Useful warnings:** Optional route configuration absent, no test result for a selected image set, limited resources on the kind node. **Actual failures:** Cannot authenticate to the selected cluster, necessary artifact absent, system SQL error, or a required platform service not running.

**Possible stricter option:** A team may choose a preview of rendered changes and selected component tests. Those are optional; installing an empty platform does not require an application test suite.

### 5.2 Fresh application installation, including its initial SQL

Intent: Install a new application on an existing platform with one command. The environment can be new or already contain other applications.

```bash
wamn deploy sample_app --env environments/dev/environment.k

# Use published application bytes without a source build:
wamn deploy --release sha256:<release-digest> --env environments/dev/environment.k
```

`--release` selects a published application release. It contains the selected application's packages, composed entry, routes, and component artifacts. It does not contain another application's selection.

1. Identify the selected application and its package dependency closure from the declared membership.
2. For source deployment, build changed components and recompose the targeted application. Use a disposable build database when SQL introspection needs one.
3. Publish immutable package and component artifacts, then construct that application's release from those facts. Publication requires no destination application database or installed candidate version.
4. For a new environment, invoke its separate provisioning operation. CNPG owns the database, and provisioning owns shared roles, credentials, and environment configuration.
5. Initialize an absent application's SQL through the independent SQL operation. For existing `migrate` state, report pending SQL without applying it. A schema change in a disposable `reset` environment follows section 4. Code-only deployment performs no schema changes, migrations, resets, seed execution or application-data mutations.
6. Resolve the targeted application's connections, credentials, hostname, and permissions. Prepare its release-cache entries, application release bindings and application-scoped runtime grants. Report absent optional configuration. Refuse unavailable required resources.
7. Persist only that application's selection as section 3 specifies, with `previous` derived under the application lock. Apply its Helm release with its own host group, composed entry, HTTP workload, Service, and hostname. Report its observed running artifacts and readiness.

Shared database ownership does not merge application deployment ownership. Adding an application initializes its absent baseline unless section 4 requires a whole-environment schema reset. That database operation lists every affected application before replacement. It cannot occur as a side effect of a code-only application deployment.

Application rollout does not reinstall the platform or reapply another application's Helm release. Code-only operations preserve every other application's selection, workloads, and data. Shared resources change only through their separate owner when initialization needs them.

Control reads and the targeted writes above are allowed. Writes must be repeatable. Shared immutable cache entries can be added or reused. Other applications' records and effective permissions stay unchanged. Do not run environment-wide grant or binding reconciliation. Control records do not select releases.

Keep the running release's bindings and grants alongside the new release's requirements until readiness and drain permit removal under section 3. A failed deployment leaves the running code usable.

If initial SQL succeeds but rollout fails, keep the SQL record. Retrying the selection uses published bytes and does not repeat committed initialization. The file retains the requested release, and status reports what actually runs.

```text
Deploy sample_app -> org/project/dev
  sample_app current        sha256:A1
  sample_app previous       none
  sample_app running        unknown (not ready)
  other_app current         sha256:B0 (unchanged)
  other_app Helm revision   unchanged
  App SQL                   initialized successfully
Result: incomplete
Next: retry sample_app current selection
SQL already applied is not reversed
```

Warnings identify schema differences, deprecated interfaces, and missing optional test results. Missing artifacts, unauthorized access, unavailable required connections, failed SQL, and failed component startup remain errors.

### 5.3 Platform upgrades: behavior, WAMN SQL, and infrastructure

**Intent:** Update one part of the platform without repeating initial installation or changing unrelated applications. Implement against an already running kind platform before introducing GCP-specific actions.

| Change | Owner and operation | Must not change implicitly |
|---|---|---|
| Host/runtime image | Helm/Kubernetes resource selection; wasmCloud manages Wasm workloads | Application release digests and application SQL |
| Control/identity service image | Helm/Kubernetes | Application packages or host images unless explicitly selected |
| Platform UI | Published web artifact and relevant resource | Application and control SQL |
| WAMN system schema | WAMN SQL executor targeting the system database | Running application code or application-owned SQL |
| WAMN project substrate schema | WAMN SQL executor targeting selected project environments | Application-owned SQL in those databases |
| PostgreSQL engine/operator | CloudNativePG and the relevant infrastructure owner | Application migration files |

#### 5.3.1 Behavior-only or configuration-only change

```bash
# Illustrative selection of one platform component.
wamn platform deploy deploy/kind/cluster.k --only host

# A control-service fix is independent.
wamn platform deploy deploy/kind/cluster.k --only control
```

1. Select an already published image/configuration; building and publishing may be invoked separately. `platform deploy` itself consumes artifacts.
2. Report which workloads will actually change. A shared host image may affect several environments; list those affected host groups and allow a smaller selection when supported.
3. Submit the changed resource definition through Helm/Kubernetes. Do not invoke application package installation, new app release construction, or WAMN SQL. The wasmCloud operator manages Wasm workload updates.
4. Check affected workloads and report which are running the selected image. If one host group fails, report it independently rather than declaring the entire platform successful.
5. Reselect a prior image when requested. No SQL reversal is implied by selecting earlier executable bytes.

**Example:** A host security fix should require no new application package, no schema migration, no manual release-digest edits, and no rerun of initial organization setup. An application release can remain exactly the same while its host image changes.

**Warnings:** Untested application/host pairing or changed WIT interface availability. **Actual failure:** The new host cannot instantiate a required component or Kubernetes cannot make it ready. A warning is not a mandatory compatibility approval.

#### 5.3.2 WAMN system or project schema change

```bash
wamn platform schema diff --scope system
wamn platform schema apply --scope system
wamn platform deploy deploy/kind/cluster.k --only control

# Independent project-substrate SQL targeting selected environments:
wamn platform schema apply --scope project --env environments/dev/environment.k
```

1. Select the WAMN-owned SQL change and target databases; platform system SQL and project substrate SQL are different operations.
2. Display pending statements and inexpensive warnings about known SQL references, removed columns, and affected services. A hash difference alone is not a blanket refusal.
3. Run the selected SQL through an in-cluster Job. In the system database, initial bootstrap and later SQL changes must have separate entry points; the application reset mode cannot reset the WAMN system database.
4. Record completed SQL IDs/checksums and return actual errors. PostgreSQL statements requiring different transaction handling are reported distinctly.
5. Allow the developer to deploy changed control-service code before or after SQL as the particular change requires. Do not impose one universal SQL/code sequence.

**Failure and retry:** A committed SQL change remains committed if a later control-service image fails. Reapplying the image or selecting its predecessor does not reverse SQL. If SQL partially commits, report exactly what completed and what requires manual repair or a corrective statement.

**Useful warnings:** Old control executable may expect a removed database object; next executable requires an object not yet installed. A known impossible SQL operation is an execution failure, not merely a warning.

#### 5.3.3 PostgreSQL and operator upgrades

PostgreSQL engine changes belong to CloudNativePG. Operator and CRD changes belong to their existing Kubernetes/Helm owners. WAMN can select updated resources and read their status, but should not implement its own database engine upgrade procedure. No application migration is implied.

**Optional additional checks:** Targeted runtime tests or SQL previews may be requested. They are not required for every local hotfix.

### 5.4 Application schema change, including rolling SQL and code changes

**Intent:** Apply selected application SQL without rebuilding or redeploying the application. Support multi-step changes chosen by the developer instead of creating an automatic SQL/application upgrade process.

```bash
wamn schema diff sample_app --env environments/dev/environment.k
wamn schema apply sample_app --env environments/dev/environment.k
```

#### 5.4.1 `migrate` mode

1. Identify the selected application SQL and target database. Read the per-package baseline and already-applied SQL records; package SemVer is not the database-state authority.
2. Display pending statements and evidence-backed warnings, such as a removed column referenced in a generated query. Do not insist on a complete field dependency analysis.
3. Execute pending migration statements in the appropriate order through the WAMN SQL Job. Transactional changes should commit their result record together with the actual SQL where possible. Nontransactional SQL must report its distinct outcome.
4. Leave selected and running application releases unchanged. There is no automatic rebuild, host restart, backfill, SQL contraction, or application release selection.
5. Retry recognizes committed migration IDs. If a previously applied file has different bytes, report the discrepancy; do not silently run it again under the same ID. The developer can issue a new corrective migration.

**Example: renaming a field while old and new application instances may coexist**

| Step | Developer action | Database | Application | WAMN behavior |
|---|---|---|---|---|
| 0 | Existing state | `display_name` | A0 | No changes |
| 1 | Apply additive SQL | Both `display_name` and `public_name` | A0 | Record SQL; keep code |
| 2 | Deploy transitional code A1 | Both columns | A0/A1 during rollout | Update app release under the code-only rule in section 5.5 |
| 3 | Explicit data backfill and synchronization | Both columns with consistent values | A1 | Execute only selected SQL/data task |
| 4 | Apply SQL removing old column | `public_name` | A1 | Warn about older code; run selected SQL |
| 5 | Deploy final code A2 if necessary | `public_name` | A2 | Update app release under the code-only rule in section 5.5 |

This is one example, not a required ordering. A change may need fewer operations or additional transitional code deployments. WAMN does not automatically synchronize old/new columns or determine when durable workflows no longer need an old column. Developers manage data semantics and the timing of removal. Existing WIT and integration parameter deprecation metadata can help produce warnings without creating mandatory retirement rules.

```text
Application SQL: sample_app/dev
  Selected statement        0003_drop_display_name.sql
  WARN possible-breaking-change: display_name is referenced by an earlier release
  Next: inspect the SQL difference and earlier executable requirements
  Applying SQL ... success
  Release selected          A1 (unchanged)
  Release running           A1 (unchanged)
Result: applied
```

**Failure and retry:** Failed SQL stops the selected schema operation, identifies what committed, and does not attempt an application rollback. The developer can submit corrective SQL independently. A different schema hash is not itself a reason to discard data in `migrate` mode.

#### 5.4.2 `reset` mode

In `reset` mode, a schema-changing deploy or explicit schema apply on a recorded disposable environment triggers a **whole project-environment database** reset. This rule applies equally when the selected schema comes from source or from published release artifacts. It does not trigger for handler code, ordinary query edits, web changes, or SemVer changes without physical schema changes.

1. Compare the selected physical schema fingerprints with the recorded installed fingerprints.
2. If unchanged, do nothing to the database. If changed, list all applications with retained schemas in the shared database and the data that will be discarded.
3. Verify that WAMN can reconstruct the WAMN-owned project schema, every installed package's complete SQL, privileges, and CDC prerequisites before changing anything. Refuse when an installed application is omitted or two schema definitions conflict.
4. State the downtime and the irreversible data loss. Then run the environment-wide reset of section 4: CNPG ensure `absent`, then `present`, under the same database name, with a wait for each operation. Do not alter the WAMN system database.
5. Activate the selected application release when the deploy requested it. Report a failed step accurately, and retry on the same database name.

**Explicit reset:** `wamn schema reset --env environments/dev/environment.k` performs the same database operation even if the schema fingerprints have not changed. This is only permitted for a target WAMN already records as disposable. The configured schema mode alone does not authorize destructive changes.

**Scope:** One project-environment database may contain several applications. Resetting it discards all their data; it is not a per-application table deletion operation. CNPG's existing resource ownership is reused, not bypassed by manually dropping tables in an active database.

### 5.5 Application code upgrade, hotfix and rollback

Intent: Change one application's release while preserving application schemas and data and every other application's records, effective permissions, selection and workloads.

```bash
wamn deploy sample_app --env environments/dev/environment.k
wamn rollback sample_app --env environments/dev/environment.k

# Select published application bytes without a source build:
wamn deploy --release sha256:<release-digest> --env environments/dev/environment.k
```

1. Build only changed inputs within the targeted application's dependency closure. Recompose its affected executable. Leave other applications and platform artifacts untouched.
2. Publish the targeted application's immutable release. Publication uses verified package facts without installing the candidate into the destination database.
3. Report differences between selected schema information and recorded installed state. Code-only deployment performs no schema changes, migrations, resets, seed execution or application-data mutations. Installed SQL records stay unchanged.
4. Under the application lock, derive `previous` from the last successfully applied release and persist the new `current`. Warn when the file disagrees. Apply only its Helm release. Kubernetes and wasmCloud manage its host group and workloads.
5. Report that application's selected and observed running releases separately. Report readiness from the actual workload, including after failure.
6. For rollback, select the last successfully applied release before the current one, or the explicit `--release` target when the local pair is ambiguous. Apply only that application's Helm release. Never invoke `helm rollback`, republish an environment aggregate, or reverse SQL. Warn when the older code expects different SQL. Attempt rollback when the artifacts and target are valid.

Rollback follows the same code-only rule. Reads and necessary control writes are allowed for release-cache entries, application release bindings and application-scoped runtime grants. Writes must be repeatable and targeted. Shared immutable cache entries can be added or reused. Do not run environment-wide grant or binding reconciliation or change another application's effective permissions or records.

Prepare the selected release's bindings and grants before applying Helm. Preserve those needed by running code until the replacement is ready and the old release drains. Remove obsolete facts only under section 3's removal rule. A failed deployment leaves the running code usable. Control records never compete with `environment.k` and Helm as a release-selection authority.

```text
Deploy sample_app -> org/project/dev
  Application release       sha256:A1
  Host image                unchanged
  Installed schema          sha256:S1
  Build-time schema         sha256:S1
  Application schema/data changes   none
  sample_app current        sha256:A1
  sample_app previous       sha256:A0
  sample_app running        sha256:A1 (ready)
  other_app current         sha256:B1 (unchanged)
  other_app Helm revision   unchanged
  other_app pod UIDs        unchanged
Result: deployed
```

A build or publication failure leaves the file and running workloads unchanged. If Helm fails after selection, the file retains A1 as current and A0 as previous. Status reports A0 if it still runs, or the observed mixed state. Retrying A1 uses the same bytes and preserves A0 as previous, because A0 is still the last successfully applied release.

Stale checkout example: developer 1 deploys A1 from a checkout. Developer 2 has an older checkout that still says `current = A0`, and deploys A2. Under the application lock, the command derives `previous = A1` from the last successful apply, writes `current = A2, previous = A1`, and warns that the file said A0. A later rollback selects A1.

Start with application selections A0 and B0. Updating the first selects A1 and keeps A0 as its previous release. Updating the second selects B1 and keeps B0 as its previous release. Rolling back the first selects A0 and leaves B1 selected. The second application's Helm revision and pod UIDs stay unchanged during that rollback. Both applications keep their data throughout.

Resource previews, schema differences, and targeted tests remain optional guidance. Missing optional qualification produces a warning. These features use the same application deployment path.

### 5.6 Local edit loop is not deployed kind

`wamn dev` retains its fast local source-watching runtime. It should discover the selected package and dependencies, regenerate/recompile only changed inputs, and keep the last running application when a candidate build fails. An edited physical schema in `reset` mode can reset its explicitly disposable local database; an ordinary query or code edit preserves rows. In `migrate` mode, a developer applies SQL explicitly.

**Separately**, the kind deployment scenarios above must use the same published artifacts, Helm charts, wasmCloud operator and CNPG integration intended for GCP. A passing local edit loop does not establish that a fresh platform or release can be installed into Kubernetes.

### 5.7 Mode changes are independent of schema execution

| Sequence | User action | Behavior |
|---|---|---|
| A | Change `schema.mode = "reset"` to `"migrate"` | No immediate SQL or data change |
| B | First mutating schema operation after A | If selected per-package baselines match the recorded installed state, adopt them as migration starting points without rerunning SQL |
| C | Baseline mismatch or unknown instance | Warn and require an explicit choice to adopt, repair, or reset; do not claim SQL ran when it did not |
| D | Apply new migration | Run selected SQL and keep rows |
| E | Change back to `"reset"` | No immediate data change |
| F | Later selected physical schema change, with a disposable target | Reconstruct a fresh project-environment database from current selected SQL |
| G | Change again to `"migrate"` | Adopt the new baseline; do not restore discarded earlier migration records |

A mode change never resets the system database or automatically changes the application release. No mode change is tied to package SemVer or the first 1.0 release.

### 5.8 Cross-cutting recovery and status

Every workflow reports each application's current and previous release, observed running artifacts, hostname, and readiness. Shared environment and database state appears separately. SQL status reports only completed SQL recorded by the database. Unreadable state is `unknown` with its cause.

Helm history records the applied operations of the targeted application's Helm release. Under the application lock, it supplies the last successfully applied release from which `previous` is derived. Do not add a database that tracks deployment actions.

Code-only deployment, retry and rollback perform no schema changes, migrations, resets, seed execution or application-data mutations. Reads and repeatable targeted writes can prepare release-cache entries, application release bindings and application-scoped runtime grants. Shared immutable cache entries can be added or reused. Other applications' records, effective permissions, selections and workloads stay unchanged. Recovery performs no environment-wide grant or binding reconciliation. Control records do not select releases.

Keep old and new release bindings and grants together during replacement. Remove old bindings and obsolete grants only after readiness and drain satisfy section 3. A failed or interrupted deployment leaves the running code usable. If readiness, live releases or pinned work are unknown, retain the facts that running code can still need.

```text
Target: org/project/dev
  Platform host              H2 (ready)
  System SQL                 PS4 (recorded)
  Project schema             PP2 (recorded)
  sample_app current         A9
  sample_app previous        A8
  sample_app running         A8 (ready)
  sample_app hostname        sample.example.test
  other_app current          B4
  other_app previous         B3
  other_app running          B4 (ready)
  other_app hostname         other.example.test
  Application SQL            S5 (recorded)
  Last sample_app apply      failed at readiness
  WARN schema-mismatch: A9 was built using different schema inputs
  NOT CHECKED: optional application test
Next: retry sample_app current selection or roll back sample_app to A8
SQL already applied is not reversed
```

After an interrupted Helm write, read that application's Helm and Kubernetes resources and reapply its persisted selection. Preserve its previous selection during retry. Rollback follows section 3. Other application selections, Helm revisions, workloads, and data stay unchanged.

After interrupted SQL execution, read the applied SQL records and report incomplete work. Do not trust installer completion ConfigMaps. A failed application rollout leaves committed SQL in place. Platform executable rollback also leaves committed platform SQL in place.

Serialize SQL mutations to the same database. Protect the shared file during selection updates so that concurrent application commands cannot overwrite each other's selections. Each application has its own application lock, so alpha and beta deployments run concurrently. An environment-wide reset excludes every application deployment in that environment. Normal application rollout needs no global platform installation lock.

## 6. Warnings and operation results

Start with a small interface rather than a large permanent warning list.

```text
WARN schema-mismatch
The selected application was built against different schema input.
  application: sha256:...
  installed:   sha256:...
  next:        wamn schema diff sample_app --env environments/dev/environment.k

Application: sample_app
Release current:  sha256:...
Release previous: sha256:...
Release running:  sha256:...
Status: ready | not-ready | failed | not-checked | unknown
```

Each notice identifies the application, the evidence, and one next action. A warning is not a hidden refusal. Show current, previous, and observed running releases separately after a failed apply. Report shared environment configuration and recorded SQL under their own owners. Output must not expose secrets.

Optional targeted tests, SQL previews, deeper diff analysis, and team deployment restrictions can be added later without changing the core execution path. Mandatory release qualification is not the default developer workflow.

## 7. Remaining implementation sequence

Epics 1 and 2 are complete as section 2 describes.
Their deleted modules are not future implementation tasks.
Epic 3 extends the existing resource and SQL owners with these operations:

| Order | Implement | Replace or remove | Completion condition |
|---|---|---|---|
| Schema configuration | Shared mode and recorded disposable defaults | Conflicting reset rules and stale implementation tasks | Selection writes preserve mode and other applications |
| Schema records and decisions | Installed schema sources, checksums, explicit schema input, and mode decisions | Immutable-root assumptions that prevent independent schema changes | Code selection and installed SQL advance independently |
| Disposable reset | Environment-wide reset with CNPG ensure absent then present under the same name, writer stop and resume, CDC rebuild, and old-event disposal | New instance-name reservation, a second candidate Database, retired-instance cleanup, and local reset decisions based only on edited migration history | Schema-changing deploy resets the whole authorized database |
| Migration and adoption | Cross-version SQL application, nontransactional outcomes, and reversible mode switches | Implicit SQL during code activation | Explicit SQL preserves application selection and supports retries |
| Selection and identity | `previous` from the last successful apply under the application lock, explicit rollback target, publisher identity from credentials | File-only rotation and caller-supplied publisher strings | A stale checkout cannot lose a rollback target |
| Recovery and acceptance | Same-name reset retry and focused product-command journeys | Duplicate installers or test-owned deployment steps | Reset, migrate, and code-only behavior work through product commands |
| Later GCP integration | Existing commands with provider-specific infrastructure inputs | Unnecessary deployment forks | Equivalent platform, application, and schema operations work on GCP |

### Epic 3 local kind completion

Epics 1 and 2 already supply the platform command and independent application deployment commands.
Epic 3 tests these product operations through focused journeys, without reinstating deleted cluster suites.
Kind acceptance runs at epic close, separately from cheap owner-level tests.

1. Deploy, reapply, and roll back a code-only alpha update. Preserve both applications' rows and the database instance. Preserve beta's selection, Helm revision, and pod identity.
2. Deploy changed alpha schema in an authorized reset environment. Reset the shared database under the same name with complete alpha and beta schemas. Preserve beta's executable selection. Repeat unchanged input without another reset. Exercise source and published inputs equally.
3. Apply selected SQL before and after code releases in migrate mode. Preserve rows and application selections during SQL-only operations. Retain checksums and avoid repeated data effects.
4. Switch modes repeatedly. Preserve data during configuration edits and matching adoption. Refuse destruction on a target without the recorded disposable designation.
5. Interrupt the reset before and after CNPG recreates the database, then retry on the same name. Keep old events out of the recreated database. Leave another environment's data and events untouched.
6. Deploy from a stale checkout and roll back. The rollback target is the last successfully applied release.

Missing schema inputs, an omitted installed application, and conflicting definitions stop the reset before the database changes.
Rollback handling, publisher verification, and schema validation stay in their existing owners: the selection document and application lock, the publication owner, and `validate_candidates`. Each gets focused tests at the lowest tier. No new broad acceptance cycle runs until the implementation settles.
Transactional failure, edited checksums, and nontransactional uncertainty belong in the lowest tests that observe those contracts.
Add deployed cases only for boundaries that those tests cannot observe.
General platform SQL upgrades, browser tests, and registry-archive tests are outside Epic 3 acceptance.

### GCP scope after local success

Keep the same build, publication, application release, SQL, and Helm implementations. Add only provider-specific details: GKE context and cluster resources, service identity and IAM, Artifact Registry, object storage, ingress/network resources, and certificates. OpenTofu remains responsible for resources it already owns. Avoid a second deployment implementation or a GCP-only command flow.

## 8. Explicitly deferred

- A WAMN Kubernetes operator, GitOps requirement, or additional SQL operator.
- Compatibility range solver, automatic schema retirement, application-version-based reset policy, and strict field deprecation enforcement.
- Mandatory release qualification or a global predeployment checklist. Run selected tests when useful; a separate team policy may later require them.
- Automatic platform/system database reset after failed initialization.
- General deployment history database, progress checkpoint records, broad distributed orchestration, and speculative concurrency controls.
- Platform UI deployment controls until the authenticated control route works. The UI then calls the same operation as the CLI, not another deployment implementation.
- Optimization for unchanged Helm applies until actual measurements show an important cost.

## 9. Measure the result

Measure cold local platform installation, first application installation, code-only edit-to-ready time, SQL-only update time, Helm reapply time, and recovery after an interrupted command. Also record the number of manual steps and how many unrelated artifacts were rebuilt. **Prefer deleting an operation to caching its completion.** Source-level review alone cannot establish the performance improvement; the local kind implementation should provide those measurements.
