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
deleted after (`deploy/README.md`). The two live Job manifests are
`socketguard-job.yaml` and `traceproof-job.yaml`; `serve-echo.yaml` is the
support Deployment that `traceproof` reads back from.

```bash
kubectl -n wamn-system apply -f deploy/gates/socketguard-job.yaml
kubectl -n wamn-system logs -f job/socketguard
```

`tools/kubernetes-gate-run` is the runner that turns a manifest into a
machine-decidable verdict (`--manifest`, `--verdict-record`, one `--job` JSON
per Job; `--help` prints the full option set). Each live Job manifest carries
its complete invocation in its header comment. Do not paraphrase it; read it.

`tools/kind-gate-build --image REF --cache-ref REF` builds a `--target gates`
image with a caller-owned registry cache and loads it into kind. It refuses the
protected tags `dev`, `latest`, and `callable-flow-base-*`.

## Build

**Debug by default.** `cargo build` / `cargo test`. Use `--release` only when a
named gate needs it — the `Dockerfile` stages do.

**Do not build to verify a config or manifest edit.** `cargo metadata
--no-deps` proves a manifest parses in seconds and compiles nothing.

Native services and the gate binary:

```bash
cargo build -p wamn-host -p wamn-ctl -p wamn-dispatcher \
  -p wamn-executor -p wamn-scenario-worker -p wamn-cdc-reader -p wamn-gates
```

Regenerate the checked-in `wamn dev` configuration schema from its owning Rust
input type:

```bash
cargo test -p wamn-ctl --lib --locked --offline \
  dev::config::tests::regenerate_checked_in_dev_config_schema \
  -- --ignored --exact
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
  of 35 workspace members.** The current `architecture/workspace-tiers.json`
  `full_ci` tier carries all 36 current members.
  [relocation: `wamn-10yt.10.29` moved four guest-consumed rlibs out of the root
  workspace into `components/`; the live counts are now 17 default of 35 root
  members and 20 components members. The dated figures above are left as the
  owner measured them.]

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

Generated package contracts and projections have a package-local gate:

```bash
cargo test -p wamn-schema-generator --all-targets --no-fail-fast
```

The Receiving SQLx siblings use one fresh disposable PostgreSQL 18 database.
The live gate refuses a pre-existing `receiving` schema, applies the exact base
and Acme overlay migrations, reads `wamn.json` structurally, and exercises the
shipped update, `receiving.record_receipt`, projection, inspection-handler, and
approval SQL. The command arm proves commit,
rollback, every closed domain refusal, immutable zero-write replay, lexical-
scale conflict, quantity/status invariants, referenced-location delete blocking,
the named receipt-reference constraint mapping, and one inspection row across
receipt and handler redelivery. Native verification selects the schema through
trusted connection context rather than changing the corpus bytes:

```bash
cargo test -p wamn-proof-integration \
  acme_overlay_publication --locked --offline

# Exact cross-package closure and the real Component Model call boundary.
cargo test -p wamn-catalog \
  component_dependency_closure_is_exact_and_acyclic --lib --locked --offline
cargo test -p wamn-ctl \
  serving_registration_is_derived_from_the_exact_handler_and_unique_entry_wiring \
  --lib --locked --offline
cargo test -p wamn-ctl \
  component_dependencies_expand_the_exact_release_closure_and_refuse_cycles \
  --lib --locked --offline
cargo test -p wamn-execution-host \
  nested_permission_denial --lib --locked --offline

set -euo pipefail
RECEIVING_PG_CONTAINER=wamn-receiving-pg18
RECEIVING_PG_PORT=54329
if docker container inspect "$RECEIVING_PG_CONTAINER" >/dev/null 2>&1; then
  echo "$RECEIVING_PG_CONTAINER already exists" >&2
  exit 1
fi
if ss -ltnH | awk '{print $4}' | grep -Eq ":${RECEIVING_PG_PORT}$"; then
  echo "port $RECEIVING_PG_PORT is already in use" >&2
  exit 1
fi
receiving_gate_cleanup() {
  docker rm -f "$RECEIVING_PG_CONTAINER" >/dev/null 2>&1 || true
}
trap receiving_gate_cleanup EXIT

docker run -d --name "$RECEIVING_PG_CONTAINER" \
  -e POSTGRES_PASSWORD=probe -e POSTGRES_DB=wamn_receiving \
  -p "127.0.0.1:${RECEIVING_PG_PORT}:5432" postgres:18
RECEIVING_PG_READY=0
for RECEIVING_PG_ATTEMPT in {1..60}; do
  if docker exec "$RECEIVING_PG_CONTAINER" \
      psql -h 127.0.0.1 -U postgres -d wamn_receiving \
      -tAc 'SELECT 1' >/dev/null 2>&1; then
    RECEIVING_PG_READY=1
    break
  fi
  sleep 1
done
test "$RECEIVING_PG_READY" -eq 1

RECEIVING_DATABASE_URL="postgresql://postgres:probe@127.0.0.1:${RECEIVING_PG_PORT}/wamn_receiving"
RECEIVING_SQLX_DATABASE_URL="${RECEIVING_DATABASE_URL}?options=-csearch_path%3Dreceiving%2Cpublic"

WAMN_RECEIVING_PG_URL="$RECEIVING_DATABASE_URL" cargo test \
  -p wamn-proof-integration \
  receiving_data_access::tests::enum_and_optimistic_update_outcomes_hold_on_postgres_18 \
  --locked --offline -- --ignored --exact

# Two independent derivations must each equal the exact shipped path/byte set.
WAMN_SCHEMA_INTROSPECTION_PG_URL="$RECEIVING_DATABASE_URL" \
  cargo run -p wamn-schema-generator --example materialize_package \
  --locked --offline -- check packages/receiving
WAMN_SCHEMA_INTROSPECTION_PG_URL="$RECEIVING_DATABASE_URL" \
  cargo run -p wamn-schema-generator --example materialize_package \
  --locked --offline -- check packages/receiving

# Normal builds consume the committed .sqlx evidence without a database.
SQLX_OFFLINE=true cargo test -p wamn-proof-conformance \
  --test receiving_sqlx_verifier --locked --offline
cargo test --manifest-path components/Cargo.toml \
  -p wamn-receiving-data-access --all-targets --locked --offline
cargo check --manifest-path components/Cargo.toml \
  -p wamn-receiving-data-access --target wasm32-wasip2 --locked --offline

# On the disposable database, compile the native sibling and verify metadata.
env -u SQLX_OFFLINE DATABASE_URL="$RECEIVING_SQLX_DATABASE_URL" cargo test \
  -p wamn-proof-conformance --test receiving_sqlx_verifier \
  --no-run --locked --offline
cargo sqlx prepare --check --workspace -D "$RECEIVING_SQLX_DATABASE_URL" -- \
  --package wamn-proof-conformance --test receiving_sqlx_verifier
```

Run the two temporary, hash-guarded mutants in the same shell before cleanup.
These commands inject the mutations; compiler and typed-validator outcomes
provide the proof. No test inspects source text for an implementation substring.

```bash
RECEIVING_SQL_FILE=packages/receiving/query/open_purchase_order.sql
RECEIVING_SQL_BASELINE_SHA="$(sha256sum "$RECEIVING_SQL_FILE" | cut -d ' ' -f 1)"
(
  RECEIVING_SQL_BACKUP="$(mktemp)"
  cp "$RECEIVING_SQL_FILE" "$RECEIVING_SQL_BACKUP"
  trap 'cp "$RECEIVING_SQL_BACKUP" "$RECEIVING_SQL_FILE"; rm -f "$RECEIVING_SQL_BACKUP"' EXIT
  perl -0pi -e \
    's/\Q    purchase_order.updated_at\E/    purchase_order.missing_receiving_column AS updated_at/' \
    "$RECEIVING_SQL_FILE"
  if env -u SQLX_OFFLINE DATABASE_URL="$RECEIVING_SQLX_DATABASE_URL" cargo test \
      -p wamn-proof-conformance --test receiving_sqlx_verifier \
      --no-run --locked --offline; then
    echo "broken-column mutant unexpectedly compiled" >&2
    exit 1
  fi
)
test "$(sha256sum "$RECEIVING_SQL_FILE" | cut -d ' ' -f 1)" = \
  "$RECEIVING_SQL_BASELINE_SHA"
env -u SQLX_OFFLINE DATABASE_URL="$RECEIVING_SQLX_DATABASE_URL" cargo test \
  -p wamn-proof-conformance --test receiving_sqlx_verifier \
  --no-run --locked --offline

RECEIVING_GENERATOR_FILE=crates/schema/generator/src/generate.rs
RECEIVING_GENERATOR_BASELINE_SHA="$(sha256sum "$RECEIVING_GENERATOR_FILE" | cut -d ' ' -f 1)"
(
  RECEIVING_GENERATOR_BACKUP="$(mktemp)"
  cp "$RECEIVING_GENERATOR_FILE" "$RECEIVING_GENERATOR_BACKUP"
  trap 'cp "$RECEIVING_GENERATOR_BACKUP" "$RECEIVING_GENERATOR_FILE"; rm -f "$RECEIVING_GENERATOR_BACKUP"' EXIT
  perl -0pi -e \
    's/\Q(Projection::Wamn, ColumnType::Uuid) => "wamn_postgres_sqlx::Uuid",\E/(Projection::Wamn, ColumnType::Uuid) => "String",/' \
    "$RECEIVING_GENERATOR_FILE"
  if cargo test -p wamn-schema-generator --test generation \
      generation_is_byte_stable_and_emits_both_projection_siblings \
      --locked --offline -- --exact --nocapture; then
    echo "Wamn UUID parity mutant unexpectedly passed" >&2
    exit 1
  fi
)
test "$(sha256sum "$RECEIVING_GENERATOR_FILE" | cut -d ' ' -f 1)" = \
  "$RECEIVING_GENERATOR_BASELINE_SHA"
cargo test -p wamn-schema-generator --test generation \
  generation_is_byte_stable_and_emits_both_projection_siblings \
  --locked --offline -- --exact
cargo test -p wamn-schema-generator --test parity --locked --offline

WAMN_CTL_PG_URL="$RECEIVING_DATABASE_URL" cargo test -p wamn-ctl \
  --test package_data_access_live --locked --offline -- \
  --ignored --exact \
  installed_package_set_unions_a_real_app_generation_and_replays_noop \
  --test-threads=1

receiving_gate_cleanup
trap - EXIT
```

### `[EFFECTIVE-RELEASE-POC]` — fresh base + Acme overlay release

This proof applies both package migration streams to one fresh project
database, admits the exact built components, authors every package wiring, and
mints the same format-3 closure twice. It requires byte-identical canonical
bytes and digest on replay, one stored snapshot, exact component dependencies
and event ownership, plus typed refusals for manifest-hash drift and an
unsatisfied generated package weld. The two databases are disposable siblings
on one PostgreSQL 18 server; no upgrade or lineage arm runs. Wiring setup seeds
only the already-ruled steady-state verdict under each document's derived hash;
the production journey remains the proof that the gate writes its first report.

```bash
set -euo pipefail
EFFECTIVE_RELEASE_CONTAINER=wamn-effective-release-pg18
EFFECTIVE_RELEASE_PORT=54334
EFFECTIVE_RELEASE_BASE_COMPONENT=components/target/virtualized/std-empty-environment/receiving.wasm
EFFECTIVE_RELEASE_BASE_DIGEST=498894030af16b22edaf7f0b2104b90673efdc0e32d2a85459368fa308ad8296
if docker container inspect "$EFFECTIVE_RELEASE_CONTAINER" >/dev/null 2>&1; then
  echo "$EFFECTIVE_RELEASE_CONTAINER already exists" >&2
  exit 1
fi
if ss -ltnH | awk '{print $4}' | grep -Eq ":${EFFECTIVE_RELEASE_PORT}$"; then
  echo "port $EFFECTIVE_RELEASE_PORT is already in use" >&2
  exit 1
fi
EFFECTIVE_RELEASE_SCRATCH="$(mktemp -d /tmp/wamn-effective-release.XXXXXX)"
EFFECTIVE_RELEASE_CONTAINER_STARTED=0
effective_release_cleanup() {
  if [ "$EFFECTIVE_RELEASE_CONTAINER_STARTED" -eq 1 ]; then
    docker rm --force "$EFFECTIVE_RELEASE_CONTAINER" >/dev/null 2>&1 || true
    EFFECTIVE_RELEASE_CONTAINER_STARTED=0
  fi
  case "$EFFECTIVE_RELEASE_SCRATCH" in
    /tmp/wamn-effective-release.*) rm -rf -- "$EFFECTIVE_RELEASE_SCRATCH" ;;
  esac
}
trap effective_release_cleanup EXIT

test "$(sha256sum "$EFFECTIVE_RELEASE_BASE_COMPONENT" | cut -d ' ' -f 1)" = \
  "$EFFECTIVE_RELEASE_BASE_DIGEST"
cargo build --manifest-path components/Cargo.toml --locked --offline \
  --target wasm32-wasip2 -p client-acme-receiving
