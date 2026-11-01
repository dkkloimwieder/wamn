# Audit report C: Update and upgrade

Three read-only Codex gpt-6.1-sol runs at reasoning effort high wrote this file, one per part, on 2026-10-31 against commit 8c831349d. Root copied each run's final message and checked that every finding cites a file and line. Root then removed each part's proposed-epics section, because README.md holds the only plan, and marked owner review edits inline with the date. Beads: wamn-16jwe.1, wamn-16jwe.2, wamn-16jwe.3. Shared tool output: [measurements.md](measurements.md).

<!-- report C1, bead wamn-16jwe.1 -->

## Part 1: schema

Audited checkout `8c831349d`. No files or issue statuses changed. No builds or tests ran. Bead descriptions came from local Dolt backups because `bd --readonly show` failed on a writable `LOCK` requirement. Current tracker status remains unverified.

## Finding C1-1: Schema truth describes fresh installations but cannot advance installed databases

Severity: high  
Effort: L  
Area: deploy/sql, crates/control/provision/src/schema_migrations.rs  
Evidence: `crates/control/provision/src/schema_migrations.rs:3`, `crates/control/provision/src/schema_migrations.rs:92`, `crates/control/lib/src/upgrade_schema.rs:127`, `deploy/sql/system-schema.sql:174`, `deploy/sql/system-schema.sql:210`, `deploy/sql/postgres-init.sql:1`.

Problem: The full SQL files define the intended schema, while each installed database holds its actual layout. `registry.meta.schema_version = '0.1'` identifies the registry format, not the precise installed DDL. Both migration lists are empty, and `upgrade-schema` returns success without examining the database. `postgres-init.sql` builds test fixtures and supplies no production upgrade path.

Rejected (owner review 2026-10-31, see README). Standing rule for this part: during development the schema is one baseline, the full SQL files. Real migrations return when development is stable. No-cruft applies: machinery that serves only the deferred path is deleted now, not kept empty.

Deletes (inverted): the full-schema installation path stays. Delete the migration machinery instead: `crates/control/provision/src/schema_migrations.rs` (both empty lists), `crates/control/lib/src/upgrade_schema.rs` and the `upgrade-schema` verb in `services/ctl/src/provisioning_verbs.rs`, `record_fresh_install` and its three callers, the migration record table `platform.schema_migrations` in `deploy/sql/app-schema-migrations.sql`, and their tests. The reviewer named `system-schema.sql` for the record table. The table is in `app-schema-migrations.sql`, which `schema_migrations.rs` includes.

Risk: Fresh installation must still produce the full schema, grants and seed data. The existing installation tests prove that after the deletion.

## Finding C1-2: Installer completion records substitute for database upgrade state

Severity: high  
Effort: M  
Area: crates/control/lib/src/platform  
Evidence: `crates/control/lib/src/platform/install.rs:309`, `crates/control/lib/src/platform/install.rs:360`, `crates/control/lib/src/platform/install.rs:405`, `crates/control/lib/src/platform/job.rs:643`, `crates/control/lib/src/platform/environments.rs:46`, `crates/control/lib/src/environment/mod.rs:296`.

Problem: `wamn-bj2kn` and `wamn-sdrnq` share one cause: newer code reaches older schemas after step 4.7 skips the completed bootstrap. Their reported symptoms differ: a missing connection-reader function and a missing `env_key` column. `wamn-zeg0z` exposes the opposite cache rule: a changed image reruns first apply with `release = none`, which requests removal of the serving release. These failures share an installer that models completed steps without a database upgrade lifecycle.

Proposal (owner review 2026-10-31): the three failures (`wamn-bj2kn`, `wamn-sdrnq`, `wamn-zeg0z`) are real, and the original proposal is not. The baseline SQL carries a content hash. Install records it. Apply compares the installed hash with the candidate's hash and refuses with "schema changed, reset the environment database" when they differ. No pending-work record. Because there is one baseline, the hash check runs first, before analysis (C1-3 closes into this finding).

Deletes: The installed-environment replay of first apply with `release = none` and completion-marker decisions about schema readiness (shared with C2-1). From C1-3, widened: every project migration call, not only the duplicates in substrate ensure and package staging.

