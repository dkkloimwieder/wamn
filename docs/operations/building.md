# Building

Run commands from the repository root with the pinned toolchain and committed lock files.
The native toolchain is Rust 1.99.0 with Clippy, rustfmt, and `wasm32-wasip2`.
Native builds require system compiler tools and `protoc`.
The web workspace uses Node 24.21.0 LTS and pnpm 12.9.1.
TypeScript 7 compiles the web packages. The TypeScript 6 compatibility package supplies the API that ESLint requires.
The guest workspaces are `apps/Cargo.toml` and `apps/platform/no-std/Cargo.toml`.
[Components](../architecture/components.md) defines artifact and interface boundaries.

Before a lane or test run, prepare the checkout once from its repository root:

```bash
eval "$(cargo xtask lane-start "$PWD")"
```

The command initializes submodules and selects the Cargo cache directory.
It runs no builds, web installation, or database.
Build the artifacts required by the selected command.
For tests, [the selection file](../../tests/selection.toml) binds preparation to its consuming targets and suites.
The command prints two exports.
`CARGO_TARGET_DIR` is `$HOME/.cache/wamn-target/<key>`, and `WAMN_CHECKOUT_KEY` is the key.
Cargo writes its output to `CARGO_TARGET_DIR`, so commands in this repository use `$CARGO_TARGET_DIR` and not `target/` or `apps/target`.
`wamn build` still writes package output to `apps/target/wamn`.

Platform installations use the full schema files under `deploy/sql/`.
These files own each schema.

## Platform version

The platform version lives in `deploy/platform/release/Chart.yaml`.
Keep `version` and `appVersion` equal.
`cargo xtask build-platform` reads this version to tag images and role components.
BuildKit decides which layers need to run again.

## Compile cache

Cargo runs `rustc` through `kache`, because `~/.cargo/config.toml` sets `rustc-wrapper = "kache"`.
The `.cargo/config.toml` of the repository sets no wrapper, so Docker builds need none.
A session shell that does not load `~/.profile` must export these variables:

```bash
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER='kache test-runner'
export PATH="$HOME/.local/lib/kache/shims:$PATH"
```

Do not set `RUSTC_WRAPPER=sccache`.

## Native programs and app components

Build native programs in debug mode by default:

```bash
cargo build --locked --offline -p wamn-host -p wamn-ctl -p wamn-identity \
  -p wamn-cdc-reader
cargo build --locked --offline -p wamn-ctl --features ops --bin wamn-ctl-ops
```

`wamn-host` uses mimalloc as its global allocator. Cargo builds its bundled C source with the existing C toolchain; no other program changes allocator.

`wamn` owns the developer commands.
`wamn-ctl` owns provisioning and publication.
`wamn-ctl-ops` owns database maintenance and retained event advisory commands.
The local identity issuer needs `wamn-identity` beside the CLI or at the explicit `WAMN_IDENTITY_BINARY` path.