cargo run -p wamn-component-virtualizer --locked --offline -- \
  --input components/target/wasm32-wasip2/debug/client_acme_receiving.wasm \
  --output "$EFFECTIVE_RELEASE_SCRATCH/client_acme_receiving.wasm"

docker run --detach --name "$EFFECTIVE_RELEASE_CONTAINER" \
  -e POSTGRES_PASSWORD=probe \
  -p "127.0.0.1:${EFFECTIVE_RELEASE_PORT}:5432" postgres:18
EFFECTIVE_RELEASE_CONTAINER_STARTED=1
EFFECTIVE_RELEASE_PG_READY=0
for EFFECTIVE_RELEASE_PG_ATTEMPT in {1..60}; do
  if PGPASSWORD=probe psql \
      "postgresql://postgres@127.0.0.1:${EFFECTIVE_RELEASE_PORT}/postgres" \
      -Atqc 'select 1' >/dev/null 2>&1; then
    EFFECTIVE_RELEASE_PG_READY=1
    break
  fi
  sleep 1
done
test "$EFFECTIVE_RELEASE_PG_READY" -eq 1
PGPASSWORD=probe createdb \
  -h 127.0.0.1 -p "$EFFECTIVE_RELEASE_PORT" -U postgres effective_release_project
PGPASSWORD=probe createdb \
  -h 127.0.0.1 -p "$EFFECTIVE_RELEASE_PORT" -U postgres effective_release_control

WAMN_EFFECTIVE_RELEASE_PROJECT_PG_URL="postgresql://postgres:probe@127.0.0.1:${EFFECTIVE_RELEASE_PORT}/effective_release_project" \
WAMN_EFFECTIVE_RELEASE_CONTROL_PG_URL="postgresql://postgres:probe@127.0.0.1:${EFFECTIVE_RELEASE_PORT}/effective_release_control" \
WAMN_EFFECTIVE_RELEASE_BASE_COMPONENT_WASM="$EFFECTIVE_RELEASE_BASE_COMPONENT" \
WAMN_EFFECTIVE_RELEASE_OVERLAY_COMPONENT_WASM="$EFFECTIVE_RELEASE_SCRATCH/client_acme_receiving.wasm" \
  cargo test -p wamn-ctl --lib --locked --offline \
  publish_release::effective_release_live::fresh_base_and_overlay_mint_byte_identically_and_refuse_drift \
  -- --ignored --exact --nocapture --test-threads=1

effective_release_cleanup
trap - EXIT
```

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

Three distinct mechanisms, and they behave differently:

0. **Deselection by name.** `cargo test <path> -- --exact` naming a test that
   no longer exists runs **nothing and exits 0**. The output —
   `running 0 tests` / `test result: ok. 0 passed; N filtered out` — is
   byte-identical to a suite with no matching work to do, which is a normal and
   expected thing to see. This is the family's most dangerous member, because
   the disarming happens in a *different file* from the one that breaks: a
   rename sweeps the test and its callers in Rust, the compiler confirms it,
   and a shell script naming the same test by string is left behind with
   nothing to complain.

   Measured: `8a73233c` renamed the route test from *eleven* to *thirteen* PAT
   routes. `tools/receiving-cluster-journey-run` still named the old one, so
   **every cluster journey run from that commit onward executed no route test
   at all** and then died forty lines later at a missing artifact, under the
   message "production journey emitted an unsafe route-caller Secret mode" — a
   file-mode complaint about a Secret that a test which never ran had never
   written.

   **A test that can be deselected by a rename must assert it ran.** Not
   downstream, where the artifact check lives and blames the wrong thing —
   at the invocation:

   ```bash
   grep -qE '^test result: ok\. 1 passed; 0 failed;' "$log" || {
     echo "cargo test ran no matching test for $name" >&2; exit 1; }
   ```

   Assert the *count*, not merely non-zero: two passing is as wrong as none,
   because an over-broad filter selected something the caller did not name, and
   it reads just as green.

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

Two things are arranged so they cannot go unarmed. The
`[RECEIVING-MATERIALIZER-JOURNEY]` helper owns and supplies its disposable
PostgreSQL, NATS, and authenticated OCI inputs. The benches take their
substrate as arguments (`--admin-database-url`, `--nats-url`) rather than from
the environment, so a missing one is a parse error.

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

### `[STD-GUEST-VIRTUALIZATION]` — std guest imports and trap visibility

This gate builds the Receiving package component through the pinned
virtualization stage, virtualizes the std probe with that same tool, then reads
the resulting component bytes. It requires the component's exact four-package
import set and eight exact operation-instance exports before exercising sentinel
isolation and panic-to-typed-refusal mapping through the production
router/ingress path.

```bash
set -euo pipefail
tools/build-components proof

STD_VIRT_REPOSITORY_ROOT="$(pwd -P)"
STD_VIRT_DIRECTORY="$STD_VIRT_REPOSITORY_ROOT/components/target/virtualized/std-empty-environment"
mkdir -p "$STD_VIRT_DIRECTORY"
cargo run -p wamn-component-virtualizer --locked --offline -- \
  --input components/target/wasm32-wasip2/debug/std_virtualization_probe.wasm \
  --output "$STD_VIRT_DIRECTORY/std_virtualization_probe.wasm"

WAMN_STD_VIRTUALIZATION_COMPONENT_WASM="$STD_VIRT_DIRECTORY/std_virtualization_probe.wasm" \
WAMN_STD_VIRTUALIZATION_RECEIVING_DIRECTORY="$STD_VIRT_DIRECTORY" \
  cargo test -p wamn-proof-integration --lib --locked --offline \
  virtualized_std_guest::tests::virtualized_artifacts_have_exact_imports_and_receiving_exports \
  -- --ignored --exact --nocapture

WAMN_STD_VIRT_PROJECT="wamn-std-virt-$$"
WAMN_STD_VIRT_PG_PORT=54331
WAMN_STD_VIRT_REGISTRY_PORT=5003
export WAMN_STD_VIRT_PG_PORT WAMN_STD_VIRT_REGISTRY_PORT
STD_VIRT_COMPOSE=test-support/infrastructure/std-virtualization.compose.yaml
std_virt_cleanup() {
  docker compose -p "$WAMN_STD_VIRT_PROJECT" -f "$STD_VIRT_COMPOSE" \
    down --volumes >/dev/null 2>&1 || true
}
trap std_virt_cleanup EXIT

docker compose -p "$WAMN_STD_VIRT_PROJECT" -f "$STD_VIRT_COMPOSE" \
  up --detach --wait --wait-timeout 60
STD_VIRT_PG_URL="postgresql://postgres:probe@127.0.0.1:${STD_VIRT_PG_PORT}/postgres"

WAMN_STD_VIRTUALIZATION_SENTINEL=must-not-cross \
WAMN_STD_VIRTUALIZATION_PG_URL="$STD_VIRT_PG_URL" \
WAMN_STD_VIRTUALIZATION_ARTIFACT_BASE="127.0.0.1:${STD_VIRT_REGISTRY_PORT}/wamn/std-proof" \
WAMN_STD_VIRTUALIZATION_COMPONENT_WASM="$STD_VIRT_DIRECTORY/std_virtualization_probe.wasm" \
WAMN_STD_VIRTUALIZATION_FLOW_HTTP_WASM="$STD_VIRT_REPOSITORY_ROOT/components/target/wasm32-wasip2/debug/http_route.wasm" \
  cargo test -p wamn-proof-integration --lib --locked --offline \
  virtualized_std_guest::tests::virtualized_std_guest_hides_the_sentinel_and_maps_a_panic_to_a_typed_refusal \
  -- --ignored --exact --nocapture --test-threads=1

std_virt_cleanup
trap - EXIT
```

The two containers and their ports are owned by this invocation. Never point
the gate at shared infrastructure or the frozen cluster.

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

**No runnable invocation is recorded here.**
`cdcbench` is a `pub mod` of `wamn-proof-integration`, which has a **lib target
only** (`cargo metadata`), and the `wamn-gates` binary
(`tests/orchestrator/src/main.rs`) exposes exactly six subcommands —
`retention`, `readerbench`, `serve-echo`, `socketguard`, `traceproof`, and
`dashproof` — and `cdcbench` is not among them. The same is true of
`provisionbench`, `streambench`, `walbench`, `exposure_live`, and
`trusted_http_route`. This is exactly the
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

### `[SQLX-TRANSACTION]` — the SQLx guest transport and transaction runner

`crates/platform/runtime/tests/sqlx_transaction_live.rs` (`wamn-0h0g.22.2a`).
The ignored gate requires a freshly initialized PostgreSQL 18 cluster and the
separately built `wasi:cli` fixture. It creates and removes its schema plus both
cluster-wide roles. It never uses kind.

```bash
docker run -d --name wamn-sqlx-pg -e POSTGRES_PASSWORD=probe \
  -p 127.0.0.1:5437:5432 postgres:18
until docker exec wamn-sqlx-pg psql -U postgres -tAc 'select 1' >/dev/null 2>&1; do :; done
cargo build --manifest-path components/Cargo.toml -p sqlx-command --target wasm32-wasip2
WAMN_SQLX_TRANSACTION_PG_URL=postgres://postgres:probe@localhost:5437/postgres \
WAMN_SQLX_TRANSACTION_COMPONENT="$PWD/components/target/wasm32-wasip2/debug/sqlx-command.wasm" \
  cargo test -p wamn-runtime --test sqlx_transaction_live -- --include-ignored
docker rm -f wamn-sqlx-pg      # BY EXPLICIT NAME. Never prune.
```

The command commits one row through `query_as`, rolls back a callback error,
and observes the typed permission denial for a row owned by the wrong
`current_user`. The host additionally requires the exact committed database
state and exported `wamn.postgres` `txn.query`/`txn.execute` spans.

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

Three arms: no guest-reachable relation keys on a settable claim; all governed
relations carry their `<table>_tkey` expression index (from
`pg_index`); and a login composed by `workload_generation_role` reads its own
tenant and only its own from `catalog.packages` — while setting `app.tenant` to
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
pins both counts against each installed artifact, because adding a `TO` clause moves no
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

### `[RECEIVING-HOST-OVERLAY]` — rendered Receiving/PAT host values

This gate renders the pinned runtime-operator chart twice: the generic values
alone, then the same base plus the complete Receiving/PAT overlay. It decodes
the rendered Deployment through Kubernetes' client-side loader and proves the
base is release-less, the overlay preserves the full host profile despite Helm
list replacement, and the trusted Receiving scope plus both mandatory scoped
Secret references reach the Pod structurally. It creates no cluster object.

```bash
cargo test -p wamn-proof-conformance --test chart_seam_governance \
  receiving_pat_overlay_renders_a_complete_scoped_host \
  -- --ignored --exact --nocapture
```

The gate requires `helm` and `kubectl`; Helm pulls the chart pinned at 2.8.0.

### `[ROUTE-AUTH-LIVE]` — route PAT and exact-operation authorization

This gate composes the production project-env/PAT provisioner, package applier,
scoped IdentityReader and HttpAdmitter generations, route authentication, and
the every-invocation operation guard. It requires its variable and never
self-skips.

```bash
docker run -d --name wamn-route-auth-pg -e POSTGRES_PASSWORD=probe \
  -p 127.0.0.1:5439:5432 postgres:18
until psql postgres://postgres:probe@localhost:5439/postgres -Atqc 'select 1'; do :; done
WAMN_ROUTE_AUTH_PG18_URL=postgres://postgres:probe@localhost:5439/postgres \
  cargo test -p wamn-proof-integration --lib --locked \
  route_authentication_live::production_route_caller_authentication_and_operation_authorization \
  -- --ignored --exact --nocapture --test-threads=1
docker rm -f wamn-route-auth-pg # BY EXPLICIT NAME. Never prune.
```

It owns the fresh server: control/project databases and cluster-wide roles are
created and reset. The frozen cluster is never a valid target.

### `[WAMN-DEV-LIVE]` — clean twelve-stage product command and cleanup

This gate runs the literal `wamn dev` product command through all twelve
stages. It uses the Receiving route journey's disposable PostgreSQL 18 and
authenticated loopback registry services plus the committed
`receiving-dev-nats` service. The proof owns the production scenario-worker
Gate/Publish surface, validates the clean source commit in the release, checks
that system and durable-environment database ACLs are unchanged, and requires
the command to remove its verification database and stop its native host. Run
it only from a clean worktree. Never point it at shared infrastructure or the
frozen cluster.

The installed upstream `wash` binary publishes the shipped flow-http workload;
`wamn dev` remains the sole stage runner. Registry credentials live only in the
mode-0700 scratch directory and are never printed.

```bash
set -euo pipefail
umask 077

WAMN_DEV_LIVE_ROOT="$(pwd -P)"
test "$(git -C "$WAMN_DEV_LIVE_ROOT" rev-parse --show-toplevel)" = "$WAMN_DEV_LIVE_ROOT"
test -z "$(git -C "$WAMN_DEV_LIVE_ROOT" status --porcelain=v1 --untracked-files=all)"
command -v wash >/dev/null

