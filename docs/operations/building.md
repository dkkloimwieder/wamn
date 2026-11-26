# Building

Run commands from the repository root with the pinned toolchain and committed lock files.
The pinned Rust 1.100.0 toolchain includes Clippy, rustfmt, and the `wasm32-wasip2` and `wasm32-wasip3` targets.
Native builds require system compiler tools and `protoc`.
The web workspace uses Node 24.21.0 LTS and pnpm 12.9.1.
TypeScript 7 compiles the web packages. The TypeScript 6 compatibility package supplies the API that ESLint requires.
The guest workspace is `apps/Cargo.toml`.
It includes application components, platform components under `apps/guests/`, guest libraries under `apps/crates/`, and guest fixtures under `apps/fixtures/guests/`.
[Components](../architecture/components.md) defines artifact and interface boundaries.

Before a lane or test run, prepare the checkout once from its repository root.
Set `CARGO_TARGET_DIR` first so that Cargo builds `xtask` in the checkout's cache directory.

```bash
checkout_key=$(printf '%s' "$(realpath .)" | sha256sum | cut -c1-16)
export CARGO_TARGET_DIR="$HOME/.cache/wamn-target/$checkout_key"
eval "$(cargo xtask lane-start "$PWD")"
```

The command initializes submodules and prints two exports.
Build the artifacts required by the selected command.
For tests, [the selection file](../../tests/selection.toml) binds preparation to its consuming targets and suites.
`CARGO_TARGET_DIR` is `$HOME/.cache/wamn-target/<key>`, and `WAMN_CHECKOUT_KEY` is the key.
Cargo writes its output to `CARGO_TARGET_DIR`, so commands in this repository use `$CARGO_TARGET_DIR` and not `target/` or `apps/target`.
`wamn build` still writes package output to `apps/target/wamn`.

Platform installations use the full schema files under `deploy/sql/`.
These files own each schema.

## Platform version

The platform version lives in `deploy/charts/app/Chart.yaml`.
The published application chart archive includes the platform charts.
Application releases install the stamped chart at `platform/app` inside that archive.
This keeps the platform files out of the Helm release record.
Helm stores each release record in one Secret.
A Secret holds at most 1 MiB (1048576 bytes).
`cargo xtask helm-size` renders the application release of each package under `apps/*/wamn.k` and prints the size of its record.
If a record is over 1 MiB, a build output is missing or Helm fails, the command fails.
Run `cargo xtask prepare-tests` first to build the packages.
Keep `version` and `appVersion` equal.
`cargo xtask build-platform` reads this version to tag images and role components.
The published defaults keep the host image at `platform.host.image`.
The prepared environment chart receives that image and owns the host runtime defaults.
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
cargo build --locked --offline -p wamn-host -p wamn-control -p wamn-cli -p wamn-identity-service \
  -p wamn-cdc-reader
