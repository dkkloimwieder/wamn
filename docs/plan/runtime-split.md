# Runtime split

Sep 23, 2026. Epic 12 in the platform sequence. It is epic 3 in [routes, workflows, and the router](routes-router.md), section 7. Beads epic: `wamn-3lw7`.

The owner accepted this scope on 2026-09-23. Section 8 records the rulings.

## 1. Goal

`wamn-runtime` becomes two crates. The engine crate holds the Wasmtime and wash-runtime host, component admission and artifacts, the plugin trait, lifecycle, and `invoke_operation`. `wamn-runtime` keeps the plugin set: every plugin that talks to a network service. Run-state gets a storage trait, and the existing Postgres code becomes its first adapter.

The edge (routes-router section 6) links the engine crate and never `wamn-runtime`. The router does not move. Epic 2 moves it later.

The split is a crate split, not a set of Cargo features. If no CI job builds a feature combination, that combination rots. The workspace has one CI build. Two crates give two dependency graphs that every build checks.

## 2. Layers

A layer depends only on the layers below it. From the bottom:

1. `wamn-run-state`: pure decisions and the storage traits. No database, no wasm, no clock.
2. `wamn-engine`: the host, admission, artifacts, lifecycle, `invoke_operation`.
3. `wamn-runtime`, the plugin set: Postgres, JetStream, blobstore, HTTP connections, credentials, logging, route authentication, OCI registry.
4. Host services: `wamn-execution-host`, `services/host`, `services/ctl`.

Run-state sits below the engine, by owner ruling 2. The reason is that `invoke_operation` takes the intent store as a parameter (routes-router section 7, decision 3). If run-state sat above the engine, the engine cannot name the trait. Run-state links no refused crate today (measured with `cargo tree -p wamn-run-state -e normal`), so this order costs nothing.

## 3. Measured facts

These facts changed the first plan. Each one is measured on main at e707ff026.

wash-runtime links network crates with every feature off. A probe crate with `wash-runtime = { default-features = false }` at the pinned rev links `async-nats`, `nkeys`, `redis`, `tonic`, `opentelemetry-otlp`, `hyper-util` (client), and `hyper-rustls`. The engine crate needs wash-runtime for `Engine`, `HostPlugin`, and native workloads. So `cargo tree -p wamn-engine` can never be free of NATS, OTLP gRPC, and a hyper client while the engine uses wash-runtime. It stays free of Postgres, `object_store`, and OCI, because those come from optional wash-runtime features (`wasmcloud-postgres`, `oci`) or from wamn code. Owner ruling 1 accepts the list, and finding `wamn-qt1t` records it. There is no upstream issue and no upstream PR. If the list costs the edge something later, the answer is a fork or a different host crate, decided then.

Session verification needs the network. `session_keys.rs` fetches the issuer JWKS over HTTPS with `reqwest`. `session_verifier.rs` uses `wamn-platform-identity`, which links `tokio-postgres`. Both stay in `wamn-runtime`.

`invoke_operation` is bound to four `wamn-runtime` plugins by name. `operation/native_policy.rs` activates each call by calling `WamnPostgres`, `WamnLogging`, `ConnectionHttp`, and `WamnBlobstore` methods directly (bind claims, bind statements, bind transaction scope, then revoke). `OperationHost` holds those plugins and reads the release components from Postgres. A `git mv` of `invoke_operation` alone does not build. An in-place seam comes first (issue 6).

The existing Postgres run-state code is the queued-run lifecycle. `production_claim.rs` implements claim, lease renewal, completion with caller release, and the reap of effect-uncertain runs, as methods on `WamnPostgres`. The queue executor in `crates/execution/host/src/queue.rs` calls them. The route path writes no intent record today. Every `invoke_operation` caller passes `None`.

`cargo metadata` resolves features for the whole workspace. `wamn-runtime` turns on the wash-runtime `oci` feature. A metadata walk from the engine then reports `oci-client`, but the engine alone does not link it. The dependency test uses `cargo tree -p wamn-engine -e normal` for that reason. The lane measures this before it writes the test.

## 4. Decisions

### Crate names and paths

Owner ruling 4: the engine moves out into `wamn-engine` at `crates/platform/engine`. The plugin set keeps the name and path it has, `wamn-runtime` at `crates/platform/runtime`. No crate is renamed. A crate name says what the crate holds, never where it runs. Crate names are singular.

### Where the plugin trait lives

wash-runtime's `HostPlugin` is the plugin trait. The engine re-exports it and owns no plugin trait of its own. The engine adds one trait for the per-call authority seam, `InvocationPolicy` (issue 6). `invoke_operation` must activate and revoke call authority on plugins that it cannot name.

### Session verification

`wamn-runtime`, by owner ruling 5. `session_keys.rs` fetches keys over HTTPS, and `session_verifier.rs` links `wamn-platform-identity`, which links `tokio-postgres`. For the edge, a verifier over a local key file is a second implementation of the same key-source trait, not a move. No issue is filed for that now.

### The storage traits

Run-state gets two traits, because today's code and the route path need different shapes.

