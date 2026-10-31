# Audit lane C: Update and upgrade

Three read-only Codex gpt-6.1-sol runs at reasoning effort high wrote this file, one per part, on 2026-10-31 against commit 8c831349d. Root copied each run's final message verbatim and checked that every finding cites a file and line. Beads: wamn-16jwe.1, wamn-16jwe.2, wamn-16jwe.3. Shared tool output: [measurements.md](measurements.md).

<!-- lane C1, bead wamn-16jwe.1 -->

## Part 1: schema

Audited checkout `8c831349d`. No files or issue statuses changed. No builds or tests ran. Bead descriptions came from local Dolt backups because `bd --readonly show` failed on a writable `LOCK` requirement. Current tracker status remains unverified.

## Finding C1-1: Schema truth describes fresh installations but cannot advance installed databases

Severity: high  
Effort: L  
Area: deploy/sql, crates/control/provision/src/schema_migrations.rs  
Evidence: `crates/control/provision/src/schema_migrations.rs:3`, `crates/control/provision/src/schema_migrations.rs:92`, `crates/control/lib/src/upgrade_schema.rs:127`, `deploy/sql/system-schema.sql:174`, `deploy/sql/system-schema.sql:210`, `deploy/sql/postgres-init.sql:1`.

Problem: The full SQL files define the intended schema, while each installed database holds its actual layout. `registry.meta.schema_version = '0.1'` identifies the registry format, not the precise installed DDL. Both migration lists are empty, and `upgrade-schema` returns success without examining the database. `postgres-init.sql` builds test fixtures and supplies no production upgrade path.

Proposal: Use migrations, ordered database changes, as the sole authored path for both installation and upgrade. Put the current system and project schemas into explicit baseline migrations, then record each applied file and its hash in the existing ledger tables. Keep the registry format version separate. A generated full schema can later serve as an installation optimization if equivalence tests justify it. A declarative diff tool, which calculates changes from desired definitions, still needs explicit data conversions and authority rules here.

Deletes: Independently maintained full-schema installation paths and `record_fresh_install`, which records migrations without executing them. Move the existing SQL into baseline migrations.

Risk: Migration ordering can break functions, roles, policies, or foreign keys. Prove that fresh installation and predecessor upgrade produce equivalent schemas, grants, seed data, and ledger entries.

## Finding C1-2: Installer completion records substitute for database upgrade state

Severity: high  
Effort: M  
Area: crates/control/lib/src/platform  
Evidence: `crates/control/lib/src/platform/install.rs:309`, `crates/control/lib/src/platform/install.rs:360`, `crates/control/lib/src/platform/install.rs:405`, `crates/control/lib/src/platform/job.rs:643`, `crates/control/lib/src/platform/environments.rs:46`, `crates/control/lib/src/environment/mod.rs:296`.

Problem: `wamn-bj2kn` and `wamn-sdrnq` share one cause: newer code reaches older schemas after step 4.7 skips the completed bootstrap. Their reported symptoms differ: a missing connection-reader function and a missing `env_key` column. `wamn-zeg0z` exposes the opposite cache rule: a changed image reruns first apply with `release = none`, which requests removal of the serving release. These failures share an installer that models completed steps without a database upgrade lifecycle.

Proposal: Let the database ledger decide pending schema work on every apply. Run schema advancement before dependent provisioning Jobs. Remove the synthetic first-apply document with `release = none` for installed environments and converge the substrate through the ordinary lifecycle. Git contains the later `wamn-zeg0z` fix `8a3b3cc29`, outside audited HEAD, but that replay fix supplies no schema upgrade path.

Deletes: The installed-environment replay of first apply and completion-marker decisions about schema readiness.

Risk: A platform update can stop traffic or leave credentials incompatible with the database. Prove an image change against a retained database while requests continue and the serving release stays installed.

## Finding C1-3: Project migrations run after queries that already require the new schema

Severity: high  
Effort: M  
Area: crates/control/lib/src/environment  
Evidence: `crates/control/lib/src/environment/mod.rs:276`, `crates/control/lib/src/environment/analyse.rs:400`, `crates/control/lib/src/environment/ensure.rs:138`, `crates/control/lib/src/environment/stage.rs:51`.

Problem: Environment apply reads installed packages during analysis before it reaches either project migration call. Those reads already require columns such as `env_key`. Restoring migration files alone cannot repair a predecessor database when analysis fails before the applier runs.

Proposal: Resolve database identity through a small, stable bootstrap interface. Advance the system schema before control reads and each project schema before environment analysis. Then run ordinary analysis and reconciliation against the established schema revision.

Deletes: The duplicate project migration calls in substrate ensure and package staging.

Risk: Incorrect sequencing can migrate the wrong database or bypass identity checks. Test a registered predecessor database that lacks a column required by analysis, plus refusal of an unregistered database.

## Finding C1-4: A failed control bootstrap can leave an installation that refuses every retry

