# Audit lane B: Coupling and architecture

Three read-only Codex gpt-6.1-sol runs at reasoning effort high wrote this file, one per part, on 2026-10-31 against commit 8c831349d. Root copied each run's final message verbatim and checked that every finding cites a file and line. Beads: wamn-m0c2w.1, wamn-m0c2w.2, wamn-m0c2w.3. Shared tool output: [measurements.md](measurements.md).

<!-- lane B1, bead wamn-m0c2w.1 -->

## Part 1: graph

The five documented runtime layers have no upward normal dependency among themselves. Their actual edges match the ordering in `docs/architecture/overview.md:218`:

```text
workflow       → execution-host, runtime, engine, run-state, router
execution-host → runtime, engine, run-state
runtime        → engine, run-state
engine         → run-state
run-state      → execution-contract, pg-core
```

Evidence: `crates/execution/workflow/Cargo.toml:16`, `crates/execution/host/Cargo.toml:19`, `crates/platform/runtime/Cargo.toml:24`, `crates/platform/engine/Cargo.toml:21`, `crates/execution/run-state/Cargo.toml:21`.

The larger graph crosses boundaries that the numbered layers do not describe:

```text
control           → runtime, engine, provisioning, schema-control, generator
runtime           → provisioning → generator → introspection
execution-host    → provisioning, generator
schema-control    → generator
generator         → catalog
native libraries  → shared libraries owned by the apps workspace
no_std guests     → WIT sources inside native runtime and router directories
```

The manifests declare 45 native members, 34 application members, and five `no_std` members. These counts describe membership, not separate copies of shared dependencies. Evidence: `Cargo.toml:3`, `apps/Cargo.toml:10`, `apps/platform/no-std/Cargo.toml:24`.

No files changed, and no builds or tests ran. Full Cargo metadata failed with `protecting target directories while Cargo runs: Read-only file system (os error 30)`. The findings use source inspection, manifest parsing, and measurements.md section 10.

## Finding B1-1: Shared contracts pull the schema generator into serving code

Severity: high  
Effort: L  
Area: `crates/control/provision`, `crates/schema/control`, `crates/execution/host`, `crates/schema/generator`  
Evidence: `crates/control/provision/Cargo.toml:43`, `crates/control/provision/src/operation_grants.rs:14`, `crates/control/provision/src/events.rs:57`, `crates/schema/control/Cargo.toml:10`, `crates/execution/host/src/control_route.rs:1126`, `crates/schema/generator/Cargo.toml:13`.

Problem: Provisioning consumes generator-owned manifest types and operation rules. The execution host consumes its release-profile implementation during requests, and schema control also depends on the generator. These three edges point from shared platform behavior toward build tooling, although the overview does not explicitly rank those owners. Through provisioning, the runtime inherits SQLx macros, PostgreSQL introspection, SQL parsing, and generation dependencies.

Proposal: Keep package declarations and their semantic rules below both generation and provisioning. Place declaration ownership in the existing catalog package model, and keep release-profile derivation beside admitted component facts. The generator must consume those contracts and own only compilation, emission, and build orchestration. Broker builders must accept package/entity pairs rather than the complete generator manifest.

Deletes: The generator dependencies from provisioning, schema control, and execution host. Remove generator-specific error sources from provisioning errors.

Risk: Manifest acceptance, permission vocabulary, and `release.profile` responses can change. Preserve the existing operation-grant tests, generator `platform_generation` tests, and execution-host profile tests.

## Finding B1-2: Six development back-edges enlarge lower-layer test builds

Severity: medium  
Effort: M  
Area: Native crate development dependencies  
Evidence: `crates/catalog/model/Cargo.toml:43`, `crates/execution/run-state/Cargo.toml:35`, `crates/identity/project-state/Cargo.toml:20`, `crates/identity/project-state/Cargo.toml:24`, `crates/identity/platform/Cargo.toml:37`, `crates/control/lib/Cargo.toml:103`. Return edges appear in `crates/control/provision/Cargo.toml:38`, `crates/schema/generator/Cargo.toml:25`, `crates/platform/runtime/Cargo.toml:34`, `test-support/infrastructure/Cargo.toml:16`, and `test-support/harness/Cargo.toml:14`.

Problem: Six development edges individually close a path through normal dependencies. Lower-level tests therefore compile higher-level owners and inherit their dependencies. Cargo separates library and test compilation, so these are package-level cycles rather than impossible compilation cycles. Neither measurements.md section 2 nor section 10 contains build timings, so elapsed costs remain unmeasured.

The following counts describe additional non-optional local packages reachable from each development dependency, relative to the source library’s normal dependency set. They include the named dependency, exclude external packages and optional features, and overlap.

| Development edge and normal return path | Additional local packages | Compile exposure |
| --- | ---: | --- |
| `catalog → provision → generator → catalog` | 9 | Generator, introspection, SQLx macros, and SQL parser dependencies |
| `run-state → provision → run-state` | 10 | Provisioning and generation enter pure execution-state tests |
| `project-state → provision → project-state` | 12 | Provisioning and generation enter schema tests |
| `project-state → runtime → project-state` | 20 | Wasmtime, cloud adapters, OCI, and telemetry dependencies |
| `platform-identity → test-infrastructure → platform-identity` | 11 | General infrastructure, provisioning, and generation |
| `control → gate-harness → control` | 2 | Two broad support libraries and their compilation units |

The runtime dependency in project-state tests serves timestamp comparisons at `crates/identity/project-state/tests/schema.rs:902`. The identity infrastructure dependency supplies a stamp assertion at `crates/identity/platform/tests/session_keys_live.rs:37`. These are concrete examples of small test needs selecting large libraries.

Proposal: Move cross-owner database and deployment tests into the existing integration suite. Keep pure tests with their implementation owners. Put timestamp equivalence tests beside the runtime formatter and its SQL counterpart. Keep small assertions local when sharing them requires the entire infrastructure library.

Deletes: The six development dependencies after their cross-owner tests move. Remove redundant support imports without adding another support crate.

Risk: Test selection can omit relocated coverage. Preserve catalog record-history, run-state admission, project-state authority, identity signing-key lifecycle, and control deployment cases.

The six edges are not the total cycle count in the combined development graph. Reciprocal development dependencies also connect control with execution-host, workflow, and test-infrastructure. Evidence: `crates/control/lib/Cargo.toml:106`, `crates/execution/host/Cargo.toml:50`, `crates/execution/workflow/Cargo.toml:42`, `test-support/infrastructure/Cargo.toml:41`. Self-development dependencies enable test features and need separate treatment.

