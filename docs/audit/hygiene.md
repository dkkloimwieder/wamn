# Audit lane H: Hygiene

Three read-only Codex gpt-6.1-sol runs at reasoning effort high wrote this file, one per part, on 2026-10-31 against commit 8c831349d. Root copied each run's final message verbatim and checked that every finding cites a file and line. Beads: wamn-lt36n.1, wamn-lt36n.2, wamn-lt36n.3. Shared tool output: [measurements.md](measurements.md).

<!-- lane H1, bead wamn-lt36n.1 -->

## Part 1: tools

This audit inspected `tools/` at `8c831349d`. The directory contains 7,634 lines across scripts, Rust tools, and support files. Syntax parsing passed for 33 shell files and 10 Python files. No files changed, and no builds or runtime tests ran. Tests named below are proposed acceptance evidence.

ShellCheck results are unavailable. `measurements.md section 11` records that ShellCheck was not installed. Source inspection cannot establish a clean ShellCheck result.

## Finding H1-1: The image cache omits actual build inputs

Severity: high  
Effort: M  
Area: `tools/journey-image-cache`

Evidence: `tools/journey-image-cache:19`, `tools/journey-image-cache:111`, `Dockerfile:36`, `Dockerfile:44`, `Dockerfile:124`, `crates/control/lib/src/upgrade_environment.rs:394`.

Problem: The cache lists its copied inputs manually, but that list omits `vendor`, `tools/bench`, `tools/submodule-check`, and `tools/guest-rustflags`. Docker copies these paths. Changes confined to omitted inputs leave the cache identity unchanged and can reuse stale images.

Proposal: Merge production image preparation into `wamn-control` and its existing platform build owner. Let Docker handle build caching, or derive any retained source identity from the complete build context.

Deletes: The parallel `COPIED` inventory, custom source-tag selection, and the production dependency on `tools/journey-image-cache`.

Risk: Image reuse and qualification can select different artifacts. Extend `tests/conformance/tests/journey_image_cache.rs` with vendor-only and tool-only changes, and preserve the private-registry assertions in `registry_image_archive.rs`.

## Finding H1-2: Failed Git reads can produce an empty successful selection

Severity: high  
Effort: S  
Area: `tools/test-changes`

Evidence: `tools/test-changes:84`, `tools/test-changes:97`.

Problem: The script reads changed paths through process substitution, which does not propagate the Git commands’ failures to `mapfile`. If path discovery fails and produces no paths, the script prints `selected: nothing` and exits successfully.

Proposal: Complete both Git reads successfully before selecting tests. Treat unreadable change information as a failure.

Deletes: The unchecked process substitution around change discovery.

Risk: The train can accept a revision without running its selected tests. Add Git-failure cases to `tests/conformance/tests/test_changes.rs` and assert that selection fails before Cargo runs.

## Finding H1-3: The descriptor diagnostic can never pass

Severity: medium  
Effort: S  
Area: `tools/native-descriptor-limit-check`

Evidence: `tools/native-descriptor-limit-check:129`, `tools/native-descriptor-limit-check:158`, `tools/native-descriptor-limit-check:167`.

Problem: The diagnostic defines one binary and runs two modes, so it creates two cases. Its final verdict requires four passing cases, which makes every invocation fail.

Proposal: Move these assertions beside the host process tests. Derive the aggregate result from the cases actually selected.

Deletes: The standalone 178-line diagnostic after its behavior moves to the host tests.

Risk: Descriptor initialization can lose executable coverage. Run both limit modes against the host binary and prove that either failed case makes the aggregate fail.

## Finding H1-4: A missing argument can hang the Kubernetes runner

Severity: medium  
Effort: S  
Area: `tools/kubernetes-gate-run`

Evidence: `tools/kubernetes-gate-run:11`, `tools/kubernetes-gate-run:32`, `tools/kubernetes-gate-run:34`.

Problem: Value-taking arguments call `shift 2` without first requiring a second argument. The script disables automatic exit on command failure, so an incomplete argument such as `--manifest` leaves the loop on the same argument indefinitely.

Proposal: Require each argument value before advancing the parser. During the identity runner consolidation, move this parser into the existing Rust test command.

Deletes: The unchecked argument branches, followed by the shell parser when its owner moves.

Risk: Invalid invocations consume a worker instead of returning an error. Extend `tests/conformance/tests/kubernetes_gate_runner.rs` with bounded missing-value cases for every value-taking flag.

## Finding H1-5: Identity tests still own a separate shell orchestration system

Severity: medium  
Effort: M  
Area: `tools/identity-jwks-journey-run`, `tools/kubernetes-gate-run`

Evidence: `tools/identity-jwks-journey-run:58`, `tools/identity-jwks-journey-run:183`, `tools/identity-jwks-journey-run:303`, `tools/identity-jwks-journey-run:424`, `tests/orchestrator/src/main.rs:40`.

Problem: The 511-line identity runner owns builds, fixture SQL, Kubernetes documents, assertions, and cleanup alongside existing Rust identity commands. It also requires output under the old repository `evidence/` location. Its 367-line Job runner duplicates a record contract that Rust already consumes.

Proposal: Merge the journey into the existing identity test owner and shared test infrastructure. Preserve key rotation, TLS refusal, membership revocation, and secret-handling assertions.

Deletes: Both shell runners after their callers move, plus the test that extracts the identity cleanup function with `sed`.

Risk: Consolidation can lose negative authentication cases or expose fixture secrets. Preserve the verdict cases in `kubernetes_gate_runner.rs` and run the complete disposable identity journey at its required acceptance boundary.

## Finding H1-6: Application lifecycle adapters duplicate resource management

Severity: medium  
Effort: M  
Area: `tools/receiving-cluster-journey-run`, `tools/wms-cluster-journey-run`, `tools/rc-gate-run`

Evidence: `tools/receiving-cluster-journey-run:14`, `tools/wms-cluster-journey-run:14`, `tools/rc-gate-run:19`, `tools/delivery-owned:85`. `measurements.md section 7` records 78 duplicated lines between the Receiving and WMS scripts.

Problem: These adapters repeat cluster creation, image import, operator installation, inspection, and cleanup. Their Rust callers already own the test lifecycle, but delegate shared infrastructure behavior to separate shell interfaces.

Proposal: Merge common process operations into `test-support/infrastructure`. Keep application assertions with their applications and retain exact resource ownership and leftover reporting.