Severity: high  
Effort: M  
Area: crates/control/lib/src/provision_system.rs  
Evidence: `crates/control/lib/src/provision_system.rs:78`, `crates/control/lib/src/provision_system.rs:110`, `crates/control/lib/src/provision_system.rs:116`, `crates/control/lib/src/provision_system.rs:53`, `crates/control/provision/src/lib.rs:164`.

Problem: The core system schema and portable control store run as separate batches without an enclosing transaction. A failure after the core batch leaves `registry` present, but a retry refuses any database with that schema. Domain assignment also runs after bootstrap, so completion spans several database operations without one atomic boundary.

Proposal: Apply the complete database baseline, domain assignment, and migration records in one transaction under a database lock. Keep cluster-level role preparation convergent. Treat an existing baseline as recorded state that the common applier can inspect.

Deletes: The `registry` existence test as the installation-completion authority and the separate unrecorded bootstrap stages.

Risk: A crash can strand a partially installed control database. Inject failure during the portable-store stage and before domain assignment, then prove rollback and successful retry.

## Finding C1-5: Run-plane repair is a partial second schema-upgrade engine

Severity: medium  
Effort: L  
Area: crates/schema/control/src/run_plane, crates/control/lib/src/reconcile_run_plane.rs  
Evidence: `crates/schema/control/src/run_plane/schema.rs:160`, `crates/schema/control/src/run_plane/plan.rs:337`, `crates/schema/control/src/run_plane/plan.rs:667`, `crates/control/lib/src/reconcile_run_plane.rs:505`, `crates/control/lib/src/reconcile_run_plane.rs:639`, `deploy/sql/catalog-schema.sql:215`. Related duplication: measurements.md section 7.

Problem: The reconciler parses SQL layout to create missing catalog tables and add missing run-plane columns. It does not provide equivalent evolution for existing catalog columns or the whole platform schema, whose presence is inferred from `platform.user`. The catalog file also creates `platform` relations outside the catalog-table loop. Repairing selected objects therefore cannot substitute for a complete upgrade.

Proposal: Give ordered migrations ownership of structural changes, including backfills and constraint transitions. Retain reconciliation for credential grants and security rules that must continuously converge. Use schema observation to detect unsupported structural drift before serving traffic.

Deletes: Structural SQL slicing, missing-column mutation, and separate table-bootstrap actions once the common migration path owns those changes.

Risk: Removing repair prematurely can break provisioning or weaken security convergence. Prove migration coverage alongside existing role, policy, and privilege-denial tests.

## Finding C1-6: Retained migration records conflict with the promised restart at 0001

Severity: medium  
Effort: M  
Area: crates/control/lib/src/upgrade_schema.rs, crates/control/provision/src/schema_migrations.rs  
Evidence: `crates/control/provision/src/schema_migrations.rs:4`, `crates/control/lib/src/upgrade_schema.rs:187`, `crates/control/lib/src/upgrade_schema.rs:198`, `crates/control/lib/src/upgrade_schema.rs:361`, `crates/control/lib/src/upgrade_schema.rs:380`.

Problem: The code promises that migrations can restart at `0001`, but its test deliberately retains a retired `0001` record. An empty list ignores that record today. A new file at ordinal one later collides with the retained path or hash and produces `schema-migration-drift`.

Proposal: Establish an explicit baseline boundary and accept only supported histories. Refuse unknown or newer records even when there are no pending files. For greenfield databases, reject unsupported histories and require an explicit recreation decision instead of silently adopting them.

Deletes: The empty-list bypass and the test that treats an unrecognized retired ledger as a successful no-op. Remove legacy adoption machinery if no retained installation requires it.

Risk: Restarted numbering can block upgrades or conceal an incompatible database. Test retired records, newer histories, missing ordinals, and altered hashes.

## Open questions for the owner

1. Which installed databases require data-preserving upgrades, and which can receive one explicit reset to the new baseline?
2. Must old and new platform images serve concurrently during upgrades, or can schema changes require a maintenance interval?
3. What live evidence closes `wamn-m6or2`? Commit `92cf2aded` corrects its fixture in this tree. `crates/control/lib/tests/environment_apply_kind.rs:3063` now requires component preload without an HTTP role, but this audit did not run it.

## Proposed fix epics

### One platform schema history

1. Establish system and project baseline migrations
2. Replace independent bootstrap and structural repair paths
3. Prove fresh-install and upgrade equivalence

### Upgrade installed clusters safely

1. Advance schemas before dependent readers
2. Remove installed-environment first-apply replay
3. Prove retained-database upgrades across platform images

### Recover from interrupted schema work

1. Make control bootstrap transactional
2. Define supported migration histories and refuse unknown state
3. Prove rollback, retry, and incompatible-binary refusal

<!-- lane C2, bead wamn-16jwe.2 -->

## Part 2: lifecycle verbs

The lifecycle has one advertised deployment owner but three production orchestration paths. The largest risks are an implicit uninstall during `platform apply` and package mutations outside the environment lock.

Source reviewed at `8c831349d3116aac27d9ca6f59a499da1c87efc7`. No files changed. No builds or tests ran. Test cases named below are proposed validation. Shared measurements report 80,688 source lines in `wamn-control` and 1,033 duplicated lines within that crate (measurements.md section 2 and section 7). The overlap below concerns behavior, beyond text duplication.