[`wamn build`](#wamn-build) compiles the components of each package it builds.
Each guest gets its own Cargo invocation, with the release profile, for `wasm32-wasip2`.
Do not combine guests into one Cargo invocation.
Cargo combines dependency features within each invocation, which can change artifact bytes.
A package build compiles these guests:

- each component that the package owns.

A package outside a Cargo workspace, with no `../Cargo.toml`, has no component to compile.
Cargo writes the guests to `CARGO_TARGET_DIR`. If that variable is unset, it writes them to `apps/target`.

`cargo xtask guest-rustflags` prints the RUSTFLAGS used by the shared xtask build library.
It maps the repository to `/wamn` and the Cargo home to `/cargo`, so no guest carries a local path.
It also maps the toolchain's `rust-src` tree to `/rustc/<commit-hash>`, so a toolchain with and without `rust-src` gives one digest.
`wamn build` calls it. The platform producer uses that same component builder.
The build refuses a built guest that contains `/home/`, the Cargo home, or the repository path.
A guest crate can hold a pin file beside its `Cargo.toml`, named for the artifact, for example `http_route.wasm.sha256`.
The build and the Dockerfile stage refuse a guest that does not match its pin.
If you change the router on purpose, write its new digest to `apps/platform/ingress/http-route/http_route.wasm.sha256`.
A plain `cargo build` of a guest does not use the guest RUSTFLAGS, so its bytes depend on the machine. Do not pin such a guest.
The release publisher also needs the pinned `http-route` guest.
The ctl image carries it at `/usr/local/share/wamn/http_route.wasm`.
For native publication, set `WAMN_FLOW_HTTP_COMPONENT` to the built guest and `WAMN_COMPONENT_ARTIFACT_BASE` to the component repository.
Publication pushes the separate route component to `<component-artifact-base>/entries`.
The manifest records only the path `entries@sha256:<manifest-digest>`, relative to the component artifact base.

The workload guests `http-route` and `materializer` belong to no package.

The platform build publishes the pinned route component without an application or dispatcher.
The publisher uses the same route bytes for each application release.
Control endpoints run natively and need no HTTP guest artifact.
The build leaves standard WASI imports unchanged. The host supplies their empty application context.

The build then composes each component of a package that pins bases in `base_dependencies`.
`wamn-component-composer` joins the overlay component and each base whose operations it imports into one component.
It reads each base component from the base's `build.json`, so build the base first.
The overlay imports each base operation that it calls under the base's export name, and the base supplies that operation.
An overlay that imports no base operation stays as it is.
Each member that imports `wamn:postgres/statements` gets its own outer import, `sql-0`, `sql-1`, in member order ([components](../architecture/components.md)).
The composed component exports every export of its members.
Imports that no member supplies stay imports of the composed component.
Each member is embedded with its bytes unchanged.
The composed component replaces the overlay's raw output.
`wamn build` compares each base component's bytes with the digest pin in `wamn.json` before compilation.
Publication repeats this comparison.

Last, the build copies each component that the package owns to `components/<name>.wasm` in the package's build output. It records each one in `outputs.components` of the package's `build.json`, with its path and `sha256` digest.
`wamn-ctl push-package` and the dev loop read `build.json` for the bytes of each component.
`push-package` refuses a component without an entry, and a file whose digest differs from its entry.
For the `build.json` that `push-package` needs, see [Pushing a package](#pushing-a-package).
The dev loop watches the package roots, `apps/platform`, and the shared platform WIT sources, so an edit of any of them rebuilds the components.

WIT bindings read canonical package directories through ordered `path` lists. Put dependencies before the consumer world.
The router owns `wamn:node`. The runtime owns PostgreSQL, connection, JetStream, flow routing, and blobstore contracts.
The execution host owns router delivery. The materializer owns the shared WASI CLI and clock packages.
Application contracts lie under `wit/` in each package's build output, `apps/target/wamn/<package>/wit`, which `wamn build` writes. Do not copy platform WIT into application directories.

## Platform artifacts

`cargo xtask build-platform` builds, pushes, and stamps the platform artifacts.
It builds `host`, `ctl`, `identity`, and `edge-web` through Docker BuildKit.
The two role components use the package component builder.
Each guest gets its own Cargo invocation.
The HTTP role publishes the raw route component after the component builder checks its pin.
The ctl image receives the same checked HTTP component through a named BuildKit context.

The registry must already be available and authenticated through the Docker configuration.
For local deployment, use the registry address in `deploy/kind/cluster.k`.
The output directory must not exist.

```bash
cargo xtask build-platform --registry localhost:5001 --out /var/tmp/wamn-platform-chart
```

Each image uses the registry cache `<registry>/wamn-<image>:buildcache`.
BuildKit imports that cache and exports all build stages after a successful build.
The command runs on the machine where you call it.
It does not choose another machine or manage a cloud spending counter.
A failure stops publication at that step. Images already pushed remain in the registry.

To stamp artifacts that already exist, run:

```bash
cargo xtask build-platform --stamp-only --registry localhost:5001 --out /var/tmp/wamn-platform-chart-restamped
```

Stamping reads every selected digest from the registry and refuses a missing artifact.
The chart holds the pinned images and role components in `values.yaml`.
The output also carries the runtime operator archive, the identity and edge charts, and deployment resources under `platform/`.
`palette.json` records immutable platform artifact pins and descriptors sealed against the published guest bytes.
The platform owns palette guests under `wamn_palette`, independently of application package membership.
The platform distribution places the document and digest-named guest files beside `http_route.wasm`.
Release publication and development use the same reader for that document and verify each guest digest.
They keep palette ports and node configuration. Palette components receive no public HTTP routes.
Packages no longer bundle palette guests.
Transfer the whole output directory to another deployment machine.
[Local deployment](deployment.md#local-platform) consumes it through `--chart`.

An explicit `--cluster` document with a web bucket selects the platform UI.
Install web dependencies with `pnpm install --offline` first.
The producer builds `@wamn/platform-ui` and publishes `<registry>/wamn/platform-ui:<platform version>`.
The chart records its digest in `platformUi`.
With `--stamp-only`, the command reads the published UI without a local bundle.

## Repository tasks and tools

`cargo xtask --help` lists repository tasks. The `justfile` calls those commands.
[mise.toml](../../mise.toml) pins Helm, KCL, OpenTofu, and wash with download checksums.
Run `mise install` to install them, then activate mise or use `mise exec -- <command>`.
The pins retain the Linux x86_64 artifacts used by the former installers.
Docker receives its Helm and KCL download pins from that same file.
The [mise HTTP backend](https://mise.jdx.dev/dev-tools/backends/http.html) verifies those checksums.

## Package manifests

A package that holds `wamn.k` authors its manifest in KCL.
`wamn build` compiles `wamn.k` to `wamn.json` in the package's build output before it generates anything else.
Every reader reads that compiled file.
A package that holds both `wamn.k` and an authored `wamn.json` is refused.

`cargo xtask install-kcl` installs the KCL CLI under `.tools/kcl/`.
It reads the version, URL and checksum from `mise.toml`.
Package builds install it when it is absent.
Set `WAMN_KCL` to use another binary of the pinned version.
The compiler runs KCL and never installs tools.
Platform document schemas live with their document types in `crates/control/environment/kcl/`.
The OpenTofu renderer reads `ProjectDocument` in that same crate.
The compiler owns the authored model in `crates/schema/generator/src/manifest.rs`. It checks document rules once and derives inherited model fields once. Catalog readers decode compiled facts without repeating document checks.

The schema module that `wamn.k` imports as `manifest` is [`crates/schema/generator/kcl/manifest`](../../crates/schema/generator/kcl/manifest/manifest.k).

- Plain English names in the schema module; use `type` for variant names; no plurals that the manifest does not already use.

An overlay's `wamn.k` imports nothing from its base; `base_dependencies` stays a declaration (package, version, digest, operations) as today, because the pin is the contract and a language-level import would let the overlay drift from the sealed base. The language adds comments and defaults to the overlay; it does not compose the two packages.

## wamn build

`wamn build` compiles each package through one SQL corpus.
It needs a PostgreSQL server, which it does not start.
Give a superuser URL of the server in `DATABASE_URL` or `--database-url`.
The build creates its own verification database on that server and drops it at the end.
The server keeps the roles `wamn_app` and `wamn_db_owner` if the build had to create them.

```bash
DATABASE_URL=postgres://postgres:<password>@127.0.0.1:<port>/postgres \
  cargo run --locked --offline -p wamn-ctl --bin wamn -- build \
  apps/wamn_receiving apps/client_acme_receiving
```

The build compiles `wamn.k` and migrates the verification database.
The migrations of each base package run first, then the package's own migrations, then the history tables.
SQLx describes authored statements to fill their declared value types.
The compiler derives relations and feed sources, then generates the remaining SQL once.
SQLx describes every generated statement, including each fixed list sort and each feed statement.
The compiler checks each accessor's parameters and selected columns against those descriptions before it writes consumers.
One type table in `wamn-scalar` maps PostgreSQL names for build and runtime.

The build classifies writes and row locks from PostgreSQL plans.
A call to an authored routine also counts as a write and retains the command transaction.
It generates no application grant contribution and plans without installing derived grants.
Deployable routine support remains incomplete.
The compiler writes history changes as SQLx migrations under `history/` in the package source and build output.
Keep these files in source control. Each file remains unchanged after release.
Authored `migrations/` and generated `history/` share one version sequence.
Choose each new authored version above every existing version in both directories.
A history declaration change appends a generated migration, even when authored SQL stays unchanged.
The build executes the combined sequence in its verification database before it returns output.

A statement value in `wamn.k` can leave out its `type`, and the build takes the type from SQLx.
A stated type that differs from the type SQLx gives is refused.
A statement value can also leave out `nullable`.
A row value without it takes the nullability that SQLx gives. If SQLx cannot tell, as for an expression, the value is nullable.
A parameter without it is not null.
A row value stated `nullable = True` is kept, even when SQLx says that the column cannot be null.
A row value stated `nullable = False`, where SQLx says that the column can be null, is refused.
If SQLx cannot tell, the stated `nullable = False` stands.
The refusal names the operation, the statement, and the value.

A consuming workspace package lists application clients in its `package.json` field `"wamn": { "clients": ["wamn_wms"] }`.
`wamn build` writes each named client as plain source under that consumer's `src/generated/<app>/`, without a `package.json`.
If the build cannot produce a requested client, it fails and names the consuming package and application.

The build writes each package to `<output root>/<package id>/`.
Package builds write to the workspace output root, `apps/target/wamn` for applications.
There is no custom-output option: guest includes and artifact readers use this same path.
Below it, the layout keeps the logical `generated/` paths that contracts and releases name, without the `generated/` prefix, plus `build.json`.
Git holds no build output, and the repository policy lint refuses a tracked `generated/` or `.sqlx` file under `apps/`.
Generation replaces obsolete output files.
The output includes copies of the authored KCL, SQL and migrations used by this build.
Build every package before you compile the apps workspace, the app test crates, or an app web: their sources include files from the build output.
`build.json` is the build record, schema `wamn.build/v1`, in canonical JSON.
It records the package, base packages, compiler identity and output digests.
The build writes it once, after component compilation and client materialization finish.
The compiler derives its identity from Cargo, the toolchain file and the dependency lock.
Cargo decides whether guest code needs compilation.
Every package build regenerates its compiler output without source freshness checks.
Artifact readers take every file from this output tree.
An older completed build publishes its older inputs and outputs together.
An overlay reads each base package's completed record, so build bases first.
The build calls the composer library directly.
The build performs no WASI byte rewriting.
`wamn build` does not run Vite or record browser files in `build.json`.

`wamn web build` builds a standalone web artifact against the recorded package contract.
It permits web source changes and requires the compiled manifest to match `outputs.manifest`.
See [web clients](web-upload.md#build-and-push) for its output, metadata, and registry commands.

### Ejecting defaults

`wamn eject action apps/wamn_receiving purchase_order.create` replaces the empty action implementation with an authored module.
The module contains the current default body.
Its WIT, SQL and input validation stay generated.
The command refuses an existing implementation or output file.

`wamn eject component apps/wamn_receiving purchase_order path/to/purchase_order.ts` copies one browser component to an authored file.
Import that file through normal application composition.
Generated client copies stay under `src/generated/`.

## Pushing a package

`wamn-ctl push-package --package <package root>` pushes one package artifact.
The command requires a completed `build.json` and reads the compiled package output.
It verifies component bytes and publishes the recorded build without comparing source files.
`push-package` never builds, and it never compares file times.

## Reading a pushed package

Layer 0 of a package artifact is the canonical `wamn.json` of the package.
`wamn inspect <digest>` prints layer 0 of the pushed artifact, byte for byte.
`wamn diff <digest> <digest>` prints each JSON pointer where layer 0 of the two artifacts differs.
Each argument is the full package artifact digest that `push-package` prints, `sha256:` and 64 hex digits, or the tag `<package_id>-<version>`.
A digest prefix is refused.
A line that starts with `-` shows the value in the first artifact, and a line that starts with `+` shows the value in the second.
Identical documents print nothing.
Both commands pull from the registry only, by full digest or tag, because no database holds a package row.
They test each layer against its digest.
They take the registry flags of `wamn-ctl apply-package`: `--artifact-base`, `--registry-auth-file`, `--insecure-registry` and `--oci-ca-path`.

```bash
"$CARGO_TARGET_DIR/debug/wamn" inspect <digest> \
  --artifact-base <registry>/wamn/packages \
  --registry-auth-file <auth file>
```

**Contract A** (`wamn-vavs4.1`, `crates/control/package/src/package_artifact.rs`). A component artifact is the object `push-package` pushes for each component. `push-component` is gone (`wamn-vavs4.7`). Its tag is the 64 hex digits of the component digest. Its image manifest has media type `application/vnd.oci.image.manifest.v1+json`, `schemaVersion` 2 and no `artifactType`. Its config is `application/vnd.wamn.component.config.v1+json`, the canonical JSON `{format-version: "0.1", component-digest, imports, imports-fingerprint}` (`component_artifact_config_bytes`). It has one layer, `application/vnd.wamn.component.v1+wasm`, with the exact wasm bytes. These media types live in `crates/platform/engine/src/component_artifact.rs`.

A package artifact has the tag `<package_id>-<version>`. Its image manifest has media type `application/vnd.oci.image.manifest.v1+json` and `artifactType` `application/vnd.wamn.package.v2` (a v1 reader requires exactly one layer). Its config is `application/vnd.oci.empty.v1+json` with the bytes `{}`. Its layers are always present, in this order; `title` is the annotation `org.opencontainers.image.title`:

| Layer | Media type | Title | Content |
|---|---|---|---|
| 0 | `application/vnd.wamn.package.manifest.v1+json` | `wamn.json` | the exact bytes of the canonical `wamn.json` in `apps/target/wamn/<package>/`, not re-encoded |
| 1 | `application/vnd.wamn.package.v1.tar` | `migrations` | `migrations/` and `history/` |
| 2 | `application/vnd.wamn.package.v1.tar` | `publication` | `generated/publication/`: final attachments and component declarations |
| 3 | `application/vnd.wamn.package.v1.tar` | `sources` | `wamn.k`, `command/`, `query/`, `generated/required-schema-contract.json`, `generated/sql/`, `generated/routes/` |
| 4..n | `application/vnd.wamn.component.descriptor.v1+json` | none | one descriptor per package-owned component, sorted by name, with the annotations `wamn.component.name` and `wamn.component.digest` |

The package artifact contains three tar layers and no browser files.
A web source or output edit leaves the packed package digest unchanged.
`wamn web build` and `wamn web push` own the separate [web artifact](web-upload.md#build-and-push).

The `generated/` paths of layers 2 and 3 are logical paths. `wamn build` writes those files below `apps/target/wamn/<package>/` without the `generated/` prefix, and pack reads them from there.

A tar has GNU headers, sorted package-relative paths, mode 0644, uid and gid 0 and mtime 0. A descriptor is the canonical JSON of `ComponentDescriptor` (`crates/catalog/model/src/component_facts.rs`, `deny_unknown_fields`): `{component, interface-version, component-digest, operations, imports, imports-fingerprint, effects, connections: [{store-alias, requirement-type}]}`, which is `AdmittedComponent` without its target plus its connections. The reader supplies the target. The manifest annotations are `wamn.package.id`, `wamn.package.version` and one `wamn.component.<name>` = `sha256:...` per component. Nothing carries a timestamp. The manifest is canonical JSON.

The artifact digest is the SHA-256 of the image manifest bytes. The registry records it, and it is what `artifact_digest` means. An unpacked artifact is not build output. The three tars keep their logical paths. Layer 0 goes to `generated/wamn.json`. Each descriptor goes to `descriptors/<name>.json`. It packs again offline to the pushed digest. `package_artifact_digest(root)` is that offline digest of a tree as it is. A source tree has no `descriptors/`, so its digest differs from the pushed digest. Only an unpacked tree packs to it.

## Host route clients

The TypeScript clients of the host routes, `@wamn/control-client` and `@wamn/control-org-client`, are build output, and Git does not hold them.
The generator makes them from the contracts in `crates/catalog/model/src/host_route`.
It writes them to `apps/target/wamn/wamn_control/client-ts` and `apps/target/wamn/wamn_control/control/client-ts`, below the output root of `wamn build`.
It then copies them into each workspace package that names them in `package.json` `"wamn": { "clients": [...] }`, as `wamn build` does for an application client.
The name `wamn_control` selects the application route client, and `wamn_control_org` selects the control route client.
A consuming package imports the copy from `src/generated/<name>`, which Git ignores.

The web scripts write the clients before they read them.
`pnpm -r build`, `pnpm run lint`, `pnpm test` and `pnpm run dev` run this command first:

```bash
cargo xtask host-route-clients
```

The command writes only a changed file, and it removes a file that the contracts no longer generate.

## The edge box binary

The edge box is an aarch64 Linux computer, for example a Raspberry Pi 3B.
Build its release binary with the Dockerfile target `edge`:

```bash
docker build --build-arg WAMN_CHECKOUT_KEY="$(printf '%s' "$(realpath .)" | sha256sum | cut -c1-16)" --target edge --output type=local,dest="$CARGO_TARGET_DIR/edge" .
```

The result is one stripped file, `$CARGO_TARGET_DIR/edge/wamn-edge`.
The target compiles with the Debian trixie cross compiler.
The cluster image `edge-web` is a different build.
The Dockerfile stage `build-edge-web` builds the separate `wamn-web-front` service for linux/amd64.

## Isolated worktrees

Keep one Cargo process per target directory.

Each checkout keeps its own Docker target cache and its own host target directory.
The native images `host`, `ctl`, and `identity` use one `build-native` stage.
The host image also carries `wamn-cdc-reader`.
It compiles every native package in one `cargo build`, so features unify once.
The `edge-web` image has its own stage, `build-edge-web`. It shares the target cache of `build-native`.
A second invocation in the same stage builds only `wamn-ctl-ops` with the `ops` feature, because that feature reaches shared code and the shipped `wamn-ctl` builds without it.
The key uses the first 16 hexadecimal characters of the SHA-256 hash of the canonical absolute checkout path, with no trailing newline.
`cargo xtask lane-start` sets `CARGO_TARGET_DIR=$HOME/.cache/wamn-target/<key>` and exports the key as `WAMN_CHECKOUT_KEY`.
Pass that key as Docker's `WAMN_CHECKOUT_KEY` build argument.
A new checkout starts with a cold host build.

Reuse that worktree's own target, and let Cargo decide which artifacts need rebuilding.
Keep large build targets off temporary memory filesystems.
[Running tests](running-tests.md#capture-a-run) explains result reporting and optional output directories.

## Host image cache

The `host` image takes its binaries from the `build-native` stage.
That stage compiles every native package except `wamn-edge`, so the first image build of a platform build carries that compile time.
`build-native` and `build-edge-web` mount the target cache `wamn-root-target-native-<checkout key>`.
They share the Cargo registry and Git caches with every checkout.
The cache mounts are not in the image. The stage copies each binary out of the target cache to `/native-output`, and the image copies it from there.

To bump the platform version, change `version` and `appVersion` in `deploy/platform/release/Chart.yaml` to the same new value.
No other file holds the version.
A version change runs the same BuildKit build with new output tags. BuildKit reuses unchanged layers.

These changes make Cargo rebuild more:

- A changed `Cargo.lock`, Cargo feature, profile or `.cargo/config.toml` rebuilds the dependencies that they reach.
- A new Rust image under the `rust:1.99-trixie` tag rebuilds everything.
- A change to a widely used workspace crate rebuilds every crate that depends on it, and links each native binary again.
- A new checkout has a new key, so its first image build compiles everything.
- BuildKit garbage collection can remove a cache. Read the caches with `docker buildx du --verbose`.

## Guest artifact comparisons

Use two clean worktrees at the same commit to test checkout independence.
Give each worktree its own target and empty `RUSTC_WRAPPER`.
Run `wamn build` of every package in each worktree.
Then pass their `wasm32-wasip2/release` directories of the Cargo target to the existing comparison:

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