Risk: An image change against a database with the same hash must keep the serving release installed while requests continue. An image change against a database with a different hash must refuse with the message above.

Owner review 2026-10-31 against origin/main 64dcf04: commit 8a3b3cc (wamn-zeg0z) is on main.

## Finding C1-3: Project migrations run after queries that already require the new schema

Severity: high  
Effort: M  
Area: crates/control/lib/src/environment  
Evidence: `crates/control/lib/src/environment/mod.rs:276`, `crates/control/lib/src/environment/analyse.rs:400`, `crates/control/lib/src/environment/ensure.rs:138`, `crates/control/lib/src/environment/stage.rs:51`.

Problem: Environment apply reads installed packages during analysis before it reaches either project migration call. Those reads already require columns such as `env_key`. Restoring migration files alone cannot repair a predecessor database when analysis fails before the applier runs.

Closed into C1-2 (owner review 2026-10-31). With one baseline there is nothing to advance before analysis. The hash check of C1-2 runs first. The Deletes stay, widened: every project migration call, not only the duplicates. They are listed under C1-2.

## Finding C1-4: A failed control bootstrap can leave an installation that refuses every retry

Severity: high  
Effort: M  
Area: crates/control/lib/src/provision_system.rs  
Evidence: `crates/control/lib/src/provision_system.rs:78`, `crates/control/lib/src/provision_system.rs:110`, `crates/control/lib/src/provision_system.rs:116`, `crates/control/lib/src/provision_system.rs:53`, `crates/control/provision/src/lib.rs:164`.

Problem: The core system schema and portable control store run as separate batches without an enclosing transaction. A failure after the core batch leaves `registry` present, but a retry refuses any database with that schema. Domain assignment also runs after bootstrap, so completion spans several database operations without one atomic boundary.

Proposal (owner review 2026-10-31, narrowed): bootstrap runs its batches in one transaction. A retry that finds `registry` without a completed bootstrap drops and recreates the system database, because a reset is free. No lock, no recorded stages. Baseline histories and refusal of altered or newer records are rejected with C1-1.

Deletes: The `registry` existence test as the installation-completion authority and the separate unrecorded bootstrap stages.

Risk: Inject a failure during the portable-store stage and before domain assignment, then prove that the retry recreates the database and completes.

## Finding C1-5: Run-plane repair is a partial second schema-upgrade engine

Severity: medium  
Effort: L  
Area: crates/schema/control/src/run_plane, crates/control/lib/src/reconcile_run_plane.rs  
Evidence: `crates/schema/control/src/run_plane/schema.rs:160`, `crates/schema/control/src/run_plane/plan.rs:337`, `crates/schema/control/src/run_plane/plan.rs:667`, `crates/control/lib/src/reconcile_run_plane.rs:505`, `crates/control/lib/src/reconcile_run_plane.rs:639`, `deploy/sql/catalog-schema.sql:215`. Related duplication: measurements.md section 7.

Problem: The reconciler parses SQL layout to create missing catalog tables and add missing run-plane columns. It does not provide equivalent evolution for existing catalog columns or the whole platform schema, whose presence is inferred from `platform.user`. The catalog file also creates `platform` relations outside the catalog-table loop. Repairing selected objects therefore cannot substitute for a complete upgrade.

Proposal (owner review 2026-10-31): deletion only. Reconciliation keeps grants, roles and policies, which must continuously converge. Structural repair goes. No migration path replaces it, because the schema is one baseline and a reset is free.

Deletes: Structural SQL slicing, missing-column mutation, and separate table-bootstrap actions in the run-plane planner and `reconcile_run_plane.rs`.

Risk: Removing repair can weaken security convergence. Keep the existing role, policy, and privilege-denial tests green.

## Finding C1-6: Retained migration records conflict with the promised restart at 0001

Severity: medium  
Effort: M  
Area: crates/control/lib/src/upgrade_schema.rs, crates/control/provision/src/schema_migrations.rs  
Evidence: `crates/control/provision/src/schema_migrations.rs:4`, `crates/control/lib/src/upgrade_schema.rs:187`, `crates/control/lib/src/upgrade_schema.rs:198`, `crates/control/lib/src/upgrade_schema.rs:361`, `crates/control/lib/src/upgrade_schema.rs:380`.