Deletes: The three lifecycle scripts and duplicated image-loading loops. Reduce `delivery-owned` to retained repository launch commands.

Risk: Cleanup can remove the wrong resources or hide leftovers. Preserve `kind-cluster-delete-test` scenarios and both applications’ `owned_release_delivery` cases.

## Finding H1-7: Compact benchmark results lose their identity and failure detail

Severity: medium  
Effort: M  
Area: `tools/bench/result.py`, `tools/bench/local.sh`, `tools/bench/tier.sh`

Evidence: `tools/bench/result.py:7`, `tools/bench/local.sh:528`, `tools/bench/src/main.rs:149`, `tools/bench/tier.sh:21`.

Problem: The Rust result records workload, duration, conflicts, and detailed errors, but the compact writer drops most of those fields. The local runner writes guest metadata that this writer never reads. The tier runner deletes its local raw capture after producing the compact result.

Proposal: Retain one result contract with source and artifact identity, workload configuration, completed attempts, conflicts, errors, and measurement boundaries. Make compact summaries derived views of a retained result.

Deletes: The unused `metadata.json` handoff and independently maintained compact field selection.

Risk: Existing baseline readers depend on the current format. Use saved results to prove conversion, including a contention result with conflicts and a result with request errors.

## Finding H1-8: The cloud floor runner leaves work active after failure

Severity: medium  
Effort: M  
Area: `tools/bench/pg-floor/gcp.sh`

Evidence: `tools/bench/pg-floor/gcp.sh:59`, `tools/bench/pg-floor/gcp.sh:116`, `tools/bench/pg-floor/gcp.sh:149`, `tools/bench/pg-floor/gcp.sh:155`, `tools/bench/db-profile.sh:7`.

Problem: The exit handler deletes only the client Job, although the runner starts a background PostgreSQL sampler. A failure during the replay can also bypass trigger restoration in `wamn_pg_floor`. The initial Pod-discovery loop has no deadline.

Proposal: Give the floor run one cleanup owner that stops sampling, restores altered triggers, and removes its client. Bound Pod discovery and use a distinct run identity for the disposable resources.

Deletes: The Job-only cleanup handler and unbounded discovery loop.

Risk: Failed runs can leave a sampler active or alter the next experiment’s conditions. Inject a replay failure against a disposable floor database and prove cleanup and trigger restoration.

## Finding H1-9: The train runner omits its documented submodule gate

Severity: medium  
Effort: S  
Area: `tools/train-run`, `tools/submodule-check`

Evidence: `tools/train-run:73`, `tools/train-run:80`, `tools/submodule-check/src/main.rs:33`, `docs/operations/running-tests.md:139`.

Problem: The operating instructions say that the train runs the submodule guard before integration. The runner initializes submodules and runs test selection, but never invokes that guard. No tracked automatic caller was found.

Proposal: Invoke the existing guard from the train before its test stage. Keep this repository rule outside `wamn-ctl`.

Deletes: No production functionality. Remove any duplicate external invocation once the train owns it.

Risk: A backward or undeclared pin can reach integration. Extend `tools/train-tools-test` with guard failure propagation and retain the submodule tool’s ancestry cases.

## Finding H1-10: Four installers repeat one installation algorithm

Severity: low  
Effort: S  
Area: `tools/install-helm`, `tools/install-kcl`, `tools/install-tofu`, `tools/install-wash`

Evidence: `tools/install-helm:19`, `tools/install-kcl:18`, `tools/install-tofu:19`, `tools/install-wash:17`, `Dockerfile:283`.

Problem: The installers repeat platform refusal, digest validation, temporary storage, download, permissions, and final replacement across 172 lines. Helm and KCL pins also appear independently in the Dockerfile.

Proposal: Keep four small entry points and one installer template. Each entry supplies its version, URL, archive digest, binary digest, and extraction member. Share one pin declaration with image construction.

Deletes: Repeated installer bodies and duplicate pin declarations.

Risk: KCL callers require stdout to contain only the binary path. Prove cached reuse, corrupt downloads, corrupt cached binaries, raw-file installation, archive extraction, and unsupported-platform refusal with local fixtures.

## Finding H1-11: Repository launchers repeat shell plumbing

Severity: low  
Effort: S  
Area: `tools/repo-lint`, `tools/test-changes`, `tools/contract-diff`, `tools/wasmcloud-release-check`

Evidence: `tools/repo-lint:18`, `tools/contract-diff:18`, `tools/contract-diff:52`, `tools/wasmcloud-release-check:21`, `tools/wasmcloud-release-check:92`.

Problem: The scripts copy root discovery, executable lookup, error output, and command-plan printing. Each copy maintains the same operational rules separately.

Proposal: Add one small `tools/lib/common.sh` with error output, repository discovery, executable lookup, and command printing. Keep step aggregation local because the runners have different failure behavior. Keep `require-test-result` as the existing executable boundary.

Deletes: Repeated plumbing in the retained launchers.

Risk: Sourcing a helper can change working directories, traps, or stdout. Keep the library free of those side effects and retain each runner’s directory, dry-run, and failure-propagation cases.

## Finding H1-12: Retired and unreferenced tools obscure the supported surface

Severity: low  
Effort: S  
Area: `tools/rename-app-system-to-platform`, `tools/kind-gate-build`, `tools/kind-gate-image-remove`

Evidence: `tools/rename-app-system-to-platform:4`, `tools/rename-app-system-to-platform:70`, `tools/kind-gate-build:93`, `tools/kind-gate-build:109`, `tools/kind-gate-image-remove:8`.

Problem: The rename tool preserves an applied one-time transformation and defaults to rewriting the checkout. The gate builder has no tracked caller, while the image remover’s tracked operational reference comes from that builder. Together these scripts retain 653 lines outside the documented delivery path.

Proposal: Delete the rename tool and the manual gate build/removal pair. Use the existing platform build and owned disposable cluster lifecycle.

Deletes: All three scripts and tests whose sole subject is the removed command surface.

Risk: Off-repository workflows can still call these commands. Resolve that ownership question before deletion, then prove the supported platform build and owned-cluster cleanup paths retain their required behavior.

### Per-script decisions

Paths in this table are relative to `tools/`. “No caller” means no tracked external reference was found, including hidden repository files.

