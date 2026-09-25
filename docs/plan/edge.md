# Edge

Epic 19 builds `wamn-edge`, a service that runs one application on a small box beside a device. Beads epic `wamn-e5in` holds the issues and their status. The owner reviewed this scope on 2026-09-24 and accepted each pick.

## 1. Goal

`wamn-edge` is one service crate. It links the engine, run-state with a SQLite adapter, and an edge plugin set. It runs the operations of one application behind local routes, and one device loop.

The first target reads a serial scale, stores each sample in SQLite, and forwards it to the platform. The box is a Raspberry Pi 3B class computer: four Cortex-A53 cores, 1 GiB of memory, an SD card, and aarch64 Linux.

## 2. Fixed rules

The owner set these rules in the epic brief.

- The edge links no `wamn-runtime`, `wamn-workflow`, Postgres, NATS, OCI, or OTLP gRPC. A dependency test asserts it.
- Wasm isolation stays. Device logic runs as components under the engine.
- The edge application is a release that the platform published. The edge loads it from a local file path. It uses the same contract, the same generated component, and the same `invoke_operation`.
- Authorization happens at publish, and the edge trusts the loaded release. Session verification uses a local key file.
- Run-state uses SQLite with a single writer. Bypass follows the operation kind: get, query, and list bypass, and create, update, delete, and command log. The intent log promises that no call runs again after a power loss.
- Store and forward: a sample persists before any forward, and a failed forward retries from the log. A forward is outbound HTTPS to a platform route. NATS comes only if a later cloud-to-edge path needs it.
- Device plugins are `HostPlugin` implementations in the edge crate. Serial comes first. MQTT, Modbus, OPC UA, and raw TCP are named here and not built.
- The epic edits nothing under `web/`, and Node is not part of the build. A diagnostics UI is out of scope and is the next candidate.
- The router stays out (routes-router rule 7). Workflows on the edge are a later question.

## 3. Current state

Measured on main at `4e63fb9a2` on 2026-09-24.

| Place | Today |
| --- | --- |
| `wamn-engine` (`crates/platform/engine`) | Exports `invoke_operation`, the engine builder, admission, `LoadedRelease`, and the local file `ArtifactSource`. `cargo tree -e normal` lists 326 crate names for x86_64 and 325 for aarch64. |
| wash-runtime 2.10.1 | Links `async-nats`, `redis`, `tonic`, `opentelemetry-otlp`, `hyper-util`, and `hyper-rustls` with every feature off. The engine dependency test pins this list (finding `wamn-qt1t`). The owner ruled that no upstream issue or PR is made, and that a fork or a different host crate is decided when the list costs the edge something. |
| `invoke_operation` | Takes `Option<&dyn IntentStore>` and ignores it. Every caller passes `None`. |
| `wamn-run-state` | Defines `IntentStore` (`begin`, `finish`, `uncertain`, `resolve`). No implementation exists. The Postgres one is epic `wamn-an24`, which is open and not scoped. |
| Route layer | Four parts. The `http-route` ingress guest (`apps/platform/ingress/http-route`, about 1,270 lines) matches paths and lowers outcomes to HTTP. It uses no `wamn-runtime` code. The `FlowHttpRouting` plugin in `wamn-runtime` (about 1,340 lines) holds the route table, authentication, and the grant read. `wamn-execution-host` holds delivery, the grant check, and settlement, and it depends on `wamn-runtime`. |
| Grants | A session's operation permissions come from Postgres (`session_operation_permissions`). The active-session check reads the identity database. |
| Session verification | `SessionVerifier` is a struct in `wamn-runtime` over `IssuerKeys`, which fetches the issuer keys over HTTPS. No key-source trait exists. The pure check `verify_session_token` is in `wamn-platform-identity`, which links `tokio-postgres` unconditionally. |
| Generated components | Every generated application component imports `wamn:postgres/types` and `wamn:postgres/statements`, the platform fixture included. The only components without SQL are platform node components, such as `label-render` and `http-request`. |
| Release identity | `LoadedRelease::load_from(dir)` derives the manifest digest from the canonical bytes. `LocalComponentSource` reads `<dir>/<sha256>.wasm` and checks its digest. A release has no signature: its identity is its digest. |
| Engine memory | The pooling allocator defaults to 512 core instances and a 256 MiB memory cap. `build_engine_with_host_memory` takes other budgets. |
| TLS | rustls uses aws-lc-rs, and `aws-lc-sys` is C code. `wamn-platform-identity` uses ring for Ed25519. |
| Toolchain | Rust 1.98.1 with the `wasm32-wasip2` target. The repository has no aarch64 target and no aarch64 linker. |