## Finding B1-3: The runtime hub makes artifact consumers depend on cloud execution

Severity: medium  
Effort: L  
Area: `crates/platform/runtime`, `crates/catalog/package-artifact`, `crates/control/lib`  
Evidence: `crates/platform/runtime/Cargo.toml:19`, `crates/platform/runtime/Cargo.toml:47`, `crates/platform/runtime/Cargo.toml:68`, `crates/platform/runtime/src/lib.rs:5`, `crates/control/lib/src/component_admission.rs:25`, `crates/catalog/package-artifact/src/lib.rs:15`.

Problem: Runtime combines cloud plugins, artifact transfer, local application facts, authentication caches, and credentials. Control admission imports its component artifact source, although registry credentials and trust roots already belong to package-artifact. A narrow artifact consumer therefore depends on the complete runtime package. Measurements.md section 2 records 45,267 source lines in that compilation unit.

Proposal: Consolidate OCI transfer and artifact formats in package-artifact. Keep the engine adapter thin and separate from transport ownership. Put shared local-target identity in project-state and keep development orchestration in control. Runtime must expose cloud execution adapters without owning publication contracts.

Deletes: Runtime-owned artifact transport implementations after consolidation, duplicate transport helpers, and control’s runtime dependency once its remaining imports move.

Risk: Registry trust, digest refusal, and local-target authority can change. Preserve `release_manifest_source`, component admission, and local-target tests, together with the engine dependency boundary gate.

## Finding B1-4: The control hub combines independent tools in one compilation unit

Severity: medium  
Effort: L  
Area: `crates/control/lib`, `services/ctl`  
Evidence: `crates/control/lib/src/lib.rs:10`, `crates/control/lib/src/lib.rs:35`, `crates/control/lib/src/lib.rs:46`, `crates/control/lib/src/dev.rs:7`, `crates/control/lib/Cargo.toml:40`, `docs/architecture/overview.md:46`.

Problem: Control owns admission, package application, deployment, platform builds, development sessions, and process supervision. Its normal dependencies include Wasmtime hosting, component composition, SQL parsing, OCI, object storage, and generation. The overview explicitly permits this ownership, so this is excessive coupling within the documented design. Measurements.md section 2 records 80,688 source lines and 32,437 test lines in the crate.

Proposal: Move executable orchestration for development sessions, platform builds, and delivery into the CLI process that runs those tools. Keep reusable package and environment operations in control. Retain one implementation of each operation and remove pass-through layers during the move.

Deletes: Control’s development-session and tool-orchestration modules after relocation. Remove dependencies used only by those modules instead of replacing them with facade crates.

Risk: Command behavior, stage order, and shutdown can change. Preserve the development stage-order tests, deployment cases, and `tests/conformance/tests/delivery_native_build.rs:9`.

## Finding B1-5: Catalog is a shared model hub with installation and product behavior

Severity: medium  
Effort: M  
Area: `crates/catalog/model`  
Evidence: `crates/catalog/model/src/lib.rs:13`, `crates/catalog/model/src/lib.rs:101`, `crates/catalog/model/src/host_route.rs:25`, `crates/catalog/model/src/host_route.rs:55`, `crates/catalog/model/src/workflow_gate.rs:16`, `crates/catalog/model/src/serving_manifest.rs:823`.

Problem: Catalog combines immutable identities with schema bootstrap, built-in control routes, workflow admission, and schedule parsing. Its foundational position makes these unrelated changes affect many consumers. Manifest inspection finds 17 direct non-optional local dependents across the three workspaces. Measurements.md section 2 records 10,062 source lines.

Proposal: Keep canonical records, identity construction, and validation required to admit those records in catalog. Move installation SQL to schema control and built-in control-route construction to its application owner. Keep workflow admission shared where multiple callers need identical decisions, with one implementation.

Deletes: Catalog’s schema bootstrap constant and embedded installation SQL references. Remove built-in application construction from the foundational model package after relocation.

Risk: Release bytes, route identities, and admission decisions must remain stable. Preserve catalog identity and serving-manifest digest vectors, plus workflow admission tests.

## Finding B1-6: Guest interfaces depend on native implementation directories

Severity: medium  
Effort: M  
Area: `apps/platform/no-std`, runtime WIT, router WIT  
Evidence: `apps/platform/no-std/http-page/src/lib.rs:29`, `apps/platform/no-std/http-request/src/lib.rs:30`, `apps/platform/no-std/transform/src/lib.rs:24`, `apps/platform/no-std/label-render/src/lib.rs:62`, `docs/operations/running-tests.md:33`.

Problem: Both HTTP guests read connection interfaces from the native runtime directory. All four component guests read node interfaces from the workflow router directory. These source dependencies cross upward into implementation owners without appearing as Cargo edges. Test selection already compensates by selecting all three workspaces for canonical WIT changes.

Proposal: Put canonical WIT, the component interface definitions, under a shared contract owner independent of native implementations. Hosts and guests must consume that same source. Preserve the separate `no_std` workspace and its allocation/panic implementation.

Deletes: Guest paths into native runtime and router directories, followed by obsolete interface copies after canonicalization.

Risk: Interface identity and generated guest bytes can change. Extend the guest-workspace closure test to cover WIT paths and preserve guest digest and import-admission tests.

## Finding B1-7: The graph gate does not enforce the complete architecture

Severity: low  
Effort: M  
Area: `tools/repo-lint-dependencies`, architecture index, workspace graph measurements  
Evidence: `tools/repo-lint-dependencies:87`, `tools/repo-lint-dependencies:110`, `docs/architecture/README.md:3`, `crates/identity/project-state/Cargo.toml:11`, `apps/Cargo.toml:112`, `Cargo.toml:232`.

Problem: The existing gate enforces selected edge, engine, and workflow boundaries through normal dependency trees. It does not encode the complete control/build separation or the six development back-edges. Measurements.md section 10 omits dependencies outside native workspace membership and includes optional declarations, so its rows cannot establish production reachability. For example, project-state’s catalog dependency belongs to `test-util`, while native dependencies on application-owned contract libraries disappear from the reduced graph.

Proposal: Extend the existing gate to describe all three workspaces, dependency kinds, and relevant feature selections. Document the permitted graph through the architecture index. Test forbidden edges by adding deliberate violations to gate fixtures.

Deletes: Duplicated boundary descriptions and incomplete graph reductions once one generated view replaces them.

Risk: An overly broad rule can reject valid test features or guest sharing. Preserve the existing boundary cases and add cases for optional dependencies, self-development dependencies, and shared application libraries.

