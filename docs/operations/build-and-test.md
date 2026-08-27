# Build and test

The gate of record, the per-bead commands, and the traps that report green
without executing anything.

Every measurement below is stated at the commit it was taken at. **A count
measured before a commit does not describe the tree after it** — re-measure
before citing one as current.

## What the gate of record is

For anything that touches the deployed surface, the gate of record is a
**Job in the local `kind` cluster named `wamn`**, running the two-stage image
built from the repository `Dockerfile`:

```bash
docker build --target host  -t wamn-host:dev  .
docker build --target gates -t wamn-gates:dev .
kind load docker-image wamn-host:dev  --name wamn
kind load docker-image wamn-gates:dev --name wamn
```

Load **both** when host code changes: the `gates` stage is `FROM host`
(`Dockerfile:238`), so the suite runs against the same host lib code it
verifies. Host-built binaries cannot be `COPY`d into the image — the build
stages are `rust:1.97-trixie` and the runtime stages `debian:trixie-slim`, and
a host toolchain's glibc does not match.

**Local Cargo success never substitutes for a named in-cluster gate of
record.** `architecture/workspace-tiers.json` says the same thing in its
`deployed_system_proof.command_semantics`.

The gate Job manifests are `deploy/gates/*-job.yaml`, applied per run and
deleted after (`deploy/README.md`). At `1bffa614` there are three:
`m1-gate-job.yaml`, `socketguard-job.yaml`, `traceproof-job.yaml`, plus the
`serve-echo.yaml` support Deployment that `traceproof` reads back from.

```bash
kubectl -n wamn-system apply -f deploy/gates/socketguard-job.yaml
kubectl -n wamn-system logs -f job/socketguard
```

`tools/kubernetes-gate-run` is the runner that turns a manifest into a
machine-decidable verdict (`--manifest`, `--verdict-record`, one `--job` JSON
per Job; `--help` prints the full option set). `deploy/gates/m1-gate-job.yaml`
carries its own complete invocation in its header comment — build the sidecar,
render the manifest, then `tools/kubernetes-gate-run`. Do not paraphrase it;
read it.

`tools/kind-gate-build --image REF --cache-ref REF` builds a `--target gates`
image with a caller-owned registry cache and loads it into kind. It refuses the
protected tags `dev`, `latest`, and `callable-flow-base-*`.

## Build

