# 86 test trees

Scope: `tests/` (conformance, integration, system, orchestrator, `selection.toml`, `bench/`) and `test-support/` (infrastructure, harness, postgres, fixture-package) at `epic/dep7` (b94833fd). Sizes: conformance 7,715 lines in 30 files; integration 15,522 in 31; system 1,547; orchestrator 179; test-support 9,271 in 30. Nothing built or run. Application test trees (`apps/*/tests`) and tests inside crates are out of scope.

## Earlier work checked first

- The earlier test area review (at 553511bbd) states what test code must show; about 37% of the area is accidental. Its open question "is the cluster Job mechanism kept after the journey ruling?" has no answer.
- Earlier proposal on test selection, not ruled (one rules file, one `requires:` parser, delete `deploy/gates/*`). At dep7 the `kubernetes_gate*` files are gone; `deploy/gates` and `tests/orchestrator` remain.
- CLAUDE.md test rules 1–5; journey ruling 2026-11-04; "test code that does again what the product does is deleted".
- Tab 81 1 A: test selection and lint become xtask commands; tooling tests go (`test_changes.rs`, `contract_diff.rs`, `repo_lint.rs`, `wasmcloud_release_check.rs`, `packages_alone.rs`, `cranelift_dev.rs`, `delivery_native_build.rs`, script parts of `registry_image_archive.rs`).
- Tab 70 1 A and tab 72 1 A: host local modes go after the `kind` loop measurement.
- Tabs 62 (1 A, 2 A), 65 (1 A), 68: Helm charts per environment and org; credential Secrets are explicit apply steps; the install crate is `wamn-platform-install`.
- Tab 57 finding 8: no user PATs. Tab 84: authoring model and management roles deleted. Tab 79: materializer deleted. Boundary ruling 2026-11-10: virtualizer deleted. Tab 85: `wamn-client` deleted.
- U2 (tab 34) and tab 78: SQLx for all host and control SQL; TLS on every PostgreSQL connection. Standing: hyper, not reqwest.

## Files read

- The head of every file in `tests/conformance`, `tests/integration/src`, `tests/system`, `tests/orchestrator`, `test-support/*`.
- In part: `tests/selection.toml` (structure, rows); `test-support/postgres/lib.rs` (public API, `require_ignored_prerequisites`); `test-support/infrastructure/{lib,rendering,secrets,locked_database,ctl_process,event_broker}.rs` (public API); `test-support/harness/src/render_check.rs` (`host_values`, `environment`, `credential_secrets`); `tests/system/tests/deployment_declarations.rs` (helpers, tests, constants); `tests/conformance/tests/chart_seam_governance.rs` and `registry_image_archive.rs` (test lists); `Dockerfile` (`gates` target); `deploy/gates/*.yaml` (headers); `tools/bench/Cargo.toml`, `tools/bench/local.sh` (paths).

## Callers read

- Repository grep, outside the defining file, for: every public module and item of `wamn-test-infrastructure` named below; every module of `wamn-gate-harness`; `wamn_fixture_package`; `wamn_test_postgres::database`; `local_application`; each bench binary; `wamn-gates`; `deploy/gates`; `tests/bench`; `#[sqlx::test]`; `ignore = "requires:`.
- The `[dependencies]` of every crate that names a test-support crate.

## Findings

**1. Test tiers are a hand list of Cargo targets.**

Fact:

- `tests/selection.toml` (943 lines) lists 265 Cargo targets by hand, each with a tier. Beside them: 18 `[[suites]]`, 57 `[[preparation]]`, 4 `[[rules]]`, 45 `[operator_inputs]`.
- `tests/conformance/tests/test_tiers.rs` (546) checks that every target has one row.
- 147 tests already state their needs in `#[ignore = "requires: ..."]` (env names and programs). Three code paths parse that text: `tools/test-changes`, `test_tiers.rs`, `wamn_test_postgres::parse_prerequisites`.
- Rows still name crates ruled deleted (`wamn-client`, `wamn-authoring-model`, `wamn-component-virtualizer`); `root_features` names `wamn-ctl/ops`.
- CLAUDE.md rule 2: never hand-pin a roster the artifact can enumerate.

Options:

- A: A test states its tier once, in its `requires:` reason, which also names services (`postgres`, `nats`, `kind`). No reason means the default tier. The xtask selection command (tab 81) lists targets with `cargo metadata` and takes the changed packages and their dependents from the diff. `selection.toml` keeps `[[rules]]`, `[[suites]]` and `[[preparation]]`. The target rows, `[operator_inputs]` and `test_tiers.rs` go. One parser, in `wamn-test-postgres`.
- B: cargo-nextest profiles, one filter expression per tier in `.config/nextest.toml`. Target rows and `test_tiers.rs` go. The tier is then stated a second time, in filter text.

Recommend A.

**2. The `wamn-gates` image has no runner.**

Fact:

- `tests/orchestrator` (`wamn-gates`, 179 lines) routes 11 commands to code in other test crates. The `Dockerfile` builds it in target `gates`. `deploy/gates` holds three Job files; their headers say to apply them by hand with `kubectl`.
- No code, script or `selection.toml` suite starts a `wamn-gates` command (grep).
- What it routes to: five deployed checks in `tests/integration/src` (`identity_session_test`, `host_session_test`, `membership_test`, `dashboard_test`, `identity_keys_test`; 2,484 lines), `tests/conformance/src/socket_test.rs` (216), `tests/system/src/trace_test.rs` (503), and four benchmarks (finding 3).
- Three of the five checks use a user PAT (`identity_session_test`, `host_session_test`, `membership_test`). Tab 57 finding 8 removed user PATs.
- Trace propagation has an in-process test in `tests/integration/src/hot_route_trace.rs`.
- CLAUDE.md says deployed tests use `--target gates`.

Options:

- A: Delete `wamn-gates`, the `gates` Dockerfile target, `deploy/gates`, the five checks, `socket_test.rs`, `trace_test.rs` and the CLAUDE.md line. A contract that needs a cluster gets a `kind` journey test through `wamn` when one is written.
- B: Delete the router, the image target and the Job files. Move the five checks, `socket_test` and `trace_test` to `kind` tests driven by `wamn`.

Recommend A. Nothing runs them, three rest on removed PATs, and B rewrites every setup step anyway (journey rule).

**3. Benchmarks live in four places.**

Fact:

- `tools/bench`: crate `wamn-bench` (reqwest, tokio-postgres) and scripts. `local.sh` and `tier.sh` write `tests/bench/*.json` (6 files). This is the path in use.
- `tests/integration/src`: `cdcbench` 1,364, `walbench` 990, `streambench` 727, `readerbench` 306. Reached only through `wamn-gates`. `streambench` and `readerbench` measure the materializer (deleted, tab 79).
- No caller: `throughput_bench.rs` 889 with bin `wamn-throughput`; bin `instantiate-bench`; conformance bin `wamn-benchmark-gate` (`src/benchmark.rs`, 207); `measure.rs`; `measurement_schema.rs`.
- `wamn-gate-harness` `check`, `emit_csv`, `percentile` serve only the four campaigns.

Options:

- A: `tools/bench` is the one benchmark path (an xtask command, tab 81). Delete the rest listed above. The tab 34 Val-path benchmark goes into `tools/bench` when it is written.
- B: Move the four campaigns into `tools/bench`; delete only the code with no caller.

Recommend A.

**4. Test support holds code with no user.**

Fact (grep for each public item outside its own file):

- `telemetry.rs` 902: no user.
- `workload.rs` 939: no user of its functions; `release_chart.rs` uses its `HostObservation` type.
- `release_chart.rs` 615: one function (`helm_releases`) used by one file (`crates/control/dev/tests/support/platform_upgrade.rs`).
- `declarations.rs` 307: used only by example `gate_request` (authoring request, tab 84).
- `secrets.rs` 514: `derive_host_secrets` is used only by its own tests.
- `rendering.rs`: `render_host_values`, `render_http_workload`, `render_kind_cluster`, `assert_rendered_identity` and their input types (about 610 lines) are used only by `rendering/tests.rs` (831) and the `secrets.rs` tests.
- `fixtures/traces` and `fixtures/workload`: read only by `telemetry.rs` and `workload.rs`.
- Example `pat_for_disposable_stack`: named only in `docs/history`; it issues a management PAT (tabs 57, 84).

Options:

- A: Delete all of it with its tests, about 4,800 lines. `helm_releases` moves into its one user.

Recommend A (CLAUDE.md rule 3 covers the tests).

**5. A test assembler builds the host itself.**

Fact:

- `tests/integration/src/local_application` (1,268 lines: `local_application.rs` 522, `assembly.rs` 635, `auth_nats.rs` 111) builds the plugins (`WamnPostgres`, `WamnJetstream`, route authentication, HTTP transport) and serves HTTP with its own hyper loop.
- Five application test files use it: `apps/wamn_wms/tests/{local_outbox,local_feed,local_business}.rs`, `apps/wamn_receiving/tests/route_authentication_live/{queue_recovery,local_business}.rs`.
- The product's local entry is `wamn_runtime::local_application`; tab 72 1 A removes it after the `kind` measurement.

Options:

- A: Delete the assembler. These tests start the product host the way the measurement decides: the host binary's local mode if it stays, else `kind`.

Recommend A (CLAUDE.md: test code that does again what the product does is deleted).

**6. Chart checks sit outside the chart owner and parse YAML by hand.**

Fact:

- `tests/system/tests/deployment_declarations.rs` (1,008) reads chart files with its own line-based YAML reader (`documents`, `object`, `scalar_after`, `indent`).
- It keeps `RETIRED_FILES` and `RETIRED_IMAGE_MARKERS` (deleted files and images must stay absent) and `EXTERNAL_PREREQUISITES` (10 Secret names by hand).
- `tests/conformance/tests/chart_seam_governance.rs` (433) and `crates/control/environment/tests/environment_render.rs` render charts through `wamn-gate-harness` `render_check` (553). It runs `helm template` on the product chart, but builds the credential Secrets and roles itself (`credential_secrets`, `fixture_roles`).
- `tests/conformance/tests/otel_collector.rs` (94) validates the collector configuration with `docker`.

Options:

- A: Each chart contract that matters (issuer credential only to identity, Secret permissions, namespace scope, host controls) is one `helm template` test, parsed with serde\_yaml, in the chart owner's crate (`wamn-platform-install`, tab 68). The Secret set comes from the apply steps that create Secrets (tab 62 2 A), not from a list. The hand YAML reader, the retired lists and `render_check` go. `tests/system` goes (`trace_test` with finding 2).
- B: The same contracts as helm-unittest files (a Helm plugin) beside each chart.

Recommend A: no new tool, and the tests stay in cargo selection.

**7. The conformance crate mixes runtime contracts and repository lints.**

Fact, what stays after tab 81:

- Runtime contracts: `capability_registry.rs` 225 (uses `wamn_component_policy`), `component_export_index.rs` 53 (`wamn_engine`), `src/invocation.rs` 160, `src/ip_name_lookup.rs` 247, `src/runtime_policy.rs` 19, `src/schema_drift.rs` 145 (`wamn_run_state`).
- Dependency checks: `src/manifest_dependencies.rs` 457; `wasmtime_source_identity.rs` 318 (one Wasmtime family and one async-nats in the lock file).
- Other lints: `guest_workspace_closure.rs` 135; `src/repo_policy/*` (3 lints) with bin `repo-policy`; `palette_declarations.rs` 98.
- `package_build.rs` 130 checks virtualized guests (virtualizer deleted). `test_tiers.rs` and `chart_seam_governance.rs`: findings 1 and 6.

Options:

- A: The crate goes. Runtime contracts move to their owner crates' tests (owners inferred from imports). The dependency checks become cargo-deny `bans` rules (an existing tool, pinned in the tab 81 tool version file). The other lints become the xtask lint command (tab 81). `palette_declarations` moves to the palette build (tab 80 2 B). `package_build.rs` goes.
- B: As A, but the dependency checks stay Rust code in the xtask lint command.

Recommend A: existing tooling over hand-written lock file checks.

**8. Per-test databases are hand-made; U2 brings an existing tool.**

Fact:

- `wamn_test_postgres::database()` is called at 186 sites; 196 files use the crate. It makes a private database on a per-process server. `Database::execute` runs SQL by starting `psql`.
- `locked_database.rs` holds a process lock because roles are server-wide.
- No test uses `#[sqlx::test]`.

Options:

- A: After U2, a test that needs a database uses `#[sqlx::test]`: SQLx makes and drops one database per test and applies the migrations. `wamn-test-postgres` keeps starting the server (with TLS, tab 78), and its runner binary sets `DATABASE_URL`. `Database`, `execute` and its `psql` call go. Tests that create roles keep one lock (inference: `#[sqlx::test]` does not separate roles).
- B: Keep `wamn-test-postgres`; switch only its client from `psql` to SQLx.

Recommend A.

**9. `wamn-gate-harness` has 14 workspace dependencies for a few helpers.**

Fact:

- Modules and users: `render_check` (4 files, finding 6); `case_release` (353), used only by `registry_image_archive.rs` (588), which tests it (rule 3); bench helpers in `lib.rs` (finding 3); `journey` (4 files); `run_check` (1); `workflow` (1, `queue_recovery.rs`).
- `workflow.rs` authors and enables workflows "with the same authorship and enable boundaries as the control verbs": it does again what the control route does.
- Its 14 workspace dependencies include five control crates.

Options:

- A: The crate goes. `journey` and `run_check` move into `wamn-test-infrastructure`. `workflow.rs` goes; its user calls the control route (tab 71 2 A). `case_release` and `registry_image_archive.rs` go.

Recommend A.

## Follows from earlier rulings

- Tab 79: the materializer credentials in `event_broker.rs`; `workload.rs::materializer_idle` (gone with finding 4).
- Boundary ruling 2026-11-10: `tests/integration/src/virtualized_std_guest.rs` (730), `test-support/infrastructure/std-virtualization.compose.yaml`, the virtualized artifact paths in `guest_workspace_closure.rs`, the `wamn-component-virtualizer` row.
- Tab 71: `ctl_process.rs` runs `wamn-ctl` and `wamn-ctl-ops`; `wamn-ctl-ops` goes and user verbs move to `wamn`.
- Tabs 84, 85: `selection.toml` rows for `wamn-client`, `wamn-authoring-model`, `wamn-scenario-worker`; example `gate_request`.
- Tab 81 1 A: the tooling tests listed there.
- Tab 82: `render_check::credential_secrets` takes `Triple` (org, project, env order).
- Tab 72 3 A: bead ids in comments (for example `throughput_bench.rs`, `cdcbench.rs`).
- Standing direction: `wamn-bench` moves from reqwest and tokio-postgres to hyper and SQLx.

## Fine as is

- `wamn-fixture-package`: 19 user files.
- `scratch`, `fixture_digest`, `record_history`, `credentials`, `nats_tls`, `operations`: many users.
- `tests/integration/src/bin/http-fixture` (773): an owned loopback server for connector tests.
- `test-util` features in `wamn-catalog`, `wamn-project-state`, `wamn-package-control`, `wamn-control-provision`: shared test helpers behind an optional feature. Correction (2026-11-12): not off in every product build. Fact: tests/integration/Cargo.toml turns on test-util (and ops) for wamn-runtime and wamn-control-provision as normal dependencies, and the Dockerfile builds wamn-gates with wamn-host in one cargo call. Inference: with resolver 2 the shipped host binary gets the test code; cargo tree -e features would confirm. Deleting wamn-gates removes this path. Listed in Summary (defects).
- Live tests in `tests/integration/src` (`route_authentication_live`, `route_interface_live`, `write_log_live`, `trusted_http_route`, `control_client_live`, `cdc_reader_process`, `hot_route_trace`): bodies not read; no finding.

## Tests deleted, changed or moved

- 1 A: `tests/conformance/tests/test_tiers.rs`.
- 2 A: the five deployed checks, `socket_test.rs`, `trace_test.rs`.
- 3 A: the campaign and bench files listed in finding 3.
- 4 A: `rendering/tests.rs` and the test modules of `telemetry.rs`, `workload.rs`, `secrets.rs`, `declarations.rs`.
- 5 A: five application test files change how they start the host.
- 6 A: `deployment_declarations.rs`, `chart_seam_governance.rs`, `otel_collector.rs` are replaced by chart tests in `wamn-platform-install`; `environment_render.rs` changes.
- 7 A: runtime contract tests move to owner crates; lint tests become lint commands; `package_build.rs` deleted.
- 8 A: about 186 call sites change to `#[sqlx::test]`.
- 9 A: `registry_image_archive.rs`; the crate's own tests.

## Not read

- Bodies of the live tests in `tests/integration/src`; `apps/*/tests`; tests inside crates.
- Whether a lower-tier test covers socket egress refusal (`socket_test.rs`).
- The 57 `[[preparation]]` entries one by one.

## Rulings

2026-11-12: findings 1–9 A (tiers stated on the test; `wamn-gates` image, `deploy/gates` and deployed checks deleted; `tools/bench` the one benchmark path; test support with no user deleted; `local_application` deleted; chart tests in `wamn-platform-install`; conformance crate split to owners, cargo-deny and xtask lint; `#[sqlx::test]` after U2; `wamn-gate-harness` deleted).
