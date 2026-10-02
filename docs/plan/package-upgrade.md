# Package upgrade

**Implementation governance — Beads.** This specification is implemented as **four Beads epics**, and only one epic is fully scoped at a time. Epic 1 is the only epic that may have issue children now. Epics 2 to 4 may exist only as placeholder Beads epics containing the goal and boundary written here; they get **no issue decomposition** until the owner reviews the completed predecessor epic and explicitly opens the next one. Closing an epic does not automatically scope or start its successor. An agent assigned Epic 1 stops when Epic 1 closes.

Updated through: 2026-10-01, `main` at `c2eadc85a`. Owner accepted Epic 1 implementation on 2026-10-01. Epic 1 is `wamn-xvu5`, with issues `wamn-xvu5.1` through `wamn-xvu5.5`. Epics 2, 3 and 4 are goal-only placeholders `wamn-orb5`, `wamn-5ihq` and `wamn-eycl`.

## 1. Goal

An installed environment moves from one version of an application package to the next without losing its data.

Before production schema changes, WAMN proves the candidate migration against a copy of the installed predecessor database and proves that the currently serving release remains structurally executable after the candidate schema and data-access grants are applied.

That proof is **upgrade qualification**. It is distinct from ordinary **release qualification**:

- `qualify-upgrade` proves the predecessor → candidate database transition before production mutation;
- `qualify-release` proves the exact published release, source and deployment artifacts after the candidate package is installed and the release exists.

The existing lifecycle remains authoritative. Upgrade adds one pre-apply proof and one persisted compatibility fact; it does not replace `apply-package`, `reconcile-package-data-access`, `push-component`, `publish-release`, `prepare-release`, `qualify-release`, `publish-qualified-release`, `select-release`, or the environment's existing host deployment mechanism.

Today the platform already has immutable package lineages, transactional suffix application, generated data-access reconciliation, immutable release publication, release qualification and release selection. It does not yet have the pre-apply compatibility proof, durable installed-superset rollback evidence, or the documented ordering that connects them.

The 2.0.0 `kind-to-type.md` package step changed versions without adding package migrations. Epic 1 proves the first ordinary application-package version step that carries a real migration.

## 2. Fixed rules

- A new package version names the installed version as `predecessor_version`, and its cumulative migration stream keeps the predecessor stream as a byte-identical prefix. Nothing here loosens that.
- `apply-package` remains the sole application migration writer.
- A migration must first satisfy the existing migration policy. An upgrade of an installed predecessor with a non-empty suffix must additionally satisfy one **predecessor-compatible additive policy** defined beside that validator in `crates/schema/introspection/src/migration_policy.rs`. `qualify-upgrade` and production `apply-package` call the same implementation; there is no second allowlist.
- Production schema changes occur while the predecessor release may still serve. Qualification must prove predecessor statements against the candidate schema under both predecessor grants and candidate post-reconciliation grants.
- Each SQL-bearing package in the proof uses one unchanged application schema, read from its actual serving workload's `wamn.schema`. Predecessor and candidate statements use that same exact `search_path`. Operator-supplied predecessor or candidate schema flags are not evidence. Packages with no SQL are exempt from this schema requirement.
- Upgrade qualification runs against a copy of the installed predecessor database with its data. A fresh schema alone is not upgrade proof.
- Ordinary release qualification still runs against the exact final release candidate after production package application and release publication.
- One deployment writer per environment. Existing lifecycle verbs keep their ownership. `qualify-upgrade` is a new read-only database-transition proof verb; it does not qualify a release, cannot satisfy `publish-qualified-release`, and does not replace a mutation or deployment verb. This does not create the second **release-qualification** path forbidden by `docs/plan/release-qualification.md`.
- An upgrade starts only from a **converged environment**: the selected release head is the release actually serving the environment. A prior failed deployment that left head and workload divergent is resolved before any production package migration starts. The environment-specific check compares the selected manifest digest with the release digest carried by the serving workload; Epic 1's wamn-dev proof reads the running host Deployment's release arguments rather than assuming a successful prior rollout.
- The project database must already carry the platform schema needed by the upgrade implementation, including the durable upgrade-qualification carrier. That carrier lands through the normal platform `upgrade-schema` path before application-package upgrade is attempted.
- A sealed coordinate never changes. A fix is the next package version.
- A failed `apply-package` transaction leaves the database unchanged.
- A committed package migration is never reversed as rollback machinery. If later work fails, the installed schema remains at that package version and the next package candidate names it as predecessor.
- No runtime upgrade mode is introduced. Upgrade is the ordinary release path preceded by qualification and followed by the environment's existing host deployment procedure.