The small crates do not justify deletion by size alone. `wamn-pg-core` explicitly began as a cycle-free home for `Sql`, but now also owns identifier and qualified-name contracts. Evidence: `crates/platform/pg-core/src/lib.rs:17`, `crates/platform/pg-core/src/lib.rs:59`, `crates/platform/pg-core/src/lib.rs:171`, `crates/platform/pg-core/src/lib.rs:226`.

`label-template` exists to test rendering without the guest panic handler. The virtualizer is a separate executable invoked by the generator. Neither is merely a one-type holder. Evidence: `apps/platform/no-std/label-template/src/lib.rs:4`, `crates/platform/component-virtualizer/src/main.rs:29`, `crates/schema/generator/src/components.rs:47`. Retain these boundaries unless a replacement preserves their concrete purpose.

## Open questions for the owner

1. Can the build owner supply isolated cold and incremental timings for the six development edges? The supplied measurements cannot support costs in seconds.
2. Does the owner accept a shared contract layer below generation, provisioning, and execution-host? The numbered runtime architecture leaves these relationships unspecified.
3. Must control remain a reusable development-session host outside the CLI? That requirement determines how much orchestration can move into `services/ctl`.

## Proposed fix epics

1. Remove build tooling from serving dependencies
   - Move package contracts below the generator
   - Relocate release-profile derivation
   - Remove generator dependencies from shared platform owners

2. Restore isolated lower-layer tests
   - Relocate cross-owner database tests
   - Remove the six development back-edges
   - Measure cold and incremental compilation costs

3. Reduce shared hubs and expose the complete graph
   - Consolidate artifact transfer and narrow control ownership
   - Move canonical WIT out of native implementations
   - Enforce workspace, feature, and dependency-kind boundaries

<!-- lane B2, bead wamn-m0c2w.2 -->

## Part 2: control

Replace the `wamn-control` umbrella with package, release, and environment owners. Put developer sessions in `services/dev` and repository delivery workflows in `tools/delivery`. Keep registry declarations, provisioning policy, and schema decisions separate.

Measurements.md section 2 records 80,688 source lines, 32,437 test lines, and 71 `Command::new` expressions in `control/lib`. The current checkout contains those expressions in 26 source files. Including integration tests and support files gives the issue’s 37 files.

This review changed no files and ran no builds or tests. `git status --short` returned no changes.

## Finding B2-1: Upgrade-environment mutates packages outside the environment lifecycle lock

Severity: high  
Effort: L  
Area: `crates/control/lib/src/upgrade_environment.rs`  
Evidence: `crates/control/lib/src/upgrade_environment.rs:109`, `crates/control/lib/src/upgrade_environment.rs:736`, `crates/control/lib/src/upgrade_environment.rs:784`, `crates/control/lib/src/upgrade_environment.rs:984`, `crates/control/lib/src/environment/mod.rs:251`.  
Problem: The upgrade workflow applies packages and reconciles grants during its Packages stage. It takes the environment lifecycle lock later, when Deploy calls `apply_release`. Package transaction locks protect individual database operations, but they do not serialize this complete workflow against environment apply or delete. The workflow also maintains a separate environment document under `deploy/gcp/environments`, while the current lifecycle uses `environment.k` (`crates/control/lib/src/delivery/environment.rs:15`).  
Proposal: Delete the independent deployment engine. A delivery tool prepares source, artifacts, and qualification evidence, then submits one environment transition. The environment owner holds the lifecycle lock across all target mutations and calls the sole package executor.  
Deletes: The duplicate package, schema, deployment, credential, and connection orchestration in `upgrade_environment.rs`. Remove the old environment JSON format after migrating its remaining inputs.  
Risk: Upgrade resume behavior and credential retirement can change. Extend `environment_lock_live.rs:11` to prove that an upgrade refuses before package mutation while another lifecycle operation holds the lock. Retain the deployed rerun and rollback coverage in `environment_apply_kind.rs:642`.

## Finding B2-2: Grant policy spans provisioning, verification, and schema repair

Severity: high  
Effort: L  
Area: `crates/control/provision/src/sql.rs`, `crates/control/lib/src/provision_project_env/grants.rs`, `crates/schema/control/src/run_plane/plan.rs`  
Evidence: `crates/control/provision/src/sql.rs:208`, `crates/control/provision/src/sql.rs:1068`, `crates/control/lib/src/provision_project_env/grants.rs:48`, `crates/control/lib/src/provision_project_env/grants.rs:456`, `crates/schema/control/src/run_plane/plan.rs:99`. Measurements.md section 9 records 128 commits to provisioning `sql.rs` in 90 days.  
Problem: Provisioning renders role grants, control/lib defines their expected shape, and schema-control repairs some of the same privileges. Several checks reuse provisioning constants, but family dispatch and privilege construction still span owners. The 1,922-line `sql.rs` includes tests from line 1112, so its total size overstates production code. Its mixed authority responsibilities explain more than its length alone.  
Proposal: Keep cluster roles, credential generations, and their grant declarations in provisioning. Give schema-control ownership of relation-level schema repair. Pass provisioning’s explicit grant requirements into repair and comparison functions. Keep database observation and mutation in the environment owner, and retain independent denial tests.  
Deletes: Duplicate family dispatch and independently assembled expected grants in control/lib. Remove hard-coded copies of provisioning grant requirements from schema repair.  
Risk: A mistaken consolidation can widen access. Preserve `family_surface_grants.rs:444`, `family_surface_grants.rs:730`, and `run_grant_confinement.rs:95`, including independent forbidden-access assertions.

## Finding B2-3: Package application depends on the qualification runner it must serve

Severity: high  
Effort: L  
Area: `crates/control/lib/src/apply_package.rs`, `crates/control/lib/src/qualify_upgrade`  
Evidence: `crates/control/lib/src/apply_package.rs:179`, `crates/control/lib/src/apply_package.rs:431`, `crates/control/lib/src/apply_package.rs:465`, `crates/control/lib/src/qualify_upgrade/mod.rs:26`, `crates/control/lib/src/qualify_upgrade/scratch.rs:85`.  
Problem: Qualification calls package application, while package preparation and transaction completion call back into qualification modules. These are Rust module dependencies rather than a Cargo cycle, but they prevent a clean extraction. Ordinary package application therefore shares an owner with private PostgreSQL startup, snapshot copying, and evidence-file publication.  
Proposal: Create one package-control owner for application, stages, backfill, ownership, history, and data-access reconciliation. Put pure evidence formats and transition rules in schema-control. Put snapshot creation and proof execution in the delivery tool. Both production and qualification use the same transaction executor, with distinct admitted requests.  
Deletes: Application’s dependency on the qualification runner. Remove scratch PostgreSQL and dump tooling from the package executor’s normal dependency closure.  
Risk: Splitting at the wrong point breaks atomic base-and-overlay application or permits stale evidence. Preserve `apply_package_live.rs:933` and `qualify_upgrade/tests.rs:334`. Keep the lock order established at `apply_package.rs:407` inside the executor.

