# WAMN — Deployment and Upgrade Simplification

**Status:** Implementation proposal — scenario detail restored. The ownership and developer-experience decisions below are accepted; command spelling is illustrative until implemented.

**Review basis:** `worktree-cleanup2b`. Source paths identify current code to change; no historical file locations or line references are used. This proposal supersedes older audit recommendations wherever they disagree with the accepted direction. The implementation has not been exercised as part of this document revision.

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

## 2. Current state on the cleanup branch

### Already completed — do not reimplement

| Area | Current state |
|---|---|
| Installer build and publication | `platform/build.rs` and `platform/releases.rs` have been deleted. `platform/install.rs::require_artifacts` consumes prepared resources and releases. |
| Package installation during platform setup | `platform/packages.rs` now contains registry and credential helpers; it no longer runs the old package publication and apply sequence. |
| CDC reader executable | `platform/cdc.rs` selects the host image, not a test image. |
| Operator version | `platform/install.rs::operator_entry` reads the operator dependency from the selected chart. |
| Development loop | Selected-package watching, changed-package builds, and retention of the running application after a failed candidate are implemented. `wamn dev reset` already exists. |
| Database infrastructure | `crates/control/provision/src/database.rs` already renders CloudNativePG `Database` resources. |
| Schema identity | System and project baseline records already exist. Current mismatch handling is still too restrictive for the proposed workflow. |

### Remaining changes — source-specific work

| Current code | Why it must change | Preferred action |
|---|---|---|
| `crates/control/environment/src/platform/install.rs` | Still executes a long, numbered process and couples platform deployment with environment changes | Replace execution with minimal platform Helm/Kubernetes application plus separate bootstrap. Delete obsolete dispatch and owner steps. |
| `.../platform/steps.rs` | A runbook is encoded as an executable step model | Remove from deployment execution. Retain concise setup documentation for actual external prerequisites. |
| `.../platform/environments.rs` | First pass forces `release = none`, with special treatment for environments already running | Remove first pass and its marker exception. Provision database state without changing application selection. |
| `.../platform/job.rs` | Generic `wamn-install-*` completion ConfigMaps control whether work runs | Delete generic completion hashing, markers, skip decisions, and associated tests. Keep only narrowly required Job execution, credentials, wait, and error reporting. |
| `.../environment/write.rs` | Helm equality shortcut can return before handling an incomplete Helm operation | Remove the custom unchanged-values shortcut. Use the normal Helm operation and report the actual result. |
| `.../platform/install.rs::skip_operator` | Additional resource-install optimization | Delete. Let Helm/Kubernetes handle repeated resource application. |
| `.../platform/lock.rs` | One local file lock coordinates the entire installation process | Remove broad coordination as the giant process is deleted. Keep small locks only for actual competing SQL mutations or other identified shared writes. |
| `.../environment/analyse/plan.rs::package_compatible` | Rejects every package version change regardless of SQL | Delete version-equality refusal. Compare code and schema identities independently; report useful warnings. |
| `.../environment/stage.rs` | Package installation and release activation still have coupled assumptions | Keep required SQL/binding work but extract it into independent operations; remove code-only upgrade restrictions. |
| `crates/control/release/src/publish_release.rs` | Publication requires the exact package artifact to have been installed in the target database | Build releases from published package facts. Resolve target-specific configuration during deployment; retain environment-scoped release selection for now. |
| `.../environment/analyse.rs::read_release` | Requires a qualification artifact before a release can be selected | Make qualification an optional test result and report its absence as a warning, not a deployment refusal. |
| `crates/control/package/src/schema_baseline.rs` | System/project schema hash mismatch always refuses | Keep initial schema records; replace blanket mismatch refusal with an explicit platform SQL operation and diagnostics. Preserve actual SQL/query failures. |
| `.../platform/web.rs` | Platform UI upload still reads built files from a source checkout | Publish UI files once, then deploy the selected web artifact; do not rebuild or copy local source output during platform deployment. |
| `.../platform/control.rs`, `.../platform/identity.rs` | Additional hand-written Helm comparison/installation paths | Reduce each to resource selection, Helm application, and status. Delete independent skip rules where no longer necessary. |
| Existing kind test support | Some tests reproduce installation rather than invoke the product command | Use real WAMN deployment commands in tests. Delete duplicate installers and their temporary support as the product operations become available. |