### 2.1 Predecessor-compatible additive policy

The existing migration policy admits more than Epic 1 may deploy while a predecessor is serving. Epic 1 therefore defines one narrower predicate in `migration_policy.rs` and uses it in both qualification and production application.

The Epic-1 subset permits:

- creation of a new ordinary table, including constraints internal to that new table;
- addition of a nullable modeled column with no default;
- addition of the two already admitted non-null constant-default column forms:
  - `boolean NOT NULL DEFAULT false`;
  - `text NOT NULL DEFAULT 'not_required'`.

The two non-null forms are predecessor-compatible because an old `INSERT` that does not name the new field remains valid and PostgreSQL supplies the declared constant default.

Epic 1 refuses additions or changes whose compatibility depends on application values or execution semantics, including a new or strengthened constraint on an existing relation. A statement can plan successfully and still fail at execution because a new `CHECK` rejects its value.

Constraint strengthening, data-dependent compatibility, backfills, destructive changes and type changes belong to epic 3.

### 2.2 Package version convention

SemVer is a signal, not compatibility proof, but WAMN uses one convention consistently:

- patch: internal correction with no application migration suffix;
- minor: predecessor-compatible additive schema or operation capability;
- major: incompatible contract or migration requiring explicit client work.

Therefore a package version with a non-empty application migration suffix is at least a minor version. Epic 1 uses `2.0.0 → 2.1.0`, not `2.0.0 → 2.0.1`.

## 3. Current state

Measured on `main` at `c2eadc85a`.

| Place | Today |
| --- | --- |
| Version registration | `apply-package` registers a successor only when its declared predecessor is the current lineage leaf and the inherited migration stream is a byte-identical prefix. |
| Suffix on installed database | `apply-package` applies only the pending migration suffix to an installed database and records it in the same transaction. |
| Migration policy | The validator admits the existing narrow additive DDL surface. It is broader than Epic 1 because a named `CHECK` on an existing relation can change predecessor runtime behavior. |
| Data-access reconciliation | `reconcile-package-data-access` takes one presented root for every installed package lineage and derives one effective `wamn_app` authority union. After a version bump, the predecessor release therefore runs under the successor presented-root ACL while it remains serving. |
| Statement check | Generation plans statements through PostgreSQL as `wamn_app` under package-derived grants. Today's normal check uses the fresh schema supplied for that package; it does **not** construct the installed predecessor database after a successor migration and therefore does not close the cross-version whole-row/grant case recorded by `wamn-emtx.29`. |
| Release snapshots | The selected release's exact serving manifest is stored in `catalog.release_manifest_snapshots`. The canonical manifest closes over each operation's admitted statement set. |
| Release publication | `publish-release` requires the package coordinate to have already reached `apply-package`. |
| Release preparation | `prepare-release` captures an already-published effective release. It cannot be used to prove a schema change before production `apply-package`. |
| Release qualification | `qualify-release` takes `--repository --revision --candidate`; it reconstructs fresh application schemas, verifies generated output and SQLx metadata, rebuilds artifacts and runs application cases against the exact candidate. It does not apply a suffix to a copy of an installed predecessor. |
| General release-qualification work | `docs/plan/release-qualification.md` / `wamn-ld93.33` now specifies candidate-driven qualification for arbitrary wamn-dev release identities, but that implementation has not landed at this snapshot. Epic 1's final WMS environment run depends on it; package-upgrade does not absorb that work. |
| Platform schema migration | Installed project databases now advance through numbered `deploy/sql/migrations/project/*` files via `upgrade-schema`. A new `catalog.package_upgrade_qualifications` carrier therefore needs both fresh-install DDL and the next project migration before `apply-package` can persist evidence on an existing environment. |
| Replica identity | Deployment documentation now requires `reconcile-replica-identity` after package/data-access work when registrations need old-row fields; the dev loop runs it after data-access reconciliation on activation. The upgrade ordering must retain this existing lifecycle step. |
| Selection | `select-release` changes the environment head after verifying qualified publication. It has no installed-superset compatibility rule. |
| Kind deployment | `deploy-release` owns the kind delivery cases and currently requires exact equality between the selected release's migration signature and the installed leaf's signature. |
| wamn-dev / GCP host switch | `deploy-release` is not the deployment path. Hosts move through generated host values, `helm upgrade`, workload `kubectl apply`, readiness waits and application verification, as `kind-to-type.md` B10 demonstrates. |
| Overlay pins | An overlay pins the exact base package version and component digest in `base_dependencies`. A base version change under an installed overlay is epic 2, not epic 1. |
| Documented application-upgrade path | None. Existing operations documentation covers fresh installation and release delivery, not this retained-data package upgrade sequence. |