## 4. Decisions

Each decision lists the options and the ruled pick. Where the owner added to a pick, the ruling follows it.

### 4.1 The wash-runtime list

The brief refuses NATS and OTLP gRPC. The engine brings both through wash-runtime.

| Option | Rule | Cost |
| --- | --- | --- |
| A (ruled) | The edge test uses the engine rule. It refuses every listed crate outside wash-runtime, and it pins the accepted wash-runtime list exactly. | NATS and OTLP gRPC code sits in the binary, unused. Issue 2 measures its size on aarch64. |
| B | Fork wash-runtime and put the six crates behind features. | A fork to keep. The repository removed its last wasmCloud fork. |
| C | A different host crate, with Wasmtime directly and no wash-runtime. | The edge cannot use the engine, `NativeApplication`, or the `HostPlugin` trait. It becomes a second engine. |

Option A keeps one engine and turns the cost into a measured number. The owner ruled that issue 2 records the aarch64 size cost. If the cost is too high for the Pi, the answer is a fork of wash-runtime with those features off, decided when the number is known. No upstream issue is made.

### 4.2 What the edge application stores

Every generated component imports `wamn:postgres`. The edge has no Postgres.

| Option | Rule | Cost |
| --- | --- | --- |
| A (ruled) | An edge application declares no SQL. The generator emits no `wamn:postgres` import for an application with no statements. The device operation returns the sample as its outcome, and the edge host writes it to the sample store. | A generator change. The edge application has no tables of its own. |
| B | The edge serves `wamn:postgres` over SQLite and runs the published statements. | Two SQL dialects. Row security, types, and functions such as `gen_random_uuid()` differ. Publish needs a second checker. |
| C | The edge links the import to a stub that refuses every call. | A component links, and any SQL call fails at run time. |

Option A keeps one meaning for the import: `wamn:postgres` is Postgres. The sample store belongs to the edge host, as the intent log does.

The owner ruled that the generator emits imports from what the contract uses, not from a fixed set. The change is correct for the cloud too. A model-free fixture application tests it.

The first device shape (owner ruling, `wamn-e5in.8`): the host owns the serial port and gives each frame to the device operation as its input, with no new WIT. A device that the host must drive (write, poll, or handshake) needs a host plugin with a WIT import. A read-only serial device does not. One frame is one call, one intent and one sample. The scale is set to send on print or on stable weight, because a continuous stream writes the SD card on every frame.

### 4.3 Local routes

| Option | Rule | Cost |
| --- | --- | --- |
| A (ruled) | The edge serves the same `http-route` ingress guest. The engine implements its two imports, `wamn:flow-http-routing/routing` and `wamn:router-delivery/delivery`. The edge implements the engine's two host traits over the loaded release and the local grants. | The edge writes an authenticator and a delivery, without Postgres and JetStream. |
| B | A native Rust route layer in the edge. | A second path matcher and a second outcome lowering. Two readers of one fact. |

Option A keeps one path matcher and one lowering, by the uniformity rule. Issue `wamn-e5in.5` moved the host side of both imports into `wamn-engine`, because only the crate that owns the generated bindings can implement them for wash-runtime's context. The engine now holds:

- the bindings of both imports, in one world;
- the route plugin `FlowHttpRouting`, with the route limiter, the input schema validators, the route table, and the header and CSRF helpers;
- the delivery plugin `RouterDelivery`, with source and target resolution, the derived causation, route settlement, and the refusal literals;
- the caller type `AuthenticatedCaller`, the grant check `authorize_registered_operation`, and the expected-host router.

A host plugs in through two traits. `RouteAuthenticator` reads the credential of a protected route. `RouteDelivery` serves one delivery request. The cloud implements them in `wamn-runtime` and `wamn-execution-host`, and the edge implements the same two traits.

Issue `wamn-e5in.6` built the edge side, and it moved two more checks into the engine so that both hosts read them once:

- `route_credential` selects the one credential that a request presents: a session cookie, a bearer session, or a PAT by its prefix. `PAT_TOKEN_PREFIX` moved into `wamn-session` so that the engine can read it.
- `authorize_released_operation` checks the entry's own grant, then every permission that publish folded into it.

On the edge, `EdgeAuthenticator` verifies a session over the key file and maps its roles through `grants.json`, and a PAT route answers 503, as an empty cloud authenticator does. `EdgeDelivery` calls `invoke_operation` for a route target and passes it the intent context of the route (section 4.7). A wiring or registration target fails as `execution-failed`. A read carries no ETag, because the box has no model versions. `EdgePolicy` admits an application that imports `wamn:node/types` only. `serve` starts a plain wash-runtime host with no NATS, and it runs the route guest from the bundle bytes.

### 4.4 Session verification and grants

The owner ruled in Epic 12 that the edge verifier is a second implementation of the same trait, not a move. No trait exists, and the pure check sits in a crate that links Postgres.

- Ruled: a new pure crate `wamn-session` holds `verify_session_token`, `PublicSessionKey`, a `KeySource` trait, and `SessionVerifier` over that trait. It links no `tokio-postgres` and no HTTPS client. The cloud and the edge both link it. `IssuerKeys` stays in `wamn-runtime`, fetches the issuer's keys over HTTPS, and implements the trait. `FileKeys` in `wamn-session` reads the edge key file. `wamn-platform-identity` uses the new crate. Built by `wamn-e5in.3`.
- Other option: a feature in `wamn-platform-identity` that turns off Postgres. Epic 12 chose crate splits over features.

A session grants operations through permissions that the platform reads from Postgres. The edge has no tenant database.

- Ruled: `grants.json` in the release bundle maps each role to its permissions, so authorization at publish holds on the box. The edge reads it once at start and refuses a permission that no operation in the release requires. Section 4.6 binds it to the release. Built by `wamn-e5in.4`.
- Other option: edge tokens carry their permissions as claims. That changes the token profile.

The edge trusts the roles of a verified token until the token expires, which is 930 seconds at most. The box keeps no session state and reads no user roles, so a revoked login keeps working on the box until its token expires. For an operator who must cut a login sooner, a revoked-list file can ship with the bundle. It is named here and not built.

Local routes admit sessions only. A PAT needs the identity database. The device loop calls its operation as a fixed local principal that the edge configuration names. That caller has the credential kind `QueuedService` and the permissions of the configured role in `grants.json`, so a `fresh_only` operation refuses it.

The edge configuration names the key file, the issuer, the organization and the audience (the project-environment identity) that a session must carry.

### 4.5 Crate layout

| Option | Rule | Cost |
| --- | --- | --- |
| A (ruled) | One crate `services/edge`, package `wamn-edge`, a library and a binary. Modules: configuration, release loading, routes, session keys, the SQLite store, samples and forward, and `plugins/serial`. | One crate grows. It has one user. |
| B | The SQLite `IntentStore` as its own crate under `crates/execution`. | A second crate for one user. |

The owner later placed the SQLite adapter in its own crate, `wamn-run-state-sqlite` (`wamn-e5in.2`), beside `wamn-run-state`. The dependency test is `services/edge/tests/dependency_boundary.rs`, in the engine test's form. The serial source is the module `serial`, not a plugin, because the host owns the port (section 4.2).

The configuration is one TOML file, which `wamn-edge --config <path>` or `WAMN_EDGE_CONFIG` names. It has the sections `release`, `session`, `store`, `http`, and an optional `device` with `device.serial`. Each key has one `WAMN_EDGE_*` variable that replaces it, and an unknown key is refused. [`edge.example.toml`](../../services/edge/edge.example.toml) shows every key.

### 4.6 How a release reaches the box

| Option | Rule | Cost |
| --- | --- | --- |
| A (ruled) | The operator copies a release bundle directory, and the edge configuration names the digest of its `edge-release.json`. | A manual copy. Publish writes the bundle, and the box never assembles one. |
| B | The edge pulls the release by digest from the platform over HTTPS. | A new platform endpoint, and the box needs a network path to the platform at start. |
| C | A signed bundle. | New signing machinery. The platform signs no release today. |