WAMN_DEV_LIVE_SCRATCH="$(mktemp -d /tmp/wamn-dev-live.XXXXXX)"
WAMN_DEV_LIVE_TARGET="$WAMN_DEV_LIVE_SCRATCH/target"
WAMN_DEV_LIVE_PROJECT="wamn-dev-live-$$"
WAMN_DEV_LIVE_COMPOSE="$WAMN_DEV_LIVE_ROOT/test-support/infrastructure/std-virtualization.compose.yaml"
WAMN_DEV_LIVE_PG_PORT=54332
WAMN_DEV_LIVE_REGISTRY_PORT=5004
WAMN_DEV_LIVE_NATS_PORT=4224
WAMN_DEV_LIVE_TEMPO_PORT=3201
WAMN_DEV_LIVE_OTLP_PORT=4319
WAMN_DEV_LIVE_AUTHORITY="127.0.0.1:${WAMN_DEV_LIVE_REGISTRY_PORT}"
WAMN_DEV_LIVE_USERNAME=wamn-dev-live
WAMN_DEV_LIVE_PASSWORD="$(openssl rand -hex 32)"
WAMN_DEV_LIVE_HTPASSWD="$WAMN_DEV_LIVE_SCRATCH/htpasswd"
WAMN_DEV_LIVE_DOCKER_AUTH="$WAMN_DEV_LIVE_SCRATCH/.dockerconfigjson"
WAMN_DEV_LIVE_FLOW_HTTP_IMAGE="$WAMN_DEV_LIVE_AUTHORITY/wamn/flow-http:dev"

wamn_dev_live_cleanup() {
  docker compose --profile receiving-route -p "$WAMN_DEV_LIVE_PROJECT" \
    -f "$WAMN_DEV_LIVE_COMPOSE" down --volumes --remove-orphans \
    >/dev/null 2>&1 || true
  if [[ "$WAMN_DEV_LIVE_SCRATCH" == /tmp/wamn-dev-live.* ]]; then
    rm -rf -- "$WAMN_DEV_LIVE_SCRATCH"
  fi
}
trap wamn_dev_live_cleanup EXIT

RUSTC_WRAPPER= CARGO_TARGET_DIR="$WAMN_DEV_LIVE_TARGET" \
  cargo build -p wamn-ctl --bin wamn --locked --offline
RUSTC_WRAPPER= CARGO_TARGET_DIR="$WAMN_DEV_LIVE_TARGET" \
  cargo build -p wamn-host --locked --offline
RUSTC_WRAPPER= CARGO_TARGET_DIR="$WAMN_DEV_LIVE_TARGET" \
  cargo build --manifest-path "$WAMN_DEV_LIVE_ROOT/components/Cargo.toml" \
    -p http-route --target wasm32-wasip2 --locked --offline
WAMN_DEV_LIVE_BIN="$WAMN_DEV_LIVE_TARGET/debug/wamn"
WAMN_DEV_LIVE_HOST_BIN="$WAMN_DEV_LIVE_TARGET/debug/wamn-host"
WAMN_DEV_LIVE_FLOW_HTTP="$WAMN_DEV_LIVE_TARGET/wasm32-wasip2/debug/http_route.wasm"
test -x "$WAMN_DEV_LIVE_BIN"
test -x "$WAMN_DEV_LIVE_HOST_BIN"
test -s "$WAMN_DEV_LIVE_FLOW_HTTP"

printf '%s\n' "$WAMN_DEV_LIVE_PASSWORD" \
  | docker run --rm -i --entrypoint htpasswd httpd:2-alpine \
      -Bni "$WAMN_DEV_LIVE_USERNAME" >"$WAMN_DEV_LIVE_HTPASSWD"
jq -n --arg authority "$WAMN_DEV_LIVE_AUTHORITY" \
  --arg username "$WAMN_DEV_LIVE_USERNAME" \
  --arg password "$WAMN_DEV_LIVE_PASSWORD" \
  '{auths:{($authority):{username:$username,password:$password}}}' \
  >"$WAMN_DEV_LIVE_DOCKER_AUTH"
jq -e --arg authority "$WAMN_DEV_LIVE_AUTHORITY" '
  .auths[$authority]
  | (.username | type == "string" and length > 0)
    and (.password | type == "string" and length > 0)
' "$WAMN_DEV_LIVE_DOCKER_AUTH" >/dev/null

WAMN_STD_VIRT_PG_PORT="$WAMN_DEV_LIVE_PG_PORT"
WAMN_STD_VIRT_REGISTRY_PORT=5003
WAMN_ROUTE_REGISTRY_PORT="$WAMN_DEV_LIVE_REGISTRY_PORT"
WAMN_ROUTE_REGISTRY_HTPASSWD="$WAMN_DEV_LIVE_HTPASSWD"
WAMN_RECEIVING_DEV_NATS_PORT="$WAMN_DEV_LIVE_NATS_PORT"
WAMN_RECEIVING_DEV_TEMPO_PORT="$WAMN_DEV_LIVE_TEMPO_PORT"
WAMN_RECEIVING_DEV_OTLP_PORT="$WAMN_DEV_LIVE_OTLP_PORT"
export WAMN_STD_VIRT_PG_PORT WAMN_STD_VIRT_REGISTRY_PORT
export WAMN_ROUTE_REGISTRY_PORT WAMN_ROUTE_REGISTRY_HTPASSWD
export WAMN_RECEIVING_DEV_NATS_PORT
export WAMN_RECEIVING_DEV_TEMPO_PORT WAMN_RECEIVING_DEV_OTLP_PORT
docker compose --profile receiving-route -p "$WAMN_DEV_LIVE_PROJECT" \
  -f "$WAMN_DEV_LIVE_COMPOSE" up --detach --wait --wait-timeout 60 \
  receiving-route-postgres authenticated-registry receiving-dev-nats receiving-dev-tempo
for _ in {1..60}; do
  curl --fail --silent "http://127.0.0.1:${WAMN_DEV_LIVE_TEMPO_PORT}/ready" \
    >/dev/null && break
  sleep 1
done
curl --fail --silent "http://127.0.0.1:${WAMN_DEV_LIVE_TEMPO_PORT}/ready" >/dev/null
PGPASSWORD=probe psql \
  "postgresql://postgres@127.0.0.1:${WAMN_DEV_LIVE_PG_PORT}/postgres" \
  -Atqc 'select 1' >/dev/null
test "$(curl --silent --output /dev/null --write-out '%{http_code}' \
  "http://${WAMN_DEV_LIVE_AUTHORITY}/v2/")" = 401

WASH_REG_USER="$WAMN_DEV_LIVE_USERNAME" \
WASH_REG_PASSWORD="$WAMN_DEV_LIVE_PASSWORD" \
  wash push "$WAMN_DEV_LIVE_FLOW_HTTP_IMAGE" "$WAMN_DEV_LIVE_FLOW_HTTP" \
    --insecure
unset WAMN_DEV_LIVE_PASSWORD

CARGO_TARGET_DIR="$WAMN_DEV_LIVE_TARGET" \
RUSTC_WRAPPER= \
WAMN_ROUTE_PG18_URL="postgresql://postgres:probe@127.0.0.1:${WAMN_DEV_LIVE_PG_PORT}/postgres" \
WAMN_RECEIVING_DEV_BIN="$WAMN_DEV_LIVE_BIN" \
WAMN_RECEIVING_DEV_HOST_BIN="$WAMN_DEV_LIVE_HOST_BIN" \
WAMN_RECEIVING_DEV_NATS_URL="nats://127.0.0.1:${WAMN_DEV_LIVE_NATS_PORT}" \
WAMN_RECEIVING_DEV_TEMPO_QUERY_URL="http://127.0.0.1:${WAMN_DEV_LIVE_TEMPO_PORT}" \
WAMN_RECEIVING_DEV_OTEL_EXPORTER_OTLP_ENDPOINT="http://127.0.0.1:${WAMN_DEV_LIVE_OTLP_PORT}" \
WAMN_RECEIVING_DEV_FLOW_HTTP_WORKLOAD_IMAGE="$WAMN_DEV_LIVE_FLOW_HTTP_IMAGE" \
WAMN_ROUTE_COMPONENT_ARTIFACT_BASE="$WAMN_DEV_LIVE_AUTHORITY/wamn/components" \
WAMN_ROUTE_RELEASE_ARTIFACT_BASE="$WAMN_DEV_LIVE_AUTHORITY/wamn/releases" \
WAMN_ROUTE_HOST=receiving.localhost \
WAMN_ROUTE_REGISTRY_AUTH_FILE="$WAMN_DEV_LIVE_DOCKER_AUTH" \
  cargo test -p wamn-proof-integration --lib --locked --offline \
  route_authentication_live::product_dev_command_owns_the_clean_twelve_stage_receipt_and_cleanup \
  -- --ignored --exact --nocapture --test-threads=1

wamn_dev_live_cleanup
trap - EXIT
```

The explicit service list keeps the anonymous std-virtualization PostgreSQL and
registry services stopped. Cleanup removes only this Compose project, its
volumes, and the validated scratch path.

### `[WAMN-DEV-ENVIRONMENT]` — the environment an operator starts the loop against

`[WAMN-DEV-LIVE]` proves the twelve-stage loop but mints its whole environment
inside the proof and throws it away with the scratch directory, so the loop was
provable and not startable (`wamn-10yt.10.30`). `wamn-dev-env` runs the same
standup module the gate runs — `tests/integration/src/dev_environment.rs`, whose
only job is to build the arguments the platform verbs take — writes the strict
`dev.json`, and then holds the authoring Gate open on a nameable port for as
long as it runs. It is not a gate: it emits no receipt, and its evidence is that
`wamn dev` starts against what it left behind.

Point it only at disposable services. Standup resets the control store, so every
run is a fresh start; never point it at shared infrastructure or the frozen
cluster. Minted PATs and credential URLs live only in the mode-0700 environment
directory and are never printed. `wamn dev` refuses to publish from a dirty
worktree, so run the loop from a clean one.

The scratch path is deliberately not under `/tmp`: a cold build of the loop is
several gigabytes, `/tmp` is a tmpfs on many machines, and the environment is
meant to outlive the session that made it. Pointing `CARGO_TARGET_DIR` at a
tmpfs is what exhausts it — the failure surfaces as `Disk quota exceeded` from
`rustc` in the middle of the build stage, which reads like a toolchain fault.

```bash
set -euo pipefail
umask 077

WAMN_DEV_ENV_TREE="$(pwd -P)"
test "$(git -C "$WAMN_DEV_ENV_TREE" rev-parse --show-toplevel)" = "$WAMN_DEV_ENV_TREE"
command -v wash >/dev/null

WAMN_DEV_ENV_HOME="${XDG_CACHE_HOME:-$HOME/.cache}"
mkdir -p "$WAMN_DEV_ENV_HOME"
WAMN_DEV_ENV_SCRATCH="$(mktemp -d "$WAMN_DEV_ENV_HOME/wamn-dev-env.XXXXXX")"
WAMN_DEV_ENV_TARGET="$WAMN_DEV_ENV_SCRATCH/target"
WAMN_DEV_ENV_DIR="$WAMN_DEV_ENV_SCRATCH/environment"
WAMN_DEV_ENV_PROJECT="wamn-dev-env-$$"
WAMN_DEV_ENV_COMPOSE="$WAMN_DEV_ENV_TREE/test-support/infrastructure/std-virtualization.compose.yaml"
WAMN_DEV_ENV_PG_PORT=54332
WAMN_DEV_ENV_REGISTRY_PORT=5004
WAMN_DEV_ENV_NATS_PORT=4224
WAMN_DEV_ENV_TEMPO_PORT=3201
WAMN_DEV_ENV_OTLP_PORT=4319
WAMN_DEV_ENV_AUTHORITY="127.0.0.1:${WAMN_DEV_ENV_REGISTRY_PORT}"
WAMN_DEV_ENV_USERNAME=wamn-dev-env
WAMN_DEV_ENV_PASSWORD="$(openssl rand -hex 32)"
WAMN_DEV_ENV_HTPASSWD="$WAMN_DEV_ENV_SCRATCH/htpasswd"
WAMN_DEV_ENV_DOCKER_AUTH="$WAMN_DEV_ENV_SCRATCH/.dockerconfigjson"
WAMN_DEV_ENV_FLOW_HTTP_IMAGE="$WAMN_DEV_ENV_AUTHORITY/wamn/flow-http:dev"

RUSTC_WRAPPER= CARGO_TARGET_DIR="$WAMN_DEV_ENV_TARGET" \
  cargo build -p wamn-ctl --bin wamn --locked --offline
RUSTC_WRAPPER= CARGO_TARGET_DIR="$WAMN_DEV_ENV_TARGET" \
  cargo build -p wamn-host --locked --offline