## Finding B2-4: Process execution has several owners with different lifecycle guarantees

Severity: high  
Effort: M  
Area: `crates/control/lib/src/owned_command.rs`, `kubectl.rs`, `release_chart.rs`, `environment`  
Evidence: `crates/control/lib/src/owned_command.rs:26`, `crates/control/lib/src/kubectl.rs:63`, `crates/control/lib/src/kubectl.rs:86`, `crates/control/lib/src/release_chart.rs:666`, `crates/control/lib/src/environment/analyse.rs:549`, `crates/control/lib/src/environment/mod.rs:192`.  
Problem: `owned_command` provides deadlines and descendant cleanup, but several Helm and kubectl paths await processes without a process deadline. An environment operation can retain its lifecycle lock while such a process stalls. The library helper also installs process-wide signal listeners, although the documented CLI boundary owns signals.  
Proposal: Retain one bounded process primitive, with cancellation supplied by the calling application. Assign command construction and cleanup as follows.

The developer service owns Git inspection in `git_source.rs` and `dev/watch.rs`, Cargo/SQLx preparation in `dev/coordinator.rs`, and the children in `dev/activation.rs`, `cdc_reader.rs`, `control.rs`, `environment.rs`, and `pat_issuer.rs`. It also owns connector builds from `integration.rs`.

The delivery tool owns Docker, Cargo, Git, pnpm, gcloud build commands, and repository scripts from `platform_build.rs`, `platform/build.rs`, and the retained preparation stages of `upgrade_environment.rs`. It owns qualification’s Docker commands and `qualify_upgrade/scratch.rs` dump, restore, and PostgreSQL lifecycle.

The platform CLI owns provider and pool commands from `platform/provider.rs` and `platform/pools.rs`. One Kubernetes/Helm adapter serves platform installation, environment analysis and writes, and release-chart operations. Remove direct calls from those domain modules.

Test-only calls in `owned_command.rs`, `event_advisories.rs`, `platform/edge.rs`, `platform_build/inputs.rs`, and inline test modules stay with their tests. The eleven integration-test and support files retain fixture-process ownership.

Deletes: Duplicate Helm and kubectl runners, synchronous context discovery in the environment library, and signal registration inside individual command executions.  
Risk: Cancellation can leave descendants or expose secret diagnostics. Retain `owned_command.rs:147`. Add a stalled Helm/kubectl test that proves bounded failure, descendant cleanup, secret suppression, and lifecycle-lock release.

## Finding B2-5: Developer orchestration is reusable, but belongs to the developer application

Severity: medium  
Effort: L  
Area: `crates/control/lib/src/dev`, `services/ctl/src/dev`  
Evidence: `crates/control/lib/src/lib.rs:18`, `crates/control/lib/src/dev.rs:45`, `crates/control/lib/src/dev/coordinator.rs:15`, `crates/control/lib/src/dev/activation.rs:214`, `services/ctl/src/dev/command.rs:126`, `services/ctl/src/bin/wamn.rs:23`, `services/ctl/Cargo.toml:65`.  
Problem: The library placement deliberately lets the product command and tests share one development engine. That is useful reuse, but it does not require every control caller to depend on the developer application. Watchers, local target resets, host supervision, identity startup, and web serving are unconditional modules of the control crate.  
Proposal: Move the `wamn` binary and development engine into `services/dev`, with a library target for its tests. That service composes package, release, generation, and environment APIs. Keep shutdown signals and child supervision together there.  
Deletes: `wamn-control::dev`, its umbrella exports, and the separate developer adapters in services/ctl after combining them with their implementation owner. Remove developer-only dependencies from the administrative CLI.  
Risk: Stage ordering, watch invalidation, and cooperative shutdown can regress. Move the tests at `dev.rs:806`, `dev.rs:1005`, and `dev/session.rs:499` with the service. Preserve CLI coverage in `services/ctl/tests/dev_command.rs:21`.

## Finding B2-6: Release qualification embeds application test policy in the control library

Severity: medium  
Effort: M  
Area: `crates/control/lib/src/delivery`  
Evidence: `crates/control/lib/src/delivery/qualification.rs:18`, `crates/control/lib/src/delivery/qualification.rs:119`, `crates/control/lib/src/delivery/qualification.rs:327`, `crates/control/lib/src/environment/analyse.rs:516`, `crates/control/lib/src/delivery/qualification_artifact.rs:57`.  
Problem: Qualification belongs here today because delivery produces evidence that environment apply consumes. However, the implementation also selects Receiving and WMS test names, mounts the Docker socket, and runs test executables. Supporting another application package set requires changing the control library’s closed match. Evidence consumption does not require those execution responsibilities.  
Proposal: Move application selection and case execution into the delivery tool. Keep qualification payloads, immutable tags, and registry verification beside release artifacts in the release owner. Environment apply depends only on that verification API.  
Deletes: Application case lists, Docker runner construction, and temporary test-result management from control/lib.  
Risk: The split can accept incomplete evidence or reuse evidence for another image set. Preserve `release_qualification_live.rs:48` and the exact-case requirements at `delivery/qualification.rs:143`.

## Finding B2-7: Platform installation also acts as a repository build application

Severity: medium  
Effort: L  
Area: `crates/control/lib/src/platform`, `crates/control/lib/src/platform_build.rs`  
Evidence: `crates/control/lib/src/platform/install.rs:54`, `crates/control/lib/src/platform/build.rs:45`, `crates/control/lib/src/platform/build.rs:56`, `crates/control/lib/src/platform_build.rs:236`, `crates/control/lib/src/upgrade_environment.rs:917`.  
Problem: Platform installation selects numbered runbook steps, builds package and UI outputs, and invokes `wamn-ctl` for stamping. Upgrade orchestration invokes another `wamn-ctl` for qualification. These nested CLI calls create dependencies on binary locations, argument spelling, and printed results alongside existing Rust APIs.  
Proposal: Put platform builds, stamping, and qualification preparation in the delivery tool. Installation consumes the resulting chart, image digests, package artifacts, and UI receipt. Keep provider setup and installation orchestration in services/ctl, with pure resource rendering in provisioning.  
Deletes: Build-machine discovery and build execution from installation. Remove self-invocation of `wamn-ctl`, duplicate stamp argument builders, and duplicate image preparation in upgrade-environment.  
Risk: Image reuse and repeated installation can change. Retain `platform_apply_kind.rs:98`, plus `platform_build.rs:2318` and `platform_build.rs:2397` for stamping and build-before-push ordering.