**Deletion rule:** Avoid preserving modules solely because older tests assert their internal output, progress messages, or skip decisions. Replace those tests with checks of actual resource state and SQL results where they cover meaningful behavior.

The current `require_artifacts` check also requires every configured environment to name a release. Change it so **platform installation works with zero applications**. Application release selection belongs to application deployment, not platform bootstrap.

## 3. Small deployment commands, not a scripted runbook

The following command forms describe the intended developer experience. They are **proposals**, not claims about the current CLI.

| Command | Responsibility |
|---|---|
| `wamn platform deploy deploy/kind/cluster.k` | Render/apply selected platform resources and report readiness. Zero applications required. |
| `wamn deploy sample_app --env dev` | Build/publish changed application artifacts from source. Publish an environment release changing only this application's selection. Initialize an empty target if needed, then deploy. |
| `wamn deploy --release sha256:... --env dev` | Select an already published **environment release**; no local source build. |
| `wamn schema apply sample_app --env dev` | Run explicitly selected application SQL or apply the configured reset behavior; independent from application activation. |
| `wamn platform schema apply --scope system` | Apply selected WAMN system SQL; independent from platform image deployment. A project schema operation selects project environments explicitly. |
| `wamn schema reset --env dev` | Explicitly reset a disposable application environment. |
| `wamn schema adopt --env dev` | Start migration tracking from an existing selected schema state. |
| `wamn rollback sample_app --env dev` | Publish an environment release using this application's previous artifact selection. Retain every other application's selection. Persist and apply the new release without reversing SQL. |
| `wamn status --env dev` | Show the selected environment release and each application's selected artifacts, running artifacts, hostname, readiness, schema state, and warnings. |

Each environment has one selected environment release and one Helm release. Each routed application has its own composed entry, HTTP workload, Service, and hostname inside that release. A composed entry is the application's executable component. Do not combine unrelated applications into one entry or create a Helm release per application.

An application update or rollback publishes a new environment release changing only the targeted application's artifact selection. Other applications retain their selections, artifact bytes, and data. They are not guaranteed independent runtime rollout. One Helm upgrade can restart or roll their workloads.

Kubernetes, wasmCloud, and CloudNativePG remain the resource owners. Do not add a deployment controller. SQL is never reversed.

**Source of release selection:** `environment.k` remains the authored reference. An application update reads the persisted environment selection and replaces only the targeted application's artifacts in a newly published environment release. Rollback uses that application's previous artifact selection from the persisted environment selection. It publishes a new environment release retaining every other application's current selection. The rollback target is neither the previous environment release nor a target read from Helm history. Do not invoke `helm rollback`.

After publication succeeds, deploy or rollback writes the resulting environment release to `environment.k` before invoking Helm. Neither command creates a Git commit. On a failed rollout, the file retains the pending selection. Status reports the artifacts actually running for each application, including mixed or unknown state. A retry applies the persisted selection. Do not create another independent selection database.

A prebuilt package digest is not interchangeable with a release digest. `--release` always refers to a published environment release. Full portability of the same release digest across different environments is not required in the initial implementation.

**Warnings by default:** Missing test results, deprecated interfaces, changed schema hashes, uncertain runtime combinations, and potentially destructive SQL should produce concise notices with the relevant artifact/database names and an actionable command. Do not demand an override to proceed. Operations that cannot execute because an artifact is absent, a component will not load, the target is unauthorized, or SQL/Helm actually fails must report failure.

## 4. Schema modes: `reset` and `migrate`

Schema lifecycle is selected **per environment**, not by package SemVer. The environment's existing disposable designation authorizes automatic data loss; `schema.mode` does not supply a second authorization mechanism.

```kcl
# Proposed environment.k field
schema = { mode = "reset" }  # or "migrate"
```