RUSTC_WRAPPER= CARGO_TARGET_DIR="$WAMN_DEV_ENV_TARGET" \
  cargo build -p wamn-proof-integration --bin wamn-dev-env --locked --offline
RUSTC_WRAPPER= CARGO_TARGET_DIR="$WAMN_DEV_ENV_TARGET" \
  cargo build --manifest-path "$WAMN_DEV_ENV_TREE/components/Cargo.toml" \
    -p http-route --target wasm32-wasip2 --locked --offline
WAMN_DEV_ENV_FLOW_HTTP="$WAMN_DEV_ENV_TARGET/wasm32-wasip2/debug/http_route.wasm"
test -x "$WAMN_DEV_ENV_TARGET/debug/wamn"
test -x "$WAMN_DEV_ENV_TARGET/debug/wamn-host"
test -x "$WAMN_DEV_ENV_TARGET/debug/wamn-dev-env"
test -s "$WAMN_DEV_ENV_FLOW_HTTP"

printf '%s\n' "$WAMN_DEV_ENV_PASSWORD" \
  | docker run --rm -i --entrypoint htpasswd httpd:2-alpine \
      -Bni "$WAMN_DEV_ENV_USERNAME" >"$WAMN_DEV_ENV_HTPASSWD"
jq -n --arg authority "$WAMN_DEV_ENV_AUTHORITY" \
  --arg username "$WAMN_DEV_ENV_USERNAME" \
  --arg password "$WAMN_DEV_ENV_PASSWORD" \
  '{auths:{($authority):{username:$username,password:$password}}}' \
  >"$WAMN_DEV_ENV_DOCKER_AUTH"

WAMN_STD_VIRT_PG_PORT="$WAMN_DEV_ENV_PG_PORT"
WAMN_STD_VIRT_REGISTRY_PORT=5003
WAMN_ROUTE_REGISTRY_PORT="$WAMN_DEV_ENV_REGISTRY_PORT"
WAMN_ROUTE_REGISTRY_HTPASSWD="$WAMN_DEV_ENV_HTPASSWD"
WAMN_RECEIVING_DEV_NATS_PORT="$WAMN_DEV_ENV_NATS_PORT"
WAMN_RECEIVING_DEV_TEMPO_PORT="$WAMN_DEV_ENV_TEMPO_PORT"
WAMN_RECEIVING_DEV_OTLP_PORT="$WAMN_DEV_ENV_OTLP_PORT"
export WAMN_STD_VIRT_PG_PORT WAMN_STD_VIRT_REGISTRY_PORT
export WAMN_ROUTE_REGISTRY_PORT WAMN_ROUTE_REGISTRY_HTPASSWD
export WAMN_RECEIVING_DEV_NATS_PORT
export WAMN_RECEIVING_DEV_TEMPO_PORT WAMN_RECEIVING_DEV_OTLP_PORT
docker compose --profile receiving-route -p "$WAMN_DEV_ENV_PROJECT" \
  -f "$WAMN_DEV_ENV_COMPOSE" up --detach --wait --wait-timeout 60 \
  receiving-route-postgres authenticated-registry receiving-dev-nats receiving-dev-tempo
for _ in {1..60}; do
  curl --fail --silent "http://127.0.0.1:${WAMN_DEV_ENV_TEMPO_PORT}/ready" \
    >/dev/null && break
  sleep 1
done
curl --fail --silent "http://127.0.0.1:${WAMN_DEV_ENV_TEMPO_PORT}/ready" >/dev/null
PGPASSWORD=probe psql \
  "postgresql://postgres@127.0.0.1:${WAMN_DEV_ENV_PG_PORT}/postgres" \
  -Atqc 'select 1' >/dev/null

WASH_REG_USER="$WAMN_DEV_ENV_USERNAME" \
WASH_REG_PASSWORD="$WAMN_DEV_ENV_PASSWORD" \
  wash push "$WAMN_DEV_ENV_FLOW_HTTP_IMAGE" "$WAMN_DEV_ENV_FLOW_HTTP" --insecure
unset WAMN_DEV_ENV_PASSWORD

"$WAMN_DEV_ENV_TARGET/debug/wamn-dev-env" \
  --system-database-url "postgresql://postgres:probe@127.0.0.1:${WAMN_DEV_ENV_PG_PORT}/postgres" \
  --root "$WAMN_DEV_ENV_DIR" \
  --nats-url "nats://127.0.0.1:${WAMN_DEV_ENV_NATS_PORT}" \
  --tempo-query-url "http://127.0.0.1:${WAMN_DEV_ENV_TEMPO_PORT}" \
  --otel-exporter-otlp-endpoint "http://127.0.0.1:${WAMN_DEV_ENV_OTLP_PORT}" \
  --component-artifact-base "$WAMN_DEV_ENV_AUTHORITY/wamn/components" \
  --release-artifact-base "$WAMN_DEV_ENV_AUTHORITY/wamn/releases" \
  --registry-auth-file "$WAMN_DEV_ENV_DOCKER_AUTH" \
  --route-host receiving.localhost \
  --flow-http-workload-image "$WAMN_DEV_ENV_FLOW_HTTP_IMAGE" \
  --host-binary "$WAMN_DEV_ENV_TARGET/debug/wamn-host" \
  --package "$WAMN_DEV_ENV_TREE/packages/receiving" \
  --overlay-root "$WAMN_DEV_ENV_TREE/packages/client_acme_receiving"
```

The command prints the Gate URL, the configuration path, and the exact `wamn
dev` line to run. Leave it running and start the loop from the repository root
in a second terminal:

```bash
"$WAMN_DEV_ENV_TARGET/debug/wamn" dev --config "$WAMN_DEV_ENV_DIR/dev.json" \
  --overlay-root "$WAMN_DEV_ENV_TREE/packages/client_acme_receiving" --tui
```

Stop the Gate with Ctrl-C when the loop is done, then remove the services and
the scratch path:

```bash
docker compose --profile receiving-route -p "$WAMN_DEV_ENV_PROJECT" \
  -f "$WAMN_DEV_ENV_COMPOSE" down --volumes --remove-orphans
if [[ "$WAMN_DEV_ENV_SCRATCH" == "$WAMN_DEV_ENV_HOME"/wamn-dev-env.* ]]; then
  rm -rf -- "$WAMN_DEV_ENV_SCRATCH"
fi
```

### `[GUEST-DIGEST-REPRODUCIBILITY]` — one commit, two checkouts, one digest

A component digest must be a function of the bytes an author wrote. It was not:
the same commit produced a different digest in every worktree, so a pin minted
in one checkout was unreproducible in all the others and `[WAMN-DEV-LIVE]` could
only pass from the directory the pin happened to be minted in
(`wamn-10yt.10.29`). Two channels caused it, and both are closed —
`-C metadata` derived from the absolute path of a path dependency that escaped
the guest workspace, fixed by relocating those crates under `components/`; and
absolute `file!()` strings baked in by `include!`d package sources, fixed by
`--remap-path-prefix` in `tools/build-components`.

`tests/conformance/tests/guest_workspace_closure.rs` asserts both properties
structurally on every run. This gate proves the property they exist to protect.
It builds one commit in two worktrees and compares every virtualized artifact.

```bash
set -euo pipefail
GUEST_REPRO_COMMIT="$(git rev-parse HEAD)"
GUEST_REPRO_A="$(mktemp -d /tmp/wamn-guest-repro-a.XXXXXX)"
GUEST_REPRO_B="$(mktemp -d /tmp/wamn-guest-repro-b.XXXXXX)"
git worktree add --detach "$GUEST_REPRO_A/tree" "$GUEST_REPRO_COMMIT"
git worktree add --detach "$GUEST_REPRO_B/tree" "$GUEST_REPRO_COMMIT"
for side in "$GUEST_REPRO_A" "$GUEST_REPRO_B"; do
  ( cd "$side/tree" \
    && CARGO_TARGET_DIR="$side/target" RUSTC_WRAPPER= \
       ./tools/build-components build-only m1 > "$side/plan.json" \
    && CARGO_TARGET_DIR="$side/target" RUSTC_WRAPPER= \
       ./tools/build-components virtualize-only "$side/plan.json" >/dev/null )
done
WAMN_DIGEST_REPRO_A="$GUEST_REPRO_A/target/virtualized/std-empty-environment" \
WAMN_DIGEST_REPRO_B="$GUEST_REPRO_B/target/virtualized/std-empty-environment" \
  cargo test -p wamn-proof-conformance --test guest_workspace_closure \
  one_commit_built_in_two_checkouts_yields_identical_guest_digests \
  -- --ignored --exact --nocapture
git worktree remove --force "$GUEST_REPRO_A/tree"
git worktree remove --force "$GUEST_REPRO_B/tree"
rm -rf -- "$GUEST_REPRO_A" "$GUEST_REPRO_B"
```

Run it whenever a guest workspace gains a member or a build flag changes. A
failure means a component digest has started depending on the build directory
again, and every pin minted since is a claim about a checkout.

### `[RECEIVING-ROUTE-JOURNEY]` — published base + overlay routes and traces

This gate builds the virtualized Receiving base and Acme overlay components
plus shipped `flow-http`, then drives production apply, installed-set ACL
reconciliation, push, fourteen wiring gates/authorships, one exact two-package
release mint/attestation/load, thirteen PAT routes, and PostgreSQL effects. It
asserts the overlay registration's exact owner/source/entity/operation set and
proves the overlay `record_receipt` span invokes its pinned base digest with the
same originating principal. That nested route is the first package invocation
against an empty disposable Wasmtime cache; the gate proves both exact
components are pulled and compiled before the parent execution clock, then
fresh-linked and instantiated. The deferred CDC/materializer leg is not part
of this route journey. Its PostgreSQL 18 server and authenticated plain-HTTP
registry are disposable and loopback-only. The Docker auth document must carry
explicit non-empty `username` and `password` fields for the exact registry
authority; credentials exist only in the scratch directory and are not printed.

The focused real-boundary guard keeps nested authorization on the same refusal
grammar as a direct invocation:

```bash
cargo test -p wamn-execution-host --lib --locked --offline \
  router_driver::tests::nested_permission_denial_survives_the_real_component_boundary \
  -- --exact
```

```bash
set -euo pipefail
umask 077

RECEIVING_ROUTE_ROOT="$(pwd -P)"
RECEIVING_ROUTE_SCRATCH="$(mktemp -d /tmp/wamn-receiving-route.XXXXXX)"
RECEIVING_ROUTE_PROJECT="wamn-receiving-route-$$"
RECEIVING_ROUTE_COMPOSE="$RECEIVING_ROUTE_ROOT/test-support/infrastructure/std-virtualization.compose.yaml"
RECEIVING_ROUTE_PG_PORT=54332
RECEIVING_ROUTE_REGISTRY_PORT=5004
RECEIVING_ROUTE_AUTHORITY="127.0.0.1:${RECEIVING_ROUTE_REGISTRY_PORT}"
RECEIVING_ROUTE_USERNAME=wamn-receiving-route
RECEIVING_ROUTE_PASSWORD="$(openssl rand -hex 32)"
RECEIVING_ROUTE_HTPASSWD="$RECEIVING_ROUTE_SCRATCH/htpasswd"
RECEIVING_ROUTE_DOCKER_AUTH="$RECEIVING_ROUTE_SCRATCH/.dockerconfigjson"
RECEIVING_ROUTE_CURL_AUTH="$RECEIVING_ROUTE_SCRATCH/curl.conf"
RECEIVING_ROUTE_HOST=receiving.localhost
RECEIVING_ROUTE_SECRET_OUTPUT_DIRECTORY="$RECEIVING_ROUTE_SCRATCH/host-secrets"
RECEIVING_ROUTE_CALLER_SECRET_OUTPUT="$RECEIVING_ROUTE_SCRATCH/route-caller-pat.json"
RECEIVING_ROUTE_COMPILATION_CACHE_DIRECTORY="$RECEIVING_ROUTE_SCRATCH/wasmtime-cache"
RECEIVING_ROUTE_SECRET_NAMESPACE=wamn-receiving-route
install -d -m 0700 "$RECEIVING_ROUTE_SECRET_OUTPUT_DIRECTORY" \
  "$RECEIVING_ROUTE_COMPILATION_CACHE_DIRECTORY"

receiving_route_cleanup() {
  docker compose --profile receiving-route -p "$RECEIVING_ROUTE_PROJECT" \
    -f "$RECEIVING_ROUTE_COMPOSE" down --volumes --remove-orphans \
    >/dev/null 2>&1 || true
  if [[ "$RECEIVING_ROUTE_SCRATCH" == /tmp/wamn-receiving-route.* ]]; then
    rm -rf -- "$RECEIVING_ROUTE_SCRATCH"
  fi
}
trap receiving_route_cleanup EXIT

CARGO_TARGET_DIR="$RECEIVING_ROUTE_SCRATCH/target" \
  "$RECEIVING_ROUTE_ROOT/tools/build-components" m1