| Script or Rust tool | Lines | Purpose and referrers | Decision |
| --- | ---: | --- | --- |
| `cargo-cranelift` | 22 | Native debug build adapter. Dockerfile and `cranelift_dev.rs`. | Keep. |
| `contract-diff` | 87 | Run authoring and HTTP contract tests. Test instructions and `contract_diff.rs`. | Keep, share shell helpers. |
| `delivery-owned` | 112 | Native build, delivery launch, image loading, teardown. Application fixtures and delivery harness. | Merge infrastructure actions into their Rust owner. Keep launch commands. |
| `dev-collector` | 38 | Render local collector files. Local benchmark and Receiving timing example. | Merge into `wamn-control::dev`. |
| `guest-rustflags` | 26 | Print reproducible guest build flags. Generator, Dockerfile, tests, benchmark. | Keep one shared build boundary. |
| `http-reuse-run` | 147 | Start a private registry and run one exact live test. Test instructions. | Merge fixture lifecycle into integration test support. |
| `identity-jwks-journey-run` | 511 | Deployed key and session journey. Cleanup test, architecture and plan references. | Merge into Rust identity tests. |
| `install-helm` | 45 | Install pinned Helm. No caller. | Merge installer body. Expose through documented prerequisites. |
| `install-kcl` | 44 | Install pinned KCL. Generator authoring code and build instructions. | Merge installer body, keep entry point. |
| `install-tofu` | 45 | Install pinned OpenTofu. GCP runbook. | Merge installer body, keep entry point. |
| `install-wash` | 38 | Install pinned wash. Upgrade operation and registry instructions. | Merge installer body, keep entry point. |
| `journey-image-cache` | 222 | Cache, relabel, lease, and retain images. Upgrade operation and registry tests. | Merge production behavior into `wamn-control`. |
| `kind-cluster-delete` | 33 | Bounded deletion and leftover inspection. Lifecycle adapters and Rust tests. | Keep one implementation until shared Rust infrastructure replaces it. |
| `kind-cluster-delete-test` | 153 | Fake-command teardown tests. No tracked caller. | Merge into discoverable conformance tests. |
| `kind-gate-build` | 114 | Build and load a gates image. No caller. | Delete. |
| `kind-gate-image-remove` | 434 | Plan and remove retired node images. Gate builder and conformance tests. | Delete with the retired builder. |
| `kind-kept-delete` | 25 | Remove resources named by `kept.json`. Environment tests and test instructions. | Merge into shared test infrastructure. |
| `kubernetes-gate-run` | 367 | Run Jobs and produce verdict records. Identity runner and conformance tests. | Merge into the Rust test owner. |
| `lane-start` | 90 | Prepare checkout, binaries, dependencies, and packages. Build and test instructions. | Keep as a repository bootstrap command. |
| `local-registry` | 41 | Start or reuse a labeled host registry. Build instructions. | Merge into control-side local platform preparation. |
| `native-descriptor-limit-check` | 178 | Exercise descriptor limits in host processes. No caller. | Merge into host tests. |
| `rc-gate-run` | 63 | RC test lifecycle adapter. `tests/integration/src/rc/resources.rs`. | Merge into shared test infrastructure. |
| `receiving-cluster-journey-run` | 133 | Receiving lifecycle adapter. Receiving cluster fixtures. | Merge into shared test infrastructure. |
| `rename-app-system-to-platform` | 105 | Apply a one-time schema spelling change. No caller. | Delete. |
| `repo-lint` | 326 | Run formatting, Clippy, and repository rules. Change runner and test instructions. | Keep, share shell helpers. |
| `repo-lint-dependencies` | 127 | Enforce dependency boundaries. `repo-lint`. | Keep. |
| `require-test-result` | 18 | Refuse successful commands with no passing test cases. Multiple runners and diagnostic examples. | Keep. |
| `test-changes` | 520 | Select tests and prepare their inputs. Train and test instructions. | Keep as repository tooling. |
| `train-run` | 80 | Test a committed revision in a detached checkout. Train self-tests and instructions. | Keep, include submodule guard. |
| `train-tools-test` | 208 | Fake-command tests for selection and train behavior. Test instructions. | Keep behavior, merge into conformance discovery. |
| `wasmcloud-release-check` | 173 | Exercise exact upstream release sources. Conformance tests and cutover history. | Keep as explicit dependency-upgrade tooling. |
| `wms-cluster-journey-run` | 134 | WMS lifecycle adapter. WMS cluster fixtures. | Merge into shared test infrastructure. |
| `submodule-check/` | 411 | Detect undeclared and backward pins. Workspace manifest and train instructions. | Keep, wire into train. |

Paths below are relative to `tools/bench/`.

| Script or Rust source | Lines | Purpose and referrers | Decision |
| --- | ---: | --- | --- |
| `local.sh` | 541 | Run local application, floor, and profile experiments. Benchmark instructions. | Keep benchmark ownership, reuse development lifecycle. |
| `tier.sh` | 37 | Run deployed get/update measurements. GCP runbook and benchmark instructions. | Keep, retain full results. |
| `pg-floor/gcp.sh` | 190 | Run floor experiments on GCP. Benchmark instructions and deployment comments. | Keep with owned cleanup and bounded waits. |
| `pg-floor/run.sh` | 41 | Run the local floor experiment. Sourced by `local.sh`. | Keep beside benchmark runner. |
| `db-profile.sh` | 35 | Sample database counters and seed Receiving. Local and GCP floor runners. | Keep one helper with an explicit cleanup contract. |
| `database-delta.py` | 28 | Subtract database counters. Profile helper and GCP runner. | Keep. |
| `result.py` | 49 | Write common compact results. Local, tier, and floor result readers. | Keep one result writer, preserve identity and failure detail. |
| `pg-floor/result.py` | 68 | Interpret pgbench results and reconcile changed rows. Both floor runners. | Keep. |
| `app-result.py` | 71 | Attribute sampled application span times. `local.sh`. | Keep as benchmark analysis. |
| `name-guests.py` | 45 | Restore guest symbol names from matching artifacts. `local.sh`. | Keep as benchmark analysis. |
| `profile-summary.py` | 90 | Summarize CPU samples. `local.sh`. | Keep as benchmark analysis. |
| `src/main.rs` | 1,109 | Generate timed Receiving traffic and collect results. Local and tier runners. | Keep as a separate benchmark binary. |