| | `reset` | `migrate` |
|---|---|---|
| Intended target | Disposable development/test database | Database retaining data |
| Fresh install | Install current complete SQL | Install baseline and pending SQL |
| Code-only deploy | Preserve existing data | Preserve existing data |
| Physical schema change | Reset **the whole disposable project-environment database**, then install the complete selected schema | Execute explicitly selected migration files; no automatic SQL during code-only deploy |
| Editing initial SQL | Normal | Warn when already applied; do not silently replay |
| Tracking | Record physical-schema hash for each package and the database instance | Record applied migration identifiers/checksums and the baseline for each package |
| Default | For explicitly disposable environments | For environments retaining data |

### The one reset rule

```text
Reset automatically when ALL are true:
  - the operation selects schema-bearing artifacts for this environment
  - the selected physical schema fingerprint differs from installed state
  - schema.mode == reset
  - the existing environment is recorded as disposable
  - WAMN can reconstruct the complete target, including every selected package

Otherwise:
  - unchanged physical schema: preserve data
  - migrate mode: do not reset; report pending changes
  - non-disposable or uncertain target: do not perform destructive replacement
```

This rule is **identical for source-based deploy and `--release` deploy**. A source-build path must not have special authority to reset. Published release artifacts must carry the information needed to compute the same decision. If required package SQL or a selected dependency is unavailable, refuse the reset *before* touching the existing database.

**Physical fingerprint:** Calculate from the actual complete schema SQL selected for installation (authored and generated physical definitions), independently for each package; derive an environment fingerprint from the selected package identities plus WAMN-owned project schema. Do **not** use `build_record::schema_digest` as the database identity: that build-input digest includes generator and shared interface inputs unrelated to the physical database. Changes to code, web files, ordinary queries, or SemVer alone must not reset data.

### Reset implementation

- **Local `wamn dev`:** Reuse its existing disposable target and template-based database reset behavior. Replace the current narrower reset triggers with the physical-schema rule above, while retaining the ability to keep data for code/query-only changes.
- **Deployed kind/GCP:** Select **a fresh CNPG-managed database instance** rather than dropping tables in the running database. Reuse the existing instance suffix/database naming scheme, extended to select a replacement instance explicitly. Create the new `Database` resource, initialize the complete project substrate and every selected application schema, update the environment database reference and credentials, and activate against that instance. Retain the old instance if preparation fails. Clean up the old instance through a separate explicit operation; CNPG's current reclaim policy retains it when only the resource is removed.
- A reset is a **whole project-environment database** operation. Show all affected applications. Do not imply that applications in the same database can be reset independently.
- Do not reset the WAMN system database during an application reset. A disposable **platform** reset remains a separate explicit operation.

This preference trades somewhat slower deployed resets for a smaller, less destructive implementation. If kind measurements later show it to be unworkable, reconsider the implementation without changing developer-facing mode semantics.

### Switching modes

- **`reset` → `migrate`:** Editing configuration does nothing immediately. On the first schema operation, compare the recorded package baseline hashes and database instance. When they match the selected state, adopt the baseline without SQL or data loss. A record match confirms what WAMN last installed, not that nobody subsequently modified the database; optional introspection can warn about drift.
- **Mismatch during adoption:** Show the differences and offer explicit adopt, manual repair, or reset. Do not silently claim unexecuted SQL has run.
- **`migrate` → `reset`:** Editing configuration does nothing immediately. The next physical schema change performs a reset only when the target is already marked disposable. Current schema must be reconstructable, including the baseline and any migrations needed to produce it.
- **Repeated transitions:** Supported. A reset creates a new database instance and begins a fresh record. Previous disposable history is not carried forward.
- **Version 1.0:** Does not silently change the environment mode. Stability is a recommendation, not a data-loss policy.

### SQL execution

CloudNativePG owns the database, not application SQL. In deployed kind/GCP environments, **a Kubernetes Job running the WAMN SQL executor is the default**; no developer laptop port-forward is required. `wamn dev` may connect directly to its local database. Record applied SQL only after actual completion; recognize identical retries; report real SQL errors. Handle PostgreSQL statements that cannot run transactionally as separate, explicitly reported work. No Atlas operator or additional SQL controller is required.

The system database's initial setup is also a special SQL operation. `provision_system::install_control_store` currently executes multiple batches and checks for the existence of the `registry` schema. Make initialization transactional with its completion record wherever possible. If a subset cannot share a transaction, provide an explicit incomplete/repair result; **do not automatically delete a partially initialized control database**.