## 4. Design

### 4.1 Two qualifications

The proofs are deliberately separate.

```text
qualify-upgrade
    installed predecessor database + candidate package roots
        → proves the database transition before production mutation

qualify-release
    published release + source + exact artifacts
        → proves the final release after production package application
```

`qualify-upgrade` has its own request and result types. Its result cannot satisfy an argument that expects ordinary release qualification. It proves a database transition only; `qualify-release` remains the single release-qualification path.

### 4.2 `qualify-upgrade`

Inputs:

```text
--database-url <installed predecessor database>
--tenant <tenant>
--environment <environment>
--package <candidate package root being upgraded>
--presented-package <root> ...   # complete presented root set, including candidate
--kubeconfig <file>
--context <Kubernetes context>
--namespace <runtime namespace>
--host-deployment <host Deployment>
--package-workload <package-id=WorkloadDeployment> ...
--result <new file>
```

The exact CLI spelling may be adjusted during implementation, but the semantic inputs are fixed: the installed predecessor database, the package being advanced, and the **complete presented package root set** required by data-access reconciliation. Epic-1 WMS happens to have one root; the interface does not encode that accident.

The Kubernetes inputs select live resources. They do not assert a schema. Qualification resolves each SQL-bearing package's `WorkloadDeployment` through its current replica set to the actual serving `Workload` objects. It reads `wamn.schema`, tenant, and environment from their component or service configuration. It also proves readiness, ownership, and placement on the observed host Deployment.

Each SQL-bearing package must expose exactly one valid schema across its serving replicas. That schema must remain unchanged for the candidate. Qualification refuses an absent or ambiguous workload, conflicting schemas, multiple application schema surfaces, or a proposed schema change. Predecessor statements use the exact observed schema as their `search_path` under both grant states. Candidate statements use that same `search_path` after reconciliation. No predecessor-schema or candidate-schema flag can replace this observation.

Packages with no SQL need no package-workload selector or application schema. Host release convergence remains required. Future schema relocation or multiple-schema support requires a persisted deployment fact that describes the serving schema. That work is outside Epic 1.

`qualify-upgrade`:

1. Reads the installed candidate package lineage leaf and requires the candidate's `predecessor_version` to name it exactly.
2. Reads the current environment head and its immutable serving manifest from `catalog.release_manifest_snapshots`.
3. Requires the selected predecessor release to be the release actually serving the environment. Reads its digest from ready host pods after full Deployment rollout. Resolves the serving workload identities and application schemas described above. Records these facts with the head. `apply-package` observes them again immediately before mutation.
4. Requires that selected predecessor release to contain the predecessor package coordinate being upgraded.
5. Records the predecessor release ID and manifest digest, predecessor package coordinate, candidate package coordinate, manifest identity, cumulative migration identities and candidate suffix identities. Captures the canonical predecessor effective privileges from the same source snapshot used for the copy, as specified in §6.
6. Verifies the candidate suffix with the shared predecessor-compatible additive policy in `migration_policy.rs`.
7. Creates its own disposable PostgreSQL target and copies the installed project database into it using the copy contract of §6.
8. Reconstructs predecessor application grants on scratch from the captured privilege facts through a library-only qualification helper. Requires the scratch effective privileges to match those facts exactly and verifies the predecessor state captured in steps 1 to 5. Candidate reconciliation must not run yet.
9. Applies the exact candidate suffix on the **disposable copy through a library-only qualification executor** that reuses the production migration planner, ownership checks, migration policy and transaction body but does **not** require or persist upgrade evidence. The public production `apply-package` command still requires accepted qualification for a non-empty installed successor suffix. This split avoids a circular requirement in which qualification would need the result it is creating.
10. Reads predecessor operation statement sets from the selected release's stored canonical `ServingManifest`. Plans every predecessor statement as `wamn_app` against the candidate schema with predecessor grants still in force. Uses the exact `search_path` observed for its serving package. Any failure refuses qualification before candidate reconciliation. Reuse/extract the existing PostgreSQL planning helper so it can plan under the grants already present without replacing them with generated grants.
11. Reconciles the **complete presented package root set** on the disposable copy through the shared data-access reconciliation engine and captures its canonical post-state. The qualification path may apply this state on its own scratch database; it is not an operator-visible bypass for production.
12. Runs `materialize_package check` against the upgraded copy for every presented candidate root so committed generated artifacts are proven current against that exact schema.
13. Loads candidate operation statements through the same exact package-statement loading path used by component publication (`push-component`), refactored into shared code if necessary. It does not discover candidate SQL by scanning generated directories ad hoc.
14. Plans every predecessor statement and every candidate statement against the upgraded copy **under the same resulting post-upgrade effective grants**. Uses the same observed `search_path` for both versions of each package. Uses the same planning helper as step 10 without changing those grants.
15. Requires the copied database's package leaf, migration history and canonical data-access post-state to match the expected candidate post-state.
16. Writes one canonical immutable upgrade qualification result and its SHA-256. The result includes the captured predecessor privilege state, the qualified candidate post-state, and the observed serving workload identities and schemas.

