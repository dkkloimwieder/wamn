# wamn test review

Oct 27, 2026 · @D Kloimwieder · main @ 18359d2

Status: closed to history on 2026-10-29. All six epics are closed: `wamn-d6ti6`, `wamn-2myrx`, `wamn-da7kv`, `wamn-l6oq4`, `wamn-6xwfx` and `wamn-kfid0`, with fix `wamn-xf60a`.
The text below records the review as written. Beads hold the work status.

## Verdict

About 24,000 lines of test code can go. Most of it is migration-cutover tests made dead by the greenfield ruling, plus tests that grep source, SQL or YAML text. Coverage gaps sit in auth refusal paths, edge auth, guest-side SQL helpers and the generator's own SQL.

- Scope: about 3,300 Rust tests and 41 TS test files on main @ 18359d2. Eight Sonnet reviewers covered the repo by area.
- Line counts are reviewer estimates. Rows marked *checked* were verified against source by me. The rest carry the reviewer's quoted evidence (scratchpad files A–H).

| Area | Est. removable test LOC | Main kind of bloat |
| --- | --- | --- |
| control/lib live tests + provision + registry | 9,000 + 1,630 SQL fixtures | one-off migration tests, run-plane cutover legs |
| apps + web | 3,400 | generated-code re-tests, publication goldens |
| tests/conformance + integration + system | 3,400 | repo-text grep, exact argv, captured old output |
| schema (generator, control, introspection) | 2,300 | run-plane cutover unit tests, generated-text `contains` |
| services + tools + test-support | 2,300 | `--help` greps, dead harness code |
| execution + identity + catalog | 900 | run-state SQL-text pins |
| platform/runtime | 650 | SQL-text pins, 13-key span sets in 10 tests |
| control/lib src | 650 | literal argv/name pins |

## Bug and owner rulings

One real bug was found. A test hides it.

**Bug (checked):** `deploy/mvp/bootstrap.sh` expects the operator PAT Secret role to be `operator` (`inspect_secret operator … operator`, lines 286 and 301; compared at line 158). The Rust writer `pat_secrets.rs::OPERATOR` writes `role: ADMIN_ROLE` = `"admin"`. The shell test mock writes `operator`, so the test passes. Likely effect (inferred, not run): bootstrap never accepts a Rust-issued operator Secret. Fix: `operator` → `admin` in the script and the mock, plus one contract test that both sides read the same value.

Owner rulings (2026-10-27):

1. **`durable` tier: deferred, delete.** It is a per-run `DurabilityClass` (`run-state/src/durability.rs`) that turns the shelved crash floor back on: claim-time effect classification, `ExpiredWithAttempt`, `Terminalized`, `EffectAttempt`. The default `standard` tier never reaches it; the code is unreachable, not deleted. Delete the code and its tests together: `DurabilityClass::Durable`, the claim-time classify branch, the registry type value, the `durable` CHECK literal, `runtime/tests/production_claim_durable_live.rs`, `run-state/tests/shelving_contract.rs`, and the durable-floor tests in `queue.rs`. Deleting the tests alone would leave dead code untested.
2. **Tenant: remove the tests.** The tenant-floor removal from code is still in progress. Until it lands, `tenant_id` / `app.tenant` remains in run-state SQL, scenario-worker, the materializer, `package_migrations::record_migration_statement` and catalog seeds, with no test pinning it.
3. **Migrations: squash** to one baseline during development. Revive numbered migrations once development is stable. This retires the `*_migration_live.rs` files, `schema_migrations.rs` text tests and the `upgrade_schema` baseline-branch tests.
4. **Agent-pilot experiment: closed.** Delete `tests/integration/src/agent_pilot/`, its fixtures and `tools/agent-pilot-*` (about 1,000 LOC).
5. **Write-log replay across principals: test it.** Whose replay is allowed likely depends on configuration (system, invoker, or a named role). Investigate the replay mechanism first. The test then pins whichever rule is chosen. Added as a gap below.
6. **`begin_item`: rename to `admit_and_open_transaction`.** It is `InvocationPolicy::begin_item` (`engine/operation/invocation_policy.rs`), implemented in host `native_policy.rs` and edge `policy.rs`. Per request element of a write call, before the claim and the guest call, it admits the call (as `activate` does) and opens the host transaction; `None` when the operation declares no SQL. "Item" names physical products only: one element of a batch call's input array is a **request element**.
   - Renames: `item_input_hash` → `request_element_hash`, `request_item` → `request_element`, `item_call` → `request_element_call`, `settle_item` → `settle_request_element`, `bind_item_session_claims` → `bind_request_element_session_claims`, `ItemTransaction` → `Transaction`, `NoItemTransaction` → `NoTransaction`, `item_facts` → `transaction_facts`, `ItemCommitFailure` → `CommitFailure`; plus doc comments, error text ("item envelope", "item list") and test names.
   - Not renamed: wash-runtime's `WorkloadItem`, `ComponentItem`, `on_workload_item_bind` (upstream; zero-patch fork).