## 5. Concrete deployment and upgrade use cases

These scenarios are part of the implementation specification, not short examples. They define user intent, work performed, work not performed, warnings, and recovery. Commands are **proposed syntax**. They do not imply a WAMN deployment controller or a fixed sequence for future upgrades.

**Common execution model:** A CLI command selects artifacts and configuration, invokes Helm or the Kubernetes API for long-lived resources, and reports running state. Kubernetes, the wasmCloud operator, and CloudNativePG continue managing the resources after the command exits. WAMN SQL is a distinct operation executed by a short-lived Job in deployed environments. The friendly CLI can combine the minimum operations needed to initialize an empty application, but it does not store progress through a general installation process.

**Common state and safety:** Every operation identifies the target, selection, running version when reachable, and completed effects. A different code/schema version, unavailable optional test result, or possibly breaking interface produces a warning. Missing artifact bytes, authorization failure, failed SQL, invalid component instantiation, or failed Helm/Kubernetes execution produces a failed operation. A configured `reset` on an explicitly disposable environment is intentional and is reported before replacing data.

### 5.1 Fresh platform installation, with zero applications

**Intent:** Start with an empty local Kubernetes cluster and finish with a WAMN platform able to accept application installations. This must work without any application package, application release, GCP project, or cloud credentials.

```bash
# Stage 1: complete the use case locally first.
wamn platform deploy deploy/kind/cluster.k
wamn platform status deploy/kind/cluster.k

# Stage 2: supported only after the local implementation works.
wamn platform deploy deploy/gcp/cluster.k
```

`deploy/kind/cluster.k` is a **new file**; the existing KCL schema already permits `provider = "kind"`. Local work requires Docker, kind, a local OCI registry, cluster configuration, and access to required pinned images. It must not require Cloud Build, GKE, GCS, Artifact Registry, Workload Identity, cloud IAM, or public DNS. GCP-specific prerequisites remain owned by their existing infrastructure mechanism, including OpenTofu where appropriate.

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

**Intent:** A developer can install a new application on an existing platform with one command. The project environment may be new or may already contain other applications.

```bash
wamn deploy sample_app --env dev

# No local source build when the environment release already exists:
wamn deploy --release sha256:<release-digest> --env dev
```

`--release` selects a **published environment release**, not a package artifact. Full reuse of an identical release digest across different environments is not a requirement for this first implementation.

**Execution:**

1. **Identify application inputs.** Read the selected package declarations, effective environment membership, base-package dependencies, and any dependent overlay. Retain each other application's exact artifact selection from the persisted environment release, including its composed entry.
2. **Prepare only changed source inputs.** For source deployment, generate the selected code and compile affected components. When a base changes, recompose dependent executable bytes within the targeted application. Reuse unchanged outputs. SQL introspection can use a disposable build database; it must not require installing the candidate into the destination database.
3. **Publish and construct the release.** Publish immutable package/component artifacts and create one environment release from published facts. Include a separate composed entry for each routed application. Change only the targeted application's artifact selection. The release constructor must not require the destination `catalog.packages` table to have the candidate version or exact artifact digest already installed.
4. **Ensure target database infrastructure.** Create the CNPG `Database` resource for a new project environment and initialize the WAMN-owned project state. The resource remains under CNPG management. An existing project environment retains its database and existing application state unless the selected schema strategy authorizes a reset.
5. **Apply initial or changed SQL according to mode.** An empty application target receives the selected complete SQL baseline. For an existing `migrate` environment, pending SQL is reported but not automatically run during a code deployment. For an existing explicitly disposable `reset` environment, a different selected **physical schema** triggers replacement of the whole project-environment database. The decision must be the same whether the selected release came from source or `--release`.
6. **Resolve runtime configuration.** Use the target's actual connection references, permissions, credentials, application membership, and route configuration. Resolve a separate hostname for each routed application. Missing optional configuration is reported; invalid required configuration fails the requested activation.
7. **Select and deploy.** Update the owning `environment.k` release selection without automatically creating a Git commit. Apply the environment's single Helm release with an HTTP workload and Service for each routed application. The wasmCloud operator starts or updates those workloads. Report each application's running artifacts, readiness, and hostname. Other applications retain their selections, but the Helm upgrade can restart or roll their workloads.