The transition proved is:

```text
installed predecessor data
    ↓
candidate migration suffix
    ↓
old selected-release statements still valid
    under predecessor grants
    ↓
complete candidate presented-root reconciliation
    ↓
old selected-release statements still valid
new candidate statements valid
    under the same post-upgrade grants
```

The predecessor must pass both grant states. Candidate grants cannot compensate for a failure in the interval before reconciliation commits.

### 4.3 What upgrade qualification proves

For Epic 1, the candidate is qualified when:

- it is the direct successor of the installed package leaf;
- the inherited migration prefix is byte-identical;
- its suffix satisfies the predecessor-compatible additive policy;
- its suffix applies successfully to the copied installed data;
- each SQL-bearing package has one observed serving schema that the candidate keeps, with identical planning `search_path` for both versions;
- every predecessor statement plans against the candidate schema with predecessor grants, before candidate data-access reconciliation;
- the complete presented package root set reconciles successfully;
- the resulting data-access post-state is captured exactly;
- every statement of the currently selected predecessor release plans against that post-state;
- every candidate statement plans against that same post-state.

This proves structural SQL and authority compatibility for the Epic-1 migration subset. It does not claim arbitrary data-dependent business compatibility; that is why Epic 1 excludes constraint strengthening and other semantic migrations.

### 4.4 Durable qualification evidence

Upgrade compatibility must survive beyond the operator machine that ran qualification. Rollback may occur days later from another machine.

Add one immutable catalog record for accepted upgrade evidence:

```text
catalog.package_upgrade_qualifications
```

keyed by:

```text
(tenant_id, package_id, candidate_package_version)
```

The carrier is platform schema, not package schema. Its `CREATE TABLE` belongs in the fresh project catalog DDL and in the **next numbered project `upgrade-schema` migration** so an existing environment receives it before Epic-1 package upgrade begins. Do not hard-code the ordinal in this document because other platform work may claim it first.

The record stores:

- canonical `qualify-upgrade` result bytes;
- SHA-256 of those canonical bytes;
- predecessor release ID;
- predecessor release manifest digest;
- recorded timestamp.

The canonical result itself contains the predecessor/candidate coordinates, migration identities, the exact complete presented-root identities, the predecessor effective privileges, and the qualified data-access post-state. It also records the host Deployment and package workload identities, deployed specification identities, and observed schemas. Resource identities include UID and generation. Transient host pod identities are not part of the comparison. These values are not redundantly hashed into independent evidence formats.

The qualified data-access post-state is a canonical structural value: sorted schema/table/column privilege facts plus the presented package identities needed to derive them. It is **not** rendered GRANT SQL, PostgreSQL OIDs, credential-generation role names, or another independently authored hash.

`apply-package` is the sole writer of the accepted transition record. For a non-empty successor suffix on an installed database, it requires the exact qualification result, verifies its digest and contents against the live predecessor head/package state and candidate bytes, and writes the immutable qualification record in the **same transaction** that registers/applies the candidate package version.

Before mutation, `apply-package` also compares live predecessor effective privileges with the captured state. It shares the data-access reconciliation lock for this check and the package application transaction. Privilege drift refuses the first application and requires fresh qualification. An exact retry of an already accepted application uses persisted evidence, since candidate reconciliation can already have changed the grants.

Before the first application, `apply-package` repeats the live workload observation through the recorded resource selectors. It refuses changed workload identities, specifications, schemas, or loss of convergence before any database mutation. An unchanged host Deployment can replace a pod without invalidating the evidence.

`reconcile-package-data-access` remains the sole production writer of package ACL state. When the installed candidate leaf has accepted upgrade evidence, reconciliation automatically requires the exact qualified presented-root set, derives the live post-state, compares it with the persisted canonical post-state, and refuses before commit on any difference. No extra CLI switch selects a weaker path.