Problem: The code promises that migrations can restart at `0001`, but its test deliberately retains a retired `0001` record. An empty list ignores that record today. A new file at ordinal one later collides with the retained path or hash and produces `schema-migration-drift`.

Closed into C1-1 (owner review 2026-10-31). The retired-record test, the empty-list bypass and the adoption machinery go with the migration tables.

## Open questions for the owner

All three questions were answered on 2026-10-31. No installed database requires a data-preserving upgrade: every one can be reset to the baseline. Old and new images serve concurrently (rolling), and a schema-breaking change during development is a reset, never a maintenance interval. `wamn-m6or2` closes on the next green `environment_apply_kind` run on main, with no dedicated run.

<!-- report C2, bead wamn-16jwe.2 -->

## Part 2: lifecycle verbs

The lifecycle has one advertised deployment owner but three production orchestration paths. The largest risks are an implicit uninstall during `platform apply` and package mutations outside the environment lock.

Owner ruling 2026-10-31: one baseline covers application packages as well as the platform schema. Migrations are a future problem, because current greenfield work is high churn and replacement. A package's SQL is one baseline. A package schema change during development is a new release and a reset of that application's tables. There is no in-place package upgrade. The package-upgrade machinery is deleted under that ruling, about 9,000 lines in `crates/control/lib`: `crates/control/lib/src/package_upgrade.rs` and `package_upgrade/` (lineage, accepted evidence, progress), the expand, backfill and contract stages of `apply_package/` (`stage.rs`, `backfill.rs`, `migration_policy.rs`, `abandon.rs`), `qualify_upgrade/` and the `qualify-upgrade` verb, `environment/contract.rs`, the tables `catalog.package_upgrade_qualifications`, `catalog.package_upgrade_stages` and `catalog.package_floors` in `deploy/sql/catalog-schema.sql`, their tests, and `migrations/0002+` under each application (nine files), collapsed to one `0001` per package or the generator's full SQL.

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

Proposal (owner review 2026-10-31): `upgrade-environment` is deleted under C3-3. What remains is `platform/packages.rs` Jobs and the standalone `apply-package` verb. Package application becomes an internal phase of `env apply` under the environment lock. The public `apply-package` verb is deleted. `wamn dev` calls the library.

Deletes: Independent production package coordination in `platform/packages.rs`, the public `apply-package` verb in `services/ctl`, and every mutation path that bypasses the environment owner.

Risk: Add a database-backed contention test that holds the lifecycle lock and attempts package application through `env apply`. The competing caller must refuse before writing.

## Finding C2-3: Env apply cannot initiate an installed-package upgrade

Severity: high  
Effort: L  
Area: `crates/control/lib/src/environment/analyse.rs`, `crates/control/lib/src/environment/stage.rs`  
Evidence: `crates/control/lib/src/environment/analyse.rs:415`, `crates/control/lib/src/environment/analyse.rs:730`, `crates/control/lib/src/environment/stage.rs:113`, `crates/control/lib/src/environment/stage.rs:122`, `crates/control/lib/src/package_upgrade.rs:352`, `crates/control/lib/tests/environment_apply_kind.rs:82`.

Problem: Analysis accepts a successor only when its lineage and accepted evidence already exist in the project database. The stage then tries to apply a successor whose recorded predecessor equals the current lineage leaf, although recording that successor makes the predecessor cease to be the leaf. It also calls the unqualified applier, which refuses pending successor migrations. Fresh installation works, but an installed upgrade needs another path to mutate the database first.

Closed by the package-baseline deletion (owner review 2026-10-31). The unreachable staged-successor branch and the successor evidence path go with `package_upgrade.rs` and the apply-package stages. No additive-upgrade test is written.

## Finding C2-4: Completion markers replace current-state observation

