# Building

Run commands from the repository root with the pinned toolchain and committed lock files.
The native toolchain is Rust 1.99.0 with Clippy, rustfmt, and `wasm32-wasip2`.
Native builds require system compiler tools and `protoc`.
The web workspace uses Node 24.21.0 LTS and pnpm 12.9.1.
TypeScript 7 compiles the web packages. The TypeScript 6 compatibility package supplies the API that ESLint requires.
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

[`wamn build`](#wamn-build) compiles the components of each package it builds (platform-deploy.md §7.1 step 7).
Each guest gets its own Cargo invocation, with the release profile, for `wasm32-wasip2`.
Do not combine guests into one Cargo invocation.
Cargo combines dependency features within each invocation, which can change artifact bytes.
A package build compiles these guests:

- each component that the package owns;
- each participant crate that [`tools/component-composition.json`](../../tools/component-composition.json) names for the package under `participants`;
- each no-op participant crate that the build generated into the package's build output;
- each palette component that a wiring of the package names. The build finds it by the `component` of its `declaration.json.in` under `apps/platform/*/*/`, and builds the crate in that directory.

A package outside a Cargo workspace, with no `../Cargo.toml`, has no component to compile.
Cargo writes the guests to its target directory, `apps/target` unless `CARGO_TARGET_DIR` names another.

[`tools/guest-rustflags`](../../tools/guest-rustflags) sets the RUSTFLAGS of every guest build.
It maps the repository to `/wamn` and the Cargo home to `/cargo`, so no guest carries a local path.
It also maps the toolchain's `rust-src` tree to `/rustc/<commit-hash>`, so a toolchain with and without `rust-src` gives one digest.
`wamn build` and the Dockerfile `component-builder` stage call it. Nothing else sets guest RUSTFLAGS.
The build refuses a built guest that contains `/home/`, the Cargo home, or the repository path.
A guest crate can hold a pin file beside its `Cargo.toml`, named for the artifact, for example `http_route.wasm.sha256`.
The build and the Dockerfile stage refuse a guest that does not match its pin.
If you change the router on purpose, write its new digest to `apps/platform/ingress/http-route/http_route.wasm.sha256`.
A plain `cargo build` of a guest does not use the guest RUSTFLAGS, so its bytes depend on the machine. Do not pin such a guest.
The workload guests `http-route` and `materializer` belong to no package. Release qualification and the cluster cases build each one with the same guest build, through `wamn_schema_generator::build::build_guest`.

The build virtualizes each component that the package owns, each no-op participant, and each crate that [`tools/component-virtualization.json`](../../tools/component-virtualization.json) lists under `packages`.
It writes the virtualized guest to `virtualized/std-empty-environment` in the Cargo target directory.
The list is an allowlist, not discovery: a std guest that it does not name stays raw and fails its own admission.

The build then composes an overlay whose component declaration names a base operation dependency.
`wamn-component-composer` joins the overlay component, each base component, and each participant component into one component.
It reads each base component and each base no-op participant from the base's `build.json`, so build the base first.
A no-op participant crate is not an apps workspace member. It is a standalone crate in the build output of its base package, `apps/target/wamn/<package>/`.
The build copies `apps/Cargo.lock` beside the crate, lets Cargo trim it to the crate, and builds the crate by `--manifest-path` with `--locked` into the apps target directory.
The declarations give each link.
The overlay imports each base operation that it depends on, and the base supplies that operation.
The base imports its pre-commit interface, and the participant that the overlay dependency names supplies it.
If the dependency names no participant, the base's no-op participant fills an optional slot. It stays inside the composition and is not exported.
Composition refuses a required slot that has no participant.
The composed component exports every export of its members.
Imports that no member supplies stay imports of the composed component.
Each member is embedded with its bytes unchanged.
The composed component replaces the overlay's virtualized output.
The composer does not compare the base bytes with the digest pin in `wamn.json`. Publication checks the pin.

Last, the build copies each component that the package owns, each palette component that its wirings name, and each no-op participant to `components/<name>.wasm` in the package's build output. It records each one in `outputs.components` of the package's `build.json`, with its path and `sha256` digest.
`wamn-ctl push-package` and the dev loop read `build.json` for the bytes of each component.
`push-package` refuses a component without an entry, and a file whose digest differs from its entry.
For the `build.json` that `push-package` needs, see [Pushing a package](#pushing-a-package).
The dev loop watches the package roots, `apps/platform`, and the shared platform WIT sources, so an edit of any of them rebuilds the components.

WIT bindings read canonical package directories through ordered `path` lists. Put dependencies before the consumer world.
The router owns `wamn:node`. The runtime owns PostgreSQL, connection, JetStream, flow routing, and blobstore contracts.
The execution host owns router delivery. The materializer owns the shared WASI CLI and clock packages.
Application contracts lie under `wit/` in each package's build output, `apps/target/wamn/<package>/wit`, which `wamn build` writes. Do not copy platform WIT into application directories.

## Package manifests

A package that holds `wamn.k` authors its manifest in KCL.
`wamn build` compiles `wamn.k` to `wamn.json` in the package's build output before it generates anything else.
Every reader reads that compiled file.
A package that holds both `wamn.k` and an authored `wamn.json` is refused.

The compile runs the KCL CLI that [`tools/install-kcl`](../../tools/install-kcl) installs.
That script pins the version and digest, and installs the binary under `.tools/kcl/`.
The generator runs the script itself, so you do not install KCL by hand.
If the repository is absent, set `WAMN_KCL` to a `kcl` binary of the pinned version.
The schema module that `wamn.k` imports as `manifest` is [`crates/schema/generator/kcl/manifest`](../../crates/schema/generator/kcl/manifest/manifest.k).

## wamn build

`wamn build` builds packages in two passes ([platform-deploy.md §7.1](../plan/platform-deploy.md)).
It needs a PostgreSQL server, which it does not start.
Give a superuser URL of the server in `DATABASE_URL` or `--database-url`.
The build creates its own verification database on that server and drops it at the end.
The server keeps the roles `wamn_app` and `wamn_db_owner` if the build had to create them.

```bash
DATABASE_URL=postgres://postgres:<password>@127.0.0.1:<port>/postgres \
  cargo run --locked --offline -p wamn-ctl --bin wamn -- build \
  apps/wamn_receiving apps/client_acme_receiving
```

Pass one compiles `wamn.k` and migrates the verification database.
The migrations of each base package run first, then the package's own migrations, then the history tables.
The build then describes every authored statement through SQLx and writes the canonical `wamn.json`.
Pass two generates every other output from those `wamn.json` bytes.

A statement value in `wamn.k` can leave out its `type`, and the build takes the type from SQLx.
A stated type that differs from the type SQLx gives is refused.
A statement value can also leave out `nullable`.
A row value without it takes the nullability that SQLx gives. If SQLx cannot tell, as for an expression, the value is nullable.
A parameter without it is not null.
A row value stated `nullable = True` is kept, even when SQLx says that the column cannot be null.
A row value stated `nullable = False`, where SQLx says that the column can be null, is refused.
If SQLx cannot tell, the stated `nullable = False` stands.
The refusal names the operation, the statement, and the value.

The build writes each package to `<output root>/<package id>/`.
The default output root is `apps/target/wamn` for an application, and `--output-root` changes it.
Below it, the layout keeps the logical `generated/` paths that contracts and releases name, without the `generated/` prefix, plus `build.json`.
Git holds no build output (platform-deploy.md R10(1)), and the repository policy lint refuses a tracked `generated/` or `.sqlx` file under `apps/`.
Build every package before you compile the apps workspace, the app test crates, or an app web: their sources include files from the build output.
`build.json` is the build receipt, schema `wamn.build/v1`, in canonical JSON.
It records the input files and their digest, the base packages, the generator identity, and the digest of each output.
An overlay reads the `build.json` of each base package, so build the base first.
For a package that has a `web/` directory, the build then runs `pnpm run build` there with Vite's `outDir` set to `web/` in the package's build output, `apps/target/wamn/<package>/web` (platform-deploy.md §7.1 step 8).
Run `pnpm install` at the repository root first.
`build.json` records the digest of the web files as `outputs.web`, and `null` for a package without a web client.
Each app's `vite.config.ts` names the same directory, so `pnpm run build` by hand writes there too.
The build compiles the package's components as [above](#native-programs-and-app-components) describes.

## Pushing a package

`wamn-ctl push-package --package <package root>` pushes one package artifact ([platform-deploy.md §7.2](../plan/platform-deploy.md)).
First it compares the `inputs.digest` of the package's `build.json` with the digest of the package tree.
If they are equal, it pushes the build output as it is.
If they differ, or the package has no `build.json`, `push-package` refuses and pushes nothing.
The refusal tells you to run `wamn build`.
`push-package` never builds, and it never compares file times.

## Reading a pushed package

Layer 0 of a package artifact is the canonical `wamn.json` of the package.
`wamn inspect <digest>` prints layer 0 of the pushed artifact, byte for byte.
`wamn diff <digest> <digest>` prints each JSON pointer where layer 0 of the two artifacts differs.
Each argument is the full package artifact digest that `push-package` prints, `sha256:` and 64 hex digits, or the tag `<package_id>-<version>`.
A digest prefix is refused.
A line that starts with `-` shows the value in the first artifact, and a line that starts with `+` shows the value in the second.
Identical documents print nothing.
Both commands pull from the registry only, by full digest or tag; no database holds a package row.
They check each layer against its digest.
They take the registry flags of `wamn-ctl apply-package`: `--artifact-base`, `--registry-auth-file`, `--insecure-registry` and `--oci-ca-path`.

```bash
"$CARGO_TARGET_DIR/debug/wamn" inspect <digest> \
  --artifact-base <registry>/wamn/packages \
  --registry-auth-file <auth file>
```

## Host route clients

The TypeScript clients of the host routes, `@wamn/control-client` and `@wamn/control-org-client`, are build output, and Git does not hold them.
The generator makes them from the contracts in `crates/catalog/model/src/host_route`.
It writes them to `target/wamn/wamn_control/client-ts` and `target/wamn/wamn_control/control/client-ts` at the repository root.
`CARGO_TARGET_DIR` does not move them.

The web scripts write the clients before they read them.
`pnpm -r build`, `pnpm run check`, `pnpm test` and `pnpm run dev` each run `web/host-route-clients.mjs` first, which runs this command from the repository root:

```bash
cargo run --locked --offline -p wamn-schema-generator --example materialize_host_route_client
```

The command writes only a changed file, and it removes a file that the contracts no longer generate.

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
Run `wamn build` of every package in each worktree.
Then pass their `virtualized/std-empty-environment` directories of the Cargo target to the existing comparison:

```bash
WAMN_DIGEST_REPRO_A="$GUEST_REPRO_FIRST_ARTIFACTS" \
WAMN_DIGEST_REPRO_B="$GUEST_REPRO_SECOND_ARTIFACTS" \
  cargo test --locked --offline -p wamn-conformance-tests --test guest_workspace_closure \
  one_commit_built_in_two_checkouts_yields_identical_guest_digests \
  -- --include-ignored --exact --nocapture
```

The comparison uses one Cargo home, so it does not test the Cargo home mapping.
To test it, build one guest with a second `CARGO_HOME` path and compare its digest.

Inspect the comparison output before changing digest pins.
Use these comparisons when guest dependencies, selected features, workspace membership, or build flags change.