## Finding B2-8: Publication mixes release construction with local runtime adaptation

Severity: medium  
Effort: L  
Area: `crates/control/lib/src/publish_release.rs`, `component_admission`, `package_artifact.rs`  
Evidence: `crates/control/lib/src/publish_release.rs:315`, `crates/control/lib/src/publish_release.rs:329`, `crates/control/lib/src/publish_release.rs:397`, `crates/control/lib/src/publish_release.rs:659`, `crates/control/lib/src/package_artifact.rs:46`.  
Problem: Publication combines immutable release construction, component composition, artifact admission, environment-policy reads, and local runtime facts. The local assembler accepts a request with database URLs that it does not use. This coupling makes the reusable release logic depend on both deployment context and developer runtime representation.  
Proposal: Give release-control ownership of component admission, package publication, release composition, and immutable release publication. Separate release construction from its filesystem, database-read, and OCI adapters. The developer service converts the result into local runtime facts. Reuse `wamn-package-artifact` for transport and verified reads.  
Deletes: `LocalApplicationFacts` construction from publication, unused database inputs from assembly, and duplicated local-versus-registry assembly paths. Remove the umbrella after its remaining responsibilities move.  
Risk: Local and published releases can acquire different identities. Preserve `release_one_digest.rs:1`, its OCI-only publication assertions, and package digest tests.

## Finding B2-9: Provisioning APIs expose CLI output paths to internal callers

Severity: medium  
Effort: M  
Area: `crates/control/lib/src/provision_project_env`, `crates/control/lib/src/environment/ensure.rs`  
Evidence: `crates/control/lib/src/provision_project_env.rs:483`, `crates/control/lib/src/provision_project_env.rs:386`, `crates/control/lib/src/environment/ensure.rs:75`, `crates/control/lib/src/environment/ensure.rs:116`, `crates/control/lib/src/provision_project_env/registry.rs:423`.  
Problem: Internal provisioning chooses behavior from `emit_*` paths. Environment reconciliation requests files, uses `-` as a sentinel, then reads and removes credential artifacts. Pure SQL composition also remains in control/lib despite the provisioning crate’s stated ownership. Environment projection updates live in provisioning, while the matching instance-claim statement lives in schema-control.  
Proposal: Keep registry coordinates, placement, policy declarations, and registry SQL in registry. Keep substrate SQL, credential rendering, and projection identity rules in provisioning. Move effectful provisioning into environment-control and return credential objects directly. Services/ctl alone converts those results into requested files. Move the projection claim beside its refresh rule.  
Deletes: Temporary credential file round trips in environment ensure, internal `emit_*` fields and `-` sentinels, pure SQL builders in control/lib, and the misplaced schema-control instance-claim module.  
Risk: Credential output permissions, projection identity, and refresh-versus-claim ordering can regress. Preserve `provision_project_env/tests.rs:1025`, including concurrent identity conflicts and missing-projection refusal. Keep file-output security tests at the CLI boundary.

## Open questions for the owner

1. Can `upgrade-environment` be retired after migrating its resume records and credential-retirement behavior into the current lifecycle?
2. Which application owner approves qualification case sets for packages beyond Receiving, Acme Receiving, and WMS?
3. Must external consumers retain the `wamn-control` Rust API, or can the umbrella disappear without a compatibility facade?

## Proposed fix epics

### One owner for environment mutation

1. Replace upgrade-environment with delivery preparation and one locked transition
2. Consolidate provisioning grants and environment identity rules
3. Return provisioning results directly and keep file emission in the CLI

### Separate developer and delivery applications

1. Move the developer binary and session engine into services/dev
2. Move release and upgrade qualification execution into tools/delivery
3. Make platform installation consume prepared build artifacts

### Replace the control umbrella

1. Extract the sole package executor and pure upgrade evidence rules
2. Extract release construction, admission, and publication
3. Consolidate process adapters and prove bounded cleanup

<!-- lane B3, bead wamn-m0c2w.3 -->

## Part 3: target map

The target contains 44 Cargo packages, including the vendor package, across three first-party workspaces. This uses the same counting unit as the 88-package scope. Deployment binaries and component exports retain their separate identities.

The current inventory is 45 native packages, 34 guest packages, five `no_std` packages, Modbus, two standalone test fixtures, and one vendor package. Evidence: `Cargo.toml:3`, `apps/Cargo.toml:10`, `apps/platform/no-std/Cargo.toml:24`, `integrations/modbus/Cargo.toml:1`, `crates/control/lib/tests/fixtures/connector/Cargo.toml:1`, `crates/control/lib/tests/support/http-fixture/Cargo.toml:1`, and `vendor/pg-walstream/Cargo.toml:15`.

This audit changed no files or tracker state. No builds or tests ran. The final Git status was clean.

## Finding B3-1: Shared manifest contracts pull the compiler into deployed services

Severity: high  
Effort: L  
Area: crates/schema, crates/control/provision  
Evidence: `crates/control/provision/Cargo.toml:43`, `crates/control/provision/src/operation_grants.rs:14`, `crates/schema/control/src/package_migrations.rs:15`, `crates/schema/generator/src/lib.rs:76`, `services/scenario-worker/Cargo.toml:18`; measurements.md section 10, `wamn-control-provision` and `wamn-runtime` rows.  
Problem: Provisioning and migration planning depend on the generator to read package declarations and validate operation names. That dependency also reaches deployed runtime users. It prevents the compiler from becoming a separate tooling owner without dependency cycles.  
Proposal: First extract `PackageManifest`, its parser, operation vocabulary, and pure schema representation into `wamn-contracts`. Move the current refusal vocabulary with the parser and preserve boundary translations. Then combine generation, database introspection, composition, and virtualization under `wamn-build`. Deployed services depend on contracts and provisioning, with no dependency on `wamn-build` or `wamn-control`.  
Deletes: Generator dependencies from provisioning and schema control. Remove the execution-host normal generator dependency after moving its test uses. Delete duplicate public re-exports after callers move to their owning modules.  
Risk: Parser refusals, operation grants, and migration decisions can drift during extraction. Existing evidence includes `business_error_details_are_closed` at `crates/schema/generator/tests/platform_generation.rs:40` and `authored_sql_access_must_match_declared_reads_and_row_locks` at line 63. Keep the live authority coverage in `crates/control/provision/tests/family_denial_matrix.rs:1016` and `crates/identity/project-state/tests/authority.rs:174`. Those tests establish behavior, while the dependency guard must also establish the new service graph.