The support files also have live owners. Keep `test-changes-suites.json` (8 lines) with test selection and `component-virtualization.json` (7 lines) with package building. Keep `database-snapshot.sql` (8 lines) with profiling. Keep floor SQL beside its runner: `get.sql` (21), `update.sql` (83), `receipt.sql` (36), and `receipt-slots.sql` (30). Keep `gcp-client.yaml` (42 lines) with the cloud floor runner.

Production image preparation, collector rendering, and local registry preparation belong behind `wamn-ctl`, with implementation in `wamn-control`. Repository bootstrap, lint, train, benchmark analysis, and test fixtures retain their existing kinds of owners. This follows `docs/architecture/overview.md:46` and `docs/architecture/overview.md:53`.

## Open questions for the owner

1. Does an off-repository workflow still require `kind-gate-build` or `kind-gate-image-remove`?
2. Must benchmark summaries remain readable without their raw result files? This determines the retained artifact contract.
3. Can the old identity journey’s repository `evidence/` requirement disappear when its Rust replacement owns temporary results?

## Proposed fix epics

### Tool correctness

1. Make image reuse depend on complete build inputs
2. Reject failed selection reads and incomplete runner arguments
3. Restore descriptor verdicts and enforce the train submodule gate

### Shared test infrastructure

1. Merge application lifecycle adapters into shared Rust infrastructure
2. Move identity journey orchestration into its Rust test owner
3. Move standalone fixture diagnostics into discoverable owner tests

### Shell surface reduction

1. Share minimal shell helpers and the installer template
2. Move production preparation behind existing control owners
3. Delete retired transformation and manual gate tools

### Benchmark integrity

1. Retain benchmark identity and failure detail in one result contract
2. Give cloud floor experiments complete cleanup and bounded waits
3. Reuse development lifecycle operations in the local benchmark

<!-- lane H2, bead wamn-lt36n.2 -->

## Part 2: docs

## Finding H2-1: Plans compete with reference pages for current contracts

Severity: high  
Effort: M  
Area: docs/plan, docs/architecture  
Evidence: `docs/plan/integrations.md:118`, `docs/plan/integration-schema.md:5`, `docs/architecture/data-access.md:1250`, `docs/plan/write-log.md:7`, `docs/architecture/data-access.md:375`, `docs/operations/observability.md:3`.

Problem: The integration plan declares org-scoped connections, while the schema plan and architecture describe project integrations with environment connections. The write-log plan excludes update and delete, while architecture includes both. These competing rules can lead an agent to implement the wrong credential boundary or retry behavior.

Proposal: Assign each implemented topic one reference owner. Plans contain only proposed changes and unresolved decisions.

| Duplicated topic | Pages that repeat it | One reference owner |
| --- | --- | --- |
| Integration identity, generations, credentials and bindings | `integrations.md`, `integration-schema.md`, `platform-config.md`, `data-access.md` | `architecture/data-access.md#project-integrations-and-credentials` |
| Transactions, composition and write-log retries | `write-log.md`, `trx-pkg-plan.md`, `routes-router.md`, `data-access.md` | `architecture/data-access.md#inputs-queries-and-transactions` |
| Request authentication and revocation | `request-round-trips.md`, `platform-ui.md`, `platform-config.md`, `execution.md` | `architecture/execution.md#caller-authentication` |
| Workflow identity and execution | `routes-router.md`, `workflow-feature.md`, `workflows.md`, `execution.md` | `architecture/execution.md#environment-workflows` |
| Environment lifecycle behavior | `platform-deploy.md`, `environment-teardown.md`, `overview.md` | `architecture/overview.md#environment-lifecycle` |
| Browser client and shell behavior | `platform-ui.md`, `platform-ui-app.md`, `execution.md` | `architecture/execution.md#operator-clients` |
| Telemetry storage and queries | `plan/observability.md`, `operations/observability.md` | `operations/observability.md` |

Commands retain their operations owners. A short overview can link to the detailed contract without repeating its rules.

Deletes: Repeated implemented contracts from active plans, including the SQL schema copies in `docs/plan/integration-schema.md:24`.

Risk: A proposed rule can disappear during consolidation. Compare each removed section with its reference owner and retain unresolved changes under their owning Beads issue. No runtime test is required for prose relocation.

## Finding H2-2: Closed work remains in the active plan collection

Severity: medium  
Effort: M  
Area: docs/plan/README.md  
Evidence: `docs/README.md:8`, `docs/plan/README.md:4`, `docs/plan/request-round-trips.md:3`, `docs/plan/platform-deploy.md:5`, `.beads/issues.jsonl:625`, `.beads/issues.jsonl:2179`, `.beads/issues.jsonl:3501`.

Problem: The documentation index defines plans as work that is not built, but the plan index includes completed programs. The retained Beads export records the scoped work below as closed. Live status remains unconfirmed because read-only `bd show` cannot open embedded Dolt without creating its lock file.

Proposal: Remove completed programs from `docs/plan`. Use compact entries in `docs/history/README.md` as history targets. Each entry records the Bead, completion evidence, an immutable Git source link, and its current reference owner.

| Plan | Closed work recorded in the export | History target |
| --- | --- | --- |
| `cdc-reader-slot.md` | `wamn-59z6`, export line 2179 | `history/README.md#cdc-reader-slot` |
| `component-pull.md` | `wamn-i87m`, line 3501 | `history/README.md#component-pull` |
| `environment-teardown.md` | `wamn-psss`, line 2175 | `history/README.md#environment-teardown` |
| `operator-image.md` | `wamn-n5d1`, line 2200, and `wamn-lo7z`, line 2347 | `history/README.md#operator-image` |
| `request-round-trips.md` | `wamn-stxy9` and its exported children, line 625 | `history/README.md#request-round-trips` |
| `routes-router.md` | Five delivered epics recorded at `docs/plan/routes-router.md:92` | `history/README.md#routes-router` |
| `web-deployment.md` | `wamn-xyxj`, line 2280, and `wamn-ghx2`, line 2263 | `history/README.md#web-deployment` |
| `workflow-feature.md` | `wamn-upl3` and its exported children, line 2241 | `history/README.md#workflow-feature` |
| `write-log.md` | `wamn-z7uv` and its exported children, line 2206 | `history/README.md#write-log` |
| `observability.md`, delivered collector program | `wamn-22lsr`, line 2058, and sampling work at line 2014 | `history/README.md#observability` |
| `platform-deploy.md`, nine-epic program | Explicit completion statement at `docs/plan/platform-deploy.md:5` | `history/README.md#platform-deploy` |

