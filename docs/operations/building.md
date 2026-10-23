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
eval "$(tools/lane-start "$PWD")"
```

The script initializes submodules, sets the checkout caches, builds the native tools, installs the web dependencies, and runs `wamn build` for every package.
Docker supplies a private PostgreSQL 18 container that the script removes when it exits.

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
The release publisher also needs the pinned `http-route` guest.
The ctl image carries it at `/usr/local/share/wamn/http_route.wasm`.
For native publication, set `WAMN_FLOW_HTTP_COMPONENT` to the built guest and `WAMN_COMPONENT_ARTIFACT_BASE` to the component repository.
Publication pushes the composed entry to `<component-artifact-base>/entries`.
The manifest records only the path `entries@sha256:<manifest-digest>`, relative to the component artifact base.

The workload guests `http-route` and `materializer` belong to no package. Release qualification and the cluster cases build each one with the same guest build, through `wamn_schema_generator::build::build_guest`.

The Docker `roles` target exports the control HTTP entry as `http_route.wasm`.
It composes the pinned route guest with an empty dispatcher at platform build time.
The publisher image retains the raw route guest for application composition.
To build the control entry locally from that raw guest, run:

```bash
cargo run --offline -p wamn-component-composer -- \
  --control-route "$CARGO_TARGET_DIR/wasm32-wasip2/release/http_route.wasm" \
  --output "$CARGO_TARGET_DIR/control_http.wasm"
```

Local development composes the same control entry before it starts the host.
It names the entry with an absolute `file:` reference to its digest-named artifact.

The build virtualizes each component that the package owns and each crate that [`tools/component-virtualization.json`](../../tools/component-virtualization.json) lists under `packages`.
It writes the virtualized guest to `virtualized/std-empty-environment` in the Cargo target directory.
The list is an allowlist, not discovery: a std guest that it does not name stays raw and fails its own admission.

The build then composes each component of a package that pins bases in `base_dependencies`.
`wamn-component-composer` joins the overlay component and each base whose operations it imports into one component.
It reads each base component from the base's `build.json`, so build the base first.
The overlay imports each base operation that it calls under the base's export name, and the base supplies that operation.
An overlay that imports no base operation stays as it is.
Each member that imports `wamn:postgres/statements` gets its own outer import, `sql-0`, `sql-1`, in member order ([components](../architecture/components.md)).
The composed component exports every export of its members.
Imports that no member supplies stay imports of the composed component.
Each member is embedded with its bytes unchanged.
The composed component replaces the overlay's virtualized output.
The composer does not compare the base bytes with the digest pin in `wamn.json`. Publication checks the pin.

Last, the build copies each component that the package owns and each palette component that its wirings name to `components/<name>.wasm` in the package's build output. It records each one in `outputs.components` of the package's `build.json`, with its path and `sha256` digest.
`wamn-ctl push-package` and the dev loop read `build.json` for the bytes of each component.
`push-package` refuses a component without an entry, and a file whose digest differs from its entry.
For the `build.json` that `push-package` needs, see [Pushing a package](#pushing-a-package).
The dev loop watches the package roots, `apps/platform`, and the shared platform WIT sources, so an edit of any of them rebuilds the components.

WIT bindings read canonical package directories through ordered `path` lists. Put dependencies before the consumer world.
The router owns `wamn:node`. The runtime owns PostgreSQL, connection, JetStream, flow routing, and blobstore contracts.
The execution host owns router delivery. The materializer owns the shared WASI CLI and clock packages.
Application contracts lie under `wit/` in each package's build output, `apps/target/wamn/<package>/wit`, which `wamn build` writes. Do not copy platform WIT into application directories.

## Platform UI

`wamn-ctl build-platform` builds `apps/platform_ui/dist` with `pnpm --filter @wamn/platform-ui build` before it builds and pushes the platform images.
The command writes one line to stderr when each stage starts and ends.
The stages cover the UI build, the input hashes and their lookups, each image build, roles export, each push or copy, each role comparison on the cloud path, and the chart stamp.
Each line includes a UTC timestamp in RFC 3339 format. End lines also include elapsed seconds:

```text
build-platform <stage> start <RFC 3339 UTC>
build-platform <stage> end <RFC 3339 UTC> elapsed <seconds>s
```

`--stamp-only` prints these lines around registry reads and the chart stamp.
Step 4.6 forwards these lines as they arrive and times its package builds, UI build, and stamp-only command.
An end line marks the stage exit, including failure. The command exit status reports success or failure.

The platform UI is a platform asset and has no `wamn.k` or package artifact.
Install the workspace dependencies with `pnpm install --offline` before the build.

The stamped chart contains `platform-build.json`.
Its `PlatformBuildReceipt.platform_ui` field records a SHA-256 digest of a canonical JSON map from relative file paths to file digests.
The map covers `index.html` and every asset in the dist directory.
The chart version identifies the platform asset version.

`build-platform --stamp-only` hashes the local dist without building or publishing it.
If the dist is absent, it refuses and names `pnpm --filter @wamn/platform-ui build`.
It creates no OCI artifact for the static bundle.
Keep the local dist with the stamped chart until [upload](deployment.md#platform-ui-files).

## Unchanged platform images

`build-platform` builds only the platform images whose inputs changed since an earlier push.
The five images are `host`, `runner`, `gates`, `identity`, and `edge-web`.
The command always builds the `roles` target, which holds the `http` and `materializer` role components.
It then compares each role by its wasm layer. See [Unchanged role components](#unchanged-role-components).

After the UI build, the command calculates an input hash for each image.
The input hash is a SHA-256 digest of the target name, the Docker build arguments, and a list of input files.
The list records the path, the SHA-256 digest, and the mode of each file, or the target of a symbolic link.

These files are inputs of every image:

- `Dockerfile` and `.dockerignore`.
- `Cargo.toml`, `Cargo.lock`, `.cargo/config.toml`, `apps/Cargo.toml`, and `apps/Cargo.lock`.
- The `Cargo.toml` of every local package in the root and `apps` workspaces. One Cargo command builds many packages, and the features of all of them apply to each one.

Each image also hashes the local packages that its binaries compile.
The command reads them from `cargo metadata --all-features`, with normal and build dependencies.
For each package, it hashes the whole package directory.
It also hashes each path outside that directory that a source file names in a file-reading macro.
These macros are `include_str!`, `include_bytes!`, `include!`, `#[path]`, `bindgen!`, and `generate!`.
If a package has a build script or a procedural macro, the image hashes every file that the Dockerfile copies.

