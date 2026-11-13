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

The script initializes submodules and selects the Cargo cache directory.
It runs no builds, web installation, or database.
Build the artifacts required by the selected command.
For tests, [the selection file](../../tests/selection.toml) binds preparation to its consuming targets and suites.
The script prints two exports.
`CARGO_TARGET_DIR` is `$HOME/.cache/wamn-target/<key>`, and `WAMN_CHECKOUT_KEY` is the key.
Cargo writes its output to `CARGO_TARGET_DIR`, so commands in this repository use `$CARGO_TARGET_DIR` and not `target/` or `apps/target`.
`wamn build` still writes package output to `apps/target/wamn`.

Platform installations use the full schema files under `deploy/sql/`.
These files own each schema.

## Platform version procedure

Every merged PR that changes a file that `wamn-ctl build-platform` reads bumps the platform patch version once, not once per commit (owner ruling 2026-10-20): `version` and `appVersion` of `deploy/platform/release/Chart.yaml`, its one owner (owner ruling 2026-10-14, `wamn-i69qo`, R15). `wamn-ctl` reads it at run time from the chart it uses, so no binary or image input changes with it (owner ruling 2026-10-23, `wamn-6qo7v`). A stale version is a findings-first failure.

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
- each palette component that a wiring of the package names. The build finds it by the `component` of its `declaration.json.in` under `apps/platform/*/*/`, and builds the crate in that directory.

A package outside a Cargo workspace, with no `../Cargo.toml`, has no component to compile.
Cargo writes the guests to `CARGO_TARGET_DIR`. If that variable is unset, it writes them to `apps/target`.

[`tools/guest-rustflags`](../../tools/guest-rustflags) sets the RUSTFLAGS of every guest build.
It maps the repository to `/wamn` and the Cargo home to `/cargo`, so no guest carries a local path.
It also maps the toolchain's `rust-src` tree to `/rustc/<commit-hash>`, so a toolchain with and without `rust-src` gives one digest.
`wamn build` and the Dockerfile `component-builder` stage call it.
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

The workload guests `http-route` and `materializer` belong to no package.

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
`wamn build` compares each base component's bytes with the digest pin in `wamn.json` before compilation and before reporting a package as current.
Publication repeats this comparison.