| Verb | Reads | Locks | Writes | External programs | Retry model |
| --- | --- | --- | --- | --- | --- |
| `env apply` | Environment document, registry policy and instance, OCI release and qualification, package artifacts, installed lineage, Helm history, pods | System-database advisory lock for the coordinate throughout the verb. Package phases also take transaction locks. | Environment substrate, credentials, platform schema, packages, grants, release cache, bindings, sequences, Helm release, floors | `kcl`, `kubectl`, `helm` | Mostly observation-based. Version and Helm-history shortcuts leave gaps. |
| `platform apply` | Cluster and environment documents, companion environment JSON, repository artifacts, stamped chart, Secrets, completion ConfigMaps | Local install-file lock. Its `env apply` Jobs separately take environment locks. | Platform resources, provisioning records, package Jobs, published releases, environment releases, CDC readers, identity, web and edge resources | Provider tools including `gcloud` or `kind`, `kubectl`, `helm`, `docker`, `pnpm`, build tools, Job-side `wamn-ctl` | Mixed resource comparison and historical completion markers. |
| `upgrade-environment` | Commit checkout, companion environment JSON, local run record, registry, database state, Kubernetes state, retained qualification file | No encompassing environment lock. Only its later `apply_release` call takes one. | Worktree, builds, images, platform migrations, package transitions, grants, credentials, release publication, environment deployment, retirement | `git`, `cargo`, `docker`, `wash`, `gcloud`, `kubectl`, helper scripts, child `wamn-ctl`, scenario worker, qualification copy tools | Finished stages are skipped from the run record. Credential creation and cleanup are not safe at every interruption point. |
| `apply-package` | Verified package, migration records, lineage, ownership, privileges, supplied or accepted upgrade evidence | Data-access and package-lineage transaction locks. No environment lifecycle lock. Backfills reacquire locks per batch. | Application migrations, package records, ownership, generated history objects, requirements, stage progress and accepted evidence. Also installs platform extensions. | None directly in the core applier | Exact migration retry is transactional. Backfills retain their cursor. Derived objects receive observation-based reconciliation when the applier runs. |
| `qualify-upgrade` | Source snapshot, predecessor release cache, installed roots, privileges, candidate artifacts and SQL | Read-only repeatable-read source transaction. Scratch application takes its normal transaction locks. | Private database, dump and new canonical result file. No source-database writes. | `pg_dump`, `pg_restore`, private PostgreSQL processes | A new qualification attempt, not an ensure. An existing result path refuses. |
| `upgrade-schema` | Embedded migration list, recorded paths and hashes, project registration | Exclusive record-table lock when the migration list is nonempty. No lifecycle lock. | Pending platform SQL and migration records. Planning rolls back. | None | Transactional exact retry. Current system and project lists are empty, so schema application returns without reconciliation. |
| `env rollback` | Successful Helm history and synthesized environment document | Same coordinate lifecycle lock | Calls the same apply path with a previous digest | Apply’s programs | Reuses apply behavior. |
| `env delete` | Environment instance, release, runs, credentials and resources | Same coordinate lifecycle lock | Removes runtime and control resources. `--data` also drops data and instance roles. | `kubectl`, `helm` | Presence-based destructive steps. |
| `env show`, `env status`, `env apply --dry-run` | Authorities and current observations | No lifecycle lock | No intended authority writes | `kubectl`, `helm`, and `kcl` for document compilation | Observations, not deployment completion guarantees. |

The main duplication is package application, data-access reconciliation and replica-identity reconciliation before a later `env apply` repeats related work. Platform-schema application occurs in `upgrade-environment::schema`, environment substrate setup and the package stage. Sources: `crates/control/lib/src/upgrade_environment.rs:529`, `crates/control/lib/src/upgrade_environment.rs:783`, `crates/control/lib/src/platform/packages.rs:179`, `crates/control/lib/src/environment/ensure.rs:138`, and `crates/control/lib/src/environment/stage.rs:51`.

## Finding C2-1: Platform setup can uninstall a serving environment

Severity: high  
Effort: M  
Area: `crates/control/lib/src/platform/environments.rs`  
Evidence: `crates/control/lib/src/platform/install.rs:72`, `crates/control/lib/src/platform/environments.rs:22`, `crates/control/lib/src/platform/environments.rs:45`, `crates/control/lib/src/platform/job.rs:643`, `crates/control/lib/src/environment/mod.rs:296`.

Problem: Every platform application schedules an initial environment application whose release is forcibly set to `none`. Its completion digest includes the runner image and chart files, so changed platform inputs can rerun that initial application against an existing environment. The ordinary `release = none` path uninstalls its chart before later publication and qualification steps finish.

Proposal: Platform installation provisions shared infrastructure and calls the environment owner with the actual declared release. Substrate creation belongs inside that application and never requires a synthetic uninstall document.

Deletes: The separate initial-apply Job, forced `DeclaredRelease::None`, and first-versus-final environment application split.