| Image | Packages | Other inputs |
| --- | --- | --- |
| `host` | `wamn-host`, `wamn-runtime` | none |
| `identity` | `wamn-identity` | none |
| `edge-web` | `wamn-edge` | none |
| `gates` | `wamn-host`, `wamn-runtime`, `wamn-gates`, `wamn-ctl`, `wamn-cdc-reader`, and the five guests of the component builder | `tools/guest-rustflags` |
| `runner` | `wamn-ctl`, `wamn-scenario-worker`, the five guests, and `wamn-wms-tests` and `wamn-receiving-tests` with their dev dependencies | `apps`, `deploy`, `tools`, and the `apps/target/wamn` context |

The five guests are `http-route`, `materializer`, `busyloop`, `connection-http-standard`, and `sockprobe`.
The command ignores a file that no `COPY` from the build context reaches, and a file that `.dockerignore` drops.
The `wamn.dev/source-head` label is not an input.

The command prints one line for each image, with the hash and the action:

```text
build-platform <image> input-<hash> build
build-platform <image> input-<hash> copy
```

Each built image gets a second tag, `input-<hash>`, and the label `wamn.dev/input-hash=<hash>`.
Before it builds, the command looks for the `input-<hash>` tag of each image in the destination registry.
The five lookups run at the same time.
If the tag is not found, the command builds the image as before.
If the lookup fails for another reason, such as a network timeout, the command tries it once more.
If the second lookup also fails, the command prints `build-platform <image> input-<hash> lookup failed: <error>` and builds the image.
If the tag exists, the command does not build the image.
It runs `docker buildx imagetools create --prefer-index=false` instead.
The chart version tag then points at the same manifest, with the same digest.
The copies run after every build succeeds, so a failed build still pushes nothing.

`wamn-ctl` reads the chart version at run time from the chart it uses: `build-platform` from the checkout's `deploy/platform/release/Chart.yaml`, `env apply` and the cases from the stamped chart.
No binary embeds `Chart.yaml`, and the `runner` image drops `deploy/platform/release` in its `runner-deploy` stage.
So a bump that changes no other input copies every image.

A copied image keeps the base image packages from its first build.
To build every image again, change the `Dockerfile` or delete the `input-<hash>` tags.

### Unchanged role components

A role component has one layer, the wasm file. The layer digest is the SHA-256 digest of the wasm bytes.
The manifest digest also covers a creation time, so the command never compares manifest digests.

For each role, the command finds the previous chart version in the registry.
That version is the highest `x.y.z` tag of `<registry>/wamn/<role>` below the chart version.
The command reads the manifest of that tag and compares its layer digest with the digest of the new wasm.
If they are equal, it stores that manifest byte for byte under the chart version tag.
The chart then pins the role by the previous manifest digest, so a qualification of that digest still applies.
If they differ, or the lookup fails, the command pushes the role as before.

The command prints one line for each compared role:

```text
build-platform <role> layer <digest> copy <previous version>
build-platform <role> layer <digest> differs from <previous version>
```

