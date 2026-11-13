# 58 edge (`services/edge`)

Scope: `services/edge/src` (5,413 lines; tests 1,543). Nothing built or run.

## Earlier work checked first

- Fact (at 553511bbd): the crate holds two programs that share only `main.rs`; the dependency rule is checked by `tools/repo-lint-dependencies`.
- Tab 31: finding 1 ("the edge has its own delivery; confirm in the edge tab"; `wamn:router-delivery` stays only if the edge needs it); check 1 A (no per-route shedding; `RouteLimiter` deleted); check 5 (the edge composes the same route guest with its own authenticator; confirm here). Tab 31 finding 3: the route table is its own component; the host keeps capabilities (identity, operation authority).
- Tab 38 finding 2 C: the host converts JSON to typed values and calls the operation's typed `run`.
- U2 (owner, 2026-11-11): SQLx replaces `rusqlite` on the edge.
- Tab 54 finding 4 B: the hand-written Kubernetes client stays, on `hyper`; `kube` later.
- Earlier proposal, not ruled: `web.rs::Releases` as the one owner of current routes. R-11 route reuse age: ruled in tab 88.

## Files read

- `main.rs`, `lib.rs` header, `Cargo.toml`; `delivery.rs` (head, impls); `authenticator.rs` (head, impls); `serve.rs` (engine budget, routing setup); `web.rs` (config, release root, Kubernetes reads); item lists of `release.rs`, `operation.rs`, `policy.rs`.
- Users of `wamn_engine::router_delivery` (grep: engine, host, workflow, runtime, edge, services/host).

## One purpose per file

- Edge device program: `serve.rs` (start), `release.rs` (load a pinned bundle), `config.rs`, `authenticator.rs`, `grants.rs`, `policy.rs`, `delivery.rs`, `operation.rs`, `application.rs`, `device.rs`, `serial.rs`, `samples.rs`, `forward.rs`, `status.rs`, `intents.rs`, `refusals.rs`.
- Cluster web edge: `web.rs` (930 lines, feature `web`): routes application host names from ConfigMaps to their APIs and serves web client files from the web bucket. A second program.

## Findings

**1. Two programs in one crate.**

Fact: `main.rs` runs the edge device program, or with feature `web` the cluster web edge (`web`, `web-check`). They share nothing else; the web edge links `object_store`, the edge device program must not.

Options:

- A: the cluster web edge becomes its own service crate; the edge crate keeps only the edge device program. The feature flag and its `main.rs` branches go.

**2. The edge device delivers routes on the old path.**

Fact: `EdgeDelivery` implements `wamn_engine::router_delivery::RouteDelivery`; a route calls `invoke_operation` with the engine's lowering. The cloud moves to: route table component, then the host invoke through `wamn:operation`, then typed `run` (tabs 25 B1, 31 finding 3, 38 finding 2 C).

Options:

- A: the edge device runs the same path as the cloud: the same route component, the same host invoke, typed `run`. `EdgeAuthenticator` is the edge device implementation of the identity capability of tab 31 finding 3 (settles tab 31 check 5). `wamn:router-delivery` is deleted once no caller remains (confirms tab 31 finding 1 for the edge).

**3. A device-wide in-flight counter.**

Fact: `serve.rs` builds `FlowHttpRouting::with_box_limit(RouteInFlightLimit(4))`, a `RouteLimiter` with device scope; the same number sizes the instance pool. Tab 31 check 1 A deleted `RouteLimiter` for the cloud and relies on the pool bound. Fact from the wash-runtime read (tab 24): when the pool is full, a call runs in a one-shot store; it neither waits nor sheds. Inference: on the edge device that bound does not cap memory.

Options:

- A: same rule as the cloud: delete the device limiter; the pool bound is the limit (memory not capped past it).
- B: keep one device-wide limit that sheds with 429 past the pool size, as the device's only limiter; delete the per-route scope.

**4. A third hand-written Kubernetes client.**

Fact: `web.rs` reads the service account and lists ConfigMaps over its own HTTP code. `wamn-credential-store::kubernetes_api` and `control/environment` (`kubectl`) are the others.

Options:

- A: `web.rs` uses the one hand-written client of tab 54 finding 4 B. Its callers for the later `kube` question become four.

**5. Hand-parsed command line.**

Fact: `main.rs` reads `--config` and the subcommands by hand; `services/identity` uses `clap`.

Options:

- A: `clap` derive, as in the other services.

**6. Kubernetes code is spread over five crates (owner question 2026-11-12).**

Fact (grep of product code):

- Kubernetes API over HTTP, hand-written: `wamn-credential-store::kubernetes_api` and `kubernetes` (Secrets); `execution/host/src/environment_status.rs` with `wamn_control_provision::environment_state::observe_environment` (Deployments, Pods, `WorkloadDeployment`); `services/edge/src/web.rs` (ConfigMaps).
- `kubectl` subprocess: `control/environment/src/kubectl.rs` (233 lines), called from about 70 places in `control/environment`.
- `helm` subprocess: 7 calls (`control/environment` `platform/resources.rs` 4, `environment/analyse.rs` 1; `control/release/src/release_chart.rs` 1; `control/dev/src/dev/activation.rs` 1).
- Kubernetes names: `secret_name`, `service_account_name` (moving beside `project_env_key`, tab 54 finding 5).

Options:

- A: one crate, `wamn-kubernetes`: the in-cluster client on `hyper` (tab 54 finding 4 B), the typed reads and writes the platform uses (Secret, ConfigMap list, status lists), and the `kubectl` and `helm` runners. Credential store, host, web edge, control environment, release and dev call it. The later `kube` swap happens inside it. The edge device never links it.
- B: the client only (API calls) in `wamn-kubernetes`; the `kubectl` and `helm` runners stay in `control/environment` and are imported by release and dev.

## Follows from earlier rulings

- U2: `samples.rs` and `wamn-run-state-sqlite` move from `rusqlite` to SQLx (SQLite).
- Tab 01: `wamn-execution-contract` becomes `wamn-digest`; the hand-written `sha256:` hex check in `web.rs::LiveRelease::root` uses it.
- Tab 02: `wamn-control-registry` is `wamn-registry`.
- Tab 05: session verification through `jsonwebtoken` (`EdDSA`).
- Tab 54 finding 1: `grants.rs` uses `RoleName`.

## Fine as is

- `forward.rs`: `hyper` with rustls over the TLS stack wash-runtime links.
- `serial.rs`: the serial port through `rustix` termios, no serial crate.

## Tests deleted, changed or moved

- Finding 2: tests of `EdgeDelivery` as a `RouteDelivery` change to the shared path.
- Finding 3 A: device limiter tests go.

## Not read

- `operation.rs`, `release.rs`, `policy.rs`, `config.rs`, `device.rs`, `samples.rs`, `status.rs` bodies; `web.rs` proxy body; tests.

## Rulings

- Finding 3 ruled A (2026-11-12): the device limiter goes, as in the cloud; the pool bound is the limit.
- Finding 6 ruled A (2026-11-12): one `wamn-kubernetes` crate holds the in-cluster client, the typed reads and writes, and the `kubectl` and `helm` runners; the later `kube` swap happens inside it.
- Findings 1, 2, 4, 5 ruled A (2026-11-12). Finding 2 settles tab 31 check 5 and confirms tab 31 finding 1 for the edge.