Risk: Fresh installation still needs substrate before publication. Extend `platform_apply_kind` with a serving environment, changed runner/chart inputs and an unavailable candidate qualification. The predecessor must keep serving.

## Finding C2-2: Production package changes escape the lifecycle lock

Severity: high  
Effort: L  
Area: `crates/control/lib/src/upgrade_environment.rs`, `crates/control/lib/src/apply_package.rs`  
Evidence: `crates/control/lib/src/upgrade_environment.rs:108`, `crates/control/lib/src/upgrade_environment.rs:783`, `crates/control/lib/src/upgrade_environment.rs:984`, `crates/control/lib/src/platform/packages.rs:263`, `crates/control/lib/src/apply_package.rs:399`, `crates/control/lib/src/package_upgrade.rs:389`.

Problem: `upgrade-environment` changes schema and packages before its deployment stage acquires the lifecycle lock. Platform package Jobs and standalone `apply-package` also use only database transaction locks. The production evidence reader assumes that apply holds the lifecycle lock, but these callers do not establish that assumption. A concurrent rollback or delete can therefore interleave with package transition work.

Proposal: One environment operation owns production transitions from predecessor observation through drain. Package executors become internal phases under that operation. Qualification remains a separate source-read operation, with predecessor state rechecked under the lifecycle lock before mutation.

Deletes: Independent production package coordination in `upgrade-environment` and `platform/packages.rs`, plus public mutation paths that bypass the environment owner.

Risk: Preserve atomic base-plus-overlay application and backfill resumption. Add a database-backed contention test that holds the lifecycle lock and attempts each production entry point. Every competing mutation must refuse before writing.

## Finding C2-3: Env apply cannot initiate an installed-package upgrade

Severity: high  
Effort: L  
Area: `crates/control/lib/src/environment/analyse.rs`, `crates/control/lib/src/environment/stage.rs`  
Evidence: `crates/control/lib/src/environment/analyse.rs:415`, `crates/control/lib/src/environment/analyse.rs:730`, `crates/control/lib/src/environment/stage.rs:113`, `crates/control/lib/src/environment/stage.rs:122`, `crates/control/lib/src/package_upgrade.rs:352`, `crates/control/lib/tests/environment_apply_kind.rs:82`.

Problem: Analysis accepts a successor only when its lineage and accepted evidence already exist in the project database. The stage then tries to apply a successor whose recorded predecessor equals the current lineage leaf, although recording that successor makes the predecessor cease to be the leaf. It also calls the unqualified applier, which refuses pending successor migrations. Fresh installation works, but an installed upgrade needs another path to mutate the database first.

Proposal: Give the environment owner one executable transition plan containing the candidate artifacts and exact qualification evidence. Analysis establishes admissibility without requiring installation first. The package phase applies that plan atomically where required.

Deletes: The unreachable staged-successor branch and duplicated prerequisite package application in the older orchestrators.

Risk: Test additive upgrade, base-plus-overlay upgrade and interrupted backfill through the public `env apply` path. The current environment fixture explicitly lacks a staged-package upgrade.

## Finding C2-4: Completion markers replace current-state observation

Severity: medium  
Effort: M  
Area: `crates/control/lib/src/platform/job.rs`, `crates/control/lib/src/delivery/run_record.rs`  
Evidence: `crates/control/lib/src/platform/job.rs:424`, `crates/control/lib/src/platform/job.rs:681`, `crates/control/lib/src/platform/environments.rs:414`, `crates/control/lib/src/delivery/run_record.rs:155`, `crates/control/lib/src/upgrade_environment.rs:82`.

Problem: Platform Jobs skip when a stored input digest matches, without observing their current postconditions. Environment Jobs use this default, so a matching marker can prevent `env apply` from repairing a deleted resource. The upgrade run record similarly skips every previously finished stage, even after external state changes. These are historical checkpoints, not observation-judged ensures.

Proposal: Retain caching for immutable artifacts and qualification results. Always invoke reconciliation for mutable environment state, with the owning phase deciding whether work remains.

Deletes: Completion-marker gating around environment reconciliation and finished-stage gating around mutable upgrade phases.

Risk: Add cases that retain completion records, remove an owned resource and rerun unchanged inputs. The operation must restore the resource or report its current refusal.

## Finding C2-5: Helm history equality bypasses recovery and drift repair

Severity: medium  
Effort: M  
Area: `crates/control/lib/src/environment/write.rs`  
Evidence: `crates/control/lib/src/environment/analyse.rs:592`, `crates/control/lib/src/environment/write.rs:92`, `crates/control/lib/src/environment/write.rs:102`, `crates/control/lib/src/environment/write.rs:114`, `crates/control/lib/src/release_chart.rs:604`.

Problem: The write phase skips Helm when desired values equal the newest successful historical revision. That return occurs before pending-operation recovery. A later pending revision can remain stuck, and changed or missing live resources receive no Helm repair. Readiness and drain can report the resulting mismatch, but repeated apply repeats the same shortcut.