## Finding B3-2: Forty-four packages can retain the enforced execution boundaries

Severity: high  
Effort: L  
Area: Cargo.toml, apps/Cargo.toml, crates, services  
Evidence: `tools/repo-lint-dependencies:7`, `tools/repo-lint-dependencies:32`, `tools/repo-lint-dependencies:118`, `services/cdc-reader/Cargo.toml:3`, `services/host/Cargo.toml:14`; measurements.md section 2, control, runtime, engine, and generator rows.  
Problem: The current package count mixes useful isolation with small implementation packages. A blanket merge loses enforced boundaries around the engine, edge, and workflow. Conversely, keeping every current owner as a package leaves little room to halve the count.  
Proposal: Adopt the following complete map. Dependencies below are allowed direct repository dependencies in production. External libraries remain subject to the existing capability restrictions. Tests use the separate permissions stated for T17 and T18.

T01. `wamn-contracts`: Pure catalog, authoring, registry, component-policy, event, execution, history, naming, and SQL vocabulary. It absorbs `wamn-catalog`, `wamn-authoring-model`, `wamn-control-registry`, `wamn-component-policy`, `wamn-number-pattern`, pure `wamn-project-state`, `wamn-pg-core`, `wamn-execution-contract`, `wamn-event-wire`, `wamn-event-reg`, `wamn-materializer`, and `wamn-record-history`. It also receives the manifest and schema types from B3-1. Allowed repository dependencies: none. Place it under `apps/platform/` so guests remain within their workspace. Move database floors into test support and bootstrap SQL into provisioning.

T02. `wamn-guest-sdk`: Generated data helpers and typed PostgreSQL statement access. It absorbs `wamn-data-access` and `wamn-postgres-statements`. Allowed repository dependencies: T01.

T03. `wamn-session`: Session verification over an injected key source. Retain its separate package. Allowed repository dependencies: none.

T04. `wamn-run-state`: Run decisions, storage traits, and their SQL plans. Retain its separate package. Allowed repository dependencies: T01.

T05. `wamn-engine`: Component admission, Wasmtime lifecycle, dispatch, and route bindings. Allowed repository dependencies: T01, T03, T04.

T06. `wamn-provision`: Pure provisioning, grants, package migration plans, and run-plane declarations. It combines `wamn-control-provision` and `wamn-schema-control`. Allowed repository dependencies: T01, T04.

T07. `wamn-identity`: Identity persistence, credentials, identity HTTP client, and identity service binary. It combines `wamn-platform-identity`, `wamn-credential-store`, `wamn-identity-client`, and the existing identity service. Allowed repository dependencies: T01, T03, T06.

T08. `wamn-runtime`: Cloud capability adapters, route execution authority, delivery bridge, and artifact transport. It combines `wamn-runtime`, `wamn-execution-host`, and `wamn-package-artifact`. Allowed repository dependencies: T01, T03, T04, T05, T06, T07.

T09. `wamn-workflow`: Wiring walk, lowering, durable queue, and workflow operations. It absorbs `wamn-router` as a private algorithm module. Allowed repository dependencies: T01, T04, T05, T08.

T10. `wamn-build`: Package generation, introspection, component composition, and virtualization. It combines `wamn-schema-generator`, `wamn-schema-introspection`, `wamn-component-composer`, and `wamn-component-virtualizer`. Allowed repository dependencies: T01.

T11. `wamn-control`: Delivery, environment lifecycle, developer orchestration, and command binaries. It absorbs `wamn-ctl`, while retaining `wamn`, `wamn-ctl`, and `wamn-ctl-ops` executable names. Allowed repository dependencies: T01, T03–T10.

T12. `wamn-edge`: Edge service and local SQLite intent storage. It absorbs `wamn-run-state-sqlite`. Allowed repository dependencies: T01, T03, T04, T05. Preserve the separate `web` feature and the default edge restrictions.

T13. `wamn-host-services`: Separate `wamn-host` and `wamn-scenario-worker` binaries in one package. It owns composition and process startup, without combining their processes or credentials. Allowed repository dependencies: T01, T03–T09.

T14. `wamn-cdc-reader`: Replication reader and acknowledged event publication. Retain its separate package and deployment. Allowed repository dependencies: T01, T06, T44. It retains no runtime dependency.

T15. `wamn-client`: Native generated-client transport. Retain its separate package. Allowed repository dependencies: T01.

T16. `wamn-test-postgres`: Private PostgreSQL processes and prerequisite handling. Retain this small test foundation. Allowed repository dependencies: none.

T17. `wamn-test-support`: Fixture packages, environment setup, database floors, and gate helpers. It combines `wamn-fixture-package`, `wamn-test-infrastructure`, and `wamn-gate-harness`. Allowed repository dependencies: T01, T03–T11, T16. Production packages use it only through development dependencies.

T18. `wamn-tests`: Conformance, integration, system, Receiving, and WMS test targets, plus their existing command binaries. It absorbs the four `tests/` packages, two application test packages, `wamn-bench`, `wamn-submodule-check`, and `http-fixture`. Allowed repository dependencies: T01–T17 and guest libraries or artifacts needed by a named test. No production package depends on T18.

T19. `receiving`: Receiving component, including its former data crate as private modules. Allowed repository dependencies: T01, T02.

T20. `wms`: WMS component, including its former data crate as private modules. Allowed repository dependencies: T01, T02.

T21. `client-acme-receiving`: Acme overlay component and private data modules. Allowed repository dependencies: T01, T02. Base access remains component composition.

T22. `device`: Device-frame application component. Allowed repository dependencies: none.

T23. `samples`: Sample ingestion component and private data modules. Allowed repository dependencies: T01, T02.

T24. `modbus-reader`: Application adapter for the bound Modbus capability. Allowed repository dependencies: none.

T25. `netsuite`: NetSuite application component. Allowed repository dependencies: none. Integration-base access remains component composition.

T26. `rest-connector`: REST integration application component. Allowed repository dependencies: none. Integration-base access remains component composition.

T27. `rest-consumer-fixture`: Consumer application fixture. Allowed repository dependencies: T02.

T28. `wamn-integration`: Shared integration application component. Allowed repository dependencies: T02.

T29. `fixture`: Platform application fixture and private data modules. Allowed repository dependencies: T01, T02.

T30. `fixture-overlay`: Platform overlay fixture. Allowed repository dependencies: T01, T02.