Last, the build copies each component that the package owns and each palette component that its wirings name to `components/<name>.wasm` in the package's build output. It records each one in `outputs.components` of the package's `build.json`, with its path and `sha256` digest.
`wamn-ctl push-package` and the dev loop read `build.json` for the bytes of each component.
`push-package` refuses a component without an entry, and a file whose digest differs from its entry.
For the `build.json` that `push-package` needs, see [Pushing a package](#pushing-a-package).
The dev loop watches the package roots, `apps/platform`, and the shared platform WIT sources, so an edit of any of them rebuilds the components.

WIT bindings read canonical package directories through ordered `path` lists. Put dependencies before the consumer world.
The router owns `wamn:node`. The runtime owns PostgreSQL, connection, JetStream, flow routing, and blobstore contracts.
The execution host owns router delivery. The materializer owns the shared WASI CLI and clock packages.
Application contracts lie under `wit/` in each package's build output, `apps/target/wamn/<package>/wit`, which `wamn build` writes. Do not copy platform WIT into application directories.

## Local platform artifacts

Artifact production runs separately from platform deployment and needs the source checkout.
The producer builds `host`, `ctl`, and `identity` images plus the `http` and `materializer` role components.
It needs no application package output or platform UI bundle.
Deployment consumes these prepared artifacts and does not run a build.

From the repository root, start or reuse the shared registry:

```bash
tools/local-registry
```

The helper uses `wamn-local-registry` with its persistent volume and publishes port 5001 on loopback.
Make sure that its reported port matches `deploy/kind/cluster.k`, which selects `localhost:5001`.
Keep the registry for later builds and deployments.
Then produce the artifacts and a prepared chart at a path that does not exist:

```bash
wamn-ctl build-platform --registry localhost:5001 --out /tmp/wamn-platform-chart
```

The command reuses unchanged published images when their inputs match.
If the registry contains every selected artifact for the chart version, stamp a new chart:

```bash
wamn-ctl build-platform --stamp-only --registry localhost:5001 --out /tmp/wamn-platform-chart-restamped
```

Both commands require the source checkout for the chart and deployment resources.
Stamping refuses a missing selected artifact.
The output includes pinned image references, the operator archive, the identity chart, and authored deployment resources under `platform/`.
Copy the whole output directory when transferring it to another deployment host.
[Local deployment](deployment.md#local-platform) uses this directory through an explicit `--chart` path and needs no source checkout.

The command writes stage start and end lines to stderr, with UTC timestamps and elapsed seconds.
`platform-build.json` records elapsed milliseconds in `stage_durations_ms`.
An end line marks stage exit, including failure. The command exit status reports success or failure.

## Platform UI

The local platform build does not select the UI.
An explicit `--cluster` document with a nonempty `buckets.web` selects it.
For that separate path, install workspace dependencies with `pnpm install --offline` before the build.
The producer then runs `pnpm --filter @wamn/platform-ui build`.
The platform UI has no `wamn.k` or application package artifact.

The producer publishes one deterministic OCI artifact under `<registry>/wamn/platform-ui:<platform version>`.
The artifact contains only `dist/` and canonical `web.json` with the platform version.
It uses the web archive format and the artifact type `application/vnd.wamn.platform-ui.v1`.
It contains no organization configuration or application package.
The prepared chart pins its manifest digest in `values.yaml` as `platformUi`.
An empty value means that the build selects no UI.
`platform-build.json` records only stage durations.

With UI selection, `--stamp-only` resolves the published artifact tag from the registry.
It reads no local UI output and publishes nothing.
Deployment and [upload](deployment.md#platform-ui-files) use the published artifact.
They need no source checkout, pnpm installation, or local UI bundle.

## Unchanged platform images

`build-platform` builds only the platform images whose inputs changed since an earlier push.
The local platform images are `host`, `ctl`, and `identity`.
An explicit GCP cluster document selects the retained `host`, `ctl`, `identity`, and `edge-web` set.
The command always builds the `roles` target, which holds the `http` and `materializer` role components.
It then compares each role by its wasm layer. See [Unchanged role components](#unchanged-role-components).

The command calculates an input hash for each selected image.
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
If a package has a procedural macro, the image hashes every file that the Dockerfile copies.
If a package has a build script, the image hashes the paths that the script names in `cargo:rerun-if-changed` lines.
A build script without such lines gets the same treatment as a procedural macro.

| Image | Packages | Other inputs |
| --- | --- | --- |
| `host` | `wamn-host`, `wamn-runtime`, `wamn-cdc-reader` | none |
| `ctl` | `wamn-ctl` and the two guests | `tools/guest-rustflags` |
| `identity` | `wamn-identity` | none |
| `edge-web` | `wamn-edge` | none |

The two guests are `http-route` and `materializer`.
The command ignores a file that no `COPY` from the build context reaches, and a file that `.dockerignore` drops.
The `wamn.dev/source-head` label is not an input.

The command prints one line for each image, with the hash and the action:

```text
build-platform <image> input-<hash> build
build-platform <image> input-<hash> copy
```

Each built image gets a second tag, `input-<hash>`, and the label `wamn.dev/input-hash=<hash>`.
Before it builds, the command looks for the `input-<hash>` tag of each image in the destination registry.
The lookups for selected images run at the same time.
If the tag is not found, the command builds the image as before.
If the lookup fails for another reason, such as a network timeout, the command tries it once more.
If the second lookup also fails, the command prints `build-platform <image> input-<hash> lookup failed: <error>` and builds the image.
If the tag exists, the command does not build the image.
It runs `docker buildx imagetools create --prefer-index=false` instead.
The chart version tag then points at the same manifest, with the same digest.
The copies run after every build succeeds, so a failed build still pushes nothing.

`wamn-ctl` reads the chart version at run time from the chart it uses: `build-platform` from the checkout's `deploy/platform/release/Chart.yaml`, `env apply` and the cases from the stamped chart.
No binary embeds `Chart.yaml`.
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
The chart then pins the role by the previous manifest digest.
If they differ, or the lookup fails, the command pushes the role as before.

The command prints one line for each compared role:

```text
build-platform <role> layer <digest> copy <previous version>
build-platform <role> layer <digest> differs from <previous version>
```

The manifest copy uses no credential for a loopback registry.
For other registries, it reads the credential from the Docker configuration or its credential helper.

## Local revision builds

Local revision builds store their images in a registry on the build host.
The build needs no GCP access or cluster document.
Input-hash lookups and role copies read this same registry.
The platform journey uses the local platform artifact set.
The build still pulls public base images unless Docker already holds them.

Prepare the checkout with `tools/lane-start` as described above.
Then run these commands from the repository root:

```bash
export TMPDIR=/var/tmp
export DOCKER_CONFIG=$(mktemp -d /var/tmp/wamn-test-revision-docker.XXXXXX)
printf '{}\n' > "$DOCKER_CONFIG/config.json"
registry=$(tools/local-registry)
revision_out=$(mktemp -d /var/tmp/wamn-test-revision.XXXXXX)
cargo build --locked --offline -p wamn-ctl --bin wamn-ctl
"$CARGO_TARGET_DIR/debug/wamn-ctl" build-platform \
  --registry "$registry" --out "$revision_out/release-chart"
export WAMN_RELEASE_CHART="$revision_out/release-chart"
```

The empty Docker configuration prevents Docker and wash from calling credential helpers.
`tools/local-registry` starts or reuses only its labeled `wamn-local-registry` container.
It prints `127.0.0.1:<port>` after the registry responds over HTTP.
The container uses `registry:2` and keeps images in the `wamn-local-registry-data` volume.
Platform deployment connects a reused registry to the kind network when needed.
Keep it between revision runs so that unchanged images remain available.

A loopback build stays local even when another builder runs.
Do not pass `--cloud` for a loopback registry.
Cloud Build cannot reach it, so the command refuses that combination.
Run the [platform kind case](running-tests.md#platform-and-environment-apply-on-kind) with the exported chart and Docker configuration.
Remove the temporary Docker configuration after the case finishes.
A push to Artifact Registry is a separate artifact-production operation that requires an explicit owner request.

## Cloud platform builds

For a remote registry, `build-platform` reads the process list before a Docker build starts.
If another `wamn-ctl build-platform` process builds images, the command uses Google Cloud Build.
It ignores its own process and processes with `--stamp-only`.
When no other build runs, it uses the local Docker build path.
With `--cloud`, it uses Google Cloud Build even when no other build runs, under the same daily cap.
A `--cloud` build runs no Docker build on this machine, so other builds do not count it as busy.
For authorized artifact production in a remote registry, use `--cloud` to avoid waiting for another local build.
GCP deployment integration remains unfinished.

The cloud path requires `--cluster <cluster.k>`.
It reads the project, `buckets.state` and `build_cloud_cap_usd` from that document.
The cap defaults to USD 3 per UTC day.
All existing flags keep their meaning, including the destination `--registry`.

```bash
# Only for explicitly requested cloud artifact production.
wamn-ctl build-platform --cloud --cluster deploy/gcp/cluster.k --registry us-central1-docker.pkg.dev/wamn-dev/wamn --out /var/tmp/wamn-release-chart
```

The command submits the same Dockerfile targets with `gcloud builds submit` in `us-central1`, on `E2_HIGHCPU_8`.
It stages the source in `gs://<buckets.state>/build/source/`, so Cloud Build needs no staging bucket of its own.
Each image build runs `docker buildx build` in `docker:28.5.1-cli` on a `docker-container` builder.
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
If the cluster document selects UI storage, the UI build and recorded digest follow the local path.
The `cloud-submit` stage prints start and end lines, including elapsed time.

The root `.gcloudignore` excludes the `.dockerignore` paths and every target directory.
Cloud and local platform builds use the same artifact set.
Application build output and Cargo target caches stay outside the cloud upload.

Before submission, the command reads `gs://<buckets.state>/build/<YYYY-MM-DD>.json` for the UTC submission day.
For wamn-dev, `buckets.state` is the OpenTofu state bucket `wamn-dev-tofu-state`.
An absent object means zero spend.
Before each counter read or write, the command runs `gcloud auth print-access-token` with the agent `CLOUDSDK_CONFIG` and uses that token.
It needs no application default credentials and no key file.
At or above the cap, it refuses the cloud build and prints the spend and cap.
The local path does not use the cloud cap.

After a successful or failed cloud build, the command calculates elapsed minutes from its start and finish times, rounded up.
Each minute adds USD 0.0156, the [e2-highcpu-8 default-pool rate for US regions](https://cloud.google.com/build/pricing).
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

- Plain English names in the schema module; `type` never `kind`; no plurals that the manifest does not already use.

An overlay's `wamn.k` imports nothing from its base; `base_dependencies` stays a declaration (package, version, digest, operations) as today, because the pin is the contract and a language-level import would let the overlay drift from the sealed base. The language adds comments and defaults to the overlay; it does not compose the two packages.

## wamn build

`wamn build` builds packages in two passes.
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

A consuming workspace package lists application clients in its `package.json` field `"wamn": { "clients": ["wamn_wms"] }`.
`wamn build` writes each named client as plain source under that consumer's `src/generated/<app>/`, without a `package.json`.
If the build cannot produce a requested client, it fails and names the consuming package and application.

The build writes each package to `<output root>/<package id>/`.
The default output root is `apps/target/wamn` for an application, and `--output-root` changes it.
Below it, the layout keeps the logical `generated/` paths that contracts and releases name, without the `generated/` prefix, plus `build.json`.
Git holds no build output, and the repository policy lint refuses a tracked `generated/` or `.sqlx` file under `apps/`.
Generation removes obsolete output files and keeps SQLx metadata prepared after generation.
Authored publication references use the current source manifest, even when an earlier build output exists.
Build every package before you compile the apps workspace, the app test crates, or an app web: their sources include files from the build output.
`build.json` is the build record, schema `wamn.build/v1`, in canonical JSON.
It records input files, both input digests, base packages, generator identity, and output digests.
A digest identifies content through its SHA-256 hash.
`inputs.digest` covers all build inputs.
`inputs.generation_digest` excludes only the package-local `component/`, `data/`, and `web/` trees.
Both digests cover the manifest, authored SQL, migrations, generator identity, and every other build input.
The development loop reads the physical SQL fingerprint from `physical-schema.json` after preparing generation.
It uses the same schema planner and installed SQL records as deployment.
The build record stores no installed schema identity.
A query edit reruns Generate and keeps Migrate and existing introspection results.
The input files include the package's own files and `apps/Cargo.lock`.
They also include `apps/platform/data`, `apps/platform/execution/contract`, and `apps/platform/execution/jsonata`.
The build derives shared WIT inputs from `wit_bindgen::generate!` paths across the guest workspace, including their dependencies.
It excludes generated WIT under `target` or `generated` because build records track those outputs.
A change in a shared WIT input or platform tree makes every package stale.
The input digest also covers the generator source, KCL templates, embedded SQL, Cargo manifest, and build script through `wamn_schema_generator::build_record::GENERATOR_FINGERPRINT`.
Both input digests cover these generator inputs, shared WIT, platform trees, and the lock file.
A change to these inputs requires generation and compilation again without a version bump.
If the input digest and base packages match the build record and its outputs exist, `wamn build` prints that the package is current.
It leaves the output files and `build.json` unchanged.
An overlay reads the `build.json` of each base package, so build the base first.
Package readers of `apps/target/wamn` call `wamn_schema_generator::build_record::read_current` first, as `push-package` does.
If the package has no `build.json`, or its `inputs.digest` differs from the package tree, the reader refuses and tells you to run `wamn build`.
If only the full digest changes, the build reuses generated output and rebuilds the package components.
Build records the new full digest, so readers accept the rebuilt output.
A reader still refuses changed source until Build refreshes the build record.
After generation and component compilation, the build copies generated clients to their consumers.
`wamn build` does not run Vite or record browser files in `build.json`.
The build compiles the package's components as [above](#native-programs-and-app-components) describes.

`wamn web build` builds a standalone web artifact against the recorded package contract.
It permits web source changes and requires the compiled manifest to match `outputs.manifest`.
See [web clients](web-upload.md#build-and-push) for its output, metadata, and registry commands.

### Package digest records

Removing the web layer changes existing package digests.
After rebuilding the packages, refresh their records with `wamn build --record-digest` before the package digest test.
No version or format bump accompanies this greenfield change.

Registry tags are immutable, so a package that changes under an unchanged version cannot be pushed to the GKE registry again.

When you change such a package, bump its version in `wamn.k`. Then write the file again and commit it with the change:

```bash
wamn build --record-digest apps/<package>
```

## Pushing a package

`wamn-ctl push-package --package <package root>` pushes one package artifact.
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
Both commands pull from the registry only, by full digest or tag, because no database holds a package row.
They check each layer against its digest.
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
| 1 | `application/vnd.wamn.package.v1.tar` | `migrations` | `migrations/` |
| 2 | `application/vnd.wamn.package.v1.tar` | `publication` | `publication/`: `attachments.json`, `wirings/`, `components/<name>.json.in`, including the declaration of each palette component a wiring names |
| 3 | `application/vnd.wamn.package.v1.tar` | `sources` | `wamn.k`, `command/`, `query/`, `generated/platform-policy/data-access.json`, `generated/contracts/`, `generated/required-schema-contract.json`, `generated/sql/`, `generated/publication/`, `generated/routes/` |
| 4..n | `application/vnd.wamn.component.descriptor.v1+json` | none | one descriptor per component, owned or palette, sorted by name, with the annotations `wamn.component.name` and `wamn.component.digest` |

The package artifact contains three tar layers and no browser files.
A web source or output edit leaves the packed package digest unchanged.
`wamn web build` and `wamn web push` own the separate [web artifact](web-upload.md#build-and-push).

The `generated/` paths of layer 3 are logical paths. `wamn build` writes those files below `apps/target/wamn/<package>/` without the `generated/` prefix, and pack reads them from there.

A tar has GNU headers, sorted package-relative paths, mode 0644, uid and gid 0 and mtime 0. A descriptor is the canonical JSON of `ComponentDescriptor` (`crates/catalog/model/src/component_facts.rs`, `deny_unknown_fields`): `{component, interface-version, component-digest, operations, imports, imports-fingerprint, effects, connections: [{store-alias, requirement-type}]}`, which is `AdmittedComponent` without its scope plus its connections. The reader supplies the scope. A palette component's store alias comes from the `params.store_alias` of the wiring nodes that name it, and two aliases refuse. The manifest annotations are `wamn.package.id`, `wamn.package.version` and one `wamn.component.<name>` = `sha256:...` per component. Nothing carries a timestamp. The manifest is canonical JSON.

The artifact digest is the SHA-256 of the image manifest bytes. The registry records it, and it is what `artifact_digest` means. An unpacked artifact is not build output. The three tars keep their logical paths. Layer 0 goes to `generated/wamn.json`. Each descriptor goes to `descriptors/<name>.json`. It packs again offline to the pushed digest. `package_artifact_digest(root)` is that offline digest of a tree as it is. A source tree has no `descriptors/`, so its digest differs from the pushed digest. Only an unpacked tree packs to it.

## Host route clients

The TypeScript clients of the host routes, `@wamn/control-client` and `@wamn/control-org-client`, are build output, and Git does not hold them.
The generator makes them from the contracts in `crates/catalog/model/src/host_route`.
It writes them to `apps/target/wamn/wamn_control/client-ts` and `apps/target/wamn/wamn_control/control/client-ts`, below the output root of `wamn build`.
It then copies them into each workspace package that names them in `package.json` `"wamn": { "clients": [...] }`, as `wamn build` does for an application client.
The name `wamn_control` selects the application route client, and `wamn_control_org` selects the control route client.
A consuming package imports the copy from `src/generated/<name>`, which Git ignores.

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
docker build --build-arg WAMN_CHECKOUT_KEY="$(printf '%s' "$(realpath .)" | sha256sum | cut -c1-16)" --target edge --output type=local,dest="$CARGO_TARGET_DIR/edge" .
```

The result is one stripped file, `$CARGO_TARGET_DIR/edge/wamn-edge`.
The target compiles with the Debian trixie cross compiler.
The cluster image `edge-web` is a different build.
The Dockerfile stage `build-edge-web` builds `wamn-edge` with the `web` feature for linux/amd64.

## Isolated worktrees

Keep one Cargo process per target directory.

Each checkout keeps its own Docker target cache and its own host target directory.
The native images `host`, `ctl`, and `identity` use one `build-native` stage.
The host image also carries `wamn-cdc-reader`.
It compiles every native package in one `cargo build`, so features unify once.
The `edge-web` image has its own stage, `build-edge-web`. It shares the target cache of `build-native`.
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
That stage compiles every native package except `wamn-edge`, so the first image build of a platform build carries that compile time.
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