Proposal: Resolve the current Helm operation before considering a skip. Skip the write only when current release status and owned resources satisfy the desired deployment.

Deletes: The historical-values-only early return and separate interpretations of successful history versus current deployment state.

Risk: Extend `environment_apply_kind` with an interrupted upgrade followed by apply of the last successful document. Also remove an owned Service while retaining Helm history. Both reruns must converge.

## Finding C2-6: Commit selection does not select the schema executor

Severity: medium  
Effort: M  
Area: `crates/control/lib/src/upgrade_environment.rs`  
Evidence: `crates/control/lib/src/upgrade_environment.rs:294`, `crates/control/lib/src/upgrade_environment.rs:320`, `crates/control/lib/src/upgrade_environment.rs:535`, `crates/control/lib/src/upgrade_environment.rs:917`, `crates/control/provision/src/schema_migrations.rs:49`.

Problem: The command checks out and builds the requested commit, but schema application calls the launching process’s linked library. Package application and deployment also call that process’s implementations, while release qualification invokes the newly built binary. The resulting run mixes executor versions despite its commit-addressed identity.

Proposal: Remove production schema and package stages from the source-upgrade orchestrator. Execute deployment through one artifact-bound control runner, whose schema declarations, package executor and chart belong to the selected platform.

Deletes: Direct schema/package/deployment library calls from the commit-build coordinator and their duplicate stage records.

Risk: Add a subprocess test with distinguishable launcher and target executors. The selected target must supply every production executor. Current empty migration lists do not exercise this distinction.

## Finding C2-7: Credential cleanup creates an unrecoverable retry window

Severity: medium  
Effort: M  
Area: `crates/control/lib/src/upgrade_environment.rs`  
Evidence: `crates/control/lib/src/upgrade_environment.rs:85`, `crates/control/lib/src/upgrade_environment.rs:1055`, `crates/control/lib/src/upgrade_environment.rs:1195`, `crates/control/lib/src/upgrade_environment.rs:1285`, `crates/control/lib/src/upgrade_environment.rs:1321`.

Problem: Retirement reads private credential files before determining which generations still need retirement. It deletes those files before the caller records stage completion. An interruption between deletion and record completion leaves a retry that requires missing files. Earlier package-stage retries also select another generation and mint another PAT instead of reusing a fixed operation identity.

Proposal: Reuse credentials owned by the existing environment and publication paths. Where private outputs remain necessary, derive retry decisions from durable state and perform disposable-file cleanup after completion is durable.

Deletes: The upgrade command’s separate generation-selection and PAT-minting workflow where existing owners already supply those credentials.

Risk: Add interruption tests after credential preparation, retirement and private-file deletion. Each retry must finish without replacing an unrelated active generation or requiring deleted output.

## Finding C2-8: The declared contract-floor path has no executor

Severity: medium  
Effort: L  
Area: `crates/control/lib/src/environment/contract.rs`, `crates/control/lib/src/qualify_upgrade/stage.rs`  
Evidence: `crates/control/lib/src/environment/contract.rs:106`, `crates/control/lib/src/environment/contract.rs:147`, `crates/control/lib/src/environment/contract.rs:159`, `crates/control/lib/src/apply_package.rs:179`, `crates/control/lib/src/qualify_upgrade/stage.rs:32`, `docs/plan/platform-deploy.md:355`.

Problem: The floor path specifically requires a contract-phase package and passes it to package application. Package preparation admits only expand and backfill stages, so this path always refuses before contract execution. The planned path also separates package application from floor recording into different transactions, contrary to the package-upgrade contract.

Proposal: One qualified contract executor owns selected-release SQL validation, schema contraction and floor advancement in one database transaction. The environment owner establishes the host/run condition and calls that executor after drain.

Deletes: The separate `apply_contract` followed by `record_floor` orchestration and duplicated contract admission decisions.

Risk: Add tests for selected-release SQL failure, older consumers, interruption before commit and rollback below the floor. Schema changes and floor advancement must commit together.

## Finding C2-9: Env apply skips reconciliation owned by the package applier

Severity: medium  
Effort: M  
Area: `crates/control/lib/src/environment/stage.rs`, `crates/control/lib/src/apply_package/record_history.rs`  
Evidence: `crates/control/lib/src/environment/stage.rs:113`, `crates/control/lib/src/apply_package.rs:768`, `crates/control/lib/src/apply_package/record_history.rs:352`, `crates/control/lib/src/apply_package/record_history.rs:429`.

Problem: The package applier observes and repairs generated history functions and triggers even when migration application is an exact retry. Env apply skips that applier when the package version is current. It therefore repairs grants through its separate reconciliation call but leaves other package-derived objects unrepaired.

Proposal: Separate immutable migration advancement from reconciliation of package-derived objects. The environment package phase invokes the same reconciliation owner for current versions without reexecuting migrations.

Deletes: The version-only skip around all package work and duplicate orchestration of selected derived objects.

Risk: Remove a generated history trigger, then apply the unchanged environment document. The trigger must return, application rows must remain unchanged and no migration record must advance.