**Debug by default.** `cargo build` / `cargo test`. Use `--release` only when a
named gate needs it — the `Dockerfile` stages do, and
`deploy/gates/m1-gate-job.yaml` explicitly does *not* ("Build the gate binary
and materializer with the debug-only SR-MVP recipe; do not use the repository
Dockerfile's release stages for this gate receipt").

**Do not build to verify a config or manifest edit.** `cargo metadata
--no-deps` proves a manifest parses in seconds and compiles nothing.

Native services and the gate binary:

```bash
cargo build -p wamn-host -p wamn-ctl -p wamn-dispatcher \
  -p wamn-executor -p wamn-scenario-worker -p wamn-cdc-reader -p wamn-gates
```

Guests live in **two** Cargo workspaces and must not share one invocation —
feature unification is additive-only and would force `std` into the `no_std`
guests (`components/Cargo.toml` header, wamn-0h0g.11.56):

```bash
(cd components         && cargo build --target wasm32-wasip2)
(cd components/no-std  && cargo build --target wasm32-wasip2)
```

`tools/build-components m1 | proof` does the same selection from the canonical
inventory in `architecture/workspace-tiers.json` instead of by hand; it
requires `jq`. `tools/workspace-tier list|dry-run|run TIER WORKSPACE MODE`
resolves a named tier's package selectors from the same manifest.

## Fork sync

The fork-sync gate takes an explicit wasmCloud checkout and refuses unless
`wamn/2.8.0`, the peeled `v2.8.0` tag, and the recorded upstream revision are
the same commit:

```bash
tools/fork-sync-check dry-run ../wasmcloud
tools/fork-sync-check run ../wasmcloud
```

The run requires a clean checkout, runs the fork formatter and wash-runtime
tests, then runs wash's template-clone fixture. It exports
`GIT_CONFIG_GLOBAL=/dev/null` and `GIT_CONFIG_NOSYSTEM=1` for every Git and
Cargo child, so developer settings such as `tag.gpgsign=true` cannot change
the fixture. Keep that isolation in this WAMN-owned gate; do not carry a
fixture-only patch in the wasmCloud branch.

The gate deliberately does not elevate upstream rustdoc warnings into a WAMN
fork requirement. The branch policy is vanilla source plus only a consumed,
red behavior patch; local documentation strictness is not such a behavior.

## The full sweep

```bash
cargo test --workspace --no-fail-fast > sweep.txt 2>&1
```

- `--no-fail-fast` is **mandatory**: plain `cargo test` stops at the first
  failing binary, so a sweep without it reports the first failure and nothing
  after it.
- **Run it unpiped.** A pipe through `tail` without `pipefail` reports *tail's*
  exit status and truncates the per-binary counts. Capture to a file, then
  analyse the file.
- `--workspace` is required. Cargo otherwise selects default members only.
  Measured at `1bffa614` from `cargo metadata --no-deps`: **17 default members
  of 35 workspace members.** `architecture/workspace-tiers.json`'s `full_ci`
  tier agrees on the 35.

**Measured state at `1bffa614`, by the owner, not re-run here: 168 binaries,
1448 passed, 21 failed, 34 ignored, no compile errors. All 21 failures are
attributed to known causes.** The branch is deliberately red and that is an
accepted owner position — a red sweep does not block feature work. This is a
measurement at a named commit, not a standing promise.

The 34 ignored is corroborated independently: `grep -rn '#\[ignore' --include=*.rs .`
returns 34 at `1bffa614`. See "Live gates" below for why ignored is not skipped.

## Conformance

```bash
cargo test -p wamn-proof-conformance --no-fail-fast
```

**Conformance runs at the wave-end integrator pass, not per lane.** A lane
runs the targeted `-p` selection its own change touches. Running the whole
conformance package inside every lane costs a full build per lane and
re-measures artifacts other lanes are still moving.

Verify the package name before trusting `-p`: it is **`wamn-proof-conformance`**.
A nonexistent name *errors* and greps as zero failures.

The `deploy/platform` bill of materials (`wamn-0h0g.10.5`) is a static
structural proof of the same kind, but it lives in `wamn-proof-system` — it
belongs beside the conformance guards and was kept out of that package only so
it would not collide with `wamn-0h0g.12.10`'s retained-manifest reconcile.
Measured 6 passed / 0 failed on the `w65-deploy` branch, base `2179f9c7`:

```bash
cargo test -p wamn-proof-system --test deploy_platform_inventory
```

### Known red

**Measured at `c72194c7`: the lib is 62 passed / 1 failed, and all twenty
test binaries are green.** This is a measurement at a named commit, not a
standing promise.

| target | state |
| --- | --- |
| `version_identity::governed_wire_schema_and_artifact_versions_stay_at_mvp_identity` | **RED** — governed first-party occurrence-count drift in `crates/execution/run-state/src/admission.rs` and `crates/platform/runtime/src/plugins/connection_http.rs` |

That is the **only** conformance red at this commit, and it is one of the two
long-standing reds on this branch (the other, `connection_http_maps_an_invalid_context_to_a_wit_error_not_a_trap`, is a runtime plugin live test and not in this package).

An earlier inventory recorded *seven* red conformance guards — `gate_registry`,
`workspace_tiers`, `package_architecture`, `protected_relations`,
`runtime_inventory`, `repo_lint` and `contract_diff`. **Re-measured at
`c72194c7`, six of those seven are green** (`gate_registry` 12/0,
`workspace_tiers` 9/0, `package_architecture` 9/0, `protected_relations` 2/0,
`repo_lint` 5/0, `contract_diff` 3/0; `runtime_inventory` is a lib module, not
a separate binary, and is inside the 62 that pass). The inventory was written
on 2026-08-23 and the tree moved under it. **Re-measure before citing a red;
a stale red list is the same false-evidence defect as a stale green one.**

Note `contract_diff` proves the *argv* against a fake cargo, so a green
`contract_diff` is never evidence that the contract legs themselves are green.

## Lint

```bash
tools/repo-lint dry-run   # prints every leg's exact argv, runs nothing
tools/repo-lint run       # runs all ten legs, reports PASS/FAIL per leg
```

`repo-lint` runs one grep-based guard over
`crates/platform/runtime/src/plugins/connection_http.rs` and nine Cargo legs:
rustfmt and Clippy across the root workspace, the components workspace (native
and wasm), and the `no-std` workspace (native and wasm). It reports every leg
and exits non-zero if any failed, rather than stopping at the first.

**`tools/repo-lint run` has never been green.** Measured at `1bffa614`, unpiped:

| leg | exit | diff hunks |
| --- | --- | --- |
| `cargo fmt --manifest-path Cargo.toml --all -- --check` | 1 | 19 |
| `cargo fmt --manifest-path components/Cargo.toml --all -- --check` | 1 | 19 |
| `cargo fmt --manifest-path components/no-std/Cargo.toml --all -- --check` | 0 | 0 |

Eight files carry the diffs: `crates/catalog/model/src/serving_manifest.rs`,
`crates/catalog/model/src/wiring.rs`, `crates/catalog/model/tests/identity.rs`,
`services/ctl/tests/verb_surface.rs`,
`services/scenario-worker/src/store/test_orchestration.rs` (DELETED by
wamn-0h0g.8.5.5 — the row stays because the table is a measurement dated to
`1bffa614`, not a live inventory),
`tests/conformance/tests/gate_registry.rs`,
`tests/conformance/tests/retained_root_outcomes.rs`,
`tests/integration/src/trusted_http_route.rs`.

This is **inventory item A on `wamn-0h0g.15.137`**, which records the same legs
red at base `c2d805e0` and the correct way to check a single file
(`rustfmt --edition 2024 --check` on a real path — reading from stdin does not
report through the exit code and silently reports clean).

`tools/contract-diff dry-run | run` runs the three WIT/contract legs
(`wamn-authoring-model --test contract`, `wamn-runtime --test
flow_http_routing_wit_coherence`, `http-route --test adversarial`).
`wamn-0h0g.15.137` note 5 records that `tests/conformance/tests/contract_diff.rs`
proves the *argv* against a fake cargo, so a green `contract_diff` is never
evidence that the legs themselves are green.

**`tools/contract-diff run` is the runner of record for those three legs, and
it belongs in the sweep of record** (`wamn-0h0g.15.138`). Run it after the
workspace sweep:

```bash
tools/contract-diff run > contract-diff.txt 2>&1
```

The reason it is not redundant with `cargo test --workspace`: legs 1 and 2 name
`crates/authoring/model` and `crates/platform/runtime`, both root-workspace
default members, so the sweep does run them. **Leg 3 names `http-route`, which
is in the `components/` workspace and is not a root workspace member at all**
(measured at `2179f9c7` from `cargo metadata --no-deps`: 35 root members,
`http-route` absent). No root sweep reaches it. `tools/contract-diff run` is the
only command in this document that does.

**Measured at `2179f9c7`, unpiped: green — exit 0, 14 + 7 + 11 = 32 assertions
passed, 0 failed.** Wall clock 7:46 cold in a fresh worktree (it builds both
workspaces), **0.72s warm**. Warm, it is close to free; that cost is why the
answer here is to run it rather than only to document that nobody does.

That is the answer to "a green `contract_diff` proves nothing about guard
health — so what does": **`tools/contract-diff run`, and nothing else in this
file.** The `contract_diff` conformance test proves the plan; this command
proves the legs. Neither substitutes for the other.

## Live gates: arming

**Env-gated live tests are the single biggest source of false green in this
repository.**

Two distinct mechanisms, and they behave differently:

1. **Self-skipping.** A test whose body does
   `let Ok(url) = std::env::var("WAMN_…_PG_URL") else { eprintln!(…); return; }`
   reports **PASS** when the variable is unset. libtest's default capture
   swallows the `eprintln!`, so the run prints `test result: ok`.
2. **`#[ignore]`.** An ignored test that `.expect()`s its variable does **not**
   self-skip — without `-- --ignored` it simply never runs, and the count of
   ignored tests is the only trace.

**Nothing in this repository sets any `WAMN_*_PG_URL` or `WAMN_*_NATS_URL`.**
Measured at `1bffa614`: grepping every `*.yaml`, `*.sh`, `*.toml`, `*.json` for
`WAMN_CTL_PG_URL`, `WAMN_PROVISION_PG_URL`,
`WAMN_MANAGEMENT_ADMITTER_PG18_URL`, `WAMN_READER_PG_URL` returns nothing.
Re-measured at `wamn-0h0g.22.17` for a **fifth** server,
`WAMN_PLATFORM_IDENTITY_PG_URL`
(`services/scenario-worker/tests/management_live.rs`, recipe `[MGMT-LIVE]`
below): also nothing. A gate whose variable nothing sets
**has never executed**.
`wamn-0h0g.15.137` inventory item 1 records what happened when wave 56 set one
by hand for the first time: `services/dispatcher/tests/read_authority.rs`
failed at four independent layers, three of which no measurement pass had found.
`wamn-0h0g.22.17` armed `WAMN_PLATFORM_IDENTITY_PG_URL` for the first time and
found the same shape: the run PASSES, and the only difference between a real run
and the self-skip is the duration and one `eprintln!` libtest swallows. **Diff
the duration.** Measured on that bead: unarmed `finished in 0.00s` with
`skipping management_surface_…` under `--nocapture`; armed `finished in 0.65s`
with no skip line and a `wamn-db-acme--receiving--dev--k3m9x2p7` database plus
three minted generation logins left on the server.

Two things are arranged so they cannot go unarmed.
`deploy/gates/m1-gate-job.yaml` sets `WAMN_PG_URL`, `WAMN_PG_ADMIN_URL`,
`WAMN_SYSTEM_ADMIN_URL`, and `WAMN_EVT_NATS_URL` for its own Pod, pointing at a
self-contained sidecar in that Pod. The benches take their substrate as
arguments (`--admin-database-url`, `--nats-url`) rather than from the
environment, so a missing one is a parse error.

**Every `wamn-ctl` live gate shares one variable and one lock.**
`services/ctl/tests/support/mod.rs` builds the name as
`concat!("WAMN_CTL_", "PG_URL")` — a naive `grep WAMN_CTL_PG_URL` over `*.rs`
does not find it — and takes a cross-process file lock at
`$TMPDIR/wamn-ctl-live-database.lock`, so two ctl live suites block rather than
contaminate each other. `LockedUrl::required(…)` panics when the variable is
unset (the `#[ignore]` gates); `LockedUrl::optional()` self-skips (the rest).
`services/ctl/tests/verb_surface.rs` guards which constructor each file uses.

## Recipes

These are the section tags cited from source doc comments. Each one names the
test that needs it, the variable that arms it, and what the substrate must be.
Every substrate below is a **throwaway** Postgres — see the next section.

### `[11.8]` — ops-only schema-change impact analysis

`services/ctl/tests/impact_report_live.rs` (wamn-wvb). The file is
`#![cfg(feature = "ops")]` **and** the target is `required-features = ["ops"]`
in `services/ctl/Cargo.toml`.

```bash
WAMN_CTL_PG_URL=postgresql://postgres:pw@127.0.0.1:PORT/postgres \
  cargo test -p wamn-ctl --features ops --test impact_report_live
```

`WAMN_CTL_PG_URL` must be a **superuser** URL. Self-skips when unset.

Two tests, both armed by that one variable and serialized by the shared
`WAMN_CTL_PG_URL` lock.
`impact_report_says_when_the_registration_edge_class_is_unevaluated`
(wamn-0h0g.12.120) additionally MINTS a `wamn_app_<tenant key>_a` generation
login — cluster-global, dropped in its own teardown — and DROPs
`catalog.event_registrations` mid-test. Disposable server only, and give this
binary its own container.

### `[EVT-REG/D24]` — registration-orphan guard

`services/ctl/tests/orphan_guard_live.rs` (wamn-rmxa, wamn-0h0g.12.119). Three
`#[ignore]` tests; they fail loudly rather than skipping when invoked without
configuration.

```bash
WAMN_CTL_PG_URL=postgresql://postgres:pw@127.0.0.1:PORT/postgres \
  cargo test -p wamn-ctl --test orphan_guard_live -- --ignored --test-threads=1
```

Superuser, path `/postgres`. Every test in the binary rebuilds the fixed
`catalog` schema in its preamble and they must not interleave — the file
carries its own `SERIALIZE` mutex, but `--test-threads=1` is the safe form.

### `[EVT-REPLICA-IDENT]` — per-entity `REPLICA IDENTITY FULL` reconciler

`services/ctl/tests/replica_identity_live.rs` (wamn-l5i9.31).

```bash
WAMN_CTL_PG_URL=postgresql://postgres:pw@127.0.0.1:PORT/postgres \
  cargo test -p wamn-ctl --test replica_identity_live
```

Superuser, path `/postgres`, and the server **must** be booted
`wal_level=logical` — the test creates a `test_decoding` slot before any writes
and compares WAL before and after the flip.

### `[RUN-PLANE-RECONCILE]` — `reconcile-run-plane` migration path

`services/ctl/tests/run_plane_live.rs` (E4/R14-migration, wamn-1wdq).

```bash
WAMN_CTL_PG_URL=postgresql://postgres:pw@127.0.0.1:PORT/postgres \
  cargo test -p wamn-ctl --test run_plane_live -- --include-ignored --test-threads=1
```

Superuser, path `/postgres`. The legs share the `catalog` schema and the
`wamn_app` role, so they run sequentially under one entry; the execution-pin
cutover has a second entry.

**`--include-ignored`, NOT `--ignored`.** This binary is MIXED: 7 of its 17
tests carry `#[ignore]` and `.expect()` the variable; the other 10 take
`LockedUrl::optional()` and SELF-SKIP. `-- --ignored` runs *only* the ignored
set, so it reports `10 filtered out` and every self-skipping test is invisible.
Measured 2026-08-27 on one fresh `postgres:18` at `2179f9c7`: `-- --ignored`
reported `5 passed; 2 failed; 10 filtered out`, while `-- --include-ignored` on
an identical fresh server reported `9 passed; 8 failed; 0 filtered out` — the
same run, with six more reds visible. The flag was hiding them, which is the
false-green class this document exists to close.

### `[CATALOG-PLANE]` — the catalog plane-residency refusal

`services/ctl/tests/catalog_plane_residency_live.rs` (`wamn-0h0g.12.180`).

```bash
WAMN_CTL_PG_URL=postgresql://postgres:pw@127.0.0.1:PORT/postgres \
  cargo test -p wamn-ctl --test catalog_plane_residency_live -- --ignored
```

Superuser URL. Builds BOTH stores from the production artifacts and proves
`ensure_catalog_storage` refuses a CONTROL database *before* it mutates
cluster-global role state. The witness is `catalog.authoring_command_audit`, not
a shared name — `catalog.catalogs` exists in both planes and cannot tell them
apart. Proves an UPGRADE, not a virgin install: it retires the release-component
migration block, proves reinstallation, and asserts a further pass is a no-op on
the table inventory. Reserves the shared `wamn-ctl` lock and hands the database
back with `PUBLIC CONNECT` restored.

### `[PROVISION-ORDER]` — the documented provisioning order, end to end

`services/ctl/tests/provisioning_order_live.rs` (`wamn-0h0g.12.179`).

```bash
WAMN_PROVISIONING_ORDER_PG18_URL=postgres://postgres:pw@127.0.0.1:PORT/postgres \
  cargo test -p wamn-ctl --test provisioning_order_live -- --ignored --test-threads=1
```

**Needs a DISPOSABLE cluster** — it drops the cluster-global `wamn_app` and
`wamn_dispatch_reader`. Two arms: an operator following the documented order
completes it, and a refused prepare leaves exactly the state its documentation
promises. The second arm is why the emitted `priv.sql` no longer grants
`CONNECT` to the stable guest ACL role: `wamn_app` is cluster-global and
generations inherit it, so one grant per environment reached EVERY environment
on the cluster.

### `[EVT-READER]` — CDC event reader

`services/cdc-reader/tests/event_reader_live.rs` (wamn-l5i9.10, D19 v3 §4).
Needs **two** substrates and self-skips when either is unset.

```bash
WAMN_READER_PG_URL=postgresql://postgres:pw@127.0.0.1:PORT/postgres \
WAMN_READER_NATS_URL=nats://127.0.0.1:PORT \
  cargo test -p wamn-cdc-reader --test event_reader_live
```

Postgres 18 superuser at path `/postgres`, `wal_level=logical`; NATS with
JetStream enabled.

### `[EVT-C-CDC]` — the CDC ceiling campaign

`tests/integration/src/cdcbench.rs` (wamn-l5i9.14). A **measurement** campaign,
not a regression gate: it emits curves and knees, and only sanity and
completeness asserts gate. Four modes — `drain`, `lag`, `ri`, `switchover` —
plus `all` (which excludes `switchover`). It needs a superuser
`--admin-database-url` at path `/postgres` on a `wal_level=logical` server and a
JetStream `--nats-url`.

**No runnable invocation is recorded here, because none exists at `1bffa614`.**
`cdcbench` is a `pub mod` of `wamn-proof-integration`, which has a **lib target
only** (`cargo metadata`), and the `wamn-gates` binary
(`tests/orchestrator/src/main.rs`) exposes exactly eight subcommands —
`retention`, `readerbench`, `m1`, `m1-cleanup`, `serve-echo`, `socketguard`,
`traceproof`, `dashproof` — and `cdcbench` is not among them. The same is true
of `provisionbench`, `streambench`, `walbench`, `rie2ebench`, `catalog_live`,
`causation_e2e`, `exposure_live`, and `trusted_http_route`. This is exactly the
shape `wamn-0h0g.15.137` exists to inventory: a verification artifact with no
runner of record.

### `[R18-NEG]` — the `standard_conforming_strings=off` fail-closed negative

`crates/platform/runtime/src/plugins/wamn_postgres/claims.rs`,
`live_scs_off_server_fails_checkout_closed` (wamn-2jkm.65). Gated on a
**separate** variable so it never runs against the stock test server, and skips
loudly when unset.

```bash
docker run -d --name wamn-scsoff -e POSTGRES_PASSWORD=pw \
  -p 127.0.0.1:PORT:5432 postgres:18 -c standard_conforming_strings=off
WAMN_SCS_OFF_PG_URL=postgresql://postgres:pw@127.0.0.1:PORT/postgres \
  cargo test -p wamn-runtime --lib live_scs_off_server_fails_checkout_closed
```

The test asserts the server genuinely reports `off` before proceeding, so a
stock server makes it fail rather than pass vacuously.

### `[TENANT-KEY]` — the Rust≡SQL tenant-key agreement gate

`crates/control/provision/tests/tenant_key_live.rs` (`wamn-0h0g.22.6.1`). The
sharpest failure mode in Phase B item 2: provisioning mints a guest login whose
name carries the scope digest, and every governed RLS predicate recomputes that
digest **in SQL** from `current_user`. One of those two implementations exists
only inside PostgreSQL, so a disagreement is invisible to every pure test — and
it would make every guest read refuse.

```bash
docker run -d --name wamn-tenantkey-pg -e POSTGRES_PASSWORD=probe \
  -p 127.0.0.1:5433:5432 postgres:18
# pg_isready LIES during postgres:18 init-then-restart; ground truth is a query.
until docker exec wamn-tenantkey-pg psql -U postgres -tAc 'select 1' >/dev/null 2>&1; do :; done
WAMN_TENANT_KEY_PG_URL=postgres://postgres:probe@localhost:5433/postgres \
  cargo test -p wamn-control-provision --test tenant_key_live
docker rm -f wamn-tenantkey-pg      # BY EXPLICIT NAME. Never prune.
```

Each test builds **its own** database and **its own** roles, so the five are
safe under the default parallel runner. They previously shared both and
destroyed each other when the workspace sweep ran them concurrently; isolation
by construction beats remembering `--test-threads=1`.

The gate reads `provolatile`/`proparallel`/`prosecdef` from `pg_proc` rather
than from the DDL text, because a function that silently lost `IMMUTABLE` still
creates fine and breaks every expression index built on it.

`the_session_derivation_returns_the_key_of_the_connected_guest_login`
(`wamn-0h0g.22.6.5`) is the end-to-end arm: it mints a login with
`workload_generation_role`, connects **as that role**, and asserts
`wamn_authority.current_tenant_key()` returns the tenant key — plus two
near-miss role names (`x<login>`, `<login>x`) that prove the pattern's anchors.
Unanchored, either one would read another tenant's rows.

### `[GUEST-RUNTIME]` — the guest SQL path on per-tenant connections

`crates/platform/runtime/src/plugins/wamn_postgres/claims.rs` (`WAMN_PG_TEST_URL`,
`wamn-0h0g.22.6.7`). Ordinary `--lib` tests, so a throwaway server is all it takes.

```bash
docker run -d --name wamn-guestrt-pg -e POSTGRES_PASSWORD=probe \
  -p 127.0.0.1:5436:5432 postgres:18
until psql postgres://postgres:probe@localhost:5436/postgres -Atqc 'select 1'; do :; done
WAMN_PG_TEST_URL=postgres://postgres:probe@localhost:5436/postgres \
  cargo test -p wamn-runtime --all-features --lib wamn_postgres
docker rm -f wamn-guestrt-pg      # BY EXPLICIT NAME. Never prune.
```

**A guest live test can no longer point at any URL.** Guest credential
resolution verifies that the login carries `app_scope_hash(tenant, database)`,
so the fixtures compose a properly named generation (`live_guest_url`) and
create the stable `wamn_app` ACL role — the credential-exactness hook requires
membership in it, and a fresh cluster has neither.

`live_begin_with_claims_sets_the_guest_set_without_a_tenant_claim` asserts
`current_setting('app.tenant', true)` is **NULL**, not `''`: a custom GUC reads
back as the empty string once it has been set and the `SET LOCAL` scope ended,
and NULL only if it was never set in the session at all. NULL is therefore the
sharper claim — the guest transaction never touched it.

### `[TENANT-FLOOR]` — the swept hand-written tenant floor

`crates/control/provision/tests/deploy_sql_authority.rs` (`wamn-0h0g.22.6.3`).
Applies the REAL `deploy/sql` files and asks the server, not the file text.

```bash
docker run -d --name wamn-floor-pg -e POSTGRES_PASSWORD=probe \
  -p 127.0.0.1:5434:5432 postgres:18
until psql postgres://postgres:probe@localhost:5434/postgres -Atqc 'select 1'; do :; done
WAMN_TENANT_FLOOR_PG_URL=postgres://postgres:probe@localhost:5434/postgres \
  cargo test -p wamn-control-provision --test deploy_sql_authority -- --test-threads=1
docker rm -f wamn-floor-pg      # BY EXPLICIT NAME. Never prune.
```

**`--test-threads=1` IS REQUIRED, NOT OPTIONAL, and the reason is now the
DATABASE, not the role** (`wamn-0h0g.12.188`). Measured 2026-08-27 on a
certified-empty cluster: without the flag this binary reports `6 passed;
2 failed` with

```
ERROR:  duplicate key value violates unique constraint "pg_database_datname_index"
DETAIL:  Key (datname)=(wamn) already exists.
```

More than one test applies `postgres-init.sql`, whose `CREATE DATABASE wamn` is
bare, and **no `DO` block can guard it**: `CREATE DATABASE` must be its own
autocommit statement and plpgsql cannot execute it inside a function body. The
only alternatives are a client-side existence check — which moves logic out of
the artifact and weakens it as a self-contained install — or accepting the
serialization. **The serialization is accepted.** Single-threaded it is
`8 passed; 0 failed` and passes TWICE IN A ROW on the SAME cluster, which is the
bar for a gate applying an artifact that creates cluster-global objects.

THE ROLE RACE THAT USED TO BE THE REASON IS CLOSED. `wamn-0h0g.12.186` guarded
the bare `CREATE ROLE wamn_app`, so `duplicate key value violates unique
constraint "pg_authid_rolname_index", Key (rolname)=(wamn_app) already exists`
no longer occurs. Guarding the role MOVED the parallel failure rather than
removing it; do not remove the flag on the strength of that fix.

Three arms: no guest-reachable relation keys on a settable claim; all 43
re-keyed relations carry their `<table>_tkey` expression index (from
`pg_index`); and a login composed by `workload_generation_role` reads its own
tenant and only its own from `catalog.catalogs` — while setting `app.tenant` to
the other tenant, which now buys nothing.

The two relations that KEEP the claim (`wamn_run.operator_run_actions`,
`wamn_run.run_queue`) are asserted as an exact set, so the sweep cannot pass by
granting the guest access to them instead.

`wamn-0h0g.22.17` adds two more tests to the same binary, for the SECOND arm
every governed relation now carries. The floor is the GUEST floor, narrowed
`TO wamn_app`; PostgreSQL default-denies when RLS is enabled and no policy
matches the connected role, so that narrowing LOCKS OUT every platform-grain
principal — at ZERO ROWS, with no error. One permissive arm `TO wamn_platform`
per relation admits them. `every_governed_relation_carries_both_the_guest_floor_and_one_platform_arm`
pins both counts per file (24/7/5/7), because adding a `TO` clause moves no
governed clause, no retired clause and no expression index — a narrowing applied
to 40 of the 43 passes every assertion that predates the bead.
`the_platform_arm_admits_every_platform_family_from_the_server` asks `pg_policy`
for the arms PER RELATION, asks `pg_auth_members` for the exact member set and
its edge options, and then reads `wamn_run.effect_attempts` under three roles: a
platform generation (all tenants), and a login in neither group holding the same
table grant (zero rows).

**`INHERIT TRUE` is the silent one.** Stable ACL roles are `NOINHERIT`, and
PostgreSQL 16+ takes a membership's default `INHERIT` option from the member's
`rolinherit` — so a bare `GRANT wamn_platform TO <acl_role>` lands
`inherit_option = false` and every platform read returns zero rows with nothing
raised. Measured on 18.6: bare grant 0 rows, `INHERIT TRUE` all rows.

**This gate OWNS its server.** `postgres-init.sql` carries a bare
`CREATE DATABASE wamn` and bare `CREATE ROLE`s, so the gate drops the database
and all four cluster roles (`wamn_app`, `wamn_scenario_author`,
`wamn_effect_writer`, `wamn_platform`) plus its two probe roles before applying —
and roles are cluster-wide. `wamn_platform` is the one that matters: a leftover
healthy one carries last run's memberships, and a mutant that deletes the grant
then passes. Point it only at a disposable server, never at one another suite is
using. It passes twice in a row against a surviving cluster; that is the
hermeticity check.

**Host-side readiness only.** `docker exec … psql` returns success during
postgres:18's init-then-restart while the published port is still down; the only
honest probe is connecting from the host.

### `[DDL]` — the generated-DDL apply gates

`crates/schema/compiler/tests/ddl.rs` (seven gates, `WAMN_DDL_PG_URL`). They
apply real generated DDL to a throwaway Postgres.

```bash
WAMN_DDL_PG_URL=postgres://postgres:probe@localhost:5433/postgres \
  cargo test -p wamn-schema-compiler --features ops --test ddl
```

**Generated DDL now has a database precondition** (`wamn-0h0g.22.6.2`): every
emitted policy and tenant-key index calls `wamn_authority.tenant_key`, so the
function must exist or nothing applies. Each gate installs it from
`authority_derivations_sql` — the same builder provisioning uses, so no test
carries a second definition of a security-critical function. The install is
advisory-locked because these gates share one database and run in parallel.

`the_generated_tenant_floor_admits_only_the_connected_guest_on_postgres`
replaces the retired empty-claim gate: it mints a guest login with
`workload_generation_role`, `SET ROLE`s to it, and proves the guest sees its own
tenant's row and **not** another tenant's — plus that the stable `wamn_app` ACL
role derives no key and sees nothing even while setting the retired
`app.tenant`.

`--nocapture` and a grep for `skipping` is the only proof these ran: with the
variable set the count is 0, without it 7.

**These gates gained the same database precondition again at
`wamn-0h0g.22.17`**: every emitted floor is now narrowed `TO wamn_app` and
followed by an `AS PERMISSIVE FOR ALL TO wamn_platform` arm, so `wamn_platform`
must exist or the `CREATE POLICY` fails outright. The three project-database
`deploy/sql` files each create it themselves (advisory-locked, EXCEPTION-guarded)
for exactly that reason; a gate that emits DDL rather than applying a file must
create it, or apply `sql::ensure_platform_group_role_sql()` first.

### `[MGMT-LIVE]` — the authenticated management authoring surface

`services/scenario-worker/tests/management_live.rs`
(`WAMN_PLATFORM_IDENTITY_PG_URL`). The FIFTH server. It provisions a control
database AND a project-environment database, mints control-author,
identity-reader and management-admitter generations, and drives the gate verb
over HTTP.

```bash
docker run -d --name wamn-mgmt-pg -e POSTGRES_PASSWORD=probe \
  -p 127.0.0.1:5437:5432 postgres:18
until psql postgres://postgres:probe@localhost:5437/postgres -Atqc 'select 1'; do :; done
WAMN_PLATFORM_IDENTITY_PG_URL=postgres://postgres:probe@localhost:5437/postgres \
  cargo test -p wamn-scenario-worker --test management_live -- --nocapture
docker rm -f wamn-mgmt-pg      # BY EXPLICIT NAME. Never prune.
```

Superuser, path `/postgres`, and it OWNS its server (it creates databases and
cluster-wide roles). **It self-skips, so a green run is not evidence it ran** —
see the arming section above for the duration diff that is.

`wamn-0h0g.22.17` is what this gate proves that nothing else does: the
management admitter reaches `catalog.wirings` and `catalog.component_library`
under its OWN generation login. That family is
`WorkloadRoleScope::ProjectEnvironment`, which has no tenant field, so
`wamn_authority.current_tenant_key()` derives NULL for it and the guest floor
can never admit it. Measured on the live project database this gate leaves
behind, under the minted admitter with `rolbypassrls = false`: with the platform
arm, 2 rows; with the arm dropped, **0 rows and no error**; with the pre-bead
untargeted floor restored, `ERROR: permission denied for function
current_tenant_key`.

### Other live gates that carry their command in-source

These have no section tag; the file's own doc comment is the recipe of record.

| test | variable | command |
| --- | --- | --- |
| `crates/catalog/model/tests/wiring_activation_live.rs` | `WAMN_CATALOG_PG_URL` | `cargo test -p wamn-catalog --test wiring_activation_live -- --ignored` |
| `crates/platform/runtime/tests/wiring_doorbell_live.rs` | `WAMN_CATALOG_PG_URL` | `cargo test -p wamn-runtime --test wiring_doorbell_live -- --ignored` |
| `services/ctl/tests/effect_writer_generation_live.rs` | `WAMN_EFFECT_WRITER_PG18_URL` | `cargo test -p wamn-ctl --test effect_writer_generation_live -- --ignored --nocapture` |
| `services/ctl/tests/guest_generation_live.rs` | `WAMN_GUEST_GENERATION_PG18_URL` | `cargo test -p wamn-ctl --features ops --test guest_generation_live -- --ignored --nocapture` |
| `services/ctl/tests/management_admitter_generation_live.rs` | `WAMN_MANAGEMENT_ADMITTER_PG18_URL` | `cargo test -p wamn-ctl --test management_admitter_generation_live -- --ignored --nocapture` |
| `services/ctl/tests/terminalize_effect_uncertain_live.rs` | `WAMN_OPERATOR_TERMINALIZE_PG18_URL` | `cargo test -p wamn-ctl --test terminalize_effect_uncertain_live` |
| `services/ctl/tests/release_manifest_mint_live.rs` | `WAMN_RELEASE_MANIFEST_MINT_PG_URL` | `cargo test -p wamn-ctl --test release_manifest_mint_live -- --ignored` |
| `services/ctl/tests/protected_relations_live.rs` | `WAMN_CTL_PG_URL` | `cargo test -p wamn-ctl --features ops --test protected_relations_live -- --ignored` |
| `services/ctl/tests/catalog_confinement_live.rs` | `WAMN_CTL_PG_URL` | `cargo test -p wamn-ctl --test catalog_confinement_live` |
| `services/ctl/tests/author_wiring_gate_report_live.rs` | `WAMN_AUTHOR_WIRING_PROJECT_PG_URL` **and** `WAMN_AUTHOR_WIRING_CONTROL_PG_URL` | `cargo test -p wamn-ctl --test author_wiring_gate_report_live -- --ignored` |
| `services/scenario-worker/tests/management_live.rs` | `WAMN_PLATFORM_IDENTITY_PG_URL` | `cargo test -p wamn-scenario-worker --test management_live` |
| `crates/execution/run-state/tests/effect_writer_live.rs` | `WAMN_RUN_STORE_PG_URL` | `cargo test -p wamn-run-state --features native --test effect_writer_live -- --ignored` |
| `crates/execution/run-state/tests/run_state_live.rs` | `WAMN_RUN_STORE_PG_URL` | `cargo test -p wamn-run-state --test run_state_live -- --include-ignored` |
| `services/dispatcher/tests/read_authority.rs` | `WAMN_PROVISION_PG_URL` | `cargo test -p wamn-dispatcher --test read_authority` |
| `crates/control/provision/tests/control_portable_store.rs` | `WAMN_CONTROL_PORTABLE_PG_URL` | `cargo test -p wamn-control-provision --test control_portable_store -- --include-ignored --test-threads=1` |
| `crates/control/provision/tests/family_surface_grants.rs` | `WAMN_FAMILY_SURFACE_PG_URL` | `cargo test -p wamn-control-provision --test family_surface_grants` |

Rows carrying `-- --ignored` have **every** test in that binary marked
`#[ignore]`; without the flag the binary runs zero tests and reports ok.
`-- --ignored` runs *only* the ignored tests, which for those binaries is all of
them. Measured at `1bffa614` by counting `#[ignore]` against the test
attributes in each file; the rows without it self-skip on an unset variable
instead.

`crates/execution/run-state/tests/run_state_live.rs` **cannot share a server
with `crates/execution/run-state/tests/admission_live.rs`**, even though the two
read the same variable. `admission_live` REVOKEs `ALL ... FROM PUBLIC` on the
database it is pointed at and re-grants `CONNECT` to named roles only; measured
on `wamn-0h0g.15.137.5`, the `postgres` database ACL afterwards is
`postgres=CTc`, `wamn_app=c`, plus the two minted logins, and PUBLIC is gone —
so `run_state_live`, whose every fenced leg opens with `CREATE TEMP TABLE`,
then dies with `ERROR: permission denied to create temporary tables in database
"postgres"`. Give each of the two its **own** container. Measured together on
one: `admission_live` ok in 3.52s, `run_state_live` FAILED in 1.03s.

`services/ctl/tests/release_manifest_mint_live.rs` is mixed: three plain
`#[test]` and three `#[ignore]`. Run it both ways.

`crates/control/provision/tests/control_portable_store.rs` is mixed the same way:
one `#[ignore]` gate and the rest self-skipping, which is why its row carries
`--include-ignored` rather than `--ignored`. Every gate in it applies the
artifact to the SAME database and resets the control schemas first, so
`--test-threads=1` is not optional: measured in parallel they collide on
`ERROR: tuple concurrently updated`. Its wamn-0h0g.7.11 arm,
`control_portable_store_renames_the_gate_ledger_literal_as_an_upgrade_on_postgres`,
is an R55 UPGRADE proof: it seeds pre-rename `authoring_command_audit` rows in
two tenants, applies `deploy/sql/control-portable-store.sql`, and asserts the
migrated rows and the installed CHECK from `pg_catalog` — then applies a second
time and asserts the post-state is unmoved. Run it alone with

```bash
WAMN_CONTROL_PORTABLE_PG_URL=postgresql://postgres:pw@127.0.0.1:PORT/postgres \
  cargo test -p wamn-control-provision --test control_portable_store -- \
  --include-ignored control_portable_store_renames_the_gate_ledger_literal
```

`author_wiring_gate_report_live` needs **two distinct databases** on a
disposable server — the wiring row is project-plane and the gate report is
control-plane — and its control preamble REVOKEs `CONNECT ... FROM PUBLIC` on
the database it is pointed at, so never share that one with another suite. It
had no row here until `wamn-0h0g.8.29`, which means it had never entered an
arming set.

The two `generation_live` tests revoke `PUBLIC CONNECT` on **every** non-template
database in the cluster. Run them only against a disposable server.
`guest_generation_live` drops and recreates the stable `wamn_app` ACL role,
which is cluster-wide — the same warning applies with more force.

## The throwaway Postgres

Several suites need one. Never point them at anything shared.

```bash
PORT=55471   # pick one; check it first
ss -ltn | grep ":${PORT}\b" && echo "busy, pick another"

docker run -d --name wamn-<suite>-pg -e POSTGRES_PASSWORD=pw \
  -p 127.0.0.1:${PORT}:5432 postgres:18
# add `-c wal_level=logical` for the CDC / replica-identity recipes

# ground truth — loop on this, nothing else
until docker exec wamn-<suite>-pg \
        psql -h 127.0.0.1 -U postgres -tAc 'select 1' >/dev/null 2>&1; do
  sleep 1
done

# … run the suite …

docker rm -f wamn-<suite>-pg    # BY NAME
```

**Why the loop is the only ground truth.** The `postgres:18` entrypoint
initialises the cluster on a unix socket, then restarts the server for TCP.
Measured at `1bffa614` against `postgres:18` (18.6):

- A **host-side TCP connect to the published port accepts immediately**, one
  second in, while `psql` inside the container is still refused. The docker
  proxy listens before the server does. A TCP probe is never evidence.
- **`pg_isready` disagrees with itself** across the window. At t=2s the unix-socket
  form reported `rejecting connections` while the TCP form reported
  `accepting connections` and `psql` returned `1`.

`docker exec <name> psql -h 127.0.0.1 -U postgres -c 'select 1'` tracked the
server's actual ability to answer in both runs. Loop on that.

Rules that follow:

- **Check the port is free first.** Other lanes and other projects use this
  machine; `55432` was already occupied when this was measured.
- **One fresh container per suite.** Roles are cluster-wide, so two suites
  sharing a server contaminate each other. Within a suite, `--test-threads=1`.
- **A superuser fixture masks RLS.** `FORCE ROW LEVEL SECURITY` does not bind a
  superuser or a `BYPASSRLS` role. `crates/schema/control/src/replica_identity.rs:169`
  and `crates/schema/control/src/sql.rs:77` both record this; `deploy/sql/postgres-init.sql:13`
  creates `wamn_app` as `NOSUPERUSER … NOBYPASSRLS` for exactly that reason. A
  test that only ever connects as superuser proves nothing about tenant
  isolation.
- **Remove the container by explicit name. Never `docker prune`.** This machine
  carries hundreds of dangling volumes belonging to other projects.

## The live kind cluster is not a test fixture

The `wamn` kind cluster is frozen. **Never touch, restart, or recreate** the
Postgres fixture pod (`deploy/platform/postgres.yaml` — the shared long-lived
fixture roughly eight gates and the dispatcher point at, per `deploy/README.md`),
the `wamn-pg` pool, `wamn-sysdb`, or the control-plane NATS Deployment named
`nats`. The fixture pod's `PGDATA` is an `emptyDir`: a restart **wipes it**.

When a suite needs a database it can own, the correct tool is a throwaway
docker `postgres:18`, above.

## Traps

**PostgreSQL identifier truncation is silent.** An identifier of 64 bytes or
more is TRUNCATED to 63 with a `NOTICE` — the statement still succeeds. Any
name that embeds a tenant-controlled or otherwise unbounded value must
therefore **refuse at mint** when it would reach 64 bytes, never rely on the
server to reject it. Measured on `wamn-0h0g.22.6`: `valid_tenant` admits 64
bytes, so embedding a tenant id verbatim in a role name would let two long
tenants collapse onto **one role** — a cross-tenant breach wearing a naming
bug. The standing answer is the scope-digest convention
(`workload_role_scope_hash`, 40 hex characters), which is bounded by
construction. This is a hazard *class*, not one bead's finding: it applies to
every future name derived from user-supplied length.

**Never share a `CARGO_TARGET_DIR` between parallel worktrees.** Three
measured failure modes: `env!("CARGO_MANIFEST_DIR")` resolves to *another*
worktree, so a test validates the wrong tree; artifact collision overwrites in
place, so a later run executes the other tree's code; and fingerprint thrash
serialises every lane on one flock. Give each worktree its own directory.

**A wrong package name greps as zero failures.** `cargo test -p <nonexistent>`
errors out — it does not run and report zero. Measured at `1bffa614`:

```
$ cargo test -p wamn-runner
error: package ID specification `wamn-runner` did not match any packages
help: a package with a similar name exists: `wamn-router`      # exit 101
```

Names that do **not** exist: `wamn-runner`, `wamn-test-fixtures`, `wamn-flow`,
`flow-http`. The current names: the orchestrator package is **`wamn-gates`**,
conformance is **`wamn-proof-conformance`**, the node contract is
**`wamn-execution-contract`**, the HTTP ingress guest is **`http-route`**.
`cargo metadata --no-deps` is the cheap way to confirm one.

**`--lib` can select nothing.** `wamn-host` has a single `bin` target and no
library. `cargo test -p wamn-host --lib` fails with
`error: no library targets found in package 'wamn-host'` (exit 101); use
`--bins`. `wamn-0h0g.15.137` item 3 records a guard that sat red for a whole
wave because the recorded sweep bar was `--lib`, which selects no `tests/`
binary at all.

**`cargo test NAME -- --exact` against a missing test runs zero tests and exits
0** (`wamn-0h0g.15.137` item 2). A mutation harness reports that as SURVIVED.

**A `required-features` target is silently deselected.**
`services/ctl/Cargo.toml` declares `required-features = ["ops"]` on both
`impact_report_live` and `protected_relations_live`, so a bare
`cargo test -p wamn-ctl` never builds them and is green without them. Naming
one explicitly without the feature does error — measured:

```
$ cargo test -p wamn-ctl --test protected_relations_live --no-run
error: target `protected_relations_live` in package `wamn-ctl` requires the features: `ops`
Consider enabling them by passing, e.g., `--features="ops"`      # exit 101
```

**A `#![cfg(feature = …)]` file compiles to zero tests and reports ok.**
`crates/execution/run-state/tests/effect_writer_live.rs` is
`#![cfg(feature = "native")]`, and `crates/execution/run-state/Cargo.toml`
declares no `required-features` for it. Without `--features native` the binary
builds, runs nothing, and prints `test result: ok`.
`services/ctl/tests/impact_report_live.rs` carries the same `#![cfg]` but is
*also* `required-features = ["ops"]`, which is what stops it failing this way —
the manifest declaration is the protection, not the attribute.

**Feature-gated unit tests need the feature *and* the right target.**
`prune_run_history`'s tests are a `#[cfg(test)] mod tests` inside
`services/ctl/src/prune_run_history.rs`, and `services/ctl/src/lib.rs:30-31`
declares the module `#[cfg(feature = "ops")]`. They run only under
`cargo test -p wamn-ctl --features ops --lib prune_run_history`.

**`git grep` for a package name is not a rename check.** `c935b88f` renamed
`flow-http` and repaired `tools/contract-diff` and
`architecture/workspace-tiers.json` in the same commit, and still left three
stale selectors in the `Dockerfile` (fixed at `237085b3`). `cargo metadata
--no-deps` and an actual `-p` resolution are the checks that catch this.

It also swept one token that was **not** a package name: the `world:` argument
to `wit_bindgen::generate!` in `components/ingress/http-route/src/guest.rs`,
which must keep naming the world the crate's own `wit/world.wit` declares —
`flow-http`, deliberately not renamed, because a versioned WIT package is a
contract change. No `-p` resolution catches that one; only building the
component does (`wamn-0h0g.26.22`).

## Not reconstructed

`architecture/gate-registry.json` carries `SourceKind::Recipe` entries under an
`H5-*` namespace (`H5-CDCBENCH`, `H5-WALBENCH`, `H5-RIE2EBENCH`,
`H5-CREDENTIALS`, `H5-EXECUTION-DEADLINE`, and others).
`tests/conformance/tests/gate_registry.rs:455` records that those named
`# recipe-test:` directives in the deleted `docs/archive/build-and-test.md`,
that the selectors were prose, and that nothing real remains to resolve them
against. **This document does not reconstruct them** — the bodies are gone and
inventing replacements would be worse than the gap. Their classification,
evidence, and decision mapping are still checked by that test.