Severity: medium  
Effort: M  
Area: `crates/control/lib/src/platform/job.rs`, `crates/control/lib/src/delivery/run_record.rs`  
Evidence: `crates/control/lib/src/platform/job.rs:424`, `crates/control/lib/src/platform/job.rs:681`, `crates/control/lib/src/platform/environments.rs:414`, `crates/control/lib/src/delivery/run_record.rs:155`, `crates/control/lib/src/upgrade_environment.rs:82`.

Problem: Platform Jobs skip when a stored input digest matches, without observing their current postconditions. Environment Jobs use this default, so a matching marker can prevent `env apply` from repairing a deleted resource. The upgrade run record similarly skips every previously finished stage, even after external state changes. These are historical checkpoints, not observation-judged ensures.

Proposal (owner review 2026-10-31, kept at medium): markers stay for immutable artifacts and qualification results only. Every mutable phase runs its reconciliation and decides itself whether work remains.

Deletes: Completion-marker gating around environment reconciliation and finished-stage gating around mutable upgrade phases.

Risk: Add cases that retain completion records, remove an owned resource and rerun unchanged inputs. The operation must restore the resource or report its current refusal.

## Finding C2-5: Helm history equality bypasses recovery and drift repair

Severity: medium  
Effort: M  
Area: `crates/control/lib/src/environment/write.rs`  
Evidence: `crates/control/lib/src/environment/analyse.rs:592`, `crates/control/lib/src/environment/write.rs:92`, `crates/control/lib/src/environment/write.rs:102`, `crates/control/lib/src/environment/write.rs:114`, `crates/control/lib/src/release_chart.rs:604`.

Problem: The write phase skips Helm when desired values equal the newest successful historical revision. That return occurs before pending-operation recovery. A later pending revision can remain stuck, and changed or missing live resources receive no Helm repair. Readiness and drain can report the resulting mismatch, but repeated apply repeats the same shortcut.

Proposal (owner review 2026-10-31, small): resolve the pending Helm operation first. Skip the write only when release status and owned resources match the desired deployment.

Deletes: The historical-values-only early return and separate interpretations of successful history versus current deployment state.

Risk: Extend `environment_apply_kind` with an interrupted upgrade followed by apply of the last successful document. Also remove an owned Service while retaining Helm history. Both reruns must converge.

## Finding C2-6: Commit selection does not select the schema executor

Severity: medium  
Effort: M  
Area: `crates/control/lib/src/upgrade_environment.rs`  
Evidence: `crates/control/lib/src/upgrade_environment.rs:294`, `crates/control/lib/src/upgrade_environment.rs:320`, `crates/control/lib/src/upgrade_environment.rs:535`, `crates/control/lib/src/upgrade_environment.rs:917`, `crates/control/provision/src/schema_migrations.rs:49`.

Problem: The command checks out and builds the requested commit, but schema application calls the launching process’s linked library. Package application and deployment also call that process’s implementations, while release qualification invokes the newly built binary. The resulting run mixes executor versions despite its commit-addressed identity.

Closed by C3-3 (owner review 2026-10-31): the commit-build coordinator `upgrade_environment.rs` is deleted, so no executor mix remains.

## Finding C2-7: Credential cleanup creates an unrecoverable retry window

Severity: medium  
Effort: M  
Area: `crates/control/lib/src/upgrade_environment.rs`  
Evidence: `crates/control/lib/src/upgrade_environment.rs:85`, `crates/control/lib/src/upgrade_environment.rs:1055`, `crates/control/lib/src/upgrade_environment.rs:1195`, `crates/control/lib/src/upgrade_environment.rs:1285`, `crates/control/lib/src/upgrade_environment.rs:1321`.

Problem: Retirement reads private credential files before determining which generations still need retirement. It deletes those files before the caller records stage completion. An interruption between deletion and record completion leaves a retry that requires missing files. Earlier package-stage retries also select another generation and mint another PAT instead of reusing a fixed operation identity.

Closed with C3-3 (owner addendum 2026-10-31): the upgrade path is deleted. What survives is the admin flow of [integration-schema](../plan/integration-schema.md) ("Admin flow (no deployment)"): a credential is set through the UI or API, a new generation becomes active, and nothing deploys.

## Finding C2-8: The declared contract-floor path has no executor