**Shared-database case:** If `sample_app` is added to an existing `migrate` environment and has not yet been installed, apply only the new application's initialization SQL while preserving other application rows. In `reset` mode, adding a package changes the complete selected schema. If that requires resetting a shared disposable database, **all applications in that project environment lose their data** and their selected schemas must be reconstructed. The command lists every affected application before replacement. It must not claim per-application isolation where there is one project-environment database.

**Must remain unchanged:** WAMN host/control/identity binaries, unrelated external environments, and unrelated application artifact bytes. Initializing an empty project environment is not a reason to run the entire platform installation command.

**Failure and retry:** If initial SQL succeeds but the component fails readiness, leave the SQL record intact. Report the running artifacts for each application, including mixed or unknown state. Retrying the release uses the published bytes and does not repeat committed SQL. If a `reset` requires reconstructing several packages, verify the selected SQL and artifacts are available **before** selecting a replacement database. Retain the former database instance when preparing the replacement fails.

```text
Deploy sample_app -> org/project/dev
  Environment selected      sha256:E1
  sample_app selected       sha256:B1
  sample_app running        sha256:B0 (ready)
  App SQL                   installed successfully
  Application workload      failed to start
Result: incomplete
Next: retry E1 or roll back sample_app if it has a previous artifact selection
```

**Useful warnings:** Schema recorded for the destination differs from the selected package, deprecated interface in use, or selected release lacks optional tests. **Actual failures:** No published component, unauthorized database access, required connection unavailable, failed SQL, or component unable to start.

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
wamn platform schema apply --scope project --env dev
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
wamn schema diff sample_app --env dev
wamn schema apply sample_app --env dev
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
| 2 | Deploy transitional code A1 | Both columns | A0/A1 during rollout | Update app release; no SQL |
| 3 | Explicit data backfill and synchronization | Both columns with consistent values | A1 | Execute only selected SQL/data task |
| 4 | Apply SQL removing old column | `public_name` | A1 | Warn about older code; run selected SQL |
| 5 | Deploy final code A2 if necessary | `public_name` | A2 | Update app release; no SQL |

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

A physical schema change on an environment already recorded as disposable triggers a **whole project-environment database** reset. This rule applies equally when the selected schema comes from source or from published release artifacts. It does not trigger for handler code, ordinary query edits, web changes, or SemVer changes without physical schema changes.

1. Compare the selected physical schema fingerprints with the recorded installed fingerprints.
2. If unchanged, do nothing to the database. If changed, list all selected applications sharing the project-environment database and the data that will be discarded.
3. Verify that WAMN can reconstruct the WAMN-owned project schema, every selected package's complete SQL, privileges, and CDC prerequisites before replacing anything.
4. Ask CNPG to create the replacement database instance; initialize it with the complete selected state. Update the environment's database instance selection and related runtime credentials. Do not alter the WAMN system database.
5. Activate the selected application release separately when requested. Retain the former database if preparation or initialization fails, and report any incomplete switch accurately.

**Explicit reset:** `wamn schema reset --env dev` performs the same database operation even if the schema fingerprints have not changed. This is only permitted for a target WAMN already records as disposable. The configured schema mode alone does not authorize destructive changes.

**Scope:** One project-environment database may contain several applications. Resetting it discards all their data; it is not a per-application table deletion operation. CNPG's existing resource ownership should be reused, not bypassed by manually dropping tables in an active database.

### 5.5 Application code upgrade, hotfix and rollback

**Intent:** Fix code, change only the targeted application's artifact selection, and preserve database rows and unrelated platform resources. Other applications retain their bytes and data, but the shared Helm upgrade can restart or roll their workloads.

```bash
wamn deploy sample_app --env dev
wamn rollback sample_app --env dev

# Select already published bytes without a source build:
wamn deploy --release sha256:<release-digest> --env dev
```

