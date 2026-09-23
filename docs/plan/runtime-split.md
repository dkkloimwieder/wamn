# Runtime split

Sep 23, 2026. Epic 12 in the platform sequence. It is epic 3 in [routes, workflows, and the router](routes-router.md), section 7. Beads epic: `wamn-3lw7`.

This page is the scope for the owner review. No code starts before that review.

## 1. Goal

`wamn-runtime` becomes two crates. The engine crate holds the Wasmtime and wash-runtime host, component admission and artifacts, the plugin trait, lifecycle, and `invoke_operation`. The cloud crate holds every plugin that talks to a network service. Run-state gets a storage trait, and the existing Postgres code becomes its first adapter.

The edge (routes-router section 6) links the engine crate and never the cloud crate. The router does not move. Epic 2 moves it later.

The split is a crate split, not a set of Cargo features. If no CI job builds a feature combination, that combination rots. The workspace has one CI build. Two crates give two dependency graphs that every build checks.

## 2. Layers

A layer depends only on the layers below it. From the bottom:

1. `wamn-run-state`: pure decisions and the storage traits. No database, no wasm, no clock.
2. `wamn-engine`: the host, admission, artifacts, lifecycle, `invoke_operation`.
3. `wamn-cloud`: Postgres, JetStream, blobstore, HTTP connections, credentials, logging, route authentication, OCI registry.
4. Host services: `wamn-execution-host`, `services/host`, `services/ctl`.

Run-state sits below the engine, not above it as the block states. The reason is that `invoke_operation` takes the intent store as a parameter (routes-router section 7, decision 3). If run-state sat above the engine, the engine cannot name the trait. Run-state links no refused crate today (measured with `cargo tree -p wamn-run-state -e normal`), so this order costs nothing. This is owner question 2.

## 3. Measured facts

These facts change the plan in the block. Each one is measured on main at e707ff026.

wash-runtime links network crates with every feature off. A probe crate with `wash-runtime = { default-features = false }` at the pinned rev links `async-nats`, `nkeys`, `redis`, `tonic`, `opentelemetry-otlp`, `hyper-util` (client), and `hyper-rustls`. The engine crate needs wash-runtime for `Engine`, `HostPlugin`, and native workloads. So `cargo tree -p wamn-engine` can never be free of NATS, OTLP gRPC, and a hyper client while the engine uses wash-runtime. It stays free of Postgres, `object_store`, and OCI, because those come from optional wash-runtime features (`wasmcloud-postgres`, `oci`) or from wamn code. This is owner question 1.

Session verification needs the network. `session_keys.rs` fetches the issuer JWKS over HTTPS with `reqwest`. `session_verifier.rs` uses `wamn-platform-identity`, which links `tokio-postgres`. By the rule in the block, both go to the cloud crate.

`invoke_operation` is bound to the four cloud plugins by name. `operation/native_policy.rs` activates each call by calling `WamnPostgres`, `WamnLogging`, `ConnectionHttp`, and `WamnBlobstore` methods directly (bind claims, bind statements, bind transaction scope, then revoke). `OperationHost` holds those plugins and reads the release components from Postgres. A `git mv` of `invoke_operation` alone does not build. An in-place seam comes first (issue 6).

The existing Postgres run-state code is the queued-run lifecycle. `production_claim.rs` implements claim, lease renewal, completion with caller release, and the reap of effect-uncertain runs, as methods on `WamnPostgres`. The queue executor in `crates/execution/host/src/queue.rs` calls them. The route path writes no intent record today. Every `invoke_operation` caller passes `None`.

`cargo metadata` resolves features for the whole workspace. The cloud crate turns on the wash-runtime `oci` feature. A metadata walk from the engine then reports `oci-client`, but the engine alone does not link it. The dependency test uses `cargo tree -p wamn-engine -e normal` for that reason. The lane measures this before it writes the test.

## 4. Decisions

### Crate names and paths

| Option | Result |
| --- | --- |
| A. `wamn-engine` at `crates/platform/engine` (new), `wamn-cloud` at `crates/platform/cloud` (renamed from `wamn-runtime`) | Both names say what the crate holds. |
| B. Keep `wamn-runtime` as the engine, add `wamn-runtime-cloud` | About nine tenths of the source moves to the new crate. |
| C. Keep `wamn-runtime` as the cloud crate, add `wamn-engine` | Least churn, but "runtime" then names the cloud half. |

Pick A. The engine files move into a new crate. The cloud crate is a directory rename, which keeps the history of its files. The rename is its own issue, so no later issue mixes a rename with a change. This is owner question 4.

### Where the plugin trait lives

wash-runtime's `HostPlugin` is the plugin trait. The engine re-exports it and owns no plugin trait of its own. The engine adds one trait for the per-call authority seam, `InvocationPolicy` (issue 6). `invoke_operation` must activate and revoke call authority on plugins that it cannot name.

### Session verification

Cloud. `session_keys.rs` fetches keys over HTTPS, and `session_verifier.rs` links `wamn-platform-identity`, which links `tokio-postgres`. An edge that needs sessions gets a key-source trait later. No issue is filed for that now.