Reconciliation accepts the qualification that matches the complete current presented-root set and its derived data-access state. It also requires the exact candidate transition identity. It never selects a qualification by timestamp. Earlier qualifications remain immutable transition history. They do not independently constrain later package sets. Each later qualification starts from the complete currently installed set and proves the complete successor set.

A replay with the same exact result is a no-op. Conflicting evidence at the same candidate coordinate refuses.

The local-target manifest comment remains a local-development mechanism and is not reused as production upgrade evidence.

### 4.5 Production order

For an installed Epic-1 environment:

0. **Require prerequisites**

   - the environment head and serving workload are converged on the predecessor release, verified by comparing the selected manifest digest with the release digest carried by the running workload;
   - the project database has the platform schema migration that installs the upgrade-evidence carrier;
   - ordinary release qualification is available for the candidate shape used by the environment. The final WMS run waits for `wamn-ld93.33`; this epic does not reimplement that work.

1. **Qualify upgrade**

   ```text
   qualify-upgrade
   ```

   against the installed predecessor. Production state is unchanged.

2. **Apply candidate package**

   ```text
   apply-package --upgrade-qualification <result>
   ```

   For a non-empty successor suffix over an installed predecessor, `apply-package`:

   - reruns the shared predecessor-compatible policy;
   - requires the live leaf and selected predecessor identity named by the result;
   - requires live predecessor effective privileges to match the captured state before the first application;
   - requires exact candidate manifest and migration identities;
   - applies the suffix in its existing transaction;
   - records the canonical qualification evidence in that same transaction.

3. **Reconcile data access**

   ```text
   reconcile-package-data-access <complete presented root set>
   ```

   The command automatically compares the exact presented-root set and the derived canonical effective ACL with the post-state recorded in the accepted upgrade qualification before committing.

4. **Reconcile replica identity**

   Run the existing idempotent `reconcile-replica-identity` lifecycle step for the upgraded/presented packages as required by their registrations. This retains the deployment ordering already used by current operations and the dev loop; Epic 1 does not invent a second CDC/replica-identity model.

   At this point:

   ```text
   schema = candidate schema
   grants = candidate effective grants
   serving release = predecessor
   ```

   and `qualify-upgrade` proved that serving state.

5. **Finish ordinary package/release publication**

   ```text
   push-component
   → publish-release
   → prepare-release
   → qualify-release
   → publish-qualified-release
   ```

   Ordinary release qualification still proves the exact candidate release and artifacts. Upgrade qualification does not replace it.

6. **Select candidate**

   ```text
   select-release
   ```

7. **Move the environment's hosts/workloads through its existing deployment path**

   - kind delivery cases: `deploy-release`;
   - wamn-dev / GCP: generate host values, `helm upgrade`, apply the released workloads with `kubectl`, wait for readiness, and exercise a meaningful authenticated application operation, following the shape of `kind-to-type.md` B10.

8. **Web artifact, when present**

   ```text
   wamn web upload
   ```

   after the release head has been selected as required by that command.

There is no new generic host deployment mechanism in Epic 1.

### 4.6 Failure states

#### `qualify-upgrade` fails

Production is untouched.

Fix the candidate and qualify again.

#### `apply-package` fails

Its transaction rolls back. Neither the candidate package transition nor the accepted qualification record exists.

#### `apply-package` commits; data-access reconciliation has not committed

The candidate schema and package leaf are installed and the qualification evidence is durable, while predecessor grants remain in force.

Upgrade qualification must explicitly prove this intermediate state. The additive policy alone does not prove that predecessor grants remain sufficient after the schema changes.

Any replacement package candidate now names the applied candidate as predecessor.

#### Data-access reconciliation commits; replica-identity reconciliation or later publication/qualification fails

The predecessor release continues serving:

```text
candidate schema
+ candidate effective grants
+ predecessor release
```

`qualify-upgrade` explicitly proved this state.

Nothing reverses the migration. The operator may repair and continue publication or abandon that release attempt. Any later package candidate names the applied package leaf as predecessor.

#### Selection or host deployment fails

A release head and the actual running workload may temporarily differ, depending on the existing environment deployment mechanism.

This is ordinary release/deployment recovery, not schema rollback. The database remains on the candidate schema. The operator either completes deployment of the selected release or reselects a predecessor release whose persisted upgrade evidence proves compatibility with the installed schema.

## 5. Rollback over a retained additive schema

Rollback means release rollback over the retained candidate schema. It never means reverse migration.

Suppose:

```text
P@2.0.0 active
    ↓
qualify + apply P@2.1.0
    ↓
R_new selected and deployed
```

The installed package leaf remains `2.1.0`.