T31. `http-route`: HTTP guest adapter. Allowed repository dependencies: T01.

T32. `materializer`: Materializer guest and its effects. Allowed repository dependencies: T01.

T33. `blob-put`: Blob-writing component. Allowed repository dependencies: none.

T34. `jsonata-expression`: Sandboxed expression component. Allowed repository dependencies: none.

T35. `wamn-command-fixtures`: Separate `busyloop` and `sockprobe` binary targets. Allowed repository dependencies: none. Preserve their artifact names and separate command worlds.

T36. `connection-http-standard`: Minimal HTTP capability fixture. Allowed repository dependencies: none.

T37. `sqlx-command`: SQLx transport fixture, with the former `wamn-postgres-sqlx` driver as private modules. Allowed repository dependencies: none.

T38. `std-virtualization-probe`: Standard-library virtualization fixture. Allowed repository dependencies: none.

T39. `http-page`: Paginated HTTP palette component. Allowed repository dependencies: none.

T40. `http-request`: HTTP request palette component. Allowed repository dependencies: none.

T41. `label-render`: Label component and private template renderer. It absorbs `label-template`. Allowed repository dependencies: none.

T42. `transform`: Transform palette component. Allowed repository dependencies: none.

T43. `wamn-modbus`: Modbus capability implementation, moved inside the `no_std` workspace. Allowed repository dependencies: none.

T44. `pg_walstream`: Existing patched vendor dependency. Retain its source and independent dependency identity. Allowed repository dependencies: none.

The total is 16 native packages, 22 standard guest packages, five `no_std` packages, and one vendor package.

Deletes: Net 44 package manifests, their redundant dependency declarations, and obsolete compatibility re-exports. Retain domain modules, deployment binaries, component declarations, and wire contracts.  
Risk: Package consolidation changes Cargo feature selection and compilation scope. It does not establish faster builds. Preserve the engine and edge exclusions in `tools/repo-lint-dependencies:99` and the workflow separation at line 118. Retain session behavior through `crates/platform/runtime/tests/session_verifier.rs:18`, SQLite recovery through `crates/execution/run-state-sqlite/tests/intent_store.rs:160`, and manifest identity through `crates/catalog/model/tests/serving_manifest_digest.rs:128`.

## Finding B3-3: Migration must follow dependency order and distinguish artifact changes

Severity: high  
Effort: L  
Area: tools/repo-lint-dependencies, tests/conformance, dependency migrations  
Evidence: `tools/repo-lint-dependencies:110`, `tests/conformance/src/manifest_dependencies.rs:292`, `tests/conformance/tests/repo_lint.rs:48`, `tests/conformance/tests/guest_workspace_closure.rs:344`, `docs/operations/running-tests.md:22`.  
Problem: The requested `dependency_boundary` tests no longer exist under that name. Their current replacement is `tools/repo-lint-dependencies`, called by `tools/repo-lint`. Manifest identity tests enforce inheritance, but they cannot prove preserved runtime behavior or guest bytes.  
Proposal: Use the following dependency order. “Mechanical” means source ownership changes with no intended runtime behavior change. Guest moves also change artifact identity and require new pins.

1. Extract the pure T01 contracts before merging their consumers. Preserve parser refusals, serialized literals, canonical bytes, and test-only constructors. Mechanical source extraction. Evidence: `platform_generation.rs:40` and `serving_manifest_digest.rs:128`, cited above. Run `manifest_dependencies::governed_dependency_identities_are_workspace_owned` after changing both workspace dependency tables.

2. Redirect run-state, engine, session consumers, provisioning, and schema control to T01. Merge schema control into T06 without moving introspection effects into it. Mechanical ownership change. Prove grants with `crates/control/provision/tests/family_denial_matrix.rs:1016` and installed schema authority with `crates/control/provision/tests/deploy_sql_authority.rs:296`. Run the dependency guard before proceeding.

3. Merge introspection, generation, composition, and virtualization into T10. Their callers now consume T01 declarations instead of importing the compiler for types. Mechanical native ownership change. Retain generation cases at `crates/schema/generator/tests/platform_generation.rs:760` and composition coverage at `crates/control/lib/tests/application_composition.rs:310`.

4. Merge identity persistence, credential storage, client, and service into T07. Keep T03 separate because the engine and edge must verify sessions without identity transport. Mechanical ownership change. Prove acceptance boundaries with `crates/platform/runtime/tests/session_verifier.rs:87` and signed caller handling with `crates/platform/runtime/tests/session_route_authentication.rs:289`.

5. Merge artifact transport and execution host into T08. Then merge the router into T09, while keeping T09 above T08. Mechanical native ownership change. Prove artifact verification with `crates/platform/runtime/tests/release_manifest_source.rs:128`, claim behavior with `crates/platform/runtime/tests/production_claim_live.rs:601`, and wiring behavior with `crates/execution/workflow/tests/wiring_lowering.rs:279`.

6. Move SQLite storage into T12. Group host and scenario binaries in T13, while retaining CDC as T14. Mechanical source change with Cargo feature changes. Prove SQLite recovery at `crates/execution/run-state-sqlite/tests/intent_store.rs:160`. Keep the native and aarch64 edge graph checks at `tools/repo-lint-dependencies:110`. Existing edge route, device, forward, and release suites remain required.

7. Fold CLI parsing into T11. Retain its executable names and keep T11 outside deployed service dependencies. Mechanical ownership change. Existing environment coverage starts at `crates/control/lib/tests/environment_apply_kind.rs:642`. Publication identity coverage starts at `crates/control/lib/tests/release_one_digest.rs:100`.

8. Consolidate T02, application data modules, SQLx fixture modules, command fixtures, and label templates. These are source moves with guest artifact identity changes. Use the behavior tests named in B3-4. Rebuild and repin affected components, then qualify the resulting release. Do not describe this step as byte-preserving.

9. Move Modbus into the `no_std` workspace and remove the two standalone fixture workspaces as described in B3-5. These change build inputs and artifact identity. Run guest workspace closure and the two-checkout digest comparison at `tests/conformance/tests/guest_workspace_closure.rs:256` and line 344.

10. Consolidate test and tool ownership into T17 and T18. Update test selection, Docker inputs, package selectors, and path-based readers in the same change. Mechanical ownership change. Preserve selection coverage at `tests/conformance/tests/test_changes.rs:431` and runner coverage at `tests/conformance/tests/repo_lint.rs:165`.

Update dependency guards with each owning move, rather than disabling them until the migration ends. Preserve the engine’s exact upstream exceptions. Replace references to retired package names with the new owners.