### The storage traits

Run-state gets two traits, because today's code and the route path need different shapes.

`RunStore` is the queued-run lifecycle. It holds the existing Postgres code as its first adapter.

```rust
#[async_trait]
pub trait RunStore: Send + Sync {
    /// Take the next claimable run: grant a lease and serialize its effect intent.
    async fn claim_next(&self, scope: &ClaimScope<'_>) -> Result<ClaimResult, StoreError>;
    /// Extend a held lease, fenced by lease generation.
    async fn renew(&self, run: &LeaseFence<'_>, ttl_ms: u64) -> Result<LeaseRenewal, StoreError>;
    /// Record the outcome and release a waiting caller.
    async fn complete(&self, run: &LeaseFence<'_>, completion: &Completion) -> Result<CompletionResult, StoreError>;
    /// Find one exhausted run and end it as effect-uncertain. It is never replayed.
    async fn reap_uncertain(&self, scope: &ClaimScope<'_>) -> Result<ReapResult, StoreError>;
    /// Record host deadline adjustments for a held run.
    async fn record_deadline_adjustments(&self, run: &LeaseFence<'_>, adjustments: &[DeadlineAdjustment]) -> Result<(), StoreError>;
}
```

The four decisions in the block map onto it. Write intent is `claim_next`, and record outcome is `complete`. Find uncertain is `reap_uncertain`. Park or release is `complete` with a caller release, and `renew`. The method bodies are today's `WamnPostgres` methods in `production_claim.rs`. A decision type that names no router or catalog type moves to run-state. The other types stay in the cloud crate.

`IntentStore` is the per-call record that `invoke_operation` takes. The route path needs no lease and no queue, so a single-writer SQLite store implements it with no lease table.

```rust
#[async_trait]
pub trait IntentStore: Send + Sync {
    /// Write the intent before the export runs. A repeated key returns the stored record.
    async fn begin(&self, intent: &Intent<'_>) -> Result<Begun, StoreError>;
    /// Record the outcome of a begun intent.
    async fn finish(&self, id: &IntentId, outcome: &StoredOutcome) -> Result<(), StoreError>;
    /// List intents that began and never finished. The host never replays them.
    async fn uncertain(&self, limit: u32) -> Result<Vec<UncertainIntent>, StoreError>;
    /// Close an uncertain intent by an operator decision.
    async fn resolve(&self, id: &IntentId, basis: OperatorActionBasis) -> Result<(), StoreError>;
}
```

`Begun` is `New(IntentId)`, `Finished(StoredOutcome)`, or `Uncertain(IntentId)`. `Intent` carries the tenant, release, package, operation, idempotency key, input hash, and deadline. Park and release are workflow concerns (routes-router section 4), so they stay on `RunStore`.

Epic 12 declares `IntentStore` and gives it no implementation. `invoke_operation` takes `Option<&dyn IntentStore>`, and every caller still passes `None`. A Postgres implementation needs a new table, because `effect_attempts` rows belong to a `runs` row and a route call has none. That table changes behavior, so it is a new epic. `wamn-7icx` said that Epic 3 turns logging on by operation kind. This spec moves that step out. This is owner question 3.

Both traits use `async-trait`, because `invoke_operation` takes a trait object. `async-trait` is a macro crate and adds no runtime dependency.

### Component artifact source

The engine declares an `ArtifactSource` trait (`pull_verified` for one admitted component). It keeps the digest check and the local file source, which exists today as `ComponentArtifactSource::local`. The OCI registry source stays in the cloud crate with `registry_credentials.rs`, `registry_transport.rs`, `release_manifest_source.rs`, and `release_manifest_artifact.rs`. No third crate: the edge uses the file source from the engine.

### The dependency test

The engine crate has a test that runs `cargo tree -p wamn-engine -e normal --prune wash-runtime --prefix none`. The test refuses these crates:

- Postgres: `tokio-postgres`, `deadpool-postgres`, `postgres-types`.
- Network services: `async-nats`, `object_store`, `redis`.
- OCI: `oci-client`, `oci-wasm`.
- HTTP clients: `hyper-util`, `hyper-rustls`, `reqwest`.
- OTLP gRPC: `opentelemetry-otlp`, `tonic`.
- Upper layers: `wamn-router`, `wamn-cloud`.

A second assertion pins the wash-runtime floor from section 3 as an exact list. If the floor grows, the test fails. `wamn-router` is on the list because routes-router rule 7 keeps the router out of the base platform.

## 5. What goes where

| Today in `wamn-runtime` | After |
| --- | --- |
| `engine.rs`, `lifecycle.rs`, `component_imports` in `lib.rs` | `wamn-engine` |
| `component_admission.rs`, `component_artifact.rs`, `release_manifest.rs`, `plugins/invocation_trace.rs` | `wamn-engine` |
| `component_artifact_source.rs` | split: trait, digest check, and file source in `wamn-engine`, registry source in `wamn-cloud` |
| every file in `plugins/` except `invocation_trace.rs` | `wamn-cloud` |
| `session_keys.rs`, `session_verifier.rs`, `connection_authority.rs`, `connection_generation.rs` | `wamn-cloud` |
| `expected_router.rs`, `local_application.rs`, `wiring_lowering.rs` | `wamn-cloud` |
| `registry_*.rs`, `release_manifest_source.rs`, `release_manifest_artifact.rs` | `wamn-cloud` |