The predecessor release `R_old` may be selected again only when the database contains the persisted upgrade qualification that proves:

```text
R_old package migration signature
    is the qualified predecessor prefix of
installed P@2.1.0

AND

R_old statements
    were proved against the qualified P@2.1.0 schema
    under the qualified post-reconcile grants

AND

the live installed leaf and relevant data-access state
    still match that persisted qualification
```

Epic 1 supports only the immediate predecessor named by one exact persisted upgrade qualification. It does not introduce version ranges, arbitrary schema subtyping, or multi-version rollback chains.

### 5.1 `select-release`

If the selected release package signatures exactly equal the installed leaves, selection proceeds as today.

If a selected release names the immediate predecessor of an installed successor, `select-release` reads the persisted upgrade qualification from the project database and verifies the live installed state against it.

It refuses:

- no persisted qualification;
- a non-prefix migration history;
- a different candidate leaf;
- a different predecessor release or manifest digest;
- a changed relevant data-access post-state.

No operator-local qualification file is required for rollback.

### 5.2 Kind `deploy-release`

`deploy-release::require_compatible_schema` changes from:

```text
installed migration signature == selected migration signature
```

to:

```text
exact equality
OR
persisted qualified installed-superset compatibility
```

The second arm uses the same persisted evidence and compatibility predicate as `select-release`.

Epic 1 proves this predicate with unit/live-PostgreSQL tests. It does **not** add another kind-cluster journey to Issue 3. Existing kind delivery cases remain the cluster owner of `deploy-release`.

### 5.3 wamn-dev / GCP rollback

wamn-dev does not call `deploy-release`.

After reselecting the qualified predecessor release, hosts/workloads move back through the same environment path used for forward deployment:

```text
render predecessor release host values
→ helm upgrade
→ kubectl apply predecessor workload definitions
→ wait for readiness
→ exercise meaningful authenticated operation
```

The database remains on the successor schema throughout.

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

A CloudNativePG physical clone is not part of Epic 1. It may later replace the copy transport for measured scale reasons without changing the qualification contract.

## 7. Epic boundaries

### 7.1 Epic 1: predecessor-compatible additive upgrade of one package

One package version moves to its direct successor with a non-empty suffix accepted by the predecessor-compatible additive policy.

No installed overlay pins the package being upgraded.

Epic 1 proves:

- pre-apply upgrade qualification on copied installed data;
- shared upgrade-policy enforcement in qualification and production apply;
- durable accepted upgrade evidence installed through the platform schema-migration path;
- post-upgrade data-access reconciliation with persisted post-state verification;
- retained replica-identity reconciliation in the ordinary package lifecycle;
- predecessor serving on candidate schema/grants;
- normal candidate release publication and qualification;
- release selection;
- immediate-predecessor rollback over the retained additive schema;
- actual wamn-dev host switch through its existing deployment path.

### 7.2 Epic 2: base upgrade under an overlay

Goal only: a base package moves while an overlay pins it; define the overlay's successor version, updated base pin, compatibility/reverification rules, and the order of the two package changes.

Scoped after Epic 1 closes.

### 7.3 Epic 3: changes outside the predecessor-compatible additive subset

Goal only: constraint strengthening on an existing relation, column removal, type change, backfill, and any drain or expand/contract sequence they require.

Owner decision (2026-10-01): migration-specific exceptions to the standard qualification checks belong to Epic 3. Epic 1 must refuse an upgrade if predecessor statements fail against the candidate schema under predecessor grants, even if candidate grants restore compatibility.

One such case is a nullable column addition that breaks a predecessor whole-row query such as `to_jsonb(widget)`. The old grants do not cover the new column, so the query can fail between `apply-package` committing and data-access reconciliation completing. Epic 3 must address an explicit exception procedure for this case. This note records the requirement without scoping that procedure or adding an Epic-1 bypass.

Scoped after Epic 2 closes.

### 7.4 Epic 4: upgrade in the platform UI

Goal only: expose `environment.upgrade` as the lifecycle worker's saga using the proven command path.

Scoped after Epic 3 closes and the platform lifecycle worker exists.

## 8. Issues — Epic 1

One branch. Workspace tests only until the final owner-scheduled real environment run. No new cluster journey is added before Issue 5.

External prerequisite: the final WMS run waits until `wamn-ld93.33` closes and its release-qualification path is merged, verified, reviewed, and accepted on `main`. Issues 1–4 may land without absorbing that epic.

### 1. Upgrade carrier, successor fixture and shared upgrade policy