Severity: medium  
Effort: L  
Area: `crates/control/lib/src/environment/contract.rs`, `crates/control/lib/src/qualify_upgrade/stage.rs`  
Evidence: `crates/control/lib/src/environment/contract.rs:106`, `crates/control/lib/src/environment/contract.rs:147`, `crates/control/lib/src/environment/contract.rs:159`, `crates/control/lib/src/apply_package.rs:179`, `crates/control/lib/src/qualify_upgrade/stage.rs:32`, `docs/plan/platform-deploy.md:355`.

Problem: The floor path specifically requires a contract-phase package and passes it to package application. Package preparation admits only expand and backfill stages, so this path always refuses before contract execution. The planned path also separates package application from floor recording into different transactions, contrary to the package-upgrade contract.

Rejected (owner review 2026-10-31, see README): online stages and version minimums are not replaced by a maintenance window, and under the package-baseline ruling there is no contract phase at all. `environment/contract.rs` and the contract admission in `qualify_upgrade/stage.rs` go with the package-baseline deletion.

## Finding C2-9: Env apply skips reconciliation owned by the package applier

Severity: medium  
Effort: M  
Area: `crates/control/lib/src/environment/stage.rs`, `crates/control/lib/src/apply_package/record_history.rs`  
Evidence: `crates/control/lib/src/environment/stage.rs:113`, `crates/control/lib/src/apply_package.rs:768`, `crates/control/lib/src/apply_package/record_history.rs:352`, `crates/control/lib/src/apply_package/record_history.rs:429`.

Problem: The package applier observes and repairs generated history functions and triggers even when migration application is an exact retry. Env apply skips that applier when the package version is current. It therefore repairs grants through its separate reconciliation call but leaves other package-derived objects unrepaired.

Proposal (owner review 2026-10-31, small): derived-object reconciliation (history functions, triggers, grants) runs on every apply regardless of package version.

Deletes: The version-only skip around all package work.

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

Owner review 2026-10-31: In passing. The duplicate migration calls vanish under C1-1. What is left is moving platform-extension installation out of the application-package applier into substrate setup.

## Open questions for the owner

All three questions were answered on 2026-10-31. `upgrade-environment` is deleted whole, and nothing is absorbed. Contract version minimums are unsupported, and the contract-phase code is deleted. Package application is an internal phase of `env apply`, with no public command.

<!-- report C3, bead wamn-16jwe.3 -->

## Part 3: target model

Use `cluster.k` and its `environment.k` documents as one desired-state tree. One reconciler moves installed state toward that tree.

Owner review 2026-10-31: the two premises of this part, versioned migrations as the only schema path and maintenance windows during upgrades, are rejected (C3-2, C3-5, and the package-baseline ruling in Part 2). The schema is one baseline and a reset is free. The rolling drain stays and no writer pauses. C3-1 and C3-3 stay as Group 1 deletions, C3-4 closes, and the migration and maintenance sentences below are superseded where they appear.

This lane changed no files and ran no builds or tests. Bead statuses below come from the checkout export, not the live database.

## Finding C3-1: Desired state still depends on checkout and local build inputs

Severity: high  
Effort: L  
Area: `crates/control/lib/src/platform/`, `crates/control/lib/src/environment/`

Evidence: `crates/control/lib/src/platform/document.rs:18` declares no platform artifact pin. `crates/control/lib/src/environment/mod.rs:118` reads the chart from `WAMN_RELEASE_CHART`. `crates/control/lib/src/platform/packages.rs:26` reads package roots from an adjacent JSON file. `deploy/gcp/environments/dkk--receiving--dev.k:8` pins a release, while its JSON counterpart supplies separate deployment inputs at `deploy/gcp/environments/dkk--receiving--dev.json:2`.

Problem: The documents do not fully determine the requested deployment. The selected checkout, JSON files, and local chart directory contribute additional desired state. Reapplying an unchanged document therefore depends on inputs outside that document.

Proposal: `cluster.k` pins one immutable platform bundle by digest. That bundle contains charts, all platform image digests, and the package artifact closure. Each `environment.k` pins its application release digest and declares environment configuration. The release supplies the exact package artifact closure, including base and overlay pins. Package versions and schema targets derive from those artifacts.