```

A machine that builds offline runs cargo fetch once first.

`wamn-host` uses mimalloc as its global allocator. Cargo builds its bundled C source with the existing C toolchain; no other program changes allocator.

`wamn` owns the developer commands.
`wamn-control` serves authenticated org control routes and runs authorized operation Jobs.
`wamn` runs internal bootstrap steps.
Development uses the identity service installed on `kind`. The CLI needs no identity process beside it.

Build the workflow service for `wasm32-wasip3` from the repository root:

```bash
cargo build --manifest-path apps/Cargo.toml -p workflow-service --target wasm32-wasip3
```

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
`wamn build` and the platform producer use the same component builder.
That builder creates a source view under `CARGO_TARGET_DIR/wamn-guest`.
Every compiled local package sits below the build workspace in that view.
Cargo then hashes relative package paths, including packages outside `apps/`.
The builder preserves dependency declarations and remaps the source view to `/wamn`.
It refreshes the copied lock file offline for that workspace before the locked build.
The same sources, lockfile and toolchain produce the same bytes in different checkouts.
`cargo xtask build-component apps/guests/ingress/Cargo.toml` uses that path for one guest.
The build refuses a built guest that contains `/home/`, the Cargo home, or the repository path.
A guest crate can hold a pin file beside its `Cargo.toml`, named for the artifact, for example `ingress.wasm.sha256`.
The build and the Dockerfile stage refuse a guest that does not match its pin.
If you change the router on purpose, write its new digest to `apps/guests/ingress/ingress.wasm.sha256`.
A plain `cargo build` bypasses the source view, so its bytes can depend on the checkout path.
Use the component builder when you update a pin.
The release publisher also needs the pinned `ingress` guest.
The ctl image carries it at `/usr/local/share/wamn/ingress.wasm`.
For edge bundle tests, set `WAMN_INGRESS_COMPONENT` to the built guest.

The workload guest `ingress` belongs to no package.

The platform build publishes the pinned route component without an application or dispatcher.
The publisher uses the same route bytes for each application release.
Control endpoints run natively and need no HTTP guest artifact.
The build leaves standard WASI imports unchanged. The host supplies their empty application context.

The build then composes each component of a package that pins bases in `base_dependencies`.
`wamn-build` joins the overlay component and each base whose operations it imports into one component.
Build the base first. The overlay hashes the base’s compiled `wamn.json` and the component named in it.
The overlay imports each base operation that it calls under the base's export name, and the base supplies that operation.
An overlay that imports no base operation stays as it is.
Each member that imports `wamn:postgres/statements` gets its own outer import, `sql-0`, `sql-1`, in member order ([components](../architecture/components.md)). Each import borrows the same host operation transaction. Generated accessors include their SQL bytes. Publication carries no admitted statement sets.
The composed component exports every export of its members.
Imports that no member supplies stay imports of the composed component.
Each member is embedded with its bytes unchanged.
The composed component replaces the overlay's raw output.
`wamn build` compares each base component's bytes with the digest pin in `wamn.json` before compilation.
Publication repeats this comparison.

The build writes each package component to `components/<name>.wasm` under its build output.
Publication checks the fingerprint beside that output and hashes the files it reads.
The dev loop watches package roots, `apps/guests/`, shared WIT, and the local Cargo dependency closure.
An edit to these inputs rebuilds the affected components.

WIT bindings read canonical package directories through ordered `path` lists. Put dependencies before the consumer world.
Shared WIT packages define the node, PostgreSQL, connection, JetStream, HTTP authentication, and blobstore interfaces.
The HTTP guest calls typed application operations through `wamn:operation`. Native ingress handlers serve platform routes.
Workflow owns native event matching.
Shared WASI definitions live under `wit/wasi-cli` and `wit/wasi-clocks`.
Application contracts lie under `wit/` in each package's build output, `apps/target/wamn/<package>/wit`, which `wamn build` writes. Do not copy platform WIT into application directories.

## Platform artifacts

`cargo xtask build-platform` builds, pushes, and stamps the platform artifacts.
It builds `host`, `control`, `ctl`, `identity`, and `edge-web` through Docker BuildKit.
`build-platform` uses its own `wamn-platform` builder (docker-container driver, host network so it reaches `localhost:5001`), and creates it if it is missing.
The native host image includes `wamn-cdc-reader`.
The component builder runs Cargo separately for each guest. It builds the HTTP route and palette for `wasm32-wasip2` and the workflow service for `wasm32-wasip3`.
It publishes the HTTP route and workflow guests as OCI components under `<registry>/wamn/ingress` and `<registry>/wamn/workflow`.
The ctl image receives the checked HTTP component through a named BuildKit context.

The registry must already be available and authenticated through the Docker configuration.
For local deployment, use the registry address in `deploy/charts/platform/values-kind.yaml`.
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
The chart holds the pinned images and guest components in `values.yaml`.
The HTTP route uses `platform.roles.ingress.image`. The workflow service uses `platform.workflow.image`.
The output contains the platform, infrastructure, org, environment, project, and application charts under `platform/`.
`palette.json` records immutable platform artifact pins and descriptors sealed against the published guest bytes.
The platform owns palette guests under `wamn_palette`, independently of application package membership.
The platform distribution places the document and digest-named guest files beside `ingress.wasm`.
Release publication and development use the same reader for that document and verify each guest digest.
They keep palette ports and node configuration. Palette components receive no public HTTP routes.
Packages no longer bundle palette guests.
Transfer the whole output directory to another deployment machine.
[Local deployment](deployment.md#local-platform) consumes it through `--chart`.

Pass the Helm values file to `--cluster`. A value for `gcp.buckets.web` selects the platform UI.
Install web dependencies with `pnpm install --offline` first.
The producer builds `@wamn/platform-ui` and publishes `<registry>/wamn/platform-ui:<platform version>`.
The org chart records its digest in `control.platformUi`.
With `--stamp-only`, the command reads the published UI without a local bundle.

## Repository tasks and tools

`cargo xtask --help` lists repository tasks. The `justfile` calls those commands.
`cargo xtask submodule-pins [main-ref]` refuses undeclared or backward submodule pins.
It compares `HEAD` with the selected reference, which defaults to `main`.
A pointer change must name the submodule and say `bump` or `pin` in its commit subject.
A backward change must also say `revert`.
Initialize the submodules and fetch their history before this check.
`cargo xtask netsuite-live` runs the two [NetSuite operator workflows](../../apps/netsuite/README.md#owner-run-live-calls).
[mise.toml](../../mise.toml) pins kind and other repository tools with download checksums.
Run `mise install` to install them, then activate mise or use `mise exec -- <command>`.
The pins use Linux x86_64 artifacts.
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
`wamn_codegen::kcl::binary` selects that override first, then the pinned installation, then `kcl` on `PATH`.
The compiler runs KCL and never installs tools.
The environment JSON Schema lives in `crates/env/env.schema.json`.
Run `cargo xtask env-schema` after editing the Rust types.
Run `cargo xtask env-schema --check` to check the committed schema.
The catalog owns the authored model in `crates/catalog/src/manifest.rs`. The code generator checks document rules once and derives inherited model fields once. Catalog readers decode compiled facts without repeating document checks.

The schema module that `wamn.k` imports as `manifest` is [`crates/codegen/kcl/manifest`](../../crates/codegen/kcl/manifest/manifest.k).

- Plain English names in the schema module; use `type` for variant names; no plurals that the manifest does not already use.

An overlay's `wamn.k` imports nothing from its base; `base_dependencies` stays a declaration (package, version, digest, operations) as today, because the pin is the contract and a language-level import would let the overlay drift from the sealed base. The language adds comments and defaults to the overlay; it does not compose the two packages.

## wamn build

`wamn build` compiles each authored `wamn.k` once and uses committed evidence in `.wamn/describe/`.
The evidence contains SQLx statement descriptions, catalog facts, and ordered history columns.
An ordinary build needs no database. It refuses missing or stale evidence.

If SQL or the verification schema changes, refresh the evidence with PostgreSQL 18.
Set `DATABASE_URL` or pass `--database-url` with a superuser URL.
The build creates and drops a verification database on the server.
The server keeps the platform roles that the build creates.

```bash
wamn build --refresh-describe apps/wamn_receiving apps/client_acme_receiving
wamn build --check-describe apps/wamn_receiving apps/client_acme_receiving
```

Commit the refreshed `.wamn/describe/` files and any new `history/` migrations.
`--check-describe` runs verification again and compares the evidence byte for byte.
Both flags are also available on `cargo xtask build-package`.

Verification runs base migrations first, then the package migrations.
The compiler reads authored WIT. `wamn-codegen` reads statement descriptions from PostgreSQL.
The compiler derives relations and feed sources, then generates the remaining SQL once.
SQLx describes every generated statement, including each fixed list sort and each feed statement.
The compiler checks each accessor's parameters and selected columns against those descriptions before it writes consumers.
One type table in `wamn-scalar` maps PostgreSQL names for build and runtime.

The compiler writes history changes as SQLx migrations under `history/` in the package source and build output.
Keep these files in source control. Each file remains unchanged after release.
Authored `migrations/` and generated `history/` share one version sequence.
Choose each new authored version above every existing version in both directories.
The build refuses duplicate versions, including collisions between the two directories.
The build compares new versions with the committed migration sources and refuses an insertion below their highest version.
Each generated history migration records the authored versions it follows.
An authored version change also appends this durable checkpoint.
The build refuses new versions below that checkpoint, including in an uncommitted source tree.
History files without an input checkpoint are refused.
Publication compares the sequence with the migrations under the current digest of that tag.
Build output does not hold the migration baseline.
A history declaration change appends a generated migration, even when authored SQL stays unchanged.
Refresh runs the complete migration sequence before it writes the updated evidence.

A custom statement in `wamn.k` lists its fetch class and ordered bind names.
The operation and statement names set its SQL path.
The build reads a named bind's type and nullability from the WIT input.
A bind computed by the handler takes its type from SQLx and must be non-null.
SQLx describes the result columns.
If SQLx cannot tell whether a result column is nullable, add a SQL alias that ends in `!` for non-null or `?` for nullable.
For example, use `AS "total!"` for a count.
Use a SQL cast to set the type of a result expression.

A consuming workspace package lists application clients in its `package.json` field `"wamn": { "clients": ["wamn_wms"] }`.
`wamn build` writes each named client as plain source under that consumer's `src/generated/<app>/`, without a `package.json`.
If the build cannot produce a requested client, it fails and names the consuming package and application.

The build writes each package to `<output root>/<package id>/`.
Package builds write to the workspace output root, `apps/target/wamn` for applications.
There is no custom-output option: guest includes and artifact readers use this same path.
The output keeps the logical `generated/` paths without that prefix.
The output also includes copies of the authored KCL, SQL, and migrations used by this build.
Build the packages before compiling the apps workspace. Its sources include files from this output.

A `<package>.fingerprint` file sits beside each package output directory.
The build writes it after generation and guest compilation finish.
A matching signature and all expected output files let the next build reuse the output.
Artifact readers use the finished output even if the authored sources change later.
An overlay reads its base manifest and component bytes from the base output.
The build calls the composer library directly.

`wamn web build` builds a standalone web artifact against the finished package contract.
It permits web source changes and checks the compiled manifest digest against the fingerprint.
See [web clients](web-upload.md#build-and-push) for its output, metadata, and registry commands.

### Ejecting defaults

`wamn eject action apps/wamn_receiving purchase_order.create` replaces the empty action implementation with an authored module.
The module contains the current default body.
Its WIT, SQL and input validation stay generated.
The command refuses an existing implementation or output file.

## Pushing a package

Build the package and any bases before publication.
Set `WAMN_REGISTRY` to the org registry origin and `WAMN_REGISTRY_AUTH_FILE` to the registry credential file.
Run `wamn publish <build directory>` to publish that build output.
Publication writes packages under `<org-origin>/wamn/packages` and components under `<org-origin>/wamn/components`.
The command prints the top package OCI digest. An OCI digest is the SHA-256 hash of an image manifest.
Publication checks the built bytes before pushing.
A new tag starts a migration history. A replacement keeps every existing migration name and byte, including `history/`.
New migrations must use higher numbers. Repeating the exact digest leaves the tag alone.
SQLx checks migration checksums during apply.
If a tag points to the older artifact format, publish under a new tag.


## Reading a pushed package

Layer 0 of a package artifact is the canonical `wamn.json` of the package.
`wamn inspect <digest>` prints layer 0 of the pushed artifact, byte for byte.
`wamn diff <digest> <digest>` prints each JSON pointer where layer 0 of the two artifacts differs.
Each argument is the full package artifact digest that source publication prints, `sha256:` and 64 lowercase hex digits, or the tag `<package_id>-<version>`.
A digest prefix is refused.
A line that starts with `-` shows the value in the first artifact, and a line that starts with `+` shows the value in the second.
Identical documents print nothing.
Both commands inspect the registry directly. The project database records the digest of each installed package.
They test each layer against its digest.
They accept these registry flags: `--artifact-base`, `--registry-auth-file`, `--insecure-registry` and `--oci-ca-path`.

```bash
"$CARGO_TARGET_DIR/debug/wamn" inspect <digest> \
  --artifact-base <registry>/wamn/packages \
  --registry-auth-file <auth file>