Deletes: Obsolete package-name assertions, retired dependency edges, and duplicated runner selections. Do not retain empty compatibility packages.  
Risk: The existing lint runner tests use fake Cargo output at `tests/conformance/tests/repo_lint.rs:48`. They prove command construction, not the real dependency graph. The real guard and behavior suites remain separate evidence. No migration or passing test result is claimed by this read-only audit.

## Finding B3-4: Guest data crates and single-consumer helpers can become modules

Severity: medium  
Effort: M  
Area: apps/*/data, apps/platform/data, apps/platform/no-std  
Evidence: `apps/wamn_receiving/data/src/lib.rs:17`, `apps/wamn_wms/data/src/lib.rs:26`, `apps/platform_fixture_overlay/component/src/lib.rs:8`, `apps/platform/fixtures/sqlx-command/Cargo.toml:11`, `apps/platform/no-std/label-render/src/lib.rs:44`.  
Problem: Five applications place their typed data implementation in a separate package from the component that owns it. The overlay fixture already includes generated data directly in its component. SQLx has one repository consumer, while label rendering separates its pure helper despite the current guest already disabling its panic runtime for native tests.  
Proposal: Move the five data crates into private modules of Receiving, WMS, Acme, Samples, and the platform fixture. Merge shared data helpers with typed statements in T02. Move the SQLx driver into `sqlx-command`, and template rendering into `label-render`. Keep the HTTP, JSONata, blob, and other component artifacts separate because they represent different admitted capabilities.  
Deletes: Five application data manifests, the PostgreSQL SQLx library manifest, the label-template manifest, and the separate data-access manifest. Remove emitter references to the retired Rust crate names.  
Risk: Module paths, generated imports, transaction ownership, and guest imports can drift. Existing behavior evidence includes `sqlx_command_commits_rolls_back_and_obeys_current_user_rls` at `crates/platform/runtime/tests/sqlx_transaction_live.rs:264`. Preserve the label vectors at `apps/platform/no-std/label-template/tests/golden.rs:56`. Receiving and WMS retain their local business suites, whose shared runtime owner is documented at `docs/operations/running-tests.md:172`. The Receiving build test at `tests/conformance/tests/package_build.rs:63` also establishes component recording and virtualization.

## Finding B3-5: Three first-party workspaces are sufficient, but feature isolation must remain

Severity: medium  
Effort: M  
Area: apps/platform/no-std, integrations/modbus, standalone test fixtures  
Evidence: `apps/platform/no-std/Cargo.toml:5`, `apps/platform/no-std/Cargo.toml:15`, `integrations/modbus/Cargo.toml:18`, `integrations/modbus/src/lib.rs:8`, `crates/control/lib/tests/integration_artifact.rs:14`, `tests/conformance/tests/guest_workspace_closure.rs:42`.  
Problem: The checkout has six first-party workspace roots. Two exist for small test fixtures, and Modbus has its own root despite using the shared `no_std` runtime. However, combining standard and `no_std` guests reintroduces a recorded panic-handler conflict and forbidden WASI imports.  
Proposal: Retain the native, standard guest, and `no_std` workspaces. Relocate Modbus under the `no_std` root so its dependencies remain inside that workspace. Move `http-fixture` into T18 as a named binary. Replace the tiny checked-in connector fixture package with fixture source literals in its existing scratch-repository builder. The test still builds a real independent connector in scratch space. Keep the vendor package excluded.

Deletes: The Modbus standalone workspace declaration, both checked-in fixture manifests and lockfiles, and their standalone build contexts. Delete duplicated dependency and lint declarations where workspace inheritance replaces them.  
Risk: Keep the standalone integration-build property, rather than replacing it with a prebuilt component. Its existing proof is `build_digest_tracks_source_bytes_and_survives_another_checkout` at `crates/control/lib/tests/integration_artifact.rs:57`. Guest closure at `tests/conformance/tests/guest_workspace_closure.rs:256` prevents path escapes. The identity guard at `tests/conformance/src/manifest_dependencies.rs:292` currently scans only native and standard guest workspaces. Extend its existing scan to the retained `no_std` workspace.

## Finding B3-6: Test package boundaries can collapse without exporting test authority

Severity: medium  
Effort: M  
Area: tests, test-support, application tests, Rust tools  
Evidence: `tests/orchestrator/src/main.rs:10`, `tests/integration/src/lib.rs:16`, `test-support/harness/Cargo.toml:14`, `test-support/infrastructure/Cargo.toml:34`, `test-support/postgres/Cargo.toml:11`, `crates/catalog/model/Cargo.toml:8`.  
Problem: The gate binary already routes commands into separate test packages. Application tests consume shared integration helpers, while harness and infrastructure both assemble substantial platform environments. Merging their package ownership is possible, but moving test construction authority into production dependencies breaks an existing compile-time restriction.  
Proposal: Use T17 for reusable fixtures and T18 for test execution, tool binaries, and named suites. Preserve test tiers as modules and test targets. Keep T16 independent so lower-level tests can start PostgreSQL without importing the full environment harness. Move database floors out of T01 while retaining feature-gated constructors for synthetic contract values. Keep application test sources in their application homes through explicit target paths.  
Deletes: Seven redundant manifests from the eight test and tool packages combined into T18. Delete two redundant support manifests. Remove facade imports between the merged owners.  
Risk: Test discovery, executable paths, and feature propagation can change. Preserve manifest inheritance and the constructor restriction through `tests/conformance/src/manifest_dependencies.rs:292` and line 320. Retain suite-selection evidence at `tests/conformance/tests/test_changes.rs:460` and zero-executed-case refusal at line 707. Update the current catalog-specific constructor test at `tests/conformance/src/manifest_dependencies.rs:300` to the new contract owner. A renamed test must retain the production restriction.

## Open questions for the owner

1. Does any external automation require the current Cargo package names, rather than the retained executable and component names?
2. Does the owner accept broader Cargo feature selection for the grouped host and scenario binaries? Their processes and credentials remain separate.
3. Can the guest migration and required component repinning occur in one release qualification cycle?

## Proposed fix epics

### Separate contracts from compilation

1. Extract shared manifest, schema, and execution contracts
2. Consolidate provisioning and package build owners
3. Enforce compiler-free deployed dependency graphs

### Consolidate native owners

1. Consolidate identity, runtime, and workflow implementations
2. Consolidate control and cloud service package ownership
3. Consolidate test support, suites, and tool binaries

### Consolidate guest ownership

1. Fold application data and single-consumer helpers into owners
2. Reduce first-party workspaces to three
3. Repin migrated components and qualify the release
