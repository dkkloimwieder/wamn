# WAMN

WAMN provides application data, component execution, and generated operator interfaces on wasmCloud.
Applications declare their data and operations in `wamn.json`.
Rust owns platform behavior, and PostgreSQL stores its durable state.

Read the [architecture overview](docs/exe-model.md) for the current ownership and security rules.
The [build and test instructions](docs/operations/build-and-test.md) contain the complete workspace test command and live test inputs.
Beads and Git record work status.

## Repository

| Path | Owner |
| --- | --- |
| `apps/wamn_receiving/` | Receiving manifest, migrations, guest, generated code, operator UI, and tests |
| `apps/wamn_wms/` | WMS manifest, migrations, guest, generated code, example, and tests |
| `apps/client_acme_receiving/` | Acme Receiving overlay and its tests |
| `apps/platform/` | Shared platform guests and guest libraries |
| `services/` | Deployable native processes and their service tests |
| `crates/` | Platform libraries, grouped by responsibility |
| `tests/` | Conformance, integration, system, and orchestration test owners |
| `test-support/` | Shared test functions, fixtures, and infrastructure |
| `deploy/` | Infrastructure, platform manifests, test Jobs, and SQL |
| `docs/` | Current architecture, contracts, and operations |
| `evidence/` | Raw run data and historical results |

## Build

The root `rust-toolchain.toml` selects Rust, Clippy, rustfmt, and the Wasm target.
Use the pinned toolchain and lock files.
Native builds use the debug profile unless a selected test requires release artifacts.

```bash
cargo build --locked --offline -p wamn-host -p wamn-ctl -p wamn-identity \
  -p wamn-dispatcher -p wamn-executor -p wamn-scenario-worker \
  -p wamn-cdc-reader -p wamn-gates
tools/build-components app apps/wamn_receiving
```

`wamn` provides `dev`, `dev up`, and `ui scaffold`.
The separate `wamn-ctl-ops` binary requires the `ops` feature.
`tools/build-components all` builds every guest with one Cargo invocation per guest.
For parallel work, use a separate worktree and target directory as described in the runbook.

## Test and develop

Run only relevant existing tests for a behavior change or a concrete failure.
For mechanical edits, compare source, paths, and syntax.
Do not repeat builds or tests without a new behavior change or concrete failure.
Cleanup boundaries do not require broad test runs.
A root workspace test does not include the separate guest workspaces.
Live tests require explicit disposable services or their app-owned cluster setup.

```bash
cargo test --locked --offline -p wamn-receiving-tests --test generation
cargo test --locked --offline -p wamn-wms-tests --lib
tools/contract-diff run
```

The [Receiving README](apps/wamn_receiving/README.md) lists its data, SQLx, and terminal tests.
The [WMS README](apps/wamn_wms/README.md) identifies its example and application tests.
The [Acme README](apps/client_acme_receiving/README.md) lists its overlay SQLx test.
The runbook covers [the full sweep](docs/operations/build-and-test.md#the-full-sweep) and [the developer session](docs/operations/build-and-test.md#wamn-dev-environment-developer-session).

Do not use the frozen `kind-wamn` cluster as a test fixture.
Use the app tests or native RC command to create and clean up their own clusters.
Read [deployment instructions](deploy/README.md) before changing a deployed environment.