RECEIVING_ROUTE_COMPONENTS="$RECEIVING_ROUTE_SCRATCH/target/virtualized/std-empty-environment"
RECEIVING_ROUTE_FLOW_HTTP="$RECEIVING_ROUTE_SCRATCH/target/wasm32-wasip2/debug/http_route.wasm"
test -s "$RECEIVING_ROUTE_COMPONENTS/receiving.wasm"
test -s "$RECEIVING_ROUTE_COMPONENTS/client_acme_receiving.wasm"
test -s "$RECEIVING_ROUTE_FLOW_HTTP"

printf '%s\n' "$RECEIVING_ROUTE_PASSWORD" \
  | docker run --rm -i --entrypoint htpasswd httpd:2-alpine \
      -Bni "$RECEIVING_ROUTE_USERNAME" >"$RECEIVING_ROUTE_HTPASSWD"
jq -n --arg authority "$RECEIVING_ROUTE_AUTHORITY" \
  --arg username "$RECEIVING_ROUTE_USERNAME" \
  --arg password "$RECEIVING_ROUTE_PASSWORD" \
  '{auths:{($authority):{username:$username,password:$password}}}' \
  >"$RECEIVING_ROUTE_DOCKER_AUTH"
printf 'user = "%s:%s"\n' \
  "$RECEIVING_ROUTE_USERNAME" "$RECEIVING_ROUTE_PASSWORD" \
  >"$RECEIVING_ROUTE_CURL_AUTH"
unset RECEIVING_ROUTE_PASSWORD
jq -e --arg authority "$RECEIVING_ROUTE_AUTHORITY" '
  .auths[$authority]
  | (.username | type == "string" and length > 0)
    and (.password | type == "string" and length > 0)
' "$RECEIVING_ROUTE_DOCKER_AUTH" >/dev/null

WAMN_STD_VIRT_PG_PORT="$RECEIVING_ROUTE_PG_PORT"
WAMN_STD_VIRT_REGISTRY_PORT=5003
WAMN_ROUTE_REGISTRY_PORT="$RECEIVING_ROUTE_REGISTRY_PORT"
WAMN_ROUTE_REGISTRY_HTPASSWD="$RECEIVING_ROUTE_HTPASSWD"
export WAMN_STD_VIRT_PG_PORT WAMN_STD_VIRT_REGISTRY_PORT
export WAMN_ROUTE_REGISTRY_PORT WAMN_ROUTE_REGISTRY_HTPASSWD
docker compose --profile receiving-route -p "$RECEIVING_ROUTE_PROJECT" \
  -f "$RECEIVING_ROUTE_COMPOSE" up --detach --wait --wait-timeout 60 \
  receiving-route-postgres authenticated-registry
PGPASSWORD=probe psql \
  "postgresql://postgres@127.0.0.1:${RECEIVING_ROUTE_PG_PORT}/postgres" \
  -Atqc 'select 1' >/dev/null
test "$(curl --silent --output /dev/null --write-out '%{http_code}' \
  "http://${RECEIVING_ROUTE_AUTHORITY}/v2/")" = 401
test "$(curl --config "$RECEIVING_ROUTE_CURL_AUTH" --silent --show-error \
  --output /dev/null --write-out '%{http_code}' \
  "http://${RECEIVING_ROUTE_AUTHORITY}/v2/")" = 200

WAMN_ROUTE_PG18_URL="postgresql://postgres:probe@127.0.0.1:${RECEIVING_ROUTE_PG_PORT}/postgres" \
WAMN_ROUTE_COMPONENT_DIRECTORY="$RECEIVING_ROUTE_COMPONENTS" \
WAMN_ROUTE_COMPILATION_CACHE_DIRECTORY="$RECEIVING_ROUTE_COMPILATION_CACHE_DIRECTORY" \
WAMN_ROUTE_FLOW_HTTP_WASM="$RECEIVING_ROUTE_FLOW_HTTP" \
WAMN_ROUTE_COMPONENT_ARTIFACT_BASE="$RECEIVING_ROUTE_AUTHORITY/wamn/components" \
WAMN_ROUTE_RELEASE_ARTIFACT_BASE="$RECEIVING_ROUTE_AUTHORITY/wamn/releases" \
WAMN_ROUTE_HOST="$RECEIVING_ROUTE_HOST" \
WAMN_ROUTE_REGISTRY_AUTH_FILE="$RECEIVING_ROUTE_DOCKER_AUTH" \
WAMN_ROUTE_SECRET_OUTPUT_DIRECTORY="$RECEIVING_ROUTE_SECRET_OUTPUT_DIRECTORY" \
WAMN_ROUTE_CALLER_SECRET_OUTPUT="$RECEIVING_ROUTE_CALLER_SECRET_OUTPUT" \
WAMN_ROUTE_SECRET_NAMESPACE="$RECEIVING_ROUTE_SECRET_NAMESPACE" \
  cargo test -p wamn-proof-integration --lib --locked --offline \
  route_authentication_live::production_two_package_release_serves_all_thirteen_pat_routes \
  -- --ignored --exact --nocapture --test-threads=1

receiving_route_cleanup
trap - EXIT
```

The route-specific PostgreSQL service applies the production
`deploy/sql/postgres-init.sql` bootstrap on first start, including the stable
NOLOGIN role floor that `reconcile-run-plane` verifies rather than creates. The
explicit service list leaves the anonymous std-virtualization PostgreSQL and
registry services unchanged and stopped. Cleanup removes only this Compose
project, its volumes, and its validated scratch path. Never substitute shared
infrastructure or the frozen cluster.

### `[RECEIVING-MATERIALIZER-JOURNEY]` — router-era causation and materialization

This is the D19 primary gate and the production-fed source for
`H5-CAUSATION` and `H5-CAUSATION-E2E`. The disposable cluster runner starts the
production CDC reader, schedules the released EventMaterializer, and issues a
real receipt through the published Receiving route. It proves the receipt
event carries route-derived root causation, the handler-created inspection
preserves that root while advancing depth, and exactly one pending inspection
is committed. The durable must settle with no pending or redelivered messages
and no dead letter. The materializer invocation trace must name the released
overlay operation and digest, include its PostgreSQL effect, and carry no
caller identity: post-commit causation is provenance, not identity.

```bash
tools/receiving-cluster-journey-run --apply \
  --evidence-dir /tmp/wamn-receiving-materializer-evidence
```

This run re-proves the materializer journey, native scheduling, and exact
cleanup. It cites the immutable RC bootstrap, M2-supersession, socketguard,
traceproof, and scoped RegistryReader receipts; it does not rerun those
unchanged mechanisms.

### `[RECEIVING-CLUSTER-JOURNEY]` — released flow-http scheduling and reachability

The same runner wraps the production Receiving route and materializer journeys
above, then installs the pinned operator 2.8.0 release before the pinned host
2.8.0 release on its own three-node kind cluster. It proves three Ready
default-group Hosts, native Workload scheduling, the operator-managed
EndpointSlice, the exact typed `route-not-found` response through
`receiving.localhost`, and the native `CrossEnvironmentSchedulingDenied`
refusal. The runner applies the WAMN-owned per-environment modern-Event Role
and RoleBinding, proves the operator's actual ServiceAccount has exactly
`create,patch` on `events.k8s.io/events`, and records both the durable condition
and matching native Warning Event. The 404 proves only HTTP routing and guest
execution; the Kubernetes objects independently prove the other arms.

```bash
tools/receiving-cluster-journey-run --apply \
  --evidence-dir /tmp/wamn-receiving-cluster-evidence
```

`wamn-10yt.8` measures the same published release under runtime-operator
2.8.0's unchanged TCP liveness and readiness probes. It records cold runtime
startup and a cache-seeding authenticated request, restarts the container in
that same Pod, then records the restart-first and immediate steady-state
requests. A disposable Tempo/OTel pair receives the real request traces; the
receipts split authentication, resolution, artifact pull, compilation, linking,
instantiation, SQL, and the ExecutorPlatform, CallableHttp, and GuestSql
connection acquisitions. The gate also proves the compiled cache remained byte-
and inode-identical. Listener readiness is not evidence that the full release
closure is resident; exact released wirings resolve on demand.

```bash
tools/receiving-cluster-journey-run --apply --measure-startup \
  --evidence-dir /tmp/wamn-receiving-startup-evidence
