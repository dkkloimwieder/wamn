# Build & Test — gate commands per bead

> **§1.9a audit (2026-07-19): amendments are additive — base sound.**

Every shipped feature/bead has a build+gate command block below. Prose rationale
lives in the design docs (`docs/*.md`) and the beads memories (`bd memories <keyword>`);
this file is the runnable-command reference. See `README.md` for the quick
dev/test/deploy commands.

## Build environment

wamn-host builds against wash-runtime consumed as a **git dependency from our
fork** (dkkloimwieder/wasmCloud, branch `wamn/2.7.0` = upstream v2.7.0).
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

The registry derives MVP gate-manifest disposition from
`docs/scope-reduction-mvp.md` anchor `## D · Gate-manifest disposition` and
commands and execution inputs from every live gate Job and documented recipe.
Retained D1-D24 decision mappings are historical compatibility metadata; the
registry also owns non-derivable gate semantics.

```bash
# recipe-test: PLAN-0-2-GATE-REGISTRY | conformance | wamn-proof-conformance | test | gate_registry | - | 6 | canonical semantic registry covers live Appendix D authority, every Job manifest and recipe selector, historical D1-D24 compatibility metadata, classifications, immutable evidence pointers, and registry mutants
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
selection for the current **38 root + 6 component packages**. The selection
combines the exact 19 root `default-members` with named explicit selectors.
`tests/conformance/tests/workspace_tiers.rs` compares those sets with live,
locked Cargo metadata and `architecture/package-roles.json`.

The selected package roots are:

| Tier | Root | Components | Selection |
|---|---:|---:|---|
| bare root default | 19 | 0 | the ratified MVP developer floor in charter order |
| fast developer/native | 31 | 0 | every root production package; excludes proof/support packages and POCs |
| product components | 0 | 3 | `flow-http`, `flowrunner`, `materializer` |
| contract/conformance | 10 | 0 | all contract packages plus `wamn-proof-conformance` |
| full CI | 38 | 6 | every Cargo member; non-Cargo inputs: 0 |
| deployed-system proof | 14 | 6 | deployable native/proof owners plus every retained guest proof input; non-Cargo inputs: 0 |
| release | 8 | 3 | every package classified `deployable: true` |

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

The root virtual workspace has 19 exact `default-members`; the component
workspace has none. Consequently:

- From the repository root, bare `cargo build`, `cargo check`, and `cargo test`
  select the 19-package MVP developer floor. Bare `cargo test` uses each
  selected package's default test targets.
- A manual root command that must cover all 38 members uses `--workspace`.
  The full-CI helper below provides the same exhaustive membership coverage
  with 38 explicit `--package` selectors plus `--all-targets --no-fail-fast`.
- From `components/`, the same bare commands select all 6 component members.
  The production guest build remains
  `cargo build --workspace --target wasm32-wasip2`.
- Full CI keeps two package/artifact steps—every root target and every retained
  component artifact—then runs the current gate-registry conformance proof:
  ```bash
  ./tools/workspace-tier run full_ci root test-all
  ./tools/workspace-tier run full_ci components build-wasm
  ./tools/workspace-tier list full_ci
  cargo test --locked --offline -p wamn-proof-conformance \
    --test gate_registry
  ```
- Neither local full CI nor a successful Cargo build substitutes for the
  owning deployed Job below. Gate-of-record semantics are unchanged.

#### Per-wave sweep bar (wamn-0h0g.15.131)

A wave's clean baseline is not `--lib`. `cargo test --workspace --lib` selects
only library unit targets, so **no integration-test binary under any `tests/`
directory executes at all** — such a test does not fail, it is never run and
never reported. Wave 8's recorded baseline was
`cargo check --workspace --all-targets` plus `cargo test --workspace --lib`;
`crates/platform/runtime/tests/flow_invocation_wit_coherence.rs` went red at
`09498e17` and stayed green-*looking* for a full wave, and the file was edited
twice afterwards — `b405694e`, then `7da5a70d` editing the guard file itself —
without anyone observing the failure. `--all-targets` on the `check` leg does
not rescue this: it compiles the test target, it does not run it.

The bar for a wave sweep is, verbatim:

```bash
cargo test --workspace --all-targets --no-fail-fast
```

- `--all-targets` is mandatory, not an optimization. It is the only thing that
  puts `tests/*.rs` binaries into the run set.
- `--no-fail-fast` reports every failing suite in one pass. It does **not** cover
  compile failures: a package that fails to build still truncates the sweep, so a
  compile break must be cleared before a sweep result means anything.
- Run it **unpiped**. `cargo test … | tee log` reports `tee`'s exit status, not
  cargo's, so a red sweep reads as exit 0. If a log is required, redirect
  (`> log 2>&1`) or set `-o pipefail`, and read cargo's own status.
- Bare `cargo test` is not a substitute: from the repository root it selects the
  19 `default-members` and only each one's *default* test targets.
- The component workspace is outside root `--workspace` and needs its own leg.
- Contract-owning guards additionally have a runner of record,
  `tools/contract-diff run`, whose legs include
  `cargo test -p wamn-runtime --test flow_invocation_wit_coherence`. Its own
  conformance test proves that argv against a **fake cargo**, so a green
  `contract_diff` is evidence the plan is right, never evidence the guards are
  green (wamn-0h0g.15.138). The tool must actually be run.

### Release identity

Release membership is the 11 `deployable: true` packages in
`architecture/package-roles.json`, including the `wamn-gates` proof image.
Membership is not release admission. SR17 must join source revision and
`Cargo.lock` digest to exact artifact SHA-256 and OCI manifest digest; SR26
must join each required gate evidence record back to that same source revision
and artifact/image digests. The exact required fields and fail-closed rule live
in `architecture/workspace-tiers.json`. Cargo defaults, a mutable tag, or an
evidence record that names only a test command are not release evidence.

### MVP landing measurement (wamn-0h0g.10.6, 2026-08-16)

The accepted local build receipt is for exact source revision
`11aa572be7afdb85ee6cd183ea6270a93ff86931` and `Cargo.lock` SHA-256
`60a91d3a0bf6f3cea64eca3ffec81e351c59f19658dea79eb40811906f537997`.
The canonical machine-readable result, including the exact 19 default members,
lives in `architecture/workspace-tiers.json`; raw local receipts live under
`/home/kaalin/dev/wamn/target/plane-wave16-10-6/raw`.

The commands ran serially on `k11`: Linux 7.0.0-29-generic x86_64, i7-1185G7
(4 cores / 8 threads), 65,437,429,760 bytes RAM, ext4 on
`/dev/nvme0n1p6[/home/kaalin/dev/wamn]`, rustc 1.97.0
(`2d8144b78`, 2026-07-07), and cargo 1.97.0 (`c980f4866`, 2026-06-30).
Each bare Cargo cold run used its own initially absent target-backed target and
TMPDIR; warm means the immediate identical Cargo command on that target. Each
image cold run was the first measured build on its own newly created
`docker-container` Buildx builder; warm means the immediate identical build on
that builder, with every load-bearing cook/build stage reporting `CACHED`.

| Command/target | Cold total | Cold cook | Cold build | Warm total |
|---|---:|---:|---:|---:|
| `cargo test --locked` | 702.98s | — | — | 14.80s |
| `cargo check --locked` | 408.41s | — | — | 0.33s |
| image `host` / `wamn-host` | 490.08s | 239.6s | 176.5s | 2.17s |
| image `run-worker` / `wamn-executor` | 873.88s | 500.3s | 233.9s | 1.00s |
| image `scenario-worker` / `wamn-scenario-worker` | 349.83s | 213.0s | 59.9s | 0.84s |
| image `ctl` / `wamn-ctl` | 175.53s | 37.9s | 71.5s | 0.75s |
| image `dispatcher` / `wamn-dispatcher` | 166.77s | 66.8s | 33.5s | 0.74s |
| image `waker` / `wamn-waker` | 144.35s | 53.7s | 21.0s | 0.77s |
| image `cdc-reader` / `wamn-cdc-reader` | 158.64s | 62.2s | 30.4s | 0.74s |

The retained `run-worker` image also ran the Dockerfile's six-package
`component-builder` stage in 108.3s cold; it was `CACHED` warm. The `ctl`
`build-ctl` stage also owns the feature-gated `wamn-ctl-ops` binary. The
separate default-binary proof below confirmed that the retained default image
still exposes only the MVP verb surface. These are local build measurements,
not cluster/Kubernetes proof or SR17/SR26 release admission. No standalone
`cargo ... --release` command was introduced or measured: the Dockerfile owns
its package-scoped release cook/build commands.

The following is the exact reproducible recipe. The target and builder names
must be absent before the cold commands.

```bash
set -o pipefail
WAMN_10_6_ROOT=/home/kaalin/dev/wamn/target/plane-wave16-10-6
WAMN_10_6_RAW="$WAMN_10_6_ROOT/raw"
mkdir -p "$WAMN_10_6_RAW"

test ! -e "$WAMN_10_6_ROOT/root-test"
mkdir -p "$WAMN_10_6_ROOT/tmp/root-test"
TMPDIR="$WAMN_10_6_ROOT/tmp/root-test" \
  CARGO_TARGET_DIR="$WAMN_10_6_ROOT/root-test" \
  /usr/bin/time -v -o "$WAMN_10_6_RAW/root-test-cold.time" \
  cargo test --locked 2>&1 | tee "$WAMN_10_6_RAW/root-test-cold.log"
TMPDIR="$WAMN_10_6_ROOT/tmp/root-test" \
  CARGO_TARGET_DIR="$WAMN_10_6_ROOT/root-test" \
  /usr/bin/time -v -o "$WAMN_10_6_RAW/root-test-warm.time" \
  cargo test --locked 2>&1 | tee "$WAMN_10_6_RAW/root-test-warm.log"

test ! -e "$WAMN_10_6_ROOT/root-check"
mkdir -p "$WAMN_10_6_ROOT/tmp/root-check"
TMPDIR="$WAMN_10_6_ROOT/tmp/root-check" \
  CARGO_TARGET_DIR="$WAMN_10_6_ROOT/root-check" \
  /usr/bin/time -v -o "$WAMN_10_6_RAW/root-check-cold.time" \
  cargo check --locked 2>&1 | tee "$WAMN_10_6_RAW/root-check-cold.log"
TMPDIR="$WAMN_10_6_ROOT/tmp/root-check" \
  CARGO_TARGET_DIR="$WAMN_10_6_ROOT/root-check" \
  /usr/bin/time -v -o "$WAMN_10_6_RAW/root-check-warm.time" \
  cargo check --locked 2>&1 | tee "$WAMN_10_6_RAW/root-check-warm.log"

for target in host run-worker scenario-worker ctl dispatcher waker cdc-reader; do
  WAMN_10_6_BUILDER="wamn-0h0g-10-6-$target"
  WAMN_10_6_IMAGE_ROOT="$WAMN_10_6_ROOT/docker/$target"
  WAMN_10_6_TAG="wamn-$target:0h0g-10-6-11aa572b"
  test ! -e "$WAMN_10_6_IMAGE_ROOT"
  mkdir -p "$WAMN_10_6_IMAGE_ROOT/tmp"
  docker buildx create --name "$WAMN_10_6_BUILDER" \
    --driver docker-container --use --bootstrap
  TMPDIR="$WAMN_10_6_IMAGE_ROOT/tmp" \
    /usr/bin/time -v -o "$WAMN_10_6_RAW/docker-$target-cold.time" \
    docker buildx build --builder "$WAMN_10_6_BUILDER" --progress=plain \
      --target "$target" --load --tag "$WAMN_10_6_TAG" . 2>&1 \
      | tee "$WAMN_10_6_RAW/docker-$target-cold.log"
  TMPDIR="$WAMN_10_6_IMAGE_ROOT/tmp" \
    /usr/bin/time -v -o "$WAMN_10_6_RAW/docker-$target-warm.time" \
    docker buildx build --builder "$WAMN_10_6_BUILDER" --progress=plain \
      --target "$target" --load --tag "$WAMN_10_6_TAG" . 2>&1 \
      | tee "$WAMN_10_6_RAW/docker-$target-warm.log"
done

docker run --rm wamn-ctl:0h0g-10-6-11aa572b --help 2>&1 \
  | tee "$WAMN_10_6_RAW/ctl-help.txt"
test ! -e "$WAMN_10_6_ROOT/ctl-tree"
mkdir -p "$WAMN_10_6_ROOT/ctl-tree/tmp"
TMPDIR="$WAMN_10_6_ROOT/ctl-tree/tmp" \
  CARGO_TARGET_DIR="$WAMN_10_6_ROOT/ctl-tree" \
  cargo tree --locked --offline -p wamn-ctl --edges features 2>&1 \
  | tee "$WAMN_10_6_RAW/ctl-default-feature-tree.txt"
```

The help receipt contained all nine MVP commands: `publish-catalog`,
`provision-project`, `provision-org`, `provision-project-env`,
`enable-cdc-project-env`, `migrate-catalog`,
`reconcile-replica-identity`, `reconcile-run-plane`, and
`terminalize-effect-uncertain`. It contained none of `dump-project-env`,
`restore-project-env`, `copy-project-env`, `prune-run-history`,
`impact-report`, or `pin-run`. The locked/offline default feature tree
contained none of `wamn-control-provision feature "ops"`,
`wamn-schema-compiler feature "ops"`, or
`wamn-schema-control feature "ops"`.

### Historical measurement (2026-07-25; pre-MVP workspace)

These receipts predate the 38-member workspace and 19-member root default
cutover. They remain only as historical evidence and are not comparable to the
accepted exact-base measurement above. The historical runs used debug/default
profile on `k11` (8 logical CPUs, i7-1185G7, 60 GiB RAM, NVMe; rustc/cargo
1.97.0) with isolated target directory
`/home/kaalin/dev/wamn/target/lanes/wamn-4tob-6-29-20260725`. Each cold row
followed `cargo clean`; each warm row immediately repeated the identical
command. The archived table below retains the historical summary.

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
docker buildx build --check --progress=plain .
git diff --check
```

## [SR-MVP / wamn-0h0g.7.1] durable flow-invocation replay

This debug-only gate proves begin/wait at identity 0.1: an identical key returns
the same in-flight or released run without another admission or dispatch, the
key requirement comes from the flow's own plan, admission expiry is removed,
and timeout/effect uncertainty retain their fixed typed HTTP representations.
The live legs require one disposable PostgreSQL 18 database.

```bash
export CARGO_TARGET_DIR=/tmp/wamn-target-cleanup-next
export CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2

cargo test --locked -p wamn-run-state -p wamn-runtime \
  -p wamn-flow-invocation -p wamn-schema-control \
  -p wamn-proof-integration
cargo test --locked -p wamn-proof-conformance --lib invocation::tests::
cargo test --locked --manifest-path components/Cargo.toml \
  -p flow-http -p materializer

docker run --rm -d --name wamn-0h0g-7-1-pg \
  -p 127.0.0.1:15657:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
until docker exec wamn-0h0g-7-1-pg pg_isready -U postgres -d wamn; do sleep 1; done
WAMN_RUN_STORE_PG_URL=postgresql://postgres:postgres@127.0.0.1:15657/wamn \
  cargo test --locked -p wamn-run-state --test admission_live \
  admission_live -- --ignored --exact --nocapture --test-threads=1
WAMN_CTL_PG_URL=postgresql://postgres:postgres@127.0.0.1:15657/wamn \
  cargo test --locked -p wamn-ctl --test run_plane_live \
  run_plane_reconcile_live -- --exact --nocapture --test-threads=1
docker stop wamn-0h0g-7-1-pg

cargo clippy --locked -p wamn-run-state -p wamn-runtime \
  -p wamn-flow-invocation -p wamn-schema-control -p wamn-ctl \
  -p wamn-proof-conformance -p wamn-proof-integration \
  --all-targets -- -D warnings
cargo clippy --locked --manifest-path components/Cargo.toml \
  -p flow-http -p materializer --all-targets -- -D warnings


cargo fmt -p wamn-run-state -p wamn-runtime -p wamn-flow-invocation \
  -p wamn-schema-control -p wamn-proof-conformance \
  -p wamn-proof-integration --check
cargo fmt --manifest-path components/Cargo.toml \
  -p flow-http -p materializer --check
git diff --check
```

Historical result: the restructure proof predates the .6.3 deletion wave and
therefore counted deleted node/builder samples in its closure. Its old `node-ts`
and node-manifest fixture commands are provenance only and are no longer
runnable gate recipes. Current full-CI coverage is the two retained workspace
steps plus the `gate_registry` conformance proof above. BuildKit's static
Dockerfile evaluation completed with no warnings in the historical run.

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

### Deleted exact/capability specialization provenance

The former exact-node and capability-class specialization recipes built deleted
fixture components. They were removed by `.6.3`; there is no current runnable
recipe for those fixture arms.

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

The former nodebench, `node-ts`, `node-rs`, `flow-driver`, `sample-node`, and
`serve-node-gate` recipes were deleted by `.6.3` with the custom-node/composed
arm. There is no retained runnable S4 nodebench gate.

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
cargo test -p wamn-standard-nodes   # standard HTTP forward + explicit-header-wins
# System-proof boundary: traceproof independently invokes the public P2 and P3
# host surfaces, captures only their post-host headers, and sends those captured
# headers across the pod boundary without guest help.
# recipe-test: H5-TRACEPROOF | system | wamn-proof-system | lib | - | traceproof::tests:: | 5 | tests/system/src/traceproof.rs independent P2/P3 host-enforced W3C trace injection and keep-alive response framing
cargo test -p wamn-proof-system --lib traceproof::tests::
cargo clippy -p wamn-standard-nodes -p wamn-proof-system -p wamn-gates \
  --all-targets -- -D warnings
cargo fmt -p wamn-standard-nodes -p wamn-proof-system -p wamn-gates --check

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
backend through draft save and validation against a pinned release. Validation
consumes the flowrunner component compiled from the current checkout and leaves
the release rows unchanged. On a green run the test drops its run schema,
`catalog`, and all three roles;
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
WAMN_AUTHORING_LOOP_FLOWRUNNER=/tmp/wamn-target-ftfc-11/wasm32-wasip2/debug/flowrunner.wasm \
CARGO_TARGET_DIR=/tmp/wamn-target-ftfc-11 \
  cargo test --locked --offline -p wamn-scenario-worker \
  --test authoring_loop_live authoring_loop_live -- --ignored --exact --nocapture

docker stop wamn-ftfc11-pg
```

### [2.6] DB-path egress review

Docs: docs/archive/data-path/security-db-path.md

```bash
REL=components/target/wasm32-wasip2/release
# First-party DB-touching workload via --flowrunner; it must import
# wamn:postgres and must not import wasi:sockets.
./target/release/wamn-gates --log-level warn egressbench \
  --flowrunner $REL/flowrunner.wasm

# Static proof spans the host artifact, reusable runtime/execution adapters,
# component import policy, executor service, and proof owners.
cargo clippy -p wamn-host -p wamn-runtime -p wamn-component-policy \
  -p wamn-execution-host -p wamn-executor -p wamn-gates -p wamn-gate-harness --all-targets \
  && cargo fmt -p wamn-host -p wamn-runtime -p wamn-component-policy \
    -p wamn-execution-host -p wamn-executor -p wamn-gates -p wamn-gate-harness --check

# E13/E15 runtime raw-socket deny, the in-cluster gate of record. sockprobe
# independently executes the P2 TcpConnect,
# UdpConnect, UdpOutgoingDatagram, and service/non-loopback UdpBind arms through
# the production host store path. Raw egress is DENIED by default and PERMITTED
# only under wamn.allow-raw-sockets; UdpBind remains service-loopback-only. The
# conformance proof resolves exact linked wash-runtime 2.7.0 revision
# daba602901507338e99f277e07a8e923c61dc557 and pins the shared policy plus every
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
# recipe-test: H5-EGRESSBENCH | conformance | wamn-proof-conformance | lib | - | egressbench::tests:: | 21 | tests/conformance/src/egressbench.rs arm-specific P2 runtime denial/opt-in assertions, exact linked-fork P2/P3 mirror guards, and the allowedHostLoopbackPorts grant surface (wamn-0h0g.15.52) — both halves isolated against the linked SocketPolicy::decide, count-mode non-softening with its own control, and the sentinel-first/no-gate/plumbing pins
cargo test -p wamn-proof-conformance --lib egressbench::tests::
# recipe-test: H5-SOCKETGUARD | conformance | wamn-proof-conformance | lib | - | socketguard::tests:: | 3 | tests/conformance/src/socketguard.rs P2/P3 publish refusal and standard-workload control
cargo test -p wamn-proof-conformance --lib socketguard::tests::
# recipe-test: H5-COMPONENT-POLICY | policy-unit | wamn-component-policy | lib | - | tests:: | 6 | crates/platform/component-policy/src/lib.rs first-party P2/P3 socket-package refusal
cargo test -p wamn-component-policy --lib tests::
./target/release/wamn-gates --log-level warn socketguard
# in-cluster sweep (carries the hermetic gate alongside egressbench-job):
kubectl -n wamn-system apply -f deploy/gates/socketguard-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/socketguard --timeout=120s
kubectl -n wamn-system logs job/socketguard
```

### [11.5] deleted builder test gate provenance

The former builder/testgate recipe was deleted by `.6.3` with the custom-node
builder and `disposition-node` sample. It is retained here only as historical
provenance; there is no current runnable testgate command.

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

### [SR-MVP / wamn-0h0g.7.2] flow-schema MVP cut

This debug-only gate proves the request/event entry set, exact node-id charset,
retired-field refusal, compiled-plan agreement, regenerated schema, and every
retained runtime fixture that consumes the authored graph.

```bash
export CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-7-2
export CARGO_INCREMENTAL=0
cargo test --locked --offline -p wamn-flow -p wamn-runner -p wamn-catalog
cargo test --locked --offline -p wamn-run-state --test store
cargo test --locked --offline -p wamn-schema-control exposure::tests::
cargo test --locked --offline -p wamn-scenario-worker authoring::tests::
cargo test --locked --offline -p wamn-proof-integration --lib
cargo test --locked --offline -p wamn-proof-conformance --lib flow::tests::
cargo test --locked --offline -p wamn-proof-conformance --lib \
  version_identity::governed_wire_schema_and_artifact_versions_stay_at_mvp_identity
cargo clippy --locked --offline -p wamn-flow -p wamn-runner -p wamn-catalog \
  -p wamn-run-state -p wamn-schema-control -p wamn-scenario-worker \
  -p wamn-proof-integration -p wamn-execution-host -p wamn-runtime \
  --all-targets -- -D warnings
cargo fmt -p wamn-flow -p wamn-runner -p wamn-catalog -p wamn-run-state \
  -p wamn-schema-control -p wamn-scenario-worker -p wamn-proof-integration \
  -p wamn-execution-host -p wamn-runtime --check
jq empty docs/archive/contracts/flow-schema.schema.json
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

### [SR-MVP / wamn-0h0g.2.2] native effect-provider revision

This gate is local, offline, and debug-only. The conformance proof regenerates
the normal dependency closure with Cargo 1.97.0 from package-scoped
`wamn-executor` resolution (`--locked --offline --target all`), projects the
`wamn-execution-host` subgraph, and verifies the checked manifest byte for byte.
It also proves exact framing, canonical rejection, every governed local and
external mutant, out-of-scope stability, and the shared executor/management
revision. The claim-time foreign-revision refusal and zero-guest sentinel belong
to `wamn-0h0g.2.3`, not this gate.

```bash
cargo --version # must report cargo 1.97.0
# Nine named tests in tests/conformance/tests/effect_provider_revision.rs own
# exact framing, closure drift, mutation polarity, and shared embedding.
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-2-2 CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-proof-conformance \
  --test effect_provider_revision -- --test-threads=1
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-2-2 CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-execution-host \
  trusted_runtime_revision_
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-2-2 CARGO_INCREMENTAL=0 \
  cargo clippy --locked --offline -p wamn-execution-host -p wamn-executor \
  -p wamn-scenario-worker -p wamn-proof-conformance --all-targets -- -D warnings
cargo fmt -p wamn-execution-host -p wamn-executor \
  -p wamn-scenario-worker -p wamn-proof-conformance --check
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
WAMN_AUTHORING_LOOP_FLOWRUNNER=/tmp/wamn-target-0h0g-2-4-components/wasm32-wasip2/debug/flowrunner.wasm \
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-2-4 CARGO_INCREMENTAL=0 \
  cargo test --locked -p wamn-scenario-worker --test authoring_loop_live \
  authoring_loop_live -- --ignored --exact --nocapture

docker stop wamn-0h0g-2-4-pg

# Each mutant first runs the named clean debug gate, then must fail that same
# gate after an exact-one source mutation; every target is restored byte-for-byte.

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

### [CALLABLE-FLOWS-P4] production invocation provider

Docs: `docs/archive/execution/FLOW-SPEC.md` §§6.1–6.2, §§9.4–9.7, §§10–11, Phase 4.

```bash
cargo test --locked -p wamn-runtime -p wamn-run-state -p wamn-flow-invocation
cargo test --locked -p wamn-proof-conformance --lib invocation
cargo test --locked -p wamn-proof-conformance docker_component_provenance
cargo check --locked -p wamn-host
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

### [5.3] standard node library v1 (crates/execution/standard-nodes)

Docs: docs/archive/execution/node-library.md

```bash
cargo test -p wamn-standard-nodes             # nodes + policy negatives + purity lint
cargo test -p wamn-runner            # taxonomy re-export + port drift-guard regression
cargo clippy -p wamn-standard-nodes --all-targets \
  && cargo fmt -p wamn-standard-nodes --check
```

### [5.4] deleted node contract/SDK scaffolding provenance

The former node contract, SDK, guest, and manifest scaffolding were deleted by
`.6.3`. No runnable commands remain for those packages.

### [5.7] run-state persistence (crates/execution/run-state)

Docs: docs/archive/execution/run-state.md

```bash
cargo test -p wamn-run-state
cargo test -p wamn-runner   # single-shot execution, context, retry, and budget semantics
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

The former occurrence-keyed child-state/runtime recipes are historical only.
`wamn-0h0g.4.4` owns deletion of that retained pre-MVP machinery; the current
global-FIFO gate neither revives nor claims `child_live` as runnable evidence.

### [5.7-admission-pin / wamn-cox] production claim records the release it runs under

Docs: docs/archive/execution/run-state.md

```bash
# The host-owned production claim is lock -> classify -> lease, plus one
# per-attempt record of (release version, manifest digest) taken from the
# claiming pod's identity (wamn-0h0g.15.10, .15.11). Resolution is a pure read
# of that pod's mounted release manifest; the admission-time pin is gone.
cargo test -p wamn-run-state
```

### [SR-MVP / wamn-0h0g.4.5] rerun/reconstruction deletion

This debug-only gate proves that execution is single-shot, retired rerun and
restore APIs are absent, from-zero DDL has no execution-lineage columns, and a
populated project schema drops only the guarded legacy lineage metadata while
preserving every run row and the trusted event-causation spine.

```bash
export CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-4-5
export CARGO_INCREMENTAL=0
cargo test --locked --offline \
  -p wamn-runner -p wamn-run-state -p wamn-schema-control
cargo test --locked --offline -p wamn-proof-integration --lib \
  contextproof::tests
cargo test --locked --offline -p wamn-proof-integration --lib \
  flowbench::tests
cargo test --locked --offline -p wamn-proof-conformance --lib schema_drift

WAMN_CTL_PG_URL="$THROWAWAY_PG_URL" \
  cargo test --locked --offline -p wamn-ctl --test run_plane_live \
    rerun_lineage_cutover_live -- --exact --nocapture --test-threads=1
WAMN_CTL_PG_URL="$THROWAWAY_PG_URL" \
  cargo test --locked --offline -p wamn-ctl --test run_plane_live \
    run_plane_reconcile_live -- --exact --nocapture --test-threads=1

cargo test --locked --offline -p wamn-proof-conformance \
  --test effect_provider_revision -- --test-threads=1
cargo test --locked --offline -p wamn-execution-host trusted_runtime_revision_
cargo clippy --locked --offline \
  -p wamn-runner -p wamn-run-state -p wamn-schema-control -p wamn-ctl \
  -p wamn-runtime -p wamn-proof-conformance -p wamn-proof-integration \
  -p wamn-test-fixtures --all-targets -- -D warnings
cargo fmt --all -- --check
jq empty architecture/state-owners.json architecture/protected-writes.json \
  crates/execution/host/effect-provider-revision.json
git diff --check
```

### [5.9] credential vault (plugins/wamn_credentials)

Docs: docs/archive/data-path/credential-vault.md

```bash
# Pure units: http-request injection/classification and host vault resolution.
cargo test -p wamn-standard-nodes
# Unit boundary: the credential plugin moved to wamn-runtime.
# recipe-test: H5-CREDENTIALS | unit | wamn-runtime | lib | - | plugins::wamn_credentials::tests:: | 3 | crates/platform/runtime/src/plugins/wamn_credentials.rs native vault parsing and project-scoped lookup
cargo test -p wamn-runtime --lib plugins::wamn_credentials::tests::
```

### [5.14] durable run queue & runner scaling (crates/execution/run-state)

Docs: docs/archive/execution/run-queue.md

```bash
cargo test -p wamn-run-state -p wamn-scheduler
cargo clippy -p wamn-run-state -p wamn-scheduler --all-targets \
  && cargo fmt -p wamn-run-state -p wamn-scheduler --check
# Historical only: the former queuebench executable and its deployed Job were
# archived by wamn-0h0g.4.1 and are not runnable against the global-FIFO source.
# Use the focused production-claim live test plus the retained runnerbench
# handoff recipe below.
```

Trusted event lineage in runner execution input has its own focused campaign.
It proves the combined claim selector, split dispatch selector, and flowrunner
context declaration while restoring each mutated source byte-exactly:

```bash
cargo test --locked -p wamn-proof-conformance --test gate_mutation_evidence
# The former receipt was de-claimed when the runner was repinned without
# rerunning its mutants; bd:wamn-2jdm.5 owns a new immutable receipt.
```

D20/R6 ordering-policy evidence is historical. wamn-0h0g.4.1 deleted that
authored/storage/runtime plane; its former proof modes are not current gates.

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

### [EVT-C7 / wamn-z7b.1] queue ceiling campaign — archived measurement

Docs: docs/archive/results/ceilings.md (the published curves) + docs/archive/events/event-plane-jetstream.md §10/§11

The published curves remain historical evidence. The executable, long-running
mode, and deployed Jobs were archived by wamn-0h0g.4.1; there is no current
rerun command for this measurement.

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

Docs: docs/archive/events/event-plane-jetstream.md §4. The CDC reader MVP:
`wamn-cdc-reader --org --project --env` reads its `registry.event_readers`
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

Historical proof record. The reader still resolves a relation OID through the
`wamn_entities` map and uses the stable entity id in mapped envelopes and
subjects. The destructive live rename drill was retired by
`wamn-0h0g.9.5`: default `migrate-catalog` is additive-only, so a table rename is
no longer a supported MVP operation. This section is not a runnable rename
recipe.

```bash
cargo test -p wamn-event-wire                # +unmapped-marker + entity/table wire pin
cargo test -p wamn-control-provision entity_map      # the OID-keyed upsert drift guard ($2::text)
cargo test -p wamn-cdc-reader --lib          # +entity_lookup_sql pin, +map-order bundle test
```

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
```

The former runnerbench live drive depended on guest-side claim and execution,
so its runnable recipe was archived by wamn-0h0g.4.1. The retained runnerbench
below stops at host-owned claim handoff; it does not prove causation emission.

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

docker build --target gates -t wamn-gates:dev .
kind load docker-image wamn-gates:dev --name wamn
kubectl -n wamn-system delete job causation-e2e --ignore-not-found
kubectl -n wamn-system apply -f deploy/gates/causation-e2e-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/causation-e2e --timeout=240s
kubectl -n wamn-system logs job/causation-e2e  # -> overall PASS: true
```

### [EVT-REG / wamn-l5i9.16] registration surface — catalog + minimal API

Docs: docs/archive/events/event-plane-jetstream.md §5. The **declaration surface** the
materializer (l5i9.17) consumes: a registration = subscribing flow id, entity id
(the rename-proof catalog **entity id**, EVT-OIDMAP — never a table name), a
non-empty op set, and an optional JMESPath condition. Model + validation in the
pure `wamn-event-reg` crate;
storage `catalog.event_registrations` (deploy/sql/catalog-schema.sql, mirrors
`rls_policies` — jsonb doc + denormalized `flow_id`/`entity_id` columns, live-
catalog-scoped not version-tied, tenant-RLS'd, indexed by entity for 11.8 impact
analysis wamn-wvb); minimal CRUD builders in `wamn-api` (`registration` module —
pinned identifiers, `$n` values, `tenant_id` server-side). NO materializer, NO
reader change, NO UI (parked). The condition is stored as a JMESPath string and
validated for SYNTAX at write time (the materializer owns evaluation). The
pre-release 0.1 declaration is an exact allowlist: the retired `partition-key`
field is refused rather than ignored. A
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

### [CALLABLE-FLOWS-POC-F1 / wamn-3rj] F1 shared fixture coherence

The shared F1 fixture retains its independent catalog, flow, seed, and burst
coherence check.

```bash
# recipe-test: H5-F1-FIXTURE | system | wamn-test-fixtures | lib | - | f1fixture::tests:: | 1 | shared F1 catalog, flow, seed, and burst fixture coherence
cargo test --locked -p wamn-test-fixtures --lib f1fixture::tests::
```

### [11.8 / wamn-wvb] schema-change impact analysis — affected flows/API

Docs: docs/archive/testing/impact-analysis.md. Before a migration applies,
enumerate the dependency graph a change touches: affected entities
(additive/destructive, from the plan's per-operation attribution) → flows via
event registration (id-keyed, rename-proof) and node config (name-keyed, not
rename-proof) → generated-API resources. The pure decision is
`crates/schema/control/src/impact` (`analyze` → `ImpactReport`); its reads
live in `crates/schema/control/src/sql.rs`. `wamn-ctl-ops impact-report` is
the read-only operations surface. The default `migrate-catalog` command is
additive-only and neither loads impact analysis nor offers a destructive
override.

```bash
cargo test -p wamn-schema-control --features ops        # pure ops decision + drift-guard pins (3 mutants killed here)
cargo test -p wamn-ctl --features ops                    # operations driver units
cargo clippy -p wamn-schema-control -p wamn-ctl --features ops --all-targets
cargo clippy -p wamn-gates --all-targets
# Live gate (throwaway PG): materialize v1 {orders, audit}, seed a dependent flow
# per entity (registration + active node-config graph), stage v2 =
# destructive-on-orders (drop column) + additive-on-audit (add column) → the report
# names EXACTLY orders' flow/api and NOT audit's; destructive changes carry
# reprovision guidance while additive changes do not. Hermetic:
docker run -d --name wave-wvb-pg -p 15502:5432 -e POSTGRES_PASSWORD=pg postgres:18
WAMN_CTL_PG_URL=postgres://postgres:pg@127.0.0.1:15502/postgres \
  cargo test -p wamn-ctl --features ops --test impact_report_live -- --nocapture
# In-cluster gate-of-record candidate: the analysis in an ephemeral schema
# (name-keyed node-config + api edges; destructive carries reprovision
# guidance, additive does not):
WAMN_CTL_OPS_BIN=./target/debug/wamn-ctl-ops \
WAMN_PG_URL=postgres://wamn_app:wamn_app@127.0.0.1:15502/postgres \
WAMN_PG_ADMIN_URL=postgres://postgres:pg@127.0.0.1:15502/postgres \
  ./target/debug/wamn-gates --log-level error impactproof
docker rm -f wave-wvb-pg
# IN-CLUSTER: deploy/gates/impactproof-job.yaml (kubectl apply; wait complete; logs).
# 3 mutants killed (apply/test/restore, debug builds): M1 entity-match inverted →
# wamn-schema-control untouched_entity_flows_are_not_reported; M2 destructive classification
# forced additive → destructive_change_with_impact_keeps_both_facts; M3
# node-config keyed on entity.id not name → node_config_edge_keys_on_entity_name_not_id.
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

### Deleted samplebench/js-sample provenance

The former `js-sample` adopter component and samplebench recipe were deleted by
`.6.3`. There is no current runnable samplebench command.

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

Historical only. The failoverbench executable and Job were archived by
wamn-0h0g.4.1. Current reclaim-classifier coverage lives in the focused
run-state/runtime PostgreSQL tests recorded by that issue's gate.

### [5.14] guest-self-claim

Docs: docs/archive/execution/run-queue.md

Historical only. The guest-owned claim export was deleted before wamn-0h0g.4.1;
the production transaction is host-only and the guest exposes only
`run(run-id, payload)`.

### [5.14 / wamn-fqg.9] guest-side partitioned claim — retired

Historical design record: the issue/commit history preceding the current
global-FIFO contract in docs/archive/execution/run-queue.md.

This section preserves provenance for the removed guest claim design. Its APIs,
storage, and proof executable are absent from the current source tree.

There is no current runnable recipe. The guest claim path and its proof modes
were deleted by wamn-0h0g.4.1 together with the ordering plane.

The earlier mode and mutation details remain recoverable from the cited issue
and commits; they are not current gates.

### [5.14 / wamn-0h0g.4.1] host-owned global-FIFO claim, recovery, and cutover

Docs: docs/archive/execution/run-queue.md · Manifests: deploy/platform/runner.yaml + deploy/platform/runner-db.example.yaml

```bash
# The flow-engine runaway budget remains independent of the retired guest claim
# and partition proofs.
# recipe-test: H5-RUNNER-BUDGET | integration | wamn-runner | test | runner | a_runaway_cycle_fails_at_exactly_the_budget | 1 | flow-engine runaway dispatch budget is exact and load-bearing
cargo test --locked -p wamn-runner --test runner \
  a_runaway_cycle_fails_at_exactly_the_budget -- --exact

cargo test --locked -p wamn-proof-integration --lib runnerbench::tests::
cargo build --locked -p wamn-gates
cargo build --locked --manifest-path components/Cargo.toml \
  --target wasm32-wasip2 -p flowrunner
docker run -d --name wamn-fqg8-pg -p 5490:5432 -e POSTGRES_PASSWORD=postgres postgres:18
until docker exec wamn-fqg8-pg pg_isready -U postgres; do sleep 1; done
docker exec wamn-fqg8-pg psql -U postgres -c \
  "CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB NOBYPASSRLS;"
./target/debug/wamn-gates --log-level warn runnerbench \
  --flowrunner components/target/wasm32-wasip2/debug/flowrunner.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5490/postgres \
  --admin-database-url postgres://postgres:postgres@127.0.0.1:5490/postgres
# Expected: fifo-a, fifo-b, fifo-z; three exact payloads; generation 1; three
# complete resolution maps; three live host-owned leases; then an empty claim.
# The schema is dropped immediately afterward. No guest call, dequeue, or
# completion is asserted while .5.4 keeps interpretation hard-refused.
docker rm -f wamn-fqg8-pg
```

This is a local handoff proof, not an in-cluster execution gate. Production
guest execution becomes a valid gate only after wamn-0h0g.5.4 activates the
effect-attempt interpreter path.

#### Final debug gate of record

This acceptance gate is debug/offline only. `WAMN_41_CLAIM_PG_URL`,
`WAMN_41_WRITER_PG_URL`, and `WAMN_41_RUNSTATE_PG_URL` must be superuser URLs
for three separate disposable PostgreSQL 18 databases. The retired
`child_live` fixture is deliberately excluded: callable-child deletion belongs
to `wamn-0h0g.4.4`. No guest interpretation, release build, image, or
live-cluster gate occurs before `wamn-0h0g.5.4`.

```bash
: "${WAMN_41_CLAIM_PG_URL:?set to a disposable PostgreSQL 18 database}"
: "${WAMN_41_WRITER_PG_URL:?set to a second disposable PostgreSQL 18 database}"
: "${WAMN_41_RUNSTATE_PG_URL:?set to a third disposable PostgreSQL 18 database}"
export CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-4-1
export CARGO_INCREMENTAL=0
export CARGO_BUILD_JOBS=2

cargo test --locked --offline \
  -p wamn-flow -p wamn-event-reg -p wamn-materializer -p wamn-run-state \
  -p wamn-runtime -p wamn-execution-host -p wamn-executor -p wamn-dispatcher \
  -p wamn-schema-control -p wamn-control-provision -p wamn-ctl \
  -p wamn-proof-integration -p wamn-proof-conformance -p wamn-pg-core \
  -p wamn-schema-compiler --features wamn-run-state/native,wamn-ctl/ops
cargo test --locked --offline -p wamn-runner --test runner \
  a_runaway_cycle_fails_at_exactly_the_budget -- --exact
cargo clippy --locked --offline \
  -p wamn-flow -p wamn-event-reg -p wamn-materializer -p wamn-run-state \
  -p wamn-runtime -p wamn-execution-host -p wamn-executor -p wamn-dispatcher \
  -p wamn-schema-control -p wamn-control-provision -p wamn-ctl \
  -p wamn-proof-integration -p wamn-proof-conformance -p wamn-pg-core \
  -p wamn-schema-compiler --features wamn-run-state/native,wamn-ctl/ops \
  --all-targets -- -D warnings

CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-4-1-components \
  cargo test --locked --offline --manifest-path components/Cargo.toml \
    -p materializer
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-4-1-components \
  cargo clippy --locked --offline --manifest-path components/Cargo.toml \
    -p materializer --target wasm32-wasip2 -- -D warnings

WAMN_PRODUCTION_CLAIM_PG_URL="$WAMN_41_CLAIM_PG_URL" \
  cargo test --locked --offline -p wamn-runtime --test production_claim_live \
    production_claim_live -- --ignored --exact --nocapture --test-threads=1
WAMN_CTL_PG_URL="$WAMN_41_CLAIM_PG_URL" \
  cargo test --locked --offline -p wamn-ctl --test run_plane_live \
    run_plane_reconcile_live -- --exact --nocapture --test-threads=1
WAMN_EFFECT_WRITER_PG18_URL="$WAMN_41_WRITER_PG_URL" \
  cargo test --locked --offline -p wamn-ctl --features ops \
    --test effect_writer_generation_live \
    effect_writer_generation_lifecycle_is_exact_and_fail_closed \
    -- --ignored --exact --nocapture --test-threads=1
WAMN_RUN_STORE_PG_URL="$WAMN_41_RUNSTATE_PG_URL" \
  cargo test --locked --offline -p wamn-run-state --features native \
    --test effect_writer_live native_effect_writer_live \
    -- --ignored --exact --nocapture --test-threads=1
WAMN_RUN_STORE_PG_URL="$WAMN_41_RUNSTATE_PG_URL" \
  cargo test --locked --offline -p wamn-run-state --test admission_live \
    admission_live -- --ignored --exact --nocapture --test-threads=1

# Regenerate only because this cutover intentionally changes the inventory;
# the ordinary run immediately afterward is the drift gate.
WAMN_UPDATE_PROTECTED_RELATIONS=1 WAMN_CTL_PG_URL="$WAMN_41_RUNSTATE_PG_URL" \
  cargo test --locked --offline -p wamn-ctl --features ops \
    --test protected_relations_live -- --nocapture --test-threads=1
WAMN_CTL_PG_URL="$WAMN_41_RUNSTATE_PG_URL" \
  cargo test --locked --offline -p wamn-ctl --features ops \
    --test protected_relations_live -- --nocapture --test-threads=1
cargo test --locked --offline -p wamn-proof-conformance \
  --test protected_relations --test state_ownership


jq empty architecture/protected-writes.json \
  docs/archive/contracts/flow-schema.schema.json \
  crates/execution/host/effect-provider-revision.json
cargo fmt --all -- --check
cargo fmt --manifest-path components/Cargo.toml --all -- --check
git diff --check
```

### [POC-F3] scale-to-zero / parked-project wake — historical

The former `wakeproof` gate required dispatcher-owned cron admission and guest
completion. The retained dispatcher is now reconciliation/wake-hint only, and
guest interpretation remains hard-refused until `wamn-0h0g.5.4`; this recipe is
therefore not runnable current evidence. `wamn-0h0g.5.8` owns the retained
wake-from-zero behavior, and `wamn-0h0g.11.19` owns absorption of the surviving
assertions into the M2 gate.

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

The former multi-mode `dispatchbench` executable was archived by
`wamn-0h0g.4.1`. Current centralized admission and queue behavior are covered by
the dispatcher units, run-plane gate, and host-owned production-claim gate.

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

### [SR-MVP / wamn-0h0g.5.8] dispatcher reconciliation

This debug gate proves three local boundaries: reconciliation reads only the
tenant-scoped project queue, claiming remains owned by the executor, and the
dispatcher has project-database access without Kubernetes scale authority while
the waker has the Kubernetes scale credential without a database credential. The
`unscoped-literal-queue-scan` mutant is killed by
`dispatcher_reconciliation_is_tenant_scoped_and_read_only`.

```bash
export CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-5-17
export CARGO_INCREMENTAL=0
export CARGO_BUILD_JOBS=2

cargo test --locked --offline -p wamn-dispatcher
cargo test --locked --offline -p wamn-scheduler
cargo test --locked --offline -p wamn-run-state --test queue \
  dispatcher_reconciliation_mirrors_claim_eligibility_and_order -- --exact
cargo test --locked --offline -p wamn-proof-conformance \
  --test dispatcher_boundary

cargo clippy --locked --offline \
  -p wamn-dispatcher -p wamn-scheduler -p wamn-run-state \
  --all-targets -- -D warnings
cargo clippy --locked --offline -p wamn-proof-conformance \
  --test dispatcher_boundary -- -D warnings

cargo fmt --manifest-path Cargo.toml \
  -p wamn-dispatcher -p wamn-scheduler -p wamn-run-state \
  -p wamn-proof-conformance -- --check
git diff --check
```

The in-cluster M2 scale-from-zero proof belongs to `wamn-0h0g.11.11`.
Historical wakeproof absorption belongs to `wamn-0h0g.11.19`; neither is
claimed by this local gate.

### [SR-MVP / wamn-0h0g.3.8] root-frame execution budgets

This debug-only owner gate proves `MAX_CALL_DEPTH = 64` and
`DEFAULT_ROOT_DISPATCH_BUDGET = 10_000` across one complete root frame stack.
It pins debit-before-input-guard-before-depth-before-frame-allocation ordering,
the exact 65th-callee and 10,001st-dispatch refusals, root-global accounting,
non-catchable terminal propagation, and the persisted `depth-budget` and
`dispatch-budget` mappings. It does not activate guest execution or claim
production/get-run visibility; those remain owned by `wamn-0h0g.5.4`. The
reducer's independent direct-Plan runaway-budget test in the
`wamn-0h0g.4.1` section remains separate.

```bash
export CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-3-9
export CARGO_INCREMENTAL=0
export CARGO_BUILD_JOBS=2

cargo test --locked --offline --manifest-path components/Cargo.toml \
  -p flowrunner frames::tests::
cargo test --locked --offline -p wamn-runner -p wamn-run-state
cargo test --locked --offline -p wamn-run-state --test store \
  root_budget_failures_map_to_exact_persisted_kinds -- --exact

cargo clippy --locked --offline -p wamn-runner -p wamn-run-state \
  --all-targets -- -D warnings
cargo clippy --locked --offline --manifest-path components/Cargo.toml \
  -p flowrunner --all-targets -- -D warnings -A dead-code
cargo clippy --locked --offline --manifest-path components/Cargo.toml \
  -p flowrunner --target wasm32-wasip2 -- -D warnings -A dead-code

cargo fmt --manifest-path Cargo.toml \
  -p wamn-runner -p wamn-run-state -- --check
cargo fmt --manifest-path components/Cargo.toml -p flowrunner -- --check
git diff --check
```

### [SR-MVP / wamn-0h0g.8.3] admitted full|off capture

Docs: docs/archive/execution/run-state.md § *Node-level I/O capture (9.6)*

```bash
# Pure contract, projection, SQL builders, and schema guards. Full capture is
# scrub-redacted; off records no node payload facts; over-ceiling output records
# size/hash and projects output-too-large without a read-side ceiling lookup:
cargo test -p wamn-flow -p wamn-authoring-model -p wamn-run-state
cargo clippy -p wamn-flow -p wamn-authoring-model -p wamn-run-state -p wamn-schema-control -p wamn-ctl -p wamn-gates --all-targets -- -D warnings
# Regenerate both published schemas (identities remain 0.1):
cargo run -p wamn-flow --example print-flow-schema > docs/archive/contracts/flow-schema.schema.json
cargo run -p wamn-authoring-model --example print-authoring-surface-schema \
  > docs/archive/contracts/authoring-surface.schema.json
node clients/authoring-client/scripts/generate.mjs
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
# capturebench phases cover off writes, oversized output -> NULL payload +
# size/hash + typed output-too-large, full-capture redaction, and retention.
# Retention verb (deployed per project-env; app-role, tenant-scoped DELETE):
#   wamn-ctl-ops prune-run-history --schema <run-schema> --tenant <t> --retention-days 30 [--dry-run]

# Nine byte-pinned mutants cover both fail-closed defaults, the draft-only full
# constraint, admission immutability, author-SQL capture denial, capture-off payload suppression, the
# write-side output ceiling, derived output-too-large projection, and full-capture
# redaction. The draft-capture default is gated by the authoring contract test
# draft_run_capture_defaults_to_full_and_accepts_only_full_or_off (wamn-0h0g.15.121).
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
cargo test -p wamn-control-provision --features ops   # dump builders + ops artifact drift
cargo test -p wamn-control-registry
cargo test -p wamn-ctl --features ops
cargo clippy -p wamn-control-provision -p wamn-ctl --features ops --all-targets
cargo clippy -p wamn-control-registry --all-targets \
  && cargo fmt -p wamn-control-provision -p wamn-control-registry -p wamn-ctl --check
# Render locally (no DB — the cadence is --schedule, default daily 03:00):
./target/debug/wamn-ctl-ops dump-project-env --org demo --project app --env prod \
  --emit-cronjob - --emit-job -
# optional live gates (throwaway postgres:18; superuser url): (a) the ARTIFACT
# idempotent + byte_size-refresh proof rides the wamn-q3n.3 storage gate:
docker run -d --rm --name wamn-dump-pg -p 5462:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_DUMP_PG_URL=postgres://postgres:postgres@127.0.0.1:5462/wamn \
  cargo test -p wamn-control-provision --features ops --test dump
WAMN_REGISTRY_PG_URL=postgres://postgres:postgres@127.0.0.1:5462/wamn cargo test -p wamn-control-registry
docker stop wamn-dump-pg
# IN-CLUSTER gate of record (the .6/.7/.9 precedent; T3 pool wamn-pg + T1 wamn-sysdb
# (writing the T1 registry's OWN DB IS .10's job; NEVER touch wamn-pg/postgres.yaml).
# The ops command installs deploy/sql/ops-schema.sql after core, then executes
# state writes as the ACL-bounded wamn_ops role.
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
WAMN_SYSTEM_ADMIN_URL="$SYS" ./target/debug/wamn-ctl-ops dump-project-env --org t10gate --project demo --env dev \
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
cargo test -p wamn-control-provision --features ops   # restore builders + ops artifact drift
cargo test -p wamn-control-registry
cargo test -p wamn-ctl --features ops
cargo clippy -p wamn-control-provision -p wamn-ctl --features ops --all-targets
cargo clippy -p wamn-control-registry --all-targets \
  && cargo fmt -p wamn-control-provision -p wamn-control-registry -p wamn-ctl --check
# Render/plan locally (no cluster/DB needed — explicit --dump-dir, render only):
./target/debug/wamn-ctl-ops restore-project-env --org demo --project app --env dev \
  --database-url postgres://postgres:postgres@127.0.0.1:5468/postgres \
  --dump-dir /tmp/some-dump --help >/dev/null   # (see the subcommand flags)
# optional live gates (throwaway postgres:18; superuser url): (a) the restore
# wamn-q3n.3 storage gate:
docker run -d --rm --name wamn-restore-pg -p 5468:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_RESTORE_PG_URL=postgres://postgres:postgres@127.0.0.1:5468/wamn \
  cargo test -p wamn-control-provision --features ops --test restore
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
WAMN_SYSTEM_ADMIN_URL="$SYS" ./target/debug/wamn-ctl-ops dump-project-env --org t11gate --project demo --env dev \
  --database-url "postgres://postgres:${PGPW}@127.0.0.1:5477/${DB}?sslmode=disable" --run-now --out-dir "$DUMPROOT"
WAMN_SYSTEM_ADMIN_URL="$SYS" ./target/debug/wamn-ctl-ops restore-project-env --org t11gate --project demo --env dev \
  --database-url "$PGADMIN" --dump-root "$DUMPROOT"   # reads the catalog -> scratch DB
# row (mutate live -> restore -> stale gone):
psql "postgres://postgres:${PGPW}@127.0.0.1:5477/wamn-restore-t11gate--demo--dev?sslmode=disable" \
  -tAc "SELECT count(*), sum(weight_kg) FROM parts;"
psql "postgres://postgres:${PGPW}@127.0.0.1:5477/${DB}?sslmode=disable" -c "INSERT INTO parts VALUES (99,'STALE',9.999);"
WAMN_SYSTEM_ADMIN_URL="$SYS" ./target/debug/wamn-ctl-ops restore-project-env --org t11gate --project demo --env dev \
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
cargo test -p wamn-control-provision --features ops copy  # clone/cutover plan + ops state
cargo test -p wamn-control-provision --features ops --test ops_storage
cargo test -p wamn-schema-control --features ops          # target reconciliation planner
cargo test -p wamn-ctl --features ops                     # copy driver + window re-verification
cargo clippy -p wamn-control-provision -p wamn-schema-control -p wamn-ctl --features ops --all-targets
cargo clippy -p wamn-control-registry --all-targets \
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
WAMN_REGISTRY_PG_URL=$U cargo test -p wamn-control-provision --features ops --test ops_storage
WAMN_MIGRATE_PG_URL=$U cargo test -p wamn-schema-control --features ops --test migrate
WAMN_DUMP_PG_URL=$U WAMN_RESTORE_PG_URL=$U WAMN_PROVISION_PG_URL=$U \
  cargo test -p wamn-control-provision --features ops
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
cargo test -p wamn-control-provision --features ops   # backup renderer + policy knobs
cargo test -p wamn-ctl --features ops                 # operations command wiring
cargo clippy -p wamn-control-provision -p wamn-ctl --features ops --all-targets
cargo clippy -p wamn-control-registry -p wamn-gates --all-targets \
  && cargo fmt -p wamn-control-provision -p wamn-ctl -p wamn-control-registry -p wamn-gates --check
# Render a dedicated org's backup CRs locally (no cluster/DB needed; the prod
# policy's backup_cadence/wal_retention drive the CRs):
cargo run -p wamn-ctl --features ops --bin wamn-ctl -- provision-org \
  --org demo --template standard \
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
env -u WAMN_SYSTEM_ADMIN_URL cargo run -p wamn-ctl --features ops --bin wamn-ctl -- \
  provision-org --org e1gate --template standard \
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

```bash
cargo test --locked -p wamn-scenario-worker -p wamn-schema-control -p wamn-ctl
# The authoring-model tests bake CARGO_MANIFEST_DIR, so they need their own
# target directory when a shared cache is in use.
cargo test --locked -p wamn-authoring-model
cargo test --locked -p wamn-proof-conformance --test state_ownership
cargo clippy --locked -p wamn-scenario-worker -p wamn-authoring-model \
  --all-targets -- -D warnings
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
   `<run-schema>.{authoring_test_run_reservations,authoring_test_case_runs,
   authoring_test_reports}`. `wamn-ctl reconcile-run-plane` creates all of those
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
  `deploy/platform/scenario-worker.yaml` ("no chart ships it"). `serve()`
  connects it before the authority probe runs.
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

⚠️ Current `reconcile-run-plane` never backfills effect authority from a mutable
`node_runs` projection. The `.4.9` writer-boundary cutover physically removes
named retired projection columns, but any populated incompatible immutable
attempt/dispatch/outcome ledger refuses before DDL with
`effect-writer-cutover-requires-empty-ledger`. Reset or explicitly archive that
pre-MVP fixture; never synthesize provenance or choose a legacy successor.

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
wamn-jvzx.2 generated client. Three verbs cover the four public command kinds:
`validate` sends `save-flow-draft` then `validate`, `draft-run` sends itself,
and `promote` sends `publish`. Two gates own it.

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
`validate`, edit it again, `validate`, then `draft-run` and `promote`, each one
a subprocess invocation of the shipped CLI whose stdout document is the result.
Like the wamn-jvzx.4 smoke it is PURE HTTP: its whole input surface is
`--base-url`, ONE `--credential` file, `--project`,
`--environment`, and an optional `--checkout`. It holds no database URL, no
platform-admin impersonation, and no test-only trusted context, so it cannot read
the ledger it is proving — it prints one `VERIFY-MANIFEST` line instead and the
runner does that read below.

**HONEST 501s, AND WHY THE GATE STILL PASSES.** The management surface mounts the
command kinds whose handlers have landed and answers a bare `501` for the rest
(the per-kind mount work owns mounting the remainder; wamn-ftfc.22 closed
having proven every remaining backend absent). Each cycle step therefore
asserts the CONTRACT shape of whatever answer it gets — `completed` must carry
that command's required identity fields, `refused` must carry a typed reason,
`unmounted` must be a bare `501` with no document — and a `fault` fails the
gate. The two saves are
required to complete at revisions 1 and 2, because that is what proves
working-tree content reaching the canonical save handler through optimistic
concurrency. The run then prints `CYCLE-COMPLETED` and `CYCLE-UNMOUNTED-501`, so
the record says exactly which steps a surface answered and which it did not.
While `validate` or `draft-run` is unmounted there is no validated-draft or
report identity to carry forward, so the downstream legs present a
contract-shaped placeholder purely to reach the transport; on a surface that
mounts them the real identity flows instead, and `promote` then answers
`completed` or a typed refusal.

**EDIT-TO-RUN LATENCY.** The CLI measures it where a checkout client can: from the
modification time of the definition file it submitted to the arrival of a run
receipt, printed as `edit-to-run-ms` on stderr and carried in the stdout
document. Until `draft-run` is mounted no receipt exists, and the gate prints
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
(cd clients/authoring-client && node scripts/generate.mjs --check && node scripts/test.mjs)
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
```

The result of record must enumerate exactly the four retained command kinds.
The management surface currently mounts `save-flow-draft`; every other leg must
either return its typed result/refusal or the honest bare `501` documented by
the cycle. The runner-side ledger read must still find exactly the two
authorized saves, no refused command ids, byte-identical revision 2 content,
and no credential material. Both mutants must print `KILLED`.

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
cargo test -p wamn-schema-control     # additive planner + lifecycle/drift guards
cargo test -p wamn-schema-control --features ops  # internal target reconciliation + impact
cargo test -p wamn-ctl --lib migrate_catalog   # the subcommand's bare-ident + param-map units
# Static proof spans the decision library and the ctl service library that owns
# migrate-catalog; the binary-only host is outside this boundary.
cargo clippy -p wamn-schema-control -p wamn-ctl --all-targets \
  && cargo fmt -p wamn-schema-control -p wamn-ctl --check
# optional live-apply gate (throwaway postgres:18; superuser url — provisions
# unset):
docker run -d --rm --name wamn-schema-control-pg -p 5467:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
WAMN_MIGRATE_PG_URL=postgres://postgres:postgres@127.0.0.1:5467/wamn \
  cargo test -p wamn-schema-control --features ops --test migrate
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

### [4.1b] catalog snapshot publish + API fixture

```bash
# Unit/fixture boundaries: publish-catalog belongs to wamn-ctl and the API
# fixture lives in repository test support.
# recipe-test: H5-API-PUBLISH | unit | wamn-ctl | lib | - | publish_catalog::tests:: | 1 | services/ctl/src/publish_catalog.rs pre-I/O schema boundary
cargo test -p wamn-ctl --lib publish_catalog::tests::
# recipe-test: H5-API-FIXTURE | fixture | wamn-test-fixtures | lib | - | apifixture::tests:: | 2 | test-support/fixtures/apifixture.rs API catalog and floor coherence
cargo test -p wamn-test-fixtures --lib apifixture::tests::
cargo clippy -p wamn-host -p wamn-ctl -p wamn-gates --all-targets \
  && cargo fmt -p wamn-host -p wamn-ctl -p wamn-gates --check
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

The former callable-flow aggregate mutation campaign and state-probe helper are
not present in the MVP-reduced tree. The retained callable-flow recipes above
remain the current runnable gates; there is no current aggregate mutation
command or checked-in aggregate evidence record in this file.

### Deleted time-shift component provenance

The former `time-shift` component recipe was deleted by `.6.3` with the
non-retained component set. There is no current runnable `time-shift` command.

### Deleted POC-F4 custom-node provenance

The former F4 `disposition-node`, serve-node, nodeinvoke, and signed custom-node
transport recipes were deleted by `.6.3`. This entry is provenance only; there
is no current runnable F4 custom-node recipe in this file.

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
immutable effect ledgers, catalog publication provenance, the locked empty-only
writer-boundary cutover, the pre-l5i9.19 outbox-era teardown, and
catalog-schema from-zero. No retained table or row is rewritten or deleted;
unknown columns remain surfaced, while named retired identity/recovery columns
are physically removed only by their locked cutovers. No effect fact is
backfilled or fabricated.
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
# Hermetic legs include shared-runner legacy; empty writer-boundary cutover and
# populated-ledger refusal;
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

### [EVT-C-E2E / wamn-l5i9.22] e2ebench — RETIRED (l5i9.19 teardown)

The C-E2E campaign of record stands in docs/archive/results/ceilings.md § C-E2E +
docs/results/ceilings-data/ (ce2e-*.csv): the one before/after chart (commit→run-start
distribution, fan-out 1→N, 10× burst — outbox vs CDC at identical load). It
ran BEFORE the teardown by design (the measure-first ordering); the bench and
deploy/gates/e2ebench-job.yaml were deleted with the old path (D19 v3 §3,
executed 2026-07-20) — a before/after against a deleted path cannot be
re-measured, so the record is final. CDC-path regression coverage continues in
[EVT-MAT] (matbench) and [E10-E2E] (samplebench).

### Deleted node invocation provenance

The former runner-to-custom-node invocation, signed-envelope authn, and
nodeinvoke hardening recipes were deleted by `.6.3` with `wamn-node-invoke`,
`wamn-node-runtime`, `wamn-node-host`, `node-cred`, and the `serve-node`
manifests. These entries are provenance only; there is no current runnable
nodeinvoke recipe.

### Deleted R24 reconstruction and partial-rerun provenance

Per-visit occurrence identity remains on current node facts, but the former
`Plan::resume`, `seed_at`, persisted-frontier reconstruction, partial-rerun, and
runnerbench resume gates were deleted by `wamn-0h0g.4.5`. There is no current
runnable resume recipe. Forward occurrence and SQL-shape coverage remains in
the ordinary `wamn-runner` and `wamn-run-state` suites above.

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
# retained gates of record run sequentially; the former queue Job is archived.
```

### Deleted builder/buildproof provenance

The former custom-node builder, builder image, builder Job, signing key,
`sample-node` fixture, and buildproof recipe were deleted by `.6.3`. This entry
is provenance only; there is no current runnable builder/buildproof command.

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

## PLAN-0.2 authoritative runner artifact handoff (`wamn-2jdm.5.10`) — retired recipe

The guest-side artifact lookup and its mutation script were superseded by the
host-owned claim-time resolver. Use the wamn-0h0g.4.1 production-claim gates;
there is no current guest lookup command.

## Deleted custom-publish provenance (`wamn-5wd1.67`)

The former `publish-catalog --custom-node` path, supplied-component manifest
preflight, and F1/F2 custom-publish live proof were deleted by `.6.3`. This entry
is provenance only; there is no current runnable custom-publish recipe.

## CF-ATTEMPTS — historical pre-MVP effect protocol (`wamn-5wd1.54`)

This gate is superseded by `wamn-0h0g.4.9`. Its recovery-class, redispatch, and
outbound stable-key model is historical provenance only and is not runnable
current authority. The retained contract is one immutable attempt and at most
one first-insert-wins dispatch per effectful occurrence; the current gate of
record is **SR-MVP — inaccessible effect-writer primitive** below.

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
  -- --ignored --nocapture --test-threads=1
```

## SR-MVP — current-plan HTTP effect authority (`wamn-0h0g.2.5`)

The trusted internal HTTP envelope carries the run id plus the exact seven-field
attempt principal: root/current plan hashes, frame id, local node id, occurrence,
source artifact hash, and requirement name. The host requires the exact immutable
attempt row, membership of the current plan in the run's resolution map, the
effectful node and requirement in that plan's exact bytes, and the current
binding and active generation. It never walks a root authored graph or mutable
node projection. `.4.9` installs the inaccessible writer primitive without a
caller; until `.5.4` mints the write-ahead attempt and activates dispatch,
Flowrunner supplies no effect context and every send remains deny-only. All
package, WIT, wire, and schema identities remain `0.1`/`0.1.0`.

```bash
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-2-5 CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-run-state -p wamn-runtime
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-2-5 CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-runtime --test http_effect_wit_coherence
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-2-5 CARGO_INCREMENTAL=0 \
  cargo clippy --locked --offline -p wamn-run-state -p wamn-runtime \
    --all-targets -- -D warnings

CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-2-5-components CARGO_INCREMENTAL=0 \
  cargo test --locked --offline --manifest-path components/Cargo.toml -p flowrunner
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-2-5-components CARGO_INCREMENTAL=0 \
  cargo check --locked --offline --manifest-path components/Cargo.toml \
    -p flowrunner --target wasm32-wasip2

docker run -d --rm --name wamn-0h0g-25-pg \
  -p 127.0.0.1:15625:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
docker exec wamn-0h0g-25-pg pg_isready -U postgres -d wamn
WAMN_CONNECTION_EFFECT_PG_URL=postgresql://postgres:postgres@127.0.0.1:15625/wamn \
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-2-5 CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-runtime --lib \
    plugins::wamn_postgres::claims::tests::live_effect_authority_uses_callee_plan_and_exact_attempt \
    -- --ignored --exact --nocapture
docker rm -f wamn-0h0g-25-pg


rustfmt --edition 2024 --check \
  crates/execution/run-state/src/invocation_context.rs \
  crates/platform/runtime/src/plugins/connection_http.rs \
  crates/platform/runtime/src/plugins/wamn_postgres/claims.rs \
  crates/platform/runtime/tests/http_effect_wit_coherence.rs \
  components/execution/flowrunner/src/lib.rs
git diff --check
```

## SR-MVP — inaccessible effect-writer primitive (`wamn-0h0g.4.9`)

This gate deletes outbound stable-key retry and proves the host-only primitive:
strict scoped A/B credentials, exact ledger ACL/RLS, empty-only legacy cutover,
exact attempt/outcome retry, and first-insert-only dispatch permission. It does
not activate an effect caller or wire I/O; `wamn-0h0g.5.4` owns that integration.
The exact-hash campaign kills 23 named mutants, including the host `pg_temp`
search-path sentinel, Secret validity metadata, definitive unpublished-generation
abort, and the target database's PUBLIC `TEMPORARY` denial. All commands use the
shared per-wave debug target.

```bash
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-4-9 CARGO_INCREMENTAL=0 \
  cargo test --locked --offline \
    -p wamn-flow -p wamn-runner -p wamn-runtime -p wamn-proof-conformance
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-4-9 CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-run-state --features native
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-4-9 CARGO_INCREMENTAL=0 \
  cargo test --locked --offline \
    -p wamn-schema-control -p wamn-control-provision -p wamn-ctl \
    -p wamn-execution-host -p wamn-executor
bash deploy/mvp/tests/bootstrap.sh

CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-4-9 CARGO_INCREMENTAL=0 \
  cargo clippy --locked --offline \
    -p wamn-flow -p wamn-runner -p wamn-runtime -p wamn-proof-conformance \
    --all-targets -- -D warnings
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-4-9 CARGO_INCREMENTAL=0 \
  cargo clippy --locked --offline -p wamn-run-state --features native \
    --all-targets -- -D warnings
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-4-9 CARGO_INCREMENTAL=0 \
  cargo clippy --locked --offline \
    -p wamn-schema-control -p wamn-control-provision -p wamn-ctl \
    -p wamn-execution-host -p wamn-executor --all-targets -- -D warnings

CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-4-9-components CARGO_INCREMENTAL=0 \
  cargo test --locked --offline --manifest-path components/Cargo.toml -p flowrunner
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-4-9-components CARGO_INCREMENTAL=0 \
  cargo check --locked --offline --manifest-path components/Cargo.toml \
    -p flowrunner --target wasm32-wasip2

docker run -d --rm --name wamn-0h0g-49-pg \
  -p 127.0.0.1:15649:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=postgres postgres:18
docker exec wamn-0h0g-49-pg pg_isready -U postgres -d postgres
WAMN_EFFECT_WRITER_PG18_URL=postgresql://postgres:postgres@127.0.0.1:15649/postgres \
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-4-9 CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-ctl --features ops \
    --test effect_writer_generation_live \
    effect_writer_generation_lifecycle_is_exact_and_fail_closed \
    -- --ignored --exact --nocapture
WAMN_CTL_PG_URL=postgresql://postgres:postgres@127.0.0.1:15649/postgres \
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-4-9 CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-ctl --test run_plane_live \
    run_plane_reconcile_live -- --exact --nocapture
WAMN_RUN_STORE_PG_URL=postgresql://postgres:postgres@127.0.0.1:15649/postgres \
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-4-9 CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-run-state --features native \
    --test effect_writer_live native_effect_writer_live \
    -- --ignored --exact --nocapture

docker rm -f wamn-0h0g-49-pg

cargo fmt --all -- --check
git diff --check
```

## SR-MVP — protected-relation authority table (`wamn-0h0g.13.33`)

The checked-in table is generated from a disposable PostgreSQL 18 database.
`state-owners.json` supplies each relation's installer, lifecycle owner, and
core/ops source scope; the canonical reconciler plus the core control, ops,
application, and one-entity project installers materialize the relations. The
generator then opens a read-only
transaction and reads `pg_catalog` for mutation grants, RLS policies, cascades,
constraints, unique indexes, triggers, and trigger-function owners. Rust caller
search is deliberately out of scope. A `wamn_app` mutation grant is emitted as
`author SQL, RLS-bounded`. Each generated row carries `ops: false|true`; the
three true rows are `provisioning.dumps`, `provisioning.copy_sagas`, and
`provisioning.migration_confirmations`. This audit changes no production
permission and all schema, package, and wire identities remain `0.1`/`0.1.0`.

```bash
jq empty architecture/protected-writes.json

CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-13-33 CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-proof-conformance \
    --test protected_relations -- --nocapture
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-13-33 CARGO_INCREMENTAL=0 \
  cargo clippy --locked --offline -p wamn-proof-conformance \
    --test protected_relations -- -D warnings

# Start a disposable postgres:18 database first. Regeneration and verification
# use the same test; omit WAMN_UPDATE_PROTECTED_RELATIONS for the normal gate.
WAMN_UPDATE_PROTECTED_RELATIONS=1 \
WAMN_CTL_PG_URL=postgresql://postgres:postgres@127.0.0.1:15656/wamn \
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-13-33 CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-ctl --features ops \
    --test protected_relations_live -- --nocapture --test-threads=1
WAMN_CTL_PG_URL=postgresql://postgres:postgres@127.0.0.1:15656/wamn \
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-13-33 CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-ctl --features ops \
    --test protected_relations_live -- --nocapture --test-threads=1


git diff --check
```

## PLAN-2A — respond standard-node dispatch (`wamn-ayq7.20`)

`respond` resolves to the pinned platform standard-node executable, dispatches
through standard-node dispatch, and only then enters the engine-owned caller-release
transition. The mutation removes `respond` from the production standard-node
resolver and must make the focused debug gate fail before restoring the source.

```bash
CARGO_TARGET_DIR=/tmp/wamn-target-ayq7-20 \
  cargo test --locked -p wamn-runner -p wamn-catalog
CARGO_TARGET_DIR=/tmp/wamn-target-ayq7-20 \
  cargo test --locked --manifest-path components/Cargo.toml -p flowrunner
```

The `respond-node-abi` mutation runner was retired by `wamn-0h0g.15.122`. Both
halves of that guard were dead: `ResolvedNode::Standard` and
`wamn_nodes::is_standard` appear nowhere in the repository — the standard-node
resolver it guarded was deleted with the custom-component plane, not moved — and
its gate test no longer exists anywhere. There is no current runnable

## PLAN-2A — request standard-node dispatch (`wamn-ayq7.22`)

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
```

The `request-node-abi` mutation runner was retired by `wamn-0h0g.15.122`:
`validate_request_outcome` relocated to `crates/execution/flow-engine/src/engine.rs:801` (commit `e05636b8`) and its gate test no longer exists anywhere, so
both halves of that guard were dead. There is no current runnable

## PLAN-2A — cron standard-node dispatch (`wamn-ayq7.23`)

`cron` resolves to the pinned, capability-free platform standard-node
executable and emits the scheduler-admitted payload unchanged on `main`. The
flowrunner validates that exact payload, port, and absent context replacement
before the generic durable attempt checkpoint can advance the entry token.
Cron consumes one dispatch-budget unit, like every other standard-node execution;
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
```

The former `cron-node-abi` mutation runner was deleted with the non-retained
component-plane campaign; there is no current runnable cron-node-abi command.

## PLAN-2A — event standard-node dispatch (`wamn-ayq7.24`)

`event` resolves to the pinned, capability-free platform standard-node
executable and emits the externally admitted payload unchanged on `main`. The
flowrunner validates that exact payload, port, and absent context replacement
before the generic durable attempt checkpoint can advance the entry token.
Every inbound-edge dispatch consumes one dispatch-budget unit, including
event; admission, callerless lifecycle, and durable ordering
remain owned by the engine and driver. The mutation bypasses event's production
validation; the focused debug gate must fail before the source is restored.

```bash
CARGO_TARGET_DIR=/tmp/wamn-target-ayq7-24 \
  cargo test --locked -p wamn-runner -p wamn-standard-nodes -p wamn-catalog
CARGO_TARGET_DIR=/tmp/wamn-target-ayq7-24 \
  cargo test --locked -p wamn-ctl --lib publish_catalog
CARGO_TARGET_DIR=/tmp/wamn-target-ayq7-24 \
  cargo test --locked --manifest-path components/Cargo.toml -p flowrunner
```

The `event-node-abi` mutation runner was retired by `wamn-0h0g.15.122`:
`validate_event_outcome` relocated to `crates/execution/flow-engine/src/engine.rs:825` (commit `e05636b8`) and its gate test no longer exists anywhere, so
both halves of that guard were dead. There is no current runnable

## PLAN-2A — fail standard-node dispatch (`wamn-ayq7.25`)

`fail` resolves to the pinned, capability-free platform standard-node
executable and returns the exact authored terminal code/message through the
standard-node dispatch. As an ordinary standard-node execution it consumes one dispatch-
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
```

The `fail-node-abi` mutation runner was retired by `wamn-0h0g.15.122`:
`validate_fail_outcome` relocated to `crates/execution/flow-engine/src/engine.rs:850` (commit `e05636b8`) and its gate test no longer exists anywhere, so
both halves of that guard were dead. There is no current runnable

## PLAN-1 — uniform node-interface pinning (`wamn-4u7p.38`)

Publication resolves platform and supplied nodes into the same canonical
resolved-contract bundle. Built-in-only artifacts persist a non-empty bundle,
and runtime admission verifies those exact contracts before single-shot
execution. The model-owned
`call-flow { flow-id }` validator is the sole current exemption from
interface-backed standard-node resolution. Historical versions retain their
explicit compatibility readers; unknown versions fail closed. The mutation
reintroduces the former `request` exemption and must make the focused
publication-to-runtime round-trip gate fail before restoring the catalog source
byte-exact.

```bash
CARGO_TARGET_DIR=/tmp/wamn-target-4u7p-38 \
  cargo test --locked -p wamn-catalog -p wamn-standard-nodes -p wamn-ctl -p wamn-runner
CARGO_TARGET_DIR=/tmp/wamn-target-4u7p-38 \
  cargo test --locked --manifest-path components/Cargo.toml -p flowrunner
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

## SR-MVP — settled admission authority (`wamn-0h0g.4.8`)

This debug-only gate proves that HTTP, event, release-scenario, and
draft-scenario producers compose the one public run-state admission recipe in
their own transactions. It also pins the immutable catalog and candidate
identity, report consistency carrier, and absence of an invocation-JSON
execution-bundle duplicate. The live legs use one disposable PostgreSQL 18
database and never target the development cluster.

```bash
CARGO_TARGET_DIR=/tmp/wamn-target-wave3 CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-run-state -p wamn-runtime \
  -p wamn-scenario-worker -p wamn-proof-integration
CARGO_TARGET_DIR=/tmp/wamn-target-wave3 CARGO_INCREMENTAL=0 \
  cargo clippy --locked --offline -p wamn-run-state -p wamn-runtime \
  -p wamn-scenario-worker --all-targets -- -D warnings
CARGO_TARGET_DIR=/tmp/wamn-target-wave3 CARGO_INCREMENTAL=0 \
  cargo clippy --locked --offline -p wamn-proof-integration \
  --all-targets -- -D warnings

CARGO_TARGET_DIR=/tmp/wamn-target-wave3-components CARGO_INCREMENTAL=0 \
  cargo build --locked --offline --manifest-path components/Cargo.toml \
  -p flowrunner -p materializer --target wasm32-wasip2

docker run --rm -d --name wamn-0h0g-4-8-pg \
  -p 127.0.0.1:15648:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
until docker exec wamn-0h0g-4-8-pg pg_isready -U postgres -d wamn; do sleep 1; done

WAMN_RUN_STORE_PG_URL=postgresql://postgres:postgres@127.0.0.1:15648/wamn \
CARGO_TARGET_DIR=/tmp/wamn-target-wave3 CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-run-state --test admission_live \
  admission_live -- --ignored --exact --nocapture

WAMN_AUTHORING_LOOP_ADMIN_PG_URL=postgresql://postgres:postgres@127.0.0.1:15648/wamn \
WAMN_AUTHORING_LOOP_AUTHOR_PG_URL=postgresql://wamn_authoring_loop_author:wamn-author-live@127.0.0.1:15648/wamn \
WAMN_AUTHORING_LOOP_FLOWRUNNER=/tmp/wamn-target-wave3-components/wasm32-wasip2/debug/flowrunner.wasm \
CARGO_TARGET_DIR=/tmp/wamn-target-wave3 CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-scenario-worker \
  --test authoring_loop_live authoring_loop_live -- --ignored --exact --nocapture

docker stop wamn-0h0g-4-8-pg

rustfmt --edition 2024 --check \
  components/execution/materializer/src/main.rs \
  crates/execution/run-state/src/{admission.rs,lib.rs} \
  crates/execution/run-state/src/queue/{mod.rs,sql.rs} \
  crates/execution/run-state/tests/{admission_live.rs,draft_admission.rs,queue.rs} \
  crates/platform/runtime/src/flow_invocation.rs \
  services/scenario-worker/src/lib.rs \
  tests/integration/src/{causation_e2e.rs,materializer.rs}
git diff --check
```

## SR-MVP — run-state and schema-control drift inventory (`wamn-0h0g.4.12`)

This debug-only gate proves the schema-control repair/drift inventory and the
PostgreSQL 18 run-plane reconcile. Its release-bound resolution substrate —
`run_flow_resolutions`, `resolution.rs`, and the typed resolution refusals —
was deleted by `wamn-0h0g.15.10`; the claim is now lock → classify → lease and
resolution is a pure read of the claiming pod's mounted release manifest. It
does not claim runs, mutate queues, terminalize runs, compose production
transactions, or dispatch effects. Use the isolated lane target below;
cross-worktree target sharing can execute artifacts compiled from a different
checkout.

```bash
CARGO_TARGET_DIR=/tmp/wamn-target-wave3-4-12 CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-run-state -p wamn-schema-control
CARGO_TARGET_DIR=/tmp/wamn-target-wave3-4-12 CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-proof-conformance --lib schema_drift::
CARGO_TARGET_DIR=/tmp/wamn-target-wave3-4-12 CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-proof-conformance \
  --test effect_provider_revision
CARGO_TARGET_DIR=/tmp/wamn-target-wave3-4-12 CARGO_INCREMENTAL=0 \
  cargo clippy --locked --offline -p wamn-run-state -p wamn-schema-control -p wamn-ctl \
  -p wamn-proof-conformance --all-targets -- -D warnings

docker run --rm -d --name wamn-0h0g-4-12-pg \
  -p 127.0.0.1:15652:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
until docker exec wamn-0h0g-4-12-pg pg_isready -U postgres -d wamn; do sleep 1; done

WAMN_RUN_STORE_PG_URL=postgresql://postgres:postgres@127.0.0.1:15652/wamn \
CARGO_TARGET_DIR=/tmp/wamn-target-wave3-4-12 CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-run-state --test run_state_live \
  run_state_live -- --ignored --exact --nocapture

docker stop wamn-0h0g-4-12-pg

docker run --rm -d --name wamn-0h0g-4-12-ctl-pg \
  -p 127.0.0.1:15653:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
until docker exec wamn-0h0g-4-12-ctl-pg pg_isready -U postgres -d wamn; do sleep 1; done

WAMN_CTL_PG_URL=postgresql://postgres:postgres@127.0.0.1:15653/wamn \
CARGO_TARGET_DIR=/tmp/wamn-target-wave3-4-12 CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-ctl --test run_plane_live \
  run_plane_reconcile_live -- --exact --nocapture --test-threads=1

docker stop wamn-0h0g-4-12-ctl-pg

rustfmt --edition 2024 --check \
  crates/execution/run-state/src/{lib.rs,sql.rs} \
  crates/execution/run-state/tests/run_state_live.rs \
  crates/schema/control/src/run_plane.rs \
  services/ctl/tests/run_plane_live.rs \
  tests/conformance/src/schema_drift.rs
python3 -m json.tool architecture/state-owners.json >/dev/null
git diff --check
```

## SR-MVP — framed node/effect fact identity (`wamn-0h0g.4.13`)

This debug-only gate proves the framed durable identity and trusted effect
payload shape. It owns schema/model/payload shape only: no effect intent writer,
authorization/send path, dispatch creation, or call-frame interpreter runtime is
claimed here. Keep all package, WIT, and wire versions at `0.1`/`0.1.0`; negative
tests use non-version sentinels.

Use isolated root and component targets so this lane cannot read artifacts from
another worktree:

```bash
CARGO_TARGET_DIR=/tmp/wamn-target-wave3-4-13 CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-run-state -p wamn-schema-control \
  -p wamn-ctl -p wamn-proof-integration
CARGO_TARGET_DIR=/tmp/wamn-target-wave3-4-13 CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-proof-conformance \
  --test effect_provider_revision
CARGO_TARGET_DIR=/tmp/wamn-target-wave3-4-13 CARGO_INCREMENTAL=0 \
  cargo clippy --locked --offline -p wamn-run-state -p wamn-schema-control \
  -p wamn-ctl --all-targets -- -D warnings
CARGO_TARGET_DIR=/tmp/wamn-target-wave3-4-13 CARGO_INCREMENTAL=0 \
  cargo clippy --locked --offline -p wamn-proof-integration \
  --all-targets -- -D warnings

CARGO_TARGET_DIR=/tmp/wamn-target-wave3-4-13-components CARGO_INCREMENTAL=0 \
  cargo test --locked --offline --manifest-path components/Cargo.toml -p flowrunner
CARGO_TARGET_DIR=/tmp/wamn-target-wave3-4-13-components CARGO_INCREMENTAL=0 \
  cargo check --locked --offline --manifest-path components/Cargo.toml \
  -p flowrunner --target wasm32-wasip2
# Flowrunner remains deliberately hard-refused until wamn-0h0g.3.4/.5.4 land;
# deny every warning except the resulting dead code on both native and wasm targets.
CARGO_TARGET_DIR=/tmp/wamn-target-wave3-4-13-components CARGO_INCREMENTAL=0 \
  cargo clippy --locked --offline --manifest-path components/Cargo.toml \
  -p flowrunner --all-targets -- -D warnings -A dead-code
CARGO_TARGET_DIR=/tmp/wamn-target-wave3-4-13-components CARGO_INCREMENTAL=0 \
  cargo clippy --locked --offline --manifest-path components/Cargo.toml \
  -p flowrunner --target wasm32-wasip2 -- -D warnings -A dead-code

docker rm -f wamn-0h0g-4-13-run-state-pg 2>/dev/null || true
docker run --rm -d --name wamn-0h0g-4-13-run-state-pg \
  -p 127.0.0.1:15654:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
until docker exec wamn-0h0g-4-13-run-state-pg pg_isready -U postgres -d wamn; do sleep 1; done

WAMN_RUN_STORE_PG_URL=postgresql://postgres:postgres@127.0.0.1:15654/wamn \
CARGO_TARGET_DIR=/tmp/wamn-target-wave3-4-13 CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-run-state --test run_state_live \
  run_state_live -- --ignored --exact --nocapture

docker rm -f wamn-0h0g-4-13-run-state-pg

docker rm -f wamn-0h0g-4-13-run-plane-pg 2>/dev/null || true
docker run --rm -d --name wamn-0h0g-4-13-run-plane-pg \
  -p 127.0.0.1:15655:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
until docker exec wamn-0h0g-4-13-run-plane-pg pg_isready -U postgres -d wamn; do sleep 1; done

WAMN_CTL_PG_URL=postgresql://postgres:postgres@127.0.0.1:15655/wamn \
CARGO_TARGET_DIR=/tmp/wamn-target-wave3-4-13 CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-ctl --test run_plane_live \
  run_plane_reconcile_live -- --exact --nocapture --test-threads=1

docker rm -f wamn-0h0g-4-13-run-plane-pg

rustfmt --edition 2024 --check --config skip_children=true \
  components/execution/flowrunner/src/lib.rs \
  crates/execution/run-state/src/invocation_context.rs \
  crates/execution/run-state/src/lib.rs \
  crates/execution/run-state/src/queue/sql.rs \
  crates/execution/run-state/src/sql.rs \
  crates/execution/run-state/src/transitions.rs \
  crates/execution/run-state/tests/queue.rs \
  crates/execution/run-state/tests/run_state_live.rs \
  crates/execution/run-state/tests/store.rs \
  crates/schema/control/src/run_plane.rs \
  services/ctl/src/reconcile_run_plane.rs \
  services/ctl/tests/run_plane_live.rs \
  test-support/fixtures/runner.rs \
  tests/integration/src/capturebench.rs \
  tests/integration/src/runnerbench.rs
git diff --check
```

## SR-MVP — ctl MVP/ops verb split (`wamn-0h0g.9.4`)

This debug-only gate proves the default control-plane binary exposes only MVP
verbs, the optional operations binary exposes only its five operations verbs,
and `pin-run` is absent from both. The feature-tree assertion ensures the
ordinary package enables neither operations feature in its dependent crates.

```bash
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-9-4 CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-ctl --all-targets
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-9-4 CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-ctl --features ops --all-targets
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-9-4 CARGO_INCREMENTAL=0 \
  cargo clippy --locked --offline -p wamn-ctl --all-targets -- -D warnings
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-9-4 CARGO_INCREMENTAL=0 \
  cargo clippy --locked --offline -p wamn-ctl --features ops --all-targets -- -D warnings

CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-9-4 \
  cargo run --locked --offline -p wamn-ctl --bin wamn-ctl -- --help
cargo tree --locked --offline -p wamn-ctl --edges features
# The help output omits dump/restore/copy-project-env, prune-run-history,
# impact-report, and pin-run. The tree omits wamn-control-provision/ops and
# wamn-schema-control/ops.

cargo fmt -p wamn-ctl -- --check
git diff --check
```

## SR-MVP — additive-only catalog migration (`wamn-0h0g.9.5`)

The default control binary accepts only safely additive catalog plans. Its
`migrate-catalog` help has no destructive override; both dry-run and apply
refuse a destructive target and point to environment reprovisioning. Dry-run
renders only the forward additive plan. Impact analysis remains available only
through `wamn-ctl-ops impact-report`.

```bash
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-9-5 CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-ctl --all-targets
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-9-5 CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-ctl --features ops --all-targets
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-9-5 CARGO_INCREMENTAL=0 \
  cargo clippy --locked --offline -p wamn-ctl -p wamn-test-infrastructure \
    -p wamn-proof-integration --all-targets -- -D warnings
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-9-5 CARGO_INCREMENTAL=0 \
  cargo clippy --locked --offline -p wamn-cdc-reader \
    --test event_reader_live -- -D warnings
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-9-5 CARGO_INCREMENTAL=0 \
  cargo clippy --locked --offline -p wamn-ctl --features ops --all-targets -- -D warnings

# Optional live proof on a disposable PostgreSQL 18 database. It covers
# additive dry-run/apply, destructive dry-run/apply refusal with zero mutation,
# and the registration-orphan refusal ordering.
WAMN_CTL_PG_URL="$THROWAWAY_PG_URL" \
  CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-9-5 CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-ctl --test orphan_guard_live \
    -- --nocapture --test-threads=1

# Eight byte-pinned mutants: destructive public planning, destructive dry-run,
# default-compiler destructive emission, restored override flag, ops enabled by
# default, skipped confirmation read, dropped one-shot authorization consumption,
# and bypassed locked-window guard.
# Only the dry-run mutant needs the disposable PostgreSQL URL used by the live proof.

cargo fmt -p wamn-ctl -p wamn-cdc-reader -p wamn-test-infrastructure \
  -p wamn-proof-integration -- --check
git diff --check
```

## SR-MVP — ops persistence and internal reconciliation (`wamn-0h0g.12.4`, `.12.5`)

The core T1 schema installs independently and contains no dump, copy-saga, or
migration-attestation relation. Operations commands install the idempotent
`deploy/sql/ops-schema.sql` extension after core, then execute its state writes
as the stable `wamn_ops` role. The extension owns exactly
`provisioning.dumps`, `provisioning.copy_sagas`, and
`provisioning.migration_confirmations`; only its project-environment identities
reference core. Confirmation actor and time are minted by PostgreSQL, and the
confirmation relation grants `wamn_ops` table `SELECT` plus column-scoped
`INSERT` only for the seven identity/window facts.

The default migration planner remains additive-only. Destructive compilation is
reachable only through the `ops` feature for impact planning and copy/restore
target reconciliation. Copy rechecks the locked project-database applied
version against the stored `(from_version, to_version)` window before applying;
the T1 row is authorization evidence, not the current project state. There is no
public destructive-migrate command, and the five-command ops list is unchanged.

```bash
# Default and ops feature surfaces. These commands also execute the assertions
# that default help contains no ops verbs and the default feature tree enables
# neither wamn-control-provision/ops nor wamn-schema-control/ops.
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-12-ops CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-ctl --all-targets
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-12-ops CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-ctl --features ops --all-targets
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-12-ops \
  cargo run --locked --offline -p wamn-ctl --bin wamn-ctl -- --help
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-12-ops \
  cargo run --locked --offline -p wamn-ctl --features ops \
    --bin wamn-ctl-ops -- --help
cargo tree --locked --offline -p wamn-ctl --edges features

# Pure/static carrier checks: core has no ops relation or literal, the packaged
# extension equals the deployed artifact, destructive public planning refuses,
# ops planning remains available, and the new SQL header stays at identity 0.1.
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-12-ops CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-control-provision --features ops \
    --test ops_storage
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-12-ops CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-schema-control --features ops \
    --test migrate
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-12-ops CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-proof-conformance \
    governed_wire_schema_and_artifact_versions_stay_at_mvp_identity -- --exact

# Fresh PostgreSQL 18 proof: apply core once, ops twice, exercise all three
# relations through their builders, verify the one-way FK set and exact ACLs,
# then prove default/ops destructive planning over real DDL.
docker run -d --rm --name wamn-0h0g-12-ops-pg -p 15658:5432 \
  -e POSTGRES_PASSWORD=postgres postgres:18
until docker exec wamn-0h0g-12-ops-pg pg_isready -U postgres; do sleep 1; done
WAMN_REGISTRY_PG_URL=postgresql://postgres:postgres@127.0.0.1:15658/postgres \
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-12-ops CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-control-provision --features ops \
    --test ops_storage ops_schema_applies_idempotently_after_core_on_postgres \
    -- --exact --nocapture
WAMN_MIGRATE_PG_URL=postgresql://postgres:postgres@127.0.0.1:15658/postgres \
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-12-ops CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-schema-control --features ops \
    --test migrate migration_engine_applies_forward_and_limits_destructive_to_ops_on_postgres \
    -- --exact --nocapture

# Regenerate and verify the 68-row core+ops authority table from pg_catalog.
WAMN_UPDATE_PROTECTED_RELATIONS=1 \
WAMN_CTL_PG_URL=postgresql://postgres:postgres@127.0.0.1:15658/postgres \
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-12-ops CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-ctl --features ops \
    --test protected_relations_live -- --nocapture --test-threads=1
WAMN_CTL_PG_URL=postgresql://postgres:postgres@127.0.0.1:15658/postgres \
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-12-ops CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-ctl --features ops \
    --test protected_relations_live -- --nocapture --test-threads=1

# Mutation outcomes: eight additive/authorization-boundary mutants and ten
# authority-table mutants, including explicit ops-scope drift and expansion of
# the append-only confirmation ACL. Every target is restored byte-for-byte.
docker rm -f wamn-0h0g-12-ops-pg

CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-12-ops CARGO_INCREMENTAL=0 \
  cargo clippy --locked --offline -p wamn-control-provision --features ops \
    --all-targets -- -D warnings
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-12-ops CARGO_INCREMENTAL=0 \
  cargo clippy --locked --offline -p wamn-schema-compiler --features ops \
    --all-targets -- -D warnings
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-12-ops CARGO_INCREMENTAL=0 \
  cargo clippy --locked --offline -p wamn-schema-control --features ops \
    --all-targets -- -D warnings
CARGO_TARGET_DIR=/tmp/wamn-target-0h0g-12-ops CARGO_INCREMENTAL=0 \
  cargo clippy --locked --offline -p wamn-ctl --features ops \
    --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
```

## SR-MVP — durable test orchestration (`wamn-0h0g.8.4`)

The management-owned durable substrate reserves one report and one stable run
identity per ordinal, reconciles deadlines and effect uncertainty to immutable
failed cases, pins one report-level resolution map, and projects frame-keyed
node facts through that map. Sequential admission/execution and the public
report query remain in their owning follow-up beads.

```bash
CARGO_TARGET_DIR=/tmp/wamn-target-cleanup-next CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-scenario-worker \
    store::test_orchestration --lib
CARGO_TARGET_DIR=/tmp/wamn-target-cleanup-next CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-schema-control \
    durable_test_orchestration_is_in_the_schema_control_record --lib

# PostgreSQL 18 proof over a disposable database. The same fixture may be used
# for the pg_catalog-derived authority-table regeneration and verification.
WAMN_TEST_ORCHESTRATION_PG_URL="$THROWAWAY_PG_URL" \
CARGO_TARGET_DIR=/tmp/wamn-target-cleanup-next CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-scenario-worker \
    --test test_orchestration_live -- --nocapture --test-threads=1
WAMN_UPDATE_PROTECTED_RELATIONS=1 WAMN_CTL_PG_URL="$THROWAWAY_PG_URL" \
CARGO_TARGET_DIR=/tmp/wamn-target-cleanup-next CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-ctl --features ops \
    --test protected_relations_live -- --nocapture --test-threads=1
CARGO_TARGET_DIR=/tmp/wamn-target-cleanup-next CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-proof-conformance \
    --test state_ownership

CARGO_TARGET_DIR=/tmp/wamn-target-cleanup-next CARGO_INCREMENTAL=0 \
  cargo clippy --locked --offline -p wamn-scenario-worker \
    -p wamn-schema-control --all-targets -- -D warnings

rustfmt --edition 2024 --check \
  crates/schema/control/src/run_plane.rs \
  services/scenario-worker/src/authoring.rs \
  services/scenario-worker/src/store/{mod.rs,test_orchestration.rs} \
  services/scenario-worker/tests/test_orchestration_live.rs \
  tests/conformance/tests/state_ownership.rs
git diff --check
```

The six test-orchestration mutants omit a durable case table from
schema-control, recreate the from-zero bug where triggers located after a shared
helper never install, drop the reservation insert's idempotency, unbound the case
deadline, rejoin the case-run insert onto the run plane, and turn the report
verdict from a conjunction of its cases into a disjunction — a report with a
failed case reporting `passed = true`. Each must fail its named owner test, then
restore the exact source hash.

The campaign **cannot run today**: its `EXPECTED_SHA` baselines are stale in two
files, and the `run_plane.rs` cases come first in `mutation_ids()`, so `check`
and `run-all` abort on mutant 1 before reaching the rest. `2dc1ee0e…` was already
five file-revisions stale when it was introduced, so three of these mutants have
never executed once. Every needle still anchors exactly once and every gate test
name exists, so a pure digest re-baseline revives all six — that is
`wamn-0h0g.15.22`'s, along with the wider finding that 109 of 135 baselines
across 31 of 37 campaigns are stale (`wamn-0h0g.15.136`).

## SR-MVP — unconditional publish gate (`wamn-0h0g.8.8`)

The T1 schema and registry expose no configurable publish switch, project
override, exemption, or resolver. The authoring contract has exactly one gate:
`publish` requires the successful green finalized report identity. This gate
does not run control-store migration or any publication transaction.

```bash
export CARGO_TARGET_DIR=/home/kaalin/dev/wamn/target/plane-wave1-8-8
export CARGO_INCREMENTAL=0

cargo test --locked --offline -p wamn-control-registry
cargo test --locked --offline -p wamn-control-provision --test control_storage \
  retired_configurable_publish_policy_stays_deleted -- --exact
cargo test --locked --offline -p wamn-authoring-model --test contract \
  publish_requires_a_successful_report_unconditionally -- --exact

# From-zero schema proof on the lane's disposable PostgreSQL 18 instance.
docker run --rm -d --name wamn-0h0g-8-8-pg -p 127.0.0.1:15663:5432 \
  -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=wamn postgres:18
until docker exec wamn-0h0g-8-8-pg pg_isready -U postgres -d wamn; do sleep 1; done
WAMN_REGISTRY_PG_URL=postgres://postgres:postgres@127.0.0.1:15663/wamn \
  cargo test --locked --offline -p wamn-control-provision --test control_storage \
    system_schema_applies_and_enforces_invariants_on_postgres \
    -- --exact --nocapture --test-threads=1
docker rm -f wamn-0h0g-8-8-pg

cargo clippy --locked --offline -p wamn-authoring-model \
  -p wamn-control-registry -p wamn-control-provision --all-targets -- -D warnings

rustfmt --edition 2024 --check \
  crates/authoring/model/tests/contract.rs \
  crates/control/provision/tests/control_storage.rs
git diff --check
```

The exact mutant makes the report identity defaultable. The named contract
test must fail, and the script must restore the exact source hash.

## SR-MVP — stored-suite persistence deletion (`wamn-0h0g.8.10`)

The populated-schema cutover deletes the five legacy stored-suite/report
tables, their two helpers, and `catalog.publish_gate_audit`, while preserving
all four management-owned `authoring_test_*` relations. The generated authority
table proves the deleted objects are absent and the remaining 68 relations have
complete ownership and grant-derived exposure records.

```bash
WAMN_CTL_PG_URL="$THROWAWAY_PG_URL" \
CARGO_TARGET_DIR=/tmp/wamn-target-cleanup-next CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-ctl --features ops \
    --test run_plane_live stored_suite_cutover_live \
    -- --exact --nocapture --test-threads=1
CARGO_TARGET_DIR=/tmp/wamn-target-cleanup-next CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-proof-conformance \
    --test state_ownership --test protected_relations --test gate_registry
CARGO_TARGET_DIR=/tmp/wamn-target-cleanup-next CARGO_INCREMENTAL=0 \
  cargo clippy --locked --offline -p wamn-schema-control -p wamn-ctl \
    -p wamn-scenario-worker \
    -p wamn-proof-integration -p wamn-proof-conformance \
    --features wamn-ctl/ops --all-targets -- -D warnings

cargo fmt --all -- --check
git diff --check
```

## SR-MVP — authoring store fold (`wamn-0h0g.8.11`)

The management service now owns the draft and test-orchestration store directly;
the standalone scenario catalog and runtime packages no longer exist. The
public authoring inventory is exactly `save-flow-draft`, `validate`,
`draft-run`, and `publish`, exposed by the CLI verbs `validate`, `draft-run`,
and `promote`. The populated-schema cutover removes the retired validation
dimension only from an empty legacy table and refuses stale retired command
history rather than silently changing its identity.

```bash
# Regenerate both published client artifacts before running their drift checks.
CARGO_TARGET_DIR=/tmp/wamn-target-cleanup-next CARGO_INCREMENTAL=0 \
  cargo run --locked --offline -p wamn-authoring-model \
    --example print-authoring-surface-schema \
    > docs/archive/contracts/authoring-surface.schema.json
(cd clients/authoring-client && node scripts/generate.mjs)

# One shared-cache debug pass over every affected Rust package.
CARGO_TARGET_DIR=/tmp/wamn-target-cleanup-next CARGO_INCREMENTAL=0 \
CARGO_BUILD_JOBS=2 \
  cargo test --locked --offline \
    -p wamn-authoring-model -p wamn-catalog -p wamn-run-state \
    -p wamn-schema-control -p wamn-scenario-model \
    -p wamn-scenario-worker -p wamn-ctl -p wamn-proof-conformance
CARGO_TARGET_DIR=/tmp/wamn-target-cleanup-next CARGO_INCREMENTAL=0 \
CARGO_BUILD_JOBS=2 \
  cargo clippy --locked --offline \
    -p wamn-authoring-model -p wamn-catalog -p wamn-run-state \
    -p wamn-schema-control -p wamn-scenario-model \
    -p wamn-scenario-worker -p wamn-ctl -p wamn-proof-conformance \
    --all-targets -- -D warnings

# PostgreSQL 18 proof: empty legacy state converges, both immutable legacy
# identity/history shapes refuse with SQLSTATE 55000, and every refusal rolls
# back the batch.
WAMN_CTL_PG_URL="$THROWAWAY_PG_URL" \
CARGO_TARGET_DIR=/tmp/wamn-target-cleanup-next CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-ctl --test run_plane_live \
    stored_suite_cutover_live -- --exact --nocapture --test-threads=1

(cd clients/authoring-client && \
  node scripts/generate.mjs --check && node scripts/test.mjs && npm run build)
cargo fmt --all -- --check
git diff --check
```

## SR-MVP — callee validation and callable eligibility (`wamn-0h0g.3.1`)

This debug-only gate proves exact `call-flow { flow-id }` validation, candidate
self-resolution, pinned-release lookup for every other name, intrinsic callable
eligibility, typed contract refusals, recursion without a static depth bound,
and the effectful-node source-connection-requirement predicate. It does not exercise
the future frame interpreter or claim-time resolution map.

```bash
CARGO_TARGET_DIR=/tmp/wamn-target-wave3 CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-flow -p wamn-catalog \
  -p wamn-scenario-worker
CARGO_TARGET_DIR=/tmp/wamn-target-wave3 CARGO_INCREMENTAL=0 \
  cargo clippy --locked --offline -p wamn-flow -p wamn-catalog \
  -p wamn-scenario-worker \
  --all-targets -- -D warnings
rustfmt --edition 2024 --check \
  crates/catalog/model/src/{execution_plan.rs,lib.rs} \
  crates/execution/flow-model/src/{lib.rs,types.rs,validate.rs} \
  services/scenario-worker/src/store/drafts.rs \
  services/scenario-worker/src/authoring.rs
git diff --check
```

## SR-MVP — ordinary HTTP admission (`wamn-0h0g.5.1`)

`flow-http` final admission commits one `dispatched` run and one immediately
available queue row with owner/expiry `NULL` and generation zero before `begin`
returns. Duplicate admission retains the winning run identity, and `begin`
never enters guest execution.

```bash
CARGO_TARGET_DIR=/tmp/wamn-target-cleanup-next CARGO_INCREMENTAL=0 \
  cargo test --locked --offline \
    -p wamn-run-state -p wamn-runtime -p wamn-proof-integration
CARGO_TARGET_DIR=/tmp/wamn-target-cleanup-next CARGO_INCREMENTAL=0 \
  cargo test --locked --offline --manifest-path components/Cargo.toml -p materializer

WAMN_RUN_STORE_PG_URL="$THROWAWAY_PG_URL" \
CARGO_TARGET_DIR=/tmp/wamn-target-cleanup-next CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-run-state --test admission_live \
    admission_live -- --ignored --exact --nocapture --test-threads=1


CARGO_TARGET_DIR=/tmp/wamn-target-cleanup-next CARGO_INCREMENTAL=0 \
  cargo clippy --locked --offline \
    -p wamn-run-state -p wamn-runtime -p wamn-proof-integration \
    --all-targets -- -D warnings
CARGO_TARGET_DIR=/tmp/wamn-target-cleanup-next CARGO_INCREMENTAL=0 \
  cargo clippy --locked --offline --manifest-path components/Cargo.toml \
    -p materializer --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
```

## SR-MVP — host-inline execution deletion (`wamn-0h0g.5.2`)

The HTTP host is admission-only: it carries no flowrunner bytes, executor
configuration, exact-claim API, or inline-driver seam. The production
`run-worker` remains the sole execution owner and both its image and the gates
image retain the flowrunner component. The obsolete invocation and credential
proof programs and Jobs are physically absent; their registry records are
retired while the ordinary-admission and credential-unit proofs remain live.

```bash
CARGO_TARGET_DIR=/tmp/wamn-target-cleanup-next CARGO_INCREMENTAL=0 \
  cargo test --locked --offline \
    -p wamn-run-state -p wamn-execution-host -p wamn-runtime -p wamn-host \
    -p wamn-proof-integration -p wamn-proof-system -p wamn-proof-conformance \
    -p wamn-gates
CARGO_TARGET_DIR=/tmp/wamn-target-cleanup-next CARGO_INCREMENTAL=0 \
  cargo clippy --locked --offline \
    -p wamn-run-state -p wamn-execution-host -p wamn-runtime -p wamn-host \
    -p wamn-proof-integration -p wamn-proof-system -p wamn-proof-conformance \
    -p wamn-gates --all-targets -- -D warnings

if cargo tree --locked --offline -p wamn-host --depth 1 --prefix none | \
  rg -q '^wamn-execution-host '; then
  echo 'wamn-host still depends on wamn-execution-host' >&2
  exit 1
fi

jq empty architecture/gate-registry.json
cargo fmt --all -- --check
git diff --check
```

## SR-MVP — durable child-run deletion (`wamn-0h0g.4.4`)

This debug-only gate proves that `call-flow` is the sole retained inter-flow
declaration, fresh schemas contain no durable child/wait/depth state, and the
leading `ChildRunCutover` either removes empty legacy state atomically or
refuses populated state with SQLSTATE `55000` before any DDL. The live proof
also pins ordinary-run, release-resolution, root-lineage, and frame-fact
retention. Use a disposable PostgreSQL 18 database for the run-plane tests.
Start the protected-relation block on a separate fresh PostgreSQL 18 cluster:
its role evidence is cluster-global and must not inherit objects from another
live gate.

```bash
export CARGO_TARGET_DIR=/tmp/wamn-target-cleanup-next
export CARGO_INCREMENTAL=0

cargo test --locked --offline \
  -p wamn-flow -p wamn-run-state -p wamn-schema-control
cargo test --locked --offline -p wamn-ctl \
  reconcile_run_plane::tests::leading_cutover_allowlist_is_exact -- --exact
cargo test --locked --offline -p wamn-proof-conformance
cargo test --locked --offline -p wamn-proof-system --lib --no-run
cargo test --locked --offline -p wamn-runtime \
  --test production_claim_live --no-run

WAMN_CTL_PG_URL="$THROWAWAY_PG_URL" \
  cargo test --locked --offline -p wamn-ctl --test run_plane_live \
    child_run_cutover_live -- --exact --nocapture --test-threads=1
WAMN_CTL_PG_URL="$THROWAWAY_PG_URL" \
  cargo test --locked --offline -p wamn-ctl --test run_plane_live \
    run_plane_reconcile_live -- --exact --nocapture --test-threads=1

# Intentional inventory regeneration after a protected-schema change.
WAMN_CTL_PG_URL="$PROTECTED_RELATIONS_PG_URL" \
WAMN_UPDATE_PROTECTED_RELATIONS=1 \
  cargo test --locked --offline -p wamn-ctl --features ops \
    --test protected_relations_live -- --nocapture --test-threads=1

cargo clippy --locked --offline \
  -p wamn-flow -p wamn-run-state -p wamn-schema-control -p wamn-ctl \
  -p wamn-proof-conformance --all-targets -- -D warnings
cargo clippy --locked --offline -p wamn-proof-system --lib -- -D warnings
cargo clippy --locked --offline -p wamn-runtime \
  --test production_claim_live -- -D warnings
cargo fmt --all -- --check
git diff --check
```

## SR-MVP — package-scoped Docker cook/build graph (`wamn-0h0g.10.4`)

This gate is structural and debug-only. It pins one shared native planner
recipe, seven retained package-scoped `cook-*`/`build-*` pairs, locked shared
cache mounts, and exact binary provenance. Buildx runs in `--check` mode only;
clean/warm image builds and timing belong exclusively to `wamn-0h0g.10.6`.

```bash
export CARGO_TARGET_DIR=/tmp/wamn-target-cleanup-next
export CARGO_INCREMENTAL=0

cargo test --locked --offline -p wamn-proof-conformance --lib \
  docker_component_provenance::
cargo test --locked --offline -p wamn-proof-integration --lib \
  metricbench::tests::executor_command_preserves_the_production_metric_boundary \
  -- --exact


# Parse/lint representative leaves of the same complete stage graph. These
# commands do not build an image.
docker buildx build --check --target gates .
docker buildx build --check --target scenario-worker .
docker buildx build --check --target waker .

cargo clippy --locked --offline \
  -p wamn-proof-conformance -p wamn-proof-integration \
  --all-targets -- -D warnings
rustfmt --edition 2024 --check \
  tests/conformance/src/docker_component_provenance.rs \
  tests/integration/src/metricbench.rs
git diff --check
```

## SR-MVP — retained authoring-test DDL rename (`wamn-0h0g.8.23`)

This mechanical gate pins the retained authoring-test DDL bytes while moving
their path and direct Rust constants away from the deleted stored-suite name.
The SQL payload SHA-256 remains
`ba26e29941d5f45ef8d29117abd6a623e9cd7fa04fc7bb858f2672ebe360362c`.

```bash
export CARGO_TARGET_DIR=/tmp/wamn-target-cleanup-next
export CARGO_INCREMENTAL=0

echo 'ba26e29941d5f45ef8d29117abd6a623e9cd7fa04fc7bb858f2672ebe360362c  deploy/sql/authoring-tests.sql' \
  | sha256sum --check
jq empty architecture/state-owners.json

cargo test --locked --offline -p wamn-proof-conformance --lib \
  version_identity::governed_wire_schema_and_artifact_versions_stay_at_mvp_identity \
  -- --exact
cargo test --locked --offline -p wamn-proof-conformance \
  --test state_ownership repository_state_ownership_manifest_is_complete -- --exact
cargo test --locked --offline -p wamn-proof-conformance \
  --test state_ownership row_lock_clause_is_not_an_update -- --exact
cargo test --locked --offline -p wamn-scenario-worker \
  store::test_orchestration::tests::
cargo test --locked --offline -p wamn-ctl \
  publish_catalog::tests::authoring_test_orchestration_provisioning_is_fresh_and_privilege_closed \
  -- --exact

cargo clippy --locked --offline \
  -p wamn-scenario-worker -p wamn-ctl -p wamn-proof-conformance \
  --all-targets -- -D warnings
rustfmt --edition 2024 --check \
  crates/schema/control/src/run_plane.rs \
  services/ctl/src/publish_catalog.rs \
  services/ctl/tests/run_plane_live.rs \
  services/scenario-worker/src/store/test_orchestration.rs \
  tests/conformance/src/version_identity.rs \
  tests/conformance/tests/state_ownership.rs
if rg -n --hidden --glob '!.git/**' --glob '!.beads/**' \
  'flow-tests''\.sql|FLOW_''TESTS_SQL' .; then
  exit 1
fi
git diff --check
```

## SR-MVP — executable workspace profiles (`wamn-0h0g.10.3`)

This debug-only gate proves that the profile tools derive cumulative package
selections from the governed inventory and locked Cargo metadata. Fake-Cargo
tests pin every command argument and refusal; the component legs perform the
real three- and six-component `wasm32-wasip2` builds.

```bash
export CARGO_TARGET_DIR=/home/kaalin/dev/wamn/target/gate-0h0g-10-3
export CARGO_INCREMENTAL=0

jq -e . architecture/workspace-tiers.json >/dev/null

cargo test --locked --offline -p wamn-proof-conformance \
  --test profile_selectors --test workspace_tiers --no-fail-fast
cargo clippy --locked --offline -p wamn-proof-conformance \
  --test profile_selectors --test workspace_tiers -- -D warnings


tools/build-components m1
tools/build-components proof

cargo fmt --all -- --check
git diff --check
```

## SR-MVP — operator terminalization (`wamn-0h0g.4.6`)

This debug-only gate proves the sole project-admin terminalization transaction,
its immutable operator action, concurrent retry/conflict classification, and
the empty-only legacy disposition cutover. Use only the disposable PostgreSQL
18 database below; a populated retired disposition table must refuse atomically
with SQLSTATE `55000`.

```bash
export CARGO_TARGET_DIR=/home/kaalin/dev/wamn/target/gate-0h0g-4-6
export CARGO_INCREMENTAL=0
export CARGO_BUILD_JOBS=2

docker run --rm -d --name wamn-gate-0h0g-4-6-pg18 \
  -e POSTGRES_PASSWORD=wamn-gate-0h0g-4-6 \
  -p 127.0.0.1:15673:5432 postgres:18-alpine
trap 'docker rm -f -v wamn-gate-0h0g-4-6-pg18 >/dev/null 2>&1 || true' EXIT
sleep 10
until docker exec wamn-gate-0h0g-4-6-pg18 \
  pg_isready -U postgres -d postgres >/dev/null; do sleep 1; done
docker exec wamn-gate-0h0g-4-6-pg18 \
  createdb -U postgres wamn_gate_0h0g_4_6

export WAMN_CTL_PG_URL=postgresql://postgres:wamn-gate-0h0g-4-6@127.0.0.1:15673/wamn_gate_0h0g_4_6
export WAMN_OPERATOR_TERMINALIZE_PG18_URL="$WAMN_CTL_PG_URL"

cargo test --locked --offline -p wamn-schema-control --lib
cargo test --locked --offline -p wamn-run-state -p wamn-ctl --lib --tests
cargo test --locked --offline -p wamn-ctl \
  --test terminalize_effect_uncertain_live \
  terminalize_effect_uncertain_is_atomic_exact_and_authority_closed_live \
  -- --exact --nocapture --test-threads=1
cargo test --locked --offline -p wamn-ctl --test run_plane_live \
  retired_effect_disposition_cutover_live \
  -- --exact --nocapture --test-threads=1

WAMN_UPDATE_PROTECTED_RELATIONS=1 cargo test --locked --offline \
  -p wamn-ctl --features ops --test protected_relations_live \
  -- --nocapture --test-threads=1
cargo test --locked --offline -p wamn-ctl --features ops \
  --test protected_relations_live -- --nocapture --test-threads=1
cargo test --locked --offline -p wamn-proof-conformance \
  --test state_ownership --test protected_relations


cargo clippy --locked --offline \
  -p wamn-run-state -p wamn-schema-control -p wamn-ctl \
  -p wamn-proof-conformance --all-targets --features wamn-ctl/ops \
  -- -D warnings
cargo fmt --all -- --check
jq -e . architecture/protected-writes.json architecture/state-owners.json \
  >/dev/null
git diff --check

docker rm -f -v wamn-gate-0h0g-4-6-pg18
trap - EXIT
```

## SR-MVP — eight-operation authoring contract (`wamn-0h0g.7.3`)

This debug-only gate pins the five command and three query wire operations,
principal-scoped exact command replay, trace-only unmounted queries, read-only
draft-safe enforcement, and the empty-only upgrade of the former command audit
into the sole retry ledger. A populated legacy audit refuses atomically with
SQLSTATE `55000`; it is never promoted into retry history.

The Rust-only MVP gate stops at the Rust-owned wire/schema boundary. Generated
TypeScript and reference-client drift are deferred to `wamn-0h0g.7.6`.

```bash
export CARGO_TARGET_DIR=/home/kaalin/dev/wamn/target/plane-wave10-7-3
export CARGO_INCREMENTAL=0
export CARGO_BUILD_JOBS=2

docker run --rm -d --name wamn-wave10-7-3-pg18 \
  -e POSTGRES_PASSWORD=wamn-wave10-7-3 \
  -e POSTGRES_DB=wamn_wave10_7_3 \
  -p 127.0.0.1:15674:5432 \
  postgres@sha256:7157393f508fd8eb46119937fab39813783fe3e7d4c6316c45c12ce2ea25e61d
trap 'docker rm -f -v wamn-wave10-7-3-pg18 >/dev/null 2>&1 || true' EXIT
until docker exec wamn-wave10-7-3-pg18 \
  pg_isready -U postgres -d wamn_wave10_7_3 >/dev/null; do sleep 1; done

export WAMN_CTL_PG_URL=postgresql://postgres:wamn-wave10-7-3@127.0.0.1:15674/wamn_wave10_7_3
export WAMN_PLATFORM_IDENTITY_PG_URL="$WAMN_CTL_PG_URL"

cargo test --locked --offline -p wamn-authoring-model
cargo test --locked --offline -p wamn-scenario-worker --lib
cargo test --locked --offline -p wamn-schema-control
cargo test --locked --offline -p wamn-proof-conformance \
  --test effect_provider_revision

cargo test --locked --offline -p wamn-scenario-worker \
  --test management_live \
  management_surface_authenticates_and_attributes_authoring_commands \
  -- --exact --nocapture
cargo test --locked --offline -p wamn-ctl \
  --test run_plane_live stored_suite_cutover_live \
  -- --exact --nocapture

# The protected inventory must be generated from a fresh database cluster.
docker rm -f -v wamn-wave10-7-3-pg18
docker run --rm -d --name wamn-wave10-7-3-pg18 \
  -e POSTGRES_PASSWORD=wamn-wave10-7-3 \
  -e POSTGRES_DB=wamn_wave10_7_3 \
  -p 127.0.0.1:15674:5432 \
  postgres@sha256:7157393f508fd8eb46119937fab39813783fe3e7d4c6316c45c12ce2ea25e61d
until docker exec wamn-wave10-7-3-pg18 \
  pg_isready -U postgres -d wamn_wave10_7_3 >/dev/null; do sleep 1; done

WAMN_UPDATE_PROTECTED_RELATIONS=1 cargo test --locked --offline \
  -p wamn-ctl --features ops --test protected_relations_live \
  -- --nocapture --test-threads=1
cargo test --locked --offline -p wamn-ctl --features ops \
  --test protected_relations_live -- --nocapture --test-threads=1
cargo test --locked --offline -p wamn-proof-conformance \
  --test protected_relations --test state_ownership


cargo clippy --locked --offline \
  -p wamn-authoring-model -p wamn-scenario-worker \
  -p wamn-schema-control -p wamn-proof-conformance \
  --all-targets -- -D warnings
cargo clippy --locked --offline -p wamn-ctl --features ops \
  --all-targets -- -D warnings
cargo fmt --all -- --check
jq -e . architecture/protected-writes.json \
  docs/archive/contracts/authoring-surface.schema.json \
  docs/archive/contracts/authoring-surface.v0.1.examples.json >/dev/null
git diff --check

docker rm -f -v wamn-wave10-7-3-pg18
trap - EXIT
```

## SR-MVP — protected node-run projection writer (`wamn-0h0g.12.43`)

This Rust-only debug gate proves that `node_runs` mutation is confined to the
private native projection writer, that expired pre-effect reset remains
advisory-serialized and freshly ledger-classified, and that each scoped A/B
generation has exactly the effect-ledger and projection-writer memberships.
Use only the disposable PostgreSQL 18 database below.

```bash
export CARGO_TARGET_DIR=/home/kaalin/dev/wamn/target/plane-wave11-12-43

docker run -d --name wamn-wave11-12-43-pg18 \
  -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn_wave11_12_43 \
  -p 127.0.0.1:15675:5432 \
  postgres@sha256:7157393f508fd8eb46119937fab39813783fe3e7d4c6316c45c12ce2ea25e61d
trap 'docker rm -f -v wamn-wave11-12-43-pg18 >/dev/null 2>&1 || true' EXIT
until docker exec wamn-wave11-12-43-pg18 \
  pg_isready -U postgres -d wamn_wave11_12_43 >/dev/null; do sleep 1; done

export WAMN_CTL_PG_URL=postgresql://postgres:postgres@127.0.0.1:15675/wamn_wave11_12_43
export WAMN_EFFECT_WRITER_PG18_URL="$WAMN_CTL_PG_URL"
export WAMN_RUN_STORE_PG_URL="$WAMN_CTL_PG_URL"
export WAMN_PRODUCTION_CLAIM_PG_URL="$WAMN_CTL_PG_URL"

cargo test --locked --offline -p wamn-schema-control --lib
cargo test --locked --offline -p wamn-control-provision --lib
cargo test --locked --offline -p wamn-run-state --features native --lib
cargo test --locked --offline -p wamn-execution-host --lib
cargo test --locked --offline -p wamn-runtime --lib
cargo test --locked --offline -p wamn-ctl --lib provision_project_env::tests
cargo test --locked --offline -p wamn-proof-conformance \
  --test dispatcher_boundary \
  --test effect_provider_revision \
  --test protected_relations \
  --test state_ownership

cargo test --locked --offline -p wamn-ctl --test run_plane_live \
  run_plane_reconcile_live \
  -- --exact --nocapture --test-threads=1
cargo test --locked --offline -p wamn-ctl \
  --test effect_writer_generation_live \
  effect_writer_generation_lifecycle_is_exact_and_fail_closed \
  -- --ignored --exact --nocapture --test-threads=1
cargo test --locked --offline -p wamn-run-state --features native \
  --test effect_writer_live native_effect_writer_live \
  -- --ignored --exact --nocapture
cargo test --locked --offline -p wamn-runtime \
  --test production_claim_live production_claim_live \
  -- --ignored --exact --nocapture

WAMN_UPDATE_PROTECTED_RELATIONS=1 cargo test --locked --offline \
  -p wamn-ctl --features ops --test protected_relations_live \
  -- --nocapture --test-threads=1
cargo test --locked --offline \
  -p wamn-ctl --features ops --test protected_relations_live \
  -- --nocapture --test-threads=1
cargo test --locked --offline -p wamn-proof-conformance \
  --test protected_relations --test state_ownership




cargo clippy --locked --offline \
  -p wamn-run-state --features native --all-targets -- -D warnings
cargo clippy --locked --offline \
  -p wamn-execution-host -p wamn-runtime -p wamn-proof-conformance \
  --all-targets -- -D warnings
cargo clippy --locked --offline \
  -p wamn-schema-control -p wamn-control-provision -p wamn-ctl \
  --all-targets --all-features -- -D warnings

cargo fmt --all -- --check
jq -e . architecture/protected-writes.json architecture/state-owners.json \
  >/dev/null
git diff --check

docker rm -f -v wamn-wave11-12-43-pg18
trap - EXIT
```

## SR-MVP — bounded flow-invocation listener (`wamn-0h0g.12.71`)

This Rust-only debug gate proves that every runtime plugin shares one
reconnecting PostgreSQL LISTEN connection across concurrent waits, preserves
subscribe-before-poll and the mandatory final authoritative poll, and uses the
same database credential as the configured max-16 pool.

```bash
export CARGO_TARGET_DIR=/home/kaalin/dev/wamn/target/plane-wave9-11-18
export CARGO_INCREMENTAL=0

cargo test --locked --offline -p wamn-runtime --lib \
  flow_invocation::tests:: -- --nocapture
cargo test --locked --offline -p wamn-runtime
cargo clippy --locked --offline -p wamn-runtime --all-targets -- -D warnings
cargo test --locked --offline -p wamn-proof-conformance \
  --test protected_relations


cargo fmt --all -- --check
git diff --check
```

## SR-MVP — M1 causation and tenant isolation (`wamn-0h0g.11.18`, `.11.25`, `.11.10`)

This Rust-only debug gate runs the completed `.11.9` forward-causation and
durable-dedup proof exactly once as M1 Check 9, then reuses that isolated fixture
for Check 10's foreign-tenant skip and tenant-unscopable delete refusal. The live
gate uses a generated Job identity and an emptyDir-backed PostgreSQL 18 sidecar;
it never contacts shared PostgreSQL. The runner fails before Job creation unless
the named sidecar image has one identical complete target-manifest, CRI-runtime,
config, and provenance tuple on all three Ready amd64 Kind nodes. The target
manifest and CRI runtime digest are deliberately distinct identities. The runner
deletes the exact Job and verifies every recorded resource absent.

```bash
set -euo pipefail

export CARGO_TARGET_DIR=/home/kaalin/dev/wamn/target/plane-wave12-11-10
export CARGO_INCREMENTAL=0

head=594872aca4ac3d2e463a080e59747e4b708c3ede
main_image=wamn-gates:m1-594872ac-debug
sidecar_image=wamn-postgres:m1-pg18-720c455e
sidecar_manifest_id=sha256:92d4f977d48900025cdad52b2bd6d37ccec93a2b42103f1d86b34b3f6796c2ed
sidecar_image_id=sha256:e62166f95a837325423e8dff775282ed1eb91f7d7b0a4e87d00f030c7ef1ed9f
sidecar_config_id=sha256:9a1a67579dc39ae2790d10ac66102510a1644bbe96f995aa722acf9168b95574
upstream_index=sha256:ae6c78831cbc35fa3a4aaf4d763ddacf6183d6004774cc2dc28b3920410d1d1a
upstream_child=sha256:cd78ca58eb75f929698e117a589488ccb2bd45107247fe02400b50ff6c418324
overlay="$CARGO_TARGET_DIR/m1-image-594872ac"
export TMPDIR="$CARGO_TARGET_DIR/docker-kind-tmp"

[[ "$(git rev-parse HEAD)" == "$head" ]]
[[ -z "$(git status --porcelain)" ]]
install -d "$overlay" "$TMPDIR"

cargo fmt --all -- --check
git diff --check
cargo test --locked --offline -p wamn-proof-integration --lib m1::tests
cargo test --locked --offline -p wamn-proof-integration --lib \
  causation_e2e::tests
cargo test --locked --offline -p wamn-proof-conformance --lib
cargo test --locked --offline -p wamn-proof-conformance \
  --test gate_registry --test kubernetes_gate_runner
cargo clippy --locked --offline \
  -p wamn-proof-integration -p wamn-proof-conformance \
  --all-targets -- -D warnings

cargo build --locked --offline -p wamn-gates -p wamn-cdc-reader
cargo build --locked --offline --manifest-path components/Cargo.toml \
  --target wasm32-wasip2 -p materializer

sha256sum -c <<'EOF'
f455a2404cf660939cdcd8457c1c828ce6a4b1d64e05659a70f8ff311ccd96e9  /home/kaalin/dev/wamn/target/plane-wave12-11-10/debug/wamn-gates
a8d427bff6f99d888f6d6e7e2b27882fe89cc1bed458a38e95c819da03d10a2a  /home/kaalin/dev/wamn/target/plane-wave12-11-10/debug/wamn-cdc-reader
a103106e2f9d932ac3eb274bdf560fae22d0039656d7ffedd8a5642224d89e96  /home/kaalin/dev/wamn/target/plane-wave12-11-10/wasm32-wasip2/debug/materializer.wasm
EOF

docker image inspect wamn-gates:dev >/dev/null
install -d "$overlay"
install -m 0755 "$CARGO_TARGET_DIR/debug/wamn-gates" "$overlay/wamn-gates"
install -m 0755 "$CARGO_TARGET_DIR/debug/wamn-cdc-reader" \
  "$overlay/wamn-cdc-reader"
install -m 0644 \
  "$CARGO_TARGET_DIR/wasm32-wasip2/debug/materializer.wasm" \
  "$overlay/materializer.wasm"

cat >"$overlay/Dockerfile" <<'EOF'
FROM wamn-gates:dev
LABEL wamn.dev/gate="m1-checks-9-and-10" \
      wamn.dev/source-head="594872aca4ac3d2e463a080e59747e4b708c3ede" \
      wamn.dev/build-profile="debug"
COPY --chmod=0755 wamn-gates /usr/local/bin/wamn-gates
COPY --chmod=0755 wamn-cdc-reader /usr/local/bin/wamn-cdc-reader
COPY --chmod=0644 materializer.wasm /bench/materializer.wasm
EOF

docker build --platform=linux/amd64 --provenance=false \
  -f "$overlay/Dockerfile" -t "$main_image" "$overlay"
docker build --platform=linux/amd64 --provenance=false \
  -f deploy/gates/m1-postgres.Dockerfile \
  -t "$sidecar_image" deploy/gates

install -d "$CARGO_TARGET_DIR/kind-tmp"
TMPDIR="$CARGO_TARGET_DIR/kind-tmp" \
  kind load docker-image "$main_image" --name wamn
TMPDIR="$CARGO_TARGET_DIR/kind-tmp" \
  kind load docker-image "$sidecar_image" --name wamn

nodes_json=$(kubectl get nodes -o json)
jq -e '
  (.items | length == 3) and
  ([.items[].metadata.name] | unique | length == 3) and
  all(.items[];
    .status.nodeInfo.architecture == "amd64" and
    any(.status.conditions[]?;
      .type == "Ready" and .status == "True"))
' >/dev/null <<<"$nodes_json"
mapfile -t nodes < <(jq -r '.items[].metadata.name' <<<"$nodes_json" | sort)

declare -a main_ids=()
for node in "${nodes[@]}"; do
  main_ids+=("$(
    docker exec "$node" crictl inspecti -o json "$main_image" |
      jq -er '
        [.status.repoDigests[]? | split("@")[1]
         | select(test("^sha256:[0-9a-f]{64}$"))]
        | unique
        | if length == 1 then .[0]
          else error("main runtime digest missing or ambiguous")
          end
      '
  )")
done
mapfile -t unique_main_ids < <(printf '%s\n' "${main_ids[@]}" | sort -u)
[[ ${#unique_main_ids[@]} -eq 1 ]]
main_image_id=${unique_main_ids[0]}

sed \
  -e 's/wamn-gates:m1-01ca7afa-debug/wamn-gates:m1-594872ac-debug/g' \
  -e "s/sha256:POST_BUILD_MAIN_IMAGE_ID/${main_image_id}/g" \
  deploy/gates/m1-gate-job.yaml \
  >"$CARGO_TARGET_DIR/m1-gate-job.594872ac.rendered.yaml"
! rg -n 'POST_BUILD_|m1-01ca7afa' \
  "$CARGO_TARGET_DIR/m1-gate-job.594872ac.rendered.yaml"

job_spec=$(
  jq -cn \
    --arg main_image "$main_image" \
    --arg main_image_id "$main_image_id" \
    --arg sidecar_image "$sidecar_image" \
    --arg sidecar_image_id "$sidecar_image_id" \
    --arg sidecar_config_id "$sidecar_config_id" \
    --arg upstream_index "$upstream_index" \
    --arg upstream_child "$upstream_child" \
    '{name:"m1-gate-",container:"m1-gate",expectation:"positive",
      exit_code:0,image:$main_image,claimed_image_id:$main_image_id,
      claim_log_prefix:"M1_MAIN_IMAGE_ID=",sidecar:"m1-postgres",
      sidecar_image:$sidecar_image,sidecar_image_id:$sidecar_image_id,
      sidecar_config_id:$sidecar_config_id,
      sidecar_upstream_index:$upstream_index,
      sidecar_upstream_child:$upstream_child,
      log_contains:
        "M1 PASS — checks 9 and 10 passed"}'
)

tools/kubernetes-gate-run \
  --manifest "$CARGO_TARGET_DIR/m1-gate-job.594872ac.rendered.yaml" \
  --generated-name-prefix m1-gate- \
  --require-final-cleanup \
  --sidecar-preflight-record "$CARGO_TARGET_DIR/m1-sidecar-preflight.594872ac.json" \
  --verdict-record "$CARGO_TARGET_DIR/m1-gate-verdict.594872ac.json" \
  --timeout-secs 900 \
  --job "$job_spec"

jq -e '
  .verdict == "pass" and .failure_classes == [] and
  (.jobs | length == 1) and .jobs[0].verdict == "pass" and
  .jobs[0].observed.condition == "complete" and
  .jobs[0].observed.claimed_image_id == .jobs[0].claimed_image_id and
  (.jobs[0].observed.pods | length == 1) and
  .jobs[0].observed.pods[0].container_exit_code == 0 and
  .jobs[0].observed.pods[0].sidecar_exit_code == 0
' >/dev/null "$CARGO_TARGET_DIR/m1-gate-verdict.594872ac.json"

jq -e \
  --arg manifest "$sidecar_manifest_id" \
  --arg runtime "$sidecar_image_id" '
  .manifest_digest == $manifest and
  .runtime_image_id == $runtime and
  .manifest_digest != .runtime_image_id and
  (.nodes | length == 3) and
  all(.nodes[];
    .complete == true and
    .manifest_digest == $manifest and
    .runtime_image_id == $runtime)
' "$CARGO_TARGET_DIR/m1-sidecar-preflight.594872ac.json" >/dev/null

sha256sum -c <<'EOF'
0d03b1a7c63fba12b4cfbf39b4d17e0419441bb9908fea330b12672883f980be  /home/kaalin/dev/wamn/target/plane-wave12-11-10/m1-gate-job.594872ac.rendered.yaml
34b46ba4e7f7a8cc08e5f3e936e3e30aaee115a3b67c5699ae49c8ddccbfd4a0  /home/kaalin/dev/wamn/target/plane-wave12-11-10/m1-sidecar-preflight.594872ac.json
EOF
```

Receipt on `594872ac`: Job `m1-gate-bs5jn` and its UID-selected Pod both
completed with exit `0` and were absent after cleanup. Check 9 reported one fire
and one durable duplicate; Check 10 reported exactly one foreign-tenant skip and
one tenant-unscopable refusal, with no admission, doorbell, poison, or retry.
The historical commit-qualified verdict SHA-256 is
`a09c4cc4d264a55319d60e415c10802c39578528e7d96f809715ca887d279076`;
the runner recorded log SHA-256
`a189168fbae5db97cad3391423b75410fd45d9c0598c282e27d48eeab79255df`.

## SR-MVP — Rust contract-diff (`wamn-0h0g.11.15`)

This repo-local gate runs the landed Rust owners for authoring,
flow-invocation, the runtime's vendored copies of `wamn:flow-invocation` and
`wamn:flow-http-routing`, flow-schema, flow-http, and the flowrunner world —
seven legs, each stopping the gate where it fails. The roadmap
revision is 0.2; governed wire/package identities remain 0.1/0.1.0, and the
owner tests refuse literal schema version 0.2. No generated TypeScript or
reference-client gate participates in the Rust-only MVP check.

```bash
export CARGO_TARGET_DIR=/home/kaalin/dev/wamn/target/plane-wave12-11-15
export CARGO_INCREMENTAL=0

cargo test --locked --offline -p wamn-proof-conformance \
  --test contract_diff
tools/contract-diff dry-run
tools/contract-diff run
cargo clippy --locked --offline -p wamn-flow --all-targets -- -D warnings
cargo clippy --locked --offline -p wamn-proof-conformance \
  --test contract_diff -- -D warnings
cargo fmt --all -- --check
bash -n tools/contract-diff
git diff --check
```

## SR-MVP — repo-local lint (`wamn-0h0g.11.16`)

The blocking lint check derives both package selections from Cargo: all 38 root
workspace members and all six component workspace members. Root targets run
Clippy with warnings denied; component targets do the same for
native and `wasm32-wasip2`. Rustfmt checks both complete workspaces. Cargo owns
the package inventories. Two target-specific exceptions name packages:
`flowrunner` confines its established fail-closed-transition `dead-code`
allowance to that package on both targets; `connection-http-standard` is named
only on native because its no-std custom panic handler cannot link with Cargo's
std test harness. The fixture receives a strict default-target native leg
without `--all-targets` or a warning allowance, using its required
`RUSTFLAGS=-C panic=abort`, and remains covered by the shared strict wasm
workspace leg.

```bash
# Inspect the exact, CWD-independent argv without executing Cargo.
tools/repo-lint dry-run

# Focused fake-Cargo proof for workspace coverage, argv safety, and refusal.
CARGO_TARGET_DIR=/tmp/wamn-target-cleanup-next CARGO_INCREMENTAL=0 \
  cargo test --locked --offline -p wamn-proof-conformance \
    --test repo_lint

# Blocking repo-local check 16.
CARGO_TARGET_DIR=/tmp/wamn-target-cleanup-next CARGO_INCREMENTAL=0 \
  tools/repo-lint
```

## SR-MVP — plan bytes by digest-verified OCI pull (`wamn-0h0g.15.12`)

Plan supply resolves a run against the mounted serving manifest and pulls the
reachable plan set by digest. Two verifications stand: `oci-client` refuses a
blob that does not hash to the layer descriptor it was fetched under, and
`insert_verified` re-hashes the bytes against the digest the release named before
they reach the cache or the guest. The second is the load-bearing one and is what
the `plan-supply` mutant campaign anchors on.

The unit legs need nothing external: the supply logic and the three pre-effect
dispositions (`unavailable` → release and requeue, `incomplete` → deployment
invalid, `hash-mismatch` → integrity) run against a stub source, and the weld is
loaded from a scratch mount because it has exactly one constructor and that
constructor reads a file.

The transport leg needs a registry. Use a **disposable** one — never the
in-cluster `registry:5000`, which is frozen. There is no publisher yet
(`wamn-0h0g.15.97`), so the test pushes its own artifacts and therefore proves
self-consistency, not agreement with a producer.

```bash
# Supply logic, dispositions, and the pinned wire literals.
CARGO_TARGET_DIR=/tmp/wamn-target-15-12 CARGO_INCREMENTAL=0 \
  cargo test --locked -p wamn-runtime --lib \
    plugins::runner_plan_supply:: plan_artifact::

# Real oci-client transport against a throwaway registry.
docker run --rm -d -p 5099:5000 --name wamn-plan-registry registry:2
CARGO_TARGET_DIR=/tmp/wamn-target-15-12 CARGO_INCREMENTAL=0 \
  WAMN_PLAN_REGISTRY=localhost:5099 \
  cargo test --locked -p wamn-runtime --test oci_plan_source_live -- --ignored
docker rm -f wamn-plan-registry

# Mutation campaign (two mutants; `moving-head-query` died with its subject).
```

## SR-MVP — the governed-dependency guard admits a self dev-dependency (wamn-0h0g.15.109)

`wamn-0h0g.15.104` gave `wamn-catalog` a dev-dependency on itself by path, which is
the only construction that lets a crate's `tests/` compilation unit see its own
`test-util` feature under resolver 2, and therefore what keeps the `M-TEST-UTIL`
fence intact downstream. The identity guard read it as ungoverned drift.

The exemption is keyed on all three of: a **dev**-dependency table, a name equal to
the manifest's own package name, and `path = "."` exactly. Its negative half is the
point — five near-miss fixtures plus two real-tree mutants prove promoting the
declaration, renaming it, or repointing the path is still refused.

```bash
cargo test --locked -p wamn-proof-conformance --lib manifest_dependencies
```

The two real-tree mutants are recorded rather than scripted, because the guard
surface and its registries are frozen until `wamn-0h0g.15.22` and a new
`crates/catalog/model/Cargo.toml`, confirm the named test fails, restore, and
verify the sha256 matches:

| mutant | edit | must fail |
| --- | --- | --- |
| `self-dep-promoted` | `[dev-dependencies]` → `[dependencies]` above the self declaration | `governed_dependency_identities_are_workspace_owned` |
| `self-dep-deleted` | remove the self declaration entirely | `the_catalog_self_dev_dependency_is_still_the_construction_this_guard_exempts` |

## SR-MVP — governed version identities reconciled with the demolished surface (wamn-0h0g.15.110)

Four governed first-party identities were watching artifacts earlier waves had
deleted or moved. Three are retired with the bead and commit that deleted their
subject cited in place, so the reconcile is auditable rather than a silencing; one
had legitimately grown and its count was corrected.

Raising the authoring-client envelope count from 2 to 4 is a **tightening**, not a
relaxation: the count is exact equality, so all four pinned envelopes must now stay.
Prove it by unpinning one of the four and confirming the guard reports
`expected 4 occurrence(s) ... found 3`.

```bash
cargo test --locked -p wamn-proof-conformance --lib version_identity
```

## SR-MVP — one release-manifest weld per host process (wamn-0h0g.15.101)

The wash host now constructs its weld, mirroring the flowrunner host's argument-shaped
absent-mount posture: no `--release-manifest-root` means this process was never given
a release, and a root that is passed but unusable is fatal to startup. It takes ONE
knob where the executor takes four, because its readers serve `attachments` and
`registrations` from inside the verified manifest and pull nothing.

The drift guard spans both host files and pins two things per process: exactly one
production construction site, and that the weld is reached before anything can bind a
component. `#[cfg(test)]` construction is excluded, so the weld's own fixtures do not
widen the inventory.

```bash
# The posture itself, at the wash host's construction site.
cargo test --locked -p wamn-host

# One site per process, and construction ahead of the first bind.
cargo test --locked -p wamn-proof-conformance --lib runtime_inventory
```

Mutants are recorded rather than scripted (the guard surface is frozen until
`wamn-0h0g.15.22`). Apply, confirm the named test fails, restore, verify the sha256:

| mutant | edit | must fail |
| --- | --- | --- |
| `weld-site-removed` | drop the wash host's construction call | `one_release_manifest_weld_construction_site_per_host_process` |
| `weld-site-duplicated` | add a second production construction | same |
| `weld-after-bind` | move construction below `ClusterHostBuilder::default()` | same |
| `exec-weld-unreached` | replace `load_plan_release(release)?` with `None` | same |
| `unusable-mount-degrades` | return `Ok(None)` instead of the weld's error | `a_host_given_an_unusable_root_refuses` |

## SR-MVP — the conformance package as a gate of record (wamn-0h0g.15.116)

`wamn-proof-conformance` is the repository's static-proof floor: 12 library guard
modules under `tests/conformance/src/` plus 23 integration targets under
`tests/conformance/tests/`. No guard is `#[ignore]`d, so a whole-package run
executes every one of them. Individual guards were reachable only through the
per-bead recipes scattered above, which is how `manifest_dependencies` and
`version_identity` stayed red from `wamn-0h0g.15.10` to `wamn-0h0g.15.12`
without anyone noticing — the package did not compile for the whole of waves 5
and 6, so the workflow could not tell an unrunnable guard from a passing one.
This entry is the whole-package command those waves lacked.

```bash
# The gate of record: every lib module and every integration target.
cargo test --locked --offline -p wamn-proof-conformance --no-fail-fast

# effect_provider_revision must run single-threaded and pins the toolchain
# exactly, so give it its own invocation rather than the sweep above.
cargo --version # must report cargo 1.97.0
cargo test --locked --offline -p wamn-proof-conformance \
  --test effect_provider_revision -- --test-threads=1
```

**What the guards actually need.** No guard in this package needs a cluster, a
Docker daemon, or the network. The kubernetes- and kind-shaped guards are
hermetic: `kubernetes_gate_runner` and `kind_gate_image_remove` write their own
deterministic fake `kubectl`/`kind`/`docker` scripts into a scratch directory and
drive those, so they need `bash` and nothing more. `cranelift_dev` likewise
drives a fake helper on `PATH` and otherwise parses the `Dockerfile` as text,
and `docker_component_provenance` only `include_str!`s `.dockerignore`. What the
package does depend on is a usable toolchain. Eleven guards spawn
`cargo metadata` or `cargo tree` against the real tree — `effect_provider_revision`,
`egressbench`, `ip_name_lookup`, `manifest_dependencies`, `package_architecture`,
`profile_selectors`, `repo_lint`, `runtime_inventory`, `version_identity`,
`wasmtime_source_identity`, `workspace_tiers` — and therefore need a warm
registry for `--offline` to resolve. `version_identity` also shells out to
`git ls-files`, and `contract_diff`, `profile_selectors`, `repo_lint` and
`workspace_tiers` execute repo-local helpers under `tools/`. Only
`effect_provider_revision` is both slow and toolchain-pinned: it walks the whole
`wamn-executor` closure with `cargo tree` and asserts Cargo 1.97.0 exactly.

**Known red, deliberately.** Three integration guards fail on purpose and are
banked evidence on `wamn-0h0g.15.22`; do not relax any of them to make this
recipe green:

| guard | failure | owner |
| --- | --- | --- |
| `effect_provider_revision::locked_effect_provider_closure_matches_manifest` | the checked manifest drifted by exactly two dependency-edge groups | `wamn-0h0g.15.22` |
| `state_ownership` (three of its tests) | two on stale `architecture/state-owners.json` rows; `host_owned_production_claim_authority_is_explicit_and_bounded` on a **pinned writer-set assertion** (`execution-host` carries an extra `wamn_run.run_flow_resolutions`) | `wamn-0h0g.15.22` |
| `protected_relations::protected_relation_table_matches_declared_ownership` | `retired wamn_run.flows must be fully revoked, still granted to ["wamn_app"]` | `wamn-0h0g.15.22` |

The `state_ownership` row is **not** three instances of one cause: repairing
`validate_canonical_inventory` alone greens two of the three and leaves
`host_owned_production_claim_authority_is_explicit_and_bounded` red.

The `protected_relations` row is the amended writers-empty rule
(`wamn-0h0g.12.113`) telling the truth about an open P1. `wamn_run.flows` is
classified `retired`, and a retired relation must hold zero grants — but
`deploy/sql/flows.sql` still grants `wamn_app`, and the frozen
`architecture/protected-writes.json` still records it. Owner ruling R4 permits
exactly this window: *a revoke that lands early makes the frozen inventory
temporarily describe a wider authority than the database grants; that is
acceptable and must be noted on the row rather than hidden.* Pre-applying the
`wamn-0h0g.12.37` revoke and the regeneration to the table — dropping the
`wamn_app` role row and setting `author-reachable` to `"no"`, which
`protected_relations_live.rs` derives from a live database — turns this guard
green, which is what isolates the red to the unrevoked grant. **Do not exempt
`wamn_run.flows` from the rule to make it pass;** an exemption is the quiet
third ownership category the decay clause exists to forbid.

`effect_provider_revision` carries a `WAMN_UPDATE_EFFECT_PROVIDER_MANIFEST`
regeneration escape hatch. It belongs to `wamn-0h0g.15.22`'s registry
regeneration, not to a guard run.

**Guards reachable only through the whole-package command.** Thirteen guards have
no named recipe anywhere above; the whole-package sweep is what covers them.
Their individual selectors, for when one needs to be run alone:

```bash
# The only src/ module with no recipe reference above. Its own unit tests; the
# protocol it parses is exercised by --test kubernetes_gate_runner.
cargo test --locked --offline -p wamn-proof-conformance --lib kubernetes_gate_verdict::

# Integration targets with no named recipe above.
cargo test --locked --offline -p wamn-proof-conformance \
  --test chart_seam_governance \
  --test component_policy_socket_docs \
  --test cranelift_dev \
  --test d23_fork_governance \
  --test effect_single_dispatch \
  --test flowrunner_linker_imports \
  --test fork_build_preamble \
  --test kind_gate_image_remove \
  --test retained_root_outcomes \
  --test security_db_path_socket_policy \
  --test wasmtime_documentation \
  --test wasmtime_source_identity \
  --no-fail-fast
```

These are commands, not `# recipe-test:` directives, and deliberately so. The
`gate_registry` guard pins the directive inventory at exactly 36 and requires the
directive-ID set to equal the `Recipe`-kind entry set in
`architecture/gate-registry.json`. Adding a directive without the matching
registry entry fails `gate_registry` with `recipe registry drift`, and that
registry is frozen until `wamn-0h0g.15.22` regenerates it. Promoting these
thirteen to directives is that bead's work, not this one's.

## SR-MVP — guard-mechanism repairs and ctl kind-filter coverage (wamn-0h0g.15.114, .15.115, .15.108)

Three mechanism fixes over guards whose CONTENT was reconciled by `.15.109` and
`.15.110`. Content and mechanism are separate concerns and were deliberately kept
in separate beads.

```bash
# .15.114 + .15.115 — both guards live in the conformance lib target.
cargo test --locked -p wamn-proof-conformance --lib version_identity
cargo test --locked -p wamn-proof-conformance --lib manifest_dependencies
# .15.108 — provision_project_env is NOT ops-gated, so default features suffice.
cargo test --locked -p wamn-ctl --lib stable_acl_inventory
```

Mutants of record:

| mutant | edit | must fail |
| --- | --- | --- |
| `absence-masks-occurrence` | restore the `Err` short-circuit in `governed_literal_violations` so a missing file skips the count check | `a_missing_watched_file_still_reports_its_missing_occurrence` |
| `dotted-table-by-last-segment` | in `dependency_table_kind`, return the LAST dot segment instead of the first dependency keyword | `a_dotted_single_dependency_table_is_scanned_by_the_identity_rule` |
| `dotted-table-always-dev` | stamp `dev: true` on every dotted declaration | `the_dotted_spelling_does_not_widen_the_self_dev_dependency_exemption` |
| `writer-acl-kind-widened` | add `\| "database"` to `verify_effect_writer_acl_role_inventory`'s accepted object-kind set | `stable_acl_inventory_refuses_an_object_kind_outside_the_writer_set` |

`writer-acl-kind-widened` is the mutant that SURVIVED at `.15.107`. Note why an
`is_err()`-only test cannot kill it: under the mutant the fixture still errors, just
later, on the exact-grant-set comparison. The assertion has to pin the refusal
MESSAGE (`carries non-writer database ACL`), and that is the load-bearing line.

`.15.114`'s guard walks 50 watch entries, not 44: `GOVERNED_LITERALS` (44) plus
`GOVERNED_JSON_SCHEMAS` (6). Only the literals carry two independent faults, which
is why the JSON half deliberately still short-circuits — a missing JSON file has no
second expectation to report.