`RunStore` is the queued-run lifecycle. It holds the existing Postgres code as its first adapter. `wamn-3lw7.1` landed it with the real signatures of the `WamnPostgres` methods that it replaced.

```rust
#[async_trait]
pub trait RunStore: Send + Sync {
    /// The result of one claim turn.
    type ClaimResult: Send;
    /// Take the next claimable run: grant a lease and serialize its effect intent.
    async fn claim_next(&self, component_id: &str, package_ids: &[String], environment: &str, lease_ttl_ms: i64) -> Result<Self::ClaimResult, ProductionClaimError>;
    /// Extend a held lease, fenced by lease generation.
    async fn renew(&self, component_id: &str, run_id: &str, lease_generation: i64, lease_ttl_ms: i64) -> Result<ProductionLeaseRenewal, ProductionClaimError>;
    /// Record the outcome and release a waiting caller.
    async fn complete(&self, component_id: &str, run_id: &str, lease_generation: i64, completion: &ProductionCompletion) -> Result<ProductionCompletionResult, ProductionClaimError>;
    /// Reap at most one crash-budget-exhausted run.
    async fn reap_exhausted(&self, component_id: &str, package_ids: &[String], environment: &str, grace_ms: i64) -> Result<ProductionReapResult, ProductionClaimError>;
    /// Record host deadline adjustments for a held run.
    async fn record_deadline_adjustments(&self, component_id: &str, run_id: &str, lease_generation: i64, adjustments: &serde_json::Value) -> Result<bool, ProductionClaimError>;
}
```

The four decisions in the block map onto it. Write intent is `claim_next`, and record outcome is `complete`. Park or release is `complete` with a caller release, and `renew`. Find uncertain is split between two methods. `reap_exhausted` ends a pre-effect exhausted run as `infrastructure-failure`. A run with effect evidence comes back to `claim_next`, which ends it as effect-uncertain and never replays it.

The decision types that name no router, catalog, or `tokio-postgres` type moved to run-state with their names. `ClaimResult` is an associated type, because `ProductionClaimResult` holds `CandidateBindingWorld` from the claim code in `wamn-runtime`. The router mapping (`ProductionRouterAction`) stays in `wamn-runtime`, because it names `wamn_router::Outcome`.

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

Owner ruling 3: Epic 12 declares `IntentStore` and gives it no implementation. The Postgres adapter in this epic is the `RunStore` adapter. No route logs an intent. `invoke_operation` takes `Option<&dyn IntentStore>`, and every caller still passes `None`. A Postgres implementation needs a new table, because `effect_attempts` rows belong to a `runs` row and a route call has none. Intent logging on routes is its own epic after this one, `wamn-an24`. `wamn-7icx` said that Epic 3 turns logging on by operation kind. That step moved to `wamn-an24`.

Both traits use `async-trait`, because `invoke_operation` takes a trait object. `async-trait` is a macro crate and adds no runtime dependency.

### Component artifact source

The engine declares an `ArtifactSource` trait (`pull_verified` for one admitted component). It keeps the digest check and the local file source, which exists today as `ComponentArtifactSource::local`. The OCI registry source stays in `wamn-runtime` with `registry_credentials.rs`, `registry_transport.rs`, `release_manifest_source.rs`, and `release_manifest_artifact.rs`. No third crate: the edge uses the file source from the engine.

### The dependency test

The engine crate has a test that runs `cargo tree -p wamn-engine -e normal --prune wash-runtime --prefix none`. The test refuses these crates:

- Postgres: `tokio-postgres`, `deadpool-postgres`, `postgres-types`.
- Network services: `async-nats`, `object_store`, `redis`.
- OCI: `oci-client`, `oci-wasm`.
- HTTP clients: `hyper-util`, `hyper-rustls`, `reqwest`.
- OTLP gRPC: `opentelemetry-otlp`, `tonic`.
- Upper layers: `wamn-router`, `wamn-runtime`.

A second assertion pins the wash-runtime list from section 3 exactly. If the list grows, the test fails. `wamn-router` is on the list because routes-router rule 7 keeps the router out of the base platform.

## 5. What goes where

| Today in `wamn-runtime` | After |
| --- | --- |
| `engine.rs`, `lifecycle.rs`, `component_imports` in `lib.rs` | `wamn-engine` |
| `component_admission.rs`, `component_artifact.rs`, `release_manifest.rs`, `plugins/invocation_trace.rs` | `wamn-engine` |
| `component_artifact_source.rs` | split: trait, digest check, and file source in `wamn-engine`, registry source in `wamn-runtime` |
| every file in `plugins/` except `invocation_trace.rs` | `wamn-runtime` |
| `session_keys.rs`, `session_verifier.rs`, `connection_authority.rs`, `connection_generation.rs` | `wamn-runtime` |
| `expected_router.rs`, `local_application.rs`, `wiring_lowering.rs` | `wamn-runtime` |
| `registry_*.rs`, `release_manifest_source.rs`, `release_manifest_artifact.rs` | `wamn-runtime` |