```

The helper refuses a dirty source tree or a pre-existing scratch cluster. Every
Kubernetes and Helm command names its private kubeconfig/context; it never
addresses the frozen `kind-wamn` cluster. PostgreSQL, authenticated OCI,
cluster, port-forward, and the uniquely tagged debug host image are exact-owned
scratch resources. Cleanup absence and a SHA-256 evidence inventory are part of
the passing verdict.

### Other live gates that carry their command in-source

These have no section tag; the file's own doc comment is the recipe of record.

| test | variable | command |
| --- | --- | --- |
| `crates/catalog/model/tests/wiring_activation_live.rs` | `WAMN_CATALOG_PG_URL` | `cargo test -p wamn-catalog --test wiring_activation_live -- --ignored` |
| `crates/schema/introspection/tests/postgres_live.rs` | `WAMN_SCHEMA_INTROSPECTION_PG_URL` | `cargo test -p wamn-schema-introspection --test postgres_live -- --include-ignored --nocapture --test-threads=1` |
| `crates/platform/runtime/tests/wiring_doorbell_live.rs` | `WAMN_CATALOG_PG_URL` | `cargo test -p wamn-runtime --test wiring_doorbell_live -- --ignored` |
| `crates/platform/runtime/tests/executor_platform_surface_live.rs` | `WAMN_EXEC_PLATFORM_PG_URL` | `cargo test -p wamn-runtime --test executor_platform_surface_live -- --include-ignored` |
| `crates/platform/runtime/tests/sqlx_transaction_live.rs` | `WAMN_SQLX_TRANSACTION_PG_URL` **and** `WAMN_SQLX_TRANSACTION_COMPONENT` | `cargo test -p wamn-runtime --test sqlx_transaction_live -- --include-ignored` |
| `services/ctl/tests/effect_writer_generation_live.rs` | `WAMN_EFFECT_WRITER_PG18_URL` | `cargo test -p wamn-ctl --test effect_writer_generation_live -- --ignored --nocapture` |
| `services/ctl/tests/guest_generation_live.rs` | `WAMN_GUEST_GENERATION_PG18_URL` | `cargo test -p wamn-ctl --features ops --test guest_generation_live -- --ignored --nocapture` |
| `services/ctl/tests/management_admitter_generation_live.rs` | `WAMN_MANAGEMENT_ADMITTER_PG18_URL` | `cargo test -p wamn-ctl --test management_admitter_generation_live -- --ignored --nocapture` |
| `services/ctl/tests/terminalize_effect_uncertain_live.rs` | `WAMN_OPERATOR_TERMINALIZE_PG18_URL` | `cargo test -p wamn-ctl --test terminalize_effect_uncertain_live` |
| `services/ctl/tests/apply_package_live.rs` | `WAMN_CTL_PG_URL` | `cargo test -p wamn-ctl --test apply_package_live` |
| `services/ctl/tests/protected_relations_live.rs` | `WAMN_CTL_PG_URL` | `cargo test -p wamn-ctl --features ops --test protected_relations_live -- --ignored` |
| `services/ctl/tests/author_wiring_gate_report_live.rs` | `WAMN_AUTHOR_WIRING_PROJECT_PG_URL` **and** `WAMN_AUTHOR_WIRING_CONTROL_PG_URL` | `cargo test -p wamn-ctl --test author_wiring_gate_report_live -- --ignored` |
| `tests/integration/src/route_authentication_live.rs` | `WAMN_ROUTE_AUTH_PG18_URL` | `cargo test -p wamn-proof-integration --lib route_authentication_live::production_route_caller_authentication_and_operation_authorization -- --ignored --exact --nocapture --test-threads=1` |
| `services/scenario-worker/tests/management_live.rs` | `WAMN_PLATFORM_IDENTITY_PG_URL` | `cargo test -p wamn-scenario-worker --test management_live` |
| `crates/execution/run-state/tests/effect_writer_live.rs` | `WAMN_RUN_STORE_PG_URL` | `cargo test -p wamn-run-state --features native --test effect_writer_live -- --ignored` |
| `crates/execution/run-state/tests/run_state_live.rs` | `WAMN_RUN_STORE_PG_URL` | `cargo test -p wamn-run-state --test run_state_live -- --include-ignored` |
| `services/dispatcher/tests/read_authority.rs` | `WAMN_PROVISION_PG_URL` | `cargo test -p wamn-dispatcher --test read_authority` |
| `crates/control/provision/tests/control_portable_store.rs` | `WAMN_CONTROL_PORTABLE_PG_URL` | `cargo test -p wamn-control-provision --test control_portable_store -- --include-ignored --test-threads=1` |
| `crates/control/provision/tests/cdc.rs` | `WAMN_CDC_PG_URL` | `cargo test -p wamn-control-provision --test cdc` |
| `crates/control/provision/tests/control_storage.rs` | `WAMN_REGISTRY_PG_URL` | `cargo test -p wamn-control-provision --test control_storage` |
| `crates/control/provision/tests/ops_storage.rs` | `WAMN_REGISTRY_PG_URL` | `cargo test -p wamn-control-provision --features ops --test ops_storage` |
| `crates/control/provision/tests/system_reader_grants.rs` | `WAMN_REGISTRY_PG_URL` | `cargo test -p wamn-control-provision --test system_reader_grants` |
| `crates/control/provision/tests/provision.rs` | `WAMN_PROVISION_PG_URL` | `cargo test -p wamn-control-provision --test provision` |
| `crates/control/provision/tests/database_owner.rs` | `WAMN_PROVISION_PG_URL` | `cargo test -p wamn-control-provision --test database_owner` |
| `crates/control/provision/tests/dump.rs` | `WAMN_DUMP_PG_URL` | `cargo test -p wamn-control-provision --features ops --test dump` |
| `crates/control/provision/tests/restore.rs` | `WAMN_RESTORE_PG_URL` | `cargo test -p wamn-control-provision --features ops --test restore` |
| `crates/control/provision/tests/family_surface_grants.rs` | `WAMN_FAMILY_SURFACE_PG_URL` | `cargo test -p wamn-control-provision --test family_surface_grants` |
| `crates/control/provision/tests/family_denial_matrix.rs` | `WAMN_DENIAL_MATRIX_PG_URL` | `cargo test -p wamn-control-provision --test family_denial_matrix -- --test-threads=1` |
| `crates/control/provision/tests/operation_grants.rs` | `WAMN_OPERATION_GRANTS_PG18_URL` | `cargo test -p wamn-control-provision --test operation_grants` |
| `crates/execution/run-state/tests/store.rs` | `WAMN_RUN_STORE_PG_URL` | `cargo test -p wamn-run-state --test store` |
| `crates/identity/platform/tests/identity_live.rs` | `WAMN_PLATFORM_IDENTITY_PG_URL` | `cargo test -p wamn-platform-identity --test identity_live` |
| `crates/identity/platform/tests/pat_live.rs` | `WAMN_PLATFORM_IDENTITY_PG_URL` | `cargo test -p wamn-platform-identity --test pat_live` |
| `crates/identity/project-state/tests/authority.rs` | `WAMN_SYSSCHEMA_PG_URL` | `cargo test -p wamn-project-state --test authority` |
| `crates/identity/project-state/tests/schema.rs` | `WAMN_SYSSCHEMA_PG_URL` | `cargo test -p wamn-project-state --test schema` |
| `crates/platform/runtime/tests/production_claim_live.rs` | `WAMN_PRODUCTION_CLAIM_PG_URL` | `cargo test -p wamn-runtime --test production_claim_live -- --include-ignored` |
| `crates/platform/runtime/tests/production_claim_durable_live.rs` | `WAMN_DURABLE_TIER_PG_URL` | `cargo test -p wamn-runtime --test production_claim_durable_live -- --include-ignored` |
| `crates/platform/runtime/tests/release_manifest_source.rs` | `WAMN_RELEASE_MANIFEST_ARTIFACT_BASE` **and** `WAMN_REGISTRY_AUTH_FILE` | `cargo test -p wamn-runtime --test release_manifest_source -- --include-ignored` |
| `crates/platform/runtime/src/plugins/wamn_postgres/claims.rs` | `WAMN_POOL_LIFECYCLE_PG_URL` | `cargo test -p wamn-runtime --all-features --lib live_size_one_guest_and_platform_pools -- --include-ignored` |
| `services/ctl/tests/dispatch_reader_provisioning_live.rs` | `WAMN_CTL_PG_URL` | `cargo test -p wamn-ctl --test dispatch_reader_provisioning_live` |

**The rows from `cdc.rs` down were added by `wamn-0h0g.15.137.2`, which means
those gates had never entered an arming set.** Each was armed once, alone, on
its own fresh `postgres:18` container. The surviving measured reds are:

| gate | measured | first failing test and reason |
| --- | --- | --- |
| `run-state/tests/store.rs` | 7 passed / 1 failed | `run_state_schema_applies_and_isolates_on_postgres` — `ERROR: role "wamn_effect_writer" does not exist`; the bootstrap mints only `wamn_app`, and `run-state.sql` GRANTs to three roles |
| `project-state/tests/authority.rs` | 0 passed / 3 failed | all three, e.g. `a_project_still_owns_its_own_configuration` — `ERROR: new row violates row-level security policy for table "configurations"` |
| `project-state/tests/schema.rs` | 4 passed / 1 failed | `app_schema_applies_and_enforces_isolation_and_claims_on_postgres` — `ERROR: t1 sees its 2 users, not t2's` |
| `runtime/tests/production_claim_live.rs` | 1 passed / 1 failed | `production_claim_live` — `effect-writer credential role does not match its scoped generation` |
| `runtime/tests/production_claim_durable_live.rs` | 0 passed / 1 failed | `production_claim_durable_live` — same credential refusal |
| `runtime .../wamn_postgres/claims.rs` | 0 passed / 1 failed | `live_size_one_guest_and_platform_pools_isolate_sessions_under_interleaving` — `platform headroom remains available while guest is saturated: PgError::ConnectionUnavailable` |

The surviving green gates, armed and measured: `cdc` 2 passed 0.78s (**start the
container with `-c wal_level=logical`** or it false-reds), `control_storage` 11
passed, `system_reader_grants` 2 passed, `provision` 1 passed, `database_owner`
1 passed, `dump` 1 passed, `restore` 1 passed, `ops_storage` 6 passed,
`identity_live` 1 passed, `pat_live` 1 passed, and
`dispatch_reader_provisioning_live` 1 passed. Unarmed, every one of them printed
a `skipping …` line and still reported `ok` in `0.00s`; the duration is the only
honest signal.

`crates/platform/runtime/tests/release_manifest_source.rs` could not be armed:
its one ignored leg needs a live authenticated OCI registry holding a published
release, and the in-tree push path went with `builder` and `node-host` at
`f6bc01eb`. Its row records the variables so the gate is at least addressable.

`crates/control/provision/tests/provision.rs` and
`crates/control/provision/tests/database_owner.rs` are the only pair here whose
source states they may share one container; everything else in this table needs
its own.

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

`crates/control/provision/tests/control_portable_store.rs` is mixed the same way:
one `#[ignore]` gate and the rest self-skipping, which is why its row carries
`--include-ignored` rather than `--ignored`. Every gate in it applies the
artifact to the SAME database and resets the control schemas first, so
`--test-threads=1` is not optional. The binary proves the current package and
effective-release record, immutable coordinate conflicts, exact control-author
tenant authority, and the Rust deployment-attestation binding. Run the whole
binary with