Option A has the same trust as a cloud pod. There, the pod template names the manifest digest and the loader checks the bytes against it. Here, the edge configuration names the bundle digest.

The platform signs no release, so one digest binds every file (owner ruling, `wamn-e5in.4`). The bundle directory holds:

- `edge-release.json`: the format and the SHA-256 of the next four files.
- The canonical serving manifest.
- `components.json`: the admitted component facts of the release. The manifest does not carry them, and the cloud host reads them from its catalog.
- `grants.json`: the permissions of each role.
- `flow-http.wasm`: the http-route ingress guest (owner ruling, `wamn-e5in.5`). The route guest is part of the release that the box runs.
- `<sha256>.wasm` for each component.

`EdgeRelease::load` refuses a file whose bytes do not match its digest, a component fact that the release does not carry, and a manifest component with no fact. It runs `validate_component_in_release`, which moved into `wamn-engine` so that the cloud host and the edge share one check. A later release signature signs the bundle digest.

### 4.7 SQLite schema

| Option | Rule | Cost |
| --- | --- | --- |
| A (ruled) | One file `edge.db` in WAL mode with `synchronous=FULL`. One task owns the only writing connection. | One file grows for both tables. |
| B | Two files, one for intents and one for samples. | SQLite in WAL mode does not commit across two files atomically. |

Option A lets one transaction record the intent outcome and insert the sample. The tables:

- `intents`: `id`, `tenant`, `release`, `package`, `operation`, `idempotency_key`, `input_hash`, `deadline_ms`, `begun_at`, `finished_at`, `outcome`, `resolved_basis`. The pair `(tenant, idempotency_key)` is unique.
- `samples`: `id`, `sample_key` (unique, the idempotency key of the forward), `intent_id`, `captured_at`, `body`, `forwarded_at`, `attempts`, `last_error`.

The samples, as issue `wamn-e5in.8` built them in [`samples.rs`](../../services/edge/src/samples.rs):

- The device loop gives each frame a new UUID v4 as its `sample_key`. It sends one item, `{"request_id": sample_key, "value": {"frame", "captured_at"}}`, so the sample key is also the intent key.
- A completed item stores its `value` as `body`, in the transaction that finishes its intent. A failed item stores no sample. `SqliteIntentStore::transact` and `finish_in` let the edge write its table in that transaction.
- The device route must change records and must name no idempotency key, and the edge refuses to start otherwise.
- The serial reader drops a trailing carriage return. It drops an empty frame, a frame that is not UTF-8, and a frame longer than `max_frame`. It counts each dropped frame for the diagnostics, because drops show a misconfigured scale, and logs the first drop only (owner ruling).
- Issue `wamn-e5in.9` added `refused_at`, `resolved_basis` and `resolved_at` to the sample row (owner ruling: the sample store owns its row, and 4.7 still holds for the intent log). A sample is pending until it is forwarded or refused. A refused sample keeps the platform's reason in `last_error`. An operator lists refused samples with `wamn-edge samples list` and closes one with `wamn-edge samples resolve <sample_key> <basis>` while the edge is stopped. A resolved sample is never forwarded.

`begin` commits before the export runs. After a power loss, a begun intent with no outcome is uncertain, and the edge never runs it again. The reading of that intent is lost, and the next reading replaces it. The promise holds only if the SD card honors a flush. The closeout records the card that the test used.

The logging decision by operation kind belongs in `wamn-engine`, beside `invoke_operation`, so that the edge and `wamn-an24` share it. Epic `wamn-an24` then adds only the Postgres adapter.

The intent rules, as issue `wamn-e5in.7` built them in the [engine](../../crates/platform/engine/src/operation/intent.rs):

- A create, update, delete, or command logs. A get, query, projection, or event handler never logs.
- Each input item is one intent. Its key is the item field that the route names in its `idempotency` member, or else the item's `request_id`. Publish writes that member from the generated input contract, as it writes `reads` and `revision`.
- The input hash is the canonical JSON SHA-256 of the item without its `request_id`. A key that repeats with another input answers the item error `idempotency_conflict`.
- Only new items run. A finished key answers its stored item outcome, and the answers merge into one item list in the order of the input.
- A begun key with no outcome answers the item error `intent-uncertain`, which names the intent. A trap, a missed deadline, or a host failure leaves the new items of a call in that state.
- An operator resolves an uncertain intent with `wamn-edge intents resolve <id> <basis>` while the edge is stopped, and lists them with `wamn-edge intents list`. A resolved key answers `intent-resolved` with the basis and is never uncertain again, so the client sends a new key.
- An input that is not an item list with a `request_id` and a key on each item fails as `invalid_input`, and nothing runs.