1. **Build selected changes.** For a source-based deploy, compile the changed code. Recompose affected executables within the targeted application, including its base dependencies. Leave other applications' selected artifacts untouched. Do not rebuild the WAMN host or platform UI.
2. **Publish immutable bytes.** Publish the changed package/component and construct a new environment release from the persisted selection. Replace only the targeted application's artifact selection. Retain every other application's exact selection and composed entry. Construction does not require installing the candidate code version as SQL in the target database.
3. **Inspect schema information.** Compare the executable's build-time schema information with the installed schema; show differences and likely SQL errors when known. No SQL is run solely because the application code version changed.
4. **Select and activate.** Write the new environment release to `environment.k`, then upgrade the environment's single Helm release. Keep each application's composed entry, HTTP workload, Service, and hostname separate. The wasmCloud operator manages the workloads. Other applications retain their selections, but this upgrade can restart or roll their workloads.
5. **Report running state.** A successful artifact publication is not a successful deployment until the selected workload is running. Show the environment selection and each application's selected and running artifacts separately, even after failure.
6. **Rollback if requested.** Read the targeted application's previous artifact selection from the persisted environment selection. Publish a new environment release replacing only that application's selection. Retain every other application's current selection, bytes, and data. Write the new release to `environment.k` before upgrading the same Helm release. Do not select the previous environment release or use Helm history to choose the target. Never reverse SQL. If the installed schema differs, warn that earlier code can fail. Attempt the requested rollback when its artifacts and target are valid.

```text
Deploy sample_app -> org/project/dev
  Application code          rebuilt -> sha256:A1
  Dependent components      reused or recomposed as necessary
  Host image                unchanged
  Installed schema          sha256:S1
  Build-time schema         sha256:S1
  SQL operations            none
  Environment selected      sha256:E1
  sample_app selected       sha256:A1
  sample_app running        sha256:A1 (ready)
  sample_app previous       sha256:A0
Result: deployed
```

**Failure examples:** A compiler error leaves the persisted selection and running artifacts unchanged. Publication succeeds but Helm fails: `environment.k` retains E1 with `sample_app` selecting A1. If A0 still runs, status reports A0. If rollout partially completes, status reports the observed artifacts for each application. If the new component cannot instantiate, report an execution failure.

Retrying E1 does not rebuild its published bytes. Rollback publishes a new environment release with `sample_app` selecting A0 and all other applications retaining their current selections.

For example, E0 selects applications A0 and B0. Updating the first application publishes E1 with A1 and B0. Updating the second publishes E2 with A1 and B1. Rolling back the first publishes E3 with A0 and B1, not E0 or E1. Both applications retain their data throughout these code operations.

**Fast/default:** Build, publish and deploy selected code with inexpensive warnings. **Optional guidance:** Preview resource changes, show structural schema differences, or run a targeted test. **Optional team restrictions:** Accept only separately tested artifacts for selected environments. These choices do not create a second deployment path.

### 5.6 Local edit loop is not deployed kind

`wamn dev` retains its fast local source-watching runtime. It should discover the selected package and dependencies, regenerate/recompile only changed inputs, and keep the last running application when a candidate build fails. An edited physical schema in `reset` mode can reset its explicitly disposable local database; an ordinary query or code edit preserves rows. In `migrate` mode, a developer applies SQL explicitly.

**Separately**, the kind deployment scenarios above must use the same published artifacts, Helm charts, wasmCloud operator and CNPG integration intended for GCP. A passing local edit loop does not establish that a fresh platform or release can be installed into Kubernetes.

### 5.7 Mode changes are independent of schema execution

| Sequence | User action | Behavior |
|---|---|---|
| A | Change `schema.mode = "reset"` to `"migrate"` | No immediate SQL or data change |
| B | First schema operation after A | If selected per-package baselines match the recorded installed state, adopt them as migration starting points without rerunning SQL |
| C | Baseline mismatch or unknown instance | Warn and require an explicit choice to adopt, repair, or reset; do not claim SQL ran when it did not |
| D | Apply new migration | Run selected SQL and keep rows |
| E | Change back to `"reset"` | No immediate data change |
| F | Later selected physical schema change, with a disposable target | Reconstruct a fresh project-environment database from current selected SQL |
| G | Change again to `"migrate"` | Adopt the new baseline; do not restore discarded earlier migration records |