## Finding C2-10: Platform-schema ownership is scattered across lifecycle phases

Severity: low  
Effort: M  
Area: `crates/control/lib/src/environment/ensure.rs`, `crates/control/lib/src/environment/stage.rs`, `crates/control/lib/src/apply_package.rs`  
Evidence: `crates/control/lib/src/environment/ensure.rs:123`, `crates/control/lib/src/environment/ensure.rs:138`, `crates/control/lib/src/environment/stage.rs:51`, `crates/control/lib/src/apply_package.rs:709`, `crates/control/provision/src/schema_migrations.rs:92`, `docs/plan/package-upgrade.md:15`.

Problem: Substrate setup reconciles the run plane and applies project migrations, then the package stage applies those migrations again. The application-package applier separately installs platform extensions. The plan treats platform schema as an upgrade prerequisite, while implementation distributes that responsibility among substrate, package and standalone schema operations. Empty migration lists currently hide much of the repeated work.

Proposal: One platform-schema phase owns full-schema installation, extension prerequisites and future numbered upgrades. Run it once before package work. Application-package execution owns application declarations and their derived objects.

Deletes: The second project-upgrade invocation, platform-extension installation inside application application, and repeated prerequisite wiring in older orchestrators.

Risk: Test fresh installation and a supplied nonempty migration list through the shared executor. Platform prerequisites must exist before package DDL, and an exact retry must execute no migration twice.

## Open questions for the owner

1. Can `upgrade-environment` lose its production schema, package and credential stages after existing delivery commands absorb its preparation work?
2. Must contract floors become executable in the next lifecycle consolidation, or remain explicitly unsupported until the contract executor lands?
3. Can standalone production `apply-package` become an internal environment phase, with a retained public command limited to explicitly owned disposable targets?

## Proposed fix epics

### One production lifecycle owner

- Remove synthetic bootstrap uninstalls and duplicate package Jobs
- Apply qualified package transitions under the environment lock
- Remove production mutation stages from upgrade-environment

### Observation-based recovery

- Replace mutable completion shortcuts with current-state reconciliation
- Repair pending Helm operations and live deployment drift
- Remove credential retry gaps and reconcile current package objects

### One schema and contract boundary

- Consolidate platform-schema prerequisites under one executor
- Commit qualified contract changes and floors atomically
- Align lifecycle documentation and public verbs with implemented ownership

<!-- lane C3, bead wamn-16jwe.3 -->

## Part 3: target model

Use `cluster.k` and its `environment.k` documents as one desired-state tree. One reconciler moves installed state toward that tree. Versioned migrations provide the only schema path.

Greenfield assumption: existing `wamn-dev` state can be dropped and reinstalled. The replacement starts with initial migrations and carries no legacy conversion path. This proposal accepts maintenance windows during upgrades.

This lane changed no files and ran no builds or tests. Bead statuses below come from the checkout export, not the live database.

## Finding C3-1: Desired state still depends on checkout and local build inputs

Severity: high  
Effort: L  
Area: `crates/control/lib/src/platform/`, `crates/control/lib/src/environment/`

Evidence: `crates/control/lib/src/platform/document.rs:18` declares no platform artifact pin. `crates/control/lib/src/environment/mod.rs:118` reads the chart from `WAMN_RELEASE_CHART`. `crates/control/lib/src/platform/packages.rs:26` reads package roots from an adjacent JSON file. `deploy/gcp/environments/dkk--receiving--dev.k:8` pins a release, while its JSON counterpart supplies separate deployment inputs at `deploy/gcp/environments/dkk--receiving--dev.json:2`.

Problem: The documents do not fully determine the requested deployment. The selected checkout, JSON files, and local chart directory contribute additional desired state. Reapplying an unchanged document therefore depends on inputs outside that document.

Proposal: `cluster.k` pins one immutable platform bundle by digest. That bundle contains charts, all platform image digests, the migration runner, and the system and project migration streams. Each `environment.k` pins its application release digest and declares environment configuration. The release supplies the exact package artifact closure, including base and overlay pins. Package versions and schema targets derive from those artifacts.

Credentials remain external references. Kubernetes, Helm, and PostgreSQL hold observed state. They do not independently select the desired release. Build and publication commands produce artifacts before application and never mutate the destination environment.

Deletes: Delete `deploy/gcp/environments/dkk--receiving--dev.json`, `deploy/gcp/environments/dkk--wms--dev.json`, and `crates/control/lib/src/delivery/environment.rs`. Delete deployment-time package discovery and builds from `platform/build.rs`, `platform/packages.rs`, and `platform/releases.rs`. Remove the local chart directory as an independent deployment selector.

Risk: Deployment loses its dependency on source directories and local build output. Extend `platform_apply_kind` to install from pinned artifacts without either input. An identical second apply must produce no deployment or migration writes.

## Finding C3-2: Fresh installation and reconciliation bypass versioned migrations

Severity: high  
Effort: L  
Area: `crates/control/provision/src/schema_migrations.rs`, `crates/schema/control/src/run_plane/`