### 4.8 Forward credential

| Option | Rule | Cost |
| --- | --- | --- |
| A (ruled) | A device PAT over HTTPS. The platform mints a PAT for a device principal, and the box keeps it in a file with mode 0600. The forward sends it as a bearer token. | Revocation happens on the platform. The token lives on the SD card. |
| B | A TLS client certificate. | Platform ingress has no client certificate check. |
| C | A token signed by an edge key. | The platform trusts a new key per box. |

The forward calls a command route with `request_id` set to `sample_key`, so the platform's idempotency makes a repeated forward harmless. The client is `hyper-util` with `hyper-rustls`, which wash-runtime links already, so the forward adds no crate.

The forward, as issue `wamn-e5in.9` built it in [`forward.rs`](../../services/edge/src/forward.rs):

- The configuration section `forward` names the https `url`, the `token_file`, an optional `ca_file` of extra PEM roots, and the `key_field`. The edge refuses to start if anyone but the owner of the token file can read it. The edge reads the PAT once at start, so a rotated PAT needs a restart (owner ruling).
- The platform keys a command by its idempotency key field, not by `request_id`. So the forward also writes the sample key into `key_field`, for example `value.idempotency_key` (owner ruling). The device operation stays a transform of one frame.
- Each attempt reads the pending samples from the store, one sample per request. A retry never comes from memory, and a restart continues where the store says.
- A refusal is an outcome (owner ruling). An item error or a 4xx status stores the sample as refused, and the forward never sends it again.
- No answer, a timeout, or a 5xx, 408 or 429 status leaves the sample pending, and the forward waits a backoff from 5 seconds to 15 minutes.
- A 401 or 403 status is a credential failure (owner ruling). The sample stays pending behind the same backoff. `Forward::credential_failures` counts each failure, and the edge logs the first. After `credential_bound` failures in a row (default 5), the forward stops sending and logs an error. A restart with a valid PAT starts it again. An expired PAT is an operator problem, and an endless retry hides it.
- The forward wakes when a sample is stored. A forward in flight when the edge stops is dropped, and the platform key makes its repeat harmless.

### 4.9 Size and dependency budget

| Option | Rule | Cost |
| --- | --- | --- |
| A (ruled) | Starting ceilings, measured in issue 2: a stripped binary of 64 MiB or less, 256 MiB resident or less with the fixture release loaded, 60 seconds or less for the first component compile, and 5 seconds or less for a restart with a warm compile cache. | The numbers can move after the first measurement. |
| B | Set no number until the first measurement. | Nothing refuses growth. |

The edge sets its own memory budgets: 8 core instances and the 256 MiB memory cap. Components compile on the box, and Wasmtime's disk cache keeps the result. Build components ahead of time for aarch64 only if the first compile misses its ceiling. The owner has no Pi 3B. Issue 2 measures on an aarch64 emulator or builder, and the closeout records the target as not measured on hardware. New crates are few: `rusqlite` with its bundled SQLite. The serial port uses `rustix` termios, which the tree links already, so the edge adds no serial crate (owner ruling, `wamn-e5in.8`). The box artifact is a release build, because a debug Wasmtime is too slow for the box. Tests use debug builds.

The closeout (`wamn-e5in.10`) measured the aarch64 release build:

- The stripped binary is 40,365,304 bytes (38.5 MiB), under the 64 MiB ceiling. Cargo built it in 14 minutes 47 seconds in `docker build --target edge` on the workstation.
- The binary links libc, libm and libgcc_s, and it needs glibc 2.38 or later. So the box runs a Debian trixie system, such as Raspberry Pi OS trixie. Raspberry Pi OS bookworm has glibc 2.36.
- Under qemu user emulation in a trixie container, the binary ran `samples list` and `intents list`, and it served the fixture release and stopped on SIGINT. This shows that it links and loads, and nothing about speed on the Pi.
- The edge does not yet set the memory budget of the paragraph above. It runs with the cloud host's defaults: 512 core instances and a 128 GiB pool reservation, and no compilation cache (finding `wamn-wk8e`).
- Resident memory, the first compile, and the restart time are not measured. An emulator gives no numbers about the Pi (owner ruling). Measure them on hardware.