| Today in `wamn-execution-host` | After |
| --- | --- |
| `invoke_operation`, `OperationCall`, `OperationClosure`, `operation/native_call.rs`, `operation/native_workload.rs`, `warm_reuse.rs` | `wamn-engine`, `invoke_operation` is `pub` |
| `OperationHost`, `operation/native_policy.rs`, nested calls, `route.rs`, `router_driver.rs`, `queue.rs` | stay, and implement the engine traits |

The admission test at `component_admission.rs:710` calls `wiring_lowering::project_component_operations`. That test moves to the cloud crate with `wiring_lowering.rs`. It is not rewritten.

## 6. Issues

Each issue names the files it moves. Each ends with the workspace building, clippy and fmt clean, and the workspace sweep count unchanged from main (2490 pass after `wamn-11tj`). A move commit is a `git mv` with no content change. Import and doc-link fixes go in the next commit.

1. `wamn-3lw7.1` Run-state storage traits, Postgres adapter behind `RunStore`. Declares `RunStore` and `IntentStore` in run-state. `WamnPostgres` implements `RunStore` with today's `production_claim.rs` bodies. `queue.rs` calls through the trait. The empty `IntentStore` in `operation.rs` goes, and `invoke_operation` takes run-state's trait. The run-state live tests, `production_claim_live.rs`, `production_claim_durable_live.rs`, and `executor_platform_surface_live.rs` pass unchanged.
2. `wamn-3lw7.2` Rename `wamn-runtime` to `wamn-cloud`. Commit one: `git mv crates/platform/runtime crates/platform/cloud` and the workspace member path. Commit two: the package name and every `wamn_runtime::` path in the workspace. No other change.
3. `wamn-3lw7.3` Create `wamn-engine` with `engine.rs` and `lifecycle.rs`, and the dependency test. The workspace `wash-runtime` entry drops `oci`, and `wamn-cloud` adds it. The lib.rs header lists what the crate exports and what it refuses.
4. `wamn-3lw7.4` Admission and artifacts into the engine: `component_admission.rs`, `component_artifact.rs`, `release_manifest.rs`, `plugins/invocation_trace.rs`, `component_imports`. The `wiring_lowering` admission test goes to the cloud crate.
5. `wamn-3lw7.5` Artifact source seam. The engine declares `ArtifactSource`, and holds the digest check and the file source. The registry source stays in `wamn-cloud` and implements the trait. `OperationHost` holds `Arc<dyn ArtifactSource>`.
6. `wamn-3lw7.6` Invocation seam in place, inside `wamn-execution-host`, before any move. `native_call.rs` and `native_workload.rs` stop naming `NativePolicy` and use an `InvocationPolicy` trait with an associated fact type. `invoke_operation` takes a host trait that gives the loaded application. `NativePolicy` and `OperationHost` implement them. No cloud type moves into the engine. The native policy, route, and router driver tests pass unchanged.
7. `wamn-3lw7.7` Move `invoke_operation` into the engine and make it `pub`: `operation/native_call.rs`, `operation/native_workload.rs`, `warm_reuse.rs`, and the call half of `operation.rs`. `route.rs` and `router_driver.rs` call `wamn_engine::invoke_operation`.
8. `wamn-3lw7.8` Docs, sweep, closeout. `docs/architecture/overview.md` and `execution.md` name the three crates. The full workspace sweep runs once with the same count and no new failures. Generated output is byte-identical. The epic closeout goes on the epic bead, and section 7 of routes-router.md gets one line.

Order: 1, 2, 3, then 4, 5, and 6 in parallel with file fences, then 7, then 8. Issue 1 comes before the rename because it edits `production_claim.rs` and `queue.rs`. Issue 5 and issue 6 both edit `operation.rs`: 5 owns the `source` field and `released_application`, and 6 owns everything below `IntentStore`.

## 7. Out

- The router move (Epic 2).
- A SQLite adapter.
- A Postgres `IntentStore`, and intent logging on the route path.
- The edge binary.
- Any plugin behavior change.
- GET and caching.
- The workflow feature.

## 8. Owner questions

1. wash-runtime links NATS, Redis, OTLP gRPC, and a hyper client with every feature off. Does the engine accept that as a pinned floor, with the dependency test measuring the graph without wash-runtime? Pick: yes, and ask upstream to make those optional.
2. Can run-state sit below the engine, so `invoke_operation` can name its trait? Pick: yes, run-state is pure.
3. Does Epic 12 only declare `IntentStore`, and leave intent logging on the route path to a new epic with its own table? `wamn-7icx` said Epic 3 turns it on. Pick: declare only.
4. Rename `wamn-runtime` to `wamn-cloud`? Pick: yes.