## Delete: whole files

These files test nothing that still exists, or only re-test generated output. Delete them with no replacement unless the row says otherwise.

| File | LOC | Why | Status |
| --- | --- | --- | --- |
| `control/lib/tests/*_migration_live.rs` (7 files: user\_type, environment\_table, environment\_status, environment\_apply, authored\_roles, administration\_grants, org\_administration) | 1,289 | replay one-off migrations from `app_system`, plural tables, tenant floor, `operator` role | checked |
| `control/lib/tests/fixtures/*-before-*.sql` (5 files) | 1,631 | used only by the files above | checked |
| `apps/wamn_receiving/tests/receiving_data_access.rs` | 1,992 | `include_str!` of generated `update.sql`/`get.sql`; boots its own Postgres every default run. **Gate:** first add the generator SQL test (gap P1-5) | checked |
| `apps/{wamn_receiving,wamn_wms,client_acme_receiving}` publication tests + `wms_wiring_shape.rs` | \~500 | generator output compared to itself or hand copies | reviewer |
| `test-support/simulator/` (crate) | 599 | no dependents; router uses plain `proptest` | checked |
| `test-support/harness/src/ceiling.rs` | 404 | archived load driver, no callers | checked |
| `services/ctl/tests/verb_surface.rs` | 227 | greps `--help` text and `cargo tree` | reviewer |
| `tests/conformance/tests/cranelift_dev.rs` | 352 | polices Dockerfile and helper-script text | checked (header) |
| `tests/conformance/tests/{instance_suffix_ownership,replica_identity_authority}.rs` | 156 | source-text and retired-YAML grep | reviewer |
| `tests/conformance/src/catalog.rs` | 65 | duplicates `catalog/model/tests/identity.rs` | reviewer |
| `tests/integration/fixtures/operator-recovery/` + `operator_recovery::retained_crd_hashes_*` | — | captured 2.9.0 / 2.10.3 output; runtime is 2.11.0 | reviewer |
| `apps/platform/ingress/http-route/tests/adversarial.rs::fresh_only_refusal_*` | 1 test | `fresh_only` no longer exists in source | checked |
| `web_scope.rs`, `generator/src/describe.rs`, `schema/control/src/sql.rs` test modules | small | each compares a literal with its own copy | A, E |

Also move out of `tests/`: `tests/bench/` result archives (`gcp-pg-*`, `a5-paired/`, flamegraph) and `tests/sweeps/*.log`. They are evidence data, not tests.

## Collapse, convert, move

These files keep a small core. The rest is cutover legs, text pins or repeated setup.

**Run-plane cutovers (largest item, checked):**

- `schema/control/src/run_plane/tests.rs`: 3,180 lines, 207 "cutover" hits. Delete the cutover tests together with the cutover code. Target is about 800 lines.
- `control/lib/tests/run_plane_live.rs` + `run_plane_live/*`: about 5,700 lines. Keep from-zero, current-no-op, two-plane residency and platform configuration. Every other leg has a planner unit twin.

**Text pins → behavior:**

- run-state `transitions.rs`: 42 `contains(` on SQL text (checked). Keep the 3 behavior tests, drop 7. `sql.rs`, `queue/*.rs`, `operator_action.rs` follow the same pattern; the live run-state tests already cover them.
- runtime `claims/tests.rs` (3,981 lines): drop the SQL-substring pins. Keep the live claim tests.
- runtime span-attribute tests: 10 tests repeat one 13-key set. Use one shared helper.
- generator `client_ts_emitter.rs`: 67 substring asserts in 21 tests. Replace with about 6 behavior tests plus one `tsc` compile.
- `prune_run_history::the_prune_statement_binds_the_tenant_*`: greps SQL (checked). Delete.
- `platform/broker.rs:370`: assertion `!subject.starts_with("tap.")` (checked). Taps are gone. Drop that line.
- `reconcile_run_plane::pre_role_bootstrap_allowlist_is_exact`: const compared to its own literal (checked). Delete.

**Live → fewer live:**

- 6 `#[ignore]` NATS `auth_drops_*` tests prove one rule. Keep 1 NATS test. Assert the returned `AuthDrops` in plain Postgres for the rest.
- `event_advisories.rs`: two giant tests that print `*_PASS` lines and assert exact counts. Split them; move the stream-config and permission-scope checks to units.
- `workflow_schedule_live`: real-minute sleeps (about 5 minutes for 2 tests). Inject the clock.
- `deploy_sql_authority`: 17 tests, each boots its own Postgres. Share one server.
- The three generation-lifecycle live tests (guest, management admitter, provisioning order) repeat one prepare/rotate/retire run. Keep one.
- `tests/conformance/tests/chart_seam_governance.rs` (779 lines): reduce to one rendered-chart test.
- `kind_gate_image_remove.rs` and `journey_image_cache.rs` (about 1,250 lines): keep only the destructive-action refusals.