```bash
WAMN_CONTROL_PORTABLE_PG_URL=postgresql://postgres:pw@127.0.0.1:PORT/postgres \
  cargo test -p wamn-control-provision --test control_portable_store -- \
  --include-ignored --test-threads=1
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

**Never run a command that operates on "everything currently present."** Two
agents share this repository, and several git commands act on ambient state
rather than on what you name. Each of these has cost real work in one session:

- **`git stash` / `git stash pop` are BANNED.** The stash list is ONE SHARED
  STACK for the whole repository — it lives in `.git/refs/stash`, not in a
  worktree. A clean tree makes `stash` save nothing, and the following `pop`
  then takes *another agent's* entry into your worktree. That happened: it
  consumed the parked `.5.1` TypeScript work, which survived only because it
  was noticed and diffed out before anything else touched the tree. To compare
  against a baseline, add a THROWAWAY WORKTREE at the base commit
  (`git worktree add --detach /tmp/<name> <ref>`) and measure there. If parked
  work must be kept, keep it on a lane ref: a stash has no owner and no name,
  and the next bare `pop` by anyone takes whatever is on top.
- **`git add -A` is BANNED.** It stages every modified file in the tree,
  including another agent's in-flight edits and any workspace-wide `cargo fmt`
  churn. Twice in one session it swept fifteen unrelated files into a feature
  commit. Name the paths you mean.
- **`git checkout <file>` on UNCOMMITTED work is BANNED.** It is the usual way
  to revert a mutant, and it silently discards everything else uncommitted in
  that file. Commit before mutation testing, then `git checkout` restores the
  commit rather than deleting the work.
  **And git cannot restore what it does not TRACK.** A mutant applied to a new,
  untracked file leaves `git checkout` failing with a pathspec error — which
  looks like a command that did nothing, because it did, while the mutation is
  still in place and the next test run measures the mutant. Restoring an
  untracked file is written by hand, or the file is committed first. Measured
  on the WMS aggregate: `WHERE true` survived a "restore" and only the explicit
  re-read caught it.

The common shape: a command whose subject is "the current state" rather than
an argument you wrote. In a single-agent repository these are conveniences; in
a shared one they are writes to somebody else's data.

**Never share a `CARGO_TARGET_DIR` between parallel worktrees.** Three
measured failure modes: `env!("CARGO_MANIFEST_DIR")` resolves to *another*
worktree, so a test validates the wrong tree; artifact collision overwrites in
place, so a later run executes the other tree's code; and fingerprint thrash
serialises every lane on one flock. Give each worktree its own directory.

**A guest fixture must PROVE its import survives.** An interface a guest never
calls is elided by the component encoder, so the fixture silently stops
carrying the surface its test names. This has now bitten three times: the
`wasi:sockets` admission fixture asserted the egress guard while importing no
socket at all; a blobstore probe componentized with no blobstore import; and a
second probe repeated it. **Standing law: every new guest fixture makes a real
call through the interface, and the test then verifies the import is present**
— by `wasm-tools component wit` on the artifact, or by asserting the refusal
names the import it caught. A fixture that only *declares* an import proves
nothing about it.

**Score a mutant by the proof's EXIT CODE, never by counting failure lines.**
A mutant that makes the harness crash before it reaches its assertions prints
no failures, so a line-counting score reads it as green — a survivor disguised
as a kill, and the disguise is best exactly when the mutation is most
damaging. This is not hypothetical: a first mutation pass over a lifted render
harness scored `FAIL` lines and reported all four mutants killed. Rescored on
exit code, two had survived, and both were properties the harness's own
contract claimed — an anchor rule weakened from exactly-once to at-least-once,
and a required-key check disabled. Neither was tested by anything.

**A mutant scores killed only when the proof ran to completion and failed on
the mutated property.** Both halves matter. Completion rules out the crash
disguise; failing on the mutated property rules out a kill for an unrelated
reason, which proves the mutant is detectable but not that the assertion you
care about detects it.

**And a mutant must prove it LANDED before its survival means anything.** A
substitution that matched nothing changes no code, so the proof passes and the
result reads as a survivor — identical output, opposite meaning. The same is
true of a substitution that matched more times than intended, which mutates
somewhere you were not looking. So every scripted mutation asserts its match
count before applying, and reports a zero-match apply as VOID rather than as a
result.

The form matters for anything carrying regex metacharacters. A `sed` pattern
for an `awk` program full of `/`, `[`, `\` and `{` failed to compile and
reported no match; re-applied as an exact-string substitution with an asserted
count, the same mutant was killed immediately. Prefer exact-string replacement
with a counted match over a regex whenever the target contains metacharacters
— the regex is one more thing that can be wrong in the direction that looks
like success.

**Every guard's proof carries a negative control — a known-bad input that MUST
fail it.** A check with no such input has not been shown to check anything.
The nameref binding check above passed against a function that had been
deliberately regressed to the exact defect it existed to catch, because the
check itself was malformed; only adding the control — the internal parameter
name, which must collide — revealed it. Run the control every time, not once
when the check is written: a check can go inert later, when the code it probes
moves out from under it.

This is the same failure as asserting a count that can coincide, one level up.
There, the assertion passed for the wrong reason; here, the whole check does.

**And the control has to ISOLATE, or it proves the neighbour.** Overlapping
guards are defence in depth in production and a measurement hazard in test: if
a known-bad input is refused by two guards, the refusal says nothing about
which one fired, so deleting the guard under test leaves the proof green. That
is a survivor produced by a control that looked exactly right.

Measured instance. A shared function refused an empty role-family declaration
by its own explicit check — and *also*, incidentally, by a later declared-set
comparison, because an empty list built a garbage `.json` expectation that
then failed to match. Deleting the explicit check changed no outcome. A first
attempt to isolate it, by seeding a directory the declared-set guard would
accept, was still caught by the declared-set guard for the second reason and
proved nothing.

**The general fix is to assert the MESSAGE, not the refusal.** What the
narrower guard is worth is naming the true cause rather than letting a
confusing downstream mismatch stand in for it, so that is the property to
assert — the error text names *this* guard's reason. It makes the guard
non-equivalent, which is exactly the thing a surviving mutant was telling you
it was not. Where a guard genuinely cannot be isolated even by its message, it
is equivalent to its neighbour: say so and delete one, rather than keeping two
claims and testing neither.

**A guard nothing can reach is a claim, not a guard — delete it, and record
why where it stood.** The deletion rule at guard grain. The same pass found an
`-f` existence check that no input could reach: the declared set had already
been compared against the directory listing, so every path the loop opened was
one the listing produced. Removing it changed nothing because nothing could
reach it. A surviving mutant means one of three things — the assertion is
missing, the control does not isolate, or the code is dead — and they are
distinguished by asking what input would reach it.

**`bash -n` is not a proof. A shell block is verified by RUNNING it against
the real consumer.** The shell-grain form of the second-consumer rule: a
parse check proves the syntax is well-formed, which is nearly orthogonal to
whether the command means what it says. Three defects in one small change,
each of which `bash -n` accepted and each of which would have failed in the
middle of a cluster run, tens of minutes from the edit:

- **`psql -c` does not interpolate psql variables.** `:'org'` there is not a
  substitution that resolves to the wrong thing — it reaches the server
  literally and is a syntax error. The query has to arrive on stdin via
  `-f -`. Nothing local says so; the manual is thin on it and the shape looks
  exactly like the working one.
- **Bundled short flags swallow what follows.** `-Atqc` ends in `-c`, so
  inserting `-v org=...` between it and the SQL made `-c` take `-v` as its
  argument. The command is still valid shell and still valid `psql` invocation
  syntax; it simply runs the wrong thing.
- **A second use site, three hundred lines away.** Under `set -u` a missed
  rename is an unbound variable at RUNTIME, not an error at parse time, so the
  block dies after the expensive part has already run.

The method that found all three was running the block against a real
PostgreSQL — extracted VERBATIM from the tool with `sed`, not retyped, so what
ran is what ships. And once it runs at all, ask the next question for free: the
same probe confirmed a second application resolves its own row through the
same code, an absent identity returns empty rather than erroring, and `:'var'`
actually escapes — a value carrying a quote matched zero rows rather than all
of them. A quoting form is a safety claim, not only a substitution.

Corollary for anything that touches a database, a cluster, or another process:
stand up the real consumer, disposable and named, and remove it by name. The
cost of a container for ninety seconds is nothing against a defect that
surfaces after a build, a push, and a deploy.

**An unquoted heredoc RENDERS; it does not quote. Nothing inside it is
inert — least of all a comment.** The fourth defect in the same block, found
by reading a passing run's log rather than by any check: the probe's
explanatory comment named its own shell in backticks, and because the
heredoc's delimiter is unquoted, the renderer ran `sh -ec` on the developer's
machine, once per generated Job, and substituted its empty output. The
manifest shipped with the sentence deleted, and the run passed, three times,
carrying the error on stderr where it read as somebody else's noise.

Two things make this worth a law rather than a fix. The first is that the
damage lands in the artifact, not the script: the tool ran correctly and the
thing it WROTE was wrong, so every local check of the tool is green. The
second is the direction of failure. A comment is the one construct an author
is certain cannot execute, so it is the one place the escaping discipline
relaxes — and command substitution does not care what a line means.

Verified the way the entry above prescribes: the block extracted VERBATIM by
line range, rendered with pinned inputs, and diffed. Post-fix the render emits
zero bytes on stderr and the sentence is whole; the pre-fix control emits
`sh: 0: -c requires an argument` and renders `The shell is , so a failing`.
The diff between the two renders touches only the comment, which is what
proves the fix changed nothing else in the manifest.

Corollary: quote the delimiter (`<<'EOF'`) whenever the block needs no
interpolation, and when it does need interpolation, remember that the price is
that every backtick, `$(`, and `$` in it — including the ones in prose — is
live.

**An environment variable carries a PROCESS SETTING. Data crosses a boundary
as a declared, schema'd artifact.** The cluster journey used environment
variables as the data contract between a Rust producer and a shell consumer,
and the shape of the damage is what makes this a law rather than a taste.

Env vars have no schema, no ownership and no types. So every new value spawns
a new name, and the name encodes whatever the author happened to be thinking
about at the time — the application, the test, the database. Thirteen names
grew that way. Renaming them to a neutral prefix was the obvious repair and
it is the wrong one: it treats the symptom, and the symptom immediately
reappeared. `WAMN_ROUTE_*` turned out not to be an empty namespace, so the
tree gained `WAMN_ROUTE_AUTH_PG18_URL` and `WAMN_ROUTE_PG18_URL` — two
databases, one segment apart, in a flat space with nothing to say they are
different things. A second application-named family was already queued behind
the first.

As FIELDS of one document the question does not arise. `auth_pg_url` and
`system_pg_url` are two fields; nobody asks whether they collide, because a
document has structure and a namespace does not.

The repo already had the right machinery, in `services/ctl/src/dev/config.rs`:
a document struct carrying `#[serde(deny_unknown_fields)]` and
`#[derive(JsonSchema)]`, schema bytes generated from it and checked in, a
drift test asserting the checked-in bytes equal the generated ones, and an
`#[ignore]`d regeneration test as the only way to update them. The one most
worth copying is the third test — that the generated schema and the strict
parser share ONE field authority, so a field cannot exist in the parser and be
absent from the schema. That is what makes the artifact a contract rather than
a serialization.

The test for whether something is a process setting: would the receiving
process still start if it were absent, differently spelled, or empty?
`RUSTC_WRAPPER` and `CARGO_TARGET_DIR` change how a process runs.
A database URL is what the process is FOR.

**The lift checklist.** Before extracting any block into a shared function,
answer three questions, and answer them BEFORE the extraction rather than
after:

1. **Does it read anything it was not handed?** Every input becomes a
   parameter, and a missing one is a named error rather than an empty string
   that produces a subtly wrong result.
2. **Can a caller collide with its parameter names?** Any indirection that
   resolves a name at runtime can silently bind to the wrong variable. Prove
   the binding with content, not names.
3. **Does every guard it claims have a mutant that kills it?** A guard nothing
   exercises is documentation, and lifting is exactly when contracts get
   written and not tested.
4. **Does it return rather than exit?** A shared function that calls `exit`
   seizes its caller's control flow, so the caller cannot add context, clean
   up, or decide that this failure is tolerable. This is question one seen
   from the output side: a block may not reach for anything it was not handed,
   and it may not take anything the caller did not offer either.

A corollary from running this: independent guards that overlap are not
redundancy. While the nameref binding was under test, the collided render was
caught by the unrelated exactly-once anchor rule, because decoy identity
values derive anchors that match nothing. Defence in depth working as
designed.

**A test must assert the DISTINGUISHING STEP, not a count that can coincide.**
An assertion can pass for a reason unrelated to the property it is named for,
and a count is the commonest way. This has now bitten three times, each caught
by mutation testing rather than by review:

- A generator seed mutant survived because the duplicate and reorder stages
  still consumed the seed, so dropping it from id generation changed nothing
  the test looked at.
- An IR byte-stability gate passed by reading the same files twice and
  comparing — trivially stable, and blind to the reordering it existed to
  detect. Strengthened to reorder-then-compare, it immediately found a real
  passthrough bug.
- A TUI highlight test pressed down three times over two rows and asserted the
  final index. Three presses over two rows land on the same index whether the
  highlight saturates or wraps, so a wrapping mutant survived a test named
  `the_highlight_saturates_rather_than_wrapping`.

**Standing law: assert the step at which the correct and incorrect behaviours
first differ, not an aggregate they may agree on.** For the highlight that is
"from the last row, one more press stays put"; for byte stability it is
"reorder the non-semantic lists and compare"; for a seed it is "vary only the
seed with every other knob off". The test that survives a mutant is the one
that names the difference, and the way to find out is to write the mutant.

**Companion at the fixture layer: choose values a hardcoded stand-in cannot
match.** The same failure appears one level down. A blobstore mutant that
hardcoded `environment` to `"prod"` survived its test because the fixture
manifest also said `"prod"` — reading the manifest and ignoring it were
indistinguishable, so an assertion named for a lookup proved only that two
constants agreed. A fixture says `"warehouse-eu-3"` for the same reason a
count is a bad assertion: plausible values collide with the wrong
implementation, unguessable ones cannot.

Corollary, from the same three: canonical means order-independent **only for
sets**, so a reorder-and-compare test must know which of its lists are sets
and which are orderings. Reversing an ordering correctly fails a correct
implementation.

**A SHARED assertion must not encode one consumer's constant.** The same
failure at harness grain, found while extracting the cluster journey so a
second application could use it. Two instances, both of which read as platform
code:

- `trace_is_complete()` looks entirely generic and asserts
  `count_named("wamn.postgres.statement") == 1` — which is Receiving's probe
  route issuing exactly one statement. A consumer whose route issues eight
  fails at `"trace is incomplete at collection"`, a message naming the wrong
  cause.
- One awk program rewrote namespace, environment and image for any caller
  (shared) while hardcoding `wamn.tenant`, `environment`, `project` and
  `schema` (Receiving's). Left fused, a second application deploys under the
  FIRST one's tenant, route authorization resolves against the wrong data, and
  the journey PASSES. A green gate proving the wrong thing is worse than a red
  one.

**Corollary: a matcher that widens its reach demands uniqueness.** Un-fusing
an anchor often means matching on less — trimming whitespace, ignoring order,
comparing content instead of a whole line. Every such widening lets the anchor
reach places the narrower form could not, and the narrower form was usually
excluding them by accident rather than by intent. A values-overlay anchor that
matched a full indented line became a match on the line's content, which fixed
a real coupling to a file the harness does not own — and simultaneously made
`replicas: 3` able to match that key at any depth in the tree, not just the
one meant.

So widening comes with a count. The anchor must fire EXACTLY once, not at
least once: a zero means the template drifted, a two means the anchor has
found a second home, and the second is the dangerous one because the render
still succeeds and produces a file nobody asked for. Prove it by planting the
duplicate — take the key the anchor matches, add it a second time somewhere
else in the document, and require the run to fail. An at-least-once guard
passes that test, which is how you know it was never the guard you needed.

The general form: whenever a match is loosened, ask what the strict form was
excluding, and assert that the loosening did not admit it. Otherwise the
un-fusing has removed one coupling and quietly built another.

**An anchor map proves what it REPLACED, never what remains. A render is
verified by what survives it.** The exactly-once counts answer "did every
substitution I declared fire?" That is a complete answer to a question that
is only half the problem. The other half is whether the template holds a
SECOND copy of a placeholder somewhere the map does not look — a field added
upstream, a legacy key beside the current one, a comment naming the demo
tenant. Nothing in the anchor machinery can see it, every declared count is
satisfied, and the placeholder renders straight through into a manifest the
cluster accepts.

The guard is the complement of the anchor map, and it is three lines: after
rendering, sweep the output for each of the template's own placeholder values
and refuse if any survives. Its negative control has to plant the placeholder
under an anchor NOBODY declared — appending `WAMN_MAT_LEGACY_TENANT: t1` to a
template whose `WAMN_MAT_TENANT: t1` is fully handled — because a control that
deletes a declared anchor is caught by the count rule instead, and proves the
neighbour.

This generalises past renders. Any transform declared as a set of rules over
an input owes two proofs: every rule fired, and nothing the rules were meant
to eliminate is still there. The first is about the transform; the second is
about the artifact, and only the second is what ships.

**THE SWEEP'S FINAL FORM IS A COUNT, NOT AN ABSENCE: a placeholder may
survive no more often than the claims that legitimately declare it.** The
absence form above is the first draft, and it is stated that way because it
is the one an author reaches for. Applied to the workload renderer it
immediately refused a correct manifest: Receiving's own catalog is `default`,
the same string the template carries as its placeholder. The tempting repair
— skip the check when the declared value equals the placeholder — blinds the
sweep at precisely the claim it was added for.

So: one surviving `"default"` for Receiving's catalog is right, and a second
occurrence is something the anchor map does not cover. The same shape appears
wherever a guard's subject and its sentinel can coincide, and the general
lesson is that "must not appear" is usually a count of zero that nobody
checked was really zero.

Two smaller things fell out of the same guard, both worth keeping:

- **Quote the sentinel when the format does.** `default` also appears in this
  template as `hostgroup: default` and inside a comment naming
  `values-host-default.yaml`. Sweeping the bare word refuses a correct file;
  sweeping `"default"` does not, because every claim value is a quoted YAML
  string. A mutant that widened the sweep to the bare word is killed by a
  positive assertion that the unquoted uses SURVIVE.
- **Every key the contract calls required needs its own control, not one
  representative.** Mutation caught this directly: dropping `catalog` from the
  required list survived, because the only missing-key case unset `tenant` and
  every fixture happened to carry a catalog. One control per required key, or
  the list is a comment.

**The test of an un-fusing: a parameter no test can distinguish from the
constant it replaced is the same fusion with extra steps.** Introducing the
argument is not the work; proving it CHANGES something is. The statement-count
un-fusing above was proven by evaluating the extracted `jq` program twice —
expecting 1 passes, expecting 8 fails — because a parameter that only ever
receives the old literal has moved the coupling without removing it, and reads
as fixed to every later reader.

**A constant is only proven generic when a SECOND CONSUMER with different
values passes through it.** This is the working definition of the second-app
problem, and it was learned three times on one file. Each block read as
platform code; each turned out to carry one application's values:

- `trace_is_complete()` — generic span names, and one statement count.
- The workload renderer — generic namespace and image, and four identity claims.
- A five-file secret assertion that looked like a platform invariant asserted
  over app-produced output. It is not a platform invariant at all: the
  platform owns a twelve-variant role-family vocabulary, the journey emits
  five of them, and the values overlay substitutes each by a name carrying
  `org--project--env`. Reading the ASSERTION made it look platform; reading
  its CONSUMER showed it is not.

The lesson is about method, not about these three blocks. Inspection cannot
distinguish "generic" from "has only ever had one caller" — only a second
caller with different values can, which is why a second application is worth
more as a review of the first than as a feature.

**A fusion does not only distort structure — it can CORRUPT EVIDENCE, and that
instance is the one to keep.** The values-overlay block substituted a replica
count with the awk pattern `/^      replicas: 3$/`, hardcoding the template's
own current value. A template that drifted made the match silently miss, and
the line passed through unchanged. In the `--measure-startup` arm that is not
a cosmetic defect: the arm asks for `0` replicas so it can scale from zero and
time a cold start, and the missed anchor rendered `3`. The host was never
scaled to zero, so "cold startup" was measured against an already-running
host — and the run stayed green, because both post-render verification loops
checked the five secret names and neither checked `replicas`.

The distinction worth carrying: a fusion that only couples code is a
maintenance cost, and a reviewer will find it. A fusion sitting between a
measurement and the state it measures manufactures a plausible number and
reports success. Nothing downstream can tell that number from a real one. So
when un-fusing anything a proof READS, ask which of its substitutions are
checked afterwards — the guarded ones fail loudly and were never the risk; the
unguarded one is where the false measurement lives. The fix is two changes,
not one: derive the anchor, AND assert that every declared anchor fired.
Measured against the same mutant, the old block exits 0 having rendered the
wrong count, and the new block exits 1 naming the anchor.

**A nameref's own name is part of its interface, and must not be guessable.**
The lift-specific hazard, and the worst failure mode in this whole family:
byte-identical output produced from the wrong source. A shared bash function
took its parameters as `local -n spec=$1`, so a caller whose own array was
also called `spec` created a circular name reference. Bash does not error on
that — it WARNS, then resolves to whatever else is in scope. The render
succeeds, and the bytes may even be right.

They were right, in the instance that found this, and only by luck: the proof
harness had named its array `spec` too, so the wrong resolution happened to
land on the same array. A hundred warnings scrolled past above output that
diffed clean. Had the two arrays held different content, the function would
have rendered from the wrong one and the diff would have looked like a
substantive regression in the lift — or, worse, like nothing at all.

**Proof method, and run it BEFORE lifting a block rather than after.** Plant a
GLOBAL holding decoy values under the callee's nameref name, have a caller
pass a LOCAL array of the same name holding real values, and assert no decoy
reaches the output. That is the reproducible shape: bash resolves the cycle to
the global, so the render silently uses the decoy.

The result is a matrix, not a single check, because the collision is
reachable for ANY name the caller happens to pick. The defence is not
impossibility — it is that no caller would pick this one. So run the plausible
names a caller might actually choose (`spec`, `config`, `values`, `params`,
`opts`, and the caller's own array name) and require all of them to pass, then
run the internal name itself as the control and require it to FAIL. A control
that passes means the proof is inert: the first version of this check bound
the decoy to a global while the caller used a *differently* named array, which
is not a cycle at all, and it passed against a deliberately regressed
function.

Silence on stderr is not the assertion. Warnings scroll past in a long run,
and the check must be on the bytes.

Worth noting where the save came from in practice: the collided render was
also caught by the exactly-once anchor rule above, because decoy identity
values derive anchors that match nothing. Two independent laws, and the second
one held when the first was the one being tested.

The general form: any indirection that resolves a NAME at runtime — namerefs,
`eval`, variable-variables, a template that interpolates an identifier — can
silently bind to the wrong thing, and the tell is absent precisely when the
two things are similar enough to be confused. Name the internal side so no
caller would collide with it, and prove the binding with content, not names.

**A variable that means both "input to a render" and "expected state after an
imperative step" is two variables wearing one name.** `host_replicas` was set
to 3, or to 0 under `--measure-startup`, and passed to both renders. Then the
measure arm deployed zero replicas through Helm, scaled to one imperatively
outside Helm, and REASSIGNED the same variable to 1 — after which four
downstream assertions read it as the expectation. Both readings are correct
and they are not the same fact, so the name was true only because nothing
between the two uses looked at it.

This shape is specifically dangerous to LIFT. Extracting the render into a
function that takes `replicas` as a parameter severs the later reassignment
from it — which is the right outcome, arrived at by accident rather than by
design, and silently. The reader who later reintroduces a single name will not
find a test that objects. Split the name before extracting, not after:
`host_render_replicas` is frozen once the arm is chosen, `host_replicas` is
the expectation and may be reassigned. The general rule is that a lift is safe
only when every name it captures means exactly one thing across its whole
lifetime, and the way to check is to look for reassignment BETWEEN the uses,
not at the uses.

**Standing law: a shared block reads only what it is passed.** No closure over
caller state, no constant belonging to one consumer. Where a block is genuinely
mixed, UN-FUSE IT IN PLACE FIRST — as its own change, proven by the existing
consumer still passing — and only then move it. Extracting first and finding
the fusion later is how the wrong-tenant green gate gets built.

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
`services/ctl/Cargo.toml` declares `required-features = ["ops"]` on
`protected_relations_live`, so a bare `cargo test -p wamn-ctl` never builds it
and is green without it. Naming it explicitly without the feature does error —
measured:

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

## Measured: what the unselected M1 guests cost the `wamn dev` loop

`wamn-10yt.10.25` asked whether the `wamn dev` watch loop spends enough time on
M1 artifacts it never consumes to justify a package-scoped selector inside
`tools/build-components`. Measured on 2026-09-04 at `3742a0e8`: **the compile
waste is not material; the virtualization waste is, and it is a different
mechanism than the bead names.** No selector was added.

**The set.** `architecture/workspace-tiers.json` puts nine packages in the M1
tier (`profiles.components.m1_inventory_tier` = `product_components`), split
across the two guest workspaces:

| workspace | M1 packages |
| --- | --- |
| `components/Cargo.toml` | `blob-put` `client-acme-receiving` `http-route` `materializer` `receiving` `wms` |
| `components/no-std/Cargo.toml` | `http-request` `label-render` `transform` |

`wamn dev --overlay-root packages/client_acme_receiving` consumes exactly two
of them: the overlay guest `client-acme-receiving`, and the guest of its single
base dependency (`packages/client_acme_receiving/wamn.json` →
`base_dependencies.base_receiving.package` = `wamn_receiving` → component
`receiving`). **Seven are unselected, not six** — the bead's count predates the
current tier. Both selected guests live in `components/Cargo.toml`, so all
three `no-std` guests are unselected and selection would drop an entire Cargo
invocation and an entire target directory from the loop.

**Conditions.** 8 cores, `jobs = 4` from `.cargo/config.toml`, `sccache`
already warm from concurrent lanes, and **five other lanes compiling
throughout**: 1-minute load average ran 8.8 → 34 across the run. Absolute
seconds here are inflated and are not a clean-machine baseline. Every A/B pair
below was measured back to back under the same load and run in both orders;
read the paired deltas, not the absolutes. Debug profile, `wasm32-wasip2`,
scratch `CARGO_TARGET_DIR` under `$HOME/.cache` (never `/tmp` — see the
disk-quota trap above). Each arm reproduces what `tools/build-components`
issues, including its `--remap-path-prefix`:

```bash
# full M1, as the loop builds it today
CARGO_TARGET_DIR=$SCRATCH/full-std RUSTFLAGS="--remap-path-prefix=$PWD=/wamn" \
  cargo build --locked --offline --target wasm32-wasip2 \
  --manifest-path components/Cargo.toml \
  -p blob-put -p client-acme-receiving -p http-route \
  -p materializer -p receiving -p wms
CARGO_TARGET_DIR=$SCRATCH/full-nostd RUSTFLAGS="--remap-path-prefix=$PWD=/wamn" \
  cargo build --locked --offline --target wasm32-wasip2 \
  --manifest-path components/no-std/Cargo.toml \
  -p http-request -p label-render -p transform

# what the loop actually consumes
CARGO_TARGET_DIR=$SCRATCH/sel-std RUSTFLAGS="--remap-path-prefix=$PWD=/wamn" \
  cargo build --locked --offline --target wasm32-wasip2 \
  --manifest-path components/Cargo.toml \
  -p client-acme-receiving -p receiving
```

**Cold** (fresh target directory per arm; crate counts are exact and stable):

| order | full std (74 crates) | full no-std (42 crates) | full total | selected (62 crates) | delta |
| --- | --- | --- | --- | --- | --- |
| selected first | 127.08 s | 118.70 s | 245.78 s | 80.69 s | **165.09 s** |
| full first | 138.39 s | 105.55 s | 243.94 s | 100.46 s | **143.48 s** |

The four extra `components/` guests add only 12 crates; the whole 42-crate
`no-std` leg is waste, and it is 72-74 % of the cold delta because a separate
target directory shares no compiled dependency with the first leg.

**Incremental**, touching the selected base guest
`components/application/receiving/src/lib.rs` — the loop's hot path. Both arms
recompile the same single crate; the unselected packages are only fingerprinted:

| pair | full std | full no-std | full total | selected | delta |
| --- | --- | --- | --- | --- | --- |
| 1 | 2.19 s | 0.35 s | 2.54 s | 2.14 s | 0.40 s |
| 2 | 2.37 s | 0.21 s | 2.58 s | 1.46 s | 1.12 s |
| 3 | 4.76 s | 0.28 s | 5.04 s | 3.63 s | 1.41 s |
| no-op (nothing touched) | 0.34 s | 0.44 s | 0.78 s | 0.41 s | 0.37 s |

Touching a crate that the unselected guests *also* consume
(`components/execution/contract/src/lib.rs`, which reaches `wms` through
`wamn-wms-data-access`) makes the full arm compile five crates against the
selected arm's three, and still costs nothing measurable: **+1.00 s** in one
order and **-0.83 s** in the other. With `jobs = 4` the two extra crates fit in
the parallel slack of the three that must rebuild anyway.

So the compile-side per-loop waste is the **0.37 s no-op delta** — a second
`cargo` process and a fingerprint scan for a `no-std` leg that compiles nothing
— not compilation. It is below the run-to-run variance of the identical
selected build, which spanned 1.46-3.63 s under this load.
`tools/build-components watch-roots m1` (two `cargo metadata --no-deps` reads
plus `jq` validation, no compilation) costs 0.48/0.48/0.65 s on top.

**Virtualization is where the loop actually wastes time.** `wamn dev`'s
Virtualize stage runs `tools/build-components virtualize-only`, which
`rm -rf`s its output directory and re-virtualizes every allowlisted artifact
unconditionally, every loop. `tools/component-virtualization.json` allowlists
three — `blob-put`, `client-acme-receiving`, `receiving` — and
`select_component_artifacts` in `services/ctl/src/dev/coordinator.rs` then
consumes two. **One of three virtualizations is thrown away on every
iteration**, at 2.80/4.56/4.59 s for `blob-put` run bare, or 4.01/5.86 s as the
loop invokes it through `cargo run`. That is roughly ten times the entire
compile-side waste.

**Verdict — not material, on the criterion the bead states.** Threshold used:
a saving is material if it exceeds run-to-run variance *and* clears 1 s or
20 % of the incremental stage it belongs to. Compile-side waste is 0.37 s,
about 15 % of a 2.5 s incremental Build and under the noise floor: **not
material**, so no package-scoped selection was added to the build owner. Cold
waste is 143-165 s, but that is paid once per fresh target directory, not per
loop, and the acceptance criterion names watch-loop cost.

**Two things this measurement did not settle, for the owner rather than the
lane.** First, the virtualization waste (~3-6 s per loop) does clear the
threshold in absolute terms, but the fix is narrowing the virtualization
allowlist filter, not package-scoped *building* — a different change from the
one the bead authorises. Second, whether ~3-6 s is material depends on total
loop wall time, which was not measured: `[WAMN-DEV-LIVE]` needs live Postgres,
NATS, and a registry, and must never be pointed at shared infrastructure.

**Two constraints on any future selector.** Watch roots are workspace
*directories*, not packages — `component_build_watch_roots` in
`services/ctl/src/dev/command.rs` takes `watch-roots m1`, which returns
`components` and `components/no-std`, so an edit anywhere under either tree
still fires the Build stage regardless of what gets built. And selection is not
purely subtractive: Cargo unifies features per invocation, and the guests carry
ungoverned `chrono` declarations (`components/data/receiving-data/Cargo.toml`
and `components/data/wms-data/Cargo.toml` name the version directly instead of
`workspace = true`, which is `wamn-onj5`), so a narrower `-p` set can resolve a
different feature union than the full build it replaces.