The manifest copy reads the registry credential from the Docker configuration or its credential helper.

## Cloud platform builds

Before a Docker build starts, `build-platform` reads the process list.
If another `wamn-ctl build-platform` process builds images, the command uses Google Cloud Build.
It ignores its own process and processes with `--stamp-only`.
When no other build runs, it uses the local Docker build path.
With `--cloud`, it uses Google Cloud Build even when no other build runs, under the same daily cap.
A `--cloud` build runs no Docker build on this machine, so other builds do not count it as busy.
If a local build would wait on another build, use the cloud path, so that no build waits on the machine.

The cloud path requires `--cluster <cluster.k>`.
It reads the project, `buckets.state` and `build_cloud_cap_usd` from that document.
The cap defaults to USD 3 per UTC day.
All existing flags keep their meaning, including the destination `--registry`.

```bash
wamn-ctl build-platform --cluster deploy/gcp/cluster.k --registry us-central1-docker.pkg.dev/wamn-dev/wamn --out /var/tmp/wamn-release-chart
```

The command submits the same Dockerfile targets with `gcloud builds submit` in `us-central1`, on `E2_HIGHCPU_8`.
It stages the source in `gs://<buckets.state>/build/source/`, so Cloud Build needs no staging bucket of its own.
Each image build runs `docker buildx build` in `docker:28.5.1-cli` on a `docker-container` builder, because the worker daemon's BuildKit has no named build contexts.
Cloud Build writes the build log to Cloud Logging only.
Read a failed build with `gcloud logging read 'resource.type="build" AND resource.labels.build_id="<id>"' --project wamn-dev`.
If gcloud fails, the command prints the last 20 lines of its stderr.
Each image build pushes its image with `--push` when it finishes, with a fresh registry token. After the role export, the two role components are pushed under the same registry tag.
The cloud path uses the same input hashes. Cloud Build builds only the images that have no `input-<hash>` tag, and pushes each of them under both tags.
Cloud Build always builds the roles and pushes each one once under the chart version tag.
After Cloud Build succeeds, the command copies the other images from this machine.
Then it compares the layer of each pushed role with the previous chart version.
If the layers are equal, it copies the previous manifest under the chart version tag.
The command waits for Cloud Build, then reads the pushed digests and stamps the chart.
The local UI build and its recorded digest follow the local path.
The `cloud-submit` stage prints start and end lines, including elapsed time.

The root `.gcloudignore` excludes the `.dockerignore` paths and every target directory.
The command copies only `apps/target/wamn` into a temporary source directory for the runner input, then removes that directory after submission.
It does not upload Cargo target caches.

Before submission, the command reads `gs://<buckets.state>/build/<YYYY-MM-DD>.json` for the UTC submission day.
For wamn-dev, `buckets.state` is the OpenTofu state bucket `wamn-dev-tofu-state`.
An absent object means zero spend.
Before each counter read or write, the command runs `gcloud auth print-access-token` with the agent `CLOUDSDK_CONFIG` and uses that token.
It needs no application default credentials and no key file.
At or above the cap, it refuses the cloud build and prints the spend and cap.
The local path does not use the cloud cap.