The dependency counts are unique crate names from `cargo tree -p <crate> -e normal`, with the crate itself. This method gives the Epic 12 numbers again at their commit.

| Crate, commit or target | Pruned at wash-runtime | With wash-runtime |
| --- | --- | --- |
| `wamn-engine` at Epic 12 issue 3 (`33a7ea78d`), the recorded numbers | 128 | 299 |
| `wamn-engine` at the Epic 12 close (`688103564`) | 183 | 317 |
| `wamn-engine` at the Epic 19 closeout | 227 | 330, and 329 for aarch64 |
| `wamn-edge`, x86_64 host | 253 | 338 |
| `wamn-edge`, aarch64 | 253 | 337 |

- The edge adds 26 crate names to the engine when pruned, but only 8 with wash-runtime, because wash-runtime already brings `hyper-rustls`, `hyper-util`, `rustls-native-certs`, `tracing-subscriber` and `time`. The 8 are `rusqlite`, `libsqlite3-sys`, `hashlink`, `fallible-iterator`, `fallible-streaming-iterator`, `tracing-log`, `wamn-run-state-sqlite` and `wamn-edge`.
- The engine gained 44 crate names after the Epic 12 close and lost none. They include the Wasmtime WASI and WASI HTTP crates, `hyper`, `h2`, `rustls` with `aws-lc-rs` and `ring`, and `wamn-session`.
- On aarch64, 84 crate names reach the edge only through wash-runtime, among them `async-nats`, `redis`, `tonic` and `opentelemetry-otlp` (finding `wamn-qt1t`). The binary is under its ceiling with them, so the edge needs no wash-runtime fork now (epic ruling 1).

### 4.10 Cross-compile and test

| Option | Rule | Cost |
| --- | --- | --- |
| A (ruled) | A Dockerfile target `edge` builds `aarch64-unknown-linux-gnu` with the Debian trixie cross compiler, the release of the `toolchain` image (owner ruling, `wamn-e5in.10`). | A new image stage. The C code in `aws-lc-sys` and `libsqlite3-sys` needs that compiler. |
| B | `cargo-zigbuild` on the workstation. | A tool outside the repository, and a result that depends on the workstation. |
| C | Build on the Pi. | 1 GiB of memory does not compile Wasmtime. |

Tests run on the x86_64 host, because the edge logic does not depend on the target. The dependency test reads `cargo tree` for both targets. A test opens a pseudo-terminal pair as the virtual serial port and writes scale frames into it, so the test needs no `socat`. The power-loss test runs the edge binary as a child process, kills it with SIGKILL between the store and the forward, and starts it again. It then asserts one forward per sample and no second export call. No Pi 3B is available, so no test waits for one.

## 5. Issues

The owner scopes each issue after the one before it closes and is reviewed, and can change this order. Beads holds the filed issues and their status.

1. The `wamn-edge` crate with its dependency test, and nothing else (`wamn-e5in.1`).
2. The aarch64 build: the Dockerfile target and the first size, memory, and crate count.
3. The shared logging decision by operation kind, and the SQLite `IntentStore`.
4. A fixture release loads from a file and serves one route: release loading, the grants file, `wamn-session`, and the routing and delivery host side.
5. The generator emits no `wamn:postgres` import for an application with no SQL, and an edge fixture application.
6. The serial plugin, the device loop, and the sample store.
7. The forward with a device PAT, the retry from the log, and the power-loss test against a local stack.
8. The closeout, the sweep log, and the section 7 line in `routes-router.md`.

## 6. Done when

- `wamn-edge` builds for aarch64, and the dependency test passes.
- On a host with a virtual serial port, a sample is read, logged, stored, and forwarded to a local stack. It survives a kill between the store and the forward.
- A fixture release loads from a file and serves one route.
- The closeout, the sweep log, and a section 7 line in `routes-router.md` exist.