Evidence: `crates/control/provision/src/schema_migrations.rs:93` and `:96` define empty platform migration lists. `crates/control/lib/src/reconcile_run_plane.rs:639` installs the full application schema, then records migrations without executing them. The same driver executes generated repair SQL at `crates/control/lib/src/reconcile_run_plane.rs:879`. `crates/schema/control/src/run_plane/schema.rs:159` parses mutable SQL declarations into repair sections.

Problem: Platform installation, platform upgrades, and schema repairs follow different rules. The migration records cannot describe changes that another path applies directly. This is deliberate current behavior, documented at `docs/architecture/overview.md:66`, rather than an accidental omission.

Proposal: Use one migration executor for system, project, and package streams. Each stream records its owner, ordered files, and exact checksums. Empty databases execute initial migrations through that executor. Installed databases execute only the pending suffix.

Create the system database first and execute its stream. For each project database, execute its platform suffix and dependency-ordered package suffixes within the owning transaction. Commit migration records and installed package facts with their changes. Preserve atomic application of bases and affected overlays.

Structural drift refuses with the affected object and owning migration. Repair requires another versioned migration. Expected schema facts derive from replaying the selected streams.

Deletes: Move definitions from `deploy/sql/system-schema.sql`, `app-schema.sql`, `catalog-schema.sql`, `run-state.sql`, and `run-queue.sql` into initial migrations. Delete their independent installation and repair paths. Delete the DDL planner in `crates/schema/control/src/run_plane/plan.rs` and its SQL-section parser. Retain useful observation code. Remove `--baseline`, legacy schema probes, and record-without-execution behavior from `upgrade_schema.rs`.

Risk: Missing grants, policies, or initialization order can break a fresh installation. Compare fresh installation with predecessor-to-target migration, including effective privileges under actual service roles. Retain the atomicity assertion in `qualify_upgrade/tests/overlay.rs:753`, which requires a second package failure to preserve the original state.

## Finding C3-3: Multiple upgrade drivers own ordering, locking, and recovery

Severity: high  
Effort: L  
Area: `crates/control/lib/src/upgrade_environment.rs`, `crates/control/lib/src/platform/install.rs`, `crates/control/lib/src/environment/mod.rs`

Evidence: `upgrade_environment.rs:109` defines an eleven-stage driver. `platform/install.rs:51` implements another sequence, including package installation before qualification at `:80` and `:122`. `environment/mod.rs:271` implements the environment sequence. `delivery/run_record.rs:155` resumes from completed local stages. `platform/lock.rs:32` uses a local file lock, while `environment/lock.rs:22` uses a database advisory lock.

Problem: The drivers disagree about the unit of work and the evidence that permits progress. A completed local stage differs from an observation that the destination still matches. The local installer lock also cannot coordinate callers on different machines.

Proposal: Keep `platform apply` and `env apply` as scope selectors for one reconciler. Use one cluster-scoped Kubernetes Lease to coordinate callers, including first installation. Database transactions retain their own migration locks. Lease loss stops further effects.

The reconciler resolves artifacts, observes installed facts, and requires qualification before destination schema changes. It then stops admission and automation, drains affected work, executes migrations, deploys the selected artifacts, and waits for readiness. It reopens admission only after the affected scope reaches its target.

The three upgrade flows use that sequence:

1. A platform image upgrade changes the bundle digest in `cluster.k`. Application release digests remain unchanged.
2. A package upgrade publishes a release with the complete successor package closure, then changes the digest in `environment.k`.
3. A schema upgrade appends migrations and publishes the owning platform bundle or package release. Runtime images can remain unchanged.

A retry observes migration records and live resources again. Logs explain execution but never authorize skipping a step. Cross-database application is not atomic. After partial progress, affected services remain paused until reconciliation succeeds.

Deletes: Delete `upgrade_environment.rs` and its sole-purpose `delivery/run_record.rs`. Delete the public `upgrade-environment` verb. Replace the numbered installer driver with shared reconciliation. Remove nested lifecycle wrappers from `platform/environments.rs` and direct installation from `platform/packages.rs`. Make `upgrade-schema`, `apply-package`, and `reconcile-run-plane` internal operations rather than alternative deployment entry points.

Risk: A crash between database commits and rollout can leave a partly upgraded cluster. Extend `platform_apply_kind` and `environment_apply_kind` with interruption, retry, and competing-caller cases. Require one migration application and no admission before readiness.

## Finding C3-4: Qualification covers separate slices instead of the complete transition

Severity: high  
Effort: L  
Area: `crates/control/lib/src/delivery/qualification_artifact.rs`, `crates/control/lib/src/qualify_upgrade/`

Evidence: `delivery/qualification_artifact.rs:57` records a release digest, three runtime image digests, and case names. `release_chart.rs:54` explicitly excludes runner, gates, and identity images from that key. `qualify_upgrade/mod.rs:104` defines separate package-transition evidence. `docs/operations/deployment.md:433` describes database qualification and release qualification as distinct requirements.

Problem: Neither result identifies the complete transition across platform schema, packages, shared services, and deployment artifacts. A schema-only change can leave the existing release qualification key unchanged. The split also requires separate application boundaries to combine the results correctly.