The export contains no nonclosed descendant rows for the listed exported epic families. This does not establish closure of separately tracked follow-ups.

Deletes: Completed plan files and their active index entries after their unique contracts and standing rules reach current owners.

Risk: Deferred work can be mistaken for completed work. Reconcile live Beads before deletion and preserve the distinctions in H2-3. Test all replacement links and history anchors.

## Finding H2-3: The previous audit gives an unsafe blanket archive list

Severity: medium  
Effort: S  
Area: docs/plan/docs-audit.md  
Evidence: `docs/plan/docs-audit.md:101`, `docs/plan/platform-ui-app.md:4`, `docs/plan/manifest-authoring.md:76`, `docs/plan/trx-pkg-plan.md:311`, `docs/plan/perf-arch.md:324`.

Problem: The previous audit lists twenty archive candidates, including plans that retain unresolved work. Platform UI paging remains open, manifest authoring retains a second epic, and transactional composition records an unbuilt administration change. The audit also reports omissions that current indexes already fix, such as the platform configuration entry.

Proposal: Treat the October 24 audit as dated evidence. Keep only unresolved design in mixed plans, linked to its owning Bead.

| Mixed plan | Work that prevents blanket deletion |
| --- | --- |
| `platform-ui-app.md` | Server paging, `wamn-n8q9m.20` |
| `manifest-authoring.md` | Epic 2 cross-field refusals |
| `trx-pkg-plan.md` | Grantable administration and other explicitly deferred consumers |
| `perf-arch.md` | Remaining performance decisions and deferred allocator work |
| `platform-config.md` | Remaining connection, file, settings and local-environment work |
| `workflows.md` | Remaining schedule, effect-writer and node-publication work |
| `package-upgrade.md` | Later upgrade policies and acceptance |
| `observability.md` | Dashboard direction, if the owner retains it as planned work |

Deletes: `docs/plan/docs-audit.md` from the active plan collection, its obsolete status inventory, and completed sections of mixed plans. Preserve the dated audit through Git or the history index.

Risk: A historical recommendation can override newer decisions. Reconcile each retained requirement with live Beads and its current reference page before removing it.

## Finding H2-4: Standing repository rules live inside a completed deployment plan

Severity: medium  
Effort: S  
Area: docs/plan/platform-deploy.md  
Evidence: `docs/plan/platform-deploy.md:16`, `docs/plan/platform-deploy.md:17`, `docs/plan/platform-deploy.md:18`, `docs/plan/platform-config.md:345`, `docs/plan/platform-install.md:5`.

Problem: The completed deployment plan holds the main-checkout restriction, platform version rule, and cloud credential restrictions. Other plans import those instructions by section name. Archiving the deployment plan without extracting them can silently remove active instructions.

Proposal: Put repository-wide rules in the canonical agent instructions. Put the platform version procedure in `operations/building.md` and cloud prerequisites in `operations/gcp.md`. Plans link to these owners.

Deletes: The standing-rule block from the deployment plan and repeated “How this plan is worked” imports from other plans.

Risk: Instruction changes affect agent behavior. Compare the extracted rules word for word and scan all tracked files for references to the retired block. No build is required.

## Finding H2-5: Local links contain five live missing paths and three missing anchors

Severity: medium  
Effort: S  
Area: docs/plan, docs/history  
Evidence: `docs/plan/wamn-sync.md:521`, `docs/plan/wamn-sync.md:586`, `docs/plan/wamn-sync.md:863`, `docs/plan/wamn-sync.md:872`, `docs/plan/wamn-sync.md:903`, `docs/plan/integrations.md:20`, `docs/plan/platform-config.md:358`.

Problem: A local scan of 105 scoped Markdown files found 27 links to absent paths and three links to absent headings. Five missing-path occurrences are in the active sync plan. All three missing headings use the retired org-connections anchor.

Proposal: Repair retained links against their actual owners.

| Broken references | Target treatment |
| --- | --- |
| Five sync links to `packaging-feed.ts`, `shell-status.ts`, `location-feed.ts`, and `saved-user.ts` | Locate the current implementation owner or remove the obsolete implementation claim |
| `integrations.md:20`, `integrations.md:124`, `platform-config.md:358` | Use `data-access.md#project-integrations-and-credentials` and correct the associated org-scoped wording |
| `history/deterministic-testing-spec.md:5` | The retained file is `history/wamn_testing_spec.md` |
| `history/operator-ui.md:10` | Use `../plan/platform-ui-app.md` |
| Three links in `history/wamn_base_application_poc_revised.md`, starting at line 54 | Use the relevant `architecture/naming.md` section, or the historical source revision |
| `history/platform-ui-history.md:1388` | Use `history/release-qualification.md` for historical context or current `operations/delivery.md` |
| Four removed deployment-file links in `history/gcp-install-history.md`, starting at line 1258 | Link to their recorded Git revision rather than restoring retired deployment files |

The remaining twelve missing paths are evidence artifacts covered by H2-6.

Deletes: Obsolete source claims and links to retired deployment files. Do not recreate removed implementation files to satisfy documentation links.

Risk: A replacement link can point to a related but different contract. Repeat the path scan, resolve headings, and inspect each replacement’s meaning. External links were not tested.

## Finding H2-6: Historical proof claims point to absent evidence

Severity: medium  
Effort: M  
Area: docs/history/consolidation-findings.md, docs/history/wamn_native_alignment_plan.md  
Evidence: `docs/history/consolidation-findings.md:139`, `docs/history/consolidation-findings.md:191`, `docs/history/consolidation-findings.md:218`, `docs/history/wamn_native_alignment_plan.md:46`, `docs/history/wamn_native_alignment_plan.md:99`, `docs/history/wamn_native_alignment_plan.md:139`.

Problem: Twelve historical links target absent files under `evidence/perf/2026.09`. Their surrounding text presents those files as proof of baselines, authenticated behavior, deployed results, and source decisions. Readers cannot inspect that proof from this checkout.

Proposal: Link to a retained artifact or immutable Git object when one exists. Otherwise, mark the artifact unavailable and retain only the independently supported claim and its Bead reference.