A mode change never resets the system database or automatically changes the application release. No mode change is tied to package SemVer or the first 1.0 release.

### 5.8 Cross-cutting recovery and status

Every workflow reports the selected environment release and each application's selected artifacts, running artifacts, hostname, and readiness. SQL status reports what the database recorded as successfully applied. When a target cannot be read, report `unknown`, not an assumed state. Retain ordinary Helm history for operation recovery and diagnostics, never for choosing an application's rollback target. Do not introduce a new database tracking each deployment action.

```text
Target: org/project/dev
  Platform host              H2 (ready)
  System SQL                 PS4 (recorded)
  Project schema             PP2 (recorded)
  Environment selected       E9
  sample_app selected        A9
  sample_app running         A8 (ready)
  sample_app hostname        sample.example.test
  other_app selected         B4
  other_app running          B4 (ready)
  other_app hostname         other.example.test
  Application SQL            S5 (recorded)
  Last application apply     failed at readiness
  WARN schema-mismatch: A9 was built using different schema inputs
  NOT CHECKED: optional application test
Next: retry E9 or roll back sample_app to A8 in a new environment release retaining B4
SQL already applied is not reversed
```

After an interruption during a Helm write, read Helm/Kubernetes resources and reapply the selected configuration if needed. An application rollback publishes and persists a new environment release as section 3 specifies. It does not restore a whole earlier environment release. Other applications retain their selections and data, but the Helm upgrade can restart or roll their workloads.

After an interruption during SQL execution, read the database's applied SQL records and report incomplete work. Do not trust an installer completion ConfigMap. A failed app deploy after a successful schema change leaves the SQL in place. A platform executable rollback similarly does not undo committed platform SQL.

**Coordination:** Only mutations that can conflict need operation-specific coordination. In particular, serialize SQL writes to the same database. Different application environments should not depend on a global platform-install lock for a normal code deployment.

## 6. Warnings and operation results

Start with a small interface rather than a large permanent warning list.

```text
WARN schema-mismatch
The selected application was built against different schema input.
  application: sha256:...
  installed:   sha256:...
  next:        wamn schema diff sample_app --env dev

Environment selected: sha256:...
Application: sample_app
Artifact selected: sha256:...
Artifact running:  sha256:...
Status: ready | not-ready | failed | not-checked
```

Each notice should identify the actual source of the finding and one next action. A warning is not a hidden refusal. Show each application's selected and running artifacts separately, including when Helm failed after the environment release pin changed. Output must not expose secrets.

Optional targeted tests, SQL previews, deeper diff analysis, and team deployment restrictions can be added later without changing the core execution path. Mandatory release qualification is not the default developer workflow.

## 7. Implementation plan — local kind first and deletion immediately

**Implementation work is organized around removal of the old deployment architecture.** The first useful milestone is not a better `platform apply` journey; it is a thin replacement that can deploy a minimal platform to kind. Remove superseded execution as soon as that replacement works, even if optional former behavior has not been carried over.

| Order | Implement | Delete in the same change or immediately after | Completion condition |
|---|---|---|---|
| **1 — Thin platform path** | Create `deploy/kind/cluster.k` (new file); KCL → Helm/Kubernetes apply for platform resources; narrow initial SQL operation | `platform/install.rs` numbered dispatcher; `platform/steps.rs` execution model; environment work inside platform installation | Empty kind cluster installs platform without applications or GCP inputs |
| **2 — Thin application path** | Select prebuilt environment release, render/apply existing application chart, update `environment.k`, report running status | First `release = none` pass, release marker exemption, redundant environment install flow | Existing release deploys on kind without platform install |
| **3 — Delete historical install state** | Keep only actual SQL result tracking and minimal Job execution | Generic `wamn-install-*` ConfigMaps, completion hashes, skip predicates, `skip_operator`, Helm-values shortcut, broad installer file lock | Repeated Helm apply works; no custom completion state decides whether workloads exist |
| **4 — Independent schema handling** | Physical schema fingerprints; `reset`/`migrate`, baseline adoption, deployed SQL Job; baseline warnings | Package-version refusal; blanket baseline mismatch refusal; coupled schema application during code deploy | Code-only changes preserve data; schema operations run independently |
| **5 — Release construction and DX** | Construct releases without an applied-target prerequisite. Keep one environment release and one Helm release, with a composed entry, HTTP workload, Service, and hostname per routed application. Publish application updates and rollbacks as new environment releases changing only the targeted selection. Report each application's running artifacts. | Target-installed-digest requirement, single-entry routing assumption, whole-environment rollback, Helm-history target selection, and remaining local source dependencies in UI deployment | Deploy two routed applications, update each, then roll back the first while retaining the second's update and both applications' data. Shared Helm upgrades can restart or roll either workload. |
| **6 — Remove parallel implementations** | Replace remaining direct resource constructors with a small chart/KCL owner where practical; retain only useful tests | Duplicate test-only installers, historical rendering paths, obsolete runbook execution rules | One product path and tests that invoke it |
| **7 — GCP integration** | Use the same commands and resource rendering; add GCP project/IAM/Artifact Registry/storage/networking configuration | GCP-only forks of deployment behavior where unnecessary | Equivalent platform/app/schema operations work on GCP |

