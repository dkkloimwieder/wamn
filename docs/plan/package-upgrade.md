# Package upgrade

Owner decision: Epic 4 uses `env apply` for the upgrade lifecycle.
Only one epic is scoped at a time. Each later epic requires an owner instruction after review of its predecessor.

See [package upgrade behavior](../architecture/data-access.md#schema-and-definition-ownership) and [environment lifecycle](../architecture/overview.md#environment-lifecycle).

## 2. Fixed rules

- A migration must first satisfy the existing migration policy. An upgrade of an installed predecessor with a non-empty suffix must additionally satisfy one **predecessor-compatible additive policy** defined beside that validator in `crates/schema/introspection/src/migration_policy.rs`. `qualify-upgrade` and production `apply-package` call the same implementation; there is no second allowlist.
- Each SQL-bearing package in the proof uses one unchanged application schema, read from its actual serving workload's `wamn.schema`. Predecessor and candidate statements use that same exact `search_path`. Operator-supplied predecessor or candidate schema flags are not evidence. Packages with no SQL are exempt from this schema requirement.
- Ordinary release qualification still runs against the exact final release candidate after production package application and release publication.
- One deployment writer per environment. Existing lifecycle verbs keep their ownership. `qualify-upgrade` is a new read-only database-transition proof verb; it does not qualify a release, cannot satisfy `publish-qualified-release`, and does not replace a mutation or deployment verb. This does not create the second **release-qualification** path forbidden by `docs/history/release-qualification.md`.
- An upgrade starts only from a **converged environment**: the selected release head is the release actually serving the environment. A prior failed deployment that left head and workload divergent is resolved before any production package migration starts. The environment-specific check compares the selected manifest digest with the release digest carried by the serving workload; Epic 1's wamn-dev proof reads the running host Deployment's release arguments rather than assuming a successful prior rollout.
- The project database must already carry the platform schema needed by the upgrade implementation, including the durable upgrade-qualification carrier. That carrier lands through the normal platform `upgrade-schema` path before application-package upgrade is attempted.
- Greenfield: changed package content records a new digest under the same package version (docs/plan/platform-config.md ruling 22).
- No runtime upgrade mode is introduced. Upgrade is the ordinary release path preceded by qualification and followed by the environment's existing host deployment procedure.

### 2.1 Predecessor-compatible additive policy

See [schema and definition ownership](../architecture/data-access.md#schema-and-definition-ownership) for the shared policy and the two grant states.

### 2.2 Package version convention

SemVer is a signal, not compatibility proof, but WAMN uses one convention consistently:

- patch: internal correction with no application migration suffix;
- minor: predecessor-compatible additive schema or operation capability;
- major: incompatible contract or migration requiring explicit client work.

Therefore a package version with a non-empty application migration suffix is at least a minor version. Epic 1 uses `2.0.0 → 2.1.0`, not `2.0.0 → 2.0.1`.

## 6. Copy contract

Upgrade qualification needs a database whose **application-visible predecessor state** matches the installed database.

The copy must preserve:

- application schemas and rows;
- package and migration records;
- effective release rows, release head and release manifest snapshot needed to identify the selected predecessor;
- application/control state required by `apply-package` and `reconcile-package-data-access`.

Cluster-level runtime credentials and login-role secrets are not qualification inputs.

`pg_dump` supplies a transactionally consistent snapshot while the predecessor remains live. Epic 1 does **not** quiesce the source: its admitted migration subset is deliberately value-independent, so writes committed after the snapshot do not invalidate the structural compatibility proof. Production `apply-package` still rechecks the live package leaf and selected predecessor immediately before mutation. Data-dependent migrations that need a boundary-state copy belong to Epic 3.

The disposable server is prepared with the current project-database **stable role and extension floor** before restore. Login credentials and credential-generation secrets are never copied. The restore preserves object ownership required by package DDL (notably `wamn_db_owner`) while omitting source ACL commands. A restore mode that changes application-table ownership to the scratch superuser is invalid, because production `apply-package` executes package DDL as `wamn_db_owner`.

Capture predecessor effective application privileges as sorted schema/table/column facts using the same canonical representation and privilege reader as the qualified post-state. Read these facts and predecessor catalog identities in one source transaction. Export its snapshot and pass it to `pg_dump --snapshot`, keeping the transaction open until the dump completes.

Before applying the suffix, reconstruct the captured privileges for `wamn_app` on scratch and compare the resulting effective privilege facts with the capture. Preserve table-level and column-level privilege distinctions. Do not widen column grants into table grants or grant access to candidate columns to make the intermediate check pass. Refuse if the stable-role scratch environment cannot reproduce the captured state. This helper acts only on the owned scratch database and exposes no production grant-writing command.

Conceptually:

```text
create scratch server/database
→ install current stable project role + extension floor
→ pg_dump source database
→ pg_restore schema/data with ownership preserved, source privileges omitted
→ reconstruct and verify captured predecessor application grants
→ apply candidate suffix through the shared qualification executor
→ plan predecessor statements under predecessor grants
→ reconcile candidate data access in scratch
→ plan predecessor and candidate statements under candidate grants
```

Use the current provisioning helpers to create stable roles rather than copying passwords or hand-recreating role attributes. The implementation receives a live round-trip test that proves a restored application table is still owned so `SET LOCAL ROLE wamn_db_owner` can apply the qualified suffix.

A CloudNativePG physical clone requires a measured scale problem. It must preserve the qualification contract.

## 7. Epic boundaries

### 7.2 Epic 2: base upgrade under an overlay

Live forward acceptance of a base upgrade under an overlay requires an owner-scheduled run on `wamn-dev`.
No live rollback or re-forward is scheduled.
TUI work requires a separate scope.

### 7.3 Epic 3: changes outside the predecessor-compatible additive subset

Goal: support constraint strengthening, column removal, type change, and backfill through online expand/contract sequences.

Expand/contract adds compatible structures before removing obsolete structures. Serving workloads continue throughout the sequence. A change that cannot be staged this way is refused and named.

Owner scope (2026-10-03): Epic 3 implementation follows acceptance of Epic 2's two reviewed fixes and this scope. Implementation is offline. Live access requires an owner instruction.

The contract stage removes the synchronization triggers.

See [application declarations](../architecture/data-access.md#application-declarations) for expand stages, resumable backfills, synchronization triggers, conditions, and the whole-row grant exception.

Widening the stage expression policy requires an owner decision. Stage SQL stays within package-owned relations and the existing function and cast policy.

Every step must preserve compatibility with the workloads that still serve and the overlays that still consume its contracts. Changed or removed contracts require compatible intermediate stages. The contract step cannot remove structures or contracts while serving consumers still require them.

Contract waits until no host serves an older release and no run pinned to an older release remains in flight. The run identity comes from `wamn_run.runs.effective_release_id`. If either condition cannot be read, contract refuses.

Contract pins the currently selected release as the environment's floor release. This floor is the oldest release that selection permits. Qualification records the exact selected release identity and its serving SQL. Application rechecks that identity and plans every statement against the contracted schema before the contract transaction commits. If any statement fails, the transaction aborts and names the statement.

Older releases' SQL can stop working after contract because those releases fall below the floor. Contract does not require their SQL to remain valid. The selected release's serving SQL must remain valid.

The contract transaction records the floor with the schema changes. `select-release` refuses any release below that floor and names the contract that set it. Later upgrades must preserve this restriction. Reverse migrations and restore procedures are outside this epic.

The existing package identities, complete presented-root identities, runtime schema requirement, and immutable transition history remain required. Schema relocation and execution across multiple schemas remain outside this epic. Ordinary release qualification remains required.

The refusal of nontransactional migration statements remains required. This includes `CREATE INDEX CONCURRENTLY`, `DROP INDEX CONCURRENTLY`, `REINDEX CONCURRENTLY`, `VACUUM`, `CREATE DATABASE`, and `ALTER SYSTEM`.

Owner decision (2026-10-01): migration-specific exceptions to the standard qualification checks belong to Epic 3. Epic 1 must refuse an upgrade if predecessor statements fail against the candidate schema under predecessor grants, even if candidate grants restore compatibility.

Implementation order:

Guard contract against older hosts and runs, then enforce the environment's floor release.
Prove the remaining sequence with retained-data PostgreSQL tests and document its failures and retry behavior.

The PostgreSQL exit covers successful staged changes, concurrent writes, failed conditions, premature contract, rollback refusal, and named refusal of unstaged changes. It also covers the whole-row grant exception under the actual predecessor privileges. Each implementation issue records its commit and proving test.

Contract tests cover older runs, unreadable consumer state, and floor enforcement after later upgrades.

Live acceptance requires an owner instruction. Stopping serving workloads is not an accepted migration strategy.

### 7.4 Epic 4: upgrade in the platform UI

Goal only: let the platform UI start an upgrade through the proven command path.

Scoped after Epic 3 closes and the platform lifecycle worker exists.

## 8. Live acceptance

### 5. Real wamn-dev WMS run

Use WMS because it is a single package in this scope and does not introduce the Acme base-pin problem.

Prerequisites on the run day:

- Ordinary `qualify-release` can reproduce the exact WMS wamn-dev candidate.
- The selected digest matches the running host Deployment's `--release-manifest-digest` value for the WMS predecessor.
- The project database carries `catalog.package_upgrade_qualifications` through `upgrade-schema`.

The WMS candidate adds nullable `wms.location.description text`, with no default or new business invariant.

Use [environment apply and rollback](../architecture/overview.md#environment-lifecycle) for the forward upgrade, predecessor rollback, and re-forward. Keep the successor schema throughout rollback.

This owner-scheduled run is the final Epic 1 acceptance:

```text
2.0.0 → 2.1.0 → rollback to 2.0.0 → re-forward to 2.1.0
```

Record commands, qualification identities/digests, release identities, host/workload artifact identities and observed application results in `gcp.md`.

The owner reviews and accepts the recorded live evidence.

Receiving plus `client_acme_receiving` is deliberately not the Epic-1 acceptance environment because the overlay pins the exact Receiving base version; changing that base is Epic 2.

## 9. Out of scope

- Everything in Epics 2 to 4.
- Upgrading a base while an installed overlay pins it.
- Adding or strengthening constraints on an existing relation.
- Destructive schema changes.
- Data backfills.
- Online or resumable backfills.
- Reverse migrations.
- Arbitrary rollback across several package versions.
- General schema subtyping or version-range compatibility.
- A second deployment writer for either kind or wamn-dev/GCP.
- Concurrent package upgrades.
- A widening of the ordinary migration policy's admitted statement set.
- CloudNativePG cloning.
- A generalized deployment saga.

## 10. Owner decisions

1. Final real migration: `wamn_wms@2.0.0 → 2.1.0`, adding nullable `wms.location.description text` with no default.
2. Upgrade qualification is a separate read-only verb: `qualify-upgrade`.
3. Accepted upgrade qualification is persisted in the project database by `apply-package` as immutable canonical evidence; rollback never depends on an operator-local file.
4. Copy mechanism: `pg_dump` / `pg_restore`.
5. Version convention: a non-empty application migration suffix is at least a minor package version.
6. Issue 3 uses unit/live-PostgreSQL tests only; the real wamn-dev deployment proof is Issue 5.
7. The final environment run waits until `wamn-ld93.33` closes and its release-qualification path is merged, verified, reviewed, and accepted on `main`. Package-upgrade does not create a second release-qualification path.
8. An upgrade begins only from a converged selected-and-serving predecessor release.
9. Durable upgrade evidence is platform schema and reaches existing project databases through the normal numbered `upgrade-schema` migration path.
10. Epic 1 ends after the WMS forward upgrade, predecessor rollback, and re-forward deployment are recorded. Epic 2 does not open automatically; the owner reviews Epic 1 first and only then scopes its Beads issues.
11. Epic 1 requires predecessor statements to pass against the candidate schema under both predecessor and candidate grants. Exceptions, including nullable additions that temporarily break whole-row reads, belong to Epic 3.
12. Each SQL-bearing upgraded package keeps one actual serving `wamn.schema`. Qualification observes the workload and plans both versions with that exact `search_path`. Application repeats the observation before mutation. Packages with no SQL are exempt. Schema relocation or multiple-schema support requires a future persisted deployment fact outside Epic 1.
13. Reconciliation matches the exact complete presented-root set, candidate transition, and derived data-access state. It never selects by timestamp. Sequential upgrades form a chain. Earlier immutable qualifications remain transition history and do not independently constrain later package sets.
14. The owner-scheduled WMS live run is the final Epic 1 acceptance. Record its commands and results in `gcp.md`. Close Epic 1 only after reviewing and accepting the live evidence.