| Today in `wamn-execution-host` | After |
| --- | --- |
| `invoke_operation`, `OperationCall`, `OperationClosure`, `operation/invocation_policy.rs`, `operation/native_call.rs`, `operation/native_workload.rs`, `warm_reuse.rs` | `wamn-engine`, `invoke_operation` is `pub` |
| `OperationHost`, `operation/native_policy.rs`, nested calls, `route.rs`, `router_driver.rs`, `queue.rs` | stay, and implement the engine traits |

The admission test at `component_admission.rs:710` calls `wiring_lowering::project_component_operations`. That test moves to `wamn-runtime` beside `wiring_lowering.rs`. It is not rewritten.

## 6. Issues

Each issue names the files it moves. Each ends with the workspace building, clippy and fmt clean, and the tests of the crates it touches passing. The full workspace sweep runs once, in `wamn-3lw7.8`, and its count stays at 2490 (main after `wamn-11tj`). A move commit is a `git mv` with no content change. Import and doc-link fixes go in the next commit.

1. `wamn-3lw7.1` Run-state storage traits, Postgres adapter behind `RunStore`. Declares `RunStore` and `IntentStore` in run-state. `WamnPostgres` implements `RunStore` with today's `production_claim.rs` bodies. `queue.rs` calls through the trait. The empty `IntentStore` in `operation.rs` goes, and `invoke_operation` takes run-state's trait. The run-state live tests, `production_claim_live.rs`, `production_claim_durable_live.rs`, and `executor_platform_surface_live.rs` pass unchanged.
3. `wamn-3lw7.3` Create `wamn-engine` with `engine.rs` and `lifecycle.rs`, and the dependency test. The workspace `wash-runtime` entry drops `oci`, and `wamn-runtime` adds it. The lib.rs header lists what the crate exports and what it refuses.
4. `wamn-3lw7.4` Admission and artifacts into the engine: `component_admission.rs`, `component_artifact.rs`, `release_manifest.rs`, `plugins/invocation_trace.rs`, `component_imports`. The `wiring_lowering` admission test moves to `wamn-runtime`.
5. `wamn-3lw7.5` Artifact source seam. The engine declares `ArtifactSource`, and holds the digest check and the file source. The registry source stays in `wamn-runtime` and implements the trait. `OperationHost` holds `Arc<dyn ArtifactSource>`.
6. `wamn-3lw7.6` Invocation seam in place, inside `wamn-execution-host`, before any move. `native_call.rs` and `native_workload.rs` stop naming `NativePolicy` and use an `InvocationPolicy` trait with an associated fact type. By owner ruling 6, the trait covers only the calls that `invoke_operation` makes today. `invoke_operation` takes a host trait that gives the loaded application. `NativePolicy` and `OperationHost` implement them. No `wamn-runtime` type moves into the engine. The native policy, route, and router driver tests pass unchanged.
7. `wamn-3lw7.7` Move `invoke_operation` into the engine and make it `pub`: `operation/invocation_policy.rs` (the `InvocationPolicy` and `ApplicationHost` traits from `wamn-3lw7.6`), `operation/native_call.rs`, `operation/native_workload.rs`, `warm_reuse.rs`, and the call half of `operation.rs`. `route.rs` and `router_driver.rs` call `wamn_engine::invoke_operation`.
8. `wamn-3lw7.8` Docs, sweep, closeout. `docs/architecture/overview.md` and `execution.md` name the three crates. The full workspace sweep runs once with the same count and no new failures. Generated output is byte-identical. The closeout states the `wamn-engine` dependency count with and without wash-runtime, so the edge epic knows the cost. It goes on the epic bead, and section 7 of routes-router.md gets one line.

Order: 1, 3, then 4, 5, and 6 in parallel with file fences, then 7, then 8. Issue `wamn-3lw7.2` (the rename) is closed by owner ruling 4. Issue 5 and issue 6 both edit `operation.rs`: 5 owns the `source` field and `released_application`, and 6 owns everything below `IntentStore`.

## 7. Out

- The router move (Epic 2).
- A SQLite adapter.
- A Postgres `IntentStore`, and intent logging on the route path.
- The edge binary.
- Any plugin behavior change.
- GET and caching.
- The workflow feature.

## 8. Owner rulings

The owner accepted this scope on 2026-09-23 with six rulings, recorded on `wamn-3lw7`.

1. The wash-runtime list is accepted. The dependency test ignores what is reached only through wash-runtime. Finding `wamn-qt1t` records the list. No upstream issue or PR.
2. Run-state sits below the engine. `invoke_operation` takes `Option<&dyn IntentStore>` from `wamn-run-state`.
3. Define only: the traits and the `RunStore` Postgres adapter, no logging on routes. Intent logging is `wamn-an24`.
4. No rename. The crates are `wamn-engine` and `wamn-runtime`. A crate name says what the crate holds, not where it runs, and is singular.
5. Session verification stays in `wamn-runtime`. An edge verifier over a local key file is a second implementation of the same trait.
6. The `InvocationPolicy` seam covers only what `invoke_operation` calls today.