Add the immutable `catalog.package_upgrade_qualifications` carrier to fresh project catalog DDL and to the next available numbered project `upgrade-schema` migration. The migration is the installed-database path; editing only `catalog-schema.sql` is not acceptance.

Add one predecessor-compatible successor fixture:

```text
platform_fixture 2.1.0 → 2.2.0
```

with one nullable modeled column and no default. For the successful fixture, extend a relation that predecessor statements do not read whole. Keep the existing `to_jsonb(widget)` statement unchanged and use a column addition on `widget` for the separate refusal case in Issue 2. No unrelated behavior change.

Define the predecessor-compatible additive policy once in `crates/schema/introspection/src/migration_policy.rs`, beside the existing migration validator.

Acceptance:

- a fresh project catalog contains the qualification carrier and an installed pre-change project database gains the identical carrier through `upgrade-schema`;
- registration admits fixture 2.2.0 over 2.1.0;
- inherited migration bytes are the exact prefix;
- the new nullable-column suffix is admitted;
- the two existing non-null constant-default shapes are admitted by the shared predicate;
- an added `CHECK` on an existing relation is refused by the upgrade predicate even though the ordinary migration policy admits its shape;
- `apply-package` applies only the suffix to a database containing fixture 2.1.0 rows;
- every predecessor row remains;
- failed suffix execution leaves predecessor state unchanged.

### 2. `qualify-upgrade`

Add the read-only `wamn-ctl qualify-upgrade` verb and its distinct result type.

It implements §4.2 with the §6 ownership-preserving scratch-copy contract and the complete presented package root set.

The scratch migration/reconcile work uses library-only shared executors owned by `qualify-upgrade`; it cannot call the production evidence-required `apply-package` path recursively and exposes no CLI bypass.

The predecessor statement source is the selected release's canonical `ServingManifest` from `catalog.release_manifest_snapshots`, not an old source checkout. Candidate generated output is first checked against the migrated scratch database, and candidate statement facts come through the same exact package-statement loader used by publication/component admission.

Refusals:

- predecessor is not the installed leaf;
- selected release does not contain that predecessor coordinate;
- candidate is not its direct successor;
- a SQL-bearing package has no unambiguous ready serving workload or no single valid `wamn.schema`;
- the package exposes multiple application schemas or changes the observed serving schema;
- inherited migration bytes differ;
- suffix violates the shared predecessor-compatible policy;
- copy cannot be created or restored with the required stable roles, extensions and object ownership;
- copied predecessor state differs from the captured installed state;
- captured predecessor effective privileges cannot be reproduced exactly on scratch;
- candidate migration fails on copied data;
- predecessor statement fails on the candidate schema under predecessor grants, even if candidate reconciliation could make it pass;
- complete presented-root data-access reconciliation fails;
- predecessor statement fails on the upgraded copy under candidate post-reconcile grants;
- candidate statement fails;
- final copied package/data-access state differs from the qualification result.

Acceptance uses disposable PostgreSQL tests:

1. The platform fixture 2.1.0 database contains predecessor data. Qualification proves the compatible 2.2.0 fixture, with predecessor statements passing under both grant states and candidate statements passing under candidate grants.
2. The copy preserves object ownership and reconstructs effective privileges exactly, including the distinction between table and column grants. The suffix executes as `wamn_db_owner`.
3. A nullable column addition to `widget` leaves the predecessor whole-row query without access to that column. Qualification refuses at the intermediate check and writes no successful result. A separate scratch assertion proves the same query passes after candidate reconciliation, so final-state compatibility cannot hide the refusal.
4. A failed intermediate check reports the predecessor statement identity and PostgreSQL refusal. The successful and refused runs leave source rows, schema, package records, release head, grants, and accepted evidence unchanged.

Unit tests cover workload observation without a Kubernetes journey. They refuse absent or ambiguous workloads, incorrect ownership or placement, incomplete readiness, conflicting schemas, and schema changes. Statement planning tests prove that both versions use the same observed `search_path`. Packages with no SQL require neither a schema selector nor workload custom resources.

### 3. Persist and consume qualification evidence

For a non-empty installed successor suffix:

- production `apply-package` requires the exact `qualify-upgrade` result;
- it reruns the shared predecessor-compatible policy;
- it verifies live predecessor release/package state and exact candidate identities;
- it observes the serving workloads again and compares their recorded identities, specifications, and schemas before mutation;
- it compares live predecessor effective privileges with the captured state before the first application;
- it writes the immutable `catalog.package_upgrade_qualifications` record in the same transaction that installs the candidate package;
- conflicting evidence for the same candidate coordinate refuses;
- `reconcile-package-data-access` automatically consumes that accepted evidence for the upgraded leaf and refuses unless the exact presented-root set and canonical post-state match before commit.

