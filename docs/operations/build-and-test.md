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
(`Dockerfile:260`), so the suite runs against the same host lib code it
verifies. Host-built binaries cannot be `COPY`d into the image — the build
stages are `rust:1.98-trixie` and the runtime stages `debian:trixie-slim`, and
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

## Upstream release gate

The release gate takes an explicit, clean upstream wasmCloud checkout.
Its `origin` must name `https://github.com/wasmCloud/wasmCloud`.
Both `HEAD` and the peeled `v2.9.0` tag must equal
`68ebece9c537f8bb4b5c9999f274ec68d60f35a9`.

Install both Wasm targets for the repository's pinned Rust toolchain.
The runtime tests also require Docker for their disposable services.

```bash
rustup target add --toolchain 1.98.0 wasm32-wasip1 wasm32-wasip2
tools/wasmcloud-release-check dry-run ../wasmcloud
tools/wasmcloud-release-check run ../wasmcloud
```

The gate runs the read-only formatter, builds upstream Wasm fixtures, and runs wash-runtime tests and wash template-clone fixtures.
Upstream `cargo xtask build-fixtures` owns fixture generation.
Generated files must leave the upstream tracked source and lockfiles clean.
It records each command, feature tree, exit code, and test skip message.
Both test legs include ignored tests and select upstream default features.
WAMN production features remain separate, with `default-features = false`.
The gate includes later legs after a test failure and returns a failing exit code.

The gate exports `GIT_CONFIG_GLOBAL=/dev/null` and `GIT_CONFIG_NOSYSTEM=1` for every Git and Cargo child.
This isolates the Git fixtures from developer signing requirements.
The gate makes sure that upstream source stays clean between test legs.
Do not edit upstream source or fixtures to satisfy this gate.
Retain warnings, failures, and missing test inputs in the release evidence.

A source update moves the root and standalone HTTP probe manifests and lockfiles together.
Update the source identity tests, runtime inventory, release gate, and native-alignment ledger in the same change.
Update the chart seam record when the deployment stage moves the chart.
The cutover charter requires zero carried patches and records separate build, deployment, and live-evidence stages.
A passing source gate does not establish release readiness.

Install the controlled CLI before manual publication:

```bash
wamn_wash=$(tools/install-wash)
"$wamn_wash" --version
```

The installer pins upstream wash 2.9.0 for Linux x86_64 by a fixed SHA-256.
It checks downloaded and cached bytes before returning the ignored `.tools/wash/2.9.0/wash` path.
Active publishers select their private credential directory through `DOCKER_CONFIG` and call upstream `oci push`.
The directory contains their existing registry credential document as `config.json`.
Passwords stay out of command arguments and command records.
JSON publication receipts must report both `.success` and `.data.success` as true.
The component digest is `.data.digest`.
WAMN custom artifact publication remains owned by `wamn-ctl push-component` and its admission proof.


### Rebuilt host process lifecycle

The ignored host test starts an owned NATS process on a temporary loopback port.
It runs the rebuilt WAMN host with cleared environment variables.
It tests ingress saturation, a 50-second NATS outage, recovery, SIGTERM, SIGINT, and a stalled native trace exporter.
The normal signal cases must exit successfully within the 70-second deployment grace.
The stalled exporter must report a flush failure within that grace.
The test records an initial `starting` response if it observes that brief state.
The [first live run](../perf/2026.09/wasmcloud-2-9-cutover/host-lifecycle-live-001/source.json) passes at clean `9d54245c`.
SIGTERM and SIGINT exit successfully in 5,012 ms and 5,004 ms. The blocked trace exporter produces exit code 1 in 7,008 ms.
Both normal cases miss the brief initial `starting` state.
The test does not prove failure before the first native beat, forced native loop/listener failure, active guest drain, or operator recovery.

Set the NATS executable to an absolute path.
Set the evidence directory to a new path outside the build worktree.
Create its parent directory before the command.

```bash
WAMN_HOST_LIVE_NATS_SERVER_BIN=/absolute/path/to/nats-server \
WAMN_HOST_LIVE_EVIDENCE_DIR=/absolute/path/to/new-host-evidence \
  cargo test -p wamn-host --test native_lifecycle_live --locked --offline \
  rebuilt_host_probes_signals_and_scheduler_recovery -- --ignored --exact --nocapture
```

The full Receiving journey also tests the actual idle executor with its provisioned credentials.
It requires native readiness and liveness, then successful SIGTERM and SIGINT exits within the 15-second deployment grace.
Those idle cases do not establish queued-delivery drain behavior.

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

[re-measured 2026-09-08 at `088dde73`, after wave 10: **204 binaries, 1911
passed, 1 failed, 73 ignored, no compile errors.** The one failure is
`every_mounted_secret_is_declared_here_or_named_a_prerequisite`, filed as
`wamn-362o.58`. The `1bffa614` figures above are left as the owner measured
them.]

[re-measured 2026-09-09 at `3c831ea9`, after wave 11: **206 binaries, 1929
passed, 1 failed, 77 ignored, no compile errors.** The same one failure, still
`wamn-362o.58`.]

## Conformance

```bash
cargo test -p wamn-proof-conformance --no-fail-fast
```

**Adding or removing a Cargo workspace member moves TEN hard-coded sites,**
all asserted against live `cargo metadata`, so a partial edit is red and a
partial COMMIT is red even when the final tree is green (remeasured at
`wamn-362o` adding two components: exactly ten files). Seven member
inventories: (1) the workspace `Cargo.toml` members list, and for root also
`[workspace.dependencies]`; (2) `architecture/package-roles.json`, one row
`{workspace,name,manifest_path,role,target_class,bounded_context,deployable}`,
unsorted, grouped by workspace then source directory, the optional eighth
field `native` absent on non-native rows; (3) `architecture/workspace-tiers.json`
-- `source_inventory.package_count`, EVERY tier's `root_packages` and
`component_packages` (sorted, unique, byte-lexicographic),
`profiles.expected_package_counts`, and the prose in `selection.reason`,
`command_semantics` and `bare_cargo_semantics.selected_packages`, the last
checked by whole-token equality against the live count;
(4) `tests/conformance/tests/package_architecture.rs` `ROOT_MEMBER_COUNT` and
`COMPONENT_WORKSPACES[].member_count`;
(5) `tests/conformance/tests/profile_selectors.rs` -- `root_members.len()`,
`component_members.len()`, the m1 and proof expected name lists, the
`profile_counts` table, and the proof-minus-m1 difference set;
(6) `tests/conformance/tests/retained_root_outcomes.rs` `RETAINED_ROOTS`,
asserted set-equal to `package-roles.json` across all three workspaces;
(7) `tests/conformance/tests/repo_lint.rs` `ROOT_MEMBER_COUNT`,
`COMPONENT_MEMBER_COUNT`, `NO_STD_MEMBER_COUNT`. Three ABI guards, each a
length-typed array asserting RAW BYTES (`cp` the file, never reformat):
(8) `crates/platform/runtime/tests/node_wit_coherence.rs` `EXPECTED_COPIES`;
(9) `tests/conformance/src/invocation.rs` `EXPECTED_NODE_ABI_COPIES`;
(10) `crates/platform/runtime/tests/postgres_wit_coherence.rs`
`EXPECTED_COPIES` -- a data-access guest vendors `wamn-postgres` as well as
`wamn-node`, so both pairs move together. App-interface WIT copies
(`wamn-<app>-<iface>`) are package-owned and guarded by nothing.

Generated operator crates in `packages/*/generated/*-tui/` are native root members.
Register them in sites (1) through (7).
They add no WIT copies, so sites (8) through (10) stay unchanged.
The root has 40 members, and its default selects 19.
The `deploy` selector has 33 members, while `m1` and `m2` retain 20 and 22.
The `full` and `ops` selectors each have 40 members.

**A component that a package declares moves none of the sites (2) to (7)**
(`wamn-10yt.10.39`). `packages/<package>/wamn.json` names its components. The
package half of every inventory above is derived from that file. This covers the
tier lists, the per-workspace counts, the virtualization artifacts, and the
`package-roles.json` row. The row's `bounded_context` is the declaring package's
own model schema. Every number and name list in those files states the PLATFORM
half only. One derivation per language owns this. In Rust it is
`tests/conformance/src/package_inventory.rs`. In shell it is
`tools/build-components` and `tools/workspace-tier`. Extend those. Do not write
a second copy. Authoring such a package touches only `packages/<package>/`, the
component crate directory, and two shared files in the components workspace.
Those two are the `members` line in `components/Cargo.toml`, which is site (1),
and `components/Cargo.lock`. Cargo owns the lockfile and regenerates it, but
`cargo metadata --locked` refuses a stale one, so the new `[[package]]` stanza
lands with the package (wamn-10yt.10.40). The allowlist stays CLOSED. A crate with no package manifest is refused by
`tools/build-components` with `component profile, canonical inventory, and
locked metadata drifted`, and `package_architecture.rs` reports it as
unclassified. If a package's models carry more than one schema, the package has
no single bounded context and the derivation refuses.

**Tier placement** (from `workspace_tiers.rs`): `role=test` goes in `full_ci`
and `deployed_system_proof`, excluded from `fast_developer_native`; a
deployable cdylib guest goes in all four (`product_components`, `release`,
`full_ci`, `deployed_system_proof`); a guest-consumed rlib (`adapter`/`guest`,
`deployable: false`) goes in `full_ci` and `deployed_system_proof` only,
because `release` asserts every member carries `cdylib` or `bin`. For
components, `m1_inventory_tier` is `product_components` and
`proof_inventory_tier` is `full_ci`. Baseline these gates BEFORE editing so an
inherited red is not attributed to the change: this class rots in both
directions. `profile_selectors.rs` and `workspace_tiers.rs` also assert the
selector tools' SOURCE contains no canonical package name as a substring, so
grep `tools/` before choosing a short name. A new guest needs
`tools/component-virtualization.json` only if it must be virtualized; the
allowlist test iterates declared artifacts only, so an omission is silent at
gate time and shows up at deploy.

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

**Generate each package against its own database.** Applying all three to one
database corrupts the base generation: `client_acme_receiving` adds
`acme_inspection_required` and `acme_quality_status` to
`receiving.purchase_order`, so introspecting afterwards writes those overlay
columns into 14 `packages/receiving/generated/` files. Use three isolated
databases: `receiving` alone, `wms` alone, and `receiving` plus its overlay.

**Editing a `wamn.json` moves two pinned artifacts, not one.** The manifest
sha256 lives in `generated/platform-policy/data-access.json`, and
`verified_schema_state_id` in `generated/package-weld.json` is the hash of the
whole introspected catalog, so an introspection change moves it as well.
Regenerate after either kind of edit.

# EVERY package root under packages/, derived from the tree and never listed.
# The list used to be the single literal `packages/receiving`, which is why
# packages/wms/generated/client could go missing at 891aa296 and nothing went
# red (wamn-10yt.57): no gate named wms. A hardcoded list makes the next package
# added the next silent gap, so this one is found (wamn-10yt.58). Each root gets
# its own database, for the reason the paragraph above gives, inside the
# container this gate already runs. A root's base_dependencies are applied
# first, resolved through `.package.id`, so an overlay never introspects without
# its base. The CREATE SCHEMA list is the generator's own derivation
# (`application_schemas` in crates/schema/generator/src/data_access.rs):
# `wamn ctl apply-package` creates those schemas and the migrations assume them.
mapfile -t MATERIALIZE_ROOTS < <(
  find packages -mindepth 2 -maxdepth 2 -name wamn.json -printf '%h\n' | sort
)
declare -A MATERIALIZE_ROOT_BY_ID=()
for MATERIALIZE_ROOT in "${MATERIALIZE_ROOTS[@]}"; do
  MATERIALIZE_ROOT_BY_ID["$(
    jq -r '.package.id' "$MATERIALIZE_ROOT/wamn.json"
  )"]="$MATERIALIZE_ROOT"
