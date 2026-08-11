# Build & Test — gate commands per bead

> **§1.9a audit (2026-07-19): amendments are additive — base sound.**

Every shipped feature/bead has a build+gate command block below. Prose rationale
lives in the design docs (`docs/*.md`) and the beads memories (`bd memories <keyword>`);
this file is the runnable-command reference. See `README.md` for the quick
dev/test/deploy commands.

## Build environment

wamn-host builds against wash-runtime consumed as a **git dependency from our
fork** (dkkloimwieder/wasmCloud, branch `wamn/2.6.1` = upstream v2.6.1).
`docs/archive/platform/wash-runtime-fork.md` is the authoritative carried-policy ledger and
rev-bump runbook; this preamble does not duplicate its commit or seam
inventory. The rev is pinned in one place:
`workspace.dependencies.wash-runtime.rev` in the root `Cargo.toml`.

### Optimized native developer builds

The repository toolchain remains pinned by `rust-toolchain.toml`. On
`x86_64-unknown-linux-gnu`, `.cargo/config.toml` asks `clang` to link with
`mold`, so both executables are host prerequisites for ordinary root-workspace
commands:

```bash
clang --version
mold --version
cargo build --locked
cargo test --locked
```

This target-specific configuration is load-bearing. It must not become global
`build.rustflags`: native workspace builds use the LLVM codegen backend and
clang+mold, while `wasm32-wasip2` components continue to use Rust's `rust-lld`.
Mold is the checked-in native linker choice; there is no automatic lld
fallback. A missing `clang` or `mold` is an environment error to fix before
building.

The root debug profile keeps workspace crates at opt-level 0 with full debug
information. Third-party dependencies use opt-level 1 and line-table debug
information, and build dependencies use opt-level 1. This keeps application
debugging unchanged while avoiding fully deoptimized dependency code. If one
dependency needs full source-level debugging, add a narrow override and remove
it again after the investigation:

```toml
[profile.dev.package."dependency-name"]
opt-level = 0
debug = "full"
```

These settings apply only to the root workspace's debug profile. They do not
change release profiles, the separate component workspace, component signing,
or any gate's codegen backend.

### Cached kind gate image build (wamn-9ler.2)

`tools/kind-gate-build` runs the ordinary `gates` Docker target with plain
BuildKit output, imports and exports a registry-backed layer cache, loads the
resulting exact image tag into kind, and leaves the image available for its
gate run. The caller owns registry authentication, TLS, retention, and buildx
builder configuration:

```bash
./tools/kind-gate-build \
  --image wamn-gates:<exact-issue-tag> \
  --cache-ref <registry>/wamn/build-cache:gates \
  --builder <buildx-builder>
```

This path requires Docker BuildKit/buildx and a builder that can read and write
the supplied OCI cache reference. The Dockerfile pins `cargo-chef` 0.1.77 and
prepares and cooks separate root and component recipes. Use one stable mutable
cache reference across builds. It is deliberately separate from the gate image
tag: source-only edits keep the root and component `cargo chef cook` steps
cached, while `Cargo.lock` and `components/Cargo.lock` key independent
dependency recipes. Named BuildKit mounts retain Cargo registries, Git sources,
and root/component target state within a builder. The registry cache makes the
cooked layers reusable by a fresh builder. The cache reference is never loaded
into kind and must not be made unique per issue.

The implemented cache path is cargo-chef plus BuildKit; sccache is not installed
or configured. If a build unexpectedly cooks dependencies again, inspect the
plain build log, confirm the same `--cache-ref` was used, and check builder
registry authentication, TLS, and cache retention. A changed lockfile or
manifest legitimately changes its workspace recipe. The ordinary uncached
fallback remains:

```bash
docker build --target gates -t wamn-gates:<exact-issue-tag> .
kind load docker-image wamn-gates:<exact-issue-tag> --name wamn
```

After the Job no longer references its exact image, retire that image from the
kind nodes and the host without deleting the shared dependency cache:

```bash
./tools/kind-gate-image-remove \
  --image wamn-gates:<exact-issue-tag> --apply
docker image rm wamn-gates:<exact-issue-tag>
```

### Dependency graph outcome

The locked root and component graphs retain duplicate version families only at
upstream compatibility boundaries; no cheap, correctness-preserving
consolidation was found. Resolver-v2 keeps the audited Tokio, Hyper, and
`wamn-run-state` test features on development-only edges, and the normal graphs
contain no test-only feature leak. There is no cargo-hakari workspace-hack:
the audit found no evidence of a feature-shift rebuild avalanche, and the
root/component cargo-chef recipes already provide stable dependency reuse.

### Opt-in Cranelift native loop

`cranelift-dev` is a leaf Docker stage for compatible root-workspace packages
that benefit from a faster native debug compile. Build it from the repository
root, then mount the source read-only and keep Cargo downloads and build output
in named volumes:

```bash
docker build --target cranelift-dev -t wamn-cranelift-dev:dev .
docker run --rm \
  -e RUSTUP_HOME=/usr/local/rustup \
  -e CARGO_HOME=/cargo-home \
  -e CARGO_TARGET_DIR=/target \
  -v "$PWD:/workspace:ro" \
  -v wamn-cranelift-cargo:/cargo-home \
  -v wamn-cranelift-target:/target \
  wamn-cranelift-dev:dev \
  cargo cranelift --locked -p wamn-flow-invocation
```

The image installs the current nightly available when it is built and invokes
Cargo with exactly `RUSTFLAGS=-Zcodegen-backend=cranelift`. The helper must run
from the mounted repository root and refuses `--release`, every `--profile`,
`--target`, `--manifest-path`, and `--config` form before Cargo starts. First use
requires network access to fill the named Cargo cache and can still be a cold
compile. Not every native dependency is guaranteed to support Cranelift.

Use ordinary pinned-stable `cargo build` or `cargo test` whenever the nightly
image or a package is incompatible. Stable LLVM remains the only release,
component, signing, Docker-gate, and shipping path. The Cranelift helper must
never be added to those commands. To discard the opt-in image and its caches:

```bash
docker image rm wamn-cranelift-dev:dev
docker volume rm wamn-cranelift-cargo wamn-cranelift-target
```

| Path | Toolchain and backend | Scope |
|---|---|---|
| Root native debug | pinned stable, LLVM, clang+mold on x86_64 GNU | ordinary developer builds and tests |
| Docker gate/release native | pinned stable, LLVM, clang+mold | shipping binaries and gate images; cargo-chef changes caching only |
| Components and signing | pinned stable, LLVM, `wasm32-wasip2` rust-lld | separate workspace and lockfile; native dev profiles do not apply |
| Cranelift native debug | image-build nightly, Cranelift | opt-in compatible root packages only; never release, components, signing, or gates |

### Canonical shipped-decision gate registry (PLAN-0.2 / wamn-2jdm.2)

The registry derives commands and execution inputs from every live gate Job and
documented recipe while owning only the canonical D1-D24 decision mappings and
non-derivable gate semantics.

```bash
# recipe-test: PLAN-0-2-GATE-REGISTRY | conformance | wamn-proof-conformance | test | gate_registry | - | 6 | canonical semantic registry covers every Job manifest and recipe selector, authoritative D1-D24 decision ownership, classifications, immutable evidence pointers, and registry mutants
CARGO_TARGET_DIR=/tmp/wamn-target-2jdm-2 \
  cargo test --locked --offline -p wamn-proof-conformance --test gate_registry
```

### wash-runtime feature and deployed-workload inventory (wamn-8zht.12, wamn-8zht.18)

The checked-in inventory resolves every production service that consumes
`wash-runtime`, records its exact feature set, and proves the three enabled
store constructors against the pinned fork source. It also inventories every
generated `WorkloadDeployment`: host-component plugins remain disabled,
`poolSize` is absent or zero (so `maxInvocations` is inert), and no P3 service
workload is deployed. Every component or service `localResources` block also
exposes exactly one `allowedIpNameLookups: []`, preserving deny-all lookup by
default.

```bash
# recipe-test: H5-RUNTIME-INVENTORY | unit | wamn-proof-conformance | lib | - | runtime_inventory:: | 7 | recorded per-service wash-runtime features, three live store constructors, generated workload reuse/P3 exclusions, and explicit empty lookup defaults
cargo test --locked --offline -p wamn-proof-conformance --lib runtime_inventory::
```

### ExecutionHost deadline re-arm and trap disposal (wamn-8zht.13)

Wasmtime 47 must preserve `ExecutionHost`'s existing per-invocation epoch
window. Invocation A consumes part of its window, invocation B receives a full
fresh window, and an interrupted invocation disposes the live instance before
any later call. NodeRuntime/H9, cancellation, and pooling are excluded.

```bash
# recipe-test: H5-EXECUTION-DEADLINE | unit | wamn-execution-host | lib | - | - | 8 | crates/execution/host/src/lib.rs per-entry epoch re-arm and trapped-instance disposal
cargo test --locked -p wamn-execution-host --lib
cargo test --locked -p wamn-executor -p wamn-proof-integration
```

### allowedIpNameLookups runtime primitive (wamn-8zht.16)

The gate exercises exact, wildcard, literal-IP, and default-empty behavior
through the pinned runtime's public matcher. It resolves the pinned fork source
and proves that P2/P3 TCP and UDP operations remain governed by the independent
raw-socket policy when lookup is approved. The workload-generation exposure is
enforced by the inventory gate above; socket-patch retirement remains separate
post-upgrade work.

```bash
# recipe-test: H5-IP-NAME-LOOKUP | conformance | wamn-proof-conformance | lib | - | ip_name_lookup:: | 6 | pinned runtime exact/wildcard/literal-IP/default [] behavior and P2/P3 TCP/UDP dominance
cargo test --locked --offline -p wamn-proof-conformance --lib ip_name_lookup::
```

## Workspace package tiers

`architecture/workspace-tiers.json` is the canonical, machine-readable
selection for the current **51 root + 31 component packages**. The selection
uses named explicit selectors and deliberately does not add
`default-members`. `tests/conformance/tests/workspace_tiers.rs` compares those
sets with live, locked Cargo metadata and `architecture/package-roles.json`.

The selected package roots are:

| Tier | Root | Components | Selection |
|---|---:|---:|---|
| fast developer/native | 41 | 0 | every root production package; excludes the 7 proof/support packages and 3 POCs |
| product components | 0 | 7 | `api-gateway`, `evaluate-specs`, `flow-http`, `flowrunner`, `materializer`, `normalize-receipt`, `time-shift` |
| contract/conformance | 13 | 0 | all 12 contract packages plus `wamn-proof-conformance` |
| full CI | 51 | 31 | every Cargo member plus the classified non-Cargo `node-ts` sample |
| deployed-system proof | 16 | 31 | deployable native/proof owners plus every guest proof input and `node-ts` |
| release | 10 | 7 | every package classified `deployable: true` |

Package roots are selection inputs, not hand-maintained dependency closures.
Cargo resolves their normal, build, and test path dependencies from live
metadata; the conformance guard proves that the fast and product-component
closures stay within the production set.

### Named selectors

`tools/workspace-tier` reads the canonical JSON on every invocation, resolves
the repository independently of the caller's working directory, and passes
package names to Cargo as an argument array. It never evaluates shell text or
maintains a second package list.

```bash
# Fast native loop.
./tools/workspace-tier run fast_developer_native root check

# Product guest artifacts and contract/conformance tests.
./tools/workspace-tier run product_components components build-wasm
./tools/workspace-tier run contract_conformance root test
```

Use `list [TIER]` to inspect membership and `dry-run TIER WORKSPACE MODE`
to inspect the exact working directory and Cargo argument vector without
executing it. `run` accepts only the documented debug modes; unknown tiers,
invalid mode/workspace pairs, and empty package selections fail before Cargo
is resolved or started. The helper prints each tier's qualification before a
run. In particular, compiling the `deployed_system_proof` or `release`
membership never constitutes deployed proof or release admission.

### Bare Cargo semantics and full coverage

There are no `default-members` in either virtual workspace. Consequently:

- From the repository root, bare `cargo build`, `cargo check`, and `cargo test`
  select all 51 root members. Bare `cargo test` uses each package's default
  test targets.
- From `components/`, the same bare commands select all 31 component members.
  The production guest build remains
  `cargo build --workspace --target wasm32-wasip2`.
- Full CI keeps three package/artifact steps—every root target, every component
  artifact, and the classified non-Cargo input—then runs the fail-closed recipe
  test selector:
  ```bash
  ./tools/workspace-tier run full_ci root test-all
  ./tools/workspace-tier run full_ci components build-wasm
  ./tools/workspace-tier list full_ci
  jco componentize components/samples/node-ts/node.js \
    --wit components/samples/node-ts/wit --world-name node-bench \
    --disable http --disable fetch-event -o /tmp/wamn-node-ts-full-ci.wasm
  ./tools/build-recipe-test-check
  ```
- Neither local full CI nor a successful Cargo build substitutes for the
  owning deployed Job below. Gate-of-record semantics are unchanged.

### Release identity

Release membership is the 16 `deployable: true` packages in
`architecture/package-roles.json`, including the `wamn-gates` proof image.
Membership is not release admission. SR17 must join source revision and
`Cargo.lock` digest to exact artifact SHA-256 and OCI manifest digest; SR26
must join each required gate evidence record back to that same source revision
and artifact/image digests. The exact required fields and fail-closed rule live
in `architecture/workspace-tiers.json`. Cargo defaults, a mutable tag, or an
evidence record that names only a test command are not release evidence.

### Measurement (2026-07-25)

Measurements used debug/default profile on `k11` (8 logical CPUs, i7-1185G7,
60 GiB RAM, NVMe; rustc/cargo 1.97.0) with the isolated target directory
recorded in `architecture/workspace-tiers.json`. Each cold row follows
`cargo clean`; each warm row immediately repeats the identical command.

| Selection and command | Cold | Warm | Cold cache |
|---|---:|---:|---:|
| fast-37 `cargo check` | 125.54s | 0.40s | 1,663,584 KiB / 4,830 files |
| bare root/all-47 `cargo check` | 136.38s | 0.40s | 1,824,188 KiB / 5,024 files |
| fast-37 `cargo build` | 221.45s | 0.43s | 10,479,512 KiB / 9,966 files |
| contract/conformance `cargo test --no-fail-fast` | 181.43s | 2.17s | 5,896,860 KiB / 7,891 files |
| product-3 wasm build | 22.98s | 0.08s | 787,536 KiB / 2,213 files |
| all-18 component wasm build | 23.99s | 0.08s | 922,840 KiB / 3,040 files |
| full root/all-47 `cargo test --workspace --all-targets --no-fail-fast` | 292.37s | 3.38s | 24,321,936 KiB / 20,017 files |

The check comparison saves 10.84s cold and 160,604 KiB by keeping proof/support
and POC roots out of the ordinary production loop; warm no-op cost is
identical. The component comparison saves only 1.01s cold, so the product
selector is justified mainly by artifact intent: fixtures, samples, and POCs
must remain full-CI/proof inputs without becoming product artifacts. Full-CI
root testing ran outside the filesystem sandbox because the builder test owns a
local registry socket; the JSON retains the sandbox-denied attempt separately
so that environment limitation cannot be mistaken for a repository failure.

### Restructure integration proof (wamn-5wd1.8, 2026-07-25)

The final repository-organization proof ran from an isolated worktree with a
dedicated on-disk target directory. These are the executed build and test
boundaries; the bead closure notes retain the exact zero-match obsolete-path
expression and resolved path inventory.

```bash
WAMN_5WD1_8_TARGET=/home/kaalin/dev/wamn/target/wamn-5wd1-8

cargo metadata --locked --offline --format-version 1 \
  > /tmp/wamn-5wd1-8-root-metadata.json
cargo metadata --locked --offline --format-version 1 \
  --manifest-path components/Cargo.toml \
  > /tmp/wamn-5wd1-8-components-metadata.json

CARGO_TARGET_DIR="$WAMN_5WD1_8_TARGET" \
  cargo test --locked --offline -p wamn-proof-conformance --no-fail-fast
CARGO_TARGET_DIR="$WAMN_5WD1_8_TARGET" CARGO_NET_OFFLINE=true \
  ./tools/workspace-tier run full_ci root test-all
CARGO_TARGET_DIR="$WAMN_5WD1_8_TARGET" CARGO_NET_OFFLINE=true \
  ./tools/workspace-tier run full_ci components build-wasm
CARGO_TARGET_DIR="$WAMN_5WD1_8_TARGET" \
  cargo test --locked --offline --manifest-path components/Cargo.toml \
  --workspace --all-targets --no-fail-fast
./tools/build-recipe-test-check

WAMN_5WD1_8_JCO=/tmp/wamn-jco-1.25.2-fresh/node_modules/.bin/jco
test "$("$WAMN_5WD1_8_JCO" --version)" = 1.25.2
"$WAMN_5WD1_8_JCO" componentize components/samples/node-ts/node.js \
  --wit components/samples/node-ts/wit --world-name node-bench \
  --disable http --disable fetch-event -o /tmp/wamn-node-ts-full-ci.wasm
wasm-tools validate /tmp/wamn-node-ts-full-ci.wasm
wasm-tools component wit /tmp/wamn-node-ts-full-ci.wasm
sha256sum /tmp/wamn-node-ts-full-ci.wasm

docker buildx build --check --progress=plain .
git diff --check
jq empty crates/node/manifest/tests/fixtures/sample-echo.manifest.json
```

Result: locked resolved metadata covered 47 root and 18 component packages; 59
named conformance checks passed; every root workspace target passed; all 18
components built for `wasm32-wasip2` in debug and passed their host-side tests;
every documented recipe selector matched at least its required count. Pinned
`jco` 1.25.2 produced a valid 12 MiB component exporting
`wamn:node/handler@0.1.0` (SHA-256
`a165e58901da2442172c5db9490137933a9c6d3fdfc2bff36252bba2bc516b5e`).
BuildKit's static Dockerfile evaluation completed with no warnings. The only
sandbox-specific failure was the builder registry test's loopback bind; the
single test and then the exhaustive root command passed outside the sandbox.

This is repository/debug/structural evidence. It built no release-profile
artifacts or images, pushed no image, and changed no live cluster. The
fail-closed recipe inventory proves that every deployed gate still resolves to
its current package, target, filter, fixture, image, and manifest; it does not
claim that those live gates ran here. The tiered live ladder remains
`wamn-5wd1.9`.

## Gates by bead

### Workspace build

```bash
cargo build --release -p wamn-host -p wamn-ctl -p wamn-dispatcher -p wamn-executor -p wamn-scenario-worker -p wamn-cdc-reader -p wamn-gates   # all artifacts (SR1/SR9 split)
(cd components && cargo build --release --target wasm32-wasip2)  # guest fixtures
```

### Proof tiers

```bash
# Compile and run each proof tier through its owning package.
cargo test -p wamn-proof-conformance -p wamn-proof-integration -p wamn-proof-system

# Repository-only fixture and temporary-infrastructure support.
cargo test -p wamn-test-fixtures -p wamn-test-infrastructure

# Compile the compatibility command router used by the existing Jobs. Its test
# implementations belong to the three proof libraries above.
cargo check -p wamn-gates
```

### [PLAN-2A / wamn-ayq7.16] execution-bundle specialization gates

`wamn-proof-conformance` owns both specialization proofs. Each recipe uses the
debug profile, the locked offline component graph, and the pinned `wac-cli`
0.10.1 composition boundary. `wasm-tools` supplies structural WIT inspection;
the artifact gates verify its output rather than pinning its executable into
execution-bundle identity.

The exact-node arm builds the two driver inputs and three one-node plugs. Its
identity evidence proves cross-flow reuse for equal inputs, exact byte and
provenance equality across rebuilds, and digest locality: the deliberate beta
component-digest mutant invalidates every and only bundle selecting beta. The
artifact evidence also excludes the unused node world and every unselected
capability world.

```bash
set -euo pipefail
WAMN_2A_ROOT_TARGET="${CARGO_TARGET_DIR:-$PWD/target/wamn-plan-2a}"
WAMN_2A_COMPONENT_TARGET="$WAMN_2A_ROOT_TARGET/components"
WAMN_WAC_PATH="$(command -v wac)"
WAMN_WASM_TOOLS_PATH="$(command -v wasm-tools)"
test "$("$WAMN_WAC_PATH" --version)" = "wac-cli 0.10.1"

# recipe-test: PLAN-2A-EXACT | conformance | wamn-proof-conformance | test | exact_node_specialization | - | 2 | exact-node identity locality, cross-flow reuse, and sorted single-plug composition plan
CARGO_TARGET_DIR="$WAMN_2A_ROOT_TARGET" \
  cargo test --locked --offline -p wamn-proof-conformance \
  --test exact_node_specialization
CARGO_TARGET_DIR="$WAMN_2A_COMPONENT_TARGET" \
  cargo build --locked --offline --manifest-path components/Cargo.toml \
  --target wasm32-wasip2 \
  -p exact-driver-alpha -p exact-driver-alpha-beta \
  -p exact-node-alpha -p exact-node-beta -p exact-node-unused
WAMN_EXACT_COMPONENT_DIR="$WAMN_2A_COMPONENT_TARGET/wasm32-wasip2/debug" \
WAMN_EXACT_OUTPUT_DIR="$WAMN_2A_ROOT_TARGET/exact-node-artifacts" \
WAMN_WAC_PATH="$WAMN_WAC_PATH" \
WAMN_WASM_TOOLS_PATH="$WAMN_WASM_TOOLS_PATH" \
CARGO_TARGET_DIR="$WAMN_2A_ROOT_TARGET" \
  cargo test --locked --offline -p wamn-proof-conformance \
  --test exact_node_specialization \
  exact_node_artifacts_compose_deterministically_and_exclude_unused_worlds \
  -- --ignored --exact
```

The capability-class arm reuses the two exact drivers and builds the pure,
HTTP, and Postgres class plugs. Its identity evidence proves equal class sets
reuse one identity and that same-input rebuilds have exact byte and provenance
equality. The deliberate class-member-digest mutants invalidate every and only
bundle carrying that class, including flows that do not select the mutated
member; the artifact evidence retains all members of a selected class while
excluding every unselected class and capability world.

```bash
set -euo pipefail
WAMN_2A_ROOT_TARGET="${CARGO_TARGET_DIR:-$PWD/target/wamn-plan-2a}"
WAMN_2A_COMPONENT_TARGET="$WAMN_2A_ROOT_TARGET/components"
WAMN_WAC_PATH="$(command -v wac)"
WAMN_WASM_TOOLS_PATH="$(command -v wasm-tools)"
test "$("$WAMN_WAC_PATH" --version)" = "wac-cli 0.10.1"

# recipe-test: PLAN-2A-CAPABILITY-CLASS | conformance | wamn-proof-conformance | test | capability_class_specialization | - | 2 | class-set identity reuse, class-wide member blast radius, and sorted single-plug composition plan
CARGO_TARGET_DIR="$WAMN_2A_ROOT_TARGET" \
  cargo test --locked --offline -p wamn-proof-conformance \
  --test capability_class_specialization
CARGO_TARGET_DIR="$WAMN_2A_COMPONENT_TARGET" \
  cargo build --locked --offline --manifest-path components/Cargo.toml \
  --target wasm32-wasip2 \
  -p exact-driver-alpha -p exact-driver-alpha-beta \
  -p capability-class-http -p capability-class-postgres \
  -p capability-class-pure
WAMN_CAPABILITY_CLASS_COMPONENT_DIR="$WAMN_2A_COMPONENT_TARGET/wasm32-wasip2/debug" \
WAMN_CAPABILITY_CLASS_OUTPUT_DIR="$WAMN_2A_ROOT_TARGET/capability-class-artifacts" \
WAMN_WAC_PATH="$WAMN_WAC_PATH" \
WAMN_WASM_TOOLS_PATH="$WAMN_WASM_TOOLS_PATH" \
CARGO_TARGET_DIR="$WAMN_2A_ROOT_TARGET" \
  cargo test --locked --offline -p wamn-proof-conformance \
  --test capability_class_specialization \
  capability_class_artifacts_are_deterministic_and_match_selected_class_worlds \
  -- --ignored --exact
```

### S1/4p3/bp4.1 gates

```bash
# Local (exit-code disciplined since wamn-cjv.1: any failed phase — p99 SLO,
# cap kill at the 256 MiB ceiling, epoch Trap::Interrupt, 64/192 budget
# differentiation — makes bench exit non-zero; job completion IS the verdict):
./target/release/wamn-gates --log-level warn bench \
  --hello components/target/wasm32-wasip2/release/hello.wasm \
  --memhog components/target/wasm32-wasip2/release/memhog.wasm \
  --busyloop components/target/wasm32-wasip2/release/busyloop.wasm
# In-cluster gate of record (no DB/NATS; fixtures ship in the image):
kubectl -n wamn-system apply -f deploy/gates/bench-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/bench --timeout=600s
kubectl -n wamn-system logs job/bench
# Mutation harness (4 mutants, each must exit non-zero): scratchpad/mutate_cjv1.py
```

### S2 gates (qps + p99, saturation, chaos/RLS/injection)

```bash
# Local iteration (throwaway container + the same fixture SQL):
docker run -d --name wamn-pg -p 5450:5432 -e POSTGRES_PASSWORD=postgres \
  -v "$PWD/deploy/sql/postgres-init.sql:/docker-entrypoint-initdb.d/init.sql:ro" postgres:18
./target/release/wamn-gates --log-level error pgbench \
  --pgprobe components/target/wasm32-wasip2/release/pgprobe.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5450/wamn --mode all --skip-multiproject
# --skip-multiproject: under --mode all, no WAMN_PG_ADMIN_URL means the [2.2]
# multiproject gate can't run; this flag declares that its coverage lives in the
# sibling superuser recipe below. Without it, --mode all now REFUSES up front
# (a preflight bail) rather than silently skipping to a false-green (C7-2).
# --mode attack is the wamn-cjv.2 in-band claim-override gate (pgprobe ops 7/8/9);
# recipe-test: H5-S2-GUARDS | unit | wamn-runtime | lib | - | plugins::wamn_postgres::claims::tests::guard_ | 5 | crates/platform/runtime/src/plugins/wamn_postgres/claims.rs claim-mutation SQL guard
cargo test -p wamn-runtime --lib plugins::wamn_postgres::claims::tests::guard_
# Mutation harness (3 guard mutants, each must fail --mode attack): scratchpad/mutate_cjv2.py
# In-cluster gate of record (p99 is measured in-cluster):
kubectl -n wamn-system create configmap pg-init --from-file=init.sql=deploy/sql/postgres-init.sql
kubectl -n wamn-system apply -f deploy/platform/postgres.yaml -f deploy/gates/pgbench-job.yaml
kubectl -n wamn-system logs -f job/pgbench
```

### [2.2] production wamn:postgres

```bash
# Local iteration (same throwaway container as S2, plus WAMN_PG_ADMIN_URL):
WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:5450/wamn \
  ./target/release/wamn-gates --log-level error pgbench \
  --pgprobe components/target/wasm32-wasip2/release/pgprobe.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5450/wamn --mode all
# In-cluster gate of record (co-located, no cpu limit — S2 CFS lesson;
# WAMN_PG_ADMIN_URL is the superuser used only to provision the project DBs):
kubectl -n wamn-system apply -f deploy/gates/pgbench-multiproject-job.yaml
kubectl -n wamn-system logs -f job/pgbench-multiproject
```

#### [R18-NEG] standard_conforming_strings fail-closed (live negative)

The R18 connect-time assert (`standard_conforming_strings_hook`) fails a pool
checkout CLOSED when the server has `standard_conforming_strings` off. The
positive is covered by stock PG; this env-gated live negative proves the
fail-closed branch against a real server booted with the setting off.

```bash
# Throwaway server with the setting OFF (own name/port — do not reuse):
docker run -d --rm --name wamn-lb3-pg -p 5465:5432 -e POSTGRES_PASSWORD=postgres \
  postgres:18 -c standard_conforming_strings=off
# GOTCHA: postgres:18 inits-then-restarts — wait >=10s before the first connect,
# then verify the setting IS off before running the test:
sleep 12 && docker exec wamn-lb3-pg psql -U postgres -c "SHOW standard_conforming_strings"  # => off
# recipe-test: H5-R18-NEG | live-negative | wamn-runtime | lib | - | plugins::wamn_postgres::claims::tests::live_scs_off_server_fails_checkout_closed | 1 | crates/platform/runtime Postgres checkout hook against the throwaway unsafe server
WAMN_SCS_OFF_PG_URL=postgres://postgres:postgres@127.0.0.1:5465/postgres \
  cargo test -p wamn-runtime --lib \
    plugins::wamn_postgres::claims::tests::live_scs_off_server_fails_checkout_closed \
    -- --exact --nocapture
docker stop wamn-lb3-pg
```

### [2.3] managed Postgres provisioning

Docs: docs/archive/platform/provisioning.md

```bash
cargo test -p wamn-control-provision   # naming/slug/reserved-prefix + SQL shape + secret + live-apply
cargo clippy -p wamn-control-provision --all-targets && cargo fmt -p wamn-control-provision --check
# optional plain-PG live-apply (throwaway postgres:18; SUPERUSER url — CREATE
# skips when unset):
docker run -d --rm --name wamn-prov-pg -p 5460:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_PROVISION_PG_URL=postgres://postgres:postgres@127.0.0.1:5460/wamn cargo test -p wamn-control-provision
# locally against the SAME throwaway postgres:18 (superuser):
WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:5460/wamn \
  ./target/debug/wamn-gates --log-level error provisionbench
docker stop wamn-prov-pg
# The production tool is `wamn-ctl provision-project --project <id>
# In-cluster gate of record (against the shared CNPG cluster = the D6 substrate,
# NO cpu limit — S2 CFS lesson):
kubectl apply --server-side -f deploy/infra/cnpg-operator.yaml
kubectl -n cnpg-system rollout status deploy/cnpg-controller-manager --timeout=150s
kubectl apply -f deploy/infra/cnpg-cluster.yaml
kubectl -n wamn-system wait --for=jsonpath='{.status.readyInstances}'=1 cluster/wamn-pg --timeout=300s
# A HOST change => full docker rebuild (both --target stages + kind load BOTH images):
docker build --target host -t wamn-host:dev . && docker build --target gates -t wamn-gates:dev .
kind load docker-image wamn-host:dev --name wamn && kind load docker-image wamn-gates:dev --name wamn
kubectl -n wamn-system apply -f deploy/gates/provisionbench-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/provisionbench --timeout=180s
kubectl -n wamn-system logs job/provisionbench
```

### S3 gates

```bash
./target/release/wamn-gates --log-level error flowbench \
  --flowrunner components/target/wasm32-wasip2/release/flowrunner.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5450/wamn --mode all
# In-cluster (same co-located / no-cpu-limit Job topology as pgbench):
kubectl -n wamn-system apply -f deploy/gates/flowbench-job.yaml
kubectl -n wamn-system logs -f job/flowbench
```

### S4 gates

```bash
# Two extra fixtures need external tools (one-time installs):
# composition are extra steps:
jco componentize components/samples/node-ts/node.js --wit components/samples/node-ts/wit \
  --world-name node-bench --disable http --disable fetch-event \
  -o components/samples/node-ts/node-ts.wasm
REL=components/target/wasm32-wasip2/release
wac plug $REL/flow_driver.wasm --plug $REL/node_rs.wasm -o $REL/flow_composed.wasm
./target/release/wamn-gates --log-level error nodebench \
  --node-rs $REL/node_rs.wasm --node-ts components/samples/node-ts/node-ts.wasm \
  --composed $REL/flow_composed.wasm --sample $REL/sample_node.wasm --mode all
# In-cluster gate of record (real cross-pod hop via the serve-node-gate Service; the
# gap/config gates run in-pod; no cpu limit — the S2 CFS lesson). The fixture is
# named serve-node-gate, so it coexists with the platform serve-node Deployment —
# no need to re-apply deploy/platform/serve-node.yaml afterward (wamn-bczu):
kubectl -n wamn-system apply -f deploy/gates/serve-node.yaml
kubectl -n wamn-system rollout status deploy/serve-node-gate --timeout=120s
kubectl -n wamn-system apply -f deploy/gates/nodebench-job.yaml
kubectl -n wamn-system logs -f job/nodebench
```

### S5 gates

```bash
# Local iteration (throwaway loki + collector on a docker network):
docker network create wamn-s5 2>/dev/null || true
docker run -d --name wamn-s5-loki --network wamn-s5 -p 3100:3100 \
  -v "$PWD/deploy/infra/loki-local.yaml:/etc/loki/loki.yaml:ro" \
  grafana/loki:3.4.2 -config.file=/etc/loki/loki.yaml
docker run -d --name wamn-s5-otelcol --network wamn-s5 -p 4317:4317 -p 8888:8888 \
  -v "$PWD/deploy/infra/otelcol-local.yaml:/etc/otelcol/config.yaml:ro" \
  otel/opentelemetry-collector-contrib:0.115.1 --config=/etc/otelcol/config.yaml
OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4317 RUST_LOG=error \
  LOKI_URL=http://127.0.0.1:3100 COLLECTOR_METRICS_URL=http://127.0.0.1:8888/metrics \
  ./target/release/wamn-gates --log-level info logbench \
  --logspewer components/target/wasm32-wasip2/release/logspewer.wasm --mode all
# In-cluster gate of record (real Loki + collector; no cpu limit — the S2 lesson):
kubectl -n wamn-system apply -f deploy/infra/loki.yaml -f deploy/infra/otel-collector.yaml
kubectl -n wamn-system rollout status deploy/loki deploy/otel-collector --timeout=120s
kubectl -n wamn-system apply -f deploy/gates/logbench-job.yaml
kubectl -n wamn-system logs -f job/logbench
```

### [9.1] OTel trace pipeline

Docs: docs/archive/observability/tracing.md

```bash
# Static proof spans the thin host artifact and the runtime library that owns
# its logging/plugin implementation boundary.
cargo clippy -p wamn-host -p wamn-runtime -p wamn-dispatcher -p wamn-gates --all-targets \
  && cargo fmt -p wamn-host -p wamn-runtime -p wamn-dispatcher -p wamn-gates --check
cargo build -p wamn-dispatcher -p wamn-gates   # tracebench spawns the sibling service binary
# Local iteration (throwaway Postgres + Tempo + collector on a docker network;
# spans are INFO):
docker network create wamn-s5 2>/dev/null || true
docker run -d --rm --name wamn-trace-pg --network wamn-s5 -p 5482:5432 \
  -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=wamn postgres:18
docker run -d --name wamn-s5-tempo --network wamn-s5 -p 3200:3200 \
  -v "$PWD/deploy/infra/tempo-local.yaml:/etc/tempo/tempo.yaml:ro" \
  grafana/tempo:2.6.1 -config.file=/etc/tempo/tempo.yaml
docker run -d --name wamn-s5-otelcol --network wamn-s5 -p 4317:4317 -p 8888:8888 \
  -v "$PWD/deploy/infra/otelcol-local.yaml:/etc/otelcol/config.yaml:ro" \
  otel/opentelemetry-collector-contrib:0.115.1 --config=/etc/otelcol/config.yaml
OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4317 OTEL_EXPORTER_OTLP_PROTOCOL=grpc \
  OTEL_BSP_SCHEDULE_DELAY=1000 RUST_LOG=error \
  ./target/debug/wamn-gates --log-level info tracebench \
  --pgprobe components/target/wasm32-wasip2/release/pgprobe.wasm \
  --database-url postgres://postgres:postgres@127.0.0.1:5482/wamn \
  --tempo-url http://127.0.0.1:3200
docker stop wamn-trace-pg wamn-s5-tempo wamn-s5-otelcol
# In-cluster gate of record (real Tempo + collector + Postgres, no cpu limit —
# --target stages + kind load BOTH images):
docker build --target host -t wamn-host:dev . && docker build --target gates -t wamn-gates:dev .
kind load docker-image wamn-host:dev --name wamn && kind load docker-image wamn-gates:dev --name wamn
kubectl -n wamn-system apply -f deploy/infra/tempo.yaml -f deploy/infra/otel-collector.yaml
kubectl -n wamn-system rollout status deploy/tempo deploy/otel-collector --timeout=120s
kubectl -n wamn-system apply -f deploy/gates/tracebench-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/tracebench --timeout=180s
kubectl -n wamn-system logs job/tracebench
```

### [9.8] OTel metric set

Docs: docs/archive/observability/metrics.md

```bash
# Unit proof: the memory instruments live in wamn-runtime; execution/service
# supervision and the metricbench assertions retain their separate owners.
# recipe-test: H5-METRIC-RUNTIME | unit | wamn-runtime | lib | - | memory_metrics::tests:: | 2 | crates/platform/runtime/src/memory_metrics.rs instrument snapshots
cargo test -p wamn-runtime --lib memory_metrics::tests::
cargo test -p wamn-execution-host -p wamn-executor -p wamn-dispatcher --no-fail-fast
# recipe-test: H5-METRIC-PROOF | integration | wamn-proof-integration | lib | - | metricbench::tests:: | 7 | tests/integration/src/metricbench.rs hermetic release fixture, executor process boundary, URL isolation, scrape parsing, and body assembly
cargo test -p wamn-proof-integration --lib metricbench::tests::
cargo build -p wamn-dispatcher -p wamn-executor -p wamn-gates   # metricbench spawns both sibling service binaries
# Local iteration: a throwaway Postgres (+ the NOSUPERUSER wamn_app role and the
# host-only NOLOGIN wamn_scenario_author role the canonical DDL GRANTs to) and the
# local collector with the new :8889 metrics pipeline. metricbench creates and
# drops its own database, applies the canonical catalog/run-plane DDL, drives the
# executor and dispatcher through their binaries, then scrapes :8889 for the
# real run/queue/pool/memory families.
docker run -d --name lane-metric-pg -e POSTGRES_PASSWORD=pg -p 127.0.0.1:15503:5432 postgres:18
# (postgres:18 inits-then-restarts — pg_isready lies during socket-only init; if the
# first connection is refused, wait a few seconds and retry)
until docker exec lane-metric-pg pg_isready -U postgres; do sleep 1; done
docker exec -e PGPASSWORD=pg lane-metric-pg psql -U postgres -c \
  "CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS;" -c \
  "CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
     NOINHERIT NOREPLICATION NOBYPASSRLS;"
docker run -d --name lane-metric-otelcol -p 127.0.0.1:4317:4317 -p 127.0.0.1:8889:8889 \
  -v "$PWD/deploy/infra/otelcol-local.yaml:/etc/otelcol/config.yaml:ro" \
  otel/opentelemetry-collector-contrib:0.115.1 --config=/etc/otelcol/config.yaml
OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4317 OTEL_EXPORTER_OTLP_PROTOCOL=grpc \
  OTEL_METRIC_EXPORT_INTERVAL=1000 RUST_LOG=error \
  ./target/debug/wamn-gates --log-level info metricbench \
  --flowrunner components/target/wasm32-wasip2/release/flowrunner.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:15503/postgres \
  --admin-database-url postgres://postgres:pg@127.0.0.1:15503/postgres \
  --metrics-url http://127.0.0.1:8889/metrics
docker rm -f lane-metric-pg lane-metric-otelcol
# Phases 1-5 PASS; phase 6 (api RPS) honest-skips (in-cluster only — ProxyPre
# bypasses the host HTTP server). In-cluster gate of record (real collector +
# Postgres, no cpu limit — the :8889 metrics pipeline rides the same collector):
docker build --target host -t wamn-host:dev . && docker build --target gates -t wamn-gates:dev .
kind load docker-image wamn-host:dev --name wamn && kind load docker-image wamn-gates:dev --name wamn
kubectl -n wamn-system apply -f deploy/infra/otel-collector.yaml
kubectl -n wamn-system rollout status deploy/otel-collector --timeout=120s
kubectl -n wamn-system apply -f deploy/gates/metricbench-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/metricbench --timeout=300s
kubectl -n wamn-system logs job/metricbench
```

### [9.2] trace context propagation

Docs: docs/archive/platform/wash-runtime-fork.md, docs/archive/observability/tracing.md

```bash
cargo test -p wamn-node-sdk -p wamn-standard-nodes   # trace_headers/apply + http-node forward + explicit-header-wins
# System-proof boundary: traceproof independently invokes the public P2 and P3
# host surfaces, captures only their post-host headers, and sends those captured
# headers across the pod boundary without guest help.
# recipe-test: H5-TRACEPROOF | system | wamn-proof-system | lib | - | traceproof::tests:: | 5 | tests/system/src/traceproof.rs independent P2/P3 host-enforced W3C trace injection and keep-alive response framing
cargo test -p wamn-proof-system --lib traceproof::tests::
cargo clippy -p wamn-node-sdk -p wamn-standard-nodes -p wamn-proof-system -p wamn-gates \
  --all-targets -- -D warnings
cargo fmt -p wamn-node-sdk -p wamn-standard-nodes -p wamn-proof-system -p wamn-gates --check

# No component fixture: the gate invokes both public host surfaces directly.
docker build --target gates -t wamn-gates:dev .
kind load docker-image wamn-gates:dev --name wamn
kubectl -n wamn-system apply -f deploy/infra/otel-collector.yaml
kubectl -n wamn-system rollout status deploy/otel-collector --timeout=120s
kubectl -n wamn-system apply -f deploy/gates/serve-echo.yaml
kubectl -n wamn-system rollout restart deploy/serve-echo
kubectl -n wamn-system rollout status deploy/serve-echo --timeout=120s
kubectl -n wamn-system delete job traceproof --ignore-not-found
kubectl -n wamn-system apply -f deploy/gates/traceproof-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/traceproof --timeout=180s
kubectl -n wamn-system logs job/traceproof
```

### S6 gates

```bash
# Local iteration (throwaway container + the same fixture SQL):
docker run -d --name wamn-pg -p 5450:5432 -e POSTGRES_PASSWORD=postgres \
  -v "$PWD/deploy/sql/postgres-init.sql:/docker-entrypoint-initdb.d/init.sql:ro" postgres:18
./target/release/wamn-gates --log-level error testhostbench \
  --flowrunner components/target/wasm32-wasip2/release/flowrunner.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5450/wamn \
  --admin-database-url postgres://postgres:postgres@127.0.0.1:5450/wamn --mode all
# In-cluster gate of record (co-located with Postgres, no cpu limit — S2 lesson;
# WAMN_PG_ADMIN_URL is the superuser used only to provision the ephemeral schema):
kubectl -n wamn-system apply -f deploy/gates/testhostbench-job.yaml
kubectl -n wamn-system logs -f job/testhostbench
```

### [11.4] assertion library (testkitbench)

Docs: docs/archive/testing/scenario-model.md · Crate: crates/scenarios/model · Fixture: deploy/gates/testkit-cases.json

```bash
# Unit tests (the pure vocabulary: serde drift-guards, subset semantics, the
# evaluate() truth table, ExactlyThese set-equality):
cargo test -p wamn-scenario-model

# The gate loads a checked-in Vec<TestCase>, drives node-level cases through a
# warm ServeNode and flow-level cases through the scenario capability set, and
# folds each evaluate() AssertionResult into a PASS/FAIL line.
# Local iteration (throwaway container + the same fixture SQL):
docker run -d --name wamn-pg -p 5450:5432 -e POSTGRES_PASSWORD=postgres \
  -v "$PWD/deploy/sql/postgres-init.sql:/docker-entrypoint-initdb.d/init.sql:ro" postgres:18
# (node cases need only the wasm; flow cases need the DB URLs)
./target/release/wamn-gates --log-level error testkitbench \
  --cases deploy/gates/testkit-cases.json \
  --node components/target/wasm32-wasip2/release/disposition_node.wasm \
  --flowrunner components/target/wasm32-wasip2/release/flowrunner.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5450/wamn \
  --admin-database-url postgres://postgres:postgres@127.0.0.1:5450/wamn
# In-cluster gate of record:
kubectl -n wamn-system apply -f deploy/gates/testkitbench-job.yaml
kubectl -n wamn-system logs -f job/testkitbench
```

#### [11.2-exec / wamn-0lfu] stored scenarios

`wamn-scenario-worker` is the product artifact. It reads one stored suite and
executes each root case run in a distinct caller-provisioned run schema through
the same flowrunner component used by the production executor. Every root run
also gets a fresh Postgres plugin/pool and `ExecutionHost`; a pool therefore
never reuses prepared statements across run schemas. Callable child subflows
remain inside their root case run's schema/pool/host; this boundary does not
provision a separate schema for each child `runs` row. The required
`--execution-schema-template` contains `{ordinal}` exactly once (for example,
`scenario_run_{ordinal}`), so case isolation is structural without giving the
worker schema-creation credentials. It resumes parked work with virtual time,
evaluates the scenario-model assertions, and emits a JSON report.
Before loading the guest or executing a node, it resolves exactly one applied
`catalog.catalog_heads` → `release_flows` / `release_manifests` →
`flow_artifacts` member and verifies the stored canonical artifact, including
its occurrence-recovery selection. Missing, ambiguous, mismatched, or
unverifiable release state is a pre-execution refusal; there is no mutable
`flows` fallback. Immediately before each root run, one transaction locks and
rechecks that catalog head, rechecks the exact member and artifact hash, then
atomically inserts the fully pinned run and queue row. SQL constructs the
versioned trusted invocation principal from that verified release member.
Its deterministic clock, random, credentials, and recording/deny egress
adapters come from `wamn-scenario-runtime`; none are linked into
`wamn-executor`.

```bash
cargo test -p wamn-scenario-model -p wamn-scenario-catalog \
  -p wamn-scenario-runtime -p wamn-scenario-worker
cargo test --locked --offline -p wamn-run-state --test queue
cargo test --locked --offline -p wamn-test-infrastructure --lib scenario_worker_gate::tests
cargo run -p wamn-scenario-worker -- --help

# DbState adapter gate of record (disposable PostgreSQL 18; no kind rollout):
docker run -d --name wamn-dbstate-proof -p 55439:5432 \
  -e POSTGRES_PASSWORD=postgres postgres:18
until docker exec wamn-dbstate-proof pg_isready -U postgres; do sleep 1; done
WAMN_DB_STATE_TEST_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:55439/postgres \
  cargo test -p wamn-scenario-runtime --test db_state_live -- --ignored --nocapture
docker rm -f wamn-dbstate-proof
```

#### [PLAN-6A / wamn-ftfc.13] public authoring contract

`wamn-authoring-model` is the pure, frontend-neutral command and projection
contract used by Git, CLI, API, and future visual clients. The package gate
pins every command/result/refusal shape, rejects unversioned and privileged or
frontend-specific fields, and keeps stable node/branch/full-edge identity plus
explicit observation states in the generated JSON Schema.

```bash
CARGO_TARGET_DIR=/tmp/wamn-target-ftfc-13 \
  cargo test --locked --offline -p wamn-authoring-model
CARGO_TARGET_DIR=/tmp/wamn-target-ftfc-13 \
  cargo clippy --locked --offline -p wamn-authoring-model \
  --all-targets -- -D warnings
CARGO_TARGET_DIR=/tmp/wamn-target-ftfc-13 \
  cargo test --locked --offline -p wamn-proof-conformance \
  --test package_architecture --test workspace_tiers
cargo fmt -p wamn-authoring-model --check

# Regenerate after changing public types; the package drift test pins the bytes.
CARGO_TARGET_DIR=/tmp/wamn-target-ftfc-13 \
  cargo run --locked --offline -p wamn-authoring-model \
  --example print-authoring-surface-schema \
  > docs/archive/contracts/authoring-surface.schema.json
```

#### [PLAN-6A / wamn-ftfc.11] flow-authoring loop

This ignored gate owns one disposable PostgreSQL database. It provisions the
canonical `wamn_app` login and a distinct login inheriting only the NOLOGIN
`wamn_scenario_author` role, then drives the public process-local authoring
backend through save, validation, draft and release execution, exact retry,
catalog-head drift, and capture-interrupted recovery. Validation and execution
consume the same flowrunner component compiled from the current checkout. On a
green run the test drops its two run schemas, `catalog`, and all three roles;
removing the disposable container is the failure-path cleanup.

```bash
CARGO_TARGET_DIR=/tmp/wamn-target-ftfc-11 \
  cargo build --locked --offline --manifest-path components/Cargo.toml \
  --target wasm32-wasip2 -p flowrunner

docker run --rm -d --name wamn-ftfc11-pg \
  -p 127.0.0.1:15623:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
until docker exec wamn-ftfc11-pg pg_isready -U postgres -d wamn; do sleep 1; done

WAMN_AUTHORING_LOOP_ADMIN_PG_URL=postgresql://postgres:postgres@127.0.0.1:15623/wamn \
WAMN_AUTHORING_LOOP_AUTHOR_PG_URL=postgresql://wamn_authoring_loop_author:wamn-author-live@127.0.0.1:15623/wamn \
WAMN_AUTHORING_LOOP_APP_PG_URL=postgresql://wamn_app:wamn-app-live@127.0.0.1:15623/wamn \
WAMN_AUTHORING_LOOP_FLOWRUNNER=/tmp/wamn-target-ftfc-11/wasm32-wasip2/debug/flowrunner.wasm \
CARGO_TARGET_DIR=/tmp/wamn-target-ftfc-11 \
  cargo test --locked --offline -p wamn-scenario-worker \
  --test authoring_loop_live authoring_loop_live -- --ignored --exact --nocapture

docker stop wamn-ftfc11-pg
```

The retained `testkitbench --suite / --impact-report` path is the compatibility
and integration proof for previously shipped gates:

The 11.4 `testkitbench` subcommand doubles as the STORED-suite EXECUTOR: it loads
`test_suites` / `test_cases` rows from a schema, re-validates each `case_body`
against the `wamn-scenario-model` vocabulary on READ, and executes each case as its OWN
run through scenario-runtime — a FRESH ephemeral schema per case (the source
schema is read-only), the verified graph read from the exact applied immutable
catalog release member,
`ScenarioCapabilities::virtualized` + `RecordingEgress` (trusted
`--allowed-hosts` outer policy intersected with the flow's declared policy;
case assertions never authorize) + `ExecutionHost` + drain, then
`wamn_scenario_model::evaluate` per case. The hermetic success suite contains
two root runs and requires each run's private `sink` to contain exactly one row,
so collapsing both runs onto one schema turns the gate red. One `check` line per assertion + a
per-suite/summary line; nonzero exit on any failure.

Selection (exactly one source; `--cases` file mode is preserved unchanged):
- `--suite <flow_id>@<version> --tenant <t> --source-schema <s>` — runs EVERY
  suite of that flow version (single tenant).
- `--impact-report <path>` — a JSON array of `SuiteSelector`
  `{tenant, flow_id, flow_version, suite_id}`, the flattened
  `wamn_schema_control::impact::SuiteEdge` tuples. Since wamn-gn6b that type
  carries its own `Serialize`/`Deserialize` (`deny_unknown_fields`) and
  `impact::suite_selectors[_json]` emits the array. `SuiteSelector` remains a
  LOCAL deserialize struct in the executor — deliberately, so the gate keeps a
  reader independent of the producer — and two named tests hold the two halves
  together: `suite_selector_matches_the_suite_edge_shape` (field-for-field shape
  + unknown-field rejection) and `flattened_impact_suites_deserialize_as_suite_selectors`
  (a real `suite_selectors_json` array round-trips into the executor's struct).
  Both live in `test-support/infrastructure/scenario_worker_gate.rs`.
- `--seed-demo` — the hermetic gate: self-seeds `--source-schema` (production
  `wamn-ctl publish-catalog --provision --runstate` process boundary) with a
  drivable `request → postgres(create sink) → respond` release member and an
  undrivable `request → disposition-recommendation → respond` member pinned to
  the exact `--node` component digest. It poisons the legacy `flows` projections,
  runs success/malformed/refusal/assertion-failure suites, and verifies exact
  root-run pins before cleanup. No external data or live egress target.

RLS posture: the fixture adapter enumerates source suites/case ordinals via its
ADMIN (superuser, RLS-bypassing) session with an explicit
`(tenant, flow_id, flow_version [, suite_id])` predicate. The product worker
re-reads case bodies and the immutable catalog release through its tenant-scoped
`wamn_app` session; the running flow uses that same role in the isolated case
schema. The `flow-tests.sql` FORCE-RLS floor is untouched. SQL read builders:
`wamn_scenario_catalog::sql::{select_suites_for_flow_sql, select_cases_for_suite_sql}`
(drift-guarded against `deploy/sql/flow-tests.sql`).

Drivability refusal (cross-lane contract): before driving, the executor checks
the graph's `nodes[].type` against the drivable set — the flowrunner built-in
dispatch arms (`BUILTIN_NODE_TYPES`, drift-guarded against
`components/execution/flowrunner/src/lib.rs`) ∪ the standard node library
(`STANDARD_NODE_TYPES`, drift-guarded against `crates/execution/standard-nodes/src/lib.rs`
`NODE_TYPES` name+count). A flow with a guest-baked type (F1's
`validate-receipt`/`upsert-receipt`/`evaluate-specs`/`create-holds`) → a typed
per-suite SKIP naming the undrivable types (NOT a crash, NOT a silent pass), so
F1 refuses cleanly while F3/F4 (std nodes) drive.

```bash
# Unit tests (SuiteSelector = SuiteEdge shape, i32→u32 version boundary,
# selection exclusivity, drivability, coherence, egress authorization separation,
# node-set drift guards) + the flow-tests sql drift/predicate guards:
# Integration proof: the router delegates testkitbench to the integration
# library; scenario catalog retains its own storage tests.
# recipe-test: H5-TESTKITBENCH | integration | wamn-proof-integration | lib | - | testkitbench::tests:: | 1 | tests/integration/src/testkitbench.rs scenario-runtime ownership guard
cargo test -p wamn-proof-integration --lib testkitbench::tests::
cargo test -p wamn-scenario-catalog

# The producer/consumer seam itself (wamn-gn6b): the executor's LOCAL
# SuiteSelector must stay field-for-field with wamn_schema_control SuiteEdge, and
# a real suite_selectors_json array must deserialize into it.
# recipe-test: H5-SUITE-SELECTOR | integration | wamn-test-infrastructure | lib | - | scenario_worker_gate::tests::suite_selector | 1 | test-support/infrastructure/scenario_worker_gate.rs SuiteEdge shape guard
cargo test -p wamn-test-infrastructure --lib scenario_worker_gate::tests::suite_selector
cargo test -p wamn-test-infrastructure --lib \
  scenario_worker_gate::tests::flattened_impact_suites_deserialize_as_suite_selectors

# Checked-in PLAN-0.2 scenario/replay/impact mutation campaign. `check` pins
# clean source hashes; `green-all` runs one debug gate per 11 named mutants; `run-all`
# requires every fixed mutant to turn red and restores each target byte-exactly.
CARGO_TARGET_DIR=/tmp/wamn-target-2jdm-5-3 \
  tools/gate-mutants/scenario-replay-impact.sh check
CARGO_TARGET_DIR=/tmp/wamn-target-2jdm-5-3 \
  tools/gate-mutants/scenario-replay-impact.sh green-all
CARGO_TARGET_DIR=/tmp/wamn-target-2jdm-5-3 \
  tools/gate-mutants/scenario-replay-impact.sh run-all
cargo test --locked -p wamn-proof-conformance --test gate_mutation_evidence
# Immutable green/red evidence:
# architecture/evidence/mutations/scenario-replay-impact.json

# Local FULL gate (throwaway PG). `--seed-demo` requires the product ctl and
# scenario-worker binaries plus the exact flowrunner and disposition-node wasm
# inputs. `--keep` preserves the source schema for the follow-on impact example:
cargo build --locked -p wamn-ctl -p wamn-scenario-worker -p wamn-gates
# If the component release artifacts are not already present:
(cd components && cargo build --locked --release --target wasm32-wasip2 \
  -p flowrunner -p disposition-node)
docker run -d --name lane0lfu-pg -p 15617:5432 -e POSTGRES_PASSWORD=postgres postgres:18
until docker exec lane0lfu-pg pg_isready -U postgres; do sleep 1; done
# catalog-schema.sql (applied by the gate) GRANTs to the host-only NOLOGIN author
# role, which nothing on a bare container creates — bootstrap it up front.
docker exec lane0lfu-pg psql -U postgres -c \
  "CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
     NOINHERIT NOREPLICATION NOBYPASSRLS;"
export ADMIN=postgres://postgres:postgres@127.0.0.1:15617/postgres
export APP=postgres://wamn_app:wamn_app@127.0.0.1:15617/postgres
REL=components/target/wasm32-wasip2/release
NODE=$REL/disposition_node.wasm
SUFFIX=$(sha256sum "$NODE" | cut -c1-12)
DEMO_FLOW_ID=tk-demo-flow-$SUFFIX
WAMN_CTL_BIN="$PWD/target/debug/wamn-ctl" \
  ./target/debug/wamn-gates --log-level error testkitbench \
  --seed-demo --keep --tenant demo-tenant --source-schema wamn_suiteexec \
  --scenario-worker "$PWD/target/debug/wamn-scenario-worker" \
  --node "$NODE" --flowrunner "$REL/flowrunner.wasm" \
  --database-url "$APP" --admin-database-url "$ADMIN"
# --impact-report over the SAME seeded suite (a JSON array of SuiteEdge tuples):
printf '[{"tenant":"demo-tenant","flow_id":"%s","flow_version":1,"suite_id":"success"}]\n' \
  "$DEMO_FLOW_ID" > /tmp/impact.json
./target/debug/wamn-gates --log-level error testkitbench \
  --impact-report /tmp/impact.json --source-schema wamn_suiteexec \
  --scenario-worker "$PWD/target/debug/wamn-scenario-worker" \
  --flowrunner "$REL/flowrunner.wasm" --database-url "$APP" \
  --admin-database-url "$ADMIN"
# The seed invocation itself also proves the undrivable member is refused before
# node execution while naming `disposition-recommendation`, with zero admitted runs.
docker rm -f lane0lfu-pg

# In-cluster gate of record (hermetic --seed-demo; the exact gates image carries
# wamn-ctl, wamn-scenario-worker, flowrunner, and disposition-node):
kubectl -n wamn-system apply -f deploy/gates/suiteexec-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/suiteexec --timeout=180s
kubectl -n wamn-system logs job/suiteexec

# Wave-end COMPOSITION gate (integrator-run, over the parallel lane's stored
# POC suites in poc_f1 — F1 refuses cleanly, F3/F4 drive). The exact shape:
#   testkitbench --suite <flow_id>@<version> --tenant <poc-tenant> \
#     --source-schema poc_f1 --flowrunner /bench/flowrunner.wasm
#     --database-url $WAMN_PG_URL --admin-database-url $WAMN_PG_ADMIN_URL
# (or --impact-report over the flattened SuiteEdge tuples). F3/F4 flows that make
# real egress (ERP/notify) need a reachable target or the case's egress asserts
# to expect the exact authority — see (h) in the executor docs.

# ROOT-RUN ISOLATION: a fresh exec schema, plugin/pool, and host per stored-suite
# CASE/root run (db-state asserts see only that run's writes). Child subflows
# share the root case run's isolation boundary. Suites are small; canonical
# run-plane provisioning remains sub-second per case locally.
# The checked-in scenario/replay/impact campaign above owns the suite-selection,
# case-isolation, aggregate-fold, RLS, replay, and impact-traversal mutants and
# their immutable green/red evidence.
# wamn-jole's focused mutation collapses two root runs onto ordinal 0; the
# named identity test must turn red and the source is restored byte-exactly.
CARGO_TARGET_DIR=/home/kaalin/dev/wamn/target \
  tools/gate-mutants/scenario-run-isolation.sh run
```

### [11.3 / wamn-htn] record-and-replay fixtures (pin-run + pinproof)

Docs: docs/archive/testing/scenario-model.md → "Record-and-replay: pin a run". The `pin_run`
transform (a `wamn_run_state` run + its `node_runs` → a `wamn_scenario_model::TestCase`)
lives in `wamn-scenario-catalog`, while the additive `normalize` vocabulary
(`ignore-paths` + `canonicalize`, no regex) stays in the pure model. The `wamn-ctl pin-run`
verb is the effect shell (app-role read + pure pin + INSERT into
`test_suites`/`test_cases`); secrets are scrubbed at pin time (even from a `full`
run), volatile ids/timestamps are normalized, and an `off`/`preview` run is
refused (`PinError::NotCaptured`).

```bash
# Unit tests (pure pin/normalize logic + the run-store pin read builders):
cargo test -p wamn-scenario-model -p wamn-scenario-catalog -p wamn-run-state -p wamn-ctl

# pinproof (host-side, provisions an ephemeral schema via the SAME ensure_* path
# production uses; seeds a full-capture run carrying a secret + volatile fields,
# pins it via the REAL ctl core, asserts scrub + normalize + replay round-trip
# (volatile mutation passes, real mutation fails) + preview-run refusal). Any
# throwaway PG works (it provisions the wamn_app role + schema itself):
docker run -d --name wamn-pg -e POSTGRES_PASSWORD=postgres -p 5461:5432 postgres:18
WAMN_PG_URL=postgres://wamn_app:wamn_app@127.0.0.1:5461/postgres \
WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:5461/postgres \
  ./target/debug/wamn-gates --log-level error pinproof
docker rm -f wamn-pg
# IN-CLUSTER: deploy/gates/pinproof-job.yaml (kubectl apply; wait complete; logs).
# 3 mutants killed (apply/test/restore, debug builds): M1 skip scrub-on-pin →
# pin_full_run_scrubs_secrets (+ pinproof SCRUB assert); M2 treat None output as
# replayable → pin_preview_run_is_refused (+ pinproof REFUSE assert); M3 normalize
# no-op / over-removes → replay_round_trip_tolerates_volatile_but_rejects_real (+
# normalize_collapses_volatile_but_keeps_real_on_both_sides).
```

### [2.6] DB-path egress review

Docs: docs/archive/data-path/security-db-path.md

```bash
REL=components/target/wasm32-wasip2/release
# E17 polarity (wamn-o3u6): first-party DB-touching workload via --flowrunner;
# genuinely allowlist-clean tenants via --component; wamn:postgres importers MUST
# be REFUSED via --reject-tenant. (Pre-E17 this swept everything under --component,
# which now FAILS: the allowlist v1 refuses the wamn:postgres importers.)
./target/release/wamn-gates --log-level warn egressbench \
  --flowrunner $REL/flowrunner.wasm \
  --component $REL/sample_node.wasm --component $REL/hello.wasm \
  --reject-tenant $REL/pgprobe.wasm \
  --reject-tenant $REL/api_gateway.wasm
  # sample-node: ZERO egress; hello: wasi:cli/clocks/io only — both CLEAR the
  # allowlist. pgprobe/api-gateway import wamn:postgres → refused.
  # node-rs / flow-composed are nodebench fixtures (import the bench-only
  # wamn:nodebench) — exercised by the nodebench gate, not this DB-path review.

# Static proof spans the host artifact, reusable runtime/execution adapters,
# component import policy, executor service, and proof owners.
cargo clippy -p wamn-host -p wamn-runtime -p wamn-component-policy \
  -p wamn-execution-host -p wamn-executor -p wamn-gates -p wamn-gate-harness --all-targets \
  && cargo fmt -p wamn-host -p wamn-runtime -p wamn-component-policy \
    -p wamn-execution-host -p wamn-executor -p wamn-gates -p wamn-gate-harness --check

# E13/E15 runtime raw-socket deny + E17 rejection (wamn-o3u6), the in-cluster
# gate of record. sockprobe independently executes the P2 TcpConnect,
# UdpConnect, UdpOutgoingDatagram, and service/non-loopback UdpBind arms through
# the production host store path. Raw egress is DENIED by default and PERMITTED
# only under wamn.allow-raw-sockets; UdpBind remains service-loopback-only. The
# conformance proof resolves exact linked wash-runtime 2.6.1 revision
# 09b1132f2bab36e6e71f4637bd0e4755e359dd43 and pins the shared policy plus every
# P2/P3 mirror call site. --reject-tenant asserts a wamn:postgres importer
# (pgprobe) is refused by the allowlist v1 (E17). Runs locally without a cluster:
./target/release/wamn-gates --log-level warn egressbench \
  --flowrunner $REL/flowrunner.wasm \
  --reject-tenant $REL/pgprobe.wasm \
  --sockprobe $REL/sockprobe.wasm
# and in-cluster (fixtures baked in the wamn-gates image; no DB/NATS):
kubectl -n wamn-system apply -f deploy/gates/egressbench-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/egressbench --timeout=300s
kubectl -n wamn-system logs job/egressbench
```

### [E13a] publish-time egress-guard refusal (socketguard)

Docs: docs/archive/data-path/security-db-path.md · Manifest: deploy/gates/socketguard-job.yaml

```bash
# Hermetic: synthesizes P2 and P3 wasi:sockets importers (both must be REFUSED at
# publish) and a standard world (must publish) in-process — no registry, no
# fixtures, no DB, so the local run IS the whole gate. Unlike egressbench (which
# walks the shipped components), this independently proves the guard REJECTS
# adversarial worlds for both ABIs.
# Conformance proof: egressbench and socketguard live behind the router in the
# conformance library. The shared classifier itself lives in component-policy.
# recipe-test: H5-EGRESSBENCH | conformance | wamn-proof-conformance | lib | - | egressbench::tests:: | 19 | tests/conformance/src/egressbench.rs arm-specific P2 runtime denial/opt-in assertions and exact linked-fork P2/P3 mirror guards
cargo test -p wamn-proof-conformance --lib egressbench::tests::
# recipe-test: H5-SOCKETGUARD | conformance | wamn-proof-conformance | lib | - | socketguard::tests:: | 3 | tests/conformance/src/socketguard.rs P2/P3 publish refusal and standard-workload control
cargo test -p wamn-proof-conformance --lib socketguard::tests::
# recipe-test: H5-COMPONENT-POLICY | policy-unit | wamn-component-policy | lib | - | tests:: | 20 | crates/platform/component-policy/src/lib.rs import classifiers, P2/P3 socket-package refusal, and derived grants
cargo test -p wamn-component-policy --lib tests::
./target/release/wamn-gates --log-level warn socketguard
# in-cluster sweep (carries the hermetic gate alongside egressbench-job):
kubectl -n wamn-system apply -f deploy/gates/socketguard-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/socketguard --timeout=120s
kubectl -n wamn-system logs job/socketguard
```

### [11.5] custom-node test gate (testgate)

Docs: docs/archive/platform/builder.md §11.5 · Manifest: deploy/gates/f2-testgate-job.yaml

```bash
# A node's cases.json is a PUBLISH gate: the builder runs it against the built
# artifact under the frozen wamn:node world, and a failing case REFUSES the
# publish (nothing is pushed). Build the disposition-node wasm the gate drives:
cd components && cargo build --release --target wasm32-wasip2 -p disposition-node && cd ..
cargo test -p wamn-builder            # test_gate serde/subset/display units
# Conformance proof boundary: run_cases is owned by the proof library; the
# wamn-gates binary only routes the deployed command.
# recipe-test: H5-TESTGATE | conformance | wamn-proof-conformance | lib | - | testgate::tests:: | 4 | tests/conformance/src/testgate.rs compiled-node pass and typed refusal cases
cargo test -p wamn-proof-conformance --lib testgate::tests::
# Hermetic (positive arm passes; negative arm REFUSES with the typed error before any push):
./target/debug/wamn-gates --log-level warn testgate \
  --node components/target/wasm32-wasip2/release/disposition_node.wasm
cargo clippy -p wamn-builder -p wamn-gates --all-targets
# In-cluster: recreate both Jobs, require fresh Job/Pod identities and exact
# positive/negative verdicts, and prove the refusal tag's registry response is
# byte-identical before and after the expected TestGateError.
kubectl -n wamn-system port-forward svc/registry 5000:5000 \
  >/tmp/wamn-registry-port-forward.log 2>&1 &
registry_port_forward_pid=$!
trap 'kill "$registry_port_forward_pid"' EXIT
tools/kubernetes-gate-run \
  --manifest deploy/gates/f2-testgate-job.yaml \
  --verdict-record /tmp/f2-testgate-verdict-record.json \
  --timeout-secs 900 \
  --job '{"name":"f2-testgate-pass","container":"wamn-builder","expectation":"positive","exit_code":0,"image":"wamn-builder:dev","log_contains":"test gate (11.5): all case(s) passed"}' \
  --job '{"name":"f2-testgate-refusal","container":"wamn-builder","expectation":"expected-negative","exit_code":1,"image":"wamn-builder:dev","log_contains":"custom-node test gate (11.5): 1 case(s) FAILED against the built artifact"}' \
  --snapshot-executable curl \
  --snapshot-arg --silent \
  --snapshot-arg --show-error \
  --snapshot-arg --header \
  --snapshot-arg 'Accept: application/vnd.oci.image.manifest.v1+json' \
  --snapshot-arg http://127.0.0.1:5000/v2/wamn/disposition-node/manifests/testgate-refusal
jq . /tmp/f2-testgate-verdict-record.json
```

### [5.1] flow-graph schema crate (crates/execution/flow-model)

Docs: docs/archive/execution/flow-schema.md

```bash
cargo test --locked -p wamn-flow
cargo test --locked -p wamn-proof-conformance --lib flow
cargo clippy --locked -p wamn-flow --all-targets -- -D warnings
cargo fmt -p wamn-flow --check
# regenerate the published JSON Schema contract after changing the types:
cargo run -p wamn-flow --example print-flow-schema > docs/archive/contracts/flow-schema.schema.json
```

### [CALLABLE-FLOWS-P2A / wamn-5wd1.44] immutable catalog definition identity

Docs: `docs/archive/execution/FLOW-SPEC.md` §§5.1–5.4 and Phase 2A.

```bash
cargo test --locked -p wamn-catalog -p wamn-flow
# recipe-test: H5-CATALOG-IDENTITY | conformance | wamn-proof-conformance | lib | - | catalog::tests:: | 3 | tests/conformance/src/catalog.rs canonical artifact/release identity, resolved-source hash refusal, activation/head types
cargo test --locked -p wamn-proof-conformance --lib catalog::tests::
cargo clippy --locked -p wamn-catalog --all-targets -- -D warnings
cargo fmt -p wamn-catalog --check
```

### [SR-MVP / wamn-0h0g.2.10] own-flow plan wire and exact-byte reader

This gate is local and debug-only. It proves the scalar node-id wire, required
own-flow members, callable guard/hash agreement, hash-before-parse ordering,
matching-hash noncanonical JSON acceptance, semantic validation after hash
success, the transitional scenario producer, and byte-for-byte preservation of
`catalog.execution_bundles`. It does not run PostgreSQL or a cluster Job.

```bash
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-2-10 CARGO_INCREMENTAL=0 \
  cargo test --locked -p wamn-catalog -p wamn-schema-control \
  -p wamn-scenario-worker
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-2-10 CARGO_INCREMENTAL=0 \
  cargo clippy --locked -p wamn-catalog -p wamn-schema-control \
  -p wamn-scenario-worker --all-targets -- -D warnings

CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-2-10-components CARGO_INCREMENTAL=0 \
  cargo test --locked --manifest-path components/Cargo.toml -p flowrunner
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-2-10-components CARGO_INCREMENTAL=0 \
  cargo check --locked --manifest-path components/Cargo.toml \
  -p flowrunner --target wasm32-wasip2
# The fail-closed transition intentionally leaves the retired interpreter
# unreachable; retain the established dead-code allowance and deny every other warning.
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-2-10-components CARGO_INCREMENTAL=0 \
  cargo clippy --locked --manifest-path components/Cargo.toml \
  -p flowrunner --target wasm32-wasip2 -- -D warnings -A dead_code

cargo fmt -p wamn-catalog -p wamn-schema-control -p wamn-scenario-worker --check
cargo fmt --manifest-path components/Cargo.toml -p flowrunner --check
```

### [SR-MVP / wamn-0h0g.2.4] admission-owned execution-bundle pin

This gate is debug-only. It proves that release and validated-draft admission
copy the root plan hash from catalog storage, persist the four immutable run
pins without an invocation-JSON duplicate, distinguish `missing-root-plan`
before writes, and apply the empty-only run/release schema cutover atomically.
The PostgreSQL legs require one disposable PostgreSQL 18 database; they do not
target the development cluster.

```bash
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-2-4 CARGO_INCREMENTAL=0 \
  cargo test --locked -p wamn-run-state -p wamn-runtime \
  -p wamn-schema-control -p wamn-ctl -p wamn-scenario-worker \
  -p wamn-proof-integration
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-2-4 CARGO_INCREMENTAL=0 \
  cargo clippy --locked -p wamn-run-state -p wamn-runtime \
  -p wamn-schema-control -p wamn-ctl -p wamn-scenario-worker \
  -p wamn-proof-integration --all-targets -- -D warnings

CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-2-4-components CARGO_INCREMENTAL=0 \
  cargo test --locked --manifest-path components/Cargo.toml -p flowrunner
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-2-4-components CARGO_INCREMENTAL=0 \
  cargo build --locked --manifest-path components/Cargo.toml \
  -p flowrunner --target wasm32-wasip2
# The retired interpreter is intentionally unreachable during the fail-closed
# transition; preserve only the established dead-code allowance.
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-2-4-components CARGO_INCREMENTAL=0 \
  cargo clippy --locked --manifest-path components/Cargo.toml \
  -p flowrunner --target wasm32-wasip2 -- -D warnings -A dead_code

docker run --rm -d --name wamn-0h0g-2-4-pg \
  -p 127.0.0.1:15624:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
until docker exec wamn-0h0g-2-4-pg pg_isready -U postgres -d wamn; do sleep 1; done

WAMN_RUN_STORE_PG_URL=postgresql://postgres:postgres@127.0.0.1:15624/wamn \
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-2-4 CARGO_INCREMENTAL=0 \
  cargo test --locked -p wamn-run-state --test admission_live \
  admission_live -- --ignored --exact --nocapture
WAMN_CTL_PG_URL=postgresql://postgres:postgres@127.0.0.1:15624/wamn \
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-2-4 CARGO_INCREMENTAL=0 \
  cargo test --locked -p wamn-ctl --test run_plane_live \
  execution_pin_cutover_live -- --exact --nocapture
WAMN_MIGRATE_PG_URL=postgresql://postgres:postgres@127.0.0.1:15624/wamn \
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-2-4 CARGO_INCREMENTAL=0 \
  cargo test --locked -p wamn-ctl --lib \
  publish_catalog::tests::missing_root_plan_rolls_back_without_publication_writes \
  -- --exact --nocapture

WAMN_AUTHORING_LOOP_ADMIN_PG_URL=postgresql://postgres:postgres@127.0.0.1:15624/wamn \
WAMN_AUTHORING_LOOP_AUTHOR_PG_URL=postgresql://wamn_authoring_loop_author:wamn-author-live@127.0.0.1:15624/wamn \
WAMN_AUTHORING_LOOP_APP_PG_URL=postgresql://wamn_app:wamn-app-live@127.0.0.1:15624/wamn \
WAMN_AUTHORING_LOOP_FLOWRUNNER=/tmp/wamn-target-0h0g-2-4-components/wasm32-wasip2/debug/flowrunner.wasm \
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-2-4 CARGO_INCREMENTAL=0 \
  cargo test --locked -p wamn-scenario-worker --test authoring_loop_live \
  authoring_loop_live -- --ignored --exact --nocapture

docker stop wamn-0h0g-2-4-pg

# Each mutant first runs the named clean debug gate, then must fail that same
# gate after an exact-one source mutation; every target is restored byte-for-byte.
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-2-4 CARGO_INCREMENTAL=0 \
  tools/gate-mutants/admission-execution-pin.sh check
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-2-4 CARGO_INCREMENTAL=0 \
  tools/gate-mutants/admission-execution-pin.sh green-all
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-2-4 CARGO_INCREMENTAL=0 \
  tools/gate-mutants/admission-execution-pin.sh run-all

cargo fmt -p wamn-run-state -p wamn-runtime -p wamn-schema-control \
  -p wamn-ctl -p wamn-scenario-worker -p wamn-proof-integration --check
cargo fmt --manifest-path components/Cargo.toml -p flowrunner --check
git diff --check
```

### [CALLABLE-FLOWS-POC-F1 / wamn-5wd1.42] pure receipt components

Docs: `docs/archive/execution/FLOW-SPEC.md` §10.3 and `docs/archive/poc/POC-PLAN.md` F1 / Named mechanical
deltas. Both components use the zero-import `wamn:node/handler` world, declare
only the `main` output port, and carry the explicit `purity: pure` assertion
that authorizes replay. The host tests run the named decimal, float-refusal,
manifest-purity, undeclared-dependency, and interface-drift guards before the
debug Wasm artifacts are built.

```bash
CARGO_TARGET_DIR=/tmp/wamn-target-cf-f1-components-42 \
  cargo test --locked --manifest-path components/Cargo.toml \
  -p normalize-receipt -p evaluate-specs
CARGO_TARGET_DIR=/tmp/wamn-target-cf-f1-components-42 \
  cargo build --locked --manifest-path components/Cargo.toml \
  -p normalize-receipt -p evaluate-specs --target wasm32-wasip2
wasm-tools component wit \
  /tmp/wamn-target-cf-f1-components-42/wasm32-wasip2/debug/normalize_receipt.wasm
wasm-tools component wit \
  /tmp/wamn-target-cf-f1-components-42/wasm32-wasip2/debug/evaluate_specs.wasm
```

### [CALLABLE-FLOWS-P4] flow invocation contract

Docs: `docs/archive/execution/FLOW-SPEC.md` §8, §§9.1–9.7, §11, Phase 4.

```bash
cargo test --locked -p wamn-flow-invocation
cargo test --locked -p wamn-proof-conformance --lib invocation
cargo clippy --locked -p wamn-flow-invocation --all-targets -- -D warnings
```

### [CALLABLE-FLOWS-P4] exact claimed-run driver

Docs: `docs/archive/execution/FLOW-SPEC.md` §§9.1–9.7, §10, §11, Phase 4.

```bash
cargo test --locked -p wamn-runner -p wamn-runtime -p wamn-run-state
cargo test --locked -p wamn-proof-system --lib invocationproof::tests::
cargo test --locked -p wamn-proof-conformance --lib invocation
cargo check --locked --manifest-path components/execution/flowrunner/Cargo.toml

# PostgreSQL live race/fault proof (throwaway PostgreSQL 18):
WAMN_RUN_QUEUE_PG_URL=postgresql://postgres:postgres@127.0.0.1:55472/postgres \
  cargo test --locked -p wamn-run-state --test claimed_inline_live -- --ignored --nocapture

# Gate of record: build the canonical two-stage exact image, load it into kind,
# then drive the baked /bench/flowrunner.wasm through the production host seam.
docker build --target gates -t wamn-gates:dev .
kind load docker-image wamn-gates:dev --name wamn
kubectl -n wamn-system delete job invocationproof --ignore-not-found
kubectl -n wamn-system apply -f deploy/gates/invocationproof-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/invocationproof --timeout=300s
kubectl -n wamn-system logs job/invocationproof
```

### [CALLABLE-FLOWS-P4] production invocation provider

Docs: `docs/archive/execution/FLOW-SPEC.md` §§6.1–6.2, §§9.4–9.7, §§10–11, Phase 4.

```bash
cargo test --locked -p wamn-runtime -p wamn-run-state -p wamn-flow-invocation
cargo test --locked -p wamn-proof-system --lib invocationproof::tests::
cargo test --locked -p wamn-proof-integration --lib invocationproof::tests::
cargo test --locked -p wamn-proof-conformance docker_component_provenance
cargo check --locked -p wamn-host

# Gate of record: the exact image composes admission, inline fenced execution,
# bounded wait, stored-outcome recovery, conflicts, and disabled recovery.
docker build --target gates -t wamn-gates:dev .
kind load docker-image wamn-gates:dev --name wamn
kubectl -n wamn-system delete job invocationproof --ignore-not-found
kubectl -n wamn-system apply -f deploy/gates/invocationproof-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/invocationproof --timeout=300s
kubectl -n wamn-system logs job/invocationproof
```

### [5.2] production flow-runner engine (crates/execution/flow-engine)

Docs: docs/archive/execution/flow-runner.md

```bash
cargo test -p wamn-runner
cargo clippy -p wamn-runner --all-targets && cargo fmt -p wamn-runner --check
# locally. Rebuild the guest (part of the guest build above), then re-run those gates:
(cd components && cargo build --release --target wasm32-wasip2 -p flowrunner)
cargo clippy --manifest-path components/execution/flowrunner/Cargo.toml --release --target wasm32-wasip2 \
  && cargo fmt --manifest-path components/execution/flowrunner/Cargo.toml --check
```

### [5.3] standard node library v1 (crates/node/sdk + crates/execution/standard-nodes)

Docs: docs/archive/execution/node-library.md

```bash
cargo test -p wamn-standard-nodes             # nodes + policy negatives + purity lint
cargo test -p wamn-node-sdk
cargo test -p wamn-runner            # taxonomy re-export + port drift-guard regression
cargo clippy -p wamn-node-sdk -p wamn-standard-nodes --all-targets \
  && cargo fmt -p wamn-node-sdk -p wamn-standard-nodes --check
```

### [5.4] wamn:node contract 0.1 FROZEN + SDK scaffolding

```bash
cargo test -p wamn-node-sdk      # incl the wit_coherence drift-guards
cargo test -p wamn-node-guest    # conversion glue + NoCapsCtx units
cargo test -p wamn-node-manifest # fixture/negatives/conformance/drift
cargo clippy -p wamn-node-guest -p wamn-node-manifest --all-targets \
  && cargo fmt -p wamn-node-sdk -p wamn-node-guest -p wamn-node-manifest --check
# regenerate the published manifest schema after changing the types:
cargo run -p wamn-node-manifest --example print-node-manifest-schema > docs/archive/contracts/wamn-node-manifest.schema.json
```

### [5.7] run-state persistence (crates/execution/run-state)

Docs: docs/archive/execution/run-state.md

```bash
cargo test -p wamn-run-state
cargo test -p wamn-runner   # the resume/seed_at primitives (regression)
cargo clippy -p wamn-run-state --all-targets && cargo fmt -p wamn-run-state --check
# optional live-apply gate (deploy/sql/run-state.sql on a throwaway PG; superuser URL
# node_runs FK cascade; skips cleanly when unset):
docker run -d --rm --name wamn-runstore-pg -p 5458:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_RUN_STORE_PG_URL=postgres://postgres:postgres@127.0.0.1:5458/wamn cargo test -p wamn-run-state
docker stop wamn-runstore-pg
# (in-cluster gate of record + locally). Rebuild the guest, re-run those gates (the
# additively (kubectl exec psql — shared-cluster guardrail, never recreate the pod).
(cd components && cargo build --release --target wasm32-wasip2 -p flowrunner)
cargo clippy --manifest-path components/execution/flowrunner/Cargo.toml --release --target wasm32-wasip2 \
  && cargo fmt --manifest-path components/execution/flowrunner/Cargo.toml --check
```

Callable-flow admission uses the same throwaway database and explicitly runs
the ignored admission transaction gate. Its event-lineage phase proves organic
and chained source/root/depth persistence, tenant-scoped parent validation,
forgery and depth refusals, immutable retry identity, and rollback:

```bash
cargo test --locked -p wamn-run-state
docker run -d --rm --name wamn-admission-pg -p 5458:5432 \
  -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=wamn postgres:18
# recipe-test: CALLABLE-EVENT-LINEAGE | integration | wamn-run-state | test | admission_live | - | 1 | trusted event source/root/depth, cross-tenant and forged-lineage refusal, immutable retry, queue-fault rollback
WAMN_RUN_STORE_PG_URL=postgres://postgres:postgres@127.0.0.1:5458/wamn \
  cargo test --locked -p wamn-run-state --test admission_live -- --ignored
docker stop wamn-admission-pg
```

Durable cancellation and the bounded dispatcher deadline sweep use the same
throwaway database:

```bash
cargo test --locked -p wamn-run-state -p wamn-runner -p wamn-dispatcher
docker run -d --rm --name wamn-cancellation-pg -p 5458:5432 \
  -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=wamn postgres:18
# recipe-test: CALLABLE-CANCEL | integration | wamn-run-state | test | cancellation_live | - | 1 | generation-fenced request, live-attempt deferral, completion-time cancellation, propagation, bounded deadline sweep
WAMN_RUN_STORE_PG_URL=postgres://postgres:postgres@127.0.0.1:5458/wamn \
  cargo test --locked -p wamn-run-state --test cancellation_live -- --ignored
docker stop wamn-cancellation-pg
```

Occurrence-keyed `invoke-flow` child creation and wake-at-release use the same
throwaway database:

```bash
cargo test --locked -p wamn-run-state -p wamn-flow
docker run -d --rm --name wamn-child-state-pg -p 5458:5432 \
  -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=wamn postgres:18
# recipe-test: CALLABLE-CHILD-STATE | integration | wamn-run-state | test | child_live | - | 1 | exact occurrence recovery, conflicting/cross-parent refusal, wait-generation fence, create/release fault rollback, atomic wake
WAMN_RUN_STORE_PG_URL=postgres://postgres:postgres@127.0.0.1:5458/wamn \
  cargo test --locked -p wamn-run-state --test child_live -- --ignored --nocapture
docker stop wamn-child-state-pg
```

Callable-flow child runtime authorization, bounds, service lineage, outcome
resume, and pre-release generation seizure extend the same gate:

```bash
cargo test --locked -p wamn-runner -p wamn-run-state
cargo test --locked -p wamn-proof-system --lib childproof::tests::
docker run -d --rm --name wamn-child-runtime-pg -p 5458:5432 \
  -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=wamn postgres:18
# recipe-test: CALLABLE-CHILD-RUNTIME | integration | wamn-run-state | test | child_live | - | 1 | creation-only authorization, service actor lineage, fresh context, depth and deadline bounds, released outcome recovery, stale-generation and pre-release cancellation
WAMN_RUN_STORE_PG_URL=postgres://postgres:postgres@127.0.0.1:5458/wamn \
  cargo test --locked -p wamn-run-state --test child_live -- --ignored --nocapture
docker stop wamn-child-runtime-pg
```

### [5.7-resume-pin / wamn-cox] resume pins the run's persisted flow_version

Docs: docs/archive/execution/run-state.md § *Resume pins the run's persisted version*

```bash
# A resume loads the run's PERSISTED runs.flow_version (stamped at write-ahead
# time), never the active version — so a flow edited/hot-reloaded mid-run cannot
# make reconstruction fold against a divergent graph. All three drive paths pin
# it: the direct execute (reads flow_version, load_flow_at), the unpartitioned
# claim (claim_dispatch_sql projects r.flow_version, the guest flow_at pins it),
# and the partitioned claim (select_run_dispatch_sql projects flow_version).
cargo test -p wamn-run-state   # pure text pins + queue.rs live
#   discriminating fixture (cd-0 PERSISTED=3 vs ACTIVE=4 -> claim returns 3)
# Gate of record: runnerbench MERGE-RESUME phase (phase 9). mr-0 (v1) parks at its
# delay-merge; a structurally-different v2 (linear in->r) is registered+activated
# MID-RUN; the pinned resume keeps driving v1 (completed, 7 node_runs rows, m/r
# visits (2,0,1)). See [5.14] production runner (run-worker, fqg.8) for the run cmd.
# The checked-in PLAN-0.2 campaign replaces the historical scratchpad mutants.
# `check` proves every exact mutation anchor and baseline hash is current;
# `green-all` proves every named gate on the clean source; `run-all` applies
# each fixed mutant, requires the named debug gate to turn red, and restores the
# target byte-for-byte under a trap.
CARGO_TARGET_DIR=/tmp/wamn-target-2jdm-5-1 \
  tools/gate-mutants/durable-invocation-recovery.sh check
CARGO_TARGET_DIR=/tmp/wamn-target-2jdm-5-1 \
  tools/gate-mutants/durable-invocation-recovery.sh green-all
CARGO_TARGET_DIR=/tmp/wamn-target-2jdm-5-1 \
  tools/gate-mutants/durable-invocation-recovery.sh run-all
cargo test --locked -p wamn-proof-conformance --test gate_mutation_evidence
# Immutable green/red evidence:
# architecture/evidence/mutations/durable-invocation-recovery.json
```

### [5.9] credential vault (plugins/wamn_credentials + credproof)

Docs: docs/archive/data-path/credential-vault.md

```bash
# Pure units: the SDK facade + http-request injection/classification + the
# guest per-dispatch scoping + the host vault resolution + the WIT coherence
# drift-guards (the credentials copies) + the credproof fixture pins.
cargo test -p wamn-node-sdk && cargo test -p wamn-standard-nodes
cargo test -p wamn-node-guest --all-features
# Unit boundary: the credential plugin moved to wamn-runtime. The deployed
# fixture contract is owned by the system-proof library.
# recipe-test: H5-CREDENTIALS | unit | wamn-runtime | lib | - | plugins::wamn_credentials::tests:: | 8 | crates/platform/runtime/src/plugins/wamn_credentials.rs vault and per-execution grant semantics
cargo test -p wamn-runtime --lib plugins::wamn_credentials::tests::
# recipe-test: H5-CREDPROOF | system | wamn-proof-system | lib | - | credproof::tests:: | 4 | tests/system/src/credproof.rs credential and deny deployment fixtures
cargo test -p wamn-proof-system --lib credproof::tests::
CARGO_TARGET_DIR=/tmp/wamn-target-2jdm-23 \
  tools/gate-mutants/credential-proof-fixtures.sh green-all
CARGO_TARGET_DIR=/tmp/wamn-target-2jdm-23 \
  tools/gate-mutants/credential-proof-fixtures.sh run-all

# cjv.3 host-enforced per-execution grant + fail-closed project. credprobe
# drives the direct-import THREAT fixture (components/fixtures/cred-probe,
# imports wamn:node/credentials directly like a custom node) in-proc against a
# vault with a NARROW host-registered grant — proves an ungranted /
# unregistered-project get() is not-granted over the real WIT boundary (no DB):
(cd components && cargo build --release --target wasm32-wasip2 -p cred-probe)
./target/debug/wamn-gates credprobe \
  --cred-probe components/target/wasm32-wasip2/release/cred_probe.wasm
# Mutation (apply/test/restore, sha256, DEBUG): scratchpad mutate_cjv3.py
#   M1 grant check skipped        -> credprobe (sibling/absent not-granted)
#   M2 project_for fail-open      -> credprobe (no-project not-granted)
#   M3 set_granted no-op          -> credprobe (DELIVERY: granted resolves)

# Local end-to-end (throwaway PG + local serve-echo; credproof creates a fresh
# database, admits both fixtures through the production transaction, and drives
# the exact flowrunner through ExecutionHost):
docker run -d --name wamn-cred-pg -p 5493:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
./target/debug/wamn-gates --log-level error serve-echo --port 8093 &
./target/debug/wamn-gates credproof \
  --flowrunner components/target/wasm32-wasip2/release/flowrunner.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5493/wamn \
  --admin-database-url postgres://postgres:postgres@127.0.0.1:5493/wamn \
  --echo-url http://127.0.0.1:8093
# In-cluster gate of record (kind 'wamn'; exact gates image carries both the
# production ExecutionHost and the baked flowrunner):
docker build --target host -t wamn-host:dev . && docker build --target gates -t wamn-gates:dev .
kind load docker-image wamn-host:dev --name wamn && kind load docker-image wamn-gates:dev --name wamn
kubectl -n wamn-system apply -f deploy/gates/serve-echo.yaml
kubectl -n wamn-system apply -f deploy/gates/credproof-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/credproof --timeout=300s
kubectl -n wamn-system logs job/credproof   # overall PASS: true
```

### [5.14] durable run queue & runner scaling (crates/execution/run-state)

Docs: docs/archive/execution/run-queue.md

```bash
cargo test -p wamn-run-state -p wamn-scheduler
cargo clippy -p wamn-run-state -p wamn-scheduler --all-targets \
  && cargo fmt -p wamn-run-state -p wamn-scheduler --check
# optional live-apply gate (deploy/sql/run-state.sql + run-queue.sql on a throwaway PG;
# skips cleanly when unset):
docker run -d --rm --name wamn-rq-pg -p 5459:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
docker exec wamn-rq-pg psql -U postgres -c \
  "CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS;"
WAMN_RUN_QUEUE_PG_URL=postgres://postgres:postgres@127.0.0.1:5459/wamn cargo test -p wamn-run-state
# throwaway PG above (the live-apply gate created wamn_app) + a throwaway NATS:
docker run -d --rm --name wamn-rq-nats -p 4232:4222 nats:2.12.8-alpine
WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:5459/wamn \
  ./target/release/wamn-gates --log-level error queuebench \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5459/wamn \
  --nats-url nats://127.0.0.1:4232 --mode all
docker stop wamn-rq-pg wamn-rq-nats
# In-cluster gate of record (co-located with postgres, NO cpu limit — S2 CFS lesson;
# kind load docker-image wamn-gates:dev --name wamn):
kubectl -n wamn-system apply -f deploy/gates/queuebench-job.yaml
kubectl -n wamn-system logs -f job/queuebench

# Checked-in PLAN-0.2 queue/runner mutation campaign. `check` pins the clean
# source hashes; `green-all` runs every named debug gate; `run-all` requires
# every fixed mutant to turn its gate red and restores each target byte-exactly.
CARGO_TARGET_DIR=/tmp/wamn-target-2jdm-5-2 \
  tools/gate-mutants/queue-runner.sh check
CARGO_TARGET_DIR=/tmp/wamn-target-2jdm-5-2 \
  tools/gate-mutants/queue-runner.sh green-all
CARGO_TARGET_DIR=/tmp/wamn-target-2jdm-5-2 \
  tools/gate-mutants/queue-runner.sh run-all
cargo test --locked -p wamn-proof-conformance --test gate_mutation_evidence
# Immutable green/red evidence:
# architecture/evidence/mutations/queue-runner.json
```

Trusted event lineage in runner execution input has its own focused campaign.
It proves the combined claim selector, split dispatch selector, and flowrunner
context declaration while restoring each mutated source byte-exactly:

```bash
CARGO_TARGET_DIR=/tmp/wamn-target-2jdm-11 \
  tools/gate-mutants/event-lineage-dispatch.sh check
CARGO_TARGET_DIR=/tmp/wamn-target-2jdm-11 \
  tools/gate-mutants/event-lineage-dispatch.sh green-all
CARGO_TARGET_DIR=/tmp/wamn-target-2jdm-11 \
  tools/gate-mutants/event-lineage-dispatch.sh run-all
cargo test --locked -p wamn-proof-conformance --test gate_mutation_evidence
# Immutable green/red evidence:
# architecture/evidence/mutations/event-lineage-dispatch.json
```

D20 (R6, wamn-1d4) the `partitioned(key)` head-unavailability policy lands here:
`wamn-flow` gains `Flow::partition_policy` (`blocking` default / `leapfrog`),
`run_queue.partition_policy` materializes it, `claim_partition_head_sql` branches on
it, and `janitor_sweep_sql` exempts a blocking-policy row (wedge). Pure coverage:
`partition_policy_decides_whether_a_later_run_overtakes_an_unavailable_head`,
`blocking_wedges_a_key_behind_an_exhausted_head_leapfrog_releases_it`,
`blocking_partition_orphan_wedges_instead_of_being_reaped` (janitor verdict), plus
shape/DDL drift guards. The live-apply gate (Phase A/B) and the queuebench
`partition` phase (`partition_policy_cases`) prove it through real Postgres. The
guest does not read the flow field until fqg.9, so the in-cluster gate is a
gates-image rebuild only (guest unchanged for this slice).

### [PLAN-3 / wamn-vshi.5] F1 capture-on run-state baseline

Docs: `docs/archive/PLAN/PLAN.md` items 1 and 3; published record in
`docs/archive/results/ceilings.md` § PLAN-3-F1.

```bash
# Deterministic F1-path and argument guards.
cargo test -p wamn-proof-integration runstate_baseline --no-fail-fast

# Short live iteration against a throwaway PostgreSQL 18 database. The command
# applies the production run-state DDL in an ephemeral schema and drops it.
WAMN_PG_URL=postgres://wamn_app:wamn_app@127.0.0.1:5457/postgres \
  WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:5457/postgres \
  ./target/debug/wamn-gates --log-level error runstate-baseline \
  --line-counts 1,10 --runs-per-size 5 --concurrency 2

# Record run: build/load the gates image from the source revision being cited,
# recreate the Job, and capture its CSV block.
docker build --target gates -t wamn-gates:dev .
kind load docker-image wamn-gates:dev --name wamn
kubectl -n wamn-system delete job runstate-baseline --ignore-not-found
kubectl -n wamn-system apply -f deploy/gates/runstate-baseline-job.yaml
kubectl -n wamn-system logs -f job/runstate-baseline
# Extract `=== BEGIN/END CSV plan3-f1-capture-baseline ===` into
# docs/archive/results/ceilings-data/plan3-f1-capture-baseline.csv.
```

The campaign measures the current capture-replay architecture before item 1
rewires recovery. It uses the canonical successful F1 node path with 1, 10,
and 100 schema-valid receipt lines. Each node boundary commits a full input and
output capture; context-bearing nodes also update `runs.state_json`, matching
the present source of heap/TOAST growth, WAL, and vacuum pressure. Measurements
are curves, not budgets. Only durable-commit provenance, exact run/node counts,
full-capture presence, and nonzero WAL are pass/fail assertions.

### [EVT-C7 / wamn-z7b.1] queuebench ceiling campaign (measurement, not a gate)

Docs: docs/archive/results/ceilings.md (the published curves) + docs/archive/events/event-plane-jetstream.md §10/§11

```bash
# The pure ramp/knee controller (coarse-double → bisect; p99-doubling /
# rate-divergence / drain-timeout saturation) lives in wamn-gate-harness:
cargo test -p wamn-gate-harness
# Local iteration (short knobs; correctness only — debug build, dev-host PG):
docker run -d --rm --name wamn-ceil-pg -p 5443:5432 -e POSTGRES_PASSWORD=postgres postgres:18
docker exec wamn-ceil-pg psql -U postgres -c \
  "CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS;"
WAMN_PG_URL=postgres://wamn_app:wamn_app@127.0.0.1:5443/postgres \
  WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:5443/postgres \
  ./target/debug/wamn-gates --log-level error queuebench --mode ceiling \
  --level-secs 5 --soak-secs 30 --burst-secs 10
docker stop wamn-ceil-pg
# Numbers of record (in-cluster, §10 knobs baked into the manifest; ~60–90 min):
kubectl -n wamn-system apply -f deploy/gates/queuebench-ceiling-job.yaml
kubectl -n wamn-system logs -f job/queuebench-ceiling
# Extract the `=== BEGIN CSV <name> ===` blocks from the job log into
# docs/results/ceilings-data/ and cite them from docs/archive/results/ceilings.md (§11 provenance).
```

The ceiling mode is deliberately NOT in `--mode all` (the regression gate of
record stays deploy/gates/queuebench-job.yaml). Only the exactly-once + completeness
sanity asserts are pass/fail; the knees/curves are measurements. Phase 2
(fillfactor × autovacuum matrix, 30-min soak, 1M-run bloat soak) = wamn-z7b.6.
Mutation harness for the knee controller: scratchpad `mutate_z7b1.py`
(saturation-arm + bisect-direction mutants each fail a named
wamn-gate-harness unit test).

### [EVT-C2 / wamn-z7b.2] outboxbench — RETIRED (l5i9.19 teardown)

The C2 campaign of record stands in docs/archive/results/ceilings.md + docs/results/ceilings-data/
(c2-*.csv). The bench, the outbox triggers it measured
(`Migration::outbox_triggers`), and deploy/gates/outboxbench-job.yaml were
deleted with the outbox path (D19 v3 §3, executed 2026-07-20) — the numbers
are history of a retired mechanism and cannot be re-measured.

### [EVT-C-WAL-0 / wamn-l5i9.4] walbench pre-CDC WAL baseline (measurement, not a gate)

Docs: docs/archive/results/ceilings.md § C-WAL-0 (the published numbers) + docs/archive/events/event-plane-jetstream.md
§7/§8/§10. The *denominator* every later C-CDC WAL-delta claim (wamn-l5i9.14) divides
by: representative-app WAL volume BEFORE any publication/slot exists (bd dep
wamn-l5i9.9 → wamn-l5i9.4 keeps it strictly pre-CDC).

```bash
# Integration-proof unit boundary: walbench implementation and fixtures live
# in wamn-proof-integration, not the wamn-gates router.
# recipe-test: H5-WALBENCH | integration | wamn-proof-integration | lib | - | walbench::tests:: | 3 | tests/integration/src/walbench.rs rate parser, blob entropy, and catalog floor
cargo test -p wamn-proof-integration --lib walbench::tests::
# Local iteration (short knobs; correctness only — debug build, dev-host PG):
docker run -d --rm --name wamn-cwal0-pg -p 5444:5432 -e POSTGRES_PASSWORD=postgres postgres:18
docker exec wamn-cwal0-pg psql -U postgres -c \
  "CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS;"
WAMN_PG_URL=postgres://wamn_app:wamn_app@127.0.0.1:5444/postgres \
  WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:5444/postgres \
  ./target/debug/wamn-gates --log-level error walbench --mode all \
  --iters 100 --mixed-rates 20,50 --mixed-secs 8
docker stop wamn-cwal0-pg
# Numbers of record (in-cluster on the fixture pod, record knobs baked into the
# manifest; ~few min; a SINGLE run is the record — byte counts + medians, no knee
# to poison). Needs a gates-only image (docker build --target gates); no wamn-host
# change so the host stage is cached apart from the crates/ recompile:
docker build --target gates -t wamn-gates:dev . && kind load docker-image wamn-gates:dev --name wamn
kubectl -n wamn-system apply -f deploy/gates/walbench-job.yaml
kubectl -n wamn-system logs -f job/walbench
# Extract the `=== BEGIN CSV <name> ===` blocks (cwal0-perop / cwal0-mixed) into
# docs/results/ceilings-data/ and cite them from docs/archive/results/ceilings.md (§ C-WAL-0 provenance).
```

The pre-CDC claim is made checkable, not assumed: `precheck` asserts the measured DB
has no publication and no replication slot and every table carries the DEFAULT replica
identity (`d`) before any measurement runs. `pg_current_wal_insert_lsn` (WAL generated),
not the flushed position — exact even under the fixture pod's `fsync=off`/
`synchronous_commit=off`. Only the sanity asserts gate: pre-CDC, per-op WAL > 24 B (the
instrument self-check), exact op counts, and the wide leg genuinely TOASTed. Mutation
harness: scratchpad `mutate_cwal0.py` (M1 instrument swap `pg_current_wal_insert_lsn` →
`pg_current_wal_lsn` fails every `> 24 B/op` assert on an `fsync=off` PG — the fixture-pod
kill; M2 op-batch runs `n/2` fails the exact-op-count assert).

### [EVT-S-CDC-1 / wamn-l5i9.2] pg_walstream diligence spike (diligence, not a gate)

Docs: docs/archive/events/event-plane-jetstream.md §7; verdicts live in the wamn-l5i9.2 bead
notes and feed wamn-l5i9.6 [BUILD-VS-BUY]. The harness is `poc/cdc1`
(pg_walstream from the wamn fork, rev-pinned in the root workspace table since
wamn-l5i9.8 — ledger: docs/archive/events/pg-walstream-fork.md).

```bash
cargo build -p wamn-cdc1 && cargo clippy -p wamn-cdc1 && cargo fmt -p wamn-cdc1 --check
# Throwaway 2-instance CNPG cluster (torn down after the spike; NEVER reuse
# wamn-pg or wamn-sysdb — switchover needs a standby):
kubectl apply -f poc/cdc1/cdc1-cluster.yaml   # cluster cdc1 + NodePort 172.28.0.4:30497
export CDC1_URL="postgresql://postgres:$(kubectl -n wamn-system get secret \
  cdc1-superuser -o jsonpath='{.data.password}' | base64 -d)@172.28.0.4:30497/app"
./target/debug/wamn-cdc1 setup        # tables + publication + failover slot (through the crate)
./target/debug/wamn-cdc1 message      # (e) pg_logical_emit_message → EventType::Message
./target/debug/wamn-cdc1 toast        # (c) unchanged-TOAST absent-vs-Null + FULL old image
./target/debug/wamn-cdc1 stream --rows 1000000   # (d) streamed txn, VmRSS profile
./target/debug/wamn-cdc1 soak --secs 1800        # (a) idle keepalive/feedback + canary
./target/debug/wamn-cdc1 switchover --secs 90    # (b) then delete the primary pod mid-run
./target/debug/wamn-cdc1 teardown && kubectl delete -f poc/cdc1/cdc1-cluster.yaml
```

FINDING F1: crates.io pg_walstream 0.8.0's `slot_options.failover = true`
emits legacy space-separated `CREATE_REPLICATION_SLOT … FAILOVER`, which PG17+
rejects (FAILOVER exists only in the parenthesized option grammar). FIXED in
the wamn fork (wamn-l5i9.8): the harness now sets `failover = true` and creates
the slot through the crate.

### [EVT-VENDOR / wamn-l5i9.8] pg_walstream fork + pin

Docs: docs/archive/events/pg-walstream-fork.md (carried-commit ledger + sync runbook). The
fork branch `wamn/0.8.0` = upstream v0.8.0 + the F1 failover-syntax commit;
the rev is pinned once in the root `Cargo.toml` workspace table.

```bash
# Fork unit tests (in a clone of dkkloimwieder/pg-walstream, branch wamn/0.8.0):
cargo test --lib          # 1247 tests incl the parenthesized-FAILOVER pins
# Consumer + lock sanity (in wamn):
cargo build -p wamn-cdc1
grep -c '^name = "pg_walstream"$' Cargo.lock   # must be 1 (git-sourced)
# Live A/B (throwaway postgres:18 -c wal_level=logical, e.g. :5444):
#   A: pin poc/cdc1 back to crates.io `=0.8.0` → `wamn-cdc1 setup` fails 42601
#   B: the fork pin → setup prints `slot cdc1_spike created: … failover=true`,
#      then `wamn-cdc1 message` passes as the streaming regression.
```

### [EVT-NATS / wamn-l5i9.7] streambench data-plane JetStream gate

Docs: docs/archive/events/event-plane-jetstream.md §5/§7 Phase 1. Stands up the DEDICATED
data-plane NATS (deploy/infra/nats-jetstream.yaml — a 3-node JetStream cluster, R3
file storage, Service `evt-nats`), SEPARATE from the operator/control-plane NATS
(Service `nats`, doorbells) which stays untouched. The gate (`streambench`, a
pure NATS client — no wasm, no Postgres) proves the four load-bearing claims:
publish → the `EVT_<org>_<env>` stream (subjects
`evt.<org>.<project>.<env>.<entity>.<op>`), `Nats-Msg-Id = <project_env>:<lsn>`
dedupe, consume in commit order, and R3 survives node loss. Accounts: single
shared (default) account — per-org accounts + replication creds are the
wamn-4xw seam (§11). `--mode all` / `--mode publish` also run the **E14 standing
guard** (docs/archive/findings.md §3): over a batch shaped like the rows of ONE large
multi-row txn (dense per-event LSNs, one commit xid), published-event count ==
distinct `Nats-Msg-Id` count — the server-side stream-delta is the honest
detector, since any msg-id collision is a silent JetStream dedupe.

```bash
cargo build -p wamn-gates   # streambench compiles into the suite
# Local iteration — a throwaway 3-node cluster is R3 (single node = R1):
docker network create evt-nats-local
R=nats://evt-nats-local-0:6222,nats://evt-nats-local-1:6222,nats://evt-nats-local-2:6222
for i in 0 1 2; do docker run -d --name evt-nats-local-$i --network evt-nats-local \
  -p $((4232+i)):4222 nats:2.10-alpine -js -sd /data --name n$i \
  --cluster nats://0.0.0.0:6222 --cluster_name evt-local --routes "$R"; done
./target/debug/wamn-gates --log-level error streambench --mode all \
  --nats-url nats://localhost:4232 --replicas 3 --messages 200
# Physical node-loss heal (degraded 2/3): publish → destroy a node → heal
./target/debug/wamn-gates --log-level error streambench --mode publish \
  --nats-url nats://localhost:4232 --replicas 3 -n 200
docker rm -f evt-nats-local-2
./target/debug/wamn-gates --log-level error streambench --mode heal \
  --nats-url nats://localhost:4232 --replicas 3 --expect-messages 200
docker rm -f evt-nats-local-0 evt-nats-local-1 evt-nats-local-2; docker network rm evt-nats-local

# Gate of record (in-cluster). Gates-only image (no wamn-host change → host stage
# cached apart from the crates/ recompile):
docker build --target gates -t wamn-gates:dev . && kind load docker-image wamn-gates:dev --name wamn
kubectl -n wamn-system apply -f deploy/infra/nats-jetstream.yaml
kubectl -n wamn-system rollout status statefulset/evt-nats --timeout=180s
kubectl -n wamn-system apply -f deploy/gates/streambench-job.yaml    # --mode all: publish/consume/dedupe/stepdown
kubectl -n wamn-system wait --for=condition=complete job/streambench --timeout=180s
kubectl -n wamn-system logs job/streambench
# Physical R3 heal (the runbook is in deploy/gates/streambench-job.yaml's header):
#   streambench-pub pod → kubectl delete pod evt-nats-2 → streambench-heal pod
```

`--mode all` proves R3 durability without k8s (a RAFT leader step-down +
re-election, all messages survive); the two-step `publish` → `kubectl delete pod`
→ `heal` runbook proves survival of a physical node deletion. The heal drain
uses an R1 in-memory consumer (transient bookkeeping — the durability guarantee
is on the R3 stream), so it succeeds while a node is still down. Mutation
harness: scratchpad `mutate_l5i9_7.py` — M1 drops the Nats-Msg-Id on re-publish
(dedupe assert fails), M2 creates the stream R1 not R3 (`stream is R3` fails),
M3 makes the LSN non-monotonic-but-unique via `i^1` (commit-order assert fails),
M4 drops the id on the focused second publish (`second publish IS a duplicate`
fails). The data-plane NATS is left STANDING as the Phase-1 substrate (the
reader wamn-l5i9.10 + C-JS wamn-l5i9.15 consume it); reclaim with
`kubectl -n wamn-system delete -f deploy/infra/nats-jetstream.yaml`.

### [EVT-PROVISION / wamn-l5i9.9] enable-cdc-project-env — publication + failover slot + reader registration

Docs: docs/archive/events/event-plane-jetstream.md §4, docs/archive/platform/provisioning.md
§enable-cdc-project-env. The CDC capture overlay on a provisioned project-env:
one shared `wamn_cdc_<org>__<project>__<env>` name for the publication
(`FOR TABLES IN SCHEMA <data schema>` — auto-includes tables catalog-publish
creates later), the failover-enabled slot (SQL-function form,
`pg_create_logical_replication_slot(…, failover => true)`; WAL pinned from
enable), and the REPLICATION role (R8b tier; own Secret
`wamn-cdc-<org>--<project>--<env>`), plus the `registry.event_readers`
registration (FK → `project_envs`, so an unprovisioned env is refused).

```bash
cargo test -p wamn-control-provision            # name/builder/secret units incl the CDC set
cargo test -p wamn-control-registry             # event-reader builder shapes + EventReader round-trip
cargo test -p wamn-ctl enable_cdc      # bundle ordering + name validation
cargo clippy -p wamn-control-provision -p wamn-control-registry -p wamn-ctl
# Live-apply gates (throwaway PG18 with logical decoding ON):
docker run -d --name wamn-cdc-pg -e POSTGRES_PASSWORD=postgres -p 5447:5432 \
  postgres:18 -c wal_level=logical
WAMN_CDC_PG_URL=postgres://postgres:postgres@127.0.0.1:5447/postgres \
  cargo test -p wamn-control-provision --test cdc          # publication/slot/role/grants live
WAMN_REGISTRY_PG_URL=postgres://postgres:postgres@127.0.0.1:5447/postgres \
  cargo test -p wamn-control-provision --test control_storage # event readers + provisioning state
docker rm -f wamn-cdc-pg
# In-cluster gate of record (no docker rebuild — the real debug subcommand +
# kubectl; scratchpad incluster_l5i9_9.sh is the scripted run): register a
# trials org + project-env on wamn-pg (q3n.7 runbook), then:
./target/debug/wamn-ctl enable-cdc-project-env --org <o> --project <p> --env <e> \
  --schema app --system-database-url "$WAMN_SYSTEM_ADMIN_URL" \
  --emit-role-sql role.sql --emit-cdc-sql cdc.sql --emit-secret secret.json
#   apply order: role.sql → the TARGET cluster (any DB; roles are cluster-global),
#   cdc.sql → the PROJECT-ENV database (publication + slot are database-bound),
#   kubectl apply secret.json. Assert pg_publication (+ auto-include after a
#   CREATE TABLE in the schema), pg_replication_slots.failover=true,
#   pg_roles.rolreplication, and the registry.event_readers read-back; teardown
#   drops the slot FIRST (releases pinned WAL — wamn-pg has no
#   max_slot_wal_keep_size bound), then CR/db/role + the org row (cascade).
# kubectl port-forward dies per-connection on this kind cluster — use the
# temporary NodePort-on-the-primary recipe (8df.5) for the host↔wamn-sysdb TCP.
```

Mutation harness: scratchpad `mutate_l5i9_9.py` — M1 slot `failover` true→false,
M2 role loses `REPLICATION`, M3 publication `FOR ALL TABLES`, M4 event-reader
upsert never refreshes; each killed by a named unit AND the live gate (the gate
drops the role in its preamble so a leftover healthy role can't mask a mutated
builder). Cluster-level preconditions (`wal_level=logical` is the CNPG default;
`synchronizeLogicalDecoding` / `max_slot_wal_keep_size` are provision-org
env-policy knobs) are a SIBLING bead, not this overlay.

### [EVT-READER / wamn-l5i9.10] event-reader — one project-env → the EVT_ stream

Docs: docs/archive/events/event-plane-jetstream.md §4. The CDC reader MVP: `wamn-cdc-reader --org --project --env` (replicas=1 Deployment,
deploy/platform/event-reader.example.yaml) reads its `registry.event_readers`
registration, opens ONE pg_walstream session (`StreamingMode::Off` — whole
txns, commit order), and publishes `wamn-event-wire` envelopes onto
`evt.<org>.<project>.<env>.<entity>.<op>` with
`Nats-Msg-Id = <project>_<env>:<lsn>`. Confirmed LSN advances ONLY on
JetStream ack, at txn granularity; JetStream down ⇒ the publish retries
forever ⇒ WAL retained (delayed, never lost). The reader NEVER creates the
slot — a missing/invalidated slot is the v3 §11 capture-gap incident and the
crash-loop is the MVP alert. `WAMN_CDC_URL` is the plain Secret url; the
reader appends `sslmode` + `replication=database` itself.

```bash
cargo test -p wamn-event-wire           # the draft wire contract, string-pinned
cargo test -p wamn-cdc-reader --lib   # url compose / error classify / row map
cargo clippy -p wamn-event-wire -p wamn-cdc-reader -p wamn-gates
# Local live gate (throwaway PG18 logical + single-node JetStream; ~90s —
# idle-stream feedback rides the ~30s server-keepalive cycle, hence the waits):
docker run -d --name wamn-reader-pg -e POSTGRES_PASSWORD=postgres -p 5448:5432 \
  postgres:18 -c wal_level=logical -c fsync=off
docker run -d --name wamn-reader-nats -p 4261:4222 nats:2.10-alpine -js -sd /data
WAMN_READER_PG_URL=postgres://postgres:postgres@127.0.0.1:5448/postgres \
WAMN_READER_NATS_URL=nats://127.0.0.1:4261 \
  cargo test -p wamn-cdc-reader --test event_reader_live
# drills: disabled-registration + missing-slot refusals, commit order +
# envelope shape (TOAST-absent vs NULL) + dedupe, LSN-advance-on-ack, crash →
# restart resume, severed-proxy JetStream-down holds the LSN, clean shutdown,
# zero-residue teardown (no slot left behind).
docker rm -f wamn-reader-pg wamn-reader-nats
# In-cluster gate of record (no image rebuild — the real debug binary against
# NodePorts on wamn-pg/wamn-sysdb/evt-nats; scripted: scratchpad
# incluster_l5i9_10.sh): provision + enable-cdc a trials org (l5i9.9 runbook),
# run `wamn-cdc-reader`, psql writes → the R3 EVT_ stream, then the
# stream-side asserts + drills:
./target/debug/wamn-gates readerbench --nats-url nats://<node>:30493 \
  --org t10cdc --project app --env dev --expect-ids 1,2,3,… [--delete-stream]
#   + SIGKILL/restart resume, severed-python-proxy LSN hold (never touches
#   evt-nats itself), SIGTERM clean exit, zero-residue teardown (slot first).
```

Mutation harness: scratchpad `mutate_l5i9_10.py` — M1 wire `msg_id` order
swapped (named unit), M2 an unacked publish counts as acked (the live gate's
"confirmed LSN must HOLD" phase), M3 the `enabled` flag ignored (disabled
probe), M4 a missing slot silently tolerated (the CAPTURE GAP probe); all
apply/test/restore with sha256, DEBUG builds.

### [EVT-OIDMAP / wamn-l5i9.11] relation-OID → catalog-entity keying (R9b)

Docs: docs/archive/events/event-plane-jetstream.md §4/§5, docs/archive/review-findings.md R9b. The
reader resolves each relation OID to its stable catalog **entity id** via the
`wamn_entities` map (`relation_oid → entity_id, table_name`), maintained by
`publish-catalog`/`migrate-catalog` IN the DDL transaction (OID-keyed, so a
rename only updates `table_name`; pg_class OIDs survive `ALTER TABLE RENAME`).
The envelope carries `entity` (the id — ABSENT ⇒ unmapped, the
delayed-never-lost fallback) and `table` (physical name); the subject's entity
segment is the id, so consumer filters are rename-proof. Same throwaway rig as
[EVT-READER]; the live gate gains **phase F**, the rename drill.

```bash
cargo test -p wamn-event-wire                # +unmapped-marker + entity/table wire pin
cargo test -p wamn-control-provision entity_map      # the OID-keyed upsert drift guard ($2::text)
cargo test -p wamn-cdc-reader --lib          # +entity_lookup_sql pin, +map-order bundle test
# Local live gate (adds the rename drill: provision entity `sales_orders` as
# table `orders` via the REAL migrate-catalog path, wipe+publish-catalog
# backfill, rename → `orders2`, assert the pg_class OID is constant and every
# envelope/subject carries the stable id across the rename; platform tables
# publish entity-ABSENT):
WAMN_READER_PG_URL=postgres://postgres:postgres@127.0.0.1:5448/postgres \
WAMN_READER_NATS_URL=nats://127.0.0.1:4261 \
  cargo test -p wamn-cdc-reader --test event_reader_live
# In-cluster gate of record: incluster_l5i9_10.sh's shape + a rename-drill step
# driving migrate-catalog, asserted with the new readerbench flags:
./target/debug/wamn-gates readerbench --nats-url nats://<node>:30493 \
  --org t10cdc --project app --env dev --stream EVT_t10cdc_dev \
  --filter-entity sales_orders --expect-entity-id sales_orders \
  --id-field num --expect-ids 80,81,90,91,92
```

Mutation harness: scratchpad `mutate_l5i9_11.py` — M1 map upsert dropped from
migrate-catalog's apply txn, M2 dropped from publish-catalog, M3 the reader's
map lookup bypassed (everything unmapped), M4 the subject keyed by the table
even when mapped, M5 the upsert loses `ON CONFLICT` — each fails a NAMED live
assert; apply/test/restore with sha256, DEBUG builds.

### [EVT-CAUSATION-STITCH] reader stitches wamn.causation (l5i9.12.1)

Docs: docs/archive/events/event-plane-jetstream.md §4 · Recipe extends [EVT-READER]/[EVT-OIDMAP]

The reader enables protocol Messages (`with_messages(true)`) and switches
`drain()` to **buffer-per-txn**: it collects a transaction's row events and
captures a transactional `wamn.causation` message whenever it lands, then at
`Commit` publishes every row with the `{run,root,depth}` stamp attached — robust
to whether the message frame arrives before or after the rows. The LSN still
advances only after every row is acked. The live gate gains **phase G**. (The
plugin-emit half — how a run-owned txn gets the message — is the split sibling
l5i9.12.2; here the message is emitted by test SQL.)

```bash
cargo test -p wamn-event-wire                        # causation wire pin (run/root/depth)
cargo test -p wamn-cdc-reader --lib parse_causation  # only a transactional wamn.causation frame counts
# Local live gate: phase G drives BOTH frame orderings (message-at-BEGIN and
# message-AFTER-rows), a plain txn (causation ABSENT), and a rolled-back txn
# that emitted one (nothing published — transactional):
WAMN_READER_PG_URL=postgres://postgres:postgres@127.0.0.1:5448/postgres \
WAMN_READER_NATS_URL=nats://127.0.0.1:4261 \
  cargo test -p wamn-cdc-reader --test event_reader_live
# In-cluster gate of record (local reader binary + wamn-pg + evt-nats R3): one
# txn emits the message AFTER 5 inserts; the new readerbench flag asserts every
# envelope carries the run. Script: scratchpad incluster_l5i9_12.sh.
./target/debug/wamn-gates readerbench --nats-url nats://<node>:30493 \
  --org t121cau --project app --env dev --stream EVT_t121cau_dev \
  --entity receipts --expect-ids 1,2,3,4,5 --expect-causation-run gate-run-1
```

Mutation harness: scratchpad `mutate_l5i9_12.py` — M1 messages disabled
(`with_messages(false)`), M2 the causation stamp dropped at `Commit`, M3 the
exact-prefix guard broken — M1/M2 fail live-gate phase G, M3 fails the
`parse_causation` unit test; apply/test/restore with sha256, DEBUG builds.

### [EVT-CAUSATION-EMIT] the plugin emits wamn.causation per run-owned txn (l5i9.12.2)

Docs: docs/archive/events/event-plane-jetstream.md §4 · The emit half of the split above.

The trusted flow-runner declares the run it drives through a new **additive**
`wamn:runner/causation.set-run-context` channel (linked ONLY into the compiled-in
runner — `wamn:postgres` stays FROZEN 0.1.0, no S2 re-gate); the host feeds a
per-component `current_run` map on the `WamnPostgres` plugin, and
`begin_with_claims` appends a transactional
`pg_logical_emit_message(true,'wamn.causation',{run,root,depth})` to every
run-owned txn. MVP: root runs only → `root = run`, `depth = 0` (no claim-SQL
change, no guest-data change; event-chain root/depth thread from the materializer
l5i9.17). A guest raw-SQL `wamn.*` emit is rejected on the query/execute/cursor
surface (defense-in-depth blocklist, AR1). HOST-changed (plugin ships in
wamn-host) AND GUEST-changed (the runner declares the channel) — the in-cluster
gate rebakes the host image + rebuilds the flowrunner wasm.

```bash
# Unit boundary: the Postgres plugin is owned by the wamn-runtime library.
# recipe-test: H5-CAUSATION | unit | wamn-runtime | lib | - | plugins::wamn_postgres::claims::tests:: | 15 | crates/platform/runtime/src/plugins/wamn_postgres/claims.rs claims, causation emit, forgery guard, and current-run map
cargo test -p wamn-runtime --lib plugins::wamn_postgres::claims::tests::
(cd components && cargo build --release --target wasm32-wasip2 -p flowrunner)  # guest declares the channel
# Local live proof — the REAL plugin emit through the REAL runner (both drive
# paths: run/run_s6/run_until_kill via execute(), run_next via execute_claimed()):
docker run -d --name caus-pg -p 5491:5432 -e POSTGRES_PASSWORD=postgres postgres:18 -c wal_level=logical
docker exec caus-pg psql -U postgres -c "CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER;"
docker exec caus-pg psql -U postgres -tAc "SELECT pg_create_logical_replication_slot('caus','test_decoding')"
./target/debug/wamn-gates runnerbench --flowrunner components/target/wasm32-wasip2/release/flowrunner.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5491/postgres \
  --admin-database-url postgres://postgres:postgres@127.0.0.1:5491/postgres   # runs drive; NOSUPERUSER app role emits, writes never break
# peek: a transactional wamn.causation {run,run,0} rides EACH run's sink-write txn, content == run_id:
docker exec caus-pg psql -U postgres -tAc "SELECT data FROM pg_logical_slot_peek_changes('caus',NULL,1500)" | grep -E "wamn.causation|sink: INSERT"
docker rm -f caus-pg
# In-cluster gate of record (deployed image drives real runs; the reader stitch of
# the identical bytes is already proven at l5i9.12.1's in-cluster R3 + phase G):
docker build --target host -t wamn-host:dev . && docker build --target gates -t wamn-gates:dev . && kind load docker-image wamn-host:dev --name wamn
```

Mutation harness: scratchpad `mutate_l5i9_12_2.py` — M1 emit dropped from
`build_claim_batch`, M2 `set_current_run` does not store the run, M3 the forgery
guard always passes — each fails a NAMED `wamn_postgres::tests` unit test;
apply/test/restore with sha256, DEBUG builds.

### [EVT-CAUSATION-E2E / wamn-ec7j] one deployed run reaches the R3 stream with its own run id

This closes the composed boundary left deliberately separate by the stitch and
emit gates above. `causation-e2e` uses the production invocation-admission
transaction to create one gate-scoped run and queue row atomically, then the
existing 2-replica runner executes its `pg-write`. The shipped `wamn-cdc-reader`
executable decodes that transaction from the shared fixture's logical WAL into
a gate-scoped R3 stream. `readerbench --expect-causation-run` filters the mapped
sink entity and requires the completed run's exact `run_id` both as the sink id
and on every delivered envelope.

The Job does not deploy or reconfigure the runner and never inserts `run_queue`
directly. Its immutable release rows are a dormant, byte-pinned extension of
the canonical shared fixture (owner decision 2026-08-02); only the temporary
catalog head/activation makes the invocation target admissible. Always-run
teardown removes that activation plus the flow/run rows, sink and entity map,
registry org, replication role/publication/slot, and JetStream stream. Final
assertions require the original flow/run/node-run/queue counts, exact dormant
release bytes, and zero mutable/runtime residue.

```bash
# recipe-test: H5-CAUSATION-E2E | integration | wamn-proof-integration | lib | - | causation_e2e::tests:: | 2 | production invocation/pg-write fixture plus R3 and exact-run reader arguments
CARGO_TARGET_DIR=/tmp/wamn-target-ec7j cargo test --locked -p wamn-proof-integration --lib causation_e2e::tests::
cargo clippy --locked -p wamn-proof-integration -p wamn-gates --all-targets -- -D warnings
tools/gate-mutants/causation-e2e.sh run-all

docker build --target gates -t wamn-gates:dev .
kind load docker-image wamn-gates:dev --name wamn
kubectl -n wamn-system delete job causation-e2e --ignore-not-found
kubectl -n wamn-system apply -f deploy/gates/causation-e2e-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/causation-e2e --timeout=240s
kubectl -n wamn-system logs job/causation-e2e  # -> overall PASS: true
```

Mutation harness: `tools/gate-mutants/causation-e2e.sh` applies exact-hash
mutants for the admitted `pg-write`, the reader's R3 request, and the exact run-id
causation assertion. Each must turn its named debug unit gate red; the trap
restores and verifies all starting hashes. Typed results live in
`architecture/evidence/mutations/causation-e2e.json`.

### [EVT-REG / wamn-l5i9.16] registration surface — catalog + minimal API

Docs: docs/archive/events/event-plane-jetstream.md §5. The **declaration surface** the
materializer (l5i9.17) consumes: a registration = subscribing flow id, entity id
(the rename-proof catalog **entity id**, EVT-OIDMAP — never a table name), a
non-empty op set, an optional JMESPath condition, and an optional JMESPath
partition-key expr. Model + validation in the pure `wamn-event-reg` crate;
storage `catalog.event_registrations` (deploy/sql/catalog-schema.sql, mirrors
`rls_policies` — jsonb doc + denormalized `flow_id`/`entity_id` columns, live-
catalog-scoped not version-tied, tenant-RLS'd, indexed by entity for 11.8 impact
analysis wamn-wvb); minimal CRUD builders in `wamn-api` (`registration` module —
pinned identifiers, `$n` values, `tenant_id` server-side). NO materializer, NO
reader change, NO UI (parked). The condition/partition-key are stored as JMESPath
strings, validated for SYNTAX at write time (the materializer owns evaluation); a
condition referencing `old` ("changed-to") is expressible but its old image needs
REPLICA IDENTITY FULL (l5i9.31) — this surface never flips replica identity. It
does DETECT the gap (EVT-RI-ORCH, wamn-l5i9.66): a create/update that needs the
old image on an entity still at DEFAULT returns an additive
`pending-replica-identity-reconcile` warning (the pure
`wamn_api::pending_replica_identity_warning` + `attach_warning`, keyed on the
SAME `EventRegistration::requires_replica_identity_full` predicate the l5i9.31
reconciler folds, so it can never diverge), so a caller sees the gap the periodic
CronJob (wamn-l5i9.65) will close. Detect-only — still no ALTER under `wamn_app`.
Note: the api-gateway does not yet ROUTE registration writes over HTTP (the
l5i9.16 CRUD is builders-only); the guest links this warning surface and builds
clean for wasm32, and attaches the warning when that route lands (deferred).

```bash
cargo test -p wamn-event-reg              # validation rules (entity-by-id, ops non-empty/dedup, JMESPath syntax, schema-version, round-trip)
cargo test -p wamn-api                     # +registration builder shapes + storage-schema drift guard + the l5i9.66 pending-reconcile warning (pure: detector direction + additive-envelope PRESENT/ABSENT)
cargo clippy -p wamn-event-reg -p wamn-api --all-targets
# Local live-apply gate (throwaway PG): applies the REAL catalog-schema.sql, then
# drives create/list/get/update/delete through the wamn-api builders AS wamn_app
# under a tenant claim — round-trips the document + proves RLS tenant isolation;
# then (l5i9.66 phase) provisions the entity table, flips it 'd'->'f' live, and
# asserts the warning is PRESENT on the DEFAULT table / ABSENT once FULL.
# Hermetic (drops+recreates the catalog schema, teardown leaves nothing):
docker run -d --name evtreg-pg -p 55433:5432 -e POSTGRES_PASSWORD=postgres postgres:18
WAMN_API_PG_URL=postgres://postgres:postgres@127.0.0.1:55433/postgres \
  cargo test -p wamn-api --test registration_live
docker rm -f evtreg-pg
# wamn-api is an api-gateway guest dep; confirm the wasm build. wamn-event-reg is
# now a RUNTIME dep of wamn-api (the l5i9.66 warning keys on it) — pure, so it and
# jmespath/schemars compile for wasm32 (the migration engine stays out):
(cd components && cargo build -p api-gateway --target wasm32-wasip2)
```

### [EVT-REG/D24 / wamn-rmxa] publish/migrate-catalog refuse an orphaning publish

Docs: platform-plan decision table D24. Both `publish-catalog` and
`migrate-catalog` REFUSE a catalog that would remove an entity still referenced
by a row in `catalog.event_registrations` — naming every orphaned registration
(id + tenant + entity) across ALL tenants — and never seed or prune
registrations (the owner deletes them via the wamn-api registration surface
first). The pure decision + the `$n` read builder live in `wamn-schema-control`
(`check_registration_orphans`, `sql::select_registrations_for_catalog_sql`); the
two `wamn-ctl` verbs share one read-only guard helper
(`publish_catalog::guard_registration_orphans`) that runs BEFORE any mutation.
`migrate-catalog --dry-run` runs the SAME read-only probe (wamn-1bfe): it
surfaces the refusal as a marked `[dry-run] would REFUSE at apply` finding and
exits nonzero on an orphaning target — the orphan refusal is unconditional (no
override flag), so dry-run treats it like the stale-base / not-forward
preconditions it already fails on, rather than merely reporting it as it does the
overridable destructive gate — so an operator can no longer dry-run clean then
fail the real run.

```bash
cargo test -p wamn-schema-control                 # pure decision + mutation-flavored unit tests
cargo clippy -p wamn-schema-control -p wamn-ctl --all-targets
# Live gate (throwaway PG): drives the REAL verbs — seed+publish a catalog, register
# entity A as two tenants, attempt a publish/migrate that removes A → REFUSAL naming
# both tenants' rows + NOTHING mutated; delete the registrations → proceeds; and a
# removal of an UNREFERENCED entity proceeds. The dry-run scenario (wamn-1bfe)
# asserts `migrate-catalog --dry-run` surfaces the same verdict + mutates nothing.
# Hermetic (drops+recreates its schemas):
docker run -d --name wave3-pg-rmxa -p 55431:5432 -e POSTGRES_PASSWORD=postgres postgres:18
WAMN_CTL_PG_URL=postgres://postgres:postgres@127.0.0.1:55431/postgres \
  cargo test -p wamn-ctl --test orphan_guard_live -- --nocapture
docker rm -f wave3-pg-rmxa
```

### [11.2 / wamn-828] test cases as catalog data — flow-tests schema, promote-with-flow

Docs: docs/archive/testing/scenario-catalog.md. A flow's test suites/cases live as catalog data
(`deploy/sql/flow-tests.sql`: `wamn_run.test_suites` + `wamn_run.test_cases`,
both FORCE-RLS + `wamn_app` grants), versioned WITH the flow via the FK to
`wamn_run.flows` ON DELETE CASCADE. Suites copy through
`copy-project-env --include definition` (block 5); the mutable `wamn_run.flows`
registry is NOT copied (immutable release is authoritative, 5wd1.46), so a
destination flow registration is a precondition and
`wamn-schema-control::check_suite_orphans` refuses FIRST (D24 shape) a copy carrying a
suite pinned to a version the destination does not already hold (re-keying
suites onto release identity is wamn-l2mi). The suite/case envelope
and validation live in `wamn-scenario-model`; `wamn-scenario-catalog` owns the
SQL queries, ordering, compatibility translations, and pin-from-run transform.
reconcile-run-plane manages the new tables (they are in `RUN_PLANE_FILES`).

```bash
cargo test -p wamn-scenario-model -p wamn-scenario-catalog -p wamn-schema-control
cargo test -p wamn-ctl                                    # driver units
cargo clippy -p wamn-scenario-catalog -p wamn-schema-control -p wamn-ctl -p wamn-gates --all-targets
# Live promote gate (throwaway PG): drives the REAL copy-project-env verb across
# two project-env databases — suite/cases copy onto a dst that pre-registers
# flow v1 (dst registration is a precondition; the mutable flows registry is
# not copied), a foreign tenant sees ZERO suites (RLS), dropping flow v1
# CASCADES its suite (FK), and an orphan-pinned suite copy is REFUSED. Applies
# deploy/sql/postgres-init.sql (dedicated DB `wamn`); URLs target /wamn:
docker run -d --name lane-828-pg -p 5465:5432 -e POSTGRES_PASSWORD=postgres \
  -v "$PWD/deploy/sql/postgres-init.sql:/docker-entrypoint-initdb.d/init.sql:ro" postgres:18
# (postgres:18 inits-then-restarts — wait for a DOUBLE pg_isready before connecting)
WAMN_CTL_PG_URL=postgres://postgres:postgres@127.0.0.1:5465/wamn \
  cargo test -p wamn-ctl --test suite_promote_live -- --nocapture
# In-cluster gate-of-record candidate: the same arc in an ephemeral schema
# (envelope round-trip + version binding + RLS + FK cascade):
WAMN_PG_URL=postgres://wamn_app:wamn_app@127.0.0.1:5465/wamn \
WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:5465/wamn \
  ./target/debug/wamn-gates --log-level error suiteproof
docker rm -f lane-828-pg
# IN-CLUSTER: deploy/gates/suiteproof-job.yaml (kubectl apply; wait complete; logs).
# 3 mutants killed (apply/test/restore, debug builds): M1 copy block #5 skipped →
# suite_promote_live PROMOTE assert; M2 suite-orphan guard inverted →
# suite_promote_live GUARD assert (+ orphan.rs suite_pinned_to_an_absent_version…);
# M3 RLS policy dropped from flow-tests.sql → suiteproof RLS zero-rows assert.
```

### [CALLABLE-FLOWS-POC / wamn-5wd1.40] from-zero F0–F4 catalog

The promoted material-receiving catalog is the single from-zero data fixture
for F0–F4. The local tests pin its receipt-line and hold natural keys,
required `dispositions.decided_at`, and unique `disposition_reviews` key. The
live Job compiles that same fixture through `wamn-schema-compiler`, injects a
failure halfway through the ordered DDL in one transaction, verifies zero
residue, then proves a clean retry is byte-identical and the database rejects
all named negative cases.

```bash
# recipe-test: H5-CALLABLE-FLOW-SCHEMA | system | wamn-proof-system | lib | - | pocsuiteproof::tests:: | 4 | tests/system/src/pocsuiteproof.rs canonical F0-F4 schema contract and fault seam
cargo test --locked -p wamn-proof-system --lib pocsuiteproof::tests::

kubectl -n wamn-system apply -f deploy/gates/callable-flow-schema-job.yaml
kubectl -n wamn-system wait --for=condition=complete \
  job/callable-flow-schema --timeout=180s
kubectl -n wamn-system logs job/callable-flow-schema
```

### [POC-TESTS / wamn-3rj] F1/F3/F4 stored suites + drive-and-fold (pocsuiteproof)

Docs: docs/archive/poc/poc-material-receiving.md ("Tests", L37–39). The F1/F3/F4 POC test
suites as STORED DATA — `wamn-scenario-model` envelopes persisted by
`wamn-scenario-catalog`
(`deploy/gates/poc-f{1,3,4}-suite.json`, case bodies = `wamn_scenario_model::TestCase`)
seeded into `wamn_run.test_suites`/`test_cases` (the 11.2 tables) and then PROVEN
REAL: the `pocsuiteproof` gate seeds them and drives each flow ONCE through its
own harness path, folding every stored assertion through `wamn_scenario_model::evaluate`.

What the stored suites cover (the expressible core) vs what stays in the sibling
proof gates:
- **F1** (`poc-f1-suite`): flow-level cases over `receipt-received` v1. The
  legacy embedded webhook driver was retired by `wamn-5wd1.57`; the callable
  graph/release proof below owns the current F1 path and `.9` owns the composed
  from-zero stored-suite campaign.
- **F3** (`poc-f3-suite`): `escalate-stale-holds` v1 under the ExecutionHost
  scenario capability set at a fixed virtual epoch. The **48h cutoff** is proven by
  time-offset arithmetic (`scheduled-at − 48h`) against **epoch-anchored** seed rows
  (2 stale opened 49h before the epoch, 1 fresh AT it, 1 stale-disposed) — 48h
  evaluated in wall-clock milliseconds. Asserts escalated=2 / open=1 / disposed=1
  (`DbState`) + the two notify `Egress{count 2, none-denied}`. The credential
  digest + cycle-visit count stay in `f3proof`.
- **F4** (`poc-f4-suite`): `disposition-recorded` v1 under the double set + a real
  serve-node hosting `disposition-node.wasm` (the signed, platform-owned F2 hop)
  + a loopback ERP sink. The **flow-egress spy** =
  `Egress{ExactlyThese([POST /dispositions])}` — exactly the ONE callback through
  the portable `erp-callback` connection, nothing else (an extra call fails the
  set) — plus `none-denied` + `RunOutcome`. The host-owned node hop is not flow
  egress. The graph has no endpoint, URL, `allowed-hosts`, or idempotency toggle;
  the HTTP effect stamps the system-owned stable key. The 429/Retry-After park,
  no-reclaim-during-backoff,
  one-effective-delivery, and no-stampede mechanics are NOT expressible in the
  stored vocabulary (the ERP ledger is an in-memory audit, not a DB table) and stay
  in `f4proof`.

The fixture-realism and stored-data tests remain useful independently. The old
hard-wired F1 drive is RETIRED (wamn-97sj), not merely demoted: it read
`poc-webhook-f1.wasm`, which the callable cutover deleted, so the leg could not
run at all — and because it ran first it also blocked F3/F4 on any invocation
without `--seed-only`. The gate now has no `--webhook-entry` flag; F1 is
seed-only (phases A and C still round-trip, RLS-check, and FK-bind its suite),
and the callable F1 arc (`callable_f1` + `deploy/gates/callable-flow-f1-job.yaml`)
owns the flow's behaviour.

```bash
# recipe-test: H5-F1-FIXTURE | system | wamn-test-fixtures | lib | - | f1fixture::tests:: | 1 | shared F1 catalog, flow, seed, and burst fixture coherence
cargo test --locked -p wamn-test-fixtures --lib f1fixture::tests::
```

```bash
# Unit / drift / coherence tests (pure — no DB): the 3 embedded suites parse +
# validate-on-write, the F1 flow-ref binding, the F3/F4 graph copies mirror the
# committed source fixtures (deploy/poc/f3-flow.json,
# crates/execution/flow-model/tests/fixtures/f4-disposition-recorded.flow.json), the F3
# epoch-anchor straddles the cutoff, the F4 flow-egress spy names exactly
# {/dispositions} (the signed node hop is platform transport):
# Integration-proof boundary: pocsuiteproof fixtures and assertions live in
# wamn-proof-integration; wamn-gates only routes the executable subcommand.
# recipe-test: H5-POCSUITEPROOF | integration | wamn-proof-integration | lib | - | pocsuiteproof::tests:: | 7 | tests/integration/src/pocsuiteproof.rs stored F1/F3/F4 suite fixtures
cargo test -p wamn-proof-integration --lib pocsuiteproof::tests::
cargo test -p wamn-scenario-model -p wamn-scenario-catalog

# Seed-only (the wave-end composition gate's path): seed the 3 suites into a
# shared target schema/tenant at a flow version and STOP (no drive, no drop) —
# 0lfu then loads them by flow@version + tenant. seed-only is ADDITIVE on a
# LIVE target: it ensure_*s the tables IF NOT EXISTS (never DROPs the schema)
# and registers missing flows ON CONFLICT DO NOTHING (a production-registered
# flow row keeps its graph_json/active untouched):
./target/debug/wamn-gates --log-level error pocsuiteproof --seed-only \
  --schema poc_f1 --tenant demo-tenant --flow-version 1 \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5450/wamn \
  --admin-database-url postgres://postgres:postgres@127.0.0.1:5450/wamn

# 3 mutants killed (apply/test/restore via sha256 byte-restore, debug builds):
# M1 F1/F3/F4 seed step skipped in Phase A → embedded_suites/STORE counts + drive
#   "no run facts captured"; M2 the ExactlyThese fold inverted (evaluate.rs
#   unexpected-check) → f4_egress_spy + exactly_these_catches_an_extra_call; M3 the
#   F3 epoch anchor broken (stale seeded now()-relative) → f3_epoch_anchor_straddles
#   + the live escalated=2/open=1 DbState asserts.
```

### [11.8 / wamn-wvb] schema-change impact analysis — affected flows/suites/API

Docs: docs/archive/testing/impact-analysis.md. Before a migration applies, enumerate the
dependency graph a change touches: affected entities (additive/destructive, from
the plan's per-op attribution) → flows via event registration (id-keyed,
rename-proof) + node config (NAME-keyed `config["entity"]`, NOT rename-proof) →
those flows' test suites (all versions) → the generated-API resources. The pure
decision is `crates/schema/control/src/impact` (`analyze` → `ImpactReport`); the `$n` reads live
next to their D24/suite siblings in `crates/schema/control/src/sql.rs`. `wamn-ctl
impact-report` is the read-only surface; `migrate-catalog` ALWAYS renders the
report and `--acknowledge-impact` REFUSES a destructive plan with dependent
flows/suites (typed error, non-zero exit, before the apply tx — nothing mutated),
orthogonal to `--confirm-with-backup`. The report enumerates the
`(tenant, flow_id, flow_version, suite_id)` tuples that would run; suite EXECUTION
of those tuples is the wamn-0lfu executor (`testkitbench --impact-report`, see
[11.2-exec] above). `impact::suite_selectors[_json]` flattens
`ImpactReport.entities[].suites[]` into that executor's `SuiteSelector` array —
and the same flattening is what the [11.7] publish gate checks evidence for
(below). `migrate-catalog` itself does NOT run suites: the gate binds to stored
suite results at promotion, not to a re-run at migration.

```bash
cargo test -p wamn-schema-control                       # pure decision + drift-guard pins (3 mutants killed here)
cargo test -p wamn-ctl                                    # driver units
cargo clippy -p wamn-schema-control -p wamn-ctl -p wamn-gates --all-targets
# Live gate (throwaway PG): materialize v1 {orders, audit}, seed a dependent flow
# per entity (registration + active node-config graph + suite), stage v2 =
# destructive-on-orders (drop column) + additive-on-audit (add column) → the report
# names EXACTLY orders' flow/suite/api and NOT audit's; migrate REFUSES without
# --acknowledge-impact (nothing mutated) and PROCEEDS with it. Hermetic:
docker run -d --name wave-wvb-pg -p 15502:5432 -e POSTGRES_PASSWORD=pg postgres:18
WAMN_CTL_PG_URL=postgres://postgres:pg@127.0.0.1:15502/postgres \
  cargo test -p wamn-ctl --test impact_report_live -- --nocapture
# In-cluster gate-of-record candidate: the analysis in an ephemeral schema
# (name-keyed node-config + suite + api edges; destructive gates, additive does not):
WAMN_PG_URL=postgres://wamn_app:wamn_app@127.0.0.1:15502/postgres \
WAMN_PG_ADMIN_URL=postgres://postgres:pg@127.0.0.1:15502/postgres \
  ./target/debug/wamn-gates --log-level error impactproof
docker rm -f wave-wvb-pg
# IN-CLUSTER: deploy/gates/impactproof-job.yaml (kubectl apply; wait complete; logs).
# 3 mutants killed (apply/test/restore, debug builds): M1 entity-match inverted →
# wamn-schema-control untouched_entity_flows_are_not_reported; M2 destructive classification
# forced additive → destructive_change_with_impact_requires_acknowledge; M3
# node-config keyed on entity.id not name → node_config_edge_keys_on_entity_name_not_id.
```

### [11.7 / wamn-12g] publish gates & policy — per-project rules + gate audit

Design: `docs/archive/platform-plan.md` §11.7. Per-project rules ("prod deploys require
green suite") enforced on the deploy/promote verb, with every verdict recorded.

POLICY lives in the T1 registry in two layers —
`registry.env_policies.requires_green_suite` (the org-wide per-env default,
`false` so an upgrade gates nothing) and `registry.project_publish_policies` (the
per-project override, authoritative in BOTH directions). They are combined by ONE
pure function, `wamn_control_registry::resolve_publish_policy`, so the CLI and
the future management transport cannot disagree about what is gated.

EVIDENCE is `wamn_schema_control::publish_gate`. For every `SuiteEdge` the 11.8
impact report names, it requires a `wamn_run.authoring_suite_reports` row that
(a) passed, (b) carries RELEASE lineage for the exact
`(catalog_id, catalog_version, environment)` being promoted, and (c) whose
recorded `artifact_hash` equals the hash that release actually pins for the flow
(`release_flows` ⋈ `flow_artifacts`). (c) is FRESHNESS as a hash comparison, not
a timestamp window — a hash match is a statement about the current bytes, where
"recent" is only a guess about them. Each miss is its own typed defect
(`no-report` / `draft-only` / `foreign-release` / `failed` / `unpinned-flow` /
`stale-artifact`) so a refusal names the cause.

ENFORCEMENT is `copy-project-env --include definition`, after every read and
before the definition transaction — a refusal mutates nothing but its own audit
row. `migrate-catalog` / `publish-catalog` are unchanged. The decision ships as a
LIBRARY (`wamn-schema-control`, which `services/scenario-worker` already depends
on) so wamn-ftfc.33 can mount the same seam at the authenticated transport;
ma5's human/promoter proof binds there, not in the CLI.

AUDIT is `catalog.publish_gate_audit` — append-only, recording PASSES AND
REFUSALS with the per-suite evidence pointer. Deliberately NOT
`authoring_command_audit`: that ledger's rows carry a verified principal and
`wamn-ctl` is an operator CLI with none.

FAIL-CLOSED: no shipped producer writes `authoring_suite_reports` yet (that
backend is wamn-ftfc.28/.33), so an env gated today refuses with `no-report`
until it lands. A definition copy now REQUIRES `--system-database-url` — a
promotion that cannot read its policy cannot be shown to be allowed.

```bash
# Pure decision (evidence classification, fail-closed, refusal rendering) + the
# registry policy resolution + the SQL drift guards pinning the evidence read,
# the freshness join, and the ledger write against deploy/sql:
# recipe-test: H5-PUBLISH-GATE | integration | wamn-schema-control | lib | - | publish_gate:: | 8 | crates/schema/control/src/publish_gate.rs green-suite decision
cargo test -p wamn-schema-control --lib publish_gate::
cargo test -p wamn-control-registry --lib publish_policy::
cargo clippy -p wamn-schema-control -p wamn-control-registry --all-targets --locked -- -D warnings

# Live gate (throwaway PG): a gated env refuses with no evidence and mutates
# nothing; fresh release-pinned evidence promotes and the ledger keeps the report
# id; a pass against SUPERSEDED flow bytes is refused; a per-project override
# gates (and exempts) one project; the ledger refuses UPDATE and DELETE.
# Evidence is seeded through the REAL reservation -> case-fact -> report triggers
# and the release is minted by the REAL publish-catalog writer. Hermetic: each
# scenario drops+recreates its own system/src/dst databases.
docker run -d --name wave6-12g-pg -p 15612:5432 -e POSTGRES_PASSWORD=pg postgres:18
WAMN_CTL_PG_URL=postgres://postgres:pg@127.0.0.1:15612/postgres \
  cargo test -p wamn-ctl --test publish_gate_live -- --test-threads=1
docker rm -f wave6-12g-pg
# 3 mutants killed (apply/test/restore via sha256 byte-restore, debug builds):
# M1 missing evidence yields no defect → wamn-schema-control
#   required_gate_refuses_when_no_report_exists + a_gated_env_refuses_a_promotion_with_no_suite_report;
# M2 the artifact-hash freshness comparison disabled →
#   stale_artifact_hash_is_not_evidence_about_the_shipped_bytes +
#   a_pass_against_superseded_flow_bytes_is_refused;
# M3 the verdict recorded only after the refusal returns →
#   a_refusal_is_recorded_in_the_append_only_ledger.
# LIVE T1 SYSDB APPLY IS A DEFERRAL: the additive registry DDL
# (env_policies.requires_green_suite + project_publish_policies) is owner-run.
```

### [EVT-MAT / wamn-l5i9.17] materializer — CDC events → flow runs (Service-first)

Docs: docs/archive/events/event-plane-jetstream.md §5 · decisions D19–D24. The Service-first
materializer: a wasi:cli/run SERVICE workload (`spec.service`, E11/D21 + E12 —
deploy/platform/materializer.example.yaml) and the **first `wamn:jetstream`
importer** (the plugin is now wired in the washlet; the doorbell rides the
host's control-plane NATS client). Per event: registration match (rename-proof
entity-id) → tenant guard (unscopable = alertable refusal, never a cross-tenant
admission) → causation budget → condition eval (root-`old` conditions HELD
until l5i9.31 — old-absent is cannot-evaluate, never condition-false) → lock
the authoritative catalog head and resolve the immutable release membership
and artifact → verify the live registration document/hash → deterministic
registration-scoped event identity → centralized `wamn_run.admit` in one
transaction → post-commit doorbell → ack. The business input contains only the
normative event envelope; trusted trigger/entity/table/sequence metadata is
stored in invocation context. Event admission never resolves an attachment and
the producer performs no direct run or queue insert. Decisions are the PURE
`wamn-materializer` crate; the guest (`components/execution/materializer`) is
the effect shell.

```bash
cargo test --locked --manifest-path components/Cargo.toml -p materializer
cargo test --locked -p wamn-materializer
cargo test --locked -p wamn-proof-integration --lib materializer::tests::
cargo test --locked -p wamn-proof-integration --lib matbench::tests::
# Unit and contract boundaries: the JetStream plugin and its WIT target are
# owned by wamn-runtime; the host package is now a binary-only composition leaf.
# recipe-test: H5-JETSTREAM-UNIT | unit | wamn-runtime | lib | - | plugins::wamn_jetstream::tests:: | 12 | crates/platform/runtime/src/plugins/wamn_jetstream.rs mappings, doorbell policy, and optional live round-trip
cargo test -p wamn-runtime --lib plugins::wamn_jetstream::tests::
# recipe-test: H5-JETSTREAM-WIT-MATERIALIZER | contract | wamn-runtime | test | jetstream_wit_coherence | - | 3 | crates/platform/runtime/tests/jetstream_wit_coherence.rs docs, host, and guest WIT copies
cargo test -p wamn-runtime --test jetstream_wit_coherence
(cd components && cargo build --locked -p materializer --target wasm32-wasip2)
# Live gate — REAL guest + REAL deploy/sql DDL (include_str! — drift-proof) +
# REAL JetStream; 17 asserts: rows/ids/keys/policy, causation thread, distinct
# refusal counters, doorbell rings, burst drain (C-MAT numbers), and a full
# server-side-consumer-delete redelivery proving ON CONFLICT exactly-once:
docker run -d --name mat-pg -p 55461:5432 -e POSTGRES_PASSWORD=matpass postgres:18
docker run -d --name mat-nats -p 44461:4222 nats:2.10 -js
./target/debug/wamn-gates matbench \
  --component components/target/wasm32-wasip2/debug/materializer.wasm \
  --admin-database-url postgres://postgres:matpass@127.0.0.1:55461/postgres \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:55461/postgres \
  --nats-url nats://127.0.0.1:44461
docker rm -f mat-pg mat-nats
# In-cluster: rebake host (plugin wiring) + run-worker (flowrunner causation
# thread) + gates (matbench + /bench/materializer.wasm), kind load, then the
# matbench Job / the CDC-write→reader→stream→materializer→run e2e.
```

Mutation harness: scratchpad `mutate_l5i9_17.py` — M1 depth guard off-by-one,
M2 root-`old` detection loses Subexpr context, M3 `enqueue_evt_sql` drops
`stream_seq`, M4 `plan_claim` loses the numeric tiebreak, M6 doorbell-subject
typo — each fails a NAMED unit test; M5 (guest skips the doorbell ring) fails
matbench's `8 doorbell rings` assert. Apply/test/restore with sha256, DEBUG.

### [E10-E2E / wamn-l5i9.57] samplebench — component-driven wamn:jetstream e2e + the js-sample adopter template

Docs: docs/archive/events/event-plane-jetstream.md §5 · docs/archive/contracts/wamn-jetstream.wit (FROZEN 0.1.0).
`components/samples/js-sample` is the **adopter template** — the smallest
wasi:cli/run guest that drives BOTH sides of the frozen `wamn:jetstream@0.1.0`
package and the **first `producer` importer** (the materializer, l5i9.17, only
consumes). It binds a durable pull consumer, drains it, and per event PUBLISHes
a derived message carrying a deterministic `Nats-Msg-Id` (`<prefix>:<input
stream-seq>` — so a redelivered input re-publishes an identical id and dedupes),
then acks; a persistent `publish-rejected` terminates the input. `samplebench`
drives it via CommandPre + the REAL `WamnJetstream` plugin over a throwaway
JetStream (input + output streams), asserting: N fetched+acked, N derived stored
on the output subject with server acks, ack-floor-advanced (rebind fetches
nothing), full-redelivery dedupe (delete the durable → same ids come back
`duplicate = true`, output count unchanged), and the producer error path
(publish to an uncovered subject → `publish-rejected` surfaces as a `js-error`).

```bash
# Contract boundary: this named integration target belongs to wamn-runtime.
# recipe-test: H5-JETSTREAM-WIT-SAMPLE | contract | wamn-runtime | test | jetstream_wit_coherence | - | 3 | crates/platform/runtime/tests/jetstream_wit_coherence.rs materializer and js-sample WIT coherence
cargo test -p wamn-runtime --test jetstream_wit_coherence
(cd components && cargo build -p js-sample --target wasm32-wasip2 --release)
# Local gate — REAL guest + REAL WamnJetstream plugin + REAL JetStream:
docker run -d --name sample-nats -p 44232:4222 nats:2.10 -js
./target/debug/wamn-gates samplebench \
  --component components/target/wasm32-wasip2/release/js-sample.wasm \
  --nats-url nats://127.0.0.1:44232
docker rm -f sample-nats
# In-cluster: rebake gates (samplebench + /bench/js-sample.wasm), kind load,
# then the samplebench Job against the data-plane evt-nats (no Postgres):
#   kubectl -n wamn-system apply -f deploy/gates/samplebench-job.yaml
#   kubectl -n wamn-system wait --for=condition=complete job/samplebench --timeout=300s
#   kubectl -n wamn-system logs job/samplebench
```

### [EVT-CUTOVER / wamn-l5i9.18] — RETIRED (l5i9.19 teardown)

The cutover shipped and the comparison machinery retired with it: `cutbench`,
the `wamn_run.evt_shadow` ledger, registration `state: shadow|live` (owner
decision 2026-07-20: removed entirely — no permanent dual mode), the
dispatcher's `cdc_live_flows` yield guard, and deploy/gates/cutbench-job.yaml
were all deleted at the §3 teardown (executed 2026-07-20). The definition of
the comparison and its evidence live in docs/archive/events/event-plane-jetstream.md §7
Phase 2 (status note) + the l5i9.18 bead. Post-teardown, row events have ONE
path: CDC reader → JetStream → materializer ([EVT-MAT], [EVT-READER],
[EVT-NATS], [E10-E2E] are the standing gates).

### [EVT-RI-E2E / wamn-3glr] rie2ebench — reader-inclusive REPLICA IDENTITY flip e2e

Docs: docs/archive/events/event-plane-jetstream.md §7 · decisions D19/l5i9.31/l5i9.61. The
coverage the l5i9.19 teardown deleted with `cutbench`'s phase 3: `matbench`
covers the old-image-absent refusal + a SYNTHESIZED FULL old image (a
hand-published tape), and `ri_orch_live` covers the ctl flip machinery on
`pg_class.relreplident` — but NO gate proved a REAL decoded WAL old image
reaching the materializer AFTER a live RI flip. `rie2ebench` embeds the REAL
`wamn-cdc-reader` service body (`run_with_token`) as a tokio task next to the
REAL materializer guest (matbench harness shape), over a throwaway
`wal_level=logical` Postgres + throwaway JetStream it OWNS. ONE FULL-flipped
entity (`dispositions`, a bare `id uuid` PK), ONE delete-subscribed flow:
(1) pre-flip DELETE under RI DEFAULT → the reader decodes a key-only old image →
the materializer REFUSES it (`tenant-unscopable`, alertable, never
condition-false); (2) flip RI→FULL via the REAL `reconcile_replica_identity`;
(3) post-flip DELETE under RI FULL → the reader decodes a REAL full old image
carrying `tenant_id` → the materializer tenant-scopes it and enqueues a scoped
`disp-del:evt:<stream_seq>` run + rings the doorbell. Asserts the NON-RETROACTIVE
boundary: the pre-flip DEFAULT delete stays refused (never retro-fires). The slot
is created LAST (provisioning + seed writes stay uncaptured) and dropped
deterministically at teardown (zero residue).

```bash
# Integration-proof boundary: rie2ebench fixtures are owned by the integration
# library rather than the command router.
# recipe-test: H5-RIE2EBENCH | integration | wamn-proof-integration | lib | - | rie2ebench::tests:: | 2 | tests/integration/src/rie2ebench.rs frozen registration, flow, and catalog fixtures
cargo test -p wamn-proof-integration --lib rie2ebench::tests::
# Local gate — REAL reader + REAL materializer guest + REAL deploy/sql DDL +
# REAL JetStream. Postgres MUST be wal_level=logical (the real slot/reader):
docker run -d --name wamn-lanec-rie-pg -p 57231:5432 -e POSTGRES_PASSWORD=postgres \
  postgres:18 -c wal_level=logical -c fsync=off -c synchronous_commit=off
docker run -d --name wamn-lanec-rie-nats -p 57232:4222 nats:2.10 -js
./target/debug/wamn-gates rie2ebench \
  --component components/target/wasm32-wasip2/release/materializer.wasm \
  --admin-database-url postgres://postgres:postgres@127.0.0.1:57231/postgres \
  --nats-url nats://127.0.0.1:57232
docker rm -f wamn-lanec-rie-pg wamn-lanec-rie-nats
# In-cluster: deploy/gates/rie2ebench-job.yaml (cutbench-job's shape) — the
# fixture Postgres runs wal_level=logical since l5i9.18, and the gate owns a
# throwaway DATABASE (wamn_rie2e, created/dropped WITH FORCE) + its slot +
# its EVT stream, so the shared fixture keeps zero residue:
kubectl -n wamn-system apply -f deploy/gates/rie2ebench-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/rie2ebench --timeout=600s
```

Mutation harness: scratchpad `mutate_lane_c.py` — M_RI neuters the production
reconcile flip (`wamn_ctl::reconcile_replica_identity::reconcile` skips the
`ALTER … REPLICA IDENTITY`) so the table stays DEFAULT: the post-flip DELETE is
refused, and rie2ebench's `post-flip DELETE fired ONE scoped :evt: delete run`
assert FAILS. Apply/test/restore with sha256, DEBUG; rebuild wamn-gates after
restoring the dep.

### [EVT-C-CDC / wamn-l5i9.14] cdcbench ceiling campaign (measurement, not a gate)

Docs: docs/archive/events/event-plane-jetstream.md §7/§8/§11 · record docs/archive/results/ceilings.md § C-CDC.
Four axes on the rie2ebench substrate (gate-owned throwaway `wamn_ccdc`
database on a `wal_level=logical` PG, REAL deploy/sql DDL + wamn-control-provision/
wamn-control-registry builders, the REAL embedded reader via
`wamn_cdc_reader::run_with_token`, slot per-variant + always-run teardown,
zero residue): **drain** — bulk import lands behind the slot with the reader
down, then the reader starts and the gate samples stream depth + slot lag to
catch-up (variants: batched narrow, one-txn narrow, one-txn narrow decoded
under a 64kB `logical_decoding_work_mem` role GUC — the forced-spill leg of
the wamn-mu4h evidence, `pg_stat_replication_slots` counters recorded — and
one-txn wide/TOASTy); **lag** — reader live, offered single-row-txn rate
step-ramped across writer connections, slot lag sampled through every step
(the §8 knee = lag divergence), eventual completeness asserted; **ri** —
per-op WAL at REPLICA IDENTITY DEFAULT then FULL (flipped by the REAL
l5i9.31/l5i9.61 reconcile off seeded delete registrations), narrow + wide
shapes + the wide non-TOAST-column update (FULL flattens the unchanged 6 KiB
old image — the l5i9.63 number); per-op WAL brackets with the MEDIAN as the
delta statistic (FPI outliers + shared-instance ambient WAL excluded — the
C-WAL-0 per-event discipline), C-WAL-0 as the pre-CDC denominator;
**switchover** — the timed availability drill (separate mode + target: see
deploy/gates/cdcbench-switchover-job.yaml), cdc1's no-gap shape with the REAL
reader's R11 re-open ladder as the recovery, write blackout / publish gap /
catch-up timed from commit wall-times + JetStream ingest timestamps.
`--mode all` = drain+lag+ri; switchover is always explicit.

```bash
# Integration-proof boundary: cdcbench fixture and URL guards live in
# wamn-proof-integration.
# recipe-test: H5-CDCBENCH | integration | wamn-proof-integration | lib | - | cdcbench::tests:: | 4 | tests/integration/src/cdcbench.rs registration, catalog, rates, and URL helpers
cargo test -p wamn-proof-integration --lib cdcbench::tests::
# Local bring-up — REAL reader + REAL DDL + REAL JetStream (numbers are NOT
# the record; the record is the in-cluster release-image job):
docker run -d --name wamn-ccdc-pg -p 55444:5432 -e POSTGRES_PASSWORD=postgres \
  postgres:18 -c wal_level=logical -c fsync=off -c synchronous_commit=off
docker run -d --name wamn-ccdc-nats -p 44222:4222 nats:2 -js
./target/debug/wamn-gates cdcbench \
  --admin-database-url postgres://postgres:postgres@127.0.0.1:55444/postgres \
  --nats-url nats://127.0.0.1:44222 --mode all
# switchover bring-up: run --mode switchover --secs 45 and `docker restart
# wamn-ccdc-pg` inside the drill window.
docker rm -f wamn-ccdc-pg wamn-ccdc-nats
# In-cluster CAMPAIGN OF RECORD (release gates image, sequential with other
# jobs — the z7b.7 noise defense; CSVs from the job log → docs/results/ceilings-data/):
kubectl -n wamn-system apply -f deploy/gates/cdcbench-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/cdcbench --timeout=2400s
# Axis 4 vs the LIVE wamn-pg pool (single-instance today → timed primary
# recreate; trigger INSIDE the drill window, watch the log for the banner):
kubectl -n wamn-system apply -f deploy/gates/cdcbench-switchover-job.yaml
kubectl -n wamn-system logs -f job/cdcbench-switchover   # wait for DRILL WINDOW OPEN
kubectl -n wamn-system delete pod wamn-pg-1              # the trigger
```

Mutation harness: scratchpad `mutate_l5i9_14.py` — M1 neuters the reconcile
apply (the ri legs become identical; the named `narrow DELETE grows under
FULL` + `wide upd-slim pays the flattened old image` asserts FAIL), M2
off-by-ones the drain completeness target (the named `stream holds exactly N
row events` assert FAILS), M3 skips the lag final catch-up wait (the named
`eventual completeness` assert FAILS on a still-draining stream). Apply/test/
restore with sha256, DEBUG builds; rebuild wamn-gates after restoring a dep.

### [5.14] checkpoint/resume on replica loss

Docs: docs/archive/execution/run-queue.md

```bash
cargo test -p wamn-run-state   # incl the janitor completion-race guard (shape + live-apply)
cargo clippy -p wamn-run-state --all-targets && cargo fmt -p wamn-run-state --check
# Local iteration (reuse the throwaway PG above [wamn-rq-pg on 5459, wamn_app created by
# so NO wasm rebuild — reuse the built flowrunner.wasm):
WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:5459/wamn \
  ./target/release/wamn-gates --log-level error failoverbench \
  --flowrunner components/target/wasm32-wasip2/release/flowrunner.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5459/wamn --mode all
# In-cluster gate of record (co-located with postgres, NO cpu limit — S2 CFS lesson;
# HOST change => full docker rebuild (both --target stages + kind load BOTH images):
kubectl -n wamn-system apply -f deploy/gates/failoverbench-job.yaml
kubectl -n wamn-system logs -f job/failoverbench
```

### [5.14] guest-self-claim

Docs: docs/archive/execution/run-queue.md

```bash
cargo test -p wamn-run-state   # incl select_run_dispatch shape (fl3's traceparent seam)
cargo build -p wamn-run-state   # the guest-safe durable-state core builds alone
cargo clippy -p wamn-dispatcher -p wamn-executor -p wamn-gates -p wamn-run-state --all-targets \
  && cargo fmt -p wamn-dispatcher -p wamn-executor -p wamn-gates -p wamn-run-state --check
(cd components && cargo build --release --target wasm32-wasip2 -p flowrunner)   # guest CHANGED
cargo clippy --manifest-path components/execution/flowrunner/Cargo.toml --release --target wasm32-wasip2 \
  && cargo fmt --manifest-path components/execution/flowrunner/Cargo.toml --check
# Local iteration (throwaway postgres:18 + wamn_app; failoverbench --mode all now includes
# claim/park/heartbeat — the guest CHANGED so rebuild the wasm above first):
docker run -d --rm --name wamn-fqg4-pg -p 5459:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
docker exec wamn-fqg4-pg psql -U postgres -d wamn -c \
  "CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS;"
WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:5459/wamn \
  ./target/debug/wamn-gates --log-level error failoverbench \
  --flowrunner components/target/wasm32-wasip2/release/flowrunner.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5459/wamn --mode all
docker stop wamn-fqg4-pg
# In-cluster gate of record (failoverbench-job runs claim/park/heartbeat + the failover/
# stages + kind load BOTH images (+ flowbench/testhostbench regress on the new guest):
docker build --target host -t wamn-host:dev . && docker build --target gates -t wamn-gates:dev .
kind load docker-image wamn-host:dev --name wamn && kind load docker-image wamn-gates:dev --name wamn
kubectl -n wamn-system apply -f deploy/gates/failoverbench-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/failoverbench --timeout=240s
kubectl -n wamn-system logs job/failoverbench
```

### [5.14 / wamn-fqg.9] guest-side partitioned claim

Docs: docs/archive/execution/run-queue.md §Head-unavailability policy + §Per-partition ownership

The guest `run-next` export now also serves `partitioned(key)` runs: when the
global (unpartitioned) `claim_dispatch_sql` is empty it leases a partition
(`acquire_partitions_sql(1)`), claims the earliest HEAD across the partitions it
owns in stream order (`claim_partition_head_sql(1)` — one in flight per key, D20
policy on the row), drives it via the SHARED `execute_claimed` path (renewing the
partition lease per node alongside the run lease), and STEPS DOWN
(`release_partition_sql`) from a just-acquired partition that yields no head. The
WIT is unchanged (`run-next` signature identical) and `ExecutionHost.drain` loops it
unchanged. The partition SQL/pure builders already existed (host-gated by
queuebench); fqg.9 is their first GUEST caller — the same shape as fqg.4 for
`claim_batch_sql`. All partition builders live in `sql.rs`/`partition.rs` OUTSIDE
the `dispatcher` feature, so `default-features = false` already exposes them —
nothing moved.

```bash
cargo test -p wamn-run-state --test queue guest_partition_loop_drives_each_key_in_stream_order  # pure: the guest limit-1 loop drives each key in (enqueued_at, stream_seq, run_id) order
cargo clippy -p wamn-run-state -p wamn-gates --all-targets \
  && cargo fmt -p wamn-run-state -p wamn-gates --check
(cd components && cargo build --release --target wasm32-wasip2 -p flowrunner)   # guest CHANGED
cargo clippy --manifest-path components/execution/flowrunner/Cargo.toml --release --target wasm32-wasip2 \
  && cargo fmt --manifest-path components/execution/flowrunner/Cargo.toml --check
# Local live gates (throwaway postgres:18 + wamn_app; guest CHANGED so rebuild wasm first):
docker run -d --name wave3-pg-fqg9 -p 55434:5432 -e POSTGRES_PASSWORD=postgres postgres:18
docker exec wave3-pg-fqg9 psql -U postgres -c \
  "CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS;"
WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:55434/postgres \
  ./target/debug/wamn-gates --log-level error failoverbench \
  --flowrunner components/target/wasm32-wasip2/release/flowrunner.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:55434/postgres --mode partition-order
WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:55434/postgres \
  ./target/debug/wamn-gates --log-level error failoverbench \
  --flowrunner components/target/wasm32-wasip2/release/flowrunner.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:55434/postgres --mode partition-failover
docker rm -f wave3-pg-fqg9
```

`failoverbench --mode all` now also runs `partition-order` + `partition-failover`.
`partition-order`: one runner drains two interleaved keyed streams IN STREAM
ORDER per key — `kseq` (equal enqueued_at, distinct stream_seq) + `kenq` (equal
stream_seq, distinct enqueued_at), each seeded so stream order REVERSES run-id
order, so a head decision that dropped either tiebreak re-orders a key — while 5
unordered NULL-key rows drain via the old global claim (exactly once).
`partition-failover`: owner A drives a key's head then dies (its partition lease
force-expired — the queuebench lease-timestamp idiom); replica B reacquires the
key and resumes IN ORDER from the next head with no skipped/duplicated run.
Terminal-BUSINESS-failure wedging of a `blocking` partition head is NOT
fqg.9's scope (D20 wedging covers crash-exhaustion via `janitor_sweep_sql`, and
head-UNAVAILABILITY via `claim_partition_head_sql`; a partitioned head that
RUNS to a terminal `failed` dequeues like the unpartitioned path — filed as a
follow-up). Mutation harness: scratchpad `mutate_fqg9.py` — M1 pure (drop
stream_seq from `partition::stream_key`) fails the pure test; M2 SQL builder
(drop stream_seq from `claim_partition_head_sql`'s blocking arm) + M3 guest loop
(short-circuit `claim_partition_run`) fail `partition-order` live.

### [5.14] production runner (run-worker, fqg.8)

Docs: docs/archive/execution/run-queue.md · Manifests: deploy/platform/runner.yaml + deploy/platform/runner-db.example.yaml

```bash
cargo test -p wamn-executor   # owner fallback + drain tally + idle backoff
cargo clippy -p wamn-executor -p wamn-gates --all-targets \
  && cargo fmt -p wamn-executor -p wamn-gates --check
# Local runnerbench (throwaway postgres:18 + wamn_app; guest UNCHANGED — no wasm rebuild):
docker run -d --name wamn-fqg8-pg -p 5490:5432 -e POSTGRES_PASSWORD=postgres postgres:18
docker exec wamn-fqg8-pg psql -U postgres -c \
  "CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS;"
./target/debug/wamn-gates --log-level warn runnerbench \
  --flowrunner components/target/wasm32-wasip2/release/flowrunner.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5490/postgres \
  --admin-database-url postgres://postgres:postgres@127.0.0.1:5490/postgres
# 8 phases: drain + reuse + empty + RUNAWAY (cjv.4 anti-wedge, LOCAL gate of
# record: a never-terminating cyclic flow drives the engine's default 10k
# dispatch budget, ends failed/runaway-budget + DEQUEUES, and the run queued
# behind it still completes — under the phase's own 180s wall guard so a
# budget-removed mutant FAILS instead of hanging; ~1-2 min wall for the 10k
# dispatches) + STREAM + STREAM-RELOAD (fqg.18 record-stream amortization:
# --stream-records record-runs of one flow on one warm instance, per-record
# correctness [exactly-once, full node_runs trail, sink witness] + the
# ms/record measurement — combined claim/checkpoint/complete statements +
# guest plan cache took the local debug number from ~66 to ~32-37 ms/record —
# then a mid-stream version flip must take effect for the following records =
# the plan-cache invalidation guard) + PARTITION-ORDER (fqg.9, wamn-7hja:
# PARTITIONED(key) runs seeded via enqueue_with_policy_sql across 2 keys with
# INTERLEAVED insertion, drained through the production ExecutionHost::drain, assert
# per-key IN-ORDER dispatch + one-in-flight — the independent proof of the keyed
# claim path through the long-lived runner [failoverbench drives it via the
# gate-local Worker]. Dispatch order is read from a gate-local sink.dispatch_seq
# IDENTITY witness [execution order, not seed order]; the nhjg drift guard still
# pins the run_queue/partition_owner stand-in DDL against deploy/sql/run-queue.sql)
# + PARTITION-TERMINAL (wamn-v8cv, D20 dead-letter + continue: a blocking key's
# HEAD fails terminally under the runner's eyes [a postgres-query node dies
# Terminal("capability-denied") with the D8 flag off — deterministic, one step]
# -> the dequeue lands the run_dead_letters marker in the SAME txn
# [dead_letter_dequeue_sql] and the key CONTINUES — the runs behind it complete
# in order; the total-ledger-count assert doubles as the polarity proof that the
# phase-4 UNPARTITIONED runaway failure wrote no marker. The composed builder's
# conditionality matrix [blocking -> marker, leapfrog/unpartitioned -> none,
# redelivery idempotent, RLS isolation, key-advances] is the run-queue live
# suite: cargo test -p wamn-run-state + WAMN_RUN_QUEUE_PG_URL).
# Engine units:
# recipe-test: H5-RUNNER-BUDGET | integration | wamn-runner | test | runner | a_runaway_cycle_fails_at_exactly_the_budget | 1 | flow-engine runaway dispatch budget is exact and load-bearing
cargo test --locked -p wamn-runner --test runner a_runaway_cycle_fails_at_exactly_the_budget -- --exact
# (budget section) + cargo test -p wamn-run-state (fail_kind literal + DDL
# drift guard). Combined-builder shape + live-apply (PREPARE/EXECUTE the real
# claim_dispatch/record+renew/complete+dequeue against deploy DDL incl
# flows.sql): cargo test -p wamn-run-state (+ WAMN_RUN_QUEUE_PG_URL).
# Mutation harnesses: scratchpad mutate_cjv4.py (6 killed) + mutate_fqg18.py
# (5 killed — cache-never-invalidates, MATERIALIZED fence, renew tail,
# dequeue arm, mark-running arm); NOTE the engine AND the claim path are
# compiled into the GUEST, so those mutants need a flowrunner wasm rebuild
# to reach the live gate. mutate_lane_c.py M_PART inverts the runnerbench
# per-key ordering comparator (reverses the expected per-key dispatch vector);
# the real in-order dispatch then FAILS the `partition-order` assert (a
# host-only mutation — rebuild wamn-gates, no wasm rebuild; the production claim
# comparator lives in the guest, covered by the fqg.9/fqg.10 partition-order
# mutants above). mutate_v8cv.py (3 killed, one per layer): the DL insert's
# policy predicate flipped blocking->leapfrog (killer: the run-queue LIVE
# suite), the guest settle terminal arm reverted to the plain dequeue (killer:
# runnerbench `partition-terminal`; wasm rebuild to reach the gate), and
# dead_letters_on_terminal dropping the policy check (killer: its unit test).
docker rm -f wamn-fqg8-pg
# In-cluster live smoke = gate of record (HOST changed — the run-worker module +
# flowrunner.wasm baked into the prod image — so FULL rebuild BOTH stages + kind load):
docker build --target host -t wamn-host:dev . && docker build --target gates -t wamn-gates:dev .
kind load docker-image wamn-host:dev --name wamn
# Provision a demo schema (wamn_runner_demo: run-state.sql + run-queue.sql rewritten,
# a flows table + a sink table) via kubectl exec psql, register a fast-cron flow, then:
kubectl -n wamn-system apply -f deploy/platform/dispatcher-projects.example.yaml   # (pointed at the demo)
kubectl -n wamn-system apply -f deploy/platform/dispatcher.yaml
kubectl -n wamn-system apply -f deploy/platform/runner-db.example.yaml
kubectl -n wamn-system apply -f deploy/platform/runner.yaml
kubectl -n wamn-system rollout status deploy/runner --timeout=120s
# Assert a dispatcher-fired cron run was CLAIMED by the runner and driven end-to-end:
#   SELECT status FROM wamn_runner_demo.runs WHERE run_id LIKE 'runner-demo:cron:%'  -> completed
#   + a wamn_runner_demo.sink row + wamn_runner_demo.node_runs rows.
```

### [POC-F3] scale-to-zero / parked-project wake (wamn-fqg.12)

Docs: docs/archive/execution/run-queue.md (Scale-to-zero wake) · Actuator: services/waker +
deploy/platform/waker.yaml · Manifest: deploy/gates/wakeproof-job.yaml

`wakeproof` parks the runner Deployment at 0 replicas, seeds an every-second
cron flow into the schema the LIVE dispatcher sweeps (wamn_runner_demo/
demo-tenant), and proves — purely from DB state + the k8s scale API — that the
LIVE dispatcher fires a cron run, the `wamn-waker` scales the runner `0 -> 1` on
the doorbell hint, and the woken runner drives a run to `completed`; then it
deletes the flow and restores the runner scale. The gate NEVER enqueues or
doorbells — the LIVE dispatcher's cron fire must (the acceptance criterion). A
distinct `dispatcher-fires` phase separates a projects-Secret wiring gap from a
wake failure.

```bash
cargo test -p wamn-waker   # decision units (parse/decide/scale-parse)
# Integration-proof boundary: the cron-flow drift guard lives with wakeproof;
# wamn-gates only routes the deployed command.
# recipe-test: H5-WAKEPROOF | integration | wamn-proof-integration | lib | - | wakeproof::tests:: | 1 | tests/integration/src/wakeproof.rs cron-flow fixture parse and validation
cargo test -p wamn-proof-integration --lib wakeproof::tests::
cargo clippy -p wamn-waker -p wamn-gates --all-targets
# The checked-in `queue-runner` mutation campaign above owns the waker decision
# mutant and its immutable green/red evidence.
# In-cluster gate of record (NEW image: wamn-waker; gates rebuilt for the subcommand):
docker build --target waker -t wamn-waker:dev . && docker build --target gates -t wamn-gates:dev .
kind load docker-image wamn-waker:dev --name wamn
kind load docker-image wamn-gates:dev --name wamn
# PRECONDITION 1 — the actuator:
kubectl -n wamn-system apply -f deploy/platform/waker.yaml
kubectl -n wamn-system rollout status deploy/waker --timeout=120s
# PRECONDITION 2 — the dispatcher MUST sweep the runner's project. The
# wamn-dispatch-projects Secret is per-environment (NOT manifest-managed), so add
# a runner-demo entry ALONGSIDE any existing entries, then restart the dispatcher.
# (Merge with the live value; do not drop other projects — e.g. f1.)
kubectl -n wamn-system create secret generic wamn-dispatch-projects \
  --from-literal=projects.json='{
    "f1": {"url":"postgres://wamn_app:wamn_app@postgres.wamn-system.svc.cluster.local:5432/wamn","tenant":"f1-tenant","schema":"poc_f1"},
    "runner-demo": {"url":"postgres://wamn_app:wamn_app@postgres.wamn-system.svc.cluster.local:5432/wamn","tenant":"demo-tenant","schema":"wamn_runner_demo"}
  }' --dry-run=client -o yaml | kubectl -n wamn-system apply -f -
kubectl -n wamn-system rollout restart deploy/dispatcher
kubectl -n wamn-system rollout status deploy/dispatcher --timeout=120s
# The runner + its wamn_runner_demo schema (run-state + run-queue + flows) must be
# live (the fqg.8 / EXEC-LADDER bring-up above provisions them). Then run the gate
# (Jobs are immutable — delete before re-apply):
kubectl -n wamn-system delete job wakeproof --ignore-not-found
kubectl -n wamn-system apply -f deploy/gates/wakeproof-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/wakeproof --timeout=300s
kubectl -n wamn-system logs job/wakeproof   # -> overall PASS: true
# Post-run: wakeproof restores the runner scale itself (teardown floors at 1).
# Confirm no residue + the runner is back up:
kubectl -n wamn-system get deploy/runner   # READY should return to its pre-gate replicas
```

### [CALLABLE-FLOWS-P2A / wamn-5wd1.49] cron attachment admission

The dispatcher reads only the authoritative active cron-attachment projection,
synthesizes the normative `scheduled-at`/`fired-at` input, and commits the
generation-0 deterministic run, available queue row, and durable anchor through
the centralized admission transaction.

```bash
cargo test --locked -p wamn-dispatcher -p wamn-scheduler
# recipe-test: H5-CALLABLE-CRON | integration | wamn-proof-integration | lib | - | callable_cron::tests:: | 1 | tests/integration/src/callable_cron.rs process-boundary catalog/attachment/admission proof
cargo test --locked -p wamn-proof-integration --lib callable_cron::tests::
docker run -d --rm --name wamn-cf-cron-pg -p 5458:5432 \
  -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=wamn postgres:18
WAMN_RUN_STORE_PG_URL=postgres://postgres:postgres@127.0.0.1:5458/wamn \
  cargo test --locked -p wamn-dispatcher callable_cron_attachment_live \
  -- --ignored --nocapture
docker stop wamn-cf-cron-pg
docker build --target gates -t wamn-gates:dev .
kind load docker-image wamn-gates:dev --name wamn
kubectl -n wamn-system delete job callable-flow-cron --ignore-not-found
kubectl -n wamn-system apply -f deploy/gates/callable-flow-cron-job.yaml
kubectl -n wamn-system wait --for=condition=complete \
  job/callable-flow-cron --timeout=180s
kubectl -n wamn-system logs job/callable-flow-cron
```

The multi-mode `dispatchbench` gate below independently publishes the same
immutable catalog/source/attachment/head/activation chain for every phase and
exercises this centralized admission path under race, rollback, retention,
reconnect, fairness, and wake faults.

### [CALLABLE-FLOWS-POC-F0 / wamn-5wd1.56] echo release and two-commit path

The canonical F0 graph and HTTP attachment are resolved through the immutable
artifact/release types. The proof refuses malformed input, bad idempotency keys,
stale artifacts, bypass publication, and a third commit; both named crash seams
recover to the same stored response and exactly two commits. The gate image is
tagged with the implementation commit; substitute that tag without editing the
tracked Job.

```bash
# recipe-test: H5-CALLABLE-F0 | system | wamn-proof-system | lib | - | callable_f0::tests:: | 5 | tests/system/src/callable_f0.rs F0 immutable release, HTTP attachment, refusals, and two-commit recovery
cargo test --locked -p wamn-proof-system --lib callable_f0::tests::
cargo test --locked -p wamn-flow --test flows f0_

docker build --target gates -t wamn-gates:cf-f0-<commit> .
kind load docker-image wamn-gates:cf-f0-<commit> --name wamn
kubectl -n wamn-system delete job callable-flow-f0 --ignore-not-found
sed "s/wamn-gates:cf-f0-ISSUE/wamn-gates:cf-f0-<commit>/" \
  deploy/gates/callable-flow-f0-job.yaml | kubectl -n wamn-system apply -f -
kubectl -n wamn-system wait --for=condition=complete \
  job/callable-flow-f0 --timeout=180s
kubectl -n wamn-system logs job/callable-flow-f0
```

### [CALLABLE-FLOWS-POC-F3 / wamn-5wd1.58] stale-hold escalation r6

The canonical F3 graph and minimal cron attachment prove the scheduled-time
cutoff, one-row notify-before-escalate loop, natural completion, and the
same-run/new-run recovery-key distinction. The gate image is tagged with the
implementation commit; substitute that tag without editing the tracked Job.

```bash
# recipe-test: H5-CALLABLE-F3 | system | wamn-proof-system | lib | - | callable_f3::tests:: | 5 | tests/system/src/callable_f3.rs F3 graph, attachment, recovery, and failure windows
cargo test --locked -p wamn-proof-system --lib callable_f3::tests::
cargo test --locked -p wamn-flow --test flows f3_
cargo test --locked -p wamn-proof-integration --lib pocsuiteproof::tests::

docker build --target gates -t wamn-gates:cf-f3-<commit> .
kind load docker-image wamn-gates:cf-f3-<commit> --name wamn
kubectl -n wamn-system delete job callable-flow-f3 --ignore-not-found
sed "s/wamn-gates:cf-f3-ISSUE/wamn-gates:cf-f3-<commit>/" \
  deploy/gates/callable-flow-f3-job.yaml | kubectl -n wamn-system apply -f -
kubectl -n wamn-system wait --for=condition=complete \
  job/callable-flow-f3 --timeout=300s
kubectl -n wamn-system logs job/callable-flow-f3
```

## H5-CALLABLE-F2 — immutable pure recommendation (`wamn-5wd1.61`)

The package proof pins F2's direct supplied-node graph, strict request
contract, verified component identity, internal caller policy, service-mode
runtime refusal, and replay/effect-uncertain controls. The exact-image Job also
hashes the baked component bytes used by the release proof.

```bash
# recipe-test: H5-CALLABLE-F2 | system | wamn-proof-system | lib | - | callable_f2::tests:: | 8 | tests/system/src/callable_f2.rs F2 direct pure component release, internal caller policy, deterministic replay, and mutation controls
CARGO_TARGET_DIR=/tmp/wamn-target-f2-61 \
  cargo test --locked -p wamn-proof-system --lib callable_f2::tests::

docker build --target gates -t wamn-gates:cf-f2-<commit> .
kind load docker-image wamn-gates:cf-f2-<commit> --name wamn
kubectl -n wamn-system delete job callable-flow-f2 --ignore-not-found
sed "s/wamn-gates:cf-f2-ISSUE/wamn-gates:cf-f2-<commit>/" \
  deploy/gates/callable-flow-f2-job.yaml | kubectl -n wamn-system apply -f -
kubectl -n wamn-system wait --for=condition=complete \
  job/callable-flow-f2 --timeout=300s
kubectl -n wamn-system logs job/callable-flow-f2
```

## H5-CALLABLE-F4 — event child/review/callback composition (`wamn-5wd1.62`)

The package proof pins F4's canonical event graph and live registration,
strictly-prior `(decided_at,id)` history, service-mode F2 invocation, unique
review read-back, occurrence-keyed callback, fully-scoped event admission, and
the existing child create/recover, atomic wake, and generation-seized
cancellation transitions.

```bash
# recipe-test: H5-CALLABLE-F4 | system | wamn-proof-system | lib | - | callable_f4::tests:: | 10 | tests/system/src/callable_f4.rs F4 graph, registration, prior history, review/callback recovery, event scope, and child transition mutants
CARGO_TARGET_DIR=/tmp/wamn-target-f4-62 \
  cargo test --locked -p wamn-proof-system --lib callable_f4::tests::
CARGO_TARGET_DIR=/tmp/wamn-target-f4-62 \
  cargo test --locked -p wamn-flow --test flows f4_

docker build --target gates -t wamn-gates:cf-f4-<commit> .
kind load docker-image wamn-gates:cf-f4-<commit> --name wamn
kubectl -n wamn-system delete job callable-flow-f4 --ignore-not-found
sed "s/wamn-gates:cf-f4-ISSUE/wamn-gates:cf-f4-<commit>/" \
  deploy/gates/callable-flow-f4-job.yaml | kubectl -n wamn-system apply -f -
kubectl -n wamn-system wait --for=condition=complete \
  job/callable-flow-f4 --timeout=300s
kubectl -n wamn-system logs job/callable-flow-f4
```

### [5.14] shared trigger dispatcher

Docs: docs/archive/execution/run-queue.md

```bash
cargo test -p wamn-run-state -p wamn-scheduler   # durable anchors + pure cron/cadence decisions
cargo clippy -p wamn-run-state -p wamn-scheduler --all-targets \
  && cargo fmt -p wamn-run-state -p wamn-scheduler --check
cargo test --locked -p wamn-proof-integration --lib dispatchbench::tests::
cargo test --locked -p wamn-dispatcher -p wamn-scheduler
cargo build -p wamn-dispatcher -p wamn-gates   # gate spawns the sibling service binary
# optional live-apply gate (two disposable project databases, each with canonical
# catalog + run-state.sql + run-queue.sql; cron admission + last-tick recovery +
# wake scan):
docker run -d --rm --name wamn-rq-pg -p 5459:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
# (postgres:18 inits-then-restarts — pg_isready lies during socket-only init; if the
# first connection is refused, wait a few seconds and retry)
until docker exec wamn-rq-pg pg_isready -U postgres; do sleep 1; done
# BOTH roles: catalog-schema.sql / run-state.sql GRANT to the host-only NOLOGIN
# author role, so without it the gate's first DDL apply dies with
# `role "wamn_scenario_author" does not exist`.
docker exec wamn-rq-pg psql -U postgres -c \
  "CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS;" -c \
  "CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
     NOINHERIT NOREPLICATION NOBYPASSRLS;"
WAMN_RUN_QUEUE_PG_URL=postgres://postgres:postgres@127.0.0.1:5459/wamn cargo test -p wamn-run-state
# the live-apply gate] + a throwaway NATS for the wake/live doorbell hints):
docker run -d --rm --name wamn-rq-nats -p 4232:4222 nats:2.12.8-alpine
WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:5459/wamn \
  ./target/debug/wamn-gates --log-level error dispatchbench \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5459/wamn \
  --nats-url nats://127.0.0.1:4232 --mode all
docker stop wamn-rq-pg wamn-rq-nats
# dispatchbench modes: cron/ordering/race/fairness/wake/live/all (the outbox +
# prune modes retired with the outbox path at l5i9.19 — row events are
# matbench/streambench/readerbench territory).
# wake (and thus --mode all) now HARD-REQUIRES NATS: a missing/unreachable
# --nats-url is a loud bail, never a soft skip that greens the Job (C7-2).
# Each phase provisions canonical catalog artifacts, schedule sources, cron
# attachments, an applied head, and activation. Source/attachment/head drift
# therefore fails the phase before any run can be admitted; there is no local
# flow-registry fallback or direct producer run insert.
# The production service is `wamn-dispatcher --projects-file <json>`.
# In-cluster gate of record (co-located with postgres):
# HOST change => full docker rebuild (both --target stages + kind load BOTH images):
kubectl -n wamn-system apply -f deploy/gates/dispatchbench-job.yaml
kubectl -n wamn-system logs -f job/dispatchbench
```

### [9.6] node-level I/O capture (wamn-srb)

Docs: docs/archive/execution/run-state.md § *Node-level I/O capture (9.6)*

```bash
# Pure decision + SQL builders (scrub / truncate / preview derivation, the
# per-flow Flow.capture parse, the prune builder, the model + arity guards):
cargo test -p wamn-flow -p wamn-run-state
cargo clippy -p wamn-flow -p wamn-run-state -p wamn-ctl -p wamn-gates --all-targets
# If Flow.capture changed, regenerate the published schema (drift-guarded):
cargo run -p wamn-flow --example print-flow-schema > docs/archive/contracts/flow-schema.schema.json
# Rebuild the flowrunner guest (9.6 enforcement site; release-wasm exception):
( cd components && cargo build --release --target wasm32-wasip2 -p flowrunner )

# capturebench (host-side, applies the REAL run-state.sql to a throwaway schema
# and drives the same pure capture + node_runs insert builders the guest binds):
docker run -d --rm --name wamn-cap-pg -p 5461:5432 -e POSTGRES_PASSWORD=postgres \
  -v "$PWD/deploy/sql/postgres-init.sql:/docker-entrypoint-initdb.d/init.sql:ro" postgres:18
WAMN_PG_URL=postgres://wamn_app:wamn_app@127.0.0.1:5461/wamn \
WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:5461/wamn \
  ./target/debug/wamn-gates --log-level error capturebench --mode all
docker stop wamn-cap-pg
# capturebench modes: toggle (NULL payloads + CaptureOff) / truncate (oversized ->
# preview head/size/hash) / scrub (secret NOWHERE in node_runs, redacted set) /
# retention (the real prune-run-history verb) / all.
# Retention verb (deployed per project-env; app-role, tenant-scoped DELETE):
#   wamn-ctl prune-run-history --schema <run-schema> --tenant <t> --retention-days 30 [--dry-run]
kubectl -n wamn-system apply -f deploy/gates/capturebench-job.yaml
kubectl -n wamn-system logs -f job/capturebench
```

### [D6/wamn-q3n.1] control-plane registry model crate

Docs: docs/archive/platform/postgres-topology.md, docs/archive/platform/registry-model.md

```bash
cargo test -p wamn-control-registry
cargo clippy -p wamn-control-registry --all-targets && cargo fmt -p wamn-control-registry --check
```

### [D6/wamn-q3n.2] T1 system cluster

Docs: docs/archive/platform/system-cluster.md

```bash
kubectl apply -f deploy/platform/wamn-sysdb.yaml
kubectl -n wamn-system wait --for=jsonpath='{.status.readyInstances}'=3 \
  cluster/wamn-sysdb --timeout=300s
# Verify (gate of record — HA + distinct plane + bootstrap + no cpu limit):
kubectl -n wamn-system get cluster wamn-sysdb -o wide   # 3/3 healthy, primary wamn-sysdb-1
kubectl -n wamn-system get svc,secret,pvc -l cnpg.io/cluster=wamn-sysdb  # own -rw/-ro/-r + wamn-sysdb-* + 3 PVCs
kubectl -n wamn-system exec wamn-sysdb-1 -c postgres -- \
  psql -U postgres -tAc "SELECT datname, pg_get_userbyid(datdba) FROM pg_database WHERE datname='wamn_system';"
kubectl -n wamn-system exec wamn-sysdb-1 -c postgres -- \
  psql -U postgres -tAc "SELECT application_name, state, sync_state FROM pg_stat_replication;"  # 2 streaming replicas
```

### [D6/wamn-q3n.3] system-DB registry schema + the four invariants

Docs: docs/archive/platform/registry-model.md, docs/archive/platform/system-cluster.md

```bash
cargo test -p wamn-control-registry   # drift-guard (placement cols + env_policies seed vs the model) + inv-1 grep (live-apply skips)
cargo clippy -p wamn-control-registry --all-targets && cargo fmt -p wamn-control-registry --check
# cjv.20: the charset/length CHECK backstop on the stored slug/name columns
# (orgs.id/pool_cluster, projects.id, env_policies.name — mirrors validate()
# check_id/check_env/check_name) is pinned by the drift-guard
# `charset_length_checks_backstop_the_stored_slug_names`, proven live by the gate
# below, and mutation-tested (scratchpad/mutate_cjv20.py: 3 mutants — drop the
# orgs.id CHECK / `~`->`~*` case-insensitive / neuter validate_org_id). Pure-crate
# + hand-written SQL — NO in-cluster required (a45 precedent; the live wamn-sysdb
# picks the CHECK up on the next system-schema re-apply — see wamn-cjv.29).
# optional throwaway-PG live-apply gate (WAMN_REGISTRY_PG_URL, superuser url —
# invariants 2/3 + the placement biconditional + the composite (org, env) FK ->
# env_policies(org, name) + the template stamp insert-if-absent + FK integrity +
# the cjv.20 charset CHECKs + saga exactly-once; skips when unset):
docker run -d --rm --name wamn-reg-pg -p 5461:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_REGISTRY_PG_URL=postgres://postgres:postgres@127.0.0.1:5461/wamn cargo test -p wamn-control-registry
docker stop wamn-reg-pg
# IN-CLUSTER gate of record — apply system-schema.sql INTO wamn-sysdb's (wamn-q3n.2)
# wamn_system DB (empty of rows — a DROP+re-apply is safe pre-production only):
{ echo "DROP SCHEMA IF EXISTS registry, provisioning CASCADE; SET ROLE wamn_system;"; \
  cat deploy/sql/system-schema.sql; } | kubectl -n wamn-system exec -i wamn-sysdb-1 \
  -c postgres -- psql -U postgres -d wamn_system -v ON_ERROR_STOP=1 -f -
kubectl -n wamn-system exec wamn-sysdb-1 -c postgres -- psql -U postgres -d wamn_system \
  -tAc "SELECT schemaname||'.'||tablename FROM pg_tables \
        WHERE schemaname IN ('registry','provisioning') ORDER BY 1;"  # 7 control-plane tables (incl env_policies + dumps)
kubectl -n wamn-system exec wamn-sysdb-1 -c postgres -- psql -U postgres -d wamn_system \
  -tAc "SELECT count(*) FROM registry.env_policies;"  # 0 — NO platform seed (8df.4): policies are stamped per org by provision-org --template
```

### [D6/wamn-q3n.6] provision-org

Docs: docs/archive/platform/provisioning.md, docs/archive/platform/postgres-topology.md

```bash
cargo test -p wamn-control-registry -p wamn-control-provision -p wamn-ctl   # renderer shape + org-row SQL + drift/subcommand units
cargo clippy -p wamn-control-registry -p wamn-control-provision -p wamn-ctl --all-targets \
  && cargo fmt -p wamn-control-registry -p wamn-control-provision -p wamn-ctl --check
# CONFLICT mutant). Render CRs locally (no cluster/DB needed — template policies):
./target/debug/wamn-ctl provision-org --org demo --template standard \
  --emit-clusters /tmp/demo-clusters.json --emit-object-store /tmp/demo-os.json \
  --emit-scheduled-backup /tmp/demo-sb.json
# IN-CLUSTER live standup = the gate of record (the wamn-q3n.2 infra precedent;
# port-forwarded wamn-sysdb — reads registry.env_policies for sizing + writes the
# org's placement row — then kubectl-apply the emitted CRs ADDITIVELY (ObjectStore
# BEFORE the clusters, ScheduledBackup after — the wamn-e1g order):
kubectl -n wamn-system port-forward svc/wamn-sysdb-rw 5463:5432 &
SYSPW=$(kubectl -n wamn-system get secret wamn-sysdb-superuser -o jsonpath='{.data.password}' | base64 -d)
WAMN_SYSTEM_ADMIN_URL="postgres://postgres:${SYSPW}@127.0.0.1:5463/wamn_system?sslmode=disable" \
  ./target/debug/wamn-ctl provision-org --org demo --template standard \
  --emit-clusters /tmp/demo-clusters.json --emit-object-store /tmp/demo-os.json \
  --emit-scheduled-backup /tmp/demo-sb.json   # renders per-recovery-domain + writes registry.orgs
kubectl apply -f /tmp/demo-os.json -f /tmp/demo-clusters.json
kubectl -n wamn-system wait --for=jsonpath='{.status.readyInstances}'=3 cluster/demo-prod --timeout=300s
kubectl -n wamn-system wait --for=jsonpath='{.status.readyInstances}'=1 cluster/demo-dev  --timeout=300s
kubectl apply -f /tmp/demo-sb.json
# deletes ONLY the new clusters + backup CRs + the org row:
kubectl -n wamn-system delete scheduledbackup demo-prod-backup
kubectl -n wamn-system delete cluster demo-prod demo-dev
kubectl -n wamn-system delete objectstore demo-prod-store
kubectl -n wamn-system exec wamn-sysdb-1 -c postgres -- \
  psql -U postgres -d wamn_system -c "DELETE FROM registry.orgs WHERE id='demo';"
```

### [D6/wamn-q3n.7] provision-project-env

Docs: docs/archive/platform/provisioning.md, docs/archive/platform/postgres-topology.md

```bash
cargo test -p wamn-control-provision -p wamn-control-registry -p wamn-ctl   # renderer/naming + project SQL + drift/subcommand units
cargo clippy -p wamn-control-provision -p wamn-control-registry -p wamn-ctl --all-targets \
  && cargo fmt -p wamn-control-provision -p wamn-control-registry -p wamn-ctl --check
# (--cluster given => no DB needed):
./target/debug/wamn-ctl provision-project-env --org demo --project demo --env dev \
  --cluster wamn-pg --emit-database - --emit-role-sql - --emit-privilege-sql - --emit-secret -
# IN-CLUSTER live standup = the gate of record (T3 pool wamn-pg is ALWAYS up; the
# SQL -> Database CR -> privilege SQL in order:
kubectl -n wamn-system exec -i wamn-sysdb-1 -c postgres -- psql -U postgres -d wamn_system \
  -c "SET ROLE wamn_system; INSERT INTO registry.orgs (id,placement_kind,pool_cluster) \
      VALUES ('demo','pooled','wamn-pg') ON CONFLICT (id) DO NOTHING;"
kubectl -n wamn-system port-forward svc/wamn-sysdb-rw 5470:5432 &
SYSPW=$(kubectl -n wamn-system get secret wamn-sysdb-superuser -o jsonpath='{.data.password}' | base64 -d)
WAMN_SYSTEM_ADMIN_URL="postgres://postgres:${SYSPW}@127.0.0.1:5470/wamn_system?sslmode=disable" \
  ./target/debug/wamn-ctl provision-project-env --org demo --project demo --env dev \
  --connection-limit 20 --emit-database /tmp/db.json --emit-role-sql /tmp/role.sql \
  --emit-privilege-sql /tmp/priv.sql --emit-secret /tmp/secret.json   # reads placement + writes rows
kubectl -n wamn-system exec -i wamn-pg-1 -c postgres -- psql -U postgres -f - < /tmp/role.sql
kubectl apply -f /tmp/db.json
kubectl -n wamn-system wait --for=jsonpath='{.status.applied}'=true database/wamn-db-demo--demo--dev --timeout=90s
kubectl -n wamn-system exec -i wamn-pg-1 -c postgres -- psql -U postgres -f - < /tmp/priv.sql
# new Database CR + rows, then DROPs the created db (retain leaves it):
kubectl -n wamn-system delete database wamn-db-demo--demo--dev
kubectl -n wamn-system exec wamn-pg-1 -c postgres -- \
  psql -U postgres -c 'DROP DATABASE IF EXISTS "wamn-db-demo--demo--dev" WITH (FORCE);'
kubectl -n wamn-system exec wamn-sysdb-1 -c postgres -- \
  psql -U postgres -d wamn_system -c "DELETE FROM registry.orgs WHERE id='demo';"
```

### [D6/wamn-q3n.8] provisionbench four-tier extension

Docs: docs/archive/platform/provisioning.md, docs/archive/platform/postgres-topology.md

```bash
cargo test -p wamn-control-registry -p wamn-control-provision   # saga/named-db builders + drift-guards
cargo clippy -p wamn-control-registry -p wamn-control-provision -p wamn-gates --all-targets \
  && cargo fmt -p wamn-control-registry -p wamn-control-provision -p wamn-gates --check
# Local iteration (throwaway postgres:18; superuser url provisions wamn_app +
# wamn_system + the per-project-env DBs + the ephemeral registry schema):
docker run -d --rm --name wamn-prov-pg -p 5460:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:5460/wamn \
  ./target/debug/wamn-gates --log-level error provisionbench --mode all
# The saga live proof rides the provisioning-owned control-storage gate:
WAMN_REGISTRY_PG_URL=postgres://postgres:postgres@127.0.0.1:5460/wamn \
  cargo test -p wamn-control-provision --test control_storage
docker stop wamn-prov-pg
# IN-CLUSTER gate of record = a LIVE DEDICATED-ORG STANDUP (the .6/.7 precedent; the
# registry read/write (the registry-write path is the .6/.7 gate of record):
./target/debug/wamn-ctl provision-org --org gate8 --template standard \
  --emit-clusters /tmp/gate8-clusters.json --emit-object-store /tmp/gate8-os.json \
  --emit-scheduled-backup /tmp/gate8-sb.json
kubectl apply -f /tmp/gate8-os.json -f /tmp/gate8-clusters.json   # ObjectStore first (prod is backed)
kubectl -n wamn-system wait --for=jsonpath='{.status.readyInstances}'=3 cluster/gate8-prod --timeout=300s
kubectl -n wamn-system wait --for=jsonpath='{.status.readyInstances}'=1 cluster/gate8-dev  --timeout=180s
for E in prod dev; do C=gate8-$E; \
  ./target/debug/wamn-ctl provision-project-env --org gate8 --project app --env $E \
    --cluster $C --emit-database /tmp/db-$E.json --emit-role-sql /tmp/role-$E.sql \
    --emit-privilege-sql /tmp/priv-$E.sql --emit-secret /tmp/sec-$E.json; \
  kubectl -n wamn-system exec -i $C-1 -c postgres -- psql -U postgres -f - < /tmp/role-$E.sql; \
  kubectl apply -f /tmp/db-$E.json; \
  kubectl -n wamn-system wait --for=jsonpath='{.status.applied}'=true database/wamn-db-gate8--app--$E --timeout=90s; \
  kubectl -n wamn-system exec -i $C-1 -c postgres -- psql -U postgres -f - < /tmp/priv-$E.sql; done
# wamn-pg/wamn-sysdb/postgres.yaml UNTOUCHED. Teardown deletes ONLY the new resources:
kubectl -n wamn-system delete database wamn-db-gate8--app--prod wamn-db-gate8--app--dev
kubectl -n wamn-system delete cluster gate8-prod gate8-dev
kubectl -n wamn-system delete objectstore gate8-prod-store --ignore-not-found
```

### [D6/wamn-q3n.9] demote the shipped shared cluster to the T3 trials pool

Docs: docs/archive/platform/postgres-topology.md, docs/archive/platform/provisioning.md

```bash
cargo test -p wamn-control-registry -p wamn-ctl   # Org::pooled placement + pooled-vs-dedicated subcommand units
cargo clippy -p wamn-control-registry -p wamn-ctl --all-targets \
  && cargo fmt -p wamn-control-registry -p wamn-ctl --check
# Plan a pooled org locally (no DB needed — omit --system-database-url):
./target/debug/wamn-ctl provision-org --org trialco --template trials --pool wamn-pg
# IN-CLUSTER gate of record = a LIVE T3 trials-org standup (the .6/.7 precedent; T3
# port-forward (check `ss -ltn | grep 547` first):
kubectl -n wamn-system port-forward svc/wamn-sysdb-rw 5473:5432 &
SYSPW=$(kubectl -n wamn-system get secret wamn-sysdb-superuser -o jsonpath='{.data.password}' | base64 -d)
WAMN_SYSTEM_ADMIN_URL="postgres://postgres:${SYSPW}@127.0.0.1:5473/wamn_system?sslmode=disable" \
  ./target/debug/wamn-ctl provision-org --org t3gate --template trials --pool wamn-pg   # records registry.orgs (pooled|wamn-pg), NO CRs
# provision-project-env WITHOUT --cluster reads placement from the registered row -> wamn-pg:
WAMN_SYSTEM_ADMIN_URL="postgres://postgres:${SYSPW}@127.0.0.1:5473/wamn_system?sslmode=disable" \
  ./target/debug/wamn-ctl provision-project-env --org t3gate --project demo --env dev \
  --connection-limit 15 --emit-database /tmp/t3-db.json --emit-role-sql /tmp/t3-role.sql \
  --emit-privilege-sql /tmp/t3-priv.sql --emit-secret /tmp/t3-secret.json   # Database CR cluster == wamn-pg
kubectl -n wamn-system exec -i wamn-pg-1 -c postgres -- psql -U postgres -f - < /tmp/t3-role.sql
kubectl apply -f /tmp/t3-db.json
kubectl -n wamn-system wait --for=jsonpath='{.status.applied}'=true database/wamn-db-t3gate--demo--dev --timeout=90s
kubectl -n wamn-system exec -i wamn-pg-1 -c postgres -- psql -U postgres -f - < /tmp/t3-priv.sql
# org's Database CR + DB + registry.orgs row (cascades projects + project_envs):
kubectl -n wamn-system delete database wamn-db-t3gate--demo--dev
kubectl -n wamn-system exec wamn-pg-1 -c postgres -- \
  psql -U postgres -c 'DROP DATABASE IF EXISTS "wamn-db-t3gate--demo--dev" WITH (FORCE);'
kubectl -n wamn-system exec wamn-sysdb-1 -c postgres -- \
  psql -U postgres -d wamn_system -c "DELETE FROM registry.orgs WHERE id='t3gate';"
```

### [D6/wamn-q3n.10] scheduled per-project-env logical dumps

Docs: docs/archive/platform/postgres-topology.md, docs/archive/platform/provisioning.md

```bash
cargo test -p wamn-control-provision -p wamn-control-registry -p wamn-ctl   # renderers/builders + record_dump SQL + drift/subcommand units
cargo clippy -p wamn-control-provision -p wamn-control-registry -p wamn-ctl --all-targets \
  && cargo fmt -p wamn-control-provision -p wamn-control-registry -p wamn-ctl --check
# Render locally (no DB — the cadence is --schedule, default daily 03:00):
./target/debug/wamn-ctl dump-project-env --org demo --project app --env prod \
  --emit-cronjob - --emit-job -
# optional live gates (throwaway postgres:18; superuser url): (a) the ARTIFACT
# idempotent + byte_size-refresh proof rides the wamn-q3n.3 storage gate:
docker run -d --rm --name wamn-dump-pg -p 5462:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_DUMP_PG_URL=postgres://postgres:postgres@127.0.0.1:5462/wamn \
  cargo test -p wamn-control-provision --test dump
WAMN_REGISTRY_PG_URL=postgres://postgres:postgres@127.0.0.1:5462/wamn cargo test -p wamn-control-registry
docker stop wamn-dump-pg
# IN-CLUSTER gate of record (the .6/.7/.9 precedent; T3 pool wamn-pg + T1 wamn-sysdb
# (writing the T1 registry's OWN DB IS .10's job; NEVER touch wamn-pg/postgres.yaml):
awk '/^CREATE TABLE provisioning\.dumps/{f=1} f{print} f&&/^\);/{exit}' deploy/sql/system-schema.sql \
  | { echo "SET ROLE wamn_system;"; cat; } | kubectl -n wamn-system exec -i wamn-sysdb-1 \
  -c postgres -- psql -U postgres -d wamn_system -v ON_ERROR_STOP=1 -f -
# it, then dump+restore. PICK CLEAN unused ports (check `ss -ltn | grep 547`):
kubectl -n wamn-system port-forward svc/wamn-sysdb-rw 5474:5432 &
kubectl -n wamn-system port-forward svc/wamn-pg-rw 5475:5432 &
SYSPW=$(kubectl -n wamn-system get secret wamn-sysdb-superuser -o jsonpath='{.data.password}' | base64 -d)
PGPW=$(kubectl -n wamn-system get secret wamn-pg-superuser -o jsonpath='{.data.password}' | base64 -d)
SYS="postgres://postgres:${SYSPW}@127.0.0.1:5474/wamn_system?sslmode=disable"
WAMN_SYSTEM_ADMIN_URL="$SYS" ./target/debug/wamn-ctl provision-org --org t10gate --template trials --pool wamn-pg
WAMN_SYSTEM_ADMIN_URL="$SYS" ./target/debug/wamn-ctl provision-project-env \
  --org t10gate --project demo --env dev --connection-limit 10 \
  --emit-database /tmp/t10-db.json --emit-role-sql /tmp/t10-role.sql \
  --emit-privilege-sql /tmp/t10-priv.sql --emit-secret /tmp/t10-secret.json
kubectl -n wamn-system exec -i wamn-pg-1 -c postgres -- psql -U postgres -f - < /tmp/t10-role.sql
kubectl apply -f /tmp/t10-db.json
kubectl -n wamn-system wait --for=jsonpath='{.status.applied}'=true database/wamn-db-t10gate--demo--dev --timeout=90s
kubectl -n wamn-system exec -i wamn-pg-1 -c postgres -- psql -U postgres -f - < /tmp/t10-priv.sql
kubectl -n wamn-system exec -i wamn-pg-1 -c postgres -- psql -U postgres -d "wamn-db-t10gate--demo--dev" \
  -c "CREATE TABLE parts (id int primary key, sku text, weight_kg numeric(8,3)); INSERT INTO parts VALUES (1,'bolt',0.125),(2,'nut',0.050),(3,'washer',0.008);"
# Dump the REAL project-env DB (records the dump in the wamn-sysdb catalog), then restore:
WAMN_SYSTEM_ADMIN_URL="$SYS" ./target/debug/wamn-ctl dump-project-env --org t10gate --project demo --env dev \
  --database-url "postgres://postgres:${PGPW}@127.0.0.1:5475/wamn-db-t10gate--demo--dev?sslmode=disable" \
  --run-now --out-dir /tmp/t10-dump
kubectl -n wamn-system exec wamn-pg-1 -c postgres -- psql -U postgres -c 'CREATE DATABASE wamn_dump_scratch_t10;'
pg_restore --no-owner --no-privileges \
  -d "postgres://postgres:${PGPW}@127.0.0.1:5475/wamn_dump_scratch_t10?sslmode=disable" /tmp/t10-dump/*/
# weights intact) + the provisioning.dumps row in wamn-sysdb (fmt=directory, byte_size):
kubectl -n wamn-system exec wamn-pg-1 -c postgres -- psql -U postgres -d wamn_dump_scratch_t10 \
  -tAc "SELECT count(*), sum(weight_kg) FROM parts;"
kubectl -n wamn-system exec wamn-sysdb-1 -c postgres -- psql -U postgres -d wamn_system \
  -tAc "SELECT object_key, format, byte_size FROM provisioning.dumps WHERE org='t10gate';"
# projects+project_envs+dumps:
kubectl -n wamn-system delete database wamn-db-t10gate--demo--dev
kubectl -n wamn-system exec wamn-pg-1 -c postgres -- psql -U postgres \
  -c 'DROP DATABASE IF EXISTS "wamn-db-t10gate--demo--dev" WITH (FORCE);' \
  -c 'DROP DATABASE IF EXISTS wamn_dump_scratch_t10 WITH (FORCE);'
kubectl -n wamn-system exec wamn-sysdb-1 -c postgres -- psql -U postgres -d wamn_system \
  -c "DELETE FROM registry.orgs WHERE id='t10gate';"
```

### [D6/wamn-q3n.11] restore per-project-env logical dumps

Docs: docs/archive/platform/postgres-topology.md, docs/archive/platform/provisioning.md

```bash
cargo test -p wamn-control-provision -p wamn-control-registry -p wamn-ctl   # restore builders + select_latest shape/drift + subcommand units
cargo clippy -p wamn-control-provision -p wamn-control-registry -p wamn-ctl --all-targets \
  && cargo fmt -p wamn-control-provision -p wamn-control-registry -p wamn-ctl --check
# Render/plan locally (no cluster/DB needed — explicit --dump-dir, render only):
./target/debug/wamn-ctl restore-project-env --org demo --project app --env dev \
  --database-url postgres://postgres:postgres@127.0.0.1:5468/postgres \
  --dump-dir /tmp/some-dump --help >/dev/null   # (see the subcommand flags)
# optional live gates (throwaway postgres:18; superuser url): (a) the restore
# wamn-q3n.3 storage gate:
docker run -d --rm --name wamn-restore-pg -p 5468:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_RESTORE_PG_URL=postgres://postgres:postgres@127.0.0.1:5468/wamn \
  cargo test -p wamn-control-provision --test restore
WAMN_REGISTRY_PG_URL=postgres://postgres:postgres@127.0.0.1:5468/wamn cargo test -p wamn-control-registry
docker stop wamn-restore-pg
# IN-CLUSTER gate of record = a LIVE restore standup on the T3 pool (the .6/.7/.9/.10
# CLEAN unused ports (check `ss -ltn | grep 547`):
kubectl -n wamn-system port-forward svc/wamn-sysdb-rw 5476:5432 &
kubectl -n wamn-system port-forward svc/wamn-pg-rw 5477:5432 &
SYSPW=$(kubectl -n wamn-system get secret wamn-sysdb-superuser -o jsonpath='{.data.password}' | base64 -d)
PGPW=$(kubectl -n wamn-system get secret wamn-pg-superuser -o jsonpath='{.data.password}' | base64 -d)
SYS="postgres://postgres:${SYSPW}@127.0.0.1:5476/wamn_system?sslmode=disable"
PGADMIN="postgres://postgres:${PGPW}@127.0.0.1:5477/postgres?sslmode=disable"
DB="wamn-db-t11gate--demo--dev"; DUMPROOT=$(mktemp -d)
# Register a pooled org + provision a project-env DB on wamn-pg (the .7/.9 path), seed:
WAMN_SYSTEM_ADMIN_URL="$SYS" ./target/debug/wamn-ctl provision-org --org t11gate --template trials --pool wamn-pg
WAMN_SYSTEM_ADMIN_URL="$SYS" ./target/debug/wamn-ctl provision-project-env \
  --org t11gate --project demo --env dev --connection-limit 10 \
  --emit-database /tmp/t11-db.json --emit-role-sql /tmp/t11-role.sql \
  --emit-privilege-sql /tmp/t11-priv.sql --emit-secret /tmp/t11-secret.json
psql "$PGADMIN" -q -f /tmp/t11-role.sql
kubectl apply -f /tmp/t11-db.json
kubectl -n wamn-system wait --for=jsonpath='{.status.applied}'=true database/$DB --timeout=90s
psql "$PGADMIN" -q -f /tmp/t11-priv.sql
psql "postgres://postgres:${PGPW}@127.0.0.1:5477/${DB}?sslmode=disable" \
  -c "CREATE TABLE parts (id int primary key, sku text, weight_kg numeric(8,3)); INSERT INTO parts VALUES (1,'bolt',0.125),(2,'nut',0.050),(3,'washer',0.008);"
# Dump it (records the REAL wamn-sysdb catalog), then RESTORE-to-last-dump into scratch:
WAMN_SYSTEM_ADMIN_URL="$SYS" ./target/debug/wamn-ctl dump-project-env --org t11gate --project demo --env dev \
  --database-url "postgres://postgres:${PGPW}@127.0.0.1:5477/${DB}?sslmode=disable" --run-now --out-dir "$DUMPROOT"
WAMN_SYSTEM_ADMIN_URL="$SYS" ./target/debug/wamn-ctl restore-project-env --org t11gate --project demo --env dev \
  --database-url "$PGADMIN" --dump-root "$DUMPROOT"   # reads the catalog -> scratch DB
# row (mutate live -> restore -> stale gone):
psql "postgres://postgres:${PGPW}@127.0.0.1:5477/wamn-restore-t11gate--demo--dev?sslmode=disable" \
  -tAc "SELECT count(*), sum(weight_kg) FROM parts;"
psql "postgres://postgres:${PGPW}@127.0.0.1:5477/${DB}?sslmode=disable" -c "INSERT INTO parts VALUES (99,'STALE',9.999);"
WAMN_SYSTEM_ADMIN_URL="$SYS" ./target/debug/wamn-ctl restore-project-env --org t11gate --project demo --env dev \
  --database-url "$PGADMIN" --dump-root "$DUMPROOT" --in-place --confirm
psql "postgres://postgres:${PGPW}@127.0.0.1:5477/${DB}?sslmode=disable" -tAc "SELECT count(*) FROM parts;"  # 3 (stale gone)
# projects+project_envs+dumps:
kubectl -n wamn-system delete database $DB
kubectl -n wamn-system exec wamn-pg-1 -c postgres -- psql -U postgres \
  -c 'DROP DATABASE IF EXISTS "wamn-db-t11gate--demo--dev" WITH (FORCE);' \
  -c 'DROP DATABASE IF EXISTS "wamn-restore-t11gate--demo--dev" WITH (FORCE);'
kubectl -n wamn-system exec wamn-sysdb-1 -c postgres -- psql -U postgres -d wamn_system \
  -c "DELETE FROM registry.orgs WHERE id='t11gate';"
```

### [D6/wamn-q3n.13] tier-move / promotion tooling — RETIRED (D18, wamn-8df.3)

Docs: docs/archive/platform/provisioning.md, docs/archive/platform/deployment-model.md

`move-org-tier` + `wamn_control_provision::tier_move` are removed with the `Tier` enum.
A placement change is one case of the unified `copy(src -> dst)` operation
(`wamn-8df.5`, with a mandatory quiesce+verify cutover gate); until it lands, a
cross-cluster move is the manual runbook: `dump-project-env` -> `provision-org`
(the new placement) -> `provision-project-env` -> `restore-project-env` ->
update the org's placement row.

### [D6/wamn-q3n.14] dedicated-per-env (T4) — now an env policy, not a tier (D18)

Docs: docs/archive/platform/postgres-topology.md, docs/archive/platform/deployment-model.md

The wamn-q3n.14 canary special case (`canary_cluster` column + two CHECKs +
`Org::cluster_for_env`) is retired (wamn-8df.3). The T4 shape is a `canary` env
policy with its **own** recovery domain; shared-with `prod` reproduces the old
T2 collapse instead. The dedicated standup itself is the `[D6/wamn-q3n.6]` gate.

```bash
# Since wamn-8df.4 the T4 shape is a TEMPLATE: `provision-org --org <org>
# --template dedicated` stamps canary(own) at provision time — three clusters
# (<org>-dev/-canary/-prod), each sized by the org's policy. To flip an EXISTING
# org's canary to its own recovery domain instead, edit THAT ORG's row (policies
# are org-scoped — no other org is affected):
kubectl -n wamn-system exec -i wamn-sysdb-1 -c postgres -- psql -U postgres -d wamn_system \
  -c "SET ROLE wamn_system; INSERT INTO registry.env_policies
      (org, name, recovery_domain, promotion_rank, instances,
       storage, cpu, memory, image, backup_cadence, wal_retention, hibernation)
      VALUES ('<org>', 'canary', '\"own\"'::jsonb, 20, 2, '2Gi', '200m', '256Mi',
              'ghcr.io/cloudnative-pg/postgresql:18', '0 0 */6 * * *', '14d', 'off')
      ON CONFLICT (org, name) DO UPDATE SET recovery_domain = '\"own\"'::jsonb;"
# Re-running provision-org (any template) re-renders from the org's own rows;
# provision-project-env --env canary derives <org>-canary via cluster_of.
# Remove the policy when done (the composite (org, env) FK blocks removal while in use):
kubectl -n wamn-system exec wamn-sysdb-1 -c postgres -- psql -U postgres -d wamn_system \
  -c "DELETE FROM registry.env_policies WHERE org='<org>' AND name='canary';"
```

### [ARCH/wamn-8df.4] templates + org-scoped env policies (the Tier successor)

Docs: docs/archive/platform/deployment-model.md, docs/archive/platform/registry-model.md, docs/archive/platform/provisioning.md

```bash
cargo test -p wamn-control-registry -p wamn-ctl   # Template presets + OrgEnvPolicy + org-scoped validate/resolve/SQL + subcommand units
cargo clippy -p wamn-control-registry -p wamn-ctl -p wamn-gates --all-targets \
  && cargo fmt -p wamn-control-registry -p wamn-ctl -p wamn-gates --check
# Throwaway-PG live gates (superuser url): the storage live-apply (composite
# (org, env) FK + stamp insert-if-absent + cross-org isolation + whole-org
# cascade) + provisionbench --mode all (tier scenarios stamp template policies):
docker run -d --rm --name wamn-8df4-pg -p 5494:5432 -e POSTGRES_PASSWORD=postgres postgres:18
WAMN_REGISTRY_PG_URL=postgres://postgres:postgres@127.0.0.1:5494/postgres cargo test -p wamn-control-registry
WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:5494/postgres \
  ./target/debug/wamn-gates --log-level error provisionbench --mode all
# Subcommand smoke (apply role + system-schema.sql into the throwaway DB as
# wamn_system first — the .3 recipe): standard + dedicated orgs COEXIST (T2/T4),
# canary derives per-org, a customized row survives a re-stamp:
export WAMN_SYSTEM_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:5494/postgres
./target/debug/wamn-ctl provision-org --org smoke1 --template standard  --emit-clusters /tmp/s1.json ...  # 2 clusters (canary -> prod)
./target/debug/wamn-ctl provision-org --org smoke2 --template dedicated --emit-clusters /tmp/s2.json ...  # 3 clusters (smoke2-canary)
./target/debug/wamn-ctl provision-project-env --org smoke1 --project app --env canary ...  # cluster smoke1-prod
./target/debug/wamn-ctl provision-project-env --org smoke2 --project app --env canary ...  # cluster smoke2-canary
docker stop wamn-8df4-pg
# 5 mutants killed (apply/test/restore, debug builds — scratchpad/mutate_8df4.py):
# M1 standard-canary->Own (template unit), M2 stamp DO NOTHING->DO UPDATE (unit +
# live customization-survives), M3 policy read drops org key (unit + live
# cross-org probe), M4 provision-org stamps nothing (scripted project-env
# refusal), M5 validate env check any-org (org-scoping unit).
# IN-CLUSTER gate of record: re-apply system-schema.sql into wamn-sysdb (the
# [D6/wamn-q3n.3] block — org-scoped env_policies, NO seed), rebuild + kind-load
# wamn-gates, run deploy/gates/provisionbench-job.yaml, then a live TEMPLATE-STAMPED
# standup: tpl1 (standard) + tpl2 (dedicated) coexisting — tpl1 canary derives
# tpl1-prod while tpl2 renders/holds tpl2-canary. Teardown deletes ONLY the new
# clusters/CRs/org rows (org DELETE cascades policies + project-envs).
```

### [ARCH/wamn-8df.5] unified copy — copy-project-env (deploy/promote/clone/move)

Docs: docs/archive/platform/deployment-model.md §4, docs/archive/platform/provisioning.md

```bash
cargo test -p wamn-control-provision copy      # the pure plan (clone vs cutover pipeline, unbuilt axes, quiesce/verify builders)
cargo test -p wamn-control-provision --test control_storage # saga shape + 'copy' kind drift-guard
cargo test -p wamn-schema-control             # select_applied_catalogs shape
cargo test -p wamn-ctl                # driver units (incl. the shared apply_catalog_target refactor)
cargo clippy -p wamn-control-provision -p wamn-control-registry -p wamn-schema-control -p wamn-ctl --all-targets \
  && cargo fmt -p wamn-control-provision -p wamn-control-registry -p wamn-schema-control -p wamn-ctl --check
# Throwaway-PG e2e gate (scratchpad/e2e_8df5.sh; postgres:18 on :5496): builds a
# src project-env (catalog via migrate-catalog + rows + a flow + RLS policy rows)
# and proves, 20 asserts:
#   R  --cutover without --system-database-url is REFUSED (the gate needs the T1 record)
#   A  cross-org DEFINITION clone ("deploy an app"): catalog applied in the dst env,
#      data tables exist, flow registration + RLS rows copied, the compiled RLS
#      policy LIVE on the dst table (pg_policies), zero rows carried, re-copy idempotent
#   C  DATA copy into a pre-populated dst FAILS verify (row counts differ) and the
#      saga records status=failed
#   B  the MOVE (both + cutover): saga completed with every step recorded (5/5),
#      dst holds rows+flow+policies+grants, snapshot recorded in provisioning.dumps,
#      and the src is quiesced — a post-cutover write from a FRESH session is
#      refused read-only (25006)
#   B2 a re-move with --deprovision-old --confirm: six-step saga completed, the
#      retained src database dropped
# Registry/migrate/provision live-apply regressions on the same throwaway:
export U=postgres://postgres:postgres@127.0.0.1:5496/postgres
WAMN_REGISTRY_PG_URL=$U cargo test -p wamn-control-provision --test control_storage # incl. copy-kind saga
WAMN_MIGRATE_PG_URL=$U cargo test -p wamn-schema-control --test migrate
WAMN_DUMP_PG_URL=$U WAMN_RESTORE_PG_URL=$U WAMN_PROVISION_PG_URL=$U cargo test -p wamn-control-provision
# 6 mutants killed (apply/test/restore, debug builds — scratchpad/mutate_8df5.py):
# M1 plan drops Quiesce (pure unit), M2 quiesce SQL read-only OFF (unit),
# M3 driver verify neutered (e2e scenario C), M4 saga advance no-op — the cutover
# gate REFUSES (e2e scenario B), M5 the sagas kind CHECK loses 'copy' (drift),
# M6 --disable-triggers dropped from the data-only restore (unit).
# IN-CLUSTER gate of record: a live CROSS-CLUSTER move — a pooled src project-env
# on wamn-pg copied --include both --cutover to a dedicated dst cluster with the
# saga recorded in the REAL wamn-sysdb (apply the additive sagas_kind_check ALTER
# first), quiesce proven on the live src, then --deprovision-old. Teardown deletes
# ONLY the new clusters/CRs/org rows; wamn-pg / wamn-sysdb untouched.
```

### [D6/wamn-e1g] per-org WAL/PITR via the Barman Cloud plugin + the shared object

Docs: docs/archive/platform/postgres-topology.md, docs/archive/platform/provisioning.md

```bash
cargo test -p wamn-control-provision -p wamn-ctl   # backup renderer + policy knobs + org/dump wiring + subcommand units
cargo clippy -p wamn-control-provision -p wamn-ctl -p wamn-control-registry -p wamn-gates --all-targets \
  && cargo fmt -p wamn-control-provision -p wamn-ctl -p wamn-control-registry -p wamn-gates --check
# Render a dedicated org's backup CRs locally (no cluster/DB needed; the prod
# policy's backup_cadence/wal_retention drive the CRs):
./target/debug/wamn-ctl provision-org --org demo --template standard \
  --emit-clusters /tmp/demo-clusters.json \
  --emit-object-store /tmp/demo-os.json --emit-scheduled-backup /tmp/demo-sb.json
# IN-CLUSTER gate of record = a LIVE WAL/PITR standup (the .6/.14 precedent; T3 pool
# precedent — the shared-cluster guardrail forbids re-applying wamn-pg/wamn-sysdb):
kubectl apply -f https://github.com/cert-manager/cert-manager/releases/download/v1.21.0/cert-manager.yaml
kubectl -n cert-manager wait --for=condition=Available deploy --all --timeout=180s
kubectl apply -f deploy/infra/barman-cloud-plugin.yaml
kubectl -n cnpg-system rollout status deploy/barman-cloud --timeout=180s
kubectl apply -f deploy/infra/minio.yaml
kubectl -n wamn-system rollout status deploy/minio --timeout=150s
kubectl -n wamn-system wait --for=condition=complete job/minio-init --timeout=120s
# backup CRs, not the registry row), apply ObjectStore -> Clusters -> ScheduledBackup:
env -u WAMN_SYSTEM_ADMIN_URL ./target/debug/wamn-ctl provision-org --org e1gate --template standard \
  --emit-clusters /tmp/e1-clusters.json \
  --emit-object-store /tmp/e1-os.json --emit-scheduled-backup /tmp/e1-sb.json
kubectl apply -f /tmp/e1-os.json                             # ObjectStore BEFORE the cluster
kubectl apply -f /tmp/e1-clusters.json
kubectl -n wamn-system wait --for=jsonpath='{.status.readyInstances}'=3 cluster/e1gate-prod --timeout=300s
kubectl apply -f /tmp/e1-sb.json                             # ScheduledBackup AFTER (immediate base backup)
# forbids re-applying the running clusters here):
kubectl -n wamn-system delete cluster e1gate-restore e1gate-prod e1gate-dev
kubectl -n wamn-system delete objectstore e1gate-prod-store
kubectl -n wamn-system delete scheduledbackup e1gate-prod-backup
```

### [5 / wamn-ctc8.6] first-party platform identity core

This gate covers the platform-plane `identity` schema, human and service
principals, Argon2id local-human verification, disabled-principal refusal,
opaque project roles, and the non-deserializable authenticated-principal seam.
It does not cover PATs, cookies, OIDC, middleware, or per-project `app_system`
identity.

```bash
cargo test --locked -p wamn-platform-identity
cargo clippy --locked -p wamn-platform-identity --all-targets -- -D warnings
cargo fmt -p wamn-platform-identity --check
# Live gate of record (throwaway postgres:18 only):
docker run -d --rm --name wamn-platform-identity-pg -p 5471:5432 \
  -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=wamn postgres:18
WAMN_PLATFORM_IDENTITY_PG_URL=postgres://postgres:postgres@127.0.0.1:5471/wamn \
  cargo test --locked -p wamn-platform-identity --test identity_live -- --nocapture
docker stop wamn-platform-identity-pg
```

### [5 / wamn-ctc8.7] personal access tokens and headless login

This gate covers `identity.pats`, the opaque token format, digest-at-rest
issuance, the `login_local` flow S0 uses headlessly, and the uniform refusal of
malformed, unknown, forged, expired, revoked, and disabled-principal tokens. It
also covers the state-ownership row for the new table. It does not cover HTTP
routes, middleware, audit wiring, cookies, or OIDC — those ride wamn-ctc8.8.

```bash
cargo test --locked -p wamn-platform-identity
cargo clippy --locked -p wamn-platform-identity --all-targets -- -D warnings
cargo fmt -p wamn-platform-identity --check
cargo test --locked -p wamn-proof-conformance --test state_ownership
# Live gate of record (throwaway postgres:18 only):
docker run -d --rm --name wamn-platform-identity-pg -p 5471:5432 \
  -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=wamn postgres:18
WAMN_PLATFORM_IDENTITY_PG_URL=postgres://postgres:postgres@127.0.0.1:5471/wamn \
  cargo test --locked -p wamn-platform-identity --test pat_live -- --nocapture
docker stop wamn-platform-identity-pg
```

### [5 / wamn-ctc8.8] authenticated management authoring surface

This gate covers the management HTTP boundary in front of the canonical
authoring commands: the `POST /authoring` and `POST /login` routes, PAT
verification against the T1 system database, the project-role check, the
append-only `catalog.authoring_command_audit` ledger that attributes every
authorized command to its principal, and the one frozen
`{"kind":"authorization-denied"}` refusal that absent, malformed, forged,
expired, revoked, cross-project, and unroled presenters all receive. It also
covers the ledger's schema-control drift entries (table count, privilege spec,
the four observation queries) and the adapter authority probe's allow-list. It
does not cover cookies, CSRF, OIDC, or the five contract commands this transport
answers `501` for — those ride wamn-ctc8.9, wamn-ftfc.2, and wamn-ftfc.14.

⚠️ The `wamn-scenario-worker` clippy leg is RED for reasons that predate this
bead: six findings in `services/scenario-worker/src/lib.rs` (one
`large_enum_variant` on `ScenarioTarget`, five `needless_borrow` in
`execute_case`) reproduce unchanged at `origin/main` 8546de5. `wamn-ctl` is red
the same way. Neither is in a file wamn-ctc8.8 touched, and neither was fixed
here; both need their own bead.

```bash
cargo test --locked -p wamn-scenario-worker -p wamn-schema-control -p wamn-ctl
cargo clippy --locked -p wamn-scenario-worker -p wamn-schema-control \
  --all-targets -- -D warnings
rustfmt --check --edition 2024 \
  services/scenario-worker/src/management.rs \
  services/scenario-worker/src/authoring.rs \
  services/scenario-worker/src/lib.rs \
  services/scenario-worker/src/main.rs \
  services/scenario-worker/tests/management_live.rs \
  crates/schema/control/src/run_plane.rs \
  services/ctl/src/publish_catalog.rs \
  services/ctl/tests/run_plane_live.rs
# Live gate of record (throwaway postgres:18 only). `pg_isready` reports ready
# during socket-only init, before the TCP listener binds — the sleep is load
# bearing, not superstition.
docker run -d --rm --name wamn-ctc88-pg -p 5472:5432 \
  -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=wamn postgres:18
until docker exec wamn-ctc88-pg pg_isready -U postgres; do sleep 1; done; sleep 3
WAMN_PLATFORM_IDENTITY_PG_URL=postgres://postgres:postgres@127.0.0.1:5472/wamn \
  cargo test --locked -p wamn-scenario-worker --test management_live -- --nocapture
docker stop wamn-ctc88-pg
```

### [5 / wamn-ctc8.9] browser sessions and CSRF-safe reserved auth routes

This gate covers the second presenter over the one identity core: `identity.sessions`,
the reserved `POST /session` (login) and `DELETE /session` (logout) routes, the
`HttpOnly; SameSite=Strict; Secure; Path=/` cookie framing, and the synchronizer
token bound to the session row that every state-changing request must echo in
`X-Wamn-Csrf`. It proves a session resolves the same principal and role a PAT
does and lands the same `catalog.authoring_command_audit` attribution, and that
session fixation, an absent/empty/wrong CSRF proof, an expired session, a revoked
session, a cross-project session, and a forged cookie each refuse with the frozen
`{"kind":"authorization-denied"}` document *before* any command runs, leaving the
ledger and the store untouched.

Sessions are a presenter, not an authority: there is no JWT, no OIDC, and no
second role store — both presenters funnel through `role_for`, which a drift test
pins. OIDC remains a later issuer (wamn-117).

⚠️ The `wamn-scenario-worker` clippy leg is RED for reasons that predate this
bead: the same six findings in `services/scenario-worker/src/lib.rs` recorded for
wamn-ctc8.8 (one `large_enum_variant` on `ScenarioTarget`, five `needless_borrow`
in `execute_case`). That file is not one this bead touched and the count is
unchanged; `wamn-platform-identity` is clean under `-D warnings`.

⚠️ Deployment is NOT covered here. `identity.sessions` and the
`wamn_scenario_worker_identity` grant delta (`SELECT, INSERT, UPDATE`) have been
applied only to throwaway PostgreSQL. The in-cluster sysdb rollout is deferred.

```bash
cargo test --locked -p wamn-platform-identity -p wamn-scenario-worker
cargo clippy --locked -p wamn-platform-identity --all-targets -- -D warnings
rustfmt --check --edition 2024 \
  crates/identity/platform/src/lib.rs \
  crates/identity/platform/tests/schema.rs \
  services/scenario-worker/src/management.rs \
  services/scenario-worker/tests/management_live.rs
# Live gate of record (throwaway postgres:18 only). `pg_isready` reports ready
# during socket-only init, before the TCP listener binds — the sleep is load
# bearing, not superstition.
docker run -d --rm --name wamn-ctc89-pg -p 5473:5432 \
  -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=wamn postgres:18
until docker exec wamn-ctc89-pg pg_isready -U postgres; do sleep 1; done; sleep 3
WAMN_PLATFORM_IDENTITY_PG_URL=postgres://postgres:postgres@127.0.0.1:5473/wamn \
  cargo test --locked -p wamn-scenario-worker --test management_live -- --nocapture
docker stop wamn-ctc89-pg
```

### [6A / wamn-ftfc.2] checkout-file draft submission

This gate covers the S1 write path: an authenticated checkout client reads
working-tree definition files and submits their content, with optional commit
provenance, through `POST /authoring` into the canonical save handler. It
covers three things the earlier surface could not do.

**Exact bytes.** `catalog.flow_drafts.definition` is `text`, so a saved
revision reads back byte for byte — whitespace, key order, trailing newline.
`graph_json` is retired to nullable (expand phase); the read falls back to it
only for rows written before this bead, whose exact bytes the old `jsonb` cast
destroyed and which are unrecoverable. There is deliberately no backfill: it
would manufacture an exactness promise, and it would add a writer on
`deploy/sql/catalog-schema.sql` that `state_ownership` correctly rejects.

**Preserved intermediate text.** Save no longer parses, so a half-finished or
emptied file is a preserved draft answering `200`, not a `500`. `validate`
parses the stored text at its own stage and keeps its typed refusals.

**Attribution that is never authority.** `save-flow-draft` carries an optional
`provenance` object recorded verbatim in three nullable
`catalog.authoring_command_audit` columns. It selects no principal, widens no
role, and changes no outcome; the mutant proving that is a named gate below.

Both schema changes are additive and land through the `publish_catalog.rs`
marker-slice pattern; `run_plane_live`'s authoring leg synthesizes the
pre-upgrade shape and re-publishes to prove the column probes converge.

⚠️ The `wamn-scenario-worker` clippy leg stays RED for the six pre-existing
`services/scenario-worker/src/lib.rs` findings described under wamn-ctc8.8.
None is in a file this bead touched.

```bash
cargo test --locked -p wamn-scenario-worker -p wamn-scenario-catalog \
  -p wamn-schema-control -p wamn-ctl
# The authoring-model tests bake CARGO_MANIFEST_DIR, so they need their own
# target directory when a shared cache is in use.
cargo test --locked -p wamn-authoring-model
cargo test --locked -p wamn-proof-conformance --test state_ownership
cargo clippy --locked -p wamn-scenario-worker -p wamn-authoring-model \
  -p wamn-scenario-catalog --all-targets -- -D warnings
# Public contract regeneration; both must be clean.
cargo run --locked --offline -p wamn-authoring-model \
  --example print-authoring-surface-schema > docs/archive/contracts/authoring-surface.schema.json
(cd clients/authoring-client && node scripts/generate.mjs --check && node scripts/test.mjs)
# Live gates of record (throwaway postgres:18). Run them against SEPARATE
# clusters: run_plane_live drops the runtime role, which management_live's
# grants pin.
docker run -d --rm --name wamn-ftfc2-pg -p 5473:5432 \
  -e POSTGRES_PASSWORD=wamn -e POSTGRES_DB=wamn postgres:18
until docker exec wamn-ftfc2-pg pg_isready -U postgres; do sleep 1; done; sleep 3
WAMN_CTL_PG_URL=postgres://postgres:wamn@127.0.0.1:5473/postgres \
  cargo test --locked -p wamn-ctl --test run_plane_live
docker stop wamn-ftfc2-pg
docker run -d --rm --name wamn-ftfc2-pg -p 5473:5432 \
  -e POSTGRES_PASSWORD=wamn -e POSTGRES_DB=wamn postgres:18
until docker exec wamn-ftfc2-pg pg_isready -U postgres; do sleep 1; done; sleep 3
WAMN_PLATFORM_IDENTITY_PG_URL=postgres://postgres:wamn@127.0.0.1:5473/wamn \
  cargo test --locked -p wamn-scenario-worker --test management_live -- --nocapture
docker stop wamn-ftfc2-pg
```

### [5 / wamn-ctc8.10] management surface in-cluster rollout

Deploys the wamn-ctc8.8 surface into the kind cluster and proves it from an
OFF-cluster client. Two things the bead's own gate does not cover, because that
gate runs against a throwaway postgres it provisions from scratch:

1. **Storage.** The adapter's startup authority probe hard-requires
   `catalog.{flow_drafts,validated_flow_drafts,draft_safe_connection_grants,
   authoring_command_audit}` plus
   `<run-schema>.{authoring_report_reservations,authoring_suite_case_facts,
   authoring_suite_reports}`. `wamn-ctl reconcile-run-plane` creates all of those
   TABLES additively — the catalog ones included (`CreateCatalogTable` actions).
   `catalog` is one schema shared by every project schema in the database, so one
   run covers them all.

   BOTH verbs are needed, and reconcile-run-plane is NOT a superset. Its column
   planner (`run_plane.rs`, "column drift on PRESENT record tables") iterates
   `RUN_PLANE_FILES` for the RUN schema only and never plans a `catalog` column,
   so a catalog table that exists but is missing a newly added COLUMN is invisible
   to it. Those land in `publish-catalog`'s guarded slices — wamn-ftfc.2's
   `AUTHORING DRAFT DEFINITION` (`flow_drafts.definition`, `graph_json` relaxed to
   nullable) and `AUTHORING COMMAND PROVENANCE` (three `provenance_*` columns) are
   exactly that shape. Run reconcile-run-plane for the tables, then
   publish-catalog for the columns; skipping the second leaves `save-flow-draft`
   writing a column the live catalog does not have.
2. **A correctly-scoped author credential.** The probe REFUSES to serve unless
   the login role is unprivileged, a member of nothing but
   `wamn_scenario_author`, denied `wamn_app`, and owns nothing in `catalog` or
   the run schema. A mis-scoped Secret crash-loops instead of serving.

OPERATOR PRECONDITIONS — neither is self-serviceable, and the Deployment cannot
start without the first:

* **`wamn-system-db` Secret** (key `url`). Recorded as operator-provided by both
  `deploy/platform/scenario-worker.yaml` and `event-reader.example.yaml` ("no
  chart ships it"). `serve()` connects it before the authority probe runs.
* **The `identity` schema in the T1 system database**
  (`identity.principals` / `local_credentials` / `project_roles` / `pats`,
  `deploy/sql/system-schema.sql`). Required for `POST /login` and for the
  attribution `POST /authoring` records. The T1 apply recipe in
  [D6 / wamn-q3n.3] predates wamn-ctc8.6/.7: it drops and recreates only
  `registry, provisioning`, so applying it whole is NOT additive against a live
  system database — apply the `identity` slice on its own instead.
  `identity.project_roles` carries an FK to `registry.projects (org, id)`, so
  the served (org, project) needs its `registry.orgs` + `registry.projects` rows
  first (`wamn-ctl provision-org` / `provision-project-env`).

```bash
docker build --target scenario-worker -t wamn-scenario-worker:dev .
docker build --target ctl -t wamn-ctl:dev .
kind load docker-image wamn-scenario-worker:dev wamn-ctl:dev --name wamn

# Storage. Read the --dry-run plan FIRST: every action must be a Create*,
# AddColumn, Repair*, or Ensure* — a Drop/Truncate means STOP. Jobs are
# immutable, so delete before re-applying.
kubectl -n wamn-system delete job ctc810-runplane --ignore-not-found
kubectl -n wamn-system apply -f <reconcile-run-plane Job, --schema wamn_run, --dry-run>
kubectl -n wamn-system logs job/ctc810-runplane
# then re-apply the same Job without --dry-run

# The dedicated author login (NEW role; password generated into a mode-600 file,
# never echoed, and the SQL shredded afterwards):
#   CREATE ROLE <role> LOGIN INHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE
#     NOREPLICATION NOBYPASSRLS PASSWORD '<generated>';
#   GRANT wamn_scenario_author TO <role>;
kubectl -n wamn-system create secret generic wamn-authoring-<org>--<project>--<env> \
  --from-file=url=<mode-600 file holding the postgres:// URL>

kubectl -n wamn-system apply -f deploy/platform/scenario-worker.yaml
kubectl -n wamn-system rollout status deploy/scenario-worker --timeout=180s

# Off-cluster proof. kubectl port-forward on this kind cluster dies on every
# connection close, so reach the surface through a temp NodePort on a kind node's
# docker IP. kube-proxy needs a few seconds to program it — the first probe
# legitimately refuses. Delete the NodePort afterwards.
kubectl -n wamn-system create service nodeport ctc810-verify-nodeport \
  --tcp=8088:8088 --node-port=31088
kubectl -n wamn-system patch svc ctc810-verify-nodeport \
  -p '{"spec":{"selector":{"app":"scenario-worker"}}}'
NODE_IP=$(docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' wamn-worker)
# (a) tokenless -> byte-exact 403 {"kind":"authorization-denied"}
curl -sS -o /dev/stdout -w ' %{http_code}\n' -X POST "http://$NODE_IP:31088/authoring"
# (b) POST /login  -> {"token":"wamn_pat_<16 hex>_<64 hex>","expires_at":...}
# (c) authenticated save-flow-draft -> 200 + one attributed row:
#     SET app.tenant='<tenant>';
#     SELECT command_kind, principal_subject, effective_role, target_ref
#       FROM catalog.authoring_command_audit;
# (d) the pod log must contain NO secret/PAT material.
kubectl -n wamn-system delete svc ctc810-verify-nodeport
```

⚠️ `reconcile-run-plane` against the deployed `wamn_run` fixture stops at
`BackfillEffectAttempts` with `legacy-effect-attempt-incomplete`
(`crates/schema/control/src/run_plane.rs:1554`): the deployed `node_runs` rows
predate the wamn-4u7p effect-attempt era and carry no complete attempt fact set,
so the guard refuses to synthesize provenance rather than fabricate it. That is
the correct refusal, and it lands AFTER every authoring table and privilege in
the plan — so the management surface's storage requirement is fully satisfied
even though the run exits non-zero. Converging the effect-attempt lineage on
that legacy fixture needs its own bead.

📌 RESOLVED — the `serve` CLI defect this rollout found (wamn-kisz, fixed at
`927db6f`). Recorded because it is the reason an in-cluster rollout gate exists,
and because the shape recurs whenever a binary carries two invocations.

This rollout was the first thing ever to run `serve` through its CLI: the
wamn-ctc8.8 `management_live` gate drives `management::serve()` as a LIBRARY
function, so the shipped argument wiring was unexercised and
`deploy/platform/scenario-worker.yaml` crash-looped with
`error: the following required argument was not provided: tenant`, printing the
BARE-invocation usage. It was not an environment or manifest problem — it
reproduced with no environment set and every argument passed explicitly.

Cause and fix: `subcommand_negates_reqs = true` relaxes the usage line but still
enforces a REQUIRED flattened struct, so `serve` died on the bare path's
`--tenant` even with all of its own arguments satisfied. Making the flattened
group `Option<ScenarioWorkerArgs>` is what actually relaxes it; wamn-kisz pairs
that with `arg_required_else_help` so an empty invocation still cannot silently
become a bare run, and pins both invocations with named CLI-parse tests.

No workaround is needed now — `serve` starts natively from the manifest as
written. The interim live-Deployment placeholder-args patch is RETIRED; the
running spec matches `deploy/platform/scenario-worker.yaml` exactly. If you are
re-running the rollout script, `SKIP_CLI_DEFECT_PATCH=1` is the correct setting.

### [6A / wamn-jvzx.4] authenticated S0 smoke over the request collection

Runs the checked-in collection (`docs/archive/contracts/authoring-surface.v0.1.http`)
against the deployed management surface as two real principals. The COLLECTION is
the artifact under test, so `clients/authoring-client/scripts/smoke.mjs` derives
every executable field of its request from the `save-flow-draft` section and
refuses to send anything when its outgoing document diverges from that section
outside three declared per-run substitutions — `command-id` (one attempt),
`draft-id` (so a run cannot collide with the last one), and `expected-revision`
(the collection's own optimistic-concurrency field, which has to track the
revision the draft actually has). A pinned SHA-256 of the canonicalized section
catches a collection-side edit; the derivation comparison catches a script-side
hand-rolled field. Neither can be a quiet difference.

**THE CLIENT/RUNNER SPLIT, AND THE NO-DATABASE-URL BOUNDARY.** The script is pure
HTTP: subject and secret to the reserved `POST /login`, the issued Bearer PAT to
`POST /authoring`, nothing else. It carries no database URL, no platform-admin
impersonation, and no test-only trusted context — so it cannot read the ledger it
is proving, because that read needs storage authority a client must never hold.
It closes instead by printing one `AUDIT-MANIFEST` line naming the command-ids
that MUST appear with which `principal_subject` and the command-ids that MUST NOT
appear at all; the GATE RUNNER does the ledger read below and checks it against
that manifest. Its whole input surface is the base URL and two credential file
paths. The two route paths are the wamn-ctc8.8 transport contract, which the
collection deliberately leaves to its caller — supplying them is this script's
job, not an endpoint the collection invented.

**FOUR LEGS ON ONE DRAFT.** Principal A creates the run's draft at
`expected-revision: 0` (revision 1). The tokenless attempt, the forged-token
attempt, and principal B then all present `expected-revision: 1` against that
same draft — the same command shape, the same target, the same expected version,
differing only in the credential presented and in the `command-id` that
identifies the attempt. Distinct `command-id`s are what make the refusals'
ledger ABSENCE checkable at all; reusing one across three attempts would also
abuse the contract's per-command identity.

**WHY THE REFUSALS ARE PROVABLY PRE-EXECUTION.** Two independent facts, and the
gate asserts both. (1) Neither refused `command-id` appears in
`catalog.authoring_command_audit` — and the surface records BEFORE it runs a
command, so an absent row means no command ran. (2) Principal B's save succeeds
at `expected-revision: 1` and returns revision 2, which is only reachable if
neither refused attempt advanced the draft. A forged token is structurally valid
with a real lookup half and one flipped hex digit in the secret half, so it
exercises digest verification rather than parse rejection, and its refusal must
be the byte-exact uniform `{"kind":"authorization-denied"}` under HTTP 403.

**TOKEN HYGIENE.** No secret ever reaches a command line or an environment block:
credentials are read from mode-600 `subject=`/`secret=` files. The script's own
`emit` refuses to print a registered secret, the issued PAT, or the forged token,
and fails the named check `no-token-material-in-output` if a line ever would.

OPERATOR PRECONDITIONS, neither self-serviceable: the wamn-ctc8.10 rollout, and
TWO principals in the T1 `identity` schema, each with a local credential and
`project-author` on the served `(org, project)`.

```bash
# Static half. Network-free, credential-free, and already inside the client
# harness, so a collection edit fails CI with no cluster in the loop.
(cd clients/authoring-client && node scripts/smoke.mjs --check)
(cd clients/authoring-client && node scripts/test.mjs)

# Live gate of record. Reach the in-cluster surface the wamn-ctc8.10 way — a temp
# NodePort on a kind node's docker IP, because port-forward dies on every
# connection close here. kube-proxy needs a few seconds to program it; the first
# probe legitimately refuses. Delete the NodePort afterwards.
kubectl -n wamn-system create service nodeport jvzx4-smoke-nodeport \
  --tcp=8088:8088 --node-port=31188
kubectl -n wamn-system patch svc jvzx4-smoke-nodeport \
  -p '{"spec":{"selector":{"app":"scenario-worker"}}}'
NODE_IP=$(docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' wamn-worker)
(cd clients/authoring-client && node scripts/smoke.mjs \
  --base-url "http://$NODE_IP:31188" \
  --principal <mode-600 subject=/secret= file for the first principal> \
  --principal <mode-600 subject=/secret= file for the second principal>)
# -> SMOKE PASS, and one AUDIT-MANIFEST line. Keep it: the next step needs it.

# RUNNER-SIDE AUDIT VERIFICATION. The ledger is in the project database, so this
# is a kubectl exec psql on the FIXTURE pod — deliberately outside the script.
# (a) exactly the two `must-appear` command-ids, one row each, identical
#     command_kind and target_ref, distinguished only by principal_subject:
kubectl -n wamn-system exec deploy/postgres -- psql -U postgres -d wamn -c \
  "SET app.tenant = '<tenant>';
   SELECT command_id, command_kind, principal_subject, effective_role, target_ref
     FROM catalog.authoring_command_audit
    WHERE command_id LIKE '%<run-id>%' ORDER BY recorded_at;"
# (b) the two `must-not-appear` command-ids must count 0 — a refused attempt
#     writes no row, because the refusal precedes the command that records it:
kubectl -n wamn-system exec deploy/postgres -- psql -U postgres -d wamn -At -c \
  "SET app.tenant = '<tenant>';
   SELECT count(*) FROM catalog.authoring_command_audit
    WHERE command_id IN ('<tokenless command-id>', '<forged-token command-id>');"
# (c) token-material scan on the ACTUAL rows. The ledger schema stores no
#     credential column by design; assert it on the rows anyway, and grep -F each
#     credential file's secret against the rows and the smoke transcript too.
kubectl -n wamn-system exec deploy/postgres -- psql -U postgres -d wamn -At -c \
  "SET app.tenant = '<tenant>';
   SELECT to_jsonb(a)::text FROM catalog.authoring_command_audit a
    WHERE command_id LIKE '%<run-id>%';" > <mode-600 rows file>
grep -q 'wamn_pat_' <mode-600 rows file> && echo 'FAIL: PAT material in the ledger'

kubectl -n wamn-system delete svc jvzx4-smoke-nodeport

# Mutants (sha256 apply/test/restore; each must print KILLED).
tools/gate-mutants/authoring-smoke-collection-drift.sh run   # network-free
WAMN_AUTHORING_SMOKE_BASE_URL="http://$NODE_IP:31188" \
  WAMN_AUTHORING_SMOKE_PRINCIPAL_A=<first credential file> \
  WAMN_AUTHORING_SMOKE_PRINCIPAL_B=<second credential file> \
  tools/gate-mutants/authoring-smoke-forged-token.sh run
```

`authoring-smoke-collection-drift` writes a field the collection owns (`flow-id`)
and must die on `collection-derivation` before any request is sent.
`authoring-smoke-forged-token` tells the script to read the forged leg's reply as
an authorized success and must die on `authoring-leg-forged-token-status`; being
live, it logs in and writes its own draft plus one audit row for principal A
before it fails, which is expected fixture residue rather than a gate result.

### [6A / wamn-ftfc.14] the headless CLI's edit-to-publish cycle

The reference checkout client (`clients/authoring-client/scripts/wamn.mjs`, source
in `src/cli/cli.ts`) drives the whole authoring loop over HTTP through the
wamn-jvzx.2 generated client. Five verbs cover the six public command kinds:
`validate` sends `save-flow-draft` then `validate`, `draft-run` and `suite-run`
send themselves, `promote` sends `publish`, and `runs` reads `suite-projection`.
Two gates own it.

**STATIC HALF — `node scripts/test.mjs`.** Network-free and credential-free, so a
drift fails CI with no surface in the loop. It carries the wamn-jvzx.2 client
suite plus the CLI suite, which proves three things. (1) REQUEST-SHAPE DRIFT:
every document the CLI can build is compared key for key with the matching
section of `docs/archive/contracts/authoring-surface.v0.1.http` and is decoded by the
generated closed validator, so the collection and the schema own the SHAPE while
the client owns only the VALUES. A pinned SHA-256 over the collection's shape
(field names, nesting, leaf types, values erased) catches a collection-side
change; a value edit deliberately does not move it. (2) TYPED ANSWERS: a
completed command, a product refusal, an unmounted command, and an
infrastructure fault are four distinct documents with four distinct exit codes
(`0`, `3`, `4`, `5`), and the `501` case carries no result and no refusal.
(3) ABSENCE OF SHORTCUTS, structurally: the compiled CLI imports the generated
client and NOTHING else — no node builtin, so it cannot open a socket, a file, or
a process on its own; every capability arrives through the injected port in
`scripts/wamn.mjs`, whose only child process is a read-only `git` query; the
contract version reaches a request from the generated constant alone and no flag
can select another; and a launch with `WAMN_AUTHORING_ENDPOINT`,
`WAMN_AUTHORING_BEARER_TOKEN`, `WAMN_SYSTEM_URL`, `WAMN_AUTHORING_PG_URL`,
`PGPASSWORD`, and `DATABASE_URL` all poisoned still refuses for want of
`--base-url` and echoes none of them.

**LIVE HALF — `node scripts/cycle.mjs`.** Edit a flow file in a real checkout,
`validate`, edit it again, `validate`, then `draft-run`, `suite-run`, `runs`, and
`promote`, each one a subprocess invocation of the shipped CLI whose stdout
document is the result. Like the wamn-jvzx.4 smoke it is PURE HTTP: its whole
input surface is `--base-url`, ONE `--credential` file, `--project`,
`--environment`, and an optional `--checkout`. It holds no database URL, no
platform-admin impersonation, and no test-only trusted context, so it cannot read
the ledger it is proving — it prints one `VERIFY-MANIFEST` line instead and the
runner does that read below.

**HONEST 501s, AND WHY THE GATE STILL PASSES.** The management surface mounts the
command kinds whose handlers have landed and answers a bare `501` for the rest
(the per-kind mount beads wamn-ftfc.30–.34 own mounting the remainder;
wamn-ftfc.22 closed having proven every remaining backend absent). Each cycle step therefore asserts the
CONTRACT shape of whatever answer it gets — `completed` must carry that command's
required identity fields, `refused` must carry a typed reason, `unmounted` must be
a bare `501` with no document — and a `fault` fails the gate. The two saves are
required to complete at revisions 1 and 2, because that is what proves
working-tree content reaching the canonical save handler through optimistic
concurrency. The run then prints `CYCLE-COMPLETED` and `CYCLE-UNMOUNTED-501`, so
the record says exactly which steps a surface answered and which it did not.
While `validate` and `suite-run` are unmounted there is no validated-draft or
report identity to carry forward, so the downstream legs present a
contract-shaped placeholder purely to reach the transport; on a surface that
mounts them the real identity flows instead, and `runs`/`promote` then answer
`completed` or a typed refusal.

**EDIT-TO-RUN LATENCY.** The CLI measures it where a checkout client can: from the
modification time of the definition file it submitted to the arrival of a run
receipt, printed as `edit-to-run-ms` on stderr and carried in the stdout
document. When a report finalizes, `runs` also reports the platform's own
`DraftSuiteProjection.edit-to-run-ms` as `server-edit-to-run-ms`. Until
`draft-run`/`suite-run` are mounted no receipt exists, and the gate prints
`edit-to-run-ms=unmeasurable` with the reason rather than a number it did not
measure; the exact-value assertion lives in the static half.

**A LOCAL SURFACE FOR THE LIVE HALF.** Either the in-cluster wamn-ctc8.10 rollout
(reach it the `[6A / wamn-jvzx.4]` way) or a local `serve` against a throwaway
PostgreSQL. The local recipe deliberately has no seeding tool of its own: the
wamn-ctc8.8 `management_live` gate already provisions the schemas, the
unprivileged author login, the registry org/project rows, and the principals with
Argon2id local credentials, so RUNNING THAT GATE IS THE SEED.

```bash
# 1. Throwaway PostgreSQL. Wait for the SECOND pg_isready: postgres:18
#    initializes, restarts, and only then serves TCP; a host connection during
#    the socket-only phase is refused.
docker run -d --name wamn-ftfc14-pg \
  --env-file <mode-600 file holding POSTGRES_PASSWORD=...> \
  -p 127.0.0.1:15432:5432 postgres:18
docker exec wamn-ftfc14-pg pg_isready -U postgres && sleep 3
docker exec wamn-ftfc14-pg pg_isready -U postgres

# 2. Seed by running the wamn-ctc8.8 gate against it. It leaves
#    alice@example.com with `project-author` on acme/receiving and the local
#    secret that gate declares, plus the wamn_management_live_author login the
#    surface needs.
WAMN_PLATFORM_IDENTITY_PG_URL=postgres://postgres:PW@127.0.0.1:15432/postgres \
  cargo test --locked -p wamn-scenario-worker --test management_live

# 3. Serve the same database on a port of its own. management_live binds
#    127.0.0.1:18088 while it runs, so do not reuse that port.
cargo run --locked -p wamn-scenario-worker -- serve \
  --bind 127.0.0.1:18188 \
  --system-url postgres://postgres:PW@127.0.0.1:15432/postgres \
  --authoring-database-url \
    postgres://wamn_management_live_author:wamn-management-live@127.0.0.1:15432/postgres \
  --org acme --project receiving --tenant management-live-tenant \
  --source-schema management_live_source

# 4. The gates. The credential file is mode-600 `subject=`/`secret=` lines.
(cd clients/authoring-client && node scripts/test.mjs)
(cd clients/authoring-client && node scripts/cycle.mjs \
  --base-url http://127.0.0.1:18188 \
  --credential <mode-600 subject=/secret= file> \
  --project receiving --environment dev)
# -> CYCLE PASS, one CYCLE-UNMOUNTED-501 line, and one VERIFY-MANIFEST line.
#    Keep the manifest: the next step needs it.

# 5. RUNNER-SIDE LEDGER VERIFICATION, deliberately outside the client, because
#    this read needs storage authority a client must not hold.
#    (a) exactly the manifest's `must-appear` command-ids, one row each, same
#        principal, with the client's provenance recorded verbatim:
docker exec wamn-ftfc14-pg psql -U postgres -d postgres -c \
  "SET app.tenant = 'management-live-tenant';
   SELECT command_id, command_kind, principal_subject, effective_role, target_ref,
          provenance_commit, provenance_ref, provenance_dirty
     FROM catalog.authoring_command_audit
    WHERE command_id LIKE '%<run-id>%' ORDER BY recorded_at;"
#    (b) the `must-not-appear` command-ids must count 0 — a refusal precedes the
#        command that records it:
docker exec wamn-ftfc14-pg psql -U postgres -d postgres -At -c \
  "SET app.tenant = 'management-live-tenant';
   SELECT count(*) FROM catalog.authoring_command_audit
    WHERE command_id = '<forged command-id>';"
#    (c) the stored draft is the working-tree file, byte for byte: compare
#        sha256(definition) with the manifest's `definition-sha256`:
docker exec wamn-ftfc14-pg psql -U postgres -d postgres -At -c \
  "SET app.tenant = 'management-live-tenant';
   SELECT revision, encode(sha256(definition::bytea), 'hex'), length(definition)
     FROM catalog.flow_drafts WHERE draft_id = '<draft-id>';"
#    (d) no credential material in the rows:
docker exec wamn-ftfc14-pg psql -U postgres -d postgres -At -c \
  "SET app.tenant = 'management-live-tenant';
   SELECT to_jsonb(a)::text FROM catalog.authoring_command_audit a
    WHERE command_id LIKE '%<run-id>%';" | grep -c 'wamn_pat_'   # must be 0

docker rm -f wamn-ftfc14-pg     # teardown; shred the credential files

# Mutants (sha256 apply/test/restore; each must print KILLED; both network-free).
tools/gate-mutants/authoring-cli-collection-drift.sh run
tools/gate-mutants/authoring-cli-unmounted-green.sh run
```

**RESULT OF RECORD (2026-08-08, integrated tree, local `serve` at
`127.0.0.1:18188`, run-id `mskcytxp-4f16`).** `node scripts/test.mjs` 14/14 + 16/16 plus `cycle --check`.
`node scripts/cycle.mjs` CYCLE PASS with
`CYCLE-COMPLETED ["save-flow-draft"]` and
`CYCLE-UNMOUNTED-501 ["validate","draft-run","suite-run","suite-projection","publish"]`
— on that surface `save-flow-draft` is the only mounted kind, so five of the six
cycle steps honestly answer `501` and the run receipt that carries edit-to-run
latency does not exist yet. Runner-side ledger read: exactly the two
`must-appear` command-ids, one row each, both `alice@example.com` /
`project-author` on the same `target_ref`, with provenance recorded verbatim
(the fixture checkout's commit, `refs/heads/main`, `dirty=f` for the committed
tree and `t` for the edited one); the forged attempt counted `0` rows; the
stored draft sat at revision 2 with `sha256(definition) = 303b8fb7…` and length
242, byte-identical to the working-tree file the client submitted; and no
`wamn_pat_` or secret material in the rows. Both mutants KILLED.
wamn-ftfc.22 landed mounting NOTHING (every remaining backend is genuinely
absent — see its close), so this five-501 record remains current. RE-RUN THE
LIVE HALF AS EACH MOUNT BEAD (wamn-ftfc.30–.34) LANDS: the same command must
then report the newly mounted kind as `completed` (or a typed refusal), print a
real `edit-to-run-ms`, and — once wamn-ma5's projection field exists —
`server-edit-to-run-ms`.

`authoring-cli-collection-drift` makes `save-flow-draft` drop the caller's
optional `provenance` claim and must die on the request-shape drift check before
anything is sent; a field RENAME cannot be the mutation because the generated
schema types reject a misspelling at compile time.
`authoring-cli-unmounted-green` makes the client read a bare `501` as a completed
command and must die on the typed-answer check — a green cycle over unmounted
handlers is exactly the false evidence this bead must not produce.

⚠️ **Do not share one `CARGO_TARGET_DIR` between two worktrees of this
repository.** Building `-p wamn-scenario-worker` from a second worktree overwrites
the first worktree's binaries and integration-test executables in place, and a
later `cargo test`/`cargo run` from either worktree can then run the OTHER tree's
code while reporting nothing unusual. Give each worktree its own target directory
when lanes run in parallel.

### [2.4] per-project system schema v1

Docs: docs/archive/schema/app-schema.md

```bash
cargo test -p wamn-project-state     # unit (status literals + table manifest) + drift-guard
cargo clippy -p wamn-project-state --all-targets && cargo fmt -p wamn-project-state --check
# optional live-apply gate (throwaway postgres:18; superuser url provisions
# when unset):
docker run -d --rm --name wamn-as5-pg -p 5466:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_SYSSCHEMA_PG_URL=postgres://postgres:postgres@127.0.0.1:5466/wamn cargo test -p wamn-project-state
docker stop wamn-as5-pg
```

### [2.5] migration engine (crates/schema/control + wamn-ctl migrate-catalog)

Docs: docs/archive/schema/migration-engine.md

```bash
cargo test -p wamn-schema-control     # unit (guards/gate/dry-run/rollback) + drift-guard + live-apply
cargo test -p wamn-ctl --lib migrate_catalog   # the subcommand's bare-ident + param-map units
# Static proof spans the decision library and the ctl service library that owns
# migrate-catalog; the binary-only host is outside this boundary.
cargo clippy -p wamn-schema-control -p wamn-ctl --all-targets \
  && cargo fmt -p wamn-schema-control -p wamn-ctl --check
# optional live-apply gate (throwaway postgres:18; superuser url — provisions
# unset):
docker run -d --rm --name wamn-schema-control-pg -p 5467:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_MIGRATE_PG_URL=postgres://postgres:postgres@127.0.0.1:5467/wamn cargo test -p wamn-schema-control
docker stop wamn-schema-control-pg
# The production tool is `wamn-ctl migrate-catalog --admin-database-url <superuser>
```

### [3.1] metadata catalog schema crate (crates/schema/model)

Docs: docs/archive/schema/catalog-model.md

```bash
cargo test -p wamn-schema-model
cargo clippy -p wamn-schema-model --all-targets && cargo fmt -p wamn-schema-model --check
# regenerate the published JSON Schema contract after changing the types:
cargo run -p wamn-schema-model --example print-catalog-model-schema > docs/archive/contracts/catalog-model.schema.json
# cjv.5 expression-chaining guard (unsafe_expression_reason): the Check (here) and
# RolePredicate (wamn-schema-compiler) validators reject a top-level ';', unbalanced parens, or
# a comment-open. Mutation harness (5 mutants, each fails a named test in
# wamn-schema-model/wamn-schema-compiler): scratchpad/mutate_cjv5.py.
```

### [3.2] DDL compiler crate (crates/schema/compiler)

Docs: docs/archive/execution/run-queue.md, docs/archive/schema/ddl-compiler.md

```bash
cargo test -p wamn-schema-compiler
cargo clippy -p wamn-schema-compiler --all-targets && cargo fmt -p wamn-schema-compiler --check
# optional live-apply gates (emitted SQL; a
# superuser URL — provisions wamn_app + ephemeral schemas; skips when unset):
docker run -d --rm --name wamn-schema-compiler-pg -p 5451:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_DDL_PG_URL=postgres://postgres:postgres@127.0.0.1:5451/wamn cargo test -p wamn-schema-compiler
docker stop wamn-schema-compiler-pg
# The WAMN_DDL_PG_URL run includes the cjv.5 live proof
# chaining_check_expression_never_reaches_postgres: a chaining Check is rejected at
# compile time so its DROP never reaches Postgres (a neutered guard would apply it
# and fail).
```

### [3.4] schema versioning & environments crate (crates/schema/control/src/lifecycle)

Docs: docs/archive/schema/schema-lifecycle.md

```bash
cargo test -p wamn-schema-control
cargo clippy -p wamn-schema-control --all-targets && cargo fmt -p wamn-schema-control --check
# optional storage check (the whole standalone schema re-applies on a throwaway
# PG18; it assumes a pre-existing wamn_app role, as in production):
docker run -d --rm --name wamn-cat-pg -p 5452:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
docker exec -i wamn-cat-pg psql -U postgres -d wamn -c \
  "CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS;"
docker exec -i wamn-cat-pg psql -v ON_ERROR_STOP=1 -U postgres -d wamn \
  < deploy/sql/catalog-schema.sql
docker stop wamn-cat-pg
```

### [3.5] RLS policy builder crate (crates/schema/compiler/src/rls)

Docs: docs/archive/schema/rls-builder.md

```bash
cargo test -p wamn-schema-compiler
cargo clippy -p wamn-schema-compiler --all-targets && cargo fmt -p wamn-schema-compiler --check
# optional live-apply gate (floor + compiled policy on a throwaway PG; asserts
# no-user-claim denies all; superuser URL provisions wamn_app; skips when unset):
docker run -d --rm --name wamn-schema-compiler-pg -p 5453:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_RLS_PG_URL=postgres://postgres:postgres@127.0.0.1:5453/wamn cargo test -p wamn-schema-compiler
docker stop wamn-schema-compiler-pg
```

### [3.6] seed-data & fixtures crate (crates/schema/compiler/src/seed)

Docs: docs/archive/schema/seed-data.md

```bash
cargo test -p wamn-schema-compiler
cargo clippy -p wamn-schema-compiler --all-targets && cargo fmt -p wamn-schema-compiler --check
# optional live-apply gate (floor + compiled seed on a throwaway PG; loads TWICE
# when unset):
docker run -d --rm --name wamn-schema-compiler-pg -p 5454:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_SEED_PG_URL=postgres://postgres:postgres@127.0.0.1:5454/wamn cargo test -p wamn-schema-compiler
docker stop wamn-schema-compiler-pg
```

### [4.1] REST API gateway (crates/data/entity-access + crates/data/api + components/ingress/api-gateway)

Docs: docs/archive/platform/api-gateway.md

```bash
cargo test -p wamn-entity-access -p wamn-api
cargo clippy -p wamn-entity-access -p wamn-api --all-targets \
  && cargo fmt -p wamn-entity-access -p wamn-api --check
# cjv.6: every list appends the unique `id ASC` tiebreaker so OFFSET pagination is
# stable under any user sort (C5-1). Mutation (revert to the guarded append -> both
# sort_and_paginate_are_capped_and_parametrized and user_sort_still_appends_the_id_tiebreaker
# fail): scratchpad/mutate_cjv6.py.
# wamn_app + seeds two tenants + the catalog snapshot the gateway reads):
docker run -d --rm --name wamn-api-pg -p 5455:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
REL=components/target/wasm32-wasip2/release
WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:5455/wamn \
  ./target/release/wamn-gates --log-level error apibench \
  --api-gateway $REL/api_gateway.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5455/wamn --mode all
docker stop wamn-api-pg
# In-cluster gate of record (co-located with Postgres, no cpu limit — S2 lesson;
# WAMN_PG_ADMIN_URL is the superuser used only to provision the ephemeral schema):
kubectl -n wamn-system apply -f deploy/gates/apibench-job.yaml
kubectl -n wamn-system logs -f job/apibench
```

### [4.1b] api-gateway SERVING deployment + catalog snapshot

Docs: docs/archive/platform/api-gateway.md

```bash
# Unit/fixture boundaries: publish-catalog belongs to wamn-ctl and the API
# fixture lives in repository test support. The host remains the serving
# artifact and the gates package remains the deployed router.
# recipe-test: H5-API-PUBLISH | unit | wamn-ctl | lib | - | publish_catalog::tests:: | 1 | services/ctl/src/publish_catalog.rs pre-I/O schema boundary
cargo test -p wamn-ctl --lib publish_catalog::tests::
# recipe-test: H5-API-FIXTURE | fixture | wamn-test-fixtures | lib | - | apifixture::tests:: | 2 | test-support/fixtures/apifixture.rs API catalog and floor coherence
cargo test -p wamn-test-fixtures --lib apifixture::tests::
cargo clippy -p wamn-host -p wamn-ctl -p wamn-gates --all-targets \
  && cargo fmt -p wamn-host -p wamn-ctl -p wamn-gates --check
# In-cluster proof of record (needs the kind 'wamn' cluster + operator + postgres):
docker build --target host -t wamn-host:dev . \
  && docker build --target gates -t wamn-gates:dev .   # cached; two tags, one build
kind load docker-image wamn-host:dev --name wamn && kind load docker-image wamn-gates:dev --name wamn
kind load docker-image registry:2 --name wamn
kubectl -n wamn-system apply -f deploy/platform/registry.yaml
kubectl -n wamn-system rollout status deploy/registry --timeout=60s
kubectl -n wamn-system port-forward svc/registry 5000:5000 &
wash push localhost:5000/wamn/api-gateway:dev \
  components/target/wasm32-wasip2/release/api_gateway.wasm --insecure
# The host group gains --allow-insecure-registries + WAMN_PG_URL:
helm upgrade --install -n wamn-system wamn \
  oci://ghcr.io/wasmcloud/charts/runtime-operator --version 2.5.2 \
  -f deploy/infra/values-wamn.yaml
kubectl -n wamn-system rollout status deploy/hostgroup-default --timeout=150s
# Provision the project schema/floor + seed + publish the snapshot:
kubectl -n wamn-system create configmap proof-catalog \
  --from-file=proof-catalog.json=deploy/poc/proof-catalog.json
kubectl -n wamn-system apply -f deploy/gates/publish-catalog-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/publish-catalog --timeout=120s
# Deploy the gateway workload, then prove it serves over the network:
kubectl -n wamn-system apply -f deploy/platform/api-gateway-workload.yaml
kubectl -n wamn-system apply -f deploy/gates/apiproof-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/apiproof --timeout=180s
kubectl -n wamn-system logs job/apiproof
```

### [4.1c] callable-flow HTTP adapter (components/ingress/flow-http)

Docs: FLOW-SPEC rev18 §§6.2, 7.2–7.3, 8, and 11

```bash
# Adversarial fake-provider proof: route precedence and disabled-definition
# recovery lookup, selected-policy auth, partial/oversize body reads, mapping
# and schema refusals, fingerprint/admission identity, every stored outcome and
# typed rejection, finite wait, disconnect cancel, and provider failures.
cargo test --locked --manifest-path components/Cargo.toml -p flow-http

# Deployable guest imports only authoritative route/auth and the frozen
# wamn:flow-invocation boundary; it exports wasi:http/incoming-handler.
cargo build --locked --manifest-path components/Cargo.toml -p flow-http \
  --target wasm32-wasip2

# Static exclusion: the adapter has no graph walker, run SQL, queue insertion,
# direct PostgreSQL import, or invocation-provider implementation.
! rg -n 'wamn:postgres|INSERT[[:space:]]+INTO[[:space:]]+(runs|run_queue)|FlowGraph|GraphWalker' \
  components/ingress/flow-http
```

### [POC-DM1] data model via the catalog API (wamn-521, P1 build)

Docs: docs/archive/poc/poc-material-receiving.md, docs/archive/poc/poc-dm1.md

```bash
cargo test -p wamn-dm1     # drift-guard + compile checks + live-apply gate (skips w/o WAMN_DM1_PG_URL)
cargo clippy -p wamn-dm1 --all-targets && cargo fmt -p wamn-dm1 --check
# optional throwaway-PG live-apply gate (superuser url — provisions wamn_app,
# skips when unset):
docker run -d --rm --name wamn-dm1-pg -p 5463:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_DM1_PG_URL=postgres://postgres:postgres@127.0.0.1:5463/wamn cargo test -p wamn-dm1
docker stop wamn-dm1-pg
# NOTHING in-cluster (a catalog + schema deliverable, the migrate/rls/seed
```

### [CALLABLE-FLOWS-POC-F1 / wamn-5wd1.57] receipt-received r6

Docs: docs/archive/poc/poc-f1.md

```bash
# recipe-test: H5-CALLABLE-F1 | system | wamn-proof-system | lib | - | callable_f1::tests:: | 6 | tests/system/src/callable_f1.rs F1 release, direct pure nodes, deterministic CTE recovery, refusals, and webhook cutover
cargo test --locked -p wamn-proof-system --lib callable_f1::tests::
cargo test --locked --manifest-path components/Cargo.toml \
  -p normalize-receipt -p evaluate-specs
cargo test --locked --manifest-path components/Cargo.toml -p flowrunner \
  f1_supplied_node_types_dispatch_through_the_custom_abi
cargo test --locked -p wamn-proof-conformance --lib \
  docker_component_provenance::every_embedded_component_comes_from_the_locked_builder

docker build --target gates -t wamn-gates:cf-f1-<commit> .
kind load docker-image wamn-gates:cf-f1-<commit> --name wamn
kubectl -n wamn-system delete job callable-flow-f1 --ignore-not-found
sed "s/wamn-gates:cf-f1-ISSUE/wamn-gates:cf-f1-<commit>/" \
  deploy/gates/callable-flow-f1-job.yaml | kubectl -n wamn-system apply -f -
kubectl -n wamn-system wait --for=condition=complete \
  job/callable-flow-f1 --timeout=300s
kubectl -n wamn-system logs job/callable-flow-f1
```

### [CALLABLE-FLOWS-POC-W1 / wamn-5wd1.9] composed F0/F1/F3 campaign

The Wave-1 gate applies the promoted POC catalog from zero, runs the production
invocation provider and the F0/F1/F3 proofs, checks T-CTX/T-NR, and emits one
gate evidence record binding the exact source, image, supplied component bytes,
POC config, schema, release inputs, and deployment identity. Replace `<commit>`
in all three positions from the same `git rev-parse HEAD`.

```bash
# recipe-test: H5-CALLABLE-WAVE1 | system | wamn-proof-system | lib | - | callable_wave1::tests:: | 4 | tests/system/src/callable_wave1.rs composed F0/F1/F3 identities and T-CTX/T-NR contracts
cargo test --locked -p wamn-proof-system --lib callable_wave1::tests::
cargo test --locked -p wamn-proof-conformance -p wamn-proof-integration -p wamn-proof-system

docker build --target gates -t wamn-gates:cf-wave1-<commit> .
kind load docker-image wamn-gates:cf-wave1-<commit> --name wamn
kubectl -n wamn-system delete job callable-flow-wave1 --ignore-not-found
sed "s/ISSUE/<commit>/g" deploy/gates/callable-flow-wave1-job.yaml | \
  kubectl -n wamn-system apply -f -
kubectl -n wamn-system wait --for=condition=complete \
  job/callable-flow-wave1 --timeout=600s
kubectl -n wamn-system logs job/callable-flow-wave1
```

### [CALLABLE-FLOWS-POC-W2 / wamn-5wd1.10] composed F0-F4 campaign

The serial Wave-2 gate reuses the Wave-1 from-zero schema and production
invocation campaign, then composes the F2/F4 contract and recovery proofs. Its
gate evidence record binds the source commit, exact image tag and Kubernetes-
observed image ID, flowrunner and all three supplied custom-node components, POC configuration
and schema, all five graph definitions, each attachment/registration input,
release membership, deployment identity, and the four T5 measurement-hook
shapes. The recorded T5 hooks deliberately carry no Phase-6 budgets.

```bash
# recipe-test: H5-CALLABLE-WAVE2 | system | wamn-proof-system | lib | - | callable_wave2::tests:: | 4 | tests/system/src/callable_wave2.rs F0-F4 identity evidence, mixed-identity refusal, T5 hooks, and exact-image routing
CARGO_TARGET_DIR=/tmp/wamn-target-wave2-10 \
  cargo test --locked -p wamn-proof-system --lib callable_wave2::tests::
CARGO_TARGET_DIR=/tmp/wamn-target-wave2-10 \
  cargo test --locked -p wamn-proof-conformance -p wamn-proof-integration \
    -p wamn-proof-system

commit="$(git rev-parse HEAD)"
tag="wamn-gates:cf-wave2-${commit}"
docker build --target gates -t "${tag}" .
kind load docker-image "${tag}" --name wamn
# Resolve IMAGE_ID from the common sha256 digest after @ in every kind node's
# `crictl inspecti "${tag}"` repoDigests entry.
image_id=<kind-observed-image-id>
kubectl -n wamn-system delete job callable-flow-wave2 --ignore-not-found
sed -e "s/ISSUE/${commit}/g" -e "s/IMAGE_ID/${image_id}/g" \
  deploy/gates/callable-flow-wave2-job.yaml > /tmp/callable-flow-wave2-job.yaml
kubectl -n wamn-system apply -f /tmp/callable-flow-wave2-job.yaml
kubectl -n wamn-system wait --for=condition=complete \
  job/callable-flow-wave2 --timeout=600s
kubectl -n wamn-system logs job/callable-flow-wave2
kubectl -n wamn-system get pod -l app=callable-flow-wave2 \
  -o jsonpath='{.items[0].status.containerStatuses[0].imageID}{"\n"}'
```

The PLAN-0.2 mutation campaign for the callable-flow family is
`tools/gate-mutants/callable-flow-aggregate.sh`. It overlays the debug
`wamn-gates` executable on the Dockerfile-owned gates image, loads the exact
image into kind, and drives fresh Jobs through `tools/kubernetes-gate-run`.
Each F0-F4 mutant runs both its direct Job and the Wave-1 or Wave-2 aggregate
that claims it; schema, cron, `f2invoke`, `f3proof`, `f4proof`, and both Wave
identity refusals run at their deployed boundary. Expected-negative Jobs use
`tools/gate-mutants/callable-flow-state-probe.sh` before and after execution
and require an identical state digest. The runner accepts fixed argv only,
restores every mutation byte-exactly, removes each case's Jobs and exact local
and kind tag/import-digest image references before advancing, and records typed evidence in
`architecture/evidence/mutations/callable-flow-aggregate.json`.

```bash
CARGO_TARGET_DIR=/tmp/wamn-target-2jdm-5-4 \
  tools/gate-mutants/callable-flow-aggregate.sh check
CARGO_TARGET_DIR=/tmp/wamn-target-2jdm-5-4 \
  tools/gate-mutants/callable-flow-aggregate.sh green-all
CARGO_TARGET_DIR=/tmp/wamn-target-2jdm-5-4 \
  tools/gate-mutants/callable-flow-aggregate.sh run-all
cargo test --locked -p wamn-proof-conformance --test gate_mutation_evidence
```

### [CF-TIMESHIFT / wamn-5wd1.41] deterministic RFC3339 time-shift component

`time-shift` is a pure, zero-import custom-node component. Its `base` config
names an RFC3339 string field, `offset-ms` is a checked signed integer, and its
single output is canonical UTC RFC3339. The named tests pin F3's 48-hour shift,
timezone normalization, malformed input refusal, and both four-digit-year
boundaries.

```bash
CARGO_TARGET_DIR=/tmp/wamn-target-cf-timeshift-41 \
  cargo test --locked --manifest-path components/Cargo.toml -p time-shift
CARGO_TARGET_DIR=/tmp/wamn-target-cf-timeshift-41 \
  cargo build --locked --manifest-path components/Cargo.toml -p time-shift \
  --target wasm32-wasip2
```

### [POC-F4 / wamn-2jdm.26] disposition-recorded CDC row-event flow + 429 throttle

Docs: docs/archive/poc/poc-material-receiving.md §F4. The `f4proof` gate is the F4
end-to-end proof AND the **EVT-CUTOVER regression by construction**: it is the
first gate to drive the WHOLE event-plane arc — REAL reader (`run_with_token`)
→ REAL materializer guest → run queue → REAL production runner (`ExecutionHost` +
`flowrunner.wasm`) → serve-node hosting the SHIPPED `disposition-node.wasm` →
ERP callback — from a single real WAL insert, over a throwaway
`wal_level=logical` Postgres + throwaway JetStream (rie2ebench substrate). ONE
`INSERT INTO dispositions` is the sole stimulus.

Authority is deliberately split. The graph declares exactly ONE portable HTTP
connection, `erp-callback`, and its callback node names only the relative
`/dispositions` path. The connection HTTP effect resolves the environment-owned
binding and stamps the dispatch's stable key
(`{run_id}:{node}:{occurrence}`, stable across retries) as the system-owned
`Idempotency-Key` header. The flow cannot supply or override that header and has
no flow-level `allowed-hosts`.

The custom-node hop is platform transport, not a portable connection. The guest
passes an admitted implementation digest and one-frame invocation context through
the node-invocation capability; its interface has no endpoint or URL. The trusted
host verifies the digest against the admitted release before consulting an
environment-owned digest-to-endpoint placement map, then signs the existing
node-host request. The fixture runs serve-node with signing required. Admission,
placement, and missing-key refusals happen before network access; a remote
signature refusal is the typed result of the real signed hop and occurs before
node execution or grant installation. Transport failure is an infrastructure
fault, while a response from the node carrying a node fault is a node failure.
Custom-node config containing `endpoint` or any absolute URL is invalid.

Three mechanics rest on the live gate: (1) the **idempotency-key** is injected by
the connection HTTP effect from the system stable key; it is dispatch mechanics,
not input-templated or flow-configured. (2) **THROTTLE v0 = the queue-park property**:
a 429 → `rate-limited` → the run PARKS (`available_at` pushed by `Retry-After`,
lease released) and is NOT re-claimed before the wake; N concurrent 429'd runs
each park with ONE claim, no thundering re-claim. The inert cross-run
`ThrottleTable` is deferred (wamn-lxk.throttle). No park-side jitter: the gate
shows synchronized wake produces NO duplicate ERP posts (idempotency + one-run
completion per key). (3) the **ERP simulator** — a separate `erp-sim` subcommand
(429 + `Retry-After` for the first `--fail-first-n` requests per idempotency
key, then 202; the exactly-once witness; `GET /audit`), distinct from serve-echo
so no always-200 consumer regresses.

Insert-only registration ⇒ NO REPLICA IDENTITY FULL (the RI reconcile is a
no-op, asserted). Redelivery leg: delete the durable consumer, re-run — ZERO new
runs (ON CONFLICT DO NOTHING). Zero-residue teardown (slot/db/role/stream).

```bash
# Focused debug proofs: capability shape, host admission/placement, portable
# graph validation, and the shared F4 fixture.
CARGO_TARGET_DIR=/tmp/wamn-target-2jdm-26 CARGO_INCREMENTAL=0 \
  cargo test --locked -p wamn-runtime --lib plugins::node_invocation::tests::
CARGO_TARGET_DIR=/tmp/wamn-target-2jdm-26 CARGO_INCREMENTAL=0 \
  cargo test --locked -p wamn-runtime --test node_invocation_wit_coherence
CARGO_TARGET_DIR=/tmp/wamn-target-2jdm-26 CARGO_INCREMENTAL=0 \
  cargo test --locked -p wamn-catalog --test identity \
    supplied_node_config_refuses_platform_transport_addresses
CARGO_TARGET_DIR=/tmp/wamn-target-2jdm-26 CARGO_INCREMENTAL=0 \
  cargo test --locked -p wamn-flow --test flows
CARGO_TARGET_DIR=/tmp/wamn-target-2jdm-26 CARGO_INCREMENTAL=0 \
  cargo test --locked -p wamn-proof-integration --lib f4proof::tests::
CARGO_TARGET_DIR=/tmp/wamn-target-2jdm-26 CARGO_INCREMENTAL=0 \
  cargo build --locked -p wamn-gates -p wamn-ctl -p wamn-cdc-reader
CARGO_TARGET_DIR=/tmp/wamn-target-2jdm-26 CARGO_INCREMENTAL=0 \
  cargo build --locked --manifest-path components/Cargo.toml \
    --target wasm32-wasip2 -p flowrunner -p materializer -p disposition-node \
    -p node-cred

# Local gate of record — REAL reader + materializer + runner + serve-node + ERP sim,
# throwaway wal_level=logical PG + throwaway JetStream:
docker run -d --name lane-f4-pg -p 5464:5432 -e POSTGRES_PASSWORD=postgres postgres:18 \
  -c wal_level=logical -c max_replication_slots=10 -c max_wal_senders=10
docker run -d --name lane-f4-nats -p 4232:4222 nats:2.10 -js
until docker exec lane-f4-pg pg_isready -U postgres; do sleep 1; done
until docker logs lane-f4-nats 2>&1 | grep -q 'Server is ready'; do sleep 1; done
# The gate bootstraps wamn_system + wamn_app itself but NOT the host-only author
# role that run-state.sql / catalog-schema.sql GRANT to; create it up front or the
# provisioning step dies `role "wamn_scenario_author" does not exist`.
docker exec lane-f4-pg psql -U postgres -c \
  "CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
     NOINHERIT NOREPLICATION NOBYPASSRLS;"
DBG=/tmp/wamn-target-2jdm-26/wasm32-wasip2/debug
export WAMN_CTL_BIN=/tmp/wamn-target-2jdm-26/debug/wamn-ctl
export WAMN_CDC_READER_BIN=/tmp/wamn-target-2jdm-26/debug/wamn-cdc-reader
/tmp/wamn-target-2jdm-26/debug/wamn-gates --log-level error f4proof \
  --component $DBG/materializer.wasm --flowrunner $DBG/flowrunner.wasm \
  --node $DBG/disposition_node.wasm \
  --admin-database-url postgres://postgres:postgres@127.0.0.1:5464/postgres \
  --nats-url nats://127.0.0.1:4232
docker rm -f lane-f4-pg lane-f4-nats
# In-cluster: the serve-node + ERP sim run IN-PROCESS on loopback (no external
# Service, no platform runner.yaml allowed-hosts change). Needs the fixture
# Postgres at wal_level=logical + the data-plane evt-nats:
kubectl -n wamn-system apply -f deploy/gates/f4proof-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/f4proof --timeout=600s
kubectl -n wamn-system logs job/f4proof
```

Mutation proofs (debug builds; each command is an expected-green negative gate):

```bash
# M1 — the ordinary nodeinvoke gate includes a REAL wrong-key host against its
# signing-required serve-node. Require the named AUTHN-MISMATCH-INFRASTRUCTURE,
# AUTHN-MISMATCH-PLANE, AUTHN-MISMATCH-RECOVERY, and
# AUTHN-MISMATCH-VERIFY-BEFORE-GRANT checks.
docker run -d --name lane-nodeinvoke-mutant-pg -p 5463:5432 \
  -e POSTGRES_PASSWORD=postgres postgres:18
until docker exec lane-nodeinvoke-mutant-pg pg_isready -U postgres; do sleep 1; done
docker exec lane-nodeinvoke-mutant-pg psql -U postgres -c \
  "CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS;"
/tmp/wamn-target-2jdm-26/debug/wamn-gates --log-level error nodeinvoke \
  --flowrunner /tmp/wamn-target-2jdm-26/wasm32-wasip2/debug/flowrunner.wasm \
  --node-cred /tmp/wamn-target-2jdm-26/wasm32-wasip2/debug/node_cred.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5463/postgres \
  --admin-database-url postgres://postgres:postgres@127.0.0.1:5463/postgres \
  --node-port 8091 --iters 1
docker rm -f lane-nodeinvoke-mutant-pg

# M2 — omit the environment binding. Require CALLBACK-BINDING-DENIAL's typed
# unbound verdict and its independent "ERP observed zero requests" check.
docker run -d --name lane-f4-mutant-pg -p 5464:5432 \
  -e POSTGRES_PASSWORD=postgres postgres:18 -c wal_level=logical \
  -c max_replication_slots=10 -c max_wal_senders=10
docker run -d --name lane-f4-mutant-nats -p 4232:4222 nats:2.10 -js
until docker exec lane-f4-mutant-pg pg_isready -U postgres; do sleep 1; done
until docker logs lane-f4-mutant-nats 2>&1 | grep -q 'Server is ready'; do sleep 1; done
export WAMN_CTL_BIN=/tmp/wamn-target-2jdm-26/debug/wamn-ctl
export WAMN_CDC_READER_BIN=/tmp/wamn-target-2jdm-26/debug/wamn-cdc-reader
/tmp/wamn-target-2jdm-26/debug/wamn-gates --log-level error f4proof \
  --component /tmp/wamn-target-2jdm-26/wasm32-wasip2/debug/materializer.wasm \
  --flowrunner /tmp/wamn-target-2jdm-26/wasm32-wasip2/debug/flowrunner.wasm \
  --node /tmp/wamn-target-2jdm-26/wasm32-wasip2/debug/disposition_node.wasm \
  --admin-database-url postgres://postgres:postgres@127.0.0.1:5464/postgres \
  --nats-url nats://127.0.0.1:4232 --mutant-deny-callback-binding
docker rm -f lane-f4-mutant-pg lane-f4-mutant-nats

# M3 — both a custom-node `endpoint` key and an absolute URL value must receive
# the typed custom-node-has-platform-transport validation refusal.
CARGO_TARGET_DIR=/tmp/wamn-target-2jdm-26 CARGO_INCREMENTAL=0 \
  cargo test --locked -p wamn-catalog --test identity \
    supplied_node_config_refuses_platform_transport_addresses
```

### [EVT-REPLICA-IDENT / wamn-l5i9.31] per-entity REPLICA IDENTITY FULL reconciler

Docs: docs/archive/events/event-plane-jetstream.md §5 ("Old images") + docs/archive/platform/provisioning.md
(`reconcile-replica-identity`). `REPLICA IDENTITY FULL` is a platform-managed
per-entity knob (l5i9.1 decision d): an entity runs FULL only when a registered
row-event needs the OLD image — any registration whose condition reads root
`old` ("changed-to") OR that subscribes to `delete` — and DEFAULT (pkey-only)
everywhere else keeps WAL minimal (the global default is never flipped). The
pure decision + SQL builders live in `wamn-schema-control`
(`reconcile_replica_identity`, `alter_replica_identity_sql`,
`select_replica_identity_sql`); the root-`old` detection is the SINGLE
`wamn_event_reg` detector the materializer's per-event old-absent guard also
keys on. The `wamn-ctl reconcile-replica-identity` verb reads the catalog's
registrations across ALL tenants + each table's `pg_class.relreplident`, plans
the idempotent flips, and (unless `--dry-run`) runs `ALTER TABLE … REPLICA
IDENTITY FULL|DEFAULT` as a superuser (ALTER needs table ownership). The flip is
**NON-RETROACTIVE**: it enriches only WAL written after it, and the materializer
refuses an absent old image (`old-image-absent`, alertable) rather than evaluate
`old` as null.

```bash
cargo test -p wamn-event-reg -p wamn-materializer   # one root-old detector + the per-event old-absent guard + delete-under-FULL fires
cargo test -p wamn-schema-control                          # reconciler derivation (old-cond/delete-op/cross-tenant union/none-required→DEFAULT) + SQL pins
cargo clippy -p wamn-schema-control -p wamn-ctl --all-targets
# Live gate (throwaway wal_level=logical PG18): drives the REAL reconcile path —
# a registration on an entity flips its table 'd'->'f' live (pg_class.relreplident),
# an unrelated entity stays 'd', removing the registrations flips back 'f'->'d',
# and a reconcile at target is a no-op; then a test_decoding slot proves the WAL
# truth NON-RETROACTIVELY: under DEFAULT an UPDATE carries no old image and a
# DELETE's old image is the pkey only (no tenant_id); after the flip an UPDATE
# carries the old image and a DELETE's old image carries tenant_id.
docker run -d --name wamn-ri-pg -p 5462:5432 -e POSTGRES_PASSWORD=postgres \
  postgres:18 -c wal_level=logical -c fsync=off -c synchronous_commit=off
WAMN_CTL_PG_URL=postgres://postgres:postgres@127.0.0.1:5462/postgres \
  cargo test -p wamn-ctl --test replica_identity_live -- --nocapture
docker rm -f wamn-ri-pg
# Dry-run the verb against a provisioned project DB (prints flips + no-ops):
./target/debug/wamn-ctl reconcile-replica-identity \
  --admin-database-url postgres://postgres:postgres@127.0.0.1:5462/postgres \
  --catalog path/to/applied-catalog.json --schema app --dry-run
# Materializer end-to-end (rebuild the guest — the served old condition + the
# old-image-absent refusal changed): matbench adds an UPDATE carrying a FULL old
# image that evaluates end to end and fires (f-old:evt:8). (The cutbench
# phase-3 RI-flip drill retired with cutbench at l5i9.19; the reconcile verb's
# own live gate is ri_orch_live.) Recipe: [EVT-MAT].
(cd components && cargo build -p materializer --target wasm32-wasip2)
```

Mutation harness: scratchpad `mutate_l5i9_31.py` — M1 the reconciler drops the
delete-op rule (killed by
`replica_identity::tests::a_delete_only_registration_requires_full_even_without_a_condition`),
M2 the materializer guard treats an absent old image as condition-false (killed
by `decide::tests::old_value_conditions_are_serviceable_and_guarded_per_event`),
M3 `alter_replica_identity_sql` emits the wrong keyword (killed by
`replica_identity::tests::alter_and_read_sql_are_pinned`); apply/test/restore
with sha256, DEBUG builds.

### [EVT-RI-ORCH / wamn-l5i9.61] publish/migrate-catalog auto-reconcile REPLICA IDENTITY

Docs: docs/archive/platform/provisioning.md (`reconcile-replica-identity`, "Automatic caller").
Wires the l5i9.31 reconciler into an OPERATIONAL caller: `publish-catalog` and
`migrate-catalog` run the RI reconcile as their last step (they already connect
as the superuser `ALTER … REPLICA IDENTITY` needs), scoped strictly to the
verb's `--schema`, so a catalog apply never leaves an entity that needs the old
image on DEFAULT — a permanent gap, since the flip is non-retroactive.
**Decision:** run reconcile INSIDE the verbs (auto-ALTER), NOT a D24-style
refuse-if-drifted, because the verbs' role can already ALTER and the pass is
idempotent + schema-scoped (no cross-schema blast; the cross-tenant union is only
in WHICH registrations demand FULL, all tables in the one schema). Escape hatch:
`--skip-reconcile-replica-identity`. The registration-change path (writes under
`wamn_app`, which cannot ALTER) is left to the automatic caller + the manual verb
for now; the pure detect surface is `ReplicaIdentityPlan::pending_old_image_gap`
(the entities with an open old-image gap). Shared shell:
`reconcile_replica_identity::reconcile_after_apply`.

```bash
cargo test -p wamn-schema-control --lib   # pending_old_image_gap direction + no-gap-on-reset (+ the l5i9.31 derivation)
cargo clippy -p wamn-schema-control -p wamn-ctl --tests
# Live gate (throwaway PG; plain postgres:18 — the flip sets the pg_class flag,
# no wal_level=logical needed): drives the REAL verbs — publish --provision
# provisions the floor AND flips the needing entity 'd'->'f' (cross-tenant union)
# while the bystander stays 'd'; re-publish is idempotent; --skip-reconcile-
# replica-identity leaves RI as-is; a plain re-publish resets 'f'->'d'; and a
# first-materialization migrate flips the entity to FULL after its apply tx
# commits; and (wamn-l5i9.65 phase) a registration create on an ALREADY-applied
# catalog opens an old-image gap `pending_old_image_gap` detects, which the
# standalone verb run EXACTLY as the periodic CronJob's command line
# (`reconcile_replica_identity::run` = `wamn-ctl reconcile-replica-identity
# --catalog … --schema …`) closes 'd'->'f', a second run being a no-op.
# Hermetic (drops+recreates its schemas):
docker run --rm -d --name wave5-riorch-pg -e POSTGRES_PASSWORD=postgres -p 56011:5432 postgres:18
WAMN_CTL_PG_URL=postgres://postgres:postgres@127.0.0.1:56011/postgres \
  cargo test -p wamn-ctl --test ri_orch_live -- --nocapture
docker rm -f wave5-riorch-pg
```

The registration-change reconcile CronJob (wamn-l5i9.65) is
`deploy/platform/replica-identity-reconcile.example.yaml` (one per project-env);
in-cluster it is `kubectl apply`'d and a tick is forced with `kubectl create job
--from=cronjob/replica-identity-reconcile-poc-f1 …`.

Mutation harness: scratchpad `mutate_l5i9_61.py` — M1 `pending_old_image_gap`
keys on the reset direction (killed by
`replica_identity::tests::pending_old_image_gap_is_the_flip_up_direction_by_entity_id`),
M2 `reconcile_after_apply` plans but never applies the flips (killed by the
`ri_orch_live` live gate), M3 the `--skip-reconcile-replica-identity` escape hatch
inverted in publish-catalog (killed by the `ri_orch_live` live gate);
apply/test/restore with sha256, DEBUG builds.

### [RUN-PLANE-RECONCILE / wamn-1wdq] reconcile-run-plane — the run-plane schema migration verb

Docs: docs/archive/platform/provisioning.md (`reconcile-run-plane`). The durable migration path
for provisioned run-plane schemas: `wamn-ctl reconcile-run-plane --schema <env>`
diffs ONE project-env schema (+ the per-database `catalog` schema) against the
deploy/sql schema of record (embedded `include_str!` — the same source the
wamn-9mg8 stand-in drift guard pins) and applies the idempotent, data-preserving plan:
create-missing tables from record sections, `ADD COLUMN` for record columns a
present table lacks, index create/recreate (the pre-E4 claimable index), the
exact canonical CHECK/FK/user-trigger/helper-function apparatus, the five
immutable effect ledgers, catalog publication provenance, the locked legacy
attempt/dispatch/outcome backfill plus `node_runs.current_effect_attempt_id`
advance, the pre-l5i9.19 outbox-era teardown, and catalog-schema from-zero.
No live column or non-legacy table is dropped, and no row in a retained table
is deleted; the named migration steps may fill defaults, strip legacy
registration state, append immutable facts, and update the current pointer.
PostgreSQL rejects incompatible
canonical constraints or incomplete legacy authority instead of guessing. Pure planner
`wamn_schema_control::plan_run_plane` (crates/schema/control/src/run_plane.rs); thin
shell `wamn_ctl::reconcile_run_plane` (shared `reconcile()` drives the CLI and
the gate). Observation and apply require a `SUPERUSER` or `BYPASSRLS` current
role (plus DDL ownership/privileges for apply); a plain forced-RLS table owner
is refused. `--dry-run` is strictly read-only after that visibility preflight.
One-shot Job template:
`deploy/platform/run-plane-reconcile.example.yaml`.

```bash
cargo test -p wamn-schema-control run_plane   # record parse pins + planner (no-op-at-record self-consistency, drift/from-zero/queue-missing plans)
cargo clippy -p wamn-schema-control -p wamn-ctl --all-targets
# Live-apply matrix (throwaway PG; plain postgres:18 — no wal_level needed).
# Eleven hermetic legs: shared-runner legacy; legacy effect-attempt backfill;
# forced-RLS owner refusal; v1-era drift; queue-missing; from-zero; current
# no-op; authoring additive upgrade/authority repair; catalog-head SHARE-lock
# concurrency; effect-disposition security drift; and fail_kind CHECK drift.
docker run --rm -d --name wamn-1wdq-pg -e POSTGRES_PASSWORD=pg -p 55461:5432 postgres:18
WAMN_CTL_PG_URL=postgres://postgres:pg@localhost:55461/postgres \
  cargo test -p wamn-ctl --test run_plane_live -- --nocapture
docker rm -f wamn-1wdq-pg
```

The live negative matrix is part of that single entry and must remain exact:

- a plain forced-RLS owner is refused for both dry-run and apply, even with a
  forged `pg_temp.pg_roles`; stale `wamn_app` INSERT grants, legacy platform
  membership, and the same temp-shadow cannot authorize attempt or disposition
  appends;
- NULL-incomplete legacy authority, an attested attempt whose pinned graph has
  no connection identity, and a join-lost legacy candidate each abort with
  their typed refusal; the append path rolls back and leaves the pointer FK
  inactive;
- a cross-run current pointer and cross-occurrence predecessor are rejected by
  their composite FKs;
- dispatch-before-start, outcome-without-the-exact-dispatch, and
  outcome-before-dispatch are rejected by the time/FK constraints;
- `system` and `project-deployer` resolve requests, a NULL resolution status,
  failure detail without a string message, and a duplicate global
  `append_ordinal` are rejected at storage; and
- UPDATE and DELETE are refused on all five immutable ledgers. Drift mutants
  additionally require repair of the closed outcome CHECK, lineage FK,
  disabled insert guard, trusted helper, and all three disposition uniqueness
  indexes before the second reconcile can report no-op.

In-cluster gate of record: rebake `wamn-ctl:dev` (`docker build --target ctl`),
kind load, then apply `deploy/platform/run-plane-reconcile.example.yaml` to
`wamn_runner_demo`. The first run upgrades the deployed legacy fixture; verify
its existing run/node row counts, the deployed runner replicas, and the helper
lock/lineage apparatus. Recreate the Job and require the second run to report
the no-op ("run plane already at the schema of record").

Mutation harness: `tools/gate-mutants/run-plane-canonical.sh` — checked-in
exact-hash mutants independently remove CHECK planning, helper/trigger repair,
and effect execution; the named planner/live gates must turn red, then the trap
restores and verifies the original hashes. Typed gate evidence lives under
`architecture/evidence/mutations/`; DEBUG builds only.

### [EVT-C-E2E / wamn-l5i9.22] e2ebench — RETIRED (l5i9.19 teardown)

The C-E2E campaign of record stands in docs/archive/results/ceilings.md § C-E2E +
docs/results/ceilings-data/ (ce2e-*.csv): the one before/after chart (commit→run-start
distribution, fan-out 1→N, 10× burst — outbox vs CDC at identical load). It
ran BEFORE the teardown by design (the measure-first ordering); the bench and
deploy/gates/e2ebench-job.yaml were deleted with the old path (D19 v3 §3,
executed 2026-07-20) — a before/after against a deleted path cannot be
re-measured, so the record is final. CDC-path regression coverage continues in
[EVT-MAT] (matbench) and [E10-E2E] (samplebench).

### [NODE-INVOKE / wamn-bd5 + wamn-2jdm.26] production runner ↔ custom-node invocation (5.6)

Docs: docs/archive/platform-plan.md §5.6, docs/archive/contracts/wamn-node.wit, docs/archive/results/p0-results.md §S4.
Current dispatch keeps placement and transport behind the trusted runner host.
The flowrunner's `custom` arm sends the admitted node identity, a one-frame
invocation context, and opaque request bytes through the internal
`wamn:runner/node-invocation` capability. Its guest-visible type contains no
endpoint, URL, authority, or signing key. The host rechecks the claimed
implementation digest against the admitted release/artifact and exact attempt,
then resolves an environment-owned `digest → authority` placement. Only after
those checks does it encode and sign the existing `wamn-node-invoke` envelope
and POST it to a `serve-node` host running the component under the REAL frozen
`wamn:node` world.

The host refuses invalid context, unadmitted identity/grant/config, missing
placement, and unavailable signing before transport. Signing refusal and
transport failure remain outer infrastructure faults; only a response authored
by the node can become a node failure. The serve-node verifies the signature
before installing the admitted GET-ONLY grant. A node cannot self-grant — it
never links `wamn:runner/credentials` — and credential resolution stays scoped
to the serve-node's OWN `--project` (never the request). The E17 tenant import
allowlist is screened at load (a node importing `wamn:postgres`, `wasi:sockets`,
or `wamn:runner` is refused).

```bash
# Pure/runtime tests: envelope/authentication, node runtime, host admission and
# placement, guest-visible WIT shape, and the descriptor-only public surface.
CARGO_TARGET_DIR=/tmp/wamn-target-2jdm-26 CARGO_INCREMENTAL=0 \
  cargo test --locked -p wamn-node-invoke -p wamn-node-runtime
CARGO_TARGET_DIR=/tmp/wamn-target-2jdm-26 CARGO_INCREMENTAL=0 \
  cargo test --locked -p wamn-runtime --lib plugins::node_invocation::tests::
CARGO_TARGET_DIR=/tmp/wamn-target-2jdm-26 CARGO_INCREMENTAL=0 \
  cargo test --locked -p wamn-runtime --test node_invocation_wit_coherence
CARGO_TARGET_DIR=/tmp/wamn-target-2jdm-26 CARGO_INCREMENTAL=0 \
  cargo test --locked -p wamn-standard-nodes \
    public_resolution_surface_is_descriptor_only
CARGO_TARGET_DIR=/tmp/wamn-target-2jdm-26 CARGO_INCREMENTAL=0 \
  cargo build --locked -p wamn-node-host -p wamn-gates

# Debug guest + node builds; node-cred reads only its admitted node credential.
CARGO_TARGET_DIR=/tmp/wamn-target-2jdm-26 CARGO_INCREMENTAL=0 \
  cargo build --locked --manifest-path components/Cargo.toml \
    --target wasm32-wasip2 -p flowrunner -p node-cred

# Local live gate — the WHOLE path on ONE task (real ExecutionHost -> internal
# node-invocation capability -> host admission/placement/signing -> real
# serve-node -> node-cred): payload round-trip, the declared credential readable,
# an UNDECLARED credential not-granted, and config parsed once across N runs.
# Throwaway PG (wamn-bd5-pg on 5463) with a wamn_app role; NATS is optional.
docker run -d --name wamn-bd5-pg -e POSTGRES_PASSWORD=postgres -p 5463:5432 \
  postgres:18 -c fsync=off -c synchronous_commit=off
docker exec wamn-bd5-pg psql -U postgres -c \
  "CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS;"
/tmp/wamn-target-2jdm-26/debug/wamn-gates --log-level error nodeinvoke \
  --flowrunner /tmp/wamn-target-2jdm-26/wasm32-wasip2/debug/flowrunner.wasm \
  --node-cred /tmp/wamn-target-2jdm-26/wasm32-wasip2/debug/node_cred.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5463/postgres \
  --admin-database-url postgres://postgres:postgres@127.0.0.1:5463/postgres \
  --node-port 8091 --iters 12
docker rm -f wamn-bd5-pg
# Mutation harness (3 mutants, each must exit non-zero): scratchpad mutate_bd5.py
#   (a) grant widened beyond the declared set; (b) config cache never invalidated;
#   (c) the pub runnable wamn_nodes::node leak restored.

# In-cluster (the main loop runs this — image rebake riders):
docker build --target node-host -t wamn-node-host:dev . && docker build --target gates -t wamn-gates:dev . \
  && kind load docker-image wamn-node-host:dev --name wamn && kind load docker-image wamn-gates:dev --name wamn
# The custom node ships as a ConfigMap (v0; the OCI image-fetch sidecar is a
# deferral). serve-node runs from the dedicated wamn-node-host image:
kubectl -n wamn-system create configmap wamn-custom-node \
  --from-file=node.wasm=/tmp/wamn-target-2jdm-26/wasm32-wasip2/debug/node_cred.wasm
kubectl -n wamn-system apply -f deploy/platform/serve-node.yaml
kubectl -n wamn-system rollout status deploy/serve-node --timeout=120s
# Populate deploy/platform/runner-node-placements.example.yaml with the admitted
# component's exact sha256 digest -> serve-node Service authority, then apply it
# before deploy/platform/runner.yaml. The flow contains neither that authority
# nor an endpoint/URL/allowed-hosts entry; an empty placement map fails closed.
```

### [NODE-INVOKE-AUTHN / wamn-fqg.22] runner ↔ node authn — signed envelope

Docs: docs/archive/platform-plan.md §5.6, deploy/platform/serve-node.yaml + runner-credentials.example.yaml.
wamn-fqg.22 added a **SIGNED INVOCATION ENVELOPE**: a per-project-env HMAC-SHA256
over the EXACT request body bytes, carried in `x-wamn-signature`. wamn-2jdm.26
moves the signer out of the guest and into the trusted node-invocation host
plugin. The canonical signed bytes remain in the pure `wamn-node-invoke` crate
(`sign_envelope` / `verify_envelope`, hmac's constant-time `verify_slice`), shared
by the trusted runner host and serve-node verifier so the transport cannot drift.

The reserved runner-host vault entry is `wamn:node-invoke-signing-key`; the
serve-node has the corresponding environment key in its host-side vault. The
guest cannot read either key. After admission and digest placement, the runner
host signs; the serve-node VERIFIES **before installing the grant**
(`ServeNode::verify_signature` precedes `invoke`). Missing, malformed, or wrong
signatures produce a 401-class refusal
(`{"error":"invocation-unauthorized","reason":...}`) that never reaches the
node. The runner-host plugin exposes that signing refusal as a typed outer
infrastructure fault rather than manufacturing a node failure. Replay freshness
remains controlled by the fqg.32 timestamp policy; mTLS is the later infra
upgrade.

The `nodeinvoke` gate (same command as above) now also asserts, on top of
DELIVERY/GRANT/NOT-GRANTED/MEMOIZED: AUTHN-POSITIVE (the real signed hop drains N
runs against a keyed serve-node + grant-install count advances), AUTHN-UNSIGNED /
AUTHN-TAMPERED / AUTHN-WRONG-KEY (raw POSTs → 401 with the reason class),
AUTHN-NO-ORACLE (a refusal never echoes the expected MAC), VERIFY-BEFORE-GRANT
(no refused request advanced `grant_install_count`), and AUTHN-SIGNED (a correct
raw POST is accepted 200 and installs exactly one grant). Its real wrong-key
runner-host phase additionally asserts AUTHN-MISMATCH-INFRASTRUCTURE,
AUTHN-MISMATCH-PLANE, AUTHN-MISMATCH-RECOVERY, and
AUTHN-MISMATCH-VERIFY-BEFORE-GRANT.

```bash
# Unit tests — canonical signing bytes + MAC roundtrip + tamper/wrong-key/malformed:
CARGO_TARGET_DIR=/tmp/wamn-target-2jdm-26 CARGO_INCREMENTAL=0 \
  cargo test --locked -p wamn-node-invoke --lib

# The live gate is the SAME nodeinvoke command as [NODE-INVOKE] above (the gate
# banks corresponding keys in both host vaults; the trusted runner host signs,
# and serve-node verifies). Rebuild the debug guest + wamn-gates first:
CARGO_TARGET_DIR=/tmp/wamn-target-2jdm-26 CARGO_INCREMENTAL=0 \
  cargo build --locked --manifest-path components/Cargo.toml \
    --target wasm32-wasip2 -p flowrunner -p node-cred
CARGO_TARGET_DIR=/tmp/wamn-target-2jdm-26 CARGO_INCREMENTAL=0 \
  cargo build --locked -p wamn-gates --bin wamn-gates
# then run the nodeinvoke command from [NODE-INVOKE / wamn-bd5].

# Mutation harness (3 mutants, each must exit non-zero): scratchpad mutate_fqg22.py
#   (a) serve-node DROPS verify-before-grant (verify_signature call removed);
#   (b) verify_envelope compare always Ok (skip constant-time / accept any MAC);
#   (c) the trusted invocation host signs the WRONG bytes (empty body instead of
#       the envelope).
# Killers: (a) VERIFY-BEFORE-GRANT + AUTHN-UNSIGNED; (b) AUTHN-WRONG-KEY +
#   a_wrong_key_signature_is_mismatch; (c) AUTHN-POSITIVE (DELIVERY) + the
#   signed_envelope unit roundtrip.
```

In-cluster gate of record (the MAIN LOOP runs this after integration): the
signing key must be present in both the trusted runner-host and serve-node vaults
(see runner-credentials.example.yaml), then rebake the host image + gates image
and re-run nodeinvoke against the deployed serve-node (the rebake riders under
[NODE-INVOKE / wamn-bd5]).

### [NODE-INVOKE-HARDENING / wamn-fqg.29·.31·.30·.32] authn follow-ups

Docs: docs/archive/platform-plan.md §5.6, deploy/platform/serve-node.yaml + runner-credentials.example.yaml.
Four surgical hardenings on the fqg.22 signed-envelope path, all asserted by the
SAME `nodeinvoke` gate (extra checks on top of the fqg.22 authn set):

* **fqg.29 — terminal on refusal (superseded attribution).** fqg.29 stopped a
  persistent 401 from consuming the retry budget when the guest owned transport.
  Under wamn-2jdm.26 the guest no longer receives HTTP status: a wrong runner-host
  key is `SigningRefused`, an outer infrastructure fault. Gate:
  `AUTHN-MISMATCH-INFRASTRUCTURE` + `AUTHN-MISMATCH-PLANE` prove the run stays
  running with no node failure verdict; `AUTHN-MISMATCH-RECOVERY` proves the
  started attempt and lease remain available to infrastructure recovery.

* **fqg.31 — fail-closed toggle.** New serve-node flag `--require-signing-key`
  (env `WAMN_REQUIRE_SIGNING_KEY`): when set and NO signing key is configured for
  the project, REFUSE ALL invocations (401 `signing-key-required`) instead of
  silently reverting to network trust. Default OFF stays backward-compatible
  (unkeyed = network-trust + loud warning). Gate: `FAIL-CLOSED` (a keyless
  require host refuses both an unsigned AND a signed POST) + `NETWORK-TRUST` (the
  default keyless host admits an unsigned POST) — both via `verify_signature`.

* **fqg.30 — dual-key rotation window.** A second reserved vault name
  `wamn:node-invoke-signing-key-previous` holds the OLD key; the serve-node
  accepts a signature under the current OR the previous key, so an env's key
  rotates with no serve-node restart (the trusted runner host always signs with
  the current key). A second NAME, not a delimited value, keeps the
  `{project:{name:secret}}` shape. Gate: `DUAL-KEY` — a previous-key signature
  verifies, the current key still verifies, garbage is still `bad-signature`.

* **fqg.32 — replay freshness (opt-in).** An additive signed timestamp: the
  trusted runner host stamps `x-wamn-timestamp` (unix seconds) folded into the
  HMAC bytes (version-safe — no timestamp ⇒ byte-identical to fqg.22). New serve-node flag
  `--signature-max-age-secs` (env `WAMN_SIGNATURE_MAX_AGE_SECS`), OFF by default
  (replay-within-project-env stays the documented accepted risk): when set, a
  signed IN-WINDOW timestamp is required, checked AFTER the MAC (never a freshness
  oracle). Gate: `FRESHNESS-FRESH` (fresh accepted when enforced),
  `FRESHNESS-STALE` (a signed-but-stale envelope → `stale-timestamp`),
  `FRESHNESS-LEGACY` (a timestamp-less envelope still verifies when OFF).

The live gate is the SAME `nodeinvoke` command as [NODE-INVOKE / wamn-bd5];
rebuild the debug flowrunner guest + trusted runtime/gates host first. Mutation
harness: scratchpad `mutate_lane_a.py`
(≥3 mutants; each must fail a NAMED gate check / unit test).

### [R24 / wamn-03m + wamn-cjv.10 + wamn-2jkm.42] per-visit occurrence — merge/loop history + resume

Docs: docs/archive/execution/run-state.md (branch-aware replay — the occurrence paragraph)

The engine computes `Dispatch::occurrence` (prior COMPLETED visits of the node
in the run); both guests bind it into the `node_runs` insert builders
(occurrence is `$3`, never a literal 0), so a merge/loop node's N visits
persist N rows and reconstruction replays visit-by-visit.

```bash
cargo test -p wamn-runner    # occurrence semantics + diamond/loop resume (R24 VERIFY)
cargo test -p wamn-run-state # per-visit reconstruction + legacy collapsed-history Mismatch
cargo test -p wamn-run-state # composed-statement arity renumbering ($8/$9, $9/$10)
# live builders (throwaway PG; the queue live script pins replay-no-op vs distinct-visit row):
WAMN_RUN_QUEUE_PG_URL=... WAMN_RUN_STORE_PG_URL=... cargo test -p wamn-run-state
# guests + the gate of record (runnerbench merge-resume: a diamond whose merge is a
# delay node parks between the merge's visits; every re-claim reconstructs — want
# 7 node_runs rows, m/r visits (2,0,1)):
(cd components/execution/flowrunner && cargo build --release --target wasm32-wasip2)
./target/debug/wamn-gates runnerbench --flowrunner components/target/wasm32-wasip2/release/flowrunner.wasm \
  --database-url ... --admin-database-url ...
# regressions: failoverbench (all), flowbench (all), testhostbench (all), f1bench (all).
# mutants (apply/test/restore, each fails a NAMED check): engine occurrence:=0 ->
# merge_visits_carry_distinct_occurrences; builder occurrence:=literal 0 ->
# builders_are_claim_scoped_and_parameterized; success-arm visits bump dropped ->
# merge/diamond/loop tests; guest claim path records 0 -> runnerbench merge-resume (5 rows).

### [S2/D15-durable / wamn-dzhw] fixture pod on durable commits

`deploy/platform/postgres.yaml` runs `fsync=on` + `synchronous_commit=on` since
2026-07-21 (wamn-dzhw; addenda in docs/archive/results/ceilings.md provenance banner +
docs/archive/results/p0-results.md §S2). The pod is EPHEMERAL: any restart (including applying
a knob change) wipes provisioned schemas — restore BEFORE re-running gates:

```bash
# initdb reruns the pg-init ConfigMap (role, wamn DB, s2+s3 fixtures) — keep it
# fresh (wamn-v1pp): kubectl -n wamn-system create configmap pg-init \
#   --from-file=init.sql=deploy/sql/postgres-init.sql --dry-run=client -o yaml | kubectl apply -f -
kubectl -n wamn-system exec -i deploy/postgres -- psql -U postgres -d wamn -f - < deploy/sql/catalog-schema.sql
kubectl -n wamn-system apply -f deploy/platform/run-plane-reconcile.example.yaml   # wamn_runner_demo
# poc_f1: f1-provision-job, then the reconcile Job sed'd to poc_f1 (queue tables)
# gates of record, SEQUENTIAL: pgbench-job, pgbench-multiproject-job, queuebench-job.
```

### [5.5 / wamn-0si] custom-node builder — build Job + buildproof

Subsystem spec: `docs/archive/platform/builder.md`. The builder is its OWN cargo-ful image
(`--target builder-svc`); the build Job runs the whole pipeline (allowlist →
build → 5.5 lint → sign + SBOM → OCI push) on the baked-in `sample-node` fixture,
and `buildproof` verifies the pushed artifact FROM the registry.

```bash
# Lane-local (no cluster): unit + integration gates (allowlist refusal, push vs
# an in-process registry stub, sign/verify, golden deployment manifests, the
# buildproof manifest/signature/SBOM checks).
cargo test -p wamn-builder
# Conformance boundary: buildproof verification units live in the conformance
# library; the gates binary only exposes the deployed subcommand.
# recipe-test: H5-BUILDPROOF | conformance | wamn-proof-conformance | lib | - | buildproof::tests:: | 3 | tests/conformance/src/buildproof.rs manifest, SBOM, and signature verification
cargo test -p wamn-proof-conformance --lib buildproof::tests::
cargo test -p wamn-component-policy                      # 5.5a lint + derived grants
# regen the emission golden files after an intentional shape change:
BLESS=1 cargo test -p wamn-builder --test golden_deploy

# In-cluster gate of record (the registry must be up: deploy/platform/registry.yaml).
# 1. Build BOTH new/changed images + kind load:
docker build --target builder-svc -t wamn-builder:dev .   # NEW, large (cargo + jco)
docker build --target gates       -t wamn-gates:dev .
kind load docker-image wamn-builder:dev --name wamn
kind load docker-image wamn-gates:dev   --name wamn
# 2. Generate the signing keypair + bank the Secret (keep the PUBLIC key):
docker run --rm -v "$PWD:/out" wamn-builder:dev keygen \
  --private-key /out/builder-signing-key.hex --public-key /out/builder-public-key.hex
kubectl -n wamn-system create secret generic wamn-builder-signing-key \
  --from-file=signing-key.hex=builder-signing-key.hex
# 3. Run the build sandbox Job (builds sample-node, signs, pushes wamn/sample-node:dev):
kubectl -n wamn-system apply -f deploy/platform/builder-netpol.yaml   # INERT under kindnetd (deferral)
kubectl -n wamn-system apply -f deploy/platform/builder-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/wamn-builder --timeout=900s
kubectl -n wamn-system logs job/wamn-builder    # expect: allowlist OK, built+linted, pushed signed …
# 4. Run buildproof with the PUBLIC key (edit deploy/gates/buildproof-job.yaml's
#    WAMN_BUILDER_PUBLIC_KEY to $(cat builder-public-key.hex)):
kubectl -n wamn-system apply -f deploy/gates/buildproof-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/buildproof --timeout=180s
kubectl -n wamn-system logs job/buildproof
#   expect PASS lines: wamn.node.manifest valid; layer digest matches;
#   wamn.node.signature verifies; SBOM present (expected packages listed).
# teardown:
kubectl -n wamn-system delete job/wamn-builder job/buildproof
```

Verify against the LIVE cluster FIRST: the `registry:2` pod is `emptyDir`
(EPHEMERAL) — if it restarted, its blobs are gone; re-run the builder Job before
buildproof. `builder-netpol.yaml` does not actually restrict egress under kind
(kindnetd ignores NetworkPolicy).

### [9.9] Dashboards (per-tenant Grafana + SRE)

Docs: docs/archive/observability/dashboards.md

```bash
# Unit tests (dashboards-as-code drift guards: metric names vs docs/archive/observability/metrics.md,
# the checked-in SRE JSON vs the render, tenant->folder uid mapping, base64/auth).
# The implementation belongs to wamn-ctl; wamn-gates only routes dashproof:
# recipe-test: H5-DASHBOARDS | unit | wamn-ctl | lib | - | provision_dashboards::tests:: | 7 | services/ctl/src/provision_dashboards.rs dashboard drift, rendering, tenant, and encoding guards
cargo test -p wamn-ctl --lib provision_dashboards::tests::
# (regenerate the SRE dashboard JSON after a panel change:)
cargo run -p wamn-ctl -- provision-dashboards --emit-sre deploy/infra/grafana/dashboards
# Local iteration: Prometheus + Grafana (SRE dashboards file-provisioned) + a
# throwaway registry Postgres. provision-dashboards creates the per-tenant folders;
# dashproof --local asserts everything (Tempo/Loki health soft-skipped — no
# backends locally; Prometheus is HARD). Images pinned to the k8s manifests.
docker network create wamn-s5 2>/dev/null || true
docker run -d --name laneb4e-prometheus --network wamn-s5 -p 127.0.0.1:19091:9090 \
  -v "$PWD/deploy/infra/prometheus-local.yaml:/etc/prometheus/prometheus.yml:ro" \
  prom/prometheus:v3.1.0
docker run -d --name laneb4e-grafana --network wamn-s5 -p 127.0.0.1:13001:3000 \
  -e GF_SECURITY_ADMIN_USER=admin -e GF_SECURITY_ADMIN_PASSWORD=admin \
  -v "$PWD/deploy/infra/grafana-local.yaml:/etc/grafana/provisioning/datasources/datasources.yaml:ro" \
  -v "$PWD/deploy/infra/grafana/provisioning/dashboards/providers.yaml:/etc/grafana/provisioning/dashboards/providers.yaml:ro" \
  -v "$PWD/deploy/infra/grafana/dashboards:/var/lib/grafana/dashboards:ro" \
  grafana/grafana:11.4.0
docker run -d --name laneb4e-pg --network wamn-s5 -e POSTGRES_PASSWORD=pg \
  -p 127.0.0.1:15621:5432 postgres:18
until docker exec laneb4e-pg pg_isready -U postgres; do sleep 1; done
docker exec -e PGPASSWORD=pg laneb4e-pg psql -U postgres -c \
  "CREATE SCHEMA registry;
   CREATE TABLE registry.orgs (id text PRIMARY KEY, placement_kind text NOT NULL, pool_cluster text);
   INSERT INTO registry.orgs (id, placement_kind, pool_cluster)
     VALUES ('acme','dedicated',NULL), ('globex','pooled','wamn-pg');"
SYS=postgres://postgres:pg@127.0.0.1:15621/postgres
until curl -sf http://127.0.0.1:13001/api/health >/dev/null; do sleep 1; done
./target/debug/wamn-ctl provision-dashboards --grafana-url http://127.0.0.1:13001 \
  --user admin --password admin --system-database-url "$SYS"
./target/debug/wamn-gates --log-level info dashproof --grafana-url http://127.0.0.1:13001 \
  --user admin --password admin --local --system-database-url "$SYS"
docker rm -f laneb4e-prometheus laneb4e-grafana laneb4e-pg
# In-cluster gate of record (real Prometheus + Grafana; Tempo/Loki HARD; rebake
# the gates + ctl images ONLY — no host/guest change):
docker build --target ctl -t wamn-ctl:dev . && docker build --target gates -t wamn-gates:dev .
kind load docker-image wamn-ctl:dev --name wamn && kind load docker-image wamn-gates:dev --name wamn
kubectl -n wamn-system apply -f deploy/infra/prometheus.yaml
kubectl -n wamn-system create configmap grafana-dashboard-provider \
  --from-file=providers.yaml=deploy/infra/grafana/provisioning/dashboards/providers.yaml \
  --dry-run=client -o yaml | kubectl -n wamn-system apply -f -
kubectl -n wamn-system create configmap grafana-dashboards-sre \
  --from-file=deploy/infra/grafana/dashboards/wamn-sre.json \
  --dry-run=client -o yaml | kubectl -n wamn-system apply -f -
kubectl -n wamn-system apply -f deploy/infra/grafana.yaml
kubectl -n wamn-system rollout status deploy/prometheus deploy/grafana --timeout=120s
# per-tenant folders: a one-off ctl pod driving the Grafana API against registry.orgs
# (creds from the grafana-admin + wamn-sysdb-superuser Secrets):
GFPW=$(kubectl -n wamn-system get secret grafana-admin -o jsonpath='{.data.admin-password}' | base64 -d)
SYSPW=$(kubectl -n wamn-system get secret wamn-sysdb-superuser -o jsonpath='{.data.password}' | base64 -d)
kubectl -n wamn-system run provision-dashboards --rm -i --restart=Never \
  --image=wamn-ctl:dev --command -- wamn-ctl provision-dashboards \
  --grafana-url http://grafana:3000 --user admin --password "$GFPW" \
  --system-database-url "postgres://postgres:$SYSPW@wamn-sysdb-rw:5432/wamn_system"
kubectl -n wamn-system apply -f deploy/gates/dashproof-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/dashproof --timeout=180s
kubectl -n wamn-system logs job/dashproof
```
## CF-RELEASE — immutable catalog publication (`wamn-5wd1.46`)

The release writer stores canonical `wamn-catalog` artifacts in
`catalog.flow_artifacts`, immutable membership in `catalog.release_flows`, and
serializes promotion through the stable `catalog.catalog_heads` row. The
statement-level proof uses a disposable Postgres when `WAMN_MIGRATE_PG_URL` is
set; its deterministic fault mutants always run.

```bash
CARGO_TARGET_DIR=/tmp/wamn-target-cf-release-46 \
  cargo test --locked -p wamn-control-registry -p wamn-schema-control -p wamn-ctl

CARGO_TARGET_DIR=/tmp/wamn-target-cf-release-46 \
  cargo test --locked -p wamn-proof-integration --lib catalog_live::tests::
```

## CF-EXPOSURE — sources, attachments, and activation (`wamn-5wd1.47`)

The exposure gate validates route/mapping/source/entry matching in the pure
preflight model, then proves immutable definitions, single-hash activation,
carry-forward, tombstones, disabled-definition recovery, and atomic publication
through the production publish/copy boundaries.

```bash
CARGO_TARGET_DIR=/tmp/wamn-target-cf-exposure-47 \
  cargo test --locked -p wamn-control-registry -p wamn-schema-control -p wamn-ctl

CARGO_TARGET_DIR=/tmp/wamn-target-cf-exposure-47 \
  cargo test --locked -p wamn-proof-integration --lib exposure_live::tests::
```

## CF-INTERFACE-BUNDLE — pinned runtime artifact lookup (`wamn-5wd1.65`)

Publication stores the exact RFC 8785 resolved-interface bundle text beside
its SHA-256. Copy verifies the graph, bundle bytes, hashes, and artifact key
before writing. The production queue path reads graph plus ordered occurrence
recovery selections in one release-pinned artifact query, with no legacy
flow-table fallback.

```bash
CARGO_TARGET_DIR=/tmp/wamn-target-cf-interface-bundle-65 \
  cargo test --locked -p wamn-catalog -p wamn-control-registry -p wamn-ctl -p wamn-runner

CARGO_TARGET_DIR=/tmp/wamn-target-cf-interface-bundle-65-components \
  cargo test --locked --manifest-path components/Cargo.toml -p flowrunner

WAMN_MIGRATE_PG_URL="$THROWAWAY_PG_URL" \
CARGO_TARGET_DIR=/tmp/wamn-target-cf-interface-bundle-65 \
  cargo test --locked -p wamn-proof-integration --lib catalog_live::tests::
```

## PLAN-0.2 authoritative runner artifact handoff (`wamn-2jdm.5.10`)

The production `run-next` path loads the exact immutable artifact selected by
the admitted run's tenant, catalog, catalog version, flow, flow version, and
release-manifest artifact hash. Inner joins and exact equality make missing or
mixed identities return no artifact; there is no legacy `flows` fallback. The
named mutation changes the catalog-version join and must make the focused debug
test fail before restoring the source hash.

```bash
CARGO_TARGET_DIR=/tmp/wamn-target-2jdm-5-4 \
  cargo test --locked --manifest-path components/Cargo.toml -p flowrunner \
  tests::production_lookup_fail_closes_missing_or_mismatched_authoritative_identity \
  -- --exact

CARGO_TARGET_DIR=/tmp/wamn-target-2jdm-5-4 \
  tools/gate-mutants/authoritative-runner-artifact.sh run
```

## CF-CUSTOM-PUBLISH — verified supplied components (`wamn-5wd1.67`)

`publish-catalog --custom-node` accepts a repeatable JSON descriptor containing
`node-type`, `component`, `manifest`, and `component-digest`. Preflight reads the
exact component bytes, recomputes the SHA-256 digest, resolves the typed manifest
ports/purity/recovery contract, and requires every supplied node type to be
declared directly by a published graph before the immutable release transaction.

```bash
CARGO_TARGET_DIR=/tmp/wamn-target-cf-custom-publish-67 \
  cargo test --locked -p wamn-ctl -p wamn-catalog -p wamn-control-registry

CARGO_TARGET_DIR=/tmp/wamn-target-cf-custom-publish-67-components \
  cargo build --locked --manifest-path components/Cargo.toml \
  -p normalize-receipt -p evaluate-specs -p disposition-node \
  --target wasm32-wasip2

docker run -d --rm --name wamn-cf-custom-publish-pg \
  -p 127.0.0.1:15622:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
# postgres:18 initializes, restarts, then becomes ready; wait for the second
# successful pg_isready before running the proof.
WAMN_MIGRATE_PG_URL=postgresql://postgres:postgres@127.0.0.1:15622/wamn \
WAMN_CF_NORMALIZE_RECEIPT_WASM=/tmp/wamn-target-cf-custom-publish-67-components/wasm32-wasip2/debug/normalize_receipt.wasm \
WAMN_CF_EVALUATE_SPECS_WASM=/tmp/wamn-target-cf-custom-publish-67-components/wasm32-wasip2/debug/evaluate_specs.wasm \
WAMN_CF_DISPOSITION_NODE_WASM=/tmp/wamn-target-cf-custom-publish-67-components/wasm32-wasip2/debug/disposition_node.wasm \
CARGO_TARGET_DIR=/tmp/wamn-target-cf-custom-publish-67 \
  cargo test --locked -p wamn-ctl --test custom_publish_live \
  real_f1_f2_components_publish_retry_and_conflict_by_exact_bytes -- --exact
```

## CF-ATTEMPTS — durable effect protocol and replay classes (`wamn-5wd1.54`)

Flowrunner commits an attempt intent before any external dispatch, marks that
attempt dispatched immediately before the effect, and commits success or error
afterward. An unmarked prepared attempt is resumable for every recovery class.
For a marked attempt, the artifact-pinned occurrence selection and its portable
claim are admitted against the current environment before the first send. The
attempt records both the selected and effective recovery classes plus the exact
connection and credential generation facts that justified admission. Recovery
uses those durable facts: an admitted `replay` or `idempotent-with-key` attempt
may redispatch under its exact key, while `never-replay` becomes
`effect-uncertain` and cannot send again. Runtime node tables, HTTP methods,
configuration, capture, or current environment state never reclassify it.

```bash
cargo test --locked -p wamn-runner -p wamn-run-state -p wamn-node-manifest
cargo test --locked --manifest-path components/Cargo.toml -p flowrunner
cargo build --locked --manifest-path components/Cargo.toml \
  -p flowrunner --target wasm32-wasip2
cargo test --locked -p wamn-proof-integration --lib never_replay::tests::

docker run -d --rm --name wamn-cf-attempts-pg \
  -p 127.0.0.1:15623:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
# Wait for PostgreSQL to complete its initialization restart.
WAMN_RUN_STORE_PG_URL=postgresql://postgres:postgres@127.0.0.1:15623/wamn \
  cargo test --locked -p wamn-run-state --test run_state_live \
  run_state_live -- --ignored --exact --nocapture
```

### [PLAN-1 / wamn-4u7p.24] FLOW-SPEC recovery authority

# recipe-test: PLAN-1-FLOW-SPEC-RECOVERY | conformance | wamn-proof-conformance | test | flow_spec_recovery_authority | - | 4 | shipped three-layer recovery authority, legacy classifier exclusions, source/DDL pins, and docs links/cross-references

```bash
CARGO_TARGET_DIR=/tmp/wamn-target-plan-1-flow-spec \
  cargo test --locked -p wamn-proof-conformance --test flow_spec_recovery_authority
```

## PLAN-2B — typed trusted invocation context (`wamn-99wl`)

Admission derives a versioned principal from the applied release and immutable
flow artifact, then wraps producer-specific metadata under `source`. The same
Rust type can add the node occurrence, attempt, and requirement in the single
trusted HTTP effect call frame. Legacy-unversioned, incomplete, unknown, and
non-object documents fail closed.

```bash
cargo test --locked -p wamn-run-state --lib
cargo clippy --locked -p wamn-run-state --all-targets -- -D warnings
cargo fmt -p wamn-run-state --check
WAMN_RUN_STORE_PG_URL=postgres://postgres:postgres@127.0.0.1:5458/wamn \
  cargo test --locked -p wamn-run-state --test admission_live \
  --test child_live -- --ignored --nocapture --test-threads=1
tools/gate-mutants/trusted-invocation-context.sh run
```

## PLAN-2B — portable HTTP connection floor (`wamn-ko5r.8`)

The flowrunner writes one authorization-derived attempt intent before the send
boundary, then calls the trusted host adapter with identity claims and a
connection-relative request. The host re-derives the release, binding, active
direct-only generation, credential handle, and node permission from admitted
state. `/holds` is portable; bare `holds`, proxy fallback, base escape,
misattribution, and reaching the wire without the exact marked intent fail.

```bash
CARGO_TARGET_DIR=/tmp/wamn-target-ko5r-8 CARGO_INCREMENTAL=0 \
  cargo test --locked -p wamn-run-state -p wamn-standard-nodes \
    -p wamn-node-manifest -p wamn-runtime
CARGO_TARGET_DIR=/tmp/wamn-target-ko5r-8 CARGO_INCREMENTAL=0 \
  cargo clippy --locked -p wamn-run-state -p wamn-standard-nodes \
    -p wamn-runtime -p wamn-execution-host --all-targets -- -D warnings
cargo fmt --all -- --check

WAMN_RUN_STORE_PG_URL=postgresql://postgres:postgres@127.0.0.1:15623/wamn \
CARGO_TARGET_DIR=/tmp/wamn-target-ko5r-8 CARGO_INCREMENTAL=0 \
  cargo test --locked -p wamn-run-state --test run_state_live \
    run_state_live -- --ignored --exact --nocapture
WAMN_CONNECTION_EFFECT_PG_URL=postgresql://wamn_app:wamn_app@127.0.0.1:15623/wamn \
CARGO_TARGET_DIR=/tmp/wamn-target-ko5r-8 CARGO_INCREMENTAL=0 \
  cargo test --locked -p wamn-runtime --lib \
    live_connection_effect_snapshot_requires_exact_marked_intent -- --nocapture

CARGO_TARGET_DIR=/tmp/wamn-target-ko5r-8 CARGO_INCREMENTAL=0 \
  tools/gate-mutants/portable-http-connection-floor.sh green-all
CARGO_TARGET_DIR=/tmp/wamn-target-ko5r-8 CARGO_INCREMENTAL=0 \
  tools/gate-mutants/portable-http-connection-floor.sh run-all
cargo test --locked -p wamn-proof-conformance --test gate_mutation_evidence

# Then run the existing H5-CALLABLE-WAVE1 and H5-CALLABLE-WAVE2 exact-image
# recipes below; F3 and F4 are the deployed standard/custom connection proofs.
```

### Runner address-level egress boundary (`wamn-4q3c.12`)

`deploy/platform/runner-netpol.yaml` is the default-deny address ceiling for
runner pods. It admits DNS, project PostgreSQL, control NATS, OTLP, and signed
custom-node transport. Business connection destinations are not part of that
platform list: each environment adds only its approved CIDRs/ports using the
shape in `runner-connection-egress.example.yaml`. The P0 environment uses
`deploy/gates/runner-connection-egress.yaml` to admit `serve-echo` and keeps the
reachable `egress-escape` control target outside the union.
Per-project policy rendering/provisioning replaces the hand-maintained
environment manifest under existing bead `wamn-ou1`.

The gate requires kindnet's `kube-network-policies` controller to be active,
not merely an accepted NetworkPolicy API object. The mutation tool refuses to
run unless the kind node has the `inet kindnet-network-policies` nftables table.

```bash
cargo test --locked -p wamn-proof-system --lib credproof::tests::
cargo test --locked -p wamn-builder --test golden_deploy
cargo clippy --locked -p wamn-proof-integration -p wamn-proof-system \
  --all-targets -- -D warnings
rustfmt --edition 2024 --check \
  services/builder/src/deploy_emit.rs tests/integration/src/credproof.rs \
  tests/system/src/credproof.rs

# Exact deployed connection_http proof. The positive environment binding reaches
# serve-echo; the equally hostname-authorized escape binding resolves but its
# address cannot reach egress-escape. Apply both policy owners before the Job.
docker build --target host -t wamn-host:dev .
docker build --target gates -t wamn-gates:dev .
kind load docker-image wamn-host:dev --name wamn
kind load docker-image wamn-gates:dev --name wamn
# The control NATS selector in the platform policy must resolve to a live pod.
kubectl -n wamn-system wait --for=condition=Ready pod \
  -l wasmcloud.com/name=nats --timeout=120s
kubectl -n wamn-system apply -f deploy/platform/runner-netpol.yaml
kubectl -n wamn-system apply -f deploy/gates/runner-connection-egress.yaml
# Fresh gate clusters need the fail-closed empty placement map. Never overwrite
# an environment-owned populated map with this example.
kubectl -n wamn-system get configmap wamn-node-placements >/dev/null || \
  kubectl -n wamn-system apply -f deploy/platform/runner-node-placements.example.yaml
kubectl -n wamn-system apply -f deploy/platform/runner.yaml
kubectl -n wamn-system rollout status deploy/runner --timeout=180s
kubectl -n wamn-system apply -f deploy/gates/serve-echo.yaml
kubectl -n wamn-system apply -f deploy/gates/egress-escape.yaml
docker exec "$(kind get nodes --name wamn | head -n 1)" \
  nft list table inet kindnet-network-policies >/dev/null
kubectl -n wamn-system delete job credproof --ignore-not-found
kubectl -n wamn-system apply -f deploy/gates/credproof-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/credproof --timeout=300s
kubectl -n wamn-system logs job/credproof

# Mutation: add app=egress-escape:8091 to the P0 connection policy. The same
# credproof turns red because the address escape reaches its target. The tool
# restores byte-exactly and reapplies; require the clean Job to pass again.
tools/gate-mutants/runner-egress-address.sh green
tools/gate-mutants/runner-egress-address.sh run
tools/gate-mutants/runner-egress-address.sh green

# Real production Deployment regression: the normal F3 flow must still reach
# its environment-admitted serve-echo connection through the policy floor.
kubectl -n wamn-system delete job f3proof --ignore-not-found
kubectl -n wamn-system apply -f deploy/gates/f3proof-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/f3proof --timeout=300s
kubectl -n wamn-system logs job/f3proof

# Existing signed-node transport regression: nodebench's source pod carries the
# runner profile and its unchanged cross-pod hop reaches only the labeled
# serve-node-gate target on :8080. F4 is co-located/loopback and does not prove
# this NetworkPolicy boundary.
kubectl -n wamn-system apply -f deploy/gates/serve-node.yaml
kubectl -n wamn-system rollout status deploy/serve-node-gate --timeout=120s
kubectl -n wamn-system delete job nodebench --ignore-not-found
kubectl -n wamn-system apply -f deploy/gates/nodebench-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/nodebench --timeout=300s
kubectl -n wamn-system logs job/nodebench
```

## PLAN-2A — respond common Node ABI (`wamn-ayq7.20`)

`respond` resolves to the pinned platform standard-node executable, dispatches
through the common Node ABI, and only then enters the engine-owned caller-release
transition. The mutation removes `respond` from the production standard-node
resolver and must make the focused debug gate fail before restoring the source.

```bash
CARGO_TARGET_DIR=/tmp/wamn-target-ayq7-20 \
  cargo test --locked -p wamn-runner -p wamn-catalog
CARGO_TARGET_DIR=/tmp/wamn-target-ayq7-20 \
  cargo test --locked --manifest-path components/Cargo.toml -p flowrunner
CARGO_TARGET_DIR=/tmp/wamn-target-ayq7-20 \
  tools/gate-mutants/respond-node-abi.sh run
```

## PLAN-2A — request common Node ABI (`wamn-ayq7.22`)

`request` resolves to the pinned, capability-free platform standard-node
executable and emits the admitted payload unchanged on `main`. The flowrunner
validates that exact payload, port, and absent context replacement before the
generic durable attempt checkpoint can advance the entry token. The mutation
bypasses this validation for `request`; the focused debug gate must fail before
the source is restored.

```bash
CARGO_TARGET_DIR=/tmp/wamn-target-ayq7-22 \
  cargo test --locked -p wamn-runner -p wamn-standard-nodes -p wamn-catalog
CARGO_TARGET_DIR=/tmp/wamn-target-ayq7-22 \
  cargo test --locked -p wamn-ctl --lib publish_catalog
CARGO_TARGET_DIR=/tmp/wamn-target-ayq7-22 \
  cargo test --locked --manifest-path components/Cargo.toml -p flowrunner
CARGO_TARGET_DIR=/tmp/wamn-target-ayq7-22 \
  tools/gate-mutants/request-node-abi.sh run
```

## PLAN-2A — cron common Node ABI (`wamn-ayq7.23`)

`cron` resolves to the pinned, capability-free platform standard-node
executable and emits the scheduler-admitted payload unchanged on `main`. The
flowrunner validates that exact payload, port, and absent context replacement
before the generic durable attempt checkpoint can advance the entry token.
Cron consumes one dispatch-budget unit, like every other Node-ABI execution;
schedule admission, callerless lifecycle, and durable ordering remain owned by
the engine and driver. The mutation bypasses cron's production validation; the
focused debug gate must fail before the source is restored.

```bash
CARGO_TARGET_DIR=/tmp/wamn-target-ayq7-23 \
  cargo test --locked -p wamn-runner -p wamn-standard-nodes -p wamn-catalog
CARGO_TARGET_DIR=/tmp/wamn-target-ayq7-23 \
  cargo test --locked -p wamn-ctl --lib publish_catalog
CARGO_TARGET_DIR=/tmp/wamn-target-ayq7-23 \
  cargo test --locked --manifest-path components/Cargo.toml -p flowrunner
CARGO_TARGET_DIR=/tmp/wamn-target-ayq7-23 \
  tools/gate-mutants/cron-node-abi.sh run
```

## PLAN-2A — event common Node ABI (`wamn-ayq7.24`)

`event` resolves to the pinned, capability-free platform standard-node
executable and emits the externally admitted payload unchanged on `main`. The
flowrunner validates that exact payload, port, and absent context replacement
before the generic durable attempt checkpoint can advance the entry token.
Every inbound-edge dispatch consumes one dispatch-budget unit, including
event; admission, callerless lifecycle, reconstruction, and durable ordering
remain owned by the engine and driver. The mutation bypasses event's production
validation; the focused debug gate must fail before the source is restored.

```bash
CARGO_TARGET_DIR=/tmp/wamn-target-ayq7-24 \
  cargo test --locked -p wamn-runner -p wamn-standard-nodes -p wamn-catalog
CARGO_TARGET_DIR=/tmp/wamn-target-ayq7-24 \
  cargo test --locked -p wamn-ctl --lib publish_catalog
CARGO_TARGET_DIR=/tmp/wamn-target-ayq7-24 \
  cargo test --locked --manifest-path components/Cargo.toml -p flowrunner
CARGO_TARGET_DIR=/tmp/wamn-target-ayq7-24 \
  tools/gate-mutants/event-node-abi.sh run
```

## PLAN-2A — fail common Node ABI (`wamn-ayq7.25`)

`fail` resolves to the pinned, capability-free platform standard-node
executable and returns the exact authored terminal code/message through the
common Node ABI. As an ordinary Node-ABI execution it consumes one dispatch-
budget unit; it is not an inbound node. The production driver validates that
typed terminal result before one replay-safe transaction completes the attempt,
releases an attached caller with the configured HTTP status, and terminalizes
the run. Non-terminal or mismatched results cannot authorize lifecycle
mutation. The mutation bypasses that production validation; the focused debug
gate must fail before the source is restored.

```bash
CARGO_TARGET_DIR=/tmp/wamn-target-ayq7-25 \
  cargo test --locked -p wamn-runner -p wamn-standard-nodes -p wamn-catalog
CARGO_TARGET_DIR=/tmp/wamn-target-ayq7-25 \
  cargo test --locked -p wamn-ctl --lib publish_catalog
CARGO_TARGET_DIR=/tmp/wamn-target-ayq7-25 \
  cargo test --locked --manifest-path components/Cargo.toml -p flowrunner
CARGO_TARGET_DIR=/tmp/wamn-target-ayq7-25 \
  tools/gate-mutants/fail-node-abi.sh run
```

## PLAN-1 — uniform node-interface pinning (`wamn-4u7p.38`)

Publication resolves platform and supplied nodes into the same canonical
resolved-contract bundle. Built-in-only artifacts persist a non-empty bundle,
and runtime reconstruction verifies those exact contracts without a current
model-owned fallback. Historical versions retain their explicit compatibility
readers; unknown versions fail closed. The mutation reintroduces the former
`request` exemption and must make the focused publication-to-runtime round-trip
gate fail before restoring the catalog source byte-exact.

```bash
CARGO_TARGET_DIR=/tmp/wamn-target-4u7p-38 \
  cargo test --locked -p wamn-catalog -p wamn-standard-nodes -p wamn-ctl -p wamn-runner
CARGO_TARGET_DIR=/tmp/wamn-target-4u7p-38 \
  cargo test --locked --manifest-path components/Cargo.toml -p flowrunner
CARGO_TARGET_DIR=/tmp/wamn-target-4u7p-38 \
  tools/gate-mutants/uniform-interface-pinning.sh run
```

## CF-DEADLINES — bounded attempts and poisoned-instance disposal (`wamn-fqg.14`)

The final fenced send marker rejects elapsed attempt and run deadlines before
writing `attempt_dispatched_at`. The execution host applies the same finite
ceiling to the Wasmtime epoch and outbound HTTP waits; Postgres pool and
statement waits clamp to that ceiling. A Wasmtime interruption drops the live
store before the executor exits for replacement, so no later call can enter the
poisoned instance.

```bash
CARGO_TARGET_DIR=/tmp/wamn-target-fqg14 \
  cargo test --locked -p wamn-runtime -p wamn-execution-host -p wamn-runner

CARGO_TARGET_DIR=/tmp/wamn-target-fqg14 \
  cargo test --locked -p wamn-proof-system --lib deadlineproof::tests::

CARGO_TARGET_DIR=/tmp/wamn-target-fqg14-components \
  cargo test --locked --manifest-path components/Cargo.toml -p flowrunner

WAMN_RUN_STORE_PG_URL=postgresql://postgres:postgres@127.0.0.1:15623/wamn \
CARGO_TARGET_DIR=/tmp/wamn-target-fqg14 \
  cargo test --locked -p wamn-run-state --test run_state_live \
  run_state_live -- --ignored --exact --nocapture
```
