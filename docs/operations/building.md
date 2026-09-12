# Building

Run commands from the repository root with the pinned toolchain and committed lock files.
The native toolchain is Rust 1.98.0 with Clippy, rustfmt, and `wasm32-wasip2`.
Native builds require system compiler tools and `protoc`.
The guest workspaces are `apps/Cargo.toml` and `apps/platform/no-std/Cargo.toml`.
[Components](../architecture/components.md) defines artifact and interface boundaries.

## Native programs and app components

Build native programs in debug mode by default:

```bash
cargo build --locked --offline -p wamn-host -p wamn-ctl -p wamn-identity \
  -p wamn-dispatcher -p wamn-executor -p wamn-scenario-worker \
  -p wamn-cdc-reader -p wamn-gates
cargo build --locked --offline -p wamn-ctl --features ops --bin wamn-ctl-ops
```

`wamn` owns the developer commands.
`wamn-ctl` owns provisioning and publication.
`wamn-ctl-ops` owns database maintenance and retained event advisory commands.
The local identity issuer needs `wamn-identity` beside the CLI or at the explicit `WAMN_IDENTITY_BINARY` path.

Build only the declared applications that the caller needs:

```bash
tools/build-components app apps/wamn_receiving
tools/build-components app apps/wamn_receiving apps/client_acme_receiving
tools/build-components all
```

These are alternative selections. The last command selects every guest from Cargo metadata.
Each selected guest gets one Cargo invocation, with the retained release profile and virtualization step.
Pass the resolved base app directories when building an overlay.
Do not combine guests into a new grouped Cargo invocation.
Cargo combines dependency features within each invocation, which can change artifact bytes.

The build tool requires `jq` and `sha256sum`.
`build-only app APP_DIRECTORY...` and `build-only all` emit an artifact plan to stdout.
`virtualize-only ARTIFACT_PLAN` refuses changed inputs or raw hashes before it updates outputs.
`watch-roots app APP_DIRECTORY...` lists selected source dependencies without building them.
For a manifest-only change, inspect `cargo metadata --no-deps` before deciding whether compilation is needed.

## Isolated worktrees

Keep one Cargo process per target directory.
Use a worktree and target under `$HOME/.cache/wamn-lanes` for independent work.
Reuse that worktree's own target, and let Cargo decide which artifacts need rebuilding.
Keep large build targets off temporary memory filesystems.
[Running tests](running-tests.md#capture-a-run) explains result reporting and optional output directories.

## Guest artifact comparisons

Use two clean worktrees at the same commit to test checkout independence.
Give each worktree its own target and empty `RUSTC_WRAPPER`.
Run `tools/build-components all` in each worktree.
Then pass their `target/virtualized/std-empty-environment` paths to the existing comparison:

```bash
WAMN_DIGEST_REPRO_A="$GUEST_REPRO_FIRST_ARTIFACTS" \
WAMN_DIGEST_REPRO_B="$GUEST_REPRO_SECOND_ARTIFACTS" \
  cargo test --locked --offline -p wamn-conformance-tests --test guest_workspace_closure \
  one_commit_built_in_two_checkouts_yields_identical_guest_digests \
  -- --include-ignored --exact --nocapture
```

To compare app and full selections, build the same commit into separate targets.
Set `WAMN_TREE` to the chosen worktree and `WAMN_RESULTS` to an [output directory](running-tests.md#capture-a-run):

```bash
CARGO_TARGET_DIR="$WAMN_TREE/app-target" RUSTC_WRAPPER='' \
  tools/build-components build-only app apps/wamn_receiving > "$WAMN_RESULTS/app-plan.json"
CARGO_TARGET_DIR="$WAMN_TREE/all-target" RUSTC_WRAPPER='' \
  tools/build-components build-only all > "$WAMN_RESULTS/all-plan.json"
WAMN_DIGEST_PROFILE_APP_PLAN="$WAMN_RESULTS/app-plan.json" \
WAMN_DIGEST_PROFILE_ALL_PLAN="$WAMN_RESULTS/all-plan.json" \
  cargo test --locked --offline -p wamn-conformance-tests --test guest_workspace_closure \
  one_commit_built_under_two_profiles_yields_identical_guest_digests \
  -- --include-ignored --exact --nocapture
```

Inspect the plans and comparison output before changing digest pins.
Use these comparisons when guest dependencies, selected features, workspace membership, or build flags change.