**Move, don't delete:**

- `dependency_boundary.rs` in edge, engine and workflow spawn `cargo tree`. They enforce the edge/engine dependency rule. Move them to `tools/repo-lint` so they run per merge, not per issue. Reviewers D and F said delete; I disagree.
- `services/host/tests/scheduler_nats_move_live.rs` (645 lines): already `#[ignore]` and the only NATS-reconnect test. Keep it; drop its debug-line assertions. Reviewer F said delete; I disagree.

## Coverage gaps

Each gap below was checked by a repo-wide grep: no test anywhere references the function, or only its success path is tested. Most are fast unit tests.

| P | Source | Missing case | Test type |
| --- | --- | --- | --- |
| P1 | `services/edge/src/authenticator.rs::authenticate` + `policy.rs::EdgePolicy::authorize` | expired session, wrong `aud`/`iss`, CSRF mismatch, missing grant, expired deadline. `tests/route.rs` has 2 tests, both success paths | unit |
| P1 | `runtime/route_authentication.rs::queued_run_caller` | service with no role, user with no role, absent or inactive principal. No test calls it | live (one PG) |
| P1 | `runtime/route_authentication.rs::PlatformRouteAuthenticator` PAT and `org:` signature branches | Platform-type PAT → 401; Service PAT with wrong subject → 401; `grants.user` via webhook → 401 | live |
| P1 | `apps/platform/execution/blob-put::run` | empty or non-string key → `missing_key`, nothing written. Crate has 0 tests | unit |
| P1 | generator-owned SQL test (replaces `receiving_data_access.rs`) | run generated get/update/delete on the fixture package: stale revision, not found, equal value keeps `row_version` | live, in generator |
| P1 | `apps/platform/data/postgres-statements::run_transaction`, `decode_one`/`decode_all`, `StatementError::from_wire` | Err → no commit; 0 or 2 rows → refusal; each `PgError` → right type. Every app's `classify` depends on this | unit |
| P1 | `apps/platform/execution/materializer::prepare_message` / `serve` | refused → terminate, transient → nack with delay, skip → ack | unit |
| P1 | `control/lib/apply_package/migration_policy.rs::validate_manifest_definition_owners`, `validate_relation_classifications` | owner naming a non-base package → `definition-owner-undeclared` | unit |
| P1 | `execution/host/src/route.rs::authorized_component` | caller `None`; operation not in component; wrong package | unit |
| P1 | write log: refused batch item | one refused item leaves its siblings committed (per-item rule, 2026-10-20) | live |
| P2 | `write_log.rs::finish_opaque` | replay after crash between claim and finish → uncertain, not empty success | live |
| P2 | `runtime/event_batch.rs::require_event_materializer` | wrong workload authority → `Identity` | unit |
| P2 | `connection_http.rs::outbound_headers` | guest `host`, `content-length`, `transfer-encoding`, `proxy-authorization` → `AuthorityDenied` | unit |
| P2 | `engine/statement_instances.rs` | documented refusals (import without record, member gets another's import) | unit |
| P2 | `control/lib/user_role.rs::resolve_user` / `change_user_role` | 0 rows; two emails differing by case | live |
| P2 | `services/identity/src/password.rs::handle` | unknown email and wrong password give the same response | live |
| P2 | `scenario-worker/management.rs::admitted_role` | `project-author` refused a `project-admin` command | unit |
| P2 | `schema/control/package_migrations.rs::transaction_statements` | statement order: apply 0001, record 0001, apply 0002, … | unit |
| P2 | `schema/control/exposure.rs::normalize_http_route` | `/a/{*rest}/b`, `/a//b`, `/a/{id}x` refused | unit |
| P2 | `web/runtime/src/supplied.ts::occurredAt`, `writeSupplied`; `sync/send.ts` | microsecond spelling; fresh `requestId` per call; same key + different hash refused | unit (vitest) |
| P2 | generator emitted Rust and TS | only 3 codecs ever compiled; TS never type-checked | one compile test each |
| P3 | `pat_secrets.rs::parse_pat_prefix`, `git_source.rs::read_status`, `registry/validate.rs::compatible`, no-std `transform` and `label-render` glue | parser refusals | unit |

Added by ruling 5: **P2, `runtime/plugins/wamn_postgres/write_log.rs::begin_opaque`.** A second principal replays the first principal's key. Expected result is set by the replay rule, once chosen (system, invoker or named role). Investigate the replay mechanism before writing it.

## Implementation plan

Six epics. Epics 1 and 3 can start now and run in parallel; the rest are ordered by shared files, migrations and the build-cost rule. Testing budget per issue: build, clippy, unit tests; per merge: only the live targets that touch the merged files.

1. **Delete dead tests** (test files only, no product code).
   - Delete:
     - the 7 `*_migration_live.rs` files and the 5 SQL fixtures
     - agent-pilot (tests, fixtures, `tools/agent-pilot-*`)
     - `test-support/simulator` and `harness/ceiling.rs`
     - the app publication tests and `wms_wiring_shape.rs`
     - the conformance text-grep files
     - the operator-recovery captures
     - the `fresh_only` test
     - the literal unit tests listed by reviewers A, C, D, E and F
     - tenant-pinning tests
   - Move `tests/bench` results and `tests/sweeps` logs out of `tests/`.
   - Check: build, clippy, `tools/test-changes`. No live runs.
   - Image rebuild: none.
2. **Delete dead code together with its tests** (serial, one agent; touches migrations).
   - Durable tier: the code, the CHECK literal, the registry value, and its 3 test files.
   - Run-plane cutovers: delete the cutover code and shrink `run_plane/tests.rs` (to about 800 lines) and `run_plane_live` (to 4 legs).
   - Squash migrations to one baseline. Then delete the `schema_migrations.rs` text tests and the `upgrade_schema` baseline tests.
   - Check: per merge, the touched live targets; one `environment_apply_kind` run at epic close.
   - Image rebuild: changes host and ctl code, so it rides the next forced build and never triggers one itself.
3. **Bootstrap bug** (small, independent).
   - Change `operator` to `admin` in `bootstrap.sh` and its mock.
   - Add one Rust↔shell contract test for the role value.
   - Image rebuild: none (script only).
4. **Collapse and convert** (after epic 2; shares run-state and control files).
   - Replace SQL-text pins with behavior tests, or delete them: run-state, `claims/tests.rs`, the span-attribute helper, and `client_ts_emitter` (plus one `tsc` compile).
   - NATS `auth_drops`: keep one NATS test; move the rest to plain Postgres.
   - Split `event_advisories`.
   - Add an injected clock to `workflow_schedule_live`.
   - Give the `deploy_sql_authority` tests one shared Postgres.
   - Merge the 3 generation-lifecycle live tests into one.
   - Reduce `chart_seam` and the image-cache conformance tests.
   - Move `dependency_boundary.rs` into `tools/repo-lint`.
5. **Gap tests.**
   - First, add the generator-owned SQL live test. Then delete `receiving_data_access.rs`.
   - P1 rows: edge authenticator and policy, route-authenticator PAT and `org:` branches, `queued_run_caller`, `blob-put`, `postgres-statements`, materializer dispositions, `migration_policy` refusals, `authorized_component`, batch sibling commit.
   - Then the P2 rows.
   - Write-log replay across principals waits for the replay-rule investigation.
6. **Rename `item` → request element** (ruling 6).
   - Rename to `admit_and_open_transaction`, plus the rename list.
   - Same agent as epic 5, after it, since both touch engine, host and edge files.
   - Image rebuild: none forced; rides the next build.

Before assigning: epics 2, 5 and 6 touch `engine/operation`, `execution/host` and `services/edge`, which may overlap the A5 work. The owner names which agents are free and who holds those files now.

## Reviewer claims rejected

I dropped or changed these after checking source.

| Claim | Reviewer | Fact |
| --- | --- | --- |
| `session_is_active` has no test anywhere | D | `services/identity/tests/session_exchange.rs:271` and `:476` test both active and inactive |
| `ControlRouteAuthenticator` has no test at all | C | covered by `execution/host/tests/host_route_live.rs` and `tests/integration/src/control_client_live.rs`; no unit test only |
| generated update SQL never runs against PG | E | `tests/integration/src/route_interface_live.rs:349-366` runs it, stale revision included; the gap is ownership, not coverage |
| ctl `role_verbs` have no test | F | `tests/integration/src/membership_test.rs` and `services/ctl/tests/receiving_ui_live.rs` call them |
| `admit_event_batch` has no test | C | `control/lib/tests/workflow_schedule_live/event_batch.rs` covers it live |
| batch with one invalid line must write nothing | G | contradicts the 2026-10-20 per-item ruling; replaced by the sibling-commit test |
| delete `dependency_boundary.rs` | D, F | it enforces the edge dependency rule; move it to repo-lint instead |
| delete `scheduler_nats_move_live.rs` | F | already ignored; only NATS-reconnect test; keep it |

Reviewer files, with each finding's quoted evidence: `/tmp/claude-0/-home-claude/d77748db-5dd1-50bf-b68a-2bdd9914e67f/scratchpad/review/` (A–H). They live in this session only.