Deletes: Unsupported proof wording and dead artifact links when no retained source exists. Do not add placeholder evidence files.

Risk: Removing a dead link does not establish the original acceptance result. For each claim, record whether its evidence is retrievable, externally retained, or unavailable. This audit did not rerun those historical tests.

## Finding H2-7: Change histories occupy pages that readers use as references

Severity: low  
Effort: M  
Area: docs/history, docs/plan/perf-arch.md  
Evidence: `docs/history/gcp-install-history.md:3`, `docs/history/gcp-install-history.md:5`, `docs/history/platform-ui-history.md:1351`, `docs/history/kind-to-type.md:1988`, `docs/plan/request-round-trips.md:23`, `docs/plan/perf-arch.md:317`.

Problem: Several pages primarily preserve rollout steps, successive decisions, or before-and-after measurements. The performance plan mixes a dated “today” path with later rulings. These records are useful history, but they do not provide a dependable current reference.

Proposal: Use the following replacements and retain chronology through immutable Git links in the history index.

| Change history or dated record | Current reference replacement |
| --- | --- |
| `history/gcp-install-history.md` | `operations/gcp.md` |
| `history/deployment-pre-apply.md` | `operations/deployment.md` |
| `history/platform-ui-history.md` | `architecture/execution.md#operator-clients` and `architecture/overview.md#organization-membership-and-administration` |
| `history/kind-to-type.md` | `architecture/naming.md` plus the actual WIT and schema definitions |
| Completed measurements and issue sequence in `plan/request-round-trips.md` | `operations/benchmark.md` and `architecture/execution.md#native-dispatch` |
| Completed experiments and decision chronology in `plan/perf-arch.md` | `operations/benchmark.md`, current execution contracts, and remaining decisions in the reduced plan |
| `plan/docs-audit.md` | Beads for unresolved findings and Git for the dated audit |

`operations/benchmark.md` is already a procedure and baseline reference. Keep that role rather than treating every measurements page as a change log.

Deletes: Redundant historical snapshots after preserving their source revisions. The three snapshot pages `gcp-install-history.md`, `platform-ui-history.md`, and `deployment-pre-apply.md` contain 3,891 lines.

Risk: Source comments depend on old numbered sections, as `gcp-install-history.md:5` explicitly states. Rewrite those references before deletion and test them across the repository.

## Finding H2-8: Navigation omits active alignment work and lacks a history map

Severity: low  
Effort: S  
Area: docs/README.md, docs/plan/README.md  
Evidence: `docs/plan/alignment.md:163`, `docs/plan/alignment.md:165`, `docs/plan/README.md:3`, `docs/README.md:9`, measurements.md section 9.

Problem: The plan index omits `alignment.md`, although that page names a scoped epic and later work. History navigation leads to a directory rather than a map from retired documents to current owners. Measurements.md section 9 also records substantial churn in the documentation indexes and operating guides.

Proposal: Keep the existing architecture, operations, and testing split. Use this target structure:

```text
docs/
  README.md
  architecture/
    README.md
    overview.md
    execution.md
    data-access.md
    capabilities.md
    components.md
    naming.md
    native-alignment.md
    edge.md
  operations/
    README.md
    <current command and recovery pages>
  testing/
    README.md
    <current methods and result interpretation>
  plan/
    README.md
    alignment.md
    authored-views.md
    delivery.md
    identity.md
    integrations.md
    manifest-authoring.md
    nats-outage.md
    package-upgrade.md
    perf-arch.md
    platform-config.md
    platform-install.md
    platform-ui-app.md
    trx-pkg-plan.md
    wamn-sync.md
    workflows.md
  history/
    README.md
    <records with unique rationale or retained evidence>
    evidence/
  audit/
    <dated audit inputs and reports>
```

Each retained plan contains unresolved design only. The history index supplies the targets in H2-2 and the replacements in H2-7.

Deletes: Completed plan entries, copied status lists, and snapshot pages covered by the preceding findings. Add only the compact history index.

Risk: Navigation can hide a retained requirement. Test that every retained plan appears in the index and that every retired page has a successor or historical source. This lane changed no files, ran no builds or tests, and left Git status clean.

## Open questions for the owner

1. Can the root audit reconcile the archive candidates against live Beads? The passive export ends on October 22, and embedded Dolt cannot open in this read-only sandbox.
2. Does an immutable Git source plus a compact history entry satisfy the rule at `docs/plan/manifest-authoring.md:94` that accepted plans remain untrimmed?
3. Where are the twelve historical performance artifacts retained, if anywhere?

## Proposed fix epics

### One owner for each current contract

1. Consolidate implemented contracts into architecture and operations
2. Move standing rules out of the completed deployment plan
3. Reduce mixed plans to unresolved design

### Retire completed plans and copied histories

1. Reconcile closed scope against live Beads
2. Replace completed plans and snapshots with history entries
3. Move the previous audit out of active planning

### Repair documentation navigation and evidence

1. Repair missing paths and heading links
2. Resolve unavailable historical proof
3. Publish and test the documentation indexes

<!-- lane H3, bead wamn-lt36n.3 -->

## Part 3: web and stray files

Read-only audit at `8c831349d3116aac27d9ca6f59a499da1c87efc7`. No files changed. No builds or tests ran. Test references below describe proposed validation.

The six first-party Cargo lockfiles have live owners. Keep the native, guest, and isolated `no_std` lockfiles (`Cargo.toml:1`, `apps/Cargo.toml:1`, `apps/platform/no-std/Cargo.toml:3`). Keep the Modbus lockfile and both fixture lockfiles because tests copy or build those independent projects (`crates/control/lib/tests/connector_restart_live.rs:60`, `crates/control/lib/tests/integration_artifact.rs:17`, `crates/control/lib/tests/support/http-fixture/Dockerfile:3`). The seventh tracked lockfile belongs to vendored code (`vendor/pg-walstream/Cargo.lock:1`).

No byte-identical tracked files appeared within the scoped first-party web packages. The duplication consists of near-identical tests, browser support functions, and configuration.

## Finding H3-1: The parent install omits wfe dependency repairs

Severity: high  
Effort: S  
Area: `pnpm-workspace.yaml`, `pnpm-lock.yaml`, `web/wfe`  
Evidence: `pnpm-workspace.yaml:35`, `pnpm-lock.yaml:3162`, `web/wfe/pnpm-workspace.yaml:23`, `web/wfe/patches/solid-primitives-interaction-1.0.0-next.4.patch:4`, `web/wfe/patches/solid-primitives-scroll-3.0.0-next.4.patch:4`.