Credentials remain external references. Kubernetes, Helm, and PostgreSQL hold observed state. They do not independently select the desired release. Build and publication commands produce artifacts before application and never mutate the destination environment.

Deletes: Delete `deploy/gcp/environments/dkk--receiving--dev.json`, `deploy/gcp/environments/dkk--wms--dev.json`, and `crates/control/lib/src/delivery/environment.rs`. Delete deployment-time package discovery and builds from `platform/build.rs`, `platform/packages.rs`, and `platform/releases.rs`. Remove the local chart directory as an independent deployment selector.

Risk: Deployment loses its dependency on source directories and local build output. Extend `platform_apply_kind` to install from pinned artifacts without either input. An identical second apply must produce no deployment writes.

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

Rejected (owner review 2026-10-31, see README): migration machinery. The schema is one baseline during development and a reset is free. The machinery itself is deleted under the inverted C1-1 Deletes.

## Finding C3-3: Multiple upgrade drivers own ordering, locking, and recovery

Severity: high  
Effort: L  
Area: `crates/control/lib/src/upgrade_environment.rs`, `crates/control/lib/src/platform/install.rs`, `crates/control/lib/src/environment/mod.rs`

Evidence: `upgrade_environment.rs:109` defines an eleven-stage driver. `platform/install.rs:51` implements another sequence, including package installation before qualification at `:80` and `:122`. `environment/mod.rs:271` implements the environment sequence. `delivery/run_record.rs:155` resumes from completed local stages. `platform/lock.rs:32` uses a local file lock, while `environment/lock.rs:22` uses a database advisory lock.

Problem: The drivers disagree about the unit of work and the evidence that permits progress. A completed local stage differs from an observation that the destination still matches. The local installer lock also cannot coordinate callers on different machines.

Proposal: Keep `platform apply` and `env apply` as scope selectors for one reconciler. Use one cluster-scoped Kubernetes Lease to coordinate callers, including first installation. Lease loss stops further effects.

The reconciler resolves artifacts, observes installed facts, and requires qualification before destination schema changes. It then stops admission and automation, drains affected work, executes migrations, deploys the selected artifacts, and waits for readiness. It reopens admission only after the affected scope reaches its target.

The three upgrade flows use that sequence:

1. A platform image upgrade changes the bundle digest in `cluster.k`. Application release digests remain unchanged.
2. A package upgrade publishes a release with the complete successor package closure, then changes the digest in `environment.k`.
3. A schema change during development is a new release plus a reset of the affected tables.

A retry observes migration records and live resources again. Logs explain execution but never authorize skipping a step. Cross-database application is not atomic. After partial progress, affected services remain paused until reconciliation succeeds.

Deletes: Delete `upgrade_environment.rs` and its sole-purpose `delivery/run_record.rs`. Delete the public `upgrade-environment` verb. Replace the numbered installer driver with shared reconciliation. Remove nested lifecycle wrappers from `platform/environments.rs` and direct installation from `platform/packages.rs`. Make `upgrade-schema`, `apply-package`, and `reconcile-run-plane` internal operations rather than alternative deployment entry points.

Risk: A crash between database commits and rollout can leave a partly upgraded cluster. Extend `platform_apply_kind` and `environment_apply_kind` with interruption, retry, and competing-caller cases. Require one migration application and no admission before readiness.

Owner review 2026-10-31: the deletions stay. The sentences about stopping admission, executing migrations and reopening admission are superseded. The rolling drain stays, no writer pauses, and there are no migrations to execute. The cluster-scoped Lease is D3-7.

## Finding C3-4: Qualification covers separate slices instead of the complete transition

Severity: high  
Effort: L  
Area: `crates/control/lib/src/delivery/qualification_artifact.rs`, `crates/control/lib/src/qualify_upgrade/`

Evidence: `delivery/qualification_artifact.rs:57` records a release digest, three runtime image digests, and case names. `release_chart.rs:54` explicitly excludes runner, gates, and identity images from that key. `qualify_upgrade/mod.rs:104` defines separate package-transition evidence. `docs/operations/deployment.md:433` describes database qualification and release qualification as distinct requirements.

