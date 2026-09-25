# Building

Run commands from the repository root with the pinned toolchain and committed lock files.
The native toolchain is Rust 1.98.1 with Clippy, rustfmt, and `wasm32-wasip2`.
Native builds require system compiler tools and `protoc`.
The guest workspaces are `apps/Cargo.toml` and `apps/platform/no-std/Cargo.toml`.
[Components](../architecture/components.md) defines artifact and interface boundaries.

## Native programs and app components

Build native programs in debug mode by default:

```bash
cargo build --locked --offline -p wamn-host -p wamn-ctl -p wamn-identity \
  -p wamn-scenario-worker \
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
If an overlay is composed and its base app is not in the selection, the build fails.
Do not combine guests into a new grouped Cargo invocation.
Cargo combines dependency features within each invocation, which can change artifact bytes.

[`tools/guest-rustflags`](../../tools/guest-rustflags) sets the RUSTFLAGS of every guest build.
It maps the repository to `/wamn` and the Cargo home to `/cargo`, so no guest carries a local path.
`tools/build-components` and the Dockerfile `component-builder` stage call it. Nothing else sets guest RUSTFLAGS.
The build tool refuses a built guest that contains `/home/`, the Cargo home, or the repository path.
A guest crate can hold a pin file beside its `Cargo.toml`, named for the artifact, for example `http_route.wasm.sha256`.
The build tool and the Dockerfile stage refuse a guest that does not match its pin.
If you change the router on purpose, write its new digest to `apps/platform/ingress/http-route/http_route.wasm.sha256`.
A plain `cargo build` of a guest does not use the script, so its bytes depend on the machine. Do not pin such a guest.

After virtualization, the tool composes each overlay whose component declaration names a base operation dependency.
`wamn-component-composer` joins the overlay component, each base component, and each participant component into one component.
[`tools/component-composition.json`](../../tools/component-composition.json) names the participant crates of each overlay package under `participants`.
It names the generated no-op participant crates of each base package under `no_op_participants`.
A selected application also builds the crates named for it.
The declarations give each link.
The overlay imports each base operation that it depends on, and the base supplies that operation.
The base imports its pre-commit interface, and the participant that the overlay dependency names supplies it.
If the dependency names no participant, the base's no-op participant fills an optional slot. It stays inside the composition and is not exported.
Composition refuses a required slot that has no participant.
The composed component exports every export of its members.
Imports that no member supplies stay imports of the composed component.
Each member is embedded with its bytes unchanged.
The composed component replaces the overlay output in `virtualized/std-empty-environment`.
The composer does not compare the base bytes with the digest pin in `wamn.json`. Publication checks the pin.

The build tool requires `jq` and `sha256sum`.
`build-only app APP_DIRECTORY...` and `build-only all` emit an artifact plan to stdout.
`virtualize-only ARTIFACT_PLAN` refuses changed inputs or raw hashes before it updates outputs. It then composes the overlays.
`watch-roots app APP_DIRECTORY...` lists selected source dependencies without building them.
It also includes the shared platform WIT sources, so interface edits rebuild the selected components.
For a manifest-only change, inspect `cargo metadata --no-deps` before deciding whether compilation is needed.

WIT bindings read canonical package directories through ordered `path` lists. Put dependencies before the consumer world.
The router owns `wamn:node`. The runtime owns PostgreSQL, connection, JetStream, flow routing, and blobstore contracts.
The execution host owns router delivery. The materializer owns the shared WASI CLI and clock packages.
Application contracts remain under each package's `generated/wit` directory. Do not copy platform WIT into application directories.

## The edge box binary

The edge box is an aarch64 Linux computer, for example a Raspberry Pi 3B.
Build its release binary with the Dockerfile target `edge`:

```bash
docker build --target edge --output type=local,dest=target/edge .
```

The result is one stripped file, `target/edge/wamn-edge`.
The target compiles with the Debian trixie cross compiler, so the box needs glibc 2.38 or later.
[The edge specification](../plan/edge.md) section 4.9 records the measured size and dependency counts.

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

Both comparisons use one Cargo home, so they do not test the Cargo home mapping.
To test it, build one guest with a second `CARGO_HOME` path and compare its digest.

Inspect the plans and comparison output before changing digest pins.
Use these comparisons when guest dependencies, selected features, workspace membership, or build flags change.