Proposal: Qualification means successful execution of the resolved transition on disposable targets through the same reconciler. One qualification entry point produces one immutable result for that transition.

The result identifies predecessor and target platform bundles, release and package digests, migration checksums, expected schema and privilege facts, and the test-suite digest. Empty installation is an explicit predecessor. Package transitions exercise copied predecessor data and ownership. Platform transitions exercise every affected release against the selected shared services.

Qualification includes migration failure and retry behavior, retained data, authority boundaries, startup, and application cases. Application requires matching artifact identities and migration history. It also evaluates migration preconditions against current data under the migration locks.

Qualification does not promise that arbitrary future data satisfies a migration. Reuse requires identical transition inputs and test definitions.

Deletes: Merge `qualify_upgrade/` and `delivery/qualification.rs` orchestration around the shared executor. Delete the separate `qualify-upgrade` workflow and its operator-supplied result-file handoff. Replace the narrow qualification key and package-specific qualification storage with one transition result and references from applied facts.

Risk: Incorrect reuse can admit an untested migration or identity image. Add cases where only either input changes and require refusal of the old result. Preserve source-isolation assertions from `qualify_upgrade/tests.rs:334`. Preserve complete-overlay and atomicity assertions from `qualify_upgrade/tests/overlay.rs:781`.

## Finding C3-5: Maintenance upgrades remove the need for online stages and inferred rollback

Severity: medium  
Effort: L  
Area: `crates/schema/generator/src/upgrade_stage.rs`, `crates/control/lib/src/package_upgrade/`, `crates/control/lib/src/environment/`

Evidence: `upgrade_stage.rs:16` defines separate expand, backfill, and contract declarations. `deploy/sql/catalog-schema.sql:429` stores batch progress, and `:491` stores package floors. `environment/contract.rs:107` can install another package version after deployment. `environment/rollback.rs:38` chooses its target from Helm history. Measurements report 195 duplicated lines in `package_upgrade/progress.rs`, in measurements.md section 7.

Problem: Online coexistence requires additional versions, progress state, compatibility tests, and rollback rules. Floors can request schema work beyond the release selected by the environment document. Helm history selects only an application digest, rather than a complete previously qualified platform and schema combination.

Proposal: Use the maintenance model from Finding C3-3. Drain all affected writers before migration, including shared services when their schema changes. Refuse a transition with stranded work. Apply each database transition transactionally and retain tenant identity, ownership, and authority boundaries.

Recovery proceeds forward from committed migration records. Returning to earlier code requires an explicit document change and qualification against the schema that remains installed. Schema reversal requires an authored migration or an explicit backup restoration.

Deletes: Delete `crates/schema/generator/src/upgrade_stage.rs`, `package_upgrade/progress.rs`, and `qualify_upgrade/stage.rs`. Remove online-stage branches from package application. Delete `catalog.package_upgrade_stages`, `catalog.package_floors`, the document’s `floors` field, and floor application from `environment/contract.rs`. Retain obsolete-binding cleanup. Delete history-derived rollback in `environment/rollback.rs`. Delete tests whose sole subject is online coexistence, while retaining data-preservation and authority assertions.

Risk: Upgrades interrupt service, and large transactional migrations can exceed the maintenance window. Test that active work blocks migration, failed SQL preserves rows, retries resume correctly, and incompatible earlier releases refuse. Uninterrupted upgrades require a different owner decision before these deletions.

The following exported open beads lose their current implementation premise:

- `wamn-qpgq`: historical cutovers no longer need migration ports because the initial installation replaces existing state. Migration-only ownership remains acceptance for Finding C3-2. Evidence: `.beads/issues.jsonl:3469`.
- `wamn-eycl`: the separate `environment.upgrade` saga disappears. The UI instead submits desired state through the same reconciler. UI support remains required. Evidence: `.beads/issues.jsonl:3458`.
- `wamn-ev5g6`: its equal-deadline premise already disappears in current source. `platform/job.rs:15` allows 900 seconds around the 600-second readiness budget. Evidence: `.beads/issues.jsonl:1919`.

These are proposed dispositions, not closures. Root must read live Beads before disposition and cite the relevant implementation commit. Qualification transport and teardown findings `wamn-byw08`, `wamn-uvqx9`, and `wamn-dxcpf` remain relevant.

## Open questions for the owner

1. Can upgrades use maintenance windows, as proposed, or must service remain available throughout schema changes?

## Proposed fix epics

### Desired state and one reconciler

1. Pin platform bundles and remove environment JSON inputs
2. Replace upgrade drivers with one reconciliation sequence
3. Exercise interruption, retry, and competing callers

### Versioned migrations as the only schema path

1. Replace full-schema installation with initial migrations
2. Unify platform and package migration execution
3. Remove schema repair DDL and exercise authority preservation

### Qualification and maintenance recovery

1. Qualify complete transitions through the shared reconciler
2. Remove online stages, floors, and inferred rollback
3. Connect the platform UI to desired-state application