```

A component uses wasmCloud's stock OCI format. Its tag is the 64 hexadecimal digits of its byte digest.
The image has one `application/wasm` layer. Publication records the image manifest digest in the package's `deploy` section.

A package artifact has the tag `<package_id>-<version>` and artifact type `application/vnd.wamn.package.v3`.
The image manifest uses `application/vnd.oci.image.manifest.v1+json`.
Its config uses `application/vnd.oci.empty.v1+json` with the bytes `{}`.
The two layers appear in this order. Each title is an `org.opencontainers.image.title` annotation.

| Layer | Media type | Title | Content |
|---|---|---|---|
| 0 | `application/vnd.wamn.package.manifest.v1+json` | `wamn.json` | Canonical package document with compiled routes, admitted component facts, image digests and base package OCI digests |
| 1 | `application/vnd.wamn.package.v1.tar` | `migrations` | `migrations/` and `history/` |

The tar uses sorted package-relative paths, GNU headers, mode 0644, owner and group IDs 0, and modification time 0.
A web source or output edit leaves the package digest unchanged. [Web artifacts](web-upload.md#build-and-push)

The OCI manifest records `wamn.package.id` and `wamn.package.version`.
Dev manifests also carry `org.opencontainers.image.created`. A retry keeps the original timestamp and digest.
Unpacking places the package document at `generated/wamn.json` and extracts the SQL files at the package root.
Unpacked dev artifacts keep the timestamp in `artifact-created-at` so that an offline repack reproduces the published digest.


## The edge box binary

The edge box is an aarch64 Linux computer, for example a Raspberry Pi 3B.
Build its release binary with the Dockerfile target `edge`:

```bash
docker build --build-arg WAMN_CHECKOUT_KEY="$(printf '%s' "$(realpath .)" | sha256sum | cut -c1-16)" --target edge --output type=local,dest="$CARGO_TARGET_DIR/edge" .
```

The result is one stripped file, `$CARGO_TARGET_DIR/edge/wamn-edge`.
The target compiles with the Debian trixie cross compiler.
The cluster image `edge-web` is a different build.
The Dockerfile stage `build-edge-web` builds the separate `wamn-web` service for linux/amd64.

## Isolated worktrees

Keep one Cargo process per target directory.

Each checkout keeps its own Docker target cache and its own host target directory.
The native images `host`, `ctl`, and `identity` use one `build-native` stage.
One `cargo build` compiles the native packages with their shared features.
The `edge-web` image has its own stage, `build-edge-web`. It shares the target cache of `build-native`.
The same native build produces `wamn-control` for the control image.
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

To bump the platform version, change `version` and `appVersion` in `deploy/charts/app/Chart.yaml` to the same new value.
No other file holds the version.
A version change runs the same BuildKit build with new output tags. BuildKit reuses unchanged layers.

These changes make Cargo rebuild more:

- A changed `Cargo.lock`, Cargo feature, profile or `.cargo/config.toml` rebuilds the dependencies that they reach.
- A new Rust image under the `rust:1.100-trixie` tag rebuilds everything.
- A change to a widely used workspace crate rebuilds every crate that depends on it, and links each native binary again.
- A new checkout has a new key, so its first image build compiles everything.
- BuildKit garbage collection can remove a cache. Read the caches with `docker buildx du --verbose`.

## Guest artifact comparisons

Use two clean worktrees at the same commit to inspect checkout independence.
Give each worktree its own Cargo target directory.
Build the same guest separately in each worktree through the shared component builder.
Compare the artifact digests with `sha256sum`.
To inspect the Cargo home mapping, repeat the build with a second `CARGO_HOME` path.

## JSONata workflow schemas

Put `input-schema` and `output-schema` in each JSONata node's `params`.
Use the source port's exact schema for the input and the target port's exact schema for the output.
Include their item and required-field rules.