done
FUNCNEST=32  # a base_dependencies cycle errors instead of recursing forever
materialize_apply() {
  local root="$1" base schema migration
  for base in $(jq -r '(.base_dependencies // {})[] | .package' "$root/wamn.json"); do
    materialize_apply "${MATERIALIZE_ROOT_BY_ID[$base]:?no package root declares $base}"
  done
  for schema in $(jq -r '[(.models // {})[].schema]
      + [(.custom_operations // {})[].relations[]?.schema] | unique[]' \
      "$root/wamn.json"); do
    docker exec "$RECEIVING_PG_CONTAINER" psql -h 127.0.0.1 -U postgres \
      -d "$MATERIALIZE_DATABASE" -v ON_ERROR_STOP=1 -q \
      -c "CREATE SCHEMA IF NOT EXISTS \"$schema\""
  done
  for migration in "$root"/migrations/*.sql; do
    docker exec -i "$RECEIVING_PG_CONTAINER" psql -h 127.0.0.1 -U postgres \
      -d "$MATERIALIZE_DATABASE" -v ON_ERROR_STOP=1 -q -f - < "$migration"
  done
}
MATERIALIZE_CHECKED=0
for MATERIALIZE_ROOT in "${MATERIALIZE_ROOTS[@]}"; do
  MATERIALIZE_DATABASE="wamn_materialize_$(basename "$MATERIALIZE_ROOT")"
  docker exec "$RECEIVING_PG_CONTAINER" psql -h 127.0.0.1 -U postgres -d postgres \
    -v ON_ERROR_STOP=1 -q -c "CREATE DATABASE \"$MATERIALIZE_DATABASE\""
  materialize_apply "$MATERIALIZE_ROOT"
  MATERIALIZE_URL="postgresql://postgres:probe@127.0.0.1:${RECEIVING_PG_PORT}/$MATERIALIZE_DATABASE"
  # Two independent derivations must each equal the exact shipped path/byte set.
  WAMN_SCHEMA_INTROSPECTION_PG_URL="$MATERIALIZE_URL" \
    cargo run -p wamn-schema-generator --example materialize_package \
    --locked --offline -- check "$MATERIALIZE_ROOT"
  WAMN_SCHEMA_INTROSPECTION_PG_URL="$MATERIALIZE_URL" \
    cargo run -p wamn-schema-generator --example materialize_package \
    --locked --offline -- check "$MATERIALIZE_ROOT"
  MATERIALIZE_CHECKED=$((MATERIALIZE_CHECKED + 1))
done
# An empty roots array and a fully green run read identically otherwise. Assert
# the count, the way "Live gates: arming" requires of anything a rename or a
# moved directory can deselect.
test "$MATERIALIZE_CHECKED" \
  -eq "$(find packages -mindepth 2 -maxdepth 2 -name wamn.json | wc -l)"
# Measured at `2a4cd288` on a fresh postgres:18, one root per database:
# packages/wms passes; packages/receiving and packages/client_acme_receiving
# each FAIL, on generated/source-map/purchase_order.json and
# generated/wamn/purchase_order.rs. `4862faee` (wamn-10yt.54) taught the
# generator to emit `UPDATE_EXCLUSION_CONSTRAINTS` and regenerated no package,
# so no shipped artifact under packages/ carries that constant and a fresh
# derivation emits it. The arm is red on arrival because the class of defect it
# was written to find was already in the tree, unseen. Filed, not fixed here:
# regenerating a package's artifacts is not this arm's change.

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

# apply-package runs its migrations as wamn_db_owner, and a bare postgres:18
# carries neither that role nor a database it owns. Without these two steps the
# gate fails first with `role "wamn_db_owner" does not exist` and then with
# `permission denied for database wamn_receiving`. The test creates wamn_app and
# wamn_scenario_author itself, so only these two are missing.
docker exec "$RECEIVING_PG_CONTAINER" psql -h 127.0.0.1 -U postgres \
  -d wamn_receiving -v ON_ERROR_STOP=1 \
  -c "CREATE ROLE wamn_db_owner NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
      NOINHERIT NOREPLICATION NOBYPASSRLS"
docker exec "$RECEIVING_PG_CONTAINER" psql -h 127.0.0.1 -U postgres \
  -d wamn_receiving -v ON_ERROR_STOP=1 \
  -c "ALTER DATABASE wamn_receiving OWNER TO wamn_db_owner"

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
mints the same format-1 closure twice. It requires byte-identical canonical
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
EFFECTIVE_RELEASE_ROOT="$(pwd -P)"
# `cargo test -p wamn-ctl --lib` runs with the working directory
# `services/ctl`, so the path the test opens must be absolute.
EFFECTIVE_RELEASE_BASE_COMPONENT="$EFFECTIVE_RELEASE_ROOT/components/target/virtualized/std-empty-environment/receiving.wasm"
# The base component digest is authored EXACTLY ONCE, in the package manifest
# (wamn-10yt.50). Derive it here; never restate it. The guard below compares a
# bare sha256sum, so the `sha256:` prefix comes off.
EFFECTIVE_RELEASE_BASE_DIGEST="$(jq -r '.base_dependencies[].digest' \
  "$EFFECTIVE_RELEASE_ROOT/packages/client_acme_receiving/wamn.json" \
  | sed 's/^sha256://')"
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

# The pinned digest is the `m1` profile's virtualized output. Another profile
# builds other bytes and fails the guard below (wamn-10yt.61), so name it.
tools/build-components m1
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
belongs beside the conformance guards. It was kept out of that package only so
it would not collide with `wamn-0h0g.12.10`'s retained-manifest reconcile; that
reconcile has landed, so the relocation is unblocked and needs its own bead.
Measured 6 passed / 0 failed on the `w65-deploy` branch, base `2179f9c7`:

```bash
cargo test -p wamn-proof-system --test deploy_platform_inventory
```

### Known red

**RE-MEASURED 2026-09-06 at `b3197905`: the lib is 70 passed / 0 failed, and
all twenty-five test binaries are green.** This is a measurement at a named
commit, not a standing promise.

**The package has no red.** The one row this table used to carry,
`version_identity::governed_wire_schema_and_artifact_versions_stay_at_mvp_identity`,
was RED at `c72194c7` on governed first-party occurrence-count drift in
`crates/execution/run-state/src/admission.rs` and
`crates/platform/runtime/src/plugins/connection_http.rs`. It passes now, and
no commit claimed the repair, so it went green under the table the way three
live gates did under `wamn-0h0g.15.137.6`'s inventory. The remaining
long-standing red on this branch,
`connection_http_maps_an_invalid_context_to_a_wit_error_not_a_trap`, is a
runtime plugin live test and not in this package.

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
  --input components/target/wasm32-wasip2/release/std_virtualization_probe.wasm \
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
WAMN_STD_VIRTUALIZATION_FLOW_HTTP_WASM="$STD_VIRT_REPOSITORY_ROOT/components/target/wasm32-wasip2/release/http_route.wasm" \
  cargo test -p wamn-proof-integration --lib --locked --offline \
  virtualized_std_guest::tests::virtualized_std_guest_hides_the_sentinel_and_maps_a_panic_to_a_typed_refusal \
  -- --ignored --exact --nocapture --test-threads=1

std_virt_cleanup
trap - EXIT
```

The two containers and their ports are owned by this invocation. Never point
the gate at shared infrastructure or the frozen cluster.
The build command produces release artifacts, so both inputs above use that profile.
After any proof build, run `tools/build-components m1` before a production journey.
That rebuild restores production inputs but does not close the separate cross-profile digest finding, `wamn-10yt.61`.

### `[CLAIM-LAW-LIVE]` — the emitted claim contract tests, executed

`tests/integration/src/claim_law_live.rs` with the runner in
`test-support/harness/src/claim_law.rs` (`wamn-f89v`). The generator emits
`packages/wms/generated/contracts/inventory/move.claim-tests.json` and nothing
executed it. The runner reads that file and its sibling `move.operation.json`.
The sibling names the SQL file and the binds for every statement a case runs.
The runner then runs that SQL on a live server.

The database must be a **fresh disposable PostgreSQL 18** with no `wms` schema.
The tests refuse a database that already has one, create it, and apply
`packages/wms/migrations/0001_initial.sql` themselves. Nothing else is needed:
the claim ledger table has no foreign key to the pallet.

```bash
set -euo pipefail
CLAIM_LAW_PG_CONTAINER=wamn-claim-law-pg18
CLAIM_LAW_PG_PORT=54331
if docker container inspect "$CLAIM_LAW_PG_CONTAINER" >/dev/null 2>&1; then
  echo "$CLAIM_LAW_PG_CONTAINER already exists" >&2
  exit 1
fi
claim_law_cleanup() {
  docker rm -f "$CLAIM_LAW_PG_CONTAINER" >/dev/null 2>&1 || true
}
trap claim_law_cleanup EXIT

docker run -d --name "$CLAIM_LAW_PG_CONTAINER" \
  -e POSTGRES_PASSWORD=probe -e POSTGRES_DB=wamn_claim_law \
  -p "127.0.0.1:${CLAIM_LAW_PG_PORT}:5432" postgres:18
for CLAIM_LAW_ATTEMPT in {1..60}; do
  docker exec "$CLAIM_LAW_PG_CONTAINER" \
    psql -h 127.0.0.1 -U postgres -d wamn_claim_law -tAc 'SELECT 1' \
    >/dev/null 2>&1 && break
  sleep 1
done

WAMN_CLAIM_LAW_PG_URL="postgresql://postgres:probe@127.0.0.1:${CLAIM_LAW_PG_PORT}/wamn_claim_law" \
  cargo test -p wamn-proof-integration --lib --locked --offline \
  claim_law_live:: -- --ignored --nocapture

claim_law_cleanup
trap - EXIT
```

Expect `test result: ok. 3 passed; 0 failed`. The three are the two emitted
cases and the mutant arm. A run without `--ignored` selects only
`the_emitted_contract_names_the_two_cases_the_live_tests_execute`. That one
reads the artifact and needs no server. Expect `1 passed` there.

Zero writes is measured, not assumed. Inside the replay transaction the runner
asks `pg_current_xact_id_if_assigned()`. PostgreSQL assigns a transaction id
the first time a transaction writes a row. A NULL answer therefore covers every
table, and it still fails when an insert is cancelled by a later delete. The
first call asks the same question and must get an id back, so a detector stuck
at NULL fails there first.

The mutant arm rewrites one clause of the emitted
`command/inventory_move/claim_command.sql`. It turns
`ON CONFLICT ... DO NOTHING` into
`DO UPDATE SET canonical_command = EXCLUDED.canonical_command`. Both emitted
cases must then go red. The arm fails if either one still passes.

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

This gate renders runtime-operator 2.9.0 with three host profiles: generic,
Receiving/PAT, and WMS/PAT. It also renders the operator release and decodes the
manifests with Kubernetes' client-side loader. The host assertions cover registry
mounts, native memory and startup configuration, HTTP probes, termination grace,
and NetworkPolicy scope.

Receiving assertions preserve the full host profile, the base without a release,
and the trusted application scope. All four mandatory scoped Secret references
must reach the Pod. The gate also covers executor health and grace, the disabled
gateway, and the Events overlay's namespace and operator identity. It creates no
cluster object.

```bash
cargo test -p wamn-proof-conformance --test chart_seam_governance \
  receiving_pat_overlay_renders_a_complete_scoped_host \
  -- --ignored --exact --nocapture
```

The gate requires `helm` and `kubectl`, and Helm pulls chart 2.9.0.
A standalone render passed in the
[deployment-001 evidence](../perf/2026.09/wasmcloud-2-9-cutover/deployment-001/).
The updated Rust gate and live 2.9.0 proofs remain unexecuted.

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

The same gate covers human PATs and explicit project-environment membership
(`wamn-ctc8.19`). It calls the provisioning CLI handlers and the production
route reader. It tests scope isolation, repeated grants and revocations,
fresh role removal, disabled users, and the org-issued principal UUID.
Service PAT scope and permission tests remain in the gate.

Both paths use one prepared system query, then the unchanged tenant permission
query (`wamn-ctc8.12`). The system query reads the PAT and principal with the
service project role or the exact human environment membership. The host still
checks the full token, expiry, revocation, principal status, and expected service
identity. No database objects or grants change.

For an existing system database, install the `identity.project_env_memberships`
definition from `deploy/sql/system-schema.sql` as `wamn_system`.
Do not replay the complete bootstrap against an existing database.
Before hosts restart, reconcile the IdentityReader and HttpAdmitter grants
through the existing provisioning command.
The first gains membership `SELECT`. The second gains `SELECT` on
`app_system.users` and `app_system.user_roles`.

Use the existing org-issued human UUID when you grant or revoke membership:

```bash
wamn-ctl grant-project-env-membership --org acme --project receiving --env dev \
  --principal-id "$WAMN_HUMAN_PRINCIPAL_ID"
wamn-ctl revoke-project-env-membership --org acme --project receiving --env dev \
  --principal-id "$WAMN_HUMAN_PRINCIPAL_ID"
```

Both commands read the administrator URL from `WAMN_SYSTEM_ADMIN_URL`.
They create no users, roles, or tokens.

### `[MEMBERSHIP-HTTP]` — deployed human membership

This gate sends real HTTP requests through the operator-managed Receiving host
(`wamn-ctc8.19`). It uses the existing `wamn-gates membershipproof` command.
The journey builds both standard Dockerfile stages, `host` and `gates`, and
loads their unique tags into its disposable cluster.
It never addresses the frozen `wamn` cluster.

```bash
mkdir -p docs/operations/evidence
tools/receiving-cluster-journey-run --membershipproof --apply \
  --evidence-dir docs/operations/evidence/ctc8-19-membership-http
```

Start from a clean worktree. Use a new evidence directory for each run.
The proof mints one human PAT through the identity authority and grants
membership through the production CLI.
It requires seven responses: missing membership 401, grant 200, repeated grant
200, role removal 403, restored role 200, revocation 401, and repeated
revocation 401.
Each successful response must contain the expected purchase order and request
identifier. The proof never retries an authorization transition.

The journey retains the proof receipt, Job and Pod states, and image identities.
It deletes its disposable cluster, databases, and images through its existing
cleanup path. It performs no startup measurement or CDC materializer proof in
this mode.

The [2026-09-07 evidence](evidence/ctc8-19-premerge-20260907/README.md) preserves
both deployed runs and the workspace sweep at their recorded source commits.

### `[IDENTITY-JWKS]` public signing keys

This foundation gate covers `wamn-ctc8.15.1`.
The separate `wamn-identity` service exposes HTTPS JWKS and health routes.
With no session targets, it does not expose `/session`.
It never exposes `/authoring` or host execution.
Signing keys stay in the system database under a dedicated identity credential.
The gate observes public keys only and receives no database credential.

Run the deterministic token and HTTPS cache proofs:

```bash
cargo test --locked --offline -p wamn-platform-identity --test session_token
cargo test --locked --offline -p wamn-runtime --features test-util \
  --test session_keys -- --test-threads=1
```

Each live suite requires its own disposable PostgreSQL 18 server.
The URL must name `wamn_system`.
Do not use a shared server because the fixtures replace schemas and cluster-wide roles.
The grant fixtures also revoke the public database connection privilege.

```bash
WAMN_SESSION_KEYS_PG_URL="$WAMN_OWNED_PG18_URL" \
WAMN_SESSION_KEYS_ALLOW_SCHEMA_RESET=1 \
  cargo test --locked --offline -p wamn-platform-identity \
  --test session_keys_live -- --include-ignored --nocapture --test-threads=1
WAMN_IDENTITY_ISSUER_PG_URL="$WAMN_OWNED_PG18_URL" \
WAMN_IDENTITY_ISSUER_ALLOW_SCHEMA_RESET=1 \
  cargo test --locked --offline -p wamn-control-provision \
  --test identity_issuer_live -- --include-ignored --nocapture --test-threads=1
WAMN_IDENTITY_SERVICE_PG_URL="$WAMN_OWNED_PG18_URL" \
WAMN_IDENTITY_SERVICE_ALLOW_SCHEMA_RESET=1 \
  cargo test --locked --offline -p wamn-identity \
  --test https_surface -- --include-ignored --nocapture --test-threads=1
WAMN_IDENTITY_ISSUER_CLI_PG_URL="$WAMN_OWNED_PG18_URL" \
WAMN_IDENTITY_ISSUER_CLI_ALLOW_SCHEMA_RESET=1 \
  cargo test --locked --offline -p wamn-ctl \
  --test identity_issuer_live -- --include-ignored --nocapture --test-threads=1
```

Replace the disposable server and URL before each command.
The key proof covers committed publication, activation, signing locks, retirement, and compromised-key removal.
The credential proofs cover narrow grants, atomic Secret output, rollback, and A/B retirement.
The HTTPS proof uses the compiled identity CLI and the production listener.
The cache proofs use real TLS with controlled evidence clocks.
They cover complete replacement, removal, outages, prior HTTP Age, cancellation, fetch limits, and exact freshness deadlines.

Run the deployed gate from a clean worktree:

```bash
tools/identity-jwks-journey-run --apply \
  --evidence-dir docs/perf/2026.09/ctc8-15-1-identity/deployed-001
```

The journey builds the standard `identity` and `gates` Dockerfile stages.
It installs the actual identity chart in a disposable kind cluster.
Its test CA is local to that cluster and does not change production trust.
It retains image identities, Job verdicts, Pod states, and public key receipts.
It removes only its own cluster and temporary credentials.
The two-host session-token proof remains mandatory in `wamn-ctc8.15.3`.
Public-key cache tests do not establish host admission.

### `[SESSION-ROUTE-LIVE]` scoped session permissions

This local proof uses production route authentication with HTTPS keys and the provisioned `HttpAdmitter` credential.
It measures one tenant permission query per warm request through PostgreSQL statistics.
It covers role unions, tenant isolation, next-request permission removal, and evidence expiry during a blocked permission read.
It does not replace the deployed two-host or nested fresh-only proofs.

Use a fresh disposable PostgreSQL 18 server with the statistics extension loaded:

```bash
docker run -d --name wamn-session-route-pg -e POSTGRES_PASSWORD=probe \
  -p 127.0.0.1:5440:5432 postgres:18 \
  -c shared_preload_libraries=pg_stat_statements -c pg_stat_statements.track=all
psql postgres://postgres:probe@127.0.0.1:5440/postgres -Atqc 'select 1'
WAMN_SESSION_ROUTE_PG18_URL=postgres://postgres:probe@127.0.0.1:5440/postgres \
  cargo test --locked --offline -p wamn-runtime --features test-util \
  --test session_route_authentication -- --include-ignored --nocapture --test-threads=1
docker rm -f -v wamn-session-route-pg
```

Wait for the connection query to succeed before starting the test.
The test refuses a populated server and requires its environment variable.
The final command removes only the named test database and its temporary volume.

### `[HOST-SESSION-HTTP]` sessions on two hosts

This gate covers the two-host proof in `wamn-ctc8.15.3`.
It sends the same session token to two distinct host processes and compares each complete response with the expected purchase order.
The runner pins one workload to each ready host ID through the existing operator field.
The proof refuses any host-container restart during either five-minute window.
Each first host request has a 30-second allowance for cold compilation.
Identity requests retain their five-second limit.
Later host requests allow ten seconds, so a five-second key fetch can finish before the host returns its refusal.
After both hosts accept the token, the runner removes its signing key.
Both hosts must refuse it after the 300-second public-key window, while the token itself remains valid.
The runner repeats the proof with JWKS unreachable and new host processes.

The runner also traces a session call through the actual overlay and base components.
Both invocations must retain the original human identity and session credential kind.
This call uses disposable release 3 while the two hosts remain on release 2.
Both releases use manifest format 1.

Set `WAMN_SESSION_PROOF_EVIDENCE` to a new directory under the main checkout's `docs/perf/2026.09/ctc8-15-3-host-sessions/` directory.
Run the gate from a clean source worktree:

```bash
tools/receiving-cluster-journey-run --session-host-proof --apply \
  --evidence-dir "$WAMN_SESSION_PROOF_EVIDENCE"
```

Create the parent directory first.
Use a new evidence directory for each run.
Keep that directory outside the source worktree during the run.
The runner builds the standard `host`, `gates`, and `identity` Dockerfile stages.
It retains the host identities, image digests, Job results, and key-removal receipts.
The private token fixture stays in disposable storage, not in the retained evidence.
The runner removes its own cluster, database, images, and temporary credentials.
Normal package routes remain PAT-only until this proof and the fresh-only proof pass.

### Identity foundation rollout

For an existing database, install the two key tables as the `wamn_system` owner.
Use the `identity.session_keys` and `identity.session_signing_state` definitions from `deploy/sql/system-schema.sql`.
Do not replay the complete bootstrap against an existing database.
Before credential preparation, apply the existing cluster policy that removes public database connection privileges.
Remove public temporary-table privileges on `wamn_system`.
The provisioner refuses an open policy and does not modify unrelated databases.

Prepare the first issuer credential:

```bash
wamn-ctl provision-identity-issuer --issuer "$WAMN_IDENTITY_ISSUER" \
  --prepare-generation a --emit-secret /secure/identity-db.json
kubectl --context "$WAMN_CONTEXT" -n wamn-system apply -f /secure/identity-db.json
```

The command reads the administrator URL from `WAMN_SYSTEM_ADMIN_URL`.
It emits a scoped credential with a finite 30-day lifetime and mode 0600.
The service refuses an owner credential or a credential for another issuer before connection.
Credential rotation uses the existing A/B slots, not signing-key identifiers.
Before retiring the old slot, start the replacement service with the new credential.
The provisioner requires a live replacement connection and then drains the old connections.

Install the service with an existing trusted serving certificate:

```bash
helm upgrade --install wamn-identity deploy/platform/identity \
  --kube-context "$WAMN_CONTEXT" --namespace wamn-system \
  -f deploy/platform/values-identity-default.yaml \
  --set-string issuer="$WAMN_IDENTITY_ISSUER" \
  --set-string tlsSecret="$WAMN_IDENTITY_TLS_SECRET"
```

Configure the intended identity image tag in the deployment values before installation.
The chart requires HTTPS and operator-provided database and serving certificate Secrets.
It adds no ingress, service-account privilege, or trust root.
The default database example contains an unusable placeholder.

Publish a signing key through the deployed identity binary:

```bash
kubectl --context "$WAMN_CONTEXT" -n wamn-system exec deployment/wamn-identity \
  -- wamn-identity publish
kubectl --context "$WAMN_CONTEXT" -n wamn-system exec deployment/wamn-identity \
  -- wamn-identity activate --kid "$WAMN_PUBLISHED_KID"
```

Use the public `kid` from the publication result for activation.
Publication must commit before activation.
Rotation erases the old private key after existing signers drain.
Its public key remains available for 930 seconds after that cutoff.
The `retire` command removes expired generations.
The `remove --kid` command removes a compromised key immediately and selects no replacement.
Neither command creates a session record.

### `[IDENTITY-SESSION]` human PAT exchange

This gate covers `wamn-ctc8.15.2` on the separate identity service.
Configured audiences enable `POST /session` for human personal access tokens (PATs).
Each exchange checks the full PAT, exact environment membership, and current database instance.
It reads that environment's active user and roles.
The service signs a token without creating a session record.
These commands define the required checks, not a recorded passing result.

#### Deterministic checks

Run the target, credential, request, response, CLI, and chart checks:

```bash
cargo test --locked --offline -p wamn-control-provision --lib session_
cargo test --locked --offline -p wamn-control-provision --test session_target
cargo test --locked --offline -p wamn-ctl --lib session_reader
cargo test --locked --offline -p wamn-ctl --test session_audience_live \
  compiled_session_reader_refuses_invalid_tenants_before_io
cargo test --locked --offline -p wamn-identity --lib
cargo test --locked --offline -p wamn-proof-integration --lib identity_session_proof::tests
cargo test --locked --offline -p wamn-proof-system --test deploy_platform_inventory
```

The chart check requires Helm.
Keep the `[IDENTITY-JWKS]` token and public-key cache checks in the same validation set.

#### Disposable database checks

Each command requires a separate, fresh PostgreSQL 18 server and its administrator URL.
Never use a shared server.
These fixtures replace schemas, create databases, and change cluster-wide roles and public connection privileges.
The role-reader and issuer grant suites also require `psql`.
For the first command, create `wamn_session_role_reader_proof` and set `WAMN_OWNED_READER_PG18_URL` to its URL.
For each remaining command, create `wamn_system` on a new server and replace `WAMN_OWNED_SYSTEM_PG18_URL`.

```bash
WAMN_SESSION_ROLE_READER_PG_URL="$WAMN_OWNED_READER_PG18_URL" \
WAMN_SESSION_ROLE_READER_ALLOW_SCHEMA_RESET=1 \
  cargo test --locked --offline -p wamn-control-provision \
  --test session_role_reader_live -- --include-ignored --nocapture --test-threads=1
WAMN_IDENTITY_ISSUER_PG_URL="$WAMN_OWNED_SYSTEM_PG18_URL" \
WAMN_IDENTITY_ISSUER_ALLOW_SCHEMA_RESET=1 \
  cargo test --locked --offline -p wamn-control-provision \
  --test identity_issuer_live -- --include-ignored --nocapture --test-threads=1
WAMN_IDENTITY_ISSUER_CLI_PG_URL="$WAMN_OWNED_SYSTEM_PG18_URL" \
WAMN_IDENTITY_ISSUER_CLI_ALLOW_SCHEMA_RESET=1 \
  cargo test --locked --offline -p wamn-ctl \
  --test identity_issuer_live -- --include-ignored --nocapture --test-threads=1
WAMN_SESSION_AUDIENCE_CLI_PG_URL="$WAMN_OWNED_SYSTEM_PG18_URL" \
WAMN_SESSION_AUDIENCE_CLI_ALLOW_SCHEMA_RESET=1 \
  cargo test --locked --offline -p wamn-ctl \
  --test session_audience_live -- --include-ignored --nocapture --test-threads=1
WAMN_SESSION_EXCHANGE_PG_URL="$WAMN_OWNED_SYSTEM_PG18_URL" \
WAMN_SESSION_EXCHANGE_ALLOW_SCHEMA_RESET=1 \
WAMN_SESSION_EXCHANGE_CTL_BIN="$PWD/target/debug/wamn-ctl" \
  cargo test --locked --offline -p wamn-identity \
  --test session_exchange -- --include-ignored --nocapture --test-threads=1
```

Before the exchange suite, build its actual retirement CLI in the same worktree:

```bash
cargo build --locked --offline -p wamn-ctl --bin wamn-ctl
```

The CLI fixture creates `wamn-db-sessioncli--receiving--dev--k3m9x2p7` and `wamn-db-sessioncli--receiving--dev--p7x2m9k3`.
The exchange fixture creates three databases with suffix `s3ss10n2`:
`wamn-db-acme--receiving--dev--s3ss10n2`, `wamn-db-acme--receiving--prod--s3ss10n2`, and `wamn-db-other--receiving--dev--s3ss10n2`.
The grant checks cover the exact added columns, denied authority, and A/B credential lifecycle.
The CLI check covers bound Secret output, publication rollback, and retirement with a live replacement connection.
The HTTPS check covers changed membership, PAT status, tenant status, roles, and database incarnation on the next exchange.
It also checks malformed input, original validation time, bounded database waits, and unchanged persisted rows.

#### Deployed exchange gate

Run the deployed gate from a clean worktree with a new evidence directory:

```bash
tools/identity-jwks-journey-run --session-exchange --apply \
  --evidence-dir docs/perf/2026.09/ctc8-15-2-session-exchange/deployed-001
```

The runner builds the canonical `identity` and `gates` images.
It installs the actual chart on its own kind cluster.
It uses its own PostgreSQL 18 server and temporary test certificate authority.
All five foundation JWKS stages run before session targets are configured.
The session stages provision three audience Secrets through the actual CLI and mint fixture PATs through the production identity library.
The first Job checks signed development claims.
It rejects missing production membership, missing other-organization membership, and a service PAT.
After membership removal, the second Job checks refusal on the next exchange.
The observer receives only its PAT cases and public certificate authority, without database or signing credentials.
Its receipt ends with `IDENTITY_SESSION result=pass cases=<n> host_admission=not_proven` only after its checks pass.
Evidence excludes raw PATs, signing keys, and database credentials.
The runner removes only its own cluster, images, and temporary credentials.
This gate does not prove host session admission, fresh-only operation enforcement, or the two-host proof in `wamn-ctc8.15.3`.

#### Session target rollout

Keep the foundation issuer, signing-key, serving-certificate, and public database privilege prerequisites above.
Existing issuer credentials require convergence to the expanded column grants defined by `IDENTITY_ISSUER_READ_COLUMNS` before exchanges are enabled.
The updated `provision-identity-issuer` command accepts the exact foundation grants during preparation and checks the expanded grants before commit.
Those reads cover principals, PAT verification fields, exact environment memberships, and registry environment instances.
The two signing-key tables retain their existing grants.
Do not widen host or authoring Gate credentials.

If issuer generation A is active, prepare generation B with the updated CLI:

```bash
wamn-ctl provision-identity-issuer --issuer "$WAMN_IDENTITY_ISSUER" \
  --prepare-generation b --emit-secret /secure/identity-db.json
```

Apply the replacement issuer Secret and follow the foundation service restart and issuer retirement procedure above.

Register and provision the project environment before preparing its session reader credential.
Grant the human explicit environment membership and provide that database's active tenant user and roles before exchange.
Supply `WAMN_SYSTEM_ADMIN_URL` and `WAMN_TARGET_ADMIN_DATABASE_URL` through the provisioning process's protected environment.
The target administrator URL must name the exact physical database derived from the stored registry instance suffix.
Keep administrator URLs out of command arguments, logs, and deployment values.
Set `WAMN_SESSION_TENANT` to the explicit trusted tenant identifier for that database.
Tenant identifiers use the existing text rule, which accepts `t1`.
They are not inferred from the caller or registry.

```bash
wamn-ctl provision-project-env --org "$WAMN_ORG" --project "$WAMN_PROJECT" \
  --env "$WAMN_ENV" --tenant "$WAMN_SESSION_TENANT" --namespace wamn-system \
  --prepare-session-role-reader-generation a \
  --emit-session-role-reader-secret /secure/session-target.json
kubectl --context "$WAMN_CONTEXT" -n wamn-system apply -f /secure/session-target.json
```

The CLI emits a mode-0600 Secret file containing `stringData["target.json"]` through atomic publication.
That document binds the organization, project, environment, stored instance suffix, tenant, physical database, and dedicated reader generation URL.
Its audience is `urn:wamn:project-env:<org>:<project>:<env>:<instance_suffix>`.
The `SessionRoleReader` credential reads only `users(tenant_id,id,status)` and `user_roles(tenant_id,user_id,role_name)` in `app_system`.
It cannot write roles or read signing keys.

Set `WAMN_SESSION_TARGET_SECRET` to the emitted Secret name.
Mount each prepared target Secret explicitly in the identity chart:

```bash
helm upgrade --install wamn-identity deploy/platform/identity \
  --kube-context "$WAMN_CONTEXT" --namespace wamn-system \
  -f deploy/platform/values-identity-default.yaml \
  --set-string issuer="$WAMN_IDENTITY_ISSUER" \
  --set-string tlsSecret="$WAMN_IDENTITY_TLS_SECRET" \
  --set-string "sessionTargetSecrets[0]=$WAMN_SESSION_TARGET_SECRET"
```

Repeat the indexed `sessionTargetSecrets` entry for each distinct audience.
All configured organizations share the same exact HTTPS issuer URL.
The service reads mounted files at startup and has no Kubernetes discovery authority.
An empty `sessionTargetSecrets` list disables `/session`.
Keep signing credentials and target Secrets out of host and authoring Gate deployments.

#### Reader credential rotation

The service retains one scoped database connection for each active configured environment.
It opens that connection only after valid PAT, membership, and current-instance checks.
Each exchange still reads the environment's user and roles afresh.
Restart identity after replacing its reader credential Secret so it reads the new target file.
Do not mount both generation documents for the same audience.

If reader generation A is active, prepare and install generation B:

```bash
wamn-ctl provision-project-env --org "$WAMN_ORG" --project "$WAMN_PROJECT" \
  --env "$WAMN_ENV" --tenant "$WAMN_SESSION_TENANT" --namespace wamn-system \
  --prepare-session-role-reader-generation b \
  --emit-session-role-reader-secret /secure/session-target.json
kubectl --context "$WAMN_CONTEXT" -n wamn-system apply -f /secure/session-target.json
kubectl --context "$WAMN_CONTEXT" -n wamn-system rollout restart deployment/wamn-identity
kubectl --context "$WAMN_CONTEXT" -n wamn-system rollout status deployment/wamn-identity --timeout=180s
```

Before retirement, complete one successful human PAT exchange for the rotated audience through the replacement identity process.
Keep that process running while the provisioner retires A:

```bash
wamn-ctl provision-project-env --org "$WAMN_ORG" --project "$WAMN_PROJECT" \
  --env "$WAMN_ENV" --tenant "$WAMN_SESSION_TENANT" \
  --retire-session-role-reader-generation a
```

The existing retirement rule requires a live replacement database connection and drains the old generation's connections.
An idle restart alone does not satisfy that rule.

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

**The Gate is a spawned `wamn-scenario-worker serve` child, not a task inside
the proof** (`wamn-10yt.10.32`). The proof takes the built binary as
`WAMN_JOURNEY_SCENARIO_WORKER_BIN` and spawns it on the fixed loopback port
`127.0.0.1:18088`; readiness is a bounded TCP connect against that port, and it
refuses by naming it. A spawned child cannot hand an ephemeral port back the
way the old in-process launch did, so the port is fixed and must be free. This
is the same launch path `wamn dev up` uses, and there is no longer a second,
in-process one.

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
WAMN_DEV_LIVE_DOCKER_AUTH="$WAMN_DEV_LIVE_SCRATCH/docker/config.json"
mkdir -m 0700 -- "$WAMN_DEV_LIVE_SCRATCH/docker"
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
  cargo build -p wamn-scenario-worker --locked --offline
RUSTC_WRAPPER= CARGO_TARGET_DIR="$WAMN_DEV_LIVE_TARGET" \
  cargo build --manifest-path "$WAMN_DEV_LIVE_ROOT/components/Cargo.toml" \
    -p http-route --target wasm32-wasip2 --locked --offline
WAMN_DEV_LIVE_BIN="$WAMN_DEV_LIVE_TARGET/debug/wamn"
WAMN_DEV_LIVE_HOST_BIN="$WAMN_DEV_LIVE_TARGET/debug/wamn-host"
WAMN_DEV_LIVE_GATE_BIN="$WAMN_DEV_LIVE_TARGET/debug/wamn-scenario-worker"
WAMN_DEV_LIVE_FLOW_HTTP="$WAMN_DEV_LIVE_TARGET/wasm32-wasip2/debug/http_route.wasm"
test -x "$WAMN_DEV_LIVE_BIN"
test -x "$WAMN_DEV_LIVE_HOST_BIN"
test -x "$WAMN_DEV_LIVE_GATE_BIN"
test -s "$WAMN_DEV_LIVE_FLOW_HTTP"
# The spawned Gate binds this exact port; a stray listener is a hard failure,
# not a fallback to an ephemeral one.
test -z "$(ss -Hltn 'sport = :18088')"

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

wamn_wash=$(tools/install-wash)
DOCKER_CONFIG="$(dirname "$WAMN_DEV_LIVE_DOCKER_AUTH")" \
  "$wamn_wash" oci push "$WAMN_DEV_LIVE_FLOW_HTTP_IMAGE" "$WAMN_DEV_LIVE_FLOW_HTTP" \
    --insecure
unset WAMN_DEV_LIVE_PASSWORD

CARGO_TARGET_DIR="$WAMN_DEV_LIVE_TARGET" \
RUSTC_WRAPPER= \
WAMN_ROUTE_PG18_URL="postgresql://postgres:probe@127.0.0.1:${WAMN_DEV_LIVE_PG_PORT}/postgres" \
WAMN_RECEIVING_DEV_BIN="$WAMN_DEV_LIVE_BIN" \
WAMN_RECEIVING_DEV_HOST_BIN="$WAMN_DEV_LIVE_HOST_BIN" \
WAMN_JOURNEY_SCENARIO_WORKER_BIN="$WAMN_DEV_LIVE_GATE_BIN" \
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
provable and not startable (`wamn-10yt.10.30`). `wamn dev up` runs the same
standup module the gate runs — `services/ctl/src/dev/environment.rs`, whose
only job is to build the arguments the platform verbs take — writes the strict
`dev.json`, and then holds the authoring Gate open on a nameable port for as
long as it runs. It is not a gate: it emits no receipt, and its evidence is that
`wamn dev` starts against what it left behind.

**The standup is `wamn dev up` in the `wamn` binary.** It was a separate
`wamn-dev-env` binary until `wamn-10yt.10.32` moved the flags, the standup
module and the spawned Gate into the product command;
`tests/integration/src/bin/wamn-dev-env.rs` is now a compatibility shim that
parses the same arguments and hands them straight over, and its own doc comment
says to delete it with this recipe. This recipe no longer builds or calls it,
so nothing here keeps it alive — retiring the shim itself is `wamn-10yt.24`.

**Not every verb lives in `wamn`.** `wamn` carries `dev` and `dev up` and
nothing else. The operations verbs — `apply-package`,
`reconcile-package-data-access` and the rest of the provisioning surface — are
subcommands of the separate `wamn-ctl` binary.

Point it only at disposable services. Standup resets the control store, so every
run is a fresh start; never point it at shared infrastructure or the frozen
cluster. Minted PATs and credential URLs live only in the mode-0700 environment
directory and are never printed.

`wamn dev` runs from a dirty worktree, and that is the point of the loop
(`wamn-10yt.43`). It recreates its target database before every run. It pushes
to the registry the session owns. Nothing it deploys outlives the session, so
the committed-source refusal has nothing to protect. Point a run at a durable
target and every one of those refusals returns, because the condition is the
target and not the stage.

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
WAMN_DEV_ENV_DOCKER_AUTH="$WAMN_DEV_ENV_SCRATCH/docker/config.json"
mkdir -m 0700 -- "$WAMN_DEV_ENV_SCRATCH/docker"
WAMN_DEV_ENV_FLOW_HTTP_IMAGE="$WAMN_DEV_ENV_AUTHORITY/wamn/flow-http:dev"

RUSTC_WRAPPER= CARGO_TARGET_DIR="$WAMN_DEV_ENV_TARGET" \
  cargo build -p wamn-ctl --bin wamn --locked --offline
RUSTC_WRAPPER= CARGO_TARGET_DIR="$WAMN_DEV_ENV_TARGET" \
  cargo build -p wamn-host --locked --offline
RUSTC_WRAPPER= CARGO_TARGET_DIR="$WAMN_DEV_ENV_TARGET" \
  cargo build -p wamn-scenario-worker --locked --offline
RUSTC_WRAPPER= CARGO_TARGET_DIR="$WAMN_DEV_ENV_TARGET" \
  cargo build --manifest-path "$WAMN_DEV_ENV_TREE/components/Cargo.toml" \
    -p http-route --target wasm32-wasip2 --locked --offline
WAMN_DEV_ENV_FLOW_HTTP="$WAMN_DEV_ENV_TARGET/wasm32-wasip2/debug/http_route.wasm"
test -x "$WAMN_DEV_ENV_TARGET/debug/wamn"
test -x "$WAMN_DEV_ENV_TARGET/debug/wamn-host"
test -x "$WAMN_DEV_ENV_TARGET/debug/wamn-scenario-worker"
# The spawned Gate binds a FIXED port, so the recipe refuses rather than
# colliding. `wamn dev up --gate-bind` names it and defaults to
# 127.0.0.1:8088; override the variable to run a second environment beside
# this one.
WAMN_DEV_ENV_GATE_PORT="${WAMN_DEV_ENV_GATE_PORT:-8088}"
test -z "$(ss -Hltn "sport = :${WAMN_DEV_ENV_GATE_PORT}")"
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

wamn_wash=$(tools/install-wash)
DOCKER_CONFIG="$(dirname "$WAMN_DEV_ENV_DOCKER_AUTH")" \
  "$wamn_wash" oci push "$WAMN_DEV_ENV_FLOW_HTTP_IMAGE" "$WAMN_DEV_ENV_FLOW_HTTP" --insecure
unset WAMN_DEV_ENV_PASSWORD

"$WAMN_DEV_ENV_TARGET/debug/wamn" dev up \
  --system-database-url "postgresql://postgres:probe@127.0.0.1:${WAMN_DEV_ENV_PG_PORT}/postgres" \
  --root "$WAMN_DEV_ENV_DIR" \
  --gate-bind "127.0.0.1:${WAMN_DEV_ENV_GATE_PORT}" \
  --nats-url "nats://127.0.0.1:${WAMN_DEV_ENV_NATS_PORT}" \
  --tempo-query-url "http://127.0.0.1:${WAMN_DEV_ENV_TEMPO_PORT}" \
  --otel-exporter-otlp-endpoint "http://127.0.0.1:${WAMN_DEV_ENV_OTLP_PORT}" \
  --component-artifact-base "$WAMN_DEV_ENV_AUTHORITY/wamn/components" \
  --release-artifact-base "$WAMN_DEV_ENV_AUTHORITY/wamn/releases" \
  --registry-auth-file "$WAMN_DEV_ENV_DOCKER_AUTH" \
  --route-host receiving.localhost \
  --flow-http-workload-image "$WAMN_DEV_ENV_FLOW_HTTP_IMAGE" \
  --host-binary "$WAMN_DEV_ENV_TARGET/debug/wamn-host" \
  --scenario-worker-binary "$WAMN_DEV_ENV_TARGET/debug/wamn-scenario-worker" \
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

### `[RECEIVING-TUI]` — using the environment the loop serves

Not a gate. It is the whole path from a clean checkout to an operator holding
the Receiving terminal against a release `wamn dev` is serving right now, and
it is also the answer to "I ran the loop; what do I do with it" — where the URL
comes from, where the operator credential lands, how to call the routes with or
without the terminal, and why promotion to a durable environment is a separate
act (`wamn-10yt.5.6`, `wamn-10yt.46`).

`[WAMN-DEV-ENVIRONMENT]` stands the environment up and stops there. This one
runs `wamn dev up`, holds one loop run open with `--hold`, and drives
`wamn-receiving` against it. Every step below was executed in this order and
the outputs quoted are the ones the run printed.

**Every port is a variable with a default, and the whole recipe is
re-entrant on a fresh cluster,** so two people can run it at once by exporting
a second set. The Gate port is the one that is not negotiable per-process:
`wamn dev up --gate-bind` names it, the configuration written outlives the
process that wrote it, and a stray listener there is a hard failure.

The `wamn` binary carries `dev`, `dev up`, and `ui scaffold`.
Bare `--tui` opens the developer console.
`--tui <package>` opens that package's generated operator terminal and holds
activation until the operator exits, without `--hold`.
Without `--tui`, `--hold` retains its existing meaning.

The loop records host output in a private file beside the Wasmtime cache.
The log directory uses the cache path with `.operator-logs` appended.
It prints the path before it opens the operator terminal.

After an emitter source change, rebuild `wamn` and restart `wamn dev`.
The running process uses the emitter linked into its binary.

This recipe uses the existing `wamn-receiving` terminal.
The loop starts and supervises `wamn-host` and `wamn-scenario-worker`.

The operations verbs — `apply-package`,
`reconcile-package-data-access` and the rest of the provisioning surface — are
subcommands of `wamn-ctl`, a different binary, and typing one after `wamn` gets
an unrecognized-subcommand error.

**Two target directories, on purpose.** The workspace binaries build into the
tree's own `target/`. `components/Cargo.toml` is a SEPARATE workspace, so
`http_route.wasm` lands under `components/target/`, not `target/`, and the
push below names that path. The loop's own Build stage runs
`tools/build-components`, which uses the same two directories, so a run costs
one component build and not two.

#### 1. Ports, names and paths

```bash
set -euo pipefail
umask 077

WAMN_TUI_TREE="$(pwd -P)"
test "$(git -C "$WAMN_TUI_TREE" rev-parse --show-toplevel)" = "$WAMN_TUI_TREE"
test -f "$WAMN_TUI_TREE/packages/receiving/wamn.json"
command -v wash >/dev/null

WAMN_TUI_ROOT="${WAMN_TUI_ROOT:-${TMPDIR:-/tmp}/wamn-receiving-tui}"
WAMN_TUI_PROJECT="${WAMN_TUI_PROJECT:-wamn-receiving-tui}"
WAMN_TUI_PG_PORT="${WAMN_TUI_PG_PORT:-54344}"
WAMN_TUI_STD_REGISTRY_PORT="${WAMN_TUI_STD_REGISTRY_PORT:-5007}"
WAMN_TUI_REGISTRY_PORT="${WAMN_TUI_REGISTRY_PORT:-5008}"
WAMN_TUI_NATS_PORT="${WAMN_TUI_NATS_PORT:-4228}"
WAMN_TUI_TEMPO_PORT="${WAMN_TUI_TEMPO_PORT:-3205}"
WAMN_TUI_OTLP_PORT="${WAMN_TUI_OTLP_PORT:-4323}"
WAMN_TUI_GATE_PORT="${WAMN_TUI_GATE_PORT:-8092}"

WAMN_TUI_COMPOSE="$WAMN_TUI_TREE/test-support/infrastructure/std-virtualization.compose.yaml"
WAMN_TUI_TARGET="$WAMN_TUI_TREE/target"
WAMN_TUI_FLOW_HTTP="$WAMN_TUI_TREE/components/target/wasm32-wasip2/debug/http_route.wasm"
WAMN_TUI_AUTHORITY="127.0.0.1:${WAMN_TUI_REGISTRY_PORT}"
WAMN_TUI_USERNAME=wamn-receiving-tui
WAMN_TUI_HTPASSWD="$WAMN_TUI_ROOT/htpasswd"
WAMN_TUI_DOCKER_AUTH="$WAMN_TUI_ROOT/docker/config.json"
mkdir -m 0700 -- "$WAMN_TUI_ROOT/docker"
WAMN_TUI_FLOW_HTTP_IMAGE="$WAMN_TUI_AUTHORITY/wamn/flow-http:dev"
WAMN_TUI_ENV_DIR="$WAMN_TUI_ROOT/environment"
WAMN_TUI_UP_LOG="$WAMN_TUI_ROOT/dev-up.log"
WAMN_TUI_RUN_LOG="$WAMN_TUI_ROOT/dev-run.log"

mkdir -p "$WAMN_TUI_ROOT"
chmod 700 "$WAMN_TUI_ROOT"
# The Gate binds this exact port. A stray listener is a hard failure, not a
# fallback to an ephemeral one.
test -z "$(ss -Hltn "sport = :${WAMN_TUI_GATE_PORT}")"
```

`$WAMN_TUI_ROOT` holds minted PATs and password-bearing URLs, which is why it
is mode 0700 and why nothing below prints its contents. `[WAMN-DEV-ENVIRONMENT]`
keeps its scratch under `$XDG_CACHE_HOME` because a cold `CARGO_TARGET_DIR`
there is several gigabytes and `/tmp` is a tmpfs on this machine. That reason
does not apply here — the builds go to the tree's own `target/`, and what lands
in `$WAMN_TUI_ROOT` is one configuration document, a handful of Secrets, some
SQL, and the Wasmtime cache.

#### 2. Build

Debug, locked, offline. Four commands, because `--bin` selects across every
`-p` given to one invocation and the components live in another workspace.

```bash
RUSTC_WRAPPER= cargo build -p wamn-ctl --bin wamn --locked --offline
RUSTC_WRAPPER= cargo build -p wamn-host -p wamn-scenario-worker --locked --offline
RUSTC_WRAPPER= cargo build -p wamn-receiving-tui --bin wamn-receiving --locked --offline
RUSTC_WRAPPER= cargo build --manifest-path "$WAMN_TUI_TREE/components/Cargo.toml" \
  -p http-route --target wasm32-wasip2 --locked --offline

test -x "$WAMN_TUI_TARGET/debug/wamn"
test -x "$WAMN_TUI_TARGET/debug/wamn-host"
test -x "$WAMN_TUI_TARGET/debug/wamn-scenario-worker"
test -x "$WAMN_TUI_TARGET/debug/wamn-receiving"
test -s "$WAMN_TUI_FLOW_HTTP"
```

#### 3. Substrate

Disposable PostgreSQL 18, an authenticated loopback registry, NATS and Tempo.
Point this only at throwaway services; never at shared infrastructure or the
frozen cluster.

```bash
WAMN_TUI_PASSWORD="$(openssl rand -hex 32)"
printf '%s\n' "$WAMN_TUI_PASSWORD" \
  | docker run --rm -i --entrypoint htpasswd httpd:2-alpine \
      -Bni "$WAMN_TUI_USERNAME" >"$WAMN_TUI_HTPASSWD"
jq -n --arg authority "$WAMN_TUI_AUTHORITY" \
  --arg username "$WAMN_TUI_USERNAME" \
  --arg password "$WAMN_TUI_PASSWORD" \
  '{auths:{($authority):{username:$username,password:$password}}}' \
  >"$WAMN_TUI_DOCKER_AUTH"

export WAMN_STD_VIRT_PG_PORT="$WAMN_TUI_PG_PORT"
export WAMN_STD_VIRT_REGISTRY_PORT="$WAMN_TUI_STD_REGISTRY_PORT"
export WAMN_ROUTE_REGISTRY_PORT="$WAMN_TUI_REGISTRY_PORT"
export WAMN_ROUTE_REGISTRY_HTPASSWD="$WAMN_TUI_HTPASSWD"
export WAMN_RECEIVING_DEV_NATS_PORT="$WAMN_TUI_NATS_PORT"
export WAMN_RECEIVING_DEV_TEMPO_PORT="$WAMN_TUI_TEMPO_PORT"
export WAMN_RECEIVING_DEV_OTLP_PORT="$WAMN_TUI_OTLP_PORT"
docker compose --profile receiving-route -p "$WAMN_TUI_PROJECT" \
  -f "$WAMN_TUI_COMPOSE" up --detach --wait --wait-timeout 90 \
  receiving-route-postgres authenticated-registry receiving-dev-nats receiving-dev-tempo

# Tempo reports healthy to Compose before /ready answers; loop on /ready.
for _ in $(seq 1 60); do
  curl --fail --silent "http://127.0.0.1:${WAMN_TUI_TEMPO_PORT}/ready" >/dev/null && break
  sleep 1
done
curl --fail --silent "http://127.0.0.1:${WAMN_TUI_TEMPO_PORT}/ready" >/dev/null
PGPASSWORD=probe psql \
  "postgresql://postgres@127.0.0.1:${WAMN_TUI_PG_PORT}/postgres" -Atqc 'select 1' >/dev/null
test "$(curl --silent --output /dev/null --write-out '%{http_code}' \
  "http://${WAMN_TUI_AUTHORITY}/v2/")" = 401

wamn_wash=$(tools/install-wash)
DOCKER_CONFIG="$(dirname "$WAMN_TUI_DOCKER_AUTH")" \
  "$wamn_wash" oci push "$WAMN_TUI_FLOW_HTTP_IMAGE" "$WAMN_TUI_FLOW_HTTP" --insecure
unset WAMN_TUI_PASSWORD
```

#### 4. Terminal 1 — `wamn dev up`

```bash
"$WAMN_TUI_TARGET/debug/wamn" dev up \
  --system-database-url "postgresql://postgres:probe@127.0.0.1:${WAMN_TUI_PG_PORT}/postgres" \
  --root "$WAMN_TUI_ENV_DIR" \
  --scenario-worker-binary "$WAMN_TUI_TARGET/debug/wamn-scenario-worker" \
  --gate-bind "127.0.0.1:${WAMN_TUI_GATE_PORT}" \
  --nats-url "nats://127.0.0.1:${WAMN_TUI_NATS_PORT}" \
  --tempo-query-url "http://127.0.0.1:${WAMN_TUI_TEMPO_PORT}" \
  --otel-exporter-otlp-endpoint "http://127.0.0.1:${WAMN_TUI_OTLP_PORT}" \
  --component-artifact-base "${WAMN_TUI_AUTHORITY}/wamn/components" \
  --release-artifact-base "${WAMN_TUI_AUTHORITY}/wamn/releases" \
  --registry-auth-file "$WAMN_TUI_DOCKER_AUTH" \
  --route-host receiving.localhost \
  --flow-http-workload-image "$WAMN_TUI_FLOW_HTTP_IMAGE" \
  --host-binary "$WAMN_TUI_TARGET/debug/wamn-host" \
  --package "$WAMN_TUI_TREE/packages/receiving" \
  --overlay-root "$WAMN_TUI_TREE/packages/client_acme_receiving" \
  2>&1 | tee "$WAMN_TUI_UP_LOG"
```

It provisions, spawns the Gate, and holds. After about forty lines of
provisioning it prints, verbatim:

```
environment ready
  gate:   http://127.0.0.1:8092/authoring
  config: /tmp/wamn-receiving-tui/environment/dev.json

run the loop from the repository root, in another terminal:
  wamn dev --config /tmp/wamn-receiving-tui/environment/dev.json --overlay-root …/packages/client_acme_receiving --tui

this process holds the Gate; stop it with Ctrl-C when the loop is done
```

**Where the credentials land, exactly.** Two PATs are minted, and they are not
interchangeable.

- The **operator** credential — the one a client presents on a published route
  — is `$WAMN_TUI_ENV_DIR/route-caller-pat.json`, at `.stringData.token`. It is
  also in `dev.json` as `operator_bearer_token` for generated operator launch.
- `dev.json` carries `gate_bearer_token`, and that is the **management-author**
  PAT the loop presents to the authoring Gate. Handing it to the operator
  client is a different principal with a different project role.

Both files are Kubernetes `Secret` documents, mode 0700 directory, never
printed. The provisioning log names each as it writes it (`wrote
…/route-caller-pat.json (route-caller PAT Secret; kubectl apply)`), and
since `wamn-10yt.56` the `environment ready` summary carries the operator
path on its `pat:` line. The management-author PAT is not in the summary;
read that one from `dev.json`.

#### 5. Terminal 2 — one run, held

`wamn dev` runs from a dirty worktree by design and recreates its target
database before every run, so nothing here needs a clean checkout
(`wamn-10yt.43`). `--hold` keeps the activated release reachable until the
process is interrupted.

```bash
"$WAMN_TUI_TARGET/debug/wamn" dev \
  --config "$WAMN_TUI_ENV_DIR/dev.json" \
  --overlay-root "$WAMN_TUI_TREE/packages/client_acme_receiving" \
  --hold 2>&1 | tee "$WAMN_TUI_RUN_LOG"
```

Four lines are printed before the hold begins, and a scripted caller reads them
by shape:

```
run completed: migrate,introspect,generate,build,virtualize,apply,acl,admit,gate,publish,release,activate
run stage-ms: prepare=1523ms migrate=1177ms introspect=501ms generate=632ms build=89942ms virtualize=57187ms apply=654ms acl=451ms admit=12375ms gate=379ms publish=1595ms release=1101ms activate=23134ms
run served: http://127.0.0.1:37085 host=receiving.localhost
run holding
```

`run served` is the only place the base URL exists: the host binds
`127.0.0.1:0` and the kernel picks the port, so it differs every run and
nothing else records it. A second run may add `run skipped: unchanged generate`
when no authored byte changed, and `run pin stale: …` when the run built past
an authored base pin; neither changes the shape of `completed` or `served`.
The timings above are one cold run on an eight-core machine — Build and
Virtualize dominate, and both fall to near zero once the component target is
warm.

Read the endpoint and the operator PAT out in the third terminal:

```bash
WAMN_TUI_BASE_URL="$(awk '/^run served: /{print $3}' "$WAMN_TUI_RUN_LOG")"
WAMN_TUI_ROUTE_HOST="$(awk '/^run served: /{sub(/^host=/,"",$4); print $4}' "$WAMN_TUI_RUN_LOG")"
WAMN_TUI_OPERATOR_PAT="$(jq -r .stringData.token "$WAMN_TUI_ENV_DIR/route-caller-pat.json")"
test -n "$WAMN_TUI_BASE_URL" && test -n "$WAMN_TUI_ROUTE_HOST"
```

#### 6. Seed the two purchase orders

A run clones a pristine template, so the target database holds the package's
schema and no business rows. The Receiving package publishes no operation that
creates a purchase order — `purchase_order` has `get`, `query` and `update`
only — so the fixture is inserted directly, exactly as the route journey seeds
it. `target_database_url` in `dev.json` is the admin URL of the database the
run just built.

```bash
psql "$(jq -r .target_database_url "$WAMN_TUI_ENV_DIR/dev.json")" -v ON_ERROR_STOP=1 -q <<'SQL'
INSERT INTO receiving.item (id, item_number) VALUES
  ('00000000-0000-0000-0000-000000000101', 'ITEM-101');
INSERT INTO receiving.location (id, location_code) VALUES
  ('00000000-0000-0000-0000-000000000201', 'DOCK-1'),
  ('00000000-0000-0000-0000-000000000202', 'DOCK-2');
INSERT INTO receiving.purchase_order
  (id, purchase_order_number, supplier_id, status, row_version, created_at, updated_at)
VALUES
  ('00000000-0000-0000-0000-000000000301', 'PO-301',
   '00000000-0000-0000-0000-000000000401', 'open', 1,
   '2026-08-31T12:00:00.000000Z', '2026-08-31T12:00:00.000000Z'),
  ('00000000-0000-0000-0000-000000000302', 'PO-302',
   '00000000-0000-0000-0000-000000000402', 'open', 1,
   '2026-08-31T12:01:00.000000Z', '2026-08-31T12:01:00.000000Z');
INSERT INTO receiving.purchase_order_line
  (id, purchase_order_id, line_number, item_id, ordered_quantity, received_quantity)
VALUES
  ('00000000-0000-0000-0000-000000000501',
   '00000000-0000-0000-0000-000000000301', 1,
   '00000000-0000-0000-0000-000000000101', 5.0000, 0.0000),
  ('00000000-0000-0000-0000-000000000502',
   '00000000-0000-0000-0000-000000000302', 1,
   '00000000-0000-0000-0000-000000000101', 7.0000, 0.0000);
SQL
```

Seed AFTER the run, not before: the loop drops and re-clones the target at the
start of every run, so rows inserted first are gone by the time it serves.

#### 6b. The bindings the run just emitted

Generate writes a Rust client beside each package's contracts
(`wamn-10yt.45`). It is a generated artifact like any other, so it is part of
the set materialization owns and refuses to find anything else in:

```
packages/receiving/generated/client/{location,purchase_order,receipt,receiving}.rs
packages/client_acme_receiving/generated/client/{purchase_order,quality,receiving}.rs
```

Each module carries its model's field descriptors, a request and a result type
per operation, the operation's grant and its typed refusals, a `*_route()`
returning the method and template the RELEASE publishes, and an invoke function
over `wamn-client`. No base URL and no host: those are the caller's deployment
config, which is why this recipe reads them from `run served` and the PAT file.

`wamn-receiving` is built from those files. It declares them with `#[path]`
rather than copying route strings, so an operation the release republishes
elsewhere moves in the client by regeneration alone. Rebuilding the frontend
after a run is therefore just:

```bash
RUSTC_WRAPPER= cargo build -p wamn-receiving-tui --bin wamn-receiving --locked --offline
```

Generate is skipped when no authored byte under any package root changed, and
the digest excludes `generated/` — so a skipped Generate leaves the previous
bindings in place, which is correct, because a skipped Generate also means the
release did not change. A run that skips prints `run skipped: unchanged
generate` on its own line, and a run that regenerates does not.

#### 7. Terminal 3 — the operator terminal

`wamn-receiving` takes the whole deployment from three environment variables
and compiles none of it in.

```bash
WAMN_BASE_URL="$WAMN_TUI_BASE_URL" \
WAMN_HOST="$WAMN_TUI_ROUTE_HOST" \
WAMN_TOKEN="$WAMN_TUI_OPERATOR_PAT" \
  "$WAMN_TUI_TARGET/debug/wamn-receiving"
```

It opens on the purchase-order list, which it has already loaded:

```
purchase_order_number status row_version
>PO-301               open   1
PO-302                open   1

2 rows  complete
```

Keys, in the order one receipt needs them:

| key | list screen | receipt screen |
| --- | --- | --- |
| `Up` / `Down` | move the highlight | move the highlighted line |
| `Enter` | open the highlighted order's receipt | — |
| digits and `.` | — | type into the focused entry |
| `Backspace` | — | delete from the quantity |
| `Tab` | — | move focus between quantity and reference |
| `Ctrl-L` | — | cycle the receiving location |
| `Ctrl-S` | — | send the receipt |
| `Esc` | quit | back to the list |
| `q` | quit | types a character; use `Esc` or `Ctrl-Q` |
| `Ctrl-C` / `Ctrl-Q` | quit | quit |

A character follows the FOCUS and not its own shape, because a receipt
reference carries digits too. One worked receipt — `Down`, `Enter`, `3`, `Tab`,
`GRN-TUI-1`, `Ctrl-L`, `Ctrl-S` — clears the entry, returns to the list, and
leaves the verdict on the status line:

```
ok  recorded receipt 667e925b-1fcd-4dda-af2a-0d8cde5119e3
```

A success SPENDS the entry on purpose: quantities left on screen after a
recorded receipt are how the same receipt gets submitted twice.

#### 8. Testing the served environment without the terminal

The same three values drive `curl`. The route host is a header, not DNS.

```bash
curl --silent --show-error --fail-with-body \
  -H "Host: $WAMN_TUI_ROUTE_HOST" \
  -H "Authorization: Bearer $WAMN_TUI_OPERATOR_PAT" \
  -H 'Content-Type: application/json' \
  --data '[{"request_id":"probe-1"}]' \
  "$WAMN_TUI_BASE_URL/purchase_order/query" | jq .
```

**Three envelope facts that a client gets wrong once each.** All three were
measured against this served release, and each one fails silently or refuses
before any operation runs:

- A `page` result spells its rows **`item`** and its continuation
  **`next_cursor`**. A `bounded_list` result spells its rows **`rows`** and has
  no continuation. Reading `rows` off a page finds nothing and renders an empty
  list with no error.
- `int64` is a JSON **string** (`"row_version":"1"`); `int32` is a JSON number
  (`"line_number":1`). A 64-bit integer does not survive every JSON reader, so
  the platform carries it lexically.
- `numeric` is a JSON **string** on the way in as well as out. The
  canonicalization is `postgresql_lexical_scale_preserved` and scale is the
  reason: a JSON number cannot carry `5.0000`. The published wiring's input
  schema spells `value.line[].quantity` `{"type": "string"}`, and a number is
  refused at ingress with `{"error":{"code":"schema-invalid"}}`.

The overlay publishes its own routes beside the base ones — `/purchase_order/get`
is the base package's and `/acme/purchase_order/get` is the overlay's — so a
client that derived a path from an operation name would call the wrong one.

#### 9. Promotion is a separate act, taken after commit

Nothing this loop serves outlives the session. The target database is
disposable, the registry is the session's own, and the committed-source
refusals that guard provenance are therefore inert:
`refuses_dirty_source` in `services/ctl/src/dev.rs` fires only when the stage
sits on the committed-source boundary AND the target is durable. Point a run at
a durable target and every one of those refusals returns — the condition is the
target, not the stage — so a dirty worktree stops at the first such stage by
name. Committing is what makes promotion possible; it is not something the loop
does, and there is no `--promote`.

#### 10. Teardown, and the trap the next standup hits

Quit the client, stop the loop with Ctrl-C, stop `wamn dev up` with Ctrl-C,
then remove the services BY THIS PROJECT NAME and the scratch path.

```bash
docker compose --profile receiving-route -p "$WAMN_TUI_PROJECT" \
  -f "$WAMN_TUI_COMPOSE" down --volumes --remove-orphans
rm -rf -- "$WAMN_TUI_ROOT"
```

**A second `wamn dev up` against a PostgreSQL cluster that already held one
refuses.** Roles are cluster-global and outlive the database they were minted
for, so the previous standup's generation member is still there and now carries
a direct `CONNECT` grant on two databases. The refusal reads:

```
app stable-role generation member does not carry exactly one direct database CONNECT grant
```

It is raised by `provision_project_env.rs` and it is correct — an ambiguous
generation member is exactly what it is there to catch. **Take the cluster
down and stand a fresh one up**; do not try to reuse the roles. `down
--volumes` above is what makes the next standup clean, and dropping only the
database is not enough.

### `[AGENT-PILOT]` — the agent-authoring experiment harness

Not a gate. It measures whether a coding agent can author a wamn package from a
scenario and prove it works, with no human relay. The method, the rubric and the
task fixtures live in `docs/experiments/agent-authoring/protocol.md`; the tools
are specified in `docs/poc/agent-authoring-tooling-spec.md`. This section is the
command surface only.

```bash
tools/agent-pilot-run all --run 001 --agent claude \
  --task docs/experiments/agent-authoring/tasks/dock-appointments
```

The verbs run separately when you want to hold the environment between them:
`up`, `launch`, `grade`, `down`. `down` is safe to run twice. `--agent stub`
drives the whole shape without spending an agent, and `--stub-mode` reproduces
each of the four driver exit reasons.

Re-grade a recorded run without an environment:

```bash
tools/agent-pilot-grade --replay "${XDG_CACHE_HOME:-$HOME/.cache}/wamn-pilot/runs/020-claude-dock-appointments"
tools/agent-pilot-grade-proof
```

`--replay` scores every step from the requests and responses the run wrote into
`grade/http.jsonl`, under the same predicates as the live grade. No host, no
database and no agent time, so a rubric change re-scores every recorded run. It
writes `checklist-replay.json` and leaves the run's own `checklist.json` and
`grade/` files exactly as the run left them. `agent-pilot-grade-proof` is its
proof and needs no environment either.

A replay reads what the run recorded. A run graded before `wamn-nvbd.10` landed
carries no request log, so its steps replay as `not replayable`.

Run directories live under `${XDG_CACHE_HOME:-$HOME/.cache}/wamn-pilot/runs`.
They are working state and any tool may delete them. Evidence leaves the cache
through `tools/agent-pilot-report`, which writes
`docs/experiments/agent-authoring/<run>.md` and the raw directory beside it,
minus the environment and the worktree.

Reclaim the arm after you promote it:

```bash
tools/agent-pilot-report --run 030
tools/agent-pilot-run down --run 030
```

The second `down` deletes the run directory and its per-commit target directory,
which is where the storage is: twelve targets at roughly 11 GB each reached
135 GB. It reclaims nothing until `agent-pilot-report` has written
`docs/experiments/agent-authoring/<run>/run.json`, and it keeps a target that
another surviving run still names. Running `down` inside `all` is therefore
always a no-op for storage, and `down` on an already-reclaimed run is a no-op
too.

Rules the harness enforces rather than asks for:

- One run per machine at a time, and never beside a cluster journey. It takes
  the same five ports the development environment takes, plus the Gate's 8088,
  and preflight refuses if any is in use.
- The run worktree has no remote, so "do not push" is true by construction.
- `bd` is off the agent's `PATH`, so the hooks no-op and a run cannot write the
  task system. This is a known deviation from an ordinary session and it is
  recorded in every run report.
- The skill inventory is frozen against run 001. A run whose inventory differs
  is refused, because both agents select skills by description and a skill
  appearing between runs changes the measurement silently.
- The grading fixture is harness state and lives outside the run directory,
  because the run directory is exported to the agent. `up` refuses the run when
  the fixture or a `grade` block is reachable from any path the agent is handed.
  It also refuses when the grading root sits on the run directory's walk-up
  path, which is why that root is
  `${XDG_STATE_HOME:-$HOME/.local/state}/wamn-pilot-grading` and not a sibling of
  `runs`. The walk stops at `$HOME`: one user on one filesystem cannot hide a
  directory from itself, and the bar this sets is deliberately leaving the
  sandbox rather than reading a path the layout hands over.
- **The pilot builds its binaries from the main checkout, not from the run
  worktree.** An edit that lands in the main checkout while `up` is building
  goes into the binaries the measurement uses. `up` now hashes the tree before
  and after the build and refuses the run if it changed. Do not write to the
  main checkout until `up` reports ok; after that the run uses binaries already
  built, and the agent compiles only inside its own worktree.

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

**Both sides of that arm run `build-only m1`, so it cannot see the third
channel** (`wamn-10yt.61`). A component profile decides which packages one
`cargo build` compiles, and Cargo unifies features across everything in that one
invocation, so a package the `proof` profile adds can turn a feature on in a
crate the `m1` guests already link. The resolved feature NAME LIST goes into
`-C metadata` whether or not the feature compiles to anything, so the artifact
moves. Measured at `2a4cd288` and again at `7b456f81`: all four virtualized
artifacts differ between the two profiles. The three `components/no-std` guests
are byte-identical, because that workspace is a separate invocation.

The cross-profile arm builds ONE tree twice, once per profile, and compares the
shared packages. It needs no worktrees; `build-only` already prints the raw
digest of every artifact it declares, so no virtualization pass runs.

```bash
set -euo pipefail
GUEST_PROFILE_SCRATCH="$(mktemp -d /tmp/wamn-guest-profile.XXXXXX)"
for profile in m1 proof; do
  CARGO_TARGET_DIR="$GUEST_PROFILE_SCRATCH/$profile" RUSTC_WRAPPER= \
    ./tools/build-components build-only "$profile" \
    > "$GUEST_PROFILE_SCRATCH/$profile.json"
done
WAMN_DIGEST_PROFILE_M1_PLAN="$GUEST_PROFILE_SCRATCH/m1.json" \
WAMN_DIGEST_PROFILE_PROOF_PLAN="$GUEST_PROFILE_SCRATCH/proof.json" \
  cargo test -p wamn-proof-conformance --test guest_workspace_closure \
  one_commit_built_under_two_profiles_yields_identical_guest_digests \
  -- --ignored --exact --nocapture
rm -rf -- "$GUEST_PROFILE_SCRATCH"
```

Separate target directories are the point: one shared directory makes the second
profile a rebuild of the first, and a rebuild that reuses cached artifacts hides
the very difference this arm exists to find. `RUSTC_WRAPPER=` is emptied for the
same reason a wrapper's cache would answer from the other profile's build.

Run it whenever a Cargo dependency is added or its features change anywhere
under `components/`. A failure names the packages that moved. To find the cause,
diff the `features` field of every `.fingerprint/*/lib-*.json` in the two target
directories; that names the crate whose resolved feature list differs, and
`cargo tree -e features -i <crate>` names the requester.

**THIS ARM IS RED ON ARRIVAL, and `wamn-10yt.61`'s stated cause is refuted.**
Measured at `7b456f81` plus the `postgres-sqlx` fix, all four artifacts still
differ. The bead attributed it to two ungoverned `default-features` declarations
in `components/data/postgres-sqlx/Cargo.toml`, saying `futures-util`'s default
set turns on `io`. It does not: `futures-util` 0.3.34 declares
`default = ["std", "async-await", "async-await-macro"]`, and `io` is not in it.
`io` comes from `sqlx-core` 0.9.0's own manifest, unconditionally —
`[dependencies.futures-util] features = ["alloc", "sink", "io"]`, no
`default-features = false` — and `io = ["std", "futures-io", "memchr"]` is what
gives `memchr` its `default`, which `serde_json` links, which is how `blob-put`
moves without going near sqlx. Governing the two declarations moved three of the
four digests and converged none:

| artifact | m1 | proof, before | proof, after |
| --- | --- | --- | --- |
| `blob-put` | `d7e0543a` | `ef5e4aa5` | `ef5e4aa5` |
| `client-acme-receiving` | `8c2c63d8` | `72124342` | `4cb2fba6` |
| `receiving` | `666745eb` | `b724c5c4` | `6ae3d667` |
| `wms` | `e1e14ee6` | `f4e4a629` | `d3c17f29` |

The residual channel is that the `proof` selection compiles `sqlx-core` and `m1`
does not, so no declaration in this repository closes it. Closing it needs a
ruling — one `cargo` invocation per guest (measured 116s against 47s and
declined), or the same member set under both profiles, or a `sqlx-core` fork, or
profile-scoped pins. Until then, mint every pin under `m1`, which is what
`[EFFECTIVE-RELEASE-POC]` and `[RECEIVING-ROUTE-JOURNEY]` both build.

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

**The Gate is a spawned `wamn-scenario-worker serve` child** (`wamn-10yt.10.32`).
This gate used to launch it in-process through `serve_with_readiness`. That was
the second gate-launch path. The owner ruled it dead in the same change that
gave `wamn dev up` a spawned Gate. The proof takes the built binary as
`WAMN_JOURNEY_SCENARIO_WORKER_BIN`. That is a process setting, so it rides as
an environment variable and not as a journey-document field. The proof spawns
it on the fixed loopback port `127.0.0.1:18089`. Readiness is a bounded TCP
connect against that port, and it refuses by naming it. The port is fixed
because a spawned child cannot report an ephemeral one back. It must be free
before the run.

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
RECEIVING_ROUTE_JOURNEY_DOCUMENT="$RECEIVING_ROUTE_SCRATCH/journey.json"
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
# `cc4b407f` moved every guest to the release profile. It left this path
# behind, so the `test -s` below has refused since 2026-09-04.
RECEIVING_ROUTE_FLOW_HTTP="$RECEIVING_ROUTE_SCRATCH/target/wasm32-wasip2/release/http_route.wasm"
test -s "$RECEIVING_ROUTE_COMPONENTS/receiving.wasm"
test -s "$RECEIVING_ROUTE_COMPONENTS/client_acme_receiving.wasm"
test -s "$RECEIVING_ROUTE_FLOW_HTTP"

# The Gate the proof spawns. Built into the default target directory, the one
# the test build below also uses.
cargo build -p wamn-scenario-worker --locked --offline
RECEIVING_ROUTE_GATE_BIN="$RECEIVING_ROUTE_ROOT/target/debug/wamn-scenario-worker"
test -x "$RECEIVING_ROUTE_GATE_BIN"
# The spawned Gate binds this exact port. A stray listener is a hard failure,
# not a fallback to an ephemeral one.
test -z "$(ss -Hltn 'sport = :18089')"

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

# The test's inputs are ONE document, not a dozen environment variables. The
# schema is generated from the Rust struct that reads the file and drift-tested
# there; the writer reads the same schema, so a key this side invents or omits
# is refused here, naming it, rather than by the Rust side after the build.
source tools/journey-document.sh
declare -A journey_spec=(
  [system_pg_url]="postgresql://postgres:probe@127.0.0.1:${RECEIVING_ROUTE_PG_PORT}/postgres"
  [component_directory]="$RECEIVING_ROUTE_COMPONENTS"
  [compilation_cache_directory]="$RECEIVING_ROUTE_COMPILATION_CACHE_DIRECTORY"
  [flow_http_wasm]="$RECEIVING_ROUTE_FLOW_HTTP"
  [component_artifact_base]="$RECEIVING_ROUTE_AUTHORITY/wamn/components"
  [release_artifact_base]="$RECEIVING_ROUTE_AUTHORITY/wamn/releases"
  [route_host]="$RECEIVING_ROUTE_HOST"
  [registry_auth_file]="$RECEIVING_ROUTE_DOCKER_AUTH"
  [host_secret_directory]="$RECEIVING_ROUTE_SECRET_OUTPUT_DIRECTORY"
  [host_secret_namespace]="$RECEIVING_ROUTE_SECRET_NAMESPACE"
  [route_caller_secret_output]="$RECEIVING_ROUTE_CALLER_SECRET_OUTPUT"
)
write_journey_document journey_spec \
  tests/integration/schema/wamn-journey.schema.json "$RECEIVING_ROUTE_JOURNEY_DOCUMENT"
WAMN_JOURNEY_DOCUMENT="$RECEIVING_ROUTE_JOURNEY_DOCUMENT" \
WAMN_JOURNEY_SCENARIO_WORKER_BIN="$RECEIVING_ROUTE_GATE_BIN" \
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

### `[BIND-CONNECTION-LIVE]` — the connection-admin verb, proven by the round trip

`wamn-ctl bind-connection` writes the three rows a bound connection is made
of — an environment-owned instance, its first generation carrying a
credential HANDLE, and the release-scoped binding of one admitted component's
declared alias — through the control library's own SQL builders and the
runtime's own definition hasher. The proof is not that the rows exist: it is
that the postgres plugin's `connection_effect_snapshot` loads them and
`wamn_blobstore::binding::resolve` returns exactly what was bound. Before the
verb the same snapshot is refused as unbound; a definition missing a
coordinate, a blobstore instance bound to an alias declared as HTTP, and an
alias the component never declared are each refused by name and write
nothing.

Two disposable databases on one PostgreSQL 18 server. The test `expect`s both
variables and never self-skips; it is ignored by default because it needs the
server.

```bash
docker run -d --name wamn-bind-connection-pg18 -e POSTGRES_PASSWORD=probe \
  -p 127.0.0.1:5441:5432 postgres:18
until psql postgres://postgres:probe@127.0.0.1:5441/postgres -Atqc 'select 1'; do sleep 1; done
psql postgres://postgres:probe@127.0.0.1:5441/postgres -Atqc 'CREATE DATABASE bind_project' \
  -c 'CREATE DATABASE bind_control'
WAMN_BIND_CONNECTION_PROJECT_PG_URL=postgres://postgres:probe@127.0.0.1:5441/bind_project \
WAMN_BIND_CONNECTION_CONTROL_PG_URL=postgres://postgres:probe@127.0.0.1:5441/bind_control \
  cargo test -p wamn-ctl --locked --test bind_connection_live \
  -- --ignored --exact bind_connection_round_trips_through_the_plugins_own_resolution --nocapture
docker rm -f wamn-bind-connection-pg18 # BY EXPLICIT NAME. Never prune.
```

### `[WMS-CLUSTER-JOURNEY]` — the WMS release minted from the shell, on its own cluster

The second application's journey, and the first minted through the product's
verbs rather than the Rust producer: `tools/wms-cluster-journey-run` sources
the nine harnesses, declares WMS's identity once, and drives the product's
verbs from the shell in the order the Rust producer proved, call for call:
`provision-org`, `provision-project-env` for the environment (emitting the
Database CR, role SQL and privilege SQL that psql applies in place of the
operator), the platform floor, `reconcile-run-plane`, then
`provision-project-env` ONCE PER FAMILY — the verb's workload-action group is
single-select, which cluster run 2 measured (`wamn-362o.37`) — then
`apply-package`, `reconcile-package-data-access`, `push-component` (wms,
label-render and blob-put, under the wms package scope), a locally served
authoring gate for each wiring, `author-wiring`, `publish-release`,
`bind-connection` and `push-release-manifest`. A verb's flags read from its
parser are not its call contract; its argument groups and what the proven
caller applies around it are. It seeds the
FIXTURE rows a move needs — one product, two locations, one pallet with one
quantity — which is precondition state, not simulation.

```bash
tools/wms-cluster-journey-run --apply \
  --evidence-dir /tmp/wamn-wms-cluster-evidence
```

The runtime assertions structure cannot make ride inside it: the journey
exposes the released route on a temporary NodePort, amends the document's
`runtime` phase with that endpoint and the fixture ids it seeded, and runs
`wms_runtime_live::contention_and_replay_through_the_composed_route`, which
fires two moves of one pallet behind one barrier and asserts exactly one
success that moved the stock (its own response carries the target location
and `row_version` 2) and exactly one `concurrency_conflict` that observed
that version, then replays the winner's body and gets the same
`movement_id`. The test prints the winner's id; the journey lists the store
with `mc` and asserts exactly one label object under `wms/`, named by it.
Then `wms_runtime_live::the_remaining_operations_serve_their_released_routes`
(`wamn-362o.52`) hits the other five routes on the same fixture: get and
aggregate to learn where it stands, adjust, a split whose replay returns the
same new pallet id, a split refused for what the row holds, a merge that
consumes the new pallet, the aggregate excluding it, and a paged query.
An operation counts as shipped when its route has been hit. The tests assert
and never provision; no environment variable carries data.

`--measure-startup` probes `pallet.get` on the fixture pallet: one generated
statement in every phase (cold, restart-first, steady), the fixed count
`trace_is_complete` holds. Before `wamn-362o.10` landed the read route, the
only possible probe was a move, which performs eight statements cold and
replays with one ever after, so the arm was refused rather than measured.
What the journey deliberately does not do: nothing flows through the
materializer, which is deployed and asserted idle because the overlay mounts
its family.

#### `--demo` — the WMS demo, the command and the URL

`--demo` holds the environment after a passing run and makes the released route
reachable from a browser. Run it, then read the block it prints:

```bash
tools/wms-cluster-journey-run --apply --demo \
  --evidence-dir /tmp/wamn-wms-demo
```

Open `http://127.0.0.1:8080/`. The route answers
`{"error":{"code":"route-not-found"}}` with the exact typed shape, which proves
it is live. Every WMS operation is a POST, so the printed block carries the four
commands: read the pallet, move it through the composed wiring, run the same
move again and get the same `movement_id` back, and list the labels the object
store's own client wrote.

The block prints the path of the route-caller token, never the token. Both that
path and the MinIO container name change per run, because both die with the
environment.

**The demo changes three things and leaves the release alone.** It pins the
route's NodePort, keeps that Service past the runtime phase, and runs one nginx
container on `127.0.0.1:8080` that rewrites `Host` to the route host the release
names. The proxy is there because a browser cannot reach the route otherwise:
the route matches its host exactly, a browser sends the port it dialled, port 80
belongs to the frozen cluster, `.localhost` names resolve to `::1` on this
machine, docker does not forward an IPv6 publish on the `kind` network, and the
runtime refuses a route host that carries a port as not a valid RFC 1123
hostname.

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
above, then installs the pinned operator 2.9.0 release before the pinned host
2.9.0 release on its own three-node kind cluster. The gate checks three Ready
default-group Hosts, native Workload scheduling, the operator-managed
EndpointSlice, the exact typed `route-not-found` response through
`receiving.localhost`, and the native `CrossEnvironmentSchedulingDenied`
refusal. The runner applies the WAMN-owned per-environment modern-Event Role
and RoleBinding, proves the operator's actual ServiceAccount has exactly
`create,patch` on `events.k8s.io/events`, and records both the durable condition
and matching native Warning Event. The 404 proves only HTTP routing and guest
execution; the Kubernetes objects independently prove the other arms.
The [first 2.9.0 full journey](../perf/2026.09/wasmcloud-2-9-cutover/live-receiving-001/journey/verdict.json) passes at `7798190c`.
The later idle executor, telemetry, and startup-burst cases have executed evidence.
Complete operator recovery still requires the corrected proof rerun.
Receiving007 stops before any NATS fault while its sampler Pod is `ContainerCreating`. `wamn-0h0g.2.7.16` owns the bounded creation/readiness wait.
Run the full mode from the next clean source commit, after its build preparation finishes.
Keep new evidence in the main repository's `docs/perf`, outside the isolated build worktree.
Select a fresh directory. The runner must not overwrite an earlier receipt.

```bash
tools/receiving-cluster-journey-run --apply \
  --evidence-dir /home/kaalin/dev/wamn/docs/perf/2026.09/wasmcloud-2-9-cutover/live-receiving-008/journey
```

The runner supplies the existing private kubeconfig, authorities, release, and fixture inputs to the helpers.
No extra production flag or manually copied credential is needed.
Each helper must pass before the runner writes the full verdict.
Current [telemetry](../perf/2026.09/wasmcloud-2-9-cutover/live-receiving-003/journey/telemetry/receipt.json) passes at `a40d1c18`.
The [startup exposure proof](../perf/2026.09/wasmcloud-2-9-cutover/live-receiving-004/journey/startup-burst/result.json) passes at `802aed06`. Complete operator recovery remains pending.

| Helper and receipt | Required proof and limit |
|---|---|
| `tools/journey-telemetry-proof`; `telemetry/receipt.json` | Within 120 seconds, collect both real PAT request traces with completed invocation spans and descendant PostgreSQL effects carrying the expected identity. Require native HTTP duration counts of at least two and PostgreSQL/JetStream counts of at least one. These histograms do not identify individual requests or measure guest CPU; HTTP egress injection and private runtime phases remain outside this proof. |
| `tools/journey-startup-burst.py`; `startup-burst/result.json` | Start one fresh release host with empty private caches and the deployed explicit start limit. Submit cold and warm bursts of twice that limit through native RPCs. Require measured native handler overlap above the limit, heartbeat/probe progress during starts, and warm serving continuity. Replicas share one production HTTP digest and native compile deduplication; overlap measures queued demand, not active permit occupancy or CPU use. |
| `tools/receiving-operator-recovery-run`; `operator-recovery/result.json` | Compare all five installed CRD schemas with the pinned distributed chart, allowing only recorded Kubernetes defaults. After 75 seconds of healthy fleet observation, stop only the chart's shared scheduler NATS for 150 seconds, observe the native fleet-deaf condition, restore it, then restart the operator on the same image. Each recovery requires fresh Host readiness and an exact successful application response within 120 seconds. Host identities/processes must persist; Workload UID changes are recorded. The external event NATS stays running. |

The 150-second scheduler outage exceeds native TTL 60 seconds, reconciliation 60 seconds, and heartbeat RPC 5 seconds, with 25 seconds of observation margin.
Actual responses during the outage are retained and checked; arbitrary error responses cannot satisfy the helper.
The helpers retain command, trace, metric, identity, cleanup, and hash receipts beneath their named evidence directories.
They run only in full mode. `--measure-startup` exits earlier and keeps its existing timing protocol and thresholds.
The [completed comparison](../perf/2026.09/wasmcloud-2-9-cutover/performance-comparison-001/tables.md) records 108 steps per source at `dfa1c318` and `e7033f72`.
Both runs pass their existing service ratio ceiling of 12. No further benchmark is requested.

The accepted operator proof permits a contiguous graceful restart in the same Pod and image only with recorded kubelet liveness evidence.
That evidence must identify terminal NATS closure or a fault-time HTTP timeout for the same container.
Host identities, fresh Host status, exact HTTP 200, and both original 120-second recovery ceilings remain required.
The revised proof must run from a clean committed source. The earlier failed runs remain failed.

Under `wamn-0h0g.2.7.12`, the owner excludes the absent production automation producer and its dependent queued-execution and active-work drain proofs from this cutover.
Producer implementation belongs to `wamn-10yt.74`, and active-work executor shutdown proof depends on it under `wamn-10yt.75`.
Idle executor shutdown and Receiving stream delivery do not stand in for those unproved paths.
Do not restore retired grants, seed substitute queue rows, or introduce a producer API to make this cutover gate pass.

**Run the ten harness proofs first. They cost a second and they stand in
front of a twenty-five-minute cluster run.**

```bash
( for proof in tools/journey-{host-values,host-secrets,rendered-identity,workload,trace,materializer,probe,document,mint,throughput}-proof; do
    "$proof" || exit
  done )
```

The explicit names select only the ten offline harness proofs. The live `journey-telemetry-proof` requires the running journey.
The subshell and the bare `exit` are load-bearing, and the first draft of this
line had neither. `for ...; do "$proof" || break; done` reports **exit 0 when a
proof fails** — `break` succeeds, and the loop's status is the status of the
last command it ran. Measured against three stub proofs, one of them failing:
the `break` form printed the failure and exited 0; the subshell form printed it
and exited 1. A command that stands in front of a twenty-five-minute cluster
run, documented in the sentence that introduces the guards, silently disarmed.

The journey's render and assert surface is lifted into ten shared harnesses,
and each has an offline proof beside it — no cluster, no containers, no
network, and the frozen cluster untouched. Together they are the whole
regression net for that surface:

| harness | proves |
|---|---|
| `journey-host-values.sh` | the host overlay renders, every derived secret anchor fires exactly once |
| `journey-host-secrets.sh` | the declared role families match the emitted Secret set, and each Secret's shape |
| `journey-rendered-identity.sh` | the overlay CLAIMS this application's identity — asserted, never rewritten |
| `journey-workload.sh` | the workload manifest carries all five identity claims and one route host |
| `journey-trace.sh` | a trace breakdown is complete, including this application's statement count |
| `journey-materializer.sh` | the materializer manifest's eight identity values come from the declaration |
| `journey-probe.sh` | the probe Job renders AND the rendered probe script actually runs |
| `journey-document.sh` | the input document is written and amended against the schema the Rust struct generated |
| `journey-mint.sh` | the declaration renders, the gate envelope and the per-family provisioning flags a shell mint needs |
| `journey-throughput.sh` | a throughput step's Job renders per layer, digest-pinned, the PAT through Kubernetes expansion, AND the rendered pgbench script actually runs |

Each pins Receiving's bytes by digest against the block it replaced, renders a
second application's declaration to prove the block is generic rather than
merely parameterized, and carries a negative control per guard. Every one has
been mutation-tested on exit code.

They are deliberately NOT a `cargo test`: they are shell over checked-in
templates, and wiring them into the Rust gate would make a one-second check
cost a build.


`wamn-10yt.8` measures the published release with runtime-operator 2.9.0's native
HTTP probes. The runner asserts `/livez` and `/readyz` on port `8081`, including
a `startupProbe` against `/livez`. These probes use a separate listener from
workload HTTP. The timing protocol, request counts, and acceptance thresholds
stay unchanged.

The gate records cold startup and a cache-seeding authenticated request. It
restarts the container in the same Pod, then records restart-first and
steady-state requests. A disposable Tempo/OTel pair receives the real request
traces. Receipts separate authentication, resolution, artifact pull, compilation,
linking, instantiation, and SQL. They also record the ExecutorPlatform,
CallableHttp, and GuestSql connection acquisitions. Compiled cache bytes and
inodes must match across the restart.

The host prepares the released synchronous closure before its native command
loop becomes ready. Other released wirings still resolve on demand. Historical
measurements below used runtime-operator 2.8.0 and TCP probes. The updated 2.9.0
startup proof remains unexecuted.

```bash
tools/receiving-cluster-journey-run --apply --measure-startup \
  --evidence-dir /home/kaalin/dev/wamn/docs/perf/2026.09/wasmcloud-2-9-cutover/live-receiving-startup-001/journey
```

The helper refuses a dirty source tree or a pre-existing scratch cluster. Every
Kubernetes and Helm command names its private kubeconfig/context; it never
addresses the frozen `kind-wamn` cluster. PostgreSQL, authenticated OCI,
cluster, port-forward, and the uniquely tagged release host image are exact-owned
scratch resources. Cleanup absence and a SHA-256 evidence inventory are part of
the passing verdict.

**Measured 2026-09-04 at `82796110`, the fifth `--measure-startup` run**
(evidence `journey-evidence-fifth`, `wamn-362o.18`). The first honest cost
figure for a host restart under a released closure; nothing else in the tree
carries one, and a later claim about startup cost should cite this rather than
an impression. **A restarted host does NOT resume where it left off.**

| phase | http_total | resolve | pull | compile | exec_plat | http_acq | sql_acq |
| --- | --- | --- | --- | --- | --- | --- | --- |
| restart-first | 3495.9 ms | 121.5 ms | 2244.4 ms | 862.4 ms | 97.0 ms | 104.2 ms | 103.8 ms |
| steady | 2943.3 ms | 0.1 ms | 2126.4 ms | 771.8 ms | 0.0 ms | 0.2 ms | 0.2 ms |
| cold | 45432.8 ms | — | — | — | — | — | — |

Runtime startup itself: **896 ms after restart against 320 ms cold**, with the
wasmtime cache holding 2 entries.

**What it means.** The restarted host re-pays resolve and all three effect
acquires — ExecutorPlatform, CallableHttp, GuestSql — that the steady host has
already paid and holds. **Compile and pull are paid in BOTH phases at roughly
the same cost**, which is the surprising half: the wasmtime cache is warm and
the artifact is local, yet compile still costs about 800 ms and pull about
2.1 s on every first request into a process. The restart-first-to-steady gap is
therefore about **550 ms of re-acquisition**, not the multi-second penalty this
arm was built to look for.

**The cold figure is not comparable and must not be quoted as a number.**
45.4 s here against 27.9 s on the run at `59315f36`: it tracks machine load,
not any property of the platform. Cite its order of magnitude — tens of
seconds — and nothing finer.

**Provenance.** `wamn-362o.18` is filed under the WMS epic, but this run was
`tools/receiving-cluster-journey-run --apply --measure-startup`, the
`wamn-10yt.8` arm documented directly above — not the WMS journey.
`tools/wms-cluster-journey-run` did not exist in the tree at `82796110`
(`git ls-tree --name-only 82796110 tools/` lists `receiving-cluster-journey-run`
alone); it landed later under `wamn-362o.25`, and its own `--measure-startup`
arm probes `pallet.get`, a different and far cheaper request that came on only
with `wamn-362o.10`. Those WMS numbers are not these numbers.

These figures describe the tree at `82796110` and nothing after it. Any commit
that changes resolution, artifact pull, or compilation invalidates them, and
the arm above is what re-measures.

### `[RECEIVING-THROUGHPUT-BENCH]` — concurrency 1 to 64 across three layers, the knee

`wamn-0h0g.17.27`. Every number in `docs/perf/2026.09` is one request at a
time; this asks how many at once and where p99 turns. On the same
measure-startup cluster, after the steady request, the runner sweeps
concurrency 1, 4, 8, 16, 32, 64 for ten seconds a step across three layers so
the cost attributes: the authenticated route end-to-end, the same generated
statement from a direct PostgreSQL client, and a route the guest answers 404
to without touching a database. **The load generator is a stock tool in a pod,
pinned by digest** (owner ruling): `oha` for the HTTP layers, `pgbench` from the
pinned `postgres` image for the direct layer. Rust does only the counter
sampling and the report: `wamn-throughput sample` reads `pg_stat_database`,
`pg_stat_activity` and NATS `/varz` before and after every step, and
`wamn-throughput report` reduces the generators' own output plus those samples
to one row per step and a knee and a peak per layer (`throughput/report.md`,
`throughput/summary.json`). The host pod's and the PostgreSQL container's
`cpu.stat` are captured around each step as well, so what saturates first is
read from the machine, not inferred.

```bash
tools/receiving-cluster-journey-run --apply --throughput \
  --evidence-dir /tmp/wamn-receiving-throughput-evidence
```

The sweep runs before the overhead-ratio gate, so a red ratio does not cost it
its evidence. Its shape gate is the ignored test below, which needs only the
sweep's directory and asserts no absolute number — every layer ran every step,
every step produced a rate and a p99, every error count is recorded, and the
knee and peak were computed. The knee and the peak are recorded in the report;
a ceiling ratchets later, on a landing.

```bash
WAMN_THROUGHPUT_EVIDENCE_DIR=/tmp/wamn-receiving-throughput-evidence/throughput \
  cargo test -p wamn-proof-integration --lib --locked --offline \
  throughput_bench_live::tests::every_layer_ran_the_whole_sweep_and_its_knee_is_recorded \
  -- --ignored --exact --nocapture
```

The render is `tools/journey-throughput.sh`, proved offline by
`tools/journey-throughput-proof` like the other harnesses; the index document
the shell writes is strict on the Rust side (`deny_unknown_fields`) and its
schema is checked in at `tests/integration/schema/wamn-throughput.schema.json`,
regenerated with `wamn-throughput schema`.

For the fresh-auth comparison (`wamn-ctc8.12`), use `--fresh-auth-bench` instead of `--throughput`.
This mode runs three sweeps for each credential: a service PAT and a human PAT with explicit environment membership.
The second pair reverses the credential order.
Each sweep retains the existing route, no-database, and direct-statement layers in its own directory.
Each step also records machine load, machine memory, and host memory.
Five human request traces accompany the existing service traces.
The trace summaries count identity-read and permission-read spans, not PostgreSQL server statements.

The fixture uses the production identity authority and membership CLI.
Its two-hour human PAT exists only for this disposable benchmark.
The runner holds that PAT in a private file and Kubernetes Secret, never in evidence.
Journey teardown removes the fixture database and its identity facts.
The original membership proof still runs its seven HTTP cases and removes its own facts.

Use separate repository evidence directories for the baseline and changed source snapshots.
Keep the measurement source worktree clean during each run.
Run builds and measurements serially, and report the spread across all repetitions.
Do not claim a latency improvement when the observed variance obscures it.

The completed 2.9 cutover measurements are retained under `performance-baseline-001` and `performance-2-9-002`.
The [comparison](../perf/2026.09/wasmcloud-2-9-cutover/performance-comparison-001/tables.md) preserves the existing reducer results and their spread.
The [candidate map](../perf/2026.09/wasmcloud-2-9-cutover/performance-candidate-evidence-001/evidence-map.json) records matching guest hashes, resource identity, and startup/cache limits.
The redundant baseline and upstream test checkouts were removed after evidence capture. The migration worktree remains.

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
| `crates/platform/runtime/src/plugins/wamn_postgres/types.rs` | `WAMN_CARRIER_SPELLING_PG_URL` | `cargo test -p wamn-runtime --lib plugins::wamn_postgres::types::tests::live_ -- --include-ignored --nocapture` |
| `services/ctl/tests/dispatch_reader_provisioning_live.rs` | `WAMN_CTL_PG_URL` | `cargo test -p wamn-ctl --test dispatch_reader_provisioning_live` |

**The rows from `cdc.rs` down were added by `wamn-0h0g.15.137.2`, which means
those gates had never entered an arming set.** Each was armed once, alone, on
its own fresh `postgres:18` container.

**RE-MEASURED 2026-09-06 at `1446936a` (`wamn-0h0g.15.137.6`).** Each surviving
binary was armed ALONE against its own fresh `postgres:18` container, with
`--include-ignored --no-fail-fast` and cargo unpiped so its exit code stands.
Three of that bead's nine NO LONGER EXIST:
`crates/schema/compiler/tests/rls.rs`, `crates/schema/compiler/tests/seed.rs`
and `crates/schema/control/tests/migrate.rs` went with the whole
`crates/schema/compiler` crate at `15d6a6b7`, and the adjacent finding about
`migrate.rs` hiding failures in an unwrapped `write_all` died with the file.
**Compare FAILING TEST NAMES, never totals** — one row below went green by
RENAME, and reading its totals alone would have called it a repair.

| gate | 2026-09-06 at `1446936a` | failing test and reason |
| --- | --- | --- |
| `run-state/tests/store.rs` | **GREEN** — 7 passed / 0 failed, 0.82s | was `run_state_schema_applies_and_isolates_on_postgres` on `ERROR: role "wamn_effect_writer" does not exist`. `e5e78da3` gave the bootstrap `provision_sql::ensure_effect_writer_acl_role_sql()` and the `wamn_scenario_author` mint. Unarmed the same command reports `ok` in 0.01s with a `skipping …` line: the duration is the arming proof |
| `project-state/tests/authority.rs` | **GREEN** — 3 passed / 0 failed, 1.26s | was all three on `new row violates row-level security policy`. `e5e78da3` retired every `app.tenant` site here and drives tenancy through `SET LOCAL ROLE` plus an `ASSERT` on `current_user` |
| `project-state/tests/schema.rs` | **GREEN** — 5 passed / 0 failed, 0.66s | **green by rename, not by repair.** The red test `app_schema_applies_and_enforces_isolation_and_claims_on_postgres` no longer exists; `e5e78da3` renamed it `app_schema_applies_and_enforces_isolation_on_postgres` and added `tenant_floor_derives_from_the_connected_role`. The file's four remaining `app.tenant` sites are all negative probes asserting the retired claim is ignored |
| `runtime/tests/production_claim_live.rs` | **RED** — 1 passed / 1 failed | `production_claim_live`. The scoped-generation refusal is FIXED here: `install_effect_writer` minted the credential under `claim-live-tenant` while the host validates it against `TENANT`, and the generation role hashes the tenant. Under it the ExecutorPlatform pool checkout is refused — the fixture creates no `wamn_executor_platform` ACL role, so `credential_exactness_hook`'s membership expectation cannot hold. Measured with that ACL role pre-created by hand: the claim then dies on `function require_executor_platform_authority() does not exist`, because this fixture's hand-rolled schema never installs the run-plane fence |
| `runtime/tests/production_claim_durable_live.rs` | **RED** — 0 passed / 1 failed | `production_claim_durable_live`, same credential fix, and under it the **wamn-0h0g.10.15 accepted red reached from a second binary**. `EffectWriterClient::begin_attempt` opens with `serialize_effect_intent_sql()`, whose first act is `require_executor_platform_authority()`; the fixture's effect-writer generation login is a member of `wamn_effect_writer` and nothing else. Measured directly on the live server: with that fence function installed, the login is refused `42501 executor-platform-authority-required`. Granting it the platform family in the fixture would fabricate authority and is refused |
| `runtime .../wamn_postgres/claims.rs` | **RED** — 0 passed / 1 failed | `live_size_one_guest_and_platform_pools_isolate_sessions_under_interleaving` — `platform headroom remains available while guest is saturated: PgError::ConnectionUnavailable`. `live_guest_url` mints a `wamn_app`-family login and grants it `wamn_app` only, then hands that one URL to every class; the platform checkout's exactness hook wants membership in `wamn_executor_platform`, which the server does not have. Separable guest/platform headroom cannot be observed without a platform-class checkout, so **this fixture does not prove it with the credential it legitimately holds**. Minting a platform-family generation login here would fabricate authority (the wamn-0h0g.10.15 precedent) and is refused, so the red stands with its reason rather than being rewritten green |

One gate that was GREEN in the `wamn-0h0g.15.137.2` sweep is RED at `1446936a`
and was swept by `wamn-0h0g.15.137.6`:
`crates/execution/run-state/tests/run_state_live.rs` died on
`ERROR: malformed array literal: "cat"`. `15d6a6b7` widened
`select_exhausted_production_sql` to take `$2::text[]` and left this file's
`PREPARE reap_select (bigint,text,text)` — written at `4ef5658f` — naming a
scalar, so PostgreSQL cast the text `cat` to `text[]`. The guard now declares
`text[]` and passes `'{cat}'`; measured 1 passed in 6.70s, and breaking the
package set to `'{other}'` reds the exact assertion
(`the janitor locked no crash-budget-exhausted candidate`).

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

# ground truth — a connection FROM THE HOST, the way the suite will connect;
# loop on this, nothing else (the PostgreSQL traps below say why not exec)
until PGPASSWORD=pw psql -h 127.0.0.1 -p "${PORT}" -U postgres -d postgres \
        -tAc 'select 1' >/dev/null 2>&1; do
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
server's own ability to answer in both runs -- and on the WMS mint it answered
about 45 s before a connection from the host did (`wamn-362o.38`). Loop on a
connection made the way the suite will make it.

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

Four traps around it:

- **After a host reboot kind crash-loops:** `fs.inotify.max_user_instances`
  resets to 128 -- `sysctl` it to 1024, then delete the crash-looped
  `kube-proxy` pods.
- **`kubectl port-forward` dies on every connection close.** Use a temporary
  NodePort, or `kubectl exec -i <pod> -- psql`.
- **Jobs are immutable** (delete before re-apply), and a ConfigMap edit does
  not restart its pod.
- **Kind-loaded images cannot be re-pulled:** removal is permanent unless a
  host copy exists. Build the protected set from POD SPECS, not from running
  containers; and the kind node container is named `wamn-control-plane`,
  which matches a grep for "lane".

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

**Lane worktrees live under `$HOME/.cache/wamn-lanes/`, never under `/tmp`.**
`/tmp` here is a 31 GB tmpfs and one warm target is 13 GB; an overflow surfaces
as `EDQUOT` from `rustc` and froze every shell on the machine, the other
session's included. `git worktree move` refuses to cross devices: `cp -a` the
tree, `git worktree repair <new-path>`, then remove the old one. A lane uses
its worktree's own default `target/` -- do not set `CARGO_TARGET_DIR` for a
lane -- so `git worktree remove` takes the artifacts with the tree.

**Cut lane worktrees yourself, pin the absolute path, and verify an anchor
file.** A worktree provisioned for you can sit hundreds of commits behind the
base you named while reporting a valid sha: it lacks every file the work
needs and nothing says so. Forbid the main checkout by name in the brief, and
`test -f <anchor>` before the first command.

**Never `git worktree remove` the lane you are standing in.** The cwd is gone
for every command chained after it (`Unable to read current working
directory`); six chains lost their cleanup and one its launch in a day.
Remove from the root checkout, or end the chain there.

**A lane branch integrated by cherry-pick is not an ancestor of `HEAD`,** so
`git branch -d` refuses and `-D` is correct -- after per-file `sha256sum`
parity between the lane and the landed commit, never on the assumption.

**No full workspace sweep inside a lane.** Targeted `-p` only while the lane
is open; one sweep at the end, on the integrated tree.

**A process-watch pattern that matches its own command line never exits.**
`pgrep -f`, `pkill -f` and a log-watching grep all see the shell that invoked
them, because the pattern is in that shell's argv; a `pkill -f` kills the
caller (exit 144). Collect pids first and exclude `$$`, or match on something
the watcher does not carry.

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

**A surviving mutant can be masked by a normalizer DOWNSTREAM of the property
it breaks. Assert a claim at the layer that makes it.** The journey-document
writer claims its bytes do not depend on bash's hash order, and emits in the
schema's key order under `jq -S`. The mutant that emits in hash order with no
sort SURVIVED — and was not equivalent. The proof's byte-pin sits after the
amender, and the amender re-sorts the whole document under its own `-S`, so
the writer's order never reached the diff. It looked like an equivalent
mutant and was a claim nobody tested. One assertion on the pre-amendment
file — `jq -S . out | diff - out` — kills it. The general shape: when a mutant
survives and the first explanation is "equivalent," find what sits between the
mutated code and the assertion, and ask whether it repairs the damage.

**A guard that matches a substring is caught by a superstring — even a guard
in a one-line chain.** `! grep -q JourneyInputs` was meant to prove a type was
gone after a rename; `DevJourneyInputs` still existed, the guard fired, and the
chain aborted before the build it was gating. Ten seconds, and only because
the failure was loud. The uniqueness corollary applies to a throwaway `grep`
exactly as it applies to a renderer's anchor.

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

**Fifth instance of the self-skipping family, and the most embarrassing
one: the DOCUMENTED COMMAND for running a set of guards reported green while
a guard failed.** The line was `for proof in tools/journey-*-proof; do
"$proof" || break; done`. `break` succeeds, and a loop's exit status is the
status of the last command it ran — so a failing proof printed its failure and
the loop exited 0. The correct form is a subshell with a bare `exit`:
`( for proof in ...; do "$proof" || exit; done )`.

The family now reads: a **rename** can deselect a test (`--exact` against a
name that no longer exists runs nothing and exits 0); an **env gate** can skip
one; a **misspelling** can select none; a **zero-match scripted edit** produces
a void mutant that reads as a survivor; and now a **loop** can run every check
and discard the verdict. What they share is that the machinery ran, produced
output, and returned success — so every one of them looks exactly like a pass.

It was caught by three stub proofs, one failing, run through both forms: the
`break` form exited 0 and the subshell form exited 1. Six seconds, against a
line that would otherwise have stood in front of a twenty-five-minute cluster
run telling people their guards were green.

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

**Editing at scale.** Each of these cost real time on a scripted edit.

- **Bound a code-block edit by brace scanning, never by a regex that can
  start earlier than intended.**
- **Every scripted replace asserts its match count.** A replace matching
  twice yields a VOID mutant, and a run against the unmutated file reports
  all-green -- which reads exactly like a surviving mutant. A zero-match
  apply reads the same way.
- **Inserting before a `fn` line steals the previous item's `#[test]`.**
  Verify with `cargo test ... -- --list`.
- **After adding a refusal, fix every fixture that supplied the now-refused
  shape.** A new refusal has consumers exactly like a rename does, and a
  green `-p` means nothing on a self-skipping gate.
- **After renaming a public field or type, build the owning crate's own
  `--tests`** and sweep every mention in one pass rather than fixing the
  compiler's first complaint.
- **Verify a claim per site before sweeping by count.**

**Mutation discipline.** Apply, test, restore, sha256-verified at each step;
debug builds; roughly one mutant per layer. The entries above -- exit-code
scoring, the landed proof, the negative control -- are half of it. The rest:

- **Run the unmutated arm first,** so the pass/fail pair is meaningful in
  both directions.
- **A sha-verified source restore is not a verified build.** `cp -p` and
  `shutil.copy2` preserve mtime, so cargo reuses a STALE binary and the next
  run reports the old source's result against the new file; the sha passes
  because the file is byte-correct and only the artifact is old. Restore by
  writing content, or `touch`, or clean. This cost a wrong bead and a phantom
  feature-unification hunt.
- **Each mutant must fail a named test for the intended reason.** If not, the
  assert is inert -- say so. A mutant dying on leftover cluster state or
  failing to compile proves nothing; discard and re-derive. An EQUIVALENT
  mutant surviving is not a hole -- report it as equivalent.
- **Prefer an additive mutant when a removal would break the build;** when
  removing from a fixed-size array, adjust the size or the mutant is a
  compile error, not a mutant.
- **One fresh cluster per mutant when the subject is roles or grants** --
  roles are cluster-wide.
- **Re-derive each mutant yourself,** at the call site, against the current
  tree; mutate only the production occurrence, never the test's
  expectation, and `sha256sum` before and after.
- **When a mutant's point is that an existing gate misses it, run the
  existing gate against it:** a surviving old gate beside a dying new one is
  the evidence; the claim alone is not.

**PostgreSQL traps.** Each was measured on a disposable server, and each read
as something else first.

- **Probe readiness the way the suite will connect** -- the same host,
  address and port -- never from inside the container: on the WMS mint
  (`wamn-362o.38`) `docker exec ... psql` answered about 45 s before the
  suite's own connection did. `pg_isready` and a bare TCP connect lie in the
  other direction (the throwaway Postgres, above).
- **Roles are cluster-wide: one fresh cluster per role or grant mutant,** and
  one fresh container per suite.
- **A leftover healthy object satisfies `IF NOT EXISTS` and masks a mutated
  builder.** `DROP OWNED BY` before `DROP ROLE`, inside an existence check; a
  gate applying a bare `CREATE DATABASE` or `CREATE ROLE` must drop first and
  PASS TWICE IN A ROW. `IF NOT EXISTS` is not idempotent under concurrency;
  only an `EXCEPTION` guard is race-tolerant.
- **`DROP OWNED BY` reaches only the current database.** A role owning
  objects elsewhere refuses to drop, with a `DETAIL` naming that database.
- **`CREATE DATABASE` and `DROP DATABASE` are their own autocommit
  statements.** `psql -c` wraps a multi-statement string in ONE transaction;
  `psql -1` fights a file that owns its own `BEGIN` and swallows the real
  error.
- **A stock container leaves `PUBLIC` connectable:** `REVOKE CONNECT ON
  DATABASE postgres FROM PUBLIC` in the preamble, or an isolation assertion
  proves nothing.
- **plpgsql `EXECUTE` accepts a multi-statement string; `PREPARE` refuses
  `UPDATE ... RETURNING`** ("prepared statement is not a SELECT") -- use
  `EXECUTE ... USING` there.
- **Any row-locking clause requires `UPDATE` on at least one column,** so a
  bare `REVOKE UPDATE` on a row-locked relation is a production outage, not a
  hardening.
- **`UPDATE ... WHERE pk IN (SELECT ... FOR UPDATE SKIP LOCKED)`
  over-claims** through planner re-scan; fence the claim with `WITH ... AS
  MATERIALIZED`.
- **An expression index matches only the bare indexed expression:** the bare
  call on the left, a scalar on the right. A function that calls
  `current_database()` is `STABLE`, not `IMMUTABLE`, so the database must be
  a literal in the body for the index to exist at all.
- **RLS default-denies when no policy matches the current role.** Narrowing a
  policy with `TO` locks out every other role rather than exempting it, and a
  role that previously hit permission-denied on a function inside the
  predicate now reads a SILENT EMPTY RESULT. Exempting needs `BYPASSRLS` or
  its own permissive arm.

## Adding a span to the request path

Two mistakes cost a full measurement cycle each while instrumenting the request
path for `docs/perf/2026.09/3a-instrument.md`.

**A span guard held across an `await` makes the future non-`Send`.** The WIT host
functions require `Send`, so `tracing::info_span!(...).entered()` around any code
containing an `.await` fails to compile with *"future cannot be sent between
threads safely"* — and the error surfaces at the host-function signature, several
files away from the guard that caused it. Use an instrumented async block:

```rust
async { /* work with .await */ }
    .instrument(tracing::info_span!("wamn.thing"))
    .await
```

`.entered()` is correct only when the block it guards is entirely synchronous.

**Span the function the path actually takes.** `run_query` and
`run_verified_query` in `wamn_postgres/resources.rs` both have a row-decode loop.
A released route takes the *verified* one, so instrumenting `run_query` produced
a span that never appeared in a single trace — which reads exactly like the work
being free. Confirm the span appears in a captured trace before drawing any
conclusion from its absence.

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
throughout**: 1-minute load average ran 8.8 → 34 across the run. On the
measurement date `.cargo/config.toml` set `rustc-wrapper = "sccache"`. It no
longer does (wamn-362o.22). To reproduce these numbers, first run
`export RUSTC_WRAPPER=sccache` in your own shell. Absolute
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