After a successful or failed cloud build, the command calculates elapsed minutes from its start and finish times, rounded up.
Each minute adds USD 0.0156, the [e2-highcpu-8 default-pool rate for US regions](https://cloud.google.com/build/pricing), read on 2026-10-20.
The counter records `cloud_minutes` and `spend_usd` against the submission day, including builds that finish after midnight.
A generation precondition requires the object version read by the writer.
If another writer changes it, the command reads the new value and retries the addition.

The cap controls submissions, and it does not cancel a running build or reserve spend for one.
Builds that run at the same time can take the total above the cap.
The counter covers build minutes, not storage or network charges.
The [owner inputs](gcp.md#1-owner-inputs) supply access and the separate monthly budget.

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
Generation removes obsolete output files and keeps SQLx metadata prepared after generation.
Authored publication references use the current source manifest, even when an earlier build output exists.
Build every package before you compile the apps workspace, the app test crates, or an app web: their sources include files from the build output.
`build.json` is the build receipt, schema `wamn.build/v1`, in canonical JSON.
It records the input files and their digest, the base packages, the generator identity, and the digest of each output.
The input files are the package's own files, `apps/Cargo.lock`, and the platform trees that its components link: `crates/platform/runtime/wit`, `apps/platform/data`, `apps/platform/execution/contract` and `apps/platform/execution/jsonata`.
A change in a platform tree makes every package stale.
If the input digest and base packages match the build record and its outputs exist, `wamn build` prints that the package is current.
It leaves the output files and `build.json` unchanged.
An overlay reads the `build.json` of each base package, so build the base first.
For a package that has a `web/` directory, the build then runs `pnpm run build` there with Vite's `outDir` set to `web/` in the package's build output, `apps/target/wamn/<package>/web` (platform-deploy.md §7.1 step 8).
Run `pnpm install` at the repository root first.
`build.json` records the digest of the web files as `outputs.web`, and `null` for a package without a web client.
Each app's `vite.config.ts` names the same directory, so `pnpm run build` by hand writes there too.
The build compiles the package's components as [above](#native-programs-and-app-components) describes.

### Package digest records

Registry tags are immutable, so a package that changes under an unchanged version cannot be pushed to the GKE registry again. Each package that a file in `deploy/gcp/environments` names has a committed `package.digest` beside its `wamn.k`. The file holds one line, the tag and the digest that the built package packs to. The test `cargo test -p wamn-control --test package_digest` builds no package, so run `wamn build` first. It refuses when the digest changed and the tag did not, and when the tag changed and the file did not.

When you change such a package, bump its version in `wamn.k`. Then write the file again and commit it with the change:

```bash
wamn build --record-digest apps/<package>
```

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
It writes them to `apps/target/wamn/wamn_control/client-ts` and `apps/target/wamn/wamn_control/control/client-ts`, below the output root of `wamn build`.

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
docker build --build-arg WAMN_CHECKOUT_KEY="$(printf '%s' "$(realpath .)" | sha256sum | cut -c1-16)" --target edge --output type=local,dest=target/edge .
```

The result is one stripped file, `target/edge/wamn-edge`.
The target compiles with the Debian trixie cross compiler, so the box needs glibc 2.38 or later.
[The edge specification](../history/edge.md) section 4.9 records the measured size and dependency counts.

## Isolated worktrees

Keep one Cargo process per target directory.

Each checkout keeps its own Docker target cache and its own host target directory.
The six native images use one `build-native` stage, which compiles every native package in one `cargo build`, so features unify once.
A second invocation in the same stage builds only `wamn-ctl-ops` with the `ops` feature, because that feature reaches shared code and the shipped `wamn-ctl` builds without it.
The key uses the first 16 hexadecimal characters of the SHA-256 hash of the canonical absolute checkout path, with no trailing newline.
[`tools/lane-start`](../../tools/lane-start) sets `CARGO_TARGET_DIR=$HOME/.cache/wamn-target/<key>` and exports the key as `WAMN_CHECKOUT_KEY`.
Pass that key as Docker's `WAMN_CHECKOUT_KEY` build argument.
A new checkout starts with a cold host build.

Reuse that worktree's own target, and let Cargo decide which artifacts need rebuilding.
Keep large build targets off temporary memory filesystems.
[Running tests](running-tests.md#capture-a-run) explains result reporting and optional output directories.

## Host image cache

The `host` image takes its binaries from the `build-native` stage.
That stage compiles every native image, so the first image build of a platform build carries all of the native compile time.
`build-native` and `build-edge-web` mount the target cache `wamn-root-target-native-<checkout key>`.
They share the Cargo registry and Git caches with every checkout.
The cache mounts are not in the image. The stage copies each binary out of the target cache to `/native-output`, and the image copies it from there.

To bump the platform version, change `version` and `appVersion` in `deploy/platform/release/Chart.yaml` to the same new value.
No other file holds the version.
A bump alone gives every image the same input hash, so `build-platform` builds no image and copies each one to the new version tag.

These changes make Cargo rebuild more:

- A changed `Cargo.lock`, Cargo feature, profile or `.cargo/config.toml` rebuilds the dependencies that they reach.
- A new Rust image under the `rust:1.99-trixie` tag rebuilds everything.
- A change to a widely used workspace crate rebuilds every crate that depends on it, and links each native binary again.
- A new checkout has a new key, so its first image build compiles everything.
- BuildKit garbage collection can remove a cache. Read the caches with `docker buildx du --verbose`.

Before wamn-gc3sl.1, every checkout used the one target cache `wamn-root-target-native`.
An image build from another checkout at another commit then compiled its own workspace crates into the cache.
The next bump compiled them again, and it waited while another build held the cache lock.

Stage times from the `build-platform` stage lines.
The 0.1.98 build on 2026-10-23 was the first build under its checkout key, so its target cache was empty.
The next bump in the same checkout gives the times of a bump with a full cache.

| Stage | 0.1.98, empty cache | Next bump |
|---|---|---|
| `input-lookups` | 306.3 s | not yet measured |
| `image-build-host` | 3107.3 s | not yet measured |
| `image-build-runner` | 147.9 s | not yet measured |
| `image-build-gates` | 3.8 s | not yet measured |
| `image-build-identity` | 1.2 s | not yet measured |
| `image-build-edge-web` | 549.9 s | not yet measured |
| Total | 4223 s | not yet measured |

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