**Do not wait for all five journeys to be completed before deleting the old installer.** Deliver the small platform path, remove its predecessor, then add application and SQL operations as independent features. Temporary missing functionality must be visible in the implementation notes rather than preserved through a second full deployment engine.

### Local kind completion tests (development milestone)

The existing kind tests provide reusable assertions, but not a finished developer-facing deployment command. Test the **actual product operations** rather than adding another test-only platform installer:

1. **Platform:** Fresh kind installation succeeds with zero applications, a local OCI registry, wasmCloud, CloudNativePG, identity, and no GCP account.
2. **Application:** Publish and select one environment release containing two routed applications. Initialize their database through product commands. Deploy one Helm release with a composed entry, HTTP workload, Service, and reachable hostname per application.
3. **Development separation:** `wamn dev` remains a fast source edit loop; deployed kind behavior uses the real Helm/registry/operator path.
4. **Hotfix:** Update the first application, then the second, then roll back the first through product commands. Each operation publishes a new environment release changing only its targeted application's artifact selection. The final release retains the second application's update and the first application's previous artifacts. Both applications retain their data. Do not assert independent runtime rollout or unchanged pod identities.
5. **SQL:** Standalone migration, disposable reset, baseline adoption, and a multi-step SQL/application sequence work without rebuilding the platform.
6. **Platform upgrade:** Host/control-only changes and platform SQL changes can run independently of application publication.
7. **Recovery:** Interrupt and repeat deployment; verify actual Helm and Kubernetes state, accurate failures, and no dependency on generic completion markers.

**Begin GCP-specific implementation only after these local tests pass.** This requirement applies to completing the implementation milestone, **not to every later developer deploy, application update, or GCP release**. Do not make an automatic kind test run a prerequisite for ordinary `wamn deploy`.

### GCP scope after local success

Keep the same build, publication, application release, SQL, and Helm implementations. Add only provider-specific details: GKE context and cluster resources, service identity and IAM, Artifact Registry, object storage, ingress/network resources, and certificates. OpenTofu remains responsible for resources it already owns. Avoid a second deployment implementation or a GCP-only command flow.

## 8. Explicitly deferred

- A WAMN Kubernetes operator, GitOps requirement, or additional SQL operator.
- Compatibility range solver, automatic schema retirement, application-version-based reset policy, and strict field deprecation enforcement.
- Mandatory release qualification or a global predeployment checklist. Run selected tests when useful; a separate team policy may later require them.
- Automatic platform/system database reset after failed initialization.
- General deployment history database, progress checkpoint records, broad distributed orchestration, and speculative concurrency controls.
- Platform UI deployment controls until the CLI/API operation is working; the UI must call the same authorized operation, not create another deployment implementation.
- Optimization for unchanged Helm applies until actual measurements show an important cost.

## 9. Measure the result

Measure cold local platform installation, first application installation, code-only edit-to-ready time, SQL-only update time, Helm reapply time, and recovery after an interrupted command. Also record the number of manual steps and how many unrelated artifacts were rebuilt. **Prefer deleting an operation to caching its completion.** Source-level review alone cannot establish the performance improvement; the local kind implementation should provide those measurements.