Problem: Neither result identifies the complete transition across platform schema, packages, shared services, and deployment artifacts. A schema-only change can leave the existing release qualification key unchanged. The split also requires separate application boundaries to combine the results correctly.

Closed (owner review 2026-10-31): there is nothing to qualify. Package transitions do not exist under the package-baseline ruling, and R12 keeps the narrow key, release digest plus host image digest (D3-2). `qualify_upgrade/` and the `qualify-upgrade` verb go with the package-baseline deletion.

## Finding C3-5: Maintenance upgrades remove the need for online stages and inferred rollback

Severity: medium  
Effort: L  
Area: `crates/schema/generator/src/upgrade_stage.rs`, `crates/control/lib/src/package_upgrade/`, `crates/control/lib/src/environment/`

Evidence: `upgrade_stage.rs:16` defines separate expand, backfill, and contract declarations. `deploy/sql/catalog-schema.sql:429` stores batch progress, and `:491` stores package floors. `environment/contract.rs:107` can install another package version after deployment. `environment/rollback.rs:38` chooses its target from Helm history. Measurements report 195 duplicated lines in `package_upgrade/progress.rs`, in measurements.md section 7.

Problem: Online coexistence requires additional versions, progress state, compatibility tests, and rollback rules. Floors can request schema work beyond the release selected by the environment document. Helm history selects only an application digest, rather than a complete previously qualified platform and schema combination.

Proposal: Use the maintenance model from Finding C3-3. Drain all affected writers before migration, including shared services when their schema changes. Refuse a transition with stranded work. Apply each database transition transactionally and retain tenant identity, ownership, and authority boundaries.

Recovery proceeds forward from committed migration records. Returning to earlier code requires an explicit document change and qualification against the schema that remains installed. Schema reversal requires an authored migration or an explicit backup restoration.

Deletes: Delete `crates/schema/generator/src/upgrade_stage.rs`, `package_upgrade/progress.rs`, and `qualify_upgrade/stage.rs`. Remove online-stage branches from package application. Delete `catalog.package_upgrade_stages`, `catalog.package_floors`, the document’s `floors` field, and floor application from `environment/contract.rs`. Retain obsolete-binding cleanup. Delete tests whose sole subject is online coexistence, while retaining data-preservation and authority assertions.

Risk: Upgrades interrupt service, and large transactional migrations can exceed the maintenance window. Test that active work blocks migration, failed SQL preserves rows, retries resume correctly, and incompatible earlier releases refuse. Uninterrupted upgrades require a different owner decision before these deletions.

The following exported open beads lose their current implementation premise:

- `wamn-qpgq`: historical cutovers no longer need migration ports because the initial installation replaces existing state. Its acceptance is the baseline hash check of C1-2, because C3-2 is rejected. Evidence: `.beads/issues.jsonl:3469`.
- `wamn-eycl`: the separate `environment.upgrade` saga disappears. There is no package upgrade in development. A release change is an `environment.k` apply, and the UI submits that document. Evidence: `.beads/issues.jsonl:3458`.
- `wamn-ev5g6`: its equal-deadline premise already disappears in current source. `platform/job.rs:15` allows 900 seconds around the 600-second readiness budget. Evidence: `.beads/issues.jsonl:1919`.

These are proposed dispositions, not closures. Root must read live Beads before disposition and cite the relevant implementation commit. Qualification transport and teardown findings `wamn-byw08`, `wamn-uvqx9`, and `wamn-dxcpf` remain relevant.

Rejected (owner review 2026-10-31, see README): maintenance-window lifecycle. Deployment state stays in Helm and Kubernetes, rollback is `helm rollback`, an upgrade never stops user workflows, and the rolling drain of the old release stays. The Deletes line above is nonetheless carried out by the package-baseline deletion in Part 2, because there is no in-place package upgrade, with one exception: `environment/rollback.rs` stays. Rollback is `helm rollback` by ruling, and the integration-schema plan defines `env rollback --reason` as applying the previous deployed digest.

## Open questions for the owner

Answered 2026-10-31: no maintenance windows. Service remains available, old and new images serve concurrently, and a schema-breaking change during development is a reset.