Add one shared compatibility predicate used by `select-release` and `deploy-release::require_compatible_schema`:

```text
exact installed signature
OR
persisted immediate-predecessor qualification whose live post-state still matches
```

Acceptance uses unit and disposable-PostgreSQL tests only:

1. applying the exact qualified 2.1.0 candidate persists its evidence atomically;
2. different candidate bytes or a different live predecessor refuse before mutation;
3. data-access reconciliation with a changed root set or changed derived ACL refuses and leaves the pre-reconcile ACL unchanged;
4. `select-release` admits the qualified immediate predecessor over the retained successor schema;
5. the same selection without persisted evidence refuses;
6. a non-prefix predecessor refuses;
7. changed relevant qualified post-state refuses;
8. `require_compatible_schema` admits exact equality and the persisted qualified immediate-predecessor case in live PostgreSQL tests, without invoking Kubernetes;
9. predecessor privilege drift after qualification refuses application before mutation and writes no evidence;
10. retrying the exact accepted application after candidate reconciliation remains a no-op and does not require predecessor grants to return.
11. changed serving workload identity, specification, or schema refuses the first application before mutation.

Existing kind delivery cluster cases remain unchanged in this issue.

### 4. Documentation

Update `docs/operations/deployment.md` with:

- the patch/minor/major package-version convention of §2.2;
- the distinction between `qualify-upgrade` and the single `qualify-release` release-qualification path, explicitly reconciling the wording with `docs/plan/release-qualification.md`;
- the shared predecessor-compatible migration policy;
- the complete production order and its converged-environment precondition, including how the selected digest is compared with the serving workload's release carrier:

```text
qualify-upgrade
→ apply-package
→ reconcile-package-data-access
→ reconcile-replica-identity
→ push-component
→ publish-release
→ prepare-release
→ qualify-release
→ publish-qualified-release
→ select-release
→ environment-specific host/workload deployment
→ optional web upload
```

- both predecessor-serving grant states and refusal when the intermediate state fails, with exceptions deferred to Epic 3;
- durable persisted qualification evidence;
- every failure state in §4.6;
- retained-schema rollback;
- the kind `deploy-release` compatibility rule;
- the separate wamn-dev/GCP host deployment path;
- that reverse migration is not rollback.

Reduce `docs/plan/upgrades.md` to the design debt that remains for Epics 2 and 3.

### 5. Real wamn-dev WMS run

Use WMS because it is a single package in this scope and does not introduce the Acme base-pin problem.

Prerequisites on the run day:

- `wamn-ld93.33` is closed and its release-qualification path is merged, verified, reviewed, and accepted on `main`;
- ordinary `qualify-release` can reproduce the exact WMS wamn-dev candidate;
- the environment is converged on the recorded WMS predecessor head/workload by comparing the selected manifest digest with the running host Deployment's `--release-manifest-digest` value;
- the platform project migration carrying `catalog.package_upgrade_qualifications` is applied with `upgrade-schema`.

Publish:

```text
wamn_wms 2.0.0
→ wamn_wms 2.1.0
```

with:

```sql
ALTER TABLE wms.location
    ADD COLUMN description text;
```

The column is nullable, has no default and introduces no new business invariant.

Run on wamn-dev:

```text
qualify-upgrade 2.0.0 → 2.1.0
→ apply-package 2.1.0 with the qualification
→ reconcile-package-data-access
→ reconcile-replica-identity
→ push-component
→ publish-release
→ prepare-release
→ qualify-release
→ publish-qualified-release
→ select-release 2.1.0
→ render host values
→ helm upgrade
→ kubectl apply WMS workloads
→ wait for readiness
→ meaningful authenticated WMS operation succeeds
```

Then prove rollback without changing the database schema:

```text
select-release 2.0.0
    using persisted immediate-predecessor qualification
→ render predecessor host values
→ helm upgrade
→ kubectl apply predecessor WMS workloads
→ readiness
→ meaningful authenticated WMS operation succeeds
```

Then reselect and redeploy 2.1.0.

This owner-scheduled run is the final Epic 1 acceptance:

```text
2.0.0 → 2.1.0 → rollback to 2.0.0 → re-forward to 2.1.0
```

Record commands, qualification identities/digests, release identities, host/workload artifact identities and observed application results in `gcp.md`.

Close Epic 1 only after reviewing and accepting the recorded live evidence.

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

All Epic-1 decisions are resolved for implementation:

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