Problem: WAMN installs wfe packages through the parent workspace, but the parent omits the patches declared by wfe. The parent lockfile resolves unpatched dependencies, while the submodule lockfile records patched versions. These repairs affect focus listeners and ownership of scroll state. Therefore, the pinned submodule commit alone does not reproduce its dependency behavior.

Proposal: The root workspace must own the effective installation policy. Reference the patch files from the pinned submodule and retain their single source there. Qualify wfe through the same dependency resolution that WAMN uses.

Deletes: No source deletion initially. Remove the separate, inconsistent installation assumptions.

Risk: Focus dismissal and scroll restoration can differ between standalone wfe tests and WAMN applications. Run the existing wfe interaction qualification under the parent installation, followed by the relevant platform browser interactions.

## Finding H3-2: Environment JSON creates a second deployment authority

Severity: high  
Effort: M  
Area: `deploy/gcp/environments/*.json`, deployment readers  
Evidence: `deploy/gcp/environments/dkk--wms--dev.json:10`, `deploy/gcp/environments/dkk--wms--dev.k:15`, `deploy/gcp/cluster.k:10`, `crates/control/lib/src/platform/packages.rs:26`, `crates/control/lib/src/platform/build.rs:110`, `tools/test-changes:316`.

Problem: The JSON files repeat cluster, registry, route, bucket, package, and image facts owned elsewhere. They remain active inputs, rather than disposable generated output. The WMS KCL document declares REST applications, but its JSON package list contains only WMS. Platform package discovery and test preparation read that smaller JSON list.

Proposal: Resolve application packages from the authored environment document and application dependencies. Resolve infrastructure from the cluster document and image identities from the stamped chart. Remove `EnvironmentFile` after its callers use those owners.

Deletes: Both environment JSON files, the obsolete `EnvironmentFile` reader, and companion-JSON discovery in package preparation.

Risk: Immediate deletion breaks publication, delivery, and test preparation. Replace the existing package-discovery test at `crates/control/lib/src/platform/build.rs:432` with coverage of the complete WMS application set. Run the affected environment deployment case.

## Finding H3-3: Source aliases hide dependencies between workspace packages

Severity: medium  
Effort: M  
Area: `apps/platform_ui`, `apps/wamn_receiving/web`, package boundaries  
Evidence: `apps/platform_ui/src/shell.tsx:15`, `apps/platform_ui/package.json:17`, `apps/wamn_receiving/web/src/main.tsx:3`, `apps/wamn_receiving/web/package.json:17`, `apps/wamn_receiving/web/vite.config.ts:55`, `apps/wamn_receiving/web/tsconfig.json:23`.

Problem: The platform UI imports `@wamn/web-runtime` without declaring that dependency. Receiving imports `@wamn/platform-ui` through source aliases without declaring that dependency. Receiving also maps wfe packages directly into submodule source. These paths conceal the dependency graph and couple applications to repository layout.

Proposal: Give each shared package a declared public entry and require consumers to declare workspace dependencies. Consume wfe through its package exports. Keep source-level wfe qualification inside the submodule.

Deletes: Cross-package source aliases, matching TypeScript path overrides, and Receiving’s direct import of the private wfe transformation script.

Risk: Package resolution and generated component types can fail during the transition. Run the existing platform and Receiving type tests and bundle parsing tests.

## Finding H3-4: Shared Vite configuration owns too little

Severity: medium  
Effort: M  
Area: `web/shell/vite.ts`, application Vite files  
Evidence: `web/shell/vite.ts:122`, `apps/platform_ui/vite.config.ts:5`, `apps/wamn_receiving/web/vite.config.ts:17`, `web/sync-bench/vite.config.ts:30`.

Problem: Three application configurations repeat Solid setup, browser resolution, deduplication, and test environment rules. Platform and Receiving separately implement checks for the bundled Solid version. Receiving and sync-bench separately define worker entry naming. The shared helper supplies an empty deduplication list and makes applications reconstruct the common policy.

Proposal: One shared Vite owner must supply Solid compilation, resolution policy, runtime identity assertions, test defaults, and the worker build recipe. It must retain the existing development proxy, scope response, output handling, and precache support. Applications supply their entries, ports, output locations, public assets, and application-specific middleware.

Deletes: Repeated Solid-version plugins, resolution blocks, test defaults, and worker filename callbacks. Replace manually restated Vite interface shapes with Vite’s own types.

Risk: Worker URLs, offline reloads, and development routing can break. Run both existing bundle tests, Receiving’s offline browser case, and sync-bench’s worker and audience-path tests.

## Finding H3-5: The tooling package still owns application UI

Severity: medium  
Effort: M  
Area: `web/shell/src`, `apps/platform_ui`  
Evidence: `web/shell/README.md:3`, `web/shell/src/sync.tsx:15`, `web/shell/src/sync.tsx:46`, `apps/wamn_receiving/web/src/sync.tsx:34`, `web/sync-bench/src/packaging-screen.tsx:3`.

Problem: The package describes itself as shared tooling with no UI components. It nevertheless owns the shared sync status and refused-write panels. Both consumers reach into its source directory. This leaves application UI outside the platform UI owner and makes a tooling package depend on Solid.

Proposal: Move the shared sync presentation into a public platform UI entry. Keep outbox storage and application-specific resubmission behavior with their existing owners. Leave Vite and browser tooling in the tooling package.

Deletes: `web/shell/src/sync.tsx`, `web/shell/src/status.ts`, and their old test location after relocation. Remove Solid and sync runtime dependencies from the tooling package when no remaining code needs them.

Risk: Refused-write correction, expired entries, and startup status can regress. Retain the status tests and run Receiving sync tests and sync-bench packaging-write tests against the new entry.

## Finding H3-6: ESLint applies different rules to equivalent application code

Severity: medium  
Effort: S  
Area: `eslint.config.js`  
Evidence: `eslint.config.js:11`, `eslint.config.js:20`, `eslint.config.js:28`, `apps/platform_ui/src/shell.tsx:1`, `.gitignore:44`.

Problem: Platform UI receives TypeScript rules but no Solid rules. Receiving and shared web code receive both. The ignore list also omits generated client directories, despite the file comment stating that generated output is excluded. Git ignores those directories, but the ESLint configuration does not explicitly exclude them.

Proposal: Apply one common policy to authored Solid application code. Explicitly exclude materialized generated clients. Keep the submodule exclusion because wfe owns its lint policy.

Deletes: The separate platform-only rule block and stale `web/demo` exclusion.

Risk: Enabling the missing rules can expose existing violations. Inspect ESLint’s effective configuration for representative platform, Receiving, and generated files, then run lint on the affected authored sources.

## Finding H3-7: Browser drivers duplicate fixture and transport support

Severity: medium  
Effort: M  
Area: Receiving browser tests and `web/sync-bench/bench`  
Evidence: `apps/wamn_receiving/web/test/offline-browser.mjs:2`, `apps/wamn_receiving/web/test/sync-browser.mjs:2`, `web/sync-bench/bench/outbox.mjs:27`, `web/sync-bench/bench/case2.mjs:102`; measurements.md section 7, Receiving-web and sync-bench directory rows.

Problem: Receiving’s browser drivers repeat input parsing, authentication, receipt interaction, waiting, and screenshot support. WMS drivers repeat HTTP forwarding and browser setup. Those copies can acquire different timeout, header, or authentication behavior while testing the same contracts.

Proposal: Give Receiving browser support one owner beside its tests. Give the WMS transport fixture one owner beside its drivers. Keep scenario assertions and deliberate network failures in their individual cases.

Deletes: Repeated fixture parsing, sign-in functions, receipt actions, and HTTP forwarding implementations. Retain every scenario entry point.

Risk: Shared support can accidentally remove differences that a scenario requires. Run `local_receiving_screens`, `local_receiving_sync`, and `local_receiving_offline`, plus the affected WMS outbox scenarios.

## Finding H3-8: Bundle parsing tests are copies with different parameters

Severity: low  
Effort: S  
Area: Platform and Receiving bundle tests  
Evidence: `apps/platform_ui/test/bundle.test.ts:13`, `apps/wamn_receiving/web/test/bundle.test.ts:13`; measurements.md section 7, corresponding raw clone rows.

Problem: Both tests build an application, copy JavaScript chunks to module filenames, invoke Node parsing, and clean up a temporary directory. Their implementations differ only in application context and the temporary directory prefix. A correction to chunk selection or diagnostics requires two edits.

Proposal: One shared web-tooling test must run the same bundle assertion for both application configurations.

Deletes: The duplicated build, parse, diagnostic, and cleanup bodies.

Risk: Consolidation can omit one application or a worker entry. Preserve separate named application cases and include Receiving’s worker in the emitted-module assertion.

## Finding H3-9: Three exports and one root dependency are removable

Severity: low  
Effort: S  
Area: Web exports and package manifests  
Evidence: `apps/platform_ui/src/projects.tsx:17`, `apps/platform_ui/src/projects.tsx:82`, `apps/platform_ui/src/projects.tsx:411`, `apps/platform_ui/src/projects.tsx:454`, `web/shell/src/sync.tsx:12`, `package.json:13`; measurements.md section 8, export and dependency rows.

Problem: `Projects` and `CreateProject` serve routes inside their own module but have no external consumers. The `shellStatus` re-export has no caller, although its original function remains used. The root `@wfe/generator` dependency has no root-owned import. These are narrower deletions than the raw unused totals suggest.

Proposal: Remove the two export modifiers, the unused re-export, and the root dependency. Retain sync-bench’s generator dependency because generated component descriptions import it (`crates/schema/generator/src/client_component.rs:197`). Retain Playwright because browser drivers resolve it from the tooling package (`apps/platform_ui/test/browser.mjs:8`). The three `@types/d3-shape` declarations support explicit TypeScript paths, so they are not immediately removable (`apps/platform_ui/tsconfig.json:18`, `apps/wamn_receiving/web/tsconfig.json:18`, `web/sync-bench/tsconfig.json:17`). Move that type requirement to the wfe package that imports it before deleting consumer workarounds (`web/wfe/packages/components/src/vendor/charts/charts-core/src/stack-internal.ts:9`).

Deletes: Three unnecessary exports and one dependency declaration initially. Later, delete consumer chart-type dependencies and path mappings after their owner supplies the types.

Risk: Generated descriptions and compiler resolution can depend on imports absent from the authored source scan. Run the existing generated-description test and affected package type tests.

## Finding H3-10: Obsolete Beads files remain tracked beside the canonical export

Severity: low  
Effort: S  
Area: `.beads`  
Evidence: `.beads/beads.jsonl:1`, `.beads/issues.jsonl:1`, `.beads/config.yaml:68`, `.beads/metadata.json:2`, `CLAUDE.md:27`.

Problem: `.beads/beads.jsonl` contains 2,163 issue records whose newest update dates to August 24. Every issue ID also appears in `.beads/issues.jsonl`. The repository additionally tracks the empty temporary file `.beads/.~issues.jsonl.3646117766`. There is no root-level `beads.jsonl` in this checkout.

Proposal: Keep Dolt as the live tracker and `.beads/issues.jsonl` as the designated passive export. Delete the older export and temporary file. Retain the tracker configuration, metadata, and hook sources.

Deletes: `.beads/beads.jsonl` and `.beads/.~issues.jsonl.3646117766`, about 7.6 MB combined.

Risk: An external reader can still depend on the old filename. Search repository consumers and preserve the canonical export’s issue IDs and memory records. No application test is required.

## Open questions for the owner

1. Can WAMN consume only public built wfe exports, with source qualification confined to the submodule?
2. Does any external tool still read `.beads/beads.jsonl` or the deployment JSON files?

## Proposed fix epics

### One web dependency boundary

1. Preserve wfe repairs in the root installation
2. Declare workspace dependencies and consume public package entries
3. Consolidate Vite policy and lint scope

### One owner for shared web behavior

1. Move sync presentation into the platform UI
2. Consolidate Receiving and WMS browser support
3. Consolidate application bundle parsing tests

### One deployment declaration path

1. Resolve packages from authored environments and dependencies
2. Replace legacy environment JSON readers
3. Delete redundant environment JSON files

### Remove unused web and tracker surface

1. Remove unnecessary exports and the root generator dependency
2. Move chart type requirements to their wfe owner
3. Delete the obsolete Beads export and temporary file
