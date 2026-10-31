# Audit lane A: Code quality

Three read-only Codex gpt-6.1-sol runs at reasoning effort high wrote this file, one per part, on 2026-10-31 against commit 8c831349d. Root copied each run's final message verbatim and checked that every finding cites a file and line. Beads: wamn-v3cpv.1, wamn-v3cpv.2, wamn-v3cpv.3. Shared tool output: [measurements.md](measurements.md).

<!-- lane A1, bead wamn-v3cpv.1 -->

## Part 1: unused code

The first ten findings rank deletion candidates by estimated lines removed. Counts exclude dependency source trees and distinguish deletion from relocation.

This lane changed no files and ran no builds or tests. Shared measurements describe commit `788cdadb4`; source inspection used `8c831349d`. Fresh feature queries failed with `error: protecting target directories while Cargo runs: Read-only file system (os error 30)`. Dependency conclusions therefore use manifest features, source references, included files, and measurements.md sections 4 and 10. Full feature output remains unverified.

## Finding A1-1: The startup-burst module has no execution entry point

Severity: medium  
Effort: S  
Area: `tests/integration/src/startup_burst.rs`

Evidence: `tests/integration/src/lib.rs:27`; `tests/integration/src/startup_burst.rs:461`; `tests/integration/src/startup_burst.rs:757`; `tests/orchestrator/src/main.rs:34`.

Problem: The 826-line module exports a live startup measurement runner, but no repository caller invokes it. The gate command enum exposes no startup command. Its three unit tests exercise a reducer inside the same module, so they cannot establish that the runner works.

Proposal: Delete the dormant runner and its private reducer tests. Keep startup measurement only when an owned command or journey executes it.

Deletes: Approximately 827 lines, including the module declaration.

Risk: Removes an unavailable measurement capability. Compile every integration target and retain the existing gate command tests. No current live test exercises `assert_startup`.

## Finding A1-2: The router wiring cache is unused

Severity: medium  
Effort: S  
Area: `crates/execution/workflow/router/src/resolution.rs`

Evidence: `crates/execution/workflow/router/src/resolution.rs:119`; `crates/execution/workflow/router/src/resolution.rs:169`; `crates/execution/workflow/router/tests/resolution.rs:8`; `crates/execution/workflow/src/router_driver.rs:246`; `crates/execution/workflow/src/router_driver.rs:270`.

Problem: `WiringCache`, `VersionKey`, `CacheInsert`, and `WiringCacheSnapshot` have no caller outside the router crate. Their callers are cache tests. The production driver resolves each workflow claim, lowers its graph, and constructs `ActiveWiring` directly.

Proposal: Delete the cache implementation and its tests. Retain the small `ActiveWiring` carrier separately. Remove the cache-only OpenTelemetry dependency and exports.

Deletes: Approximately 574 source and test lines, plus cache dependency comments and declarations.

Risk: `ActiveWiring` remains a production dependency. The router walk tests and workflow lowering tests must pass after the carrier moves.

## Finding A1-3: Most operator-recovery helpers outlive their callers

Severity: low  
Effort: S  
Area: `tests/integration/src/operator_recovery.rs`

Evidence: `tests/integration/src/operator_recovery.rs:22`; `tests/integration/src/operator_recovery.rs:97`; `tests/integration/src/operator_recovery.rs:110`; `tests/integration/src/operator_recovery.rs:123`; `tests/integration/src/operator_recovery.rs:161`; `apps/wamn_receiving/tests/route_authentication_live/cluster/materializer_case.rs:135`.

Problem: The remaining external caller uses only `connection_refused`. CRD normalization, host and pod identity collection, operator-state extraction, timestamp helpers, and digest helpers have no external caller. A local test still exercises the otherwise unused host-restart predicate.

Proposal: Retain `connection_refused` and its test in shared test support. Delete the unused recovery framework.

Deletes: Approximately 180 net lines from the 211-line module, after retaining the useful helper.

Risk: Removes dormant recovery assertions. Preserve `only_refused_connections_are_retryable` and the Receiving materializer scenario that uses the helper.

## Finding A1-4: Pool observability exposes functions that nothing calls

Severity: low  
Effort: S  
Area: `crates/platform/runtime/src/plugins/wamn_postgres/claims/pools.rs`

Evidence: `crates/platform/runtime/src/plugins/wamn_postgres/claims/pools.rs:151`; `crates/platform/runtime/src/plugins/wamn_postgres/claims/pools.rs:199`; `crates/platform/runtime/src/plugins/wamn_postgres/claims/pools.rs:208`; `crates/platform/runtime/src/plugins/wamn_postgres/claims/pools.rs:214`; `crates/platform/runtime/src/plugins/wamn_postgres/claims/pools.rs:232`.

Problem: `project_pool_count`, the public pool-status wrappers, and `register_pool_metrics` have no repository callers. The metric registration function therefore registers no production gauges. The private lifecycle view remains useful to a pool-isolation test.

Proposal: Delete the unused public count, aggregation, and registration functions. Retain the lifecycle view with its test.

Deletes: Approximately 91 lines.

Risk: Removes an unused metric registration API. Preserve `guest_and_platform_pool_caches_remain_distinct_under_interleaving` at `crates/platform/runtime/src/plugins/wamn_postgres/claims/tests.rs:215`.

## Finding A1-5: The engine carries an unused reverse context decoder

Severity: low  
Effort: S  
Area: `crates/platform/engine/src/operation/native_call.rs`

Evidence: `crates/platform/engine/src/operation/native_call.rs:396`; `crates/platform/engine/src/operation/native_call.rs:414`.

Problem: `typed_context` decodes a Wasmtime record into `NodeContext`, but no repository code calls it. The forward context construction remains active. Keeping both directions suggests a supported conversion that execution does not use.

Proposal: Delete `typed_context`. Retain the forward conversion and current invocation path.

Deletes: 49 lines.

Risk: No current caller breaks. Preserve the native operation tests, including `native_readiness_initializes_without_running_the_handler`.

## Finding A1-6: Forty-eight native dependency declarations are unused

Severity: medium  
Effort: S  
Area: Native workspace manifests

Evidence: measurements.md section 4; `services/ctl/Cargo.toml:49`; `tests/integration/Cargo.toml:65`; `tests/conformance/Cargo.toml:12`; `crates/execution/host/Cargo.toml:19`.

Problem: Source inspection confirms 48 removable declarations across 15 manifests. This count includes both `wamn-run-state` declarations in the system-test manifest. Four machete entries are false positives because tests include a session fixture from another crate.

Proposal: Remove these declarations:

| Manifest owner | Confirmed unused dependencies |
| --- | --- |
| `services/ctl` | `chrono`, `serde`, `wamn-authoring-model`, `wit-component`, `wit-parser` |
| `test-support/harness` | `async-nats`, `wamn-event-wire`, `wamn-schema-control`, `wamn-test-postgres` |
| `test-support/infrastructure` | `reqwest`, `wamn-event-wire` |
| `tests/system` | Both normal and development declarations of `wamn-run-state` |
| `tests/integration` | `async-trait`, `proptest`, `wamn-client`, `wamn-event-reg`, `wamn-schema-introspection`, `wasm-encoder`, `wasmparser`, `wat` |
| `tests/orchestrator` | `wamn-runtime` |
| `tests/conformance` | `bytes`, `chrono`, `http-body-util`, `hyper`, `hyper-util`, `ring`, `rust_decimal`, `sqlx`, `uuid`, `wamn-execution-contract`, `wamn-execution-host`, `wamn-runtime`, `wasmtime-wasi` |
| `crates/schema/generator` | `wamn-event-wire` |
| `crates/control/lib` | `tokio-rustls` |
| `crates/identity/platform` | `wamn-catalog` |
| `crates/platform/runtime` | `percent-encoding`, `rand_core` |
| `crates/execution/run-state` | `wamn-pg-core` |
| `crates/execution/workflow` | `boon`, `croner`, `ring` |
| `crates/execution/host` | `boon`, `wamn-run-state`, `wamn-schema-control` |
| `crates/client/core` | `rust_decimal` |

Keep `ring` and `tokio-rustls` in integration tests. Keep `base64` and `ring` in execution-host tests. Their included fixture uses them at `crates/platform/runtime/tests/support/session_fixture.rs:6`, `:8`, and `:15`; inclusion appears at `tests/integration/src/control_client_live.rs:28` and `crates/execution/host/tests/host_route_live.rs:28`.

Deletes: 48 dependency lines, plus stale comments. Lockfile reductions require a new resolution and are not counted.

Risk: Dependency removal can expose reliance on combined Cargo features. Compile each affected package independently with all targets and repeat feature analysis. A workspace-only build can hide that reliance.

## Finding A1-7: Generator convenience methods have no callers

Severity: low  
Effort: S  
Area: `crates/schema/generator/src`

Evidence: `crates/schema/generator/src/manifest.rs:444`; `crates/schema/generator/src/manifest.rs:626`; `crates/schema/generator/src/manifest.rs:2813`; `crates/schema/generator/src/data_access.rs:306`; `crates/schema/generator/src/route_schema.rs:161`.

Problem: `FieldText::of_field`, `PackageManifest::has_claim_operation`, `ModelDeclaration::constraint_owner`, `granted_update_fields`, and `read_from_package` have no repository callers. Their underlying fields and active generation paths remain used.

Proposal: Delete these five methods. Retain the data structures and active readers.

Deletes: Approximately 43 lines, excluding surrounding blank lines.

Risk: No current call site changes. Preserve the generator manifest, data-access, and platform-generation tests.

## Finding A1-8: Runtime convenience APIs have no callers

Severity: low  
Effort: S  
Area: `crates/platform/runtime/src`

Evidence: `crates/platform/runtime/src/connection_authority.rs:76`; `crates/platform/runtime/src/connection_authority.rs:86`; `crates/platform/runtime/src/connection_authority.rs:91`; `crates/platform/runtime/src/plugins/wamn_postgres/wiring_resolution.rs:89`; `crates/platform/runtime/src/plugins/wamn_postgres/wiring_resolution.rs:113`.

Problem: Three HTTP authority getters, `ResolvedActiveWiring::component_by_digest`, and `released_connection_bindings` have no repository callers. The last method retains an unused database query beside the active release-fact reader.

Proposal: Delete these five APIs. Keep authority validation, resolved component facts, and `release_fact`.

Deletes: Approximately 39 lines.

Risk: No current caller changes. Preserve HTTP authority tests and workflow resolution tests, especially connection-binding refusal cases.

## Finding A1-9: The Kubernetes credential provider is an unimplemented placeholder

Severity: low  
Effort: S  
Area: `crates/platform/runtime/src/plugins/wamn_postgres/pool.rs`

Evidence: `crates/platform/runtime/src/plugins/wamn_postgres/pool.rs:450`; `crates/platform/runtime/src/plugins/wamn_postgres/pool.rs:454`; `crates/platform/runtime/src/plugins/wamn_postgres/mod.rs:105`.

Problem: `K8sSecretProvider` has no constructor call and always returns an unimplemented error. `allow(dead_code)` preserves a deferred design placeholder in the public runtime API.

Proposal: Delete the provider and its re-export. Add a Kubernetes-backed implementation only with an actual credential consumer.

Deletes: Approximately 24 lines, plus the re-export entry.

Risk: No current execution path changes. Preserve credential-provider and composed-project credential tests.

## Finding A1-10: Nine guest dependency declarations are unused

Severity: low  
Effort: S  
Area: Guest workspace manifests

Evidence: measurements.md section 4; `apps/client_acme_receiving/data/Cargo.toml:9`; `apps/wamn_wms/component/Cargo.toml:13`; `apps/wamn_receiving/component/Cargo.toml:13`; `apps/edge_samples/data/Cargo.toml:12`; `apps/platform/data/access/Cargo.toml:10`.

Problem: Generated source explains most guest machete warnings, but nine declarations remain unused. The Acme data crate emits statement and data access code, not component codecs. The component codec emitter does not emit calls to `wamn-execution-contract`.

Proposal: Remove `serde`, `serde_json`, and development dependency `futures-executor` from Acme data access. Remove `wamn-execution-contract` from the WMS, Receiving, edge-samples, and platform-fixture components, and from edge-samples data access. Remove `chrono` from shared data access.

The remaining guest warnings are false positives under the current generator:

| Reported entries | Source that requires them |
| --- | --- |
| Component `serde`, `serde_json`, and `uuid` entries | Generated codec imports and helpers at `crates/schema/generator/src/generate/wit.rs:1518`, `:1520`, and `:1524` |
| `wamn-data-access` in Acme, WMS, Receiving, edge-samples, and platform-fixture data crates | Generated imports at `crates/schema/generator/src/generate/data.rs:152` |
| Overlay component `wamn-data-access` and `wamn-postgres-statements` | Generated data inclusion at `apps/platform_fixture_overlay/component/src/lib.rs:23` |
| NetSuite `jsonata-expression` | External test module at `apps/netsuite/component/src/lib.rs:116`, with calls at `apps/netsuite/tests/mappings.rs:10` |
| All five `dlmalloc` warnings, including Modbus | Shared allocator at `apps/platform/no-std/guest_runtime.rs:3`; Modbus inclusion at `integrations/modbus/src/lib.rs:9` |

This classification covers every guest and Modbus row in measurements.md section 4. Component dependency entries remain required even when a generated helper itself is unused.

Deletes: Nine dependency lines, plus obsolete comments.

Risk: Compile regenerated guests independently for their real Wasm targets. Native tests omit the allocator path and cannot prove its dependency is unnecessary.

## Finding A1-11: Generated templates emit support that particular operations never use

Severity: medium  
Effort: M  
Area: `crates/schema/generator/src/generate`

Evidence: `crates/schema/generator/src/generate/wit.rs:1407`; `:1483`; `:1503`; `:1522`; `:1529`; `:1562`; `crates/schema/generator/src/generate/data.rs:175`.

Problem: Every shared operation codec receives both envelope decoders, UUID normalization, integer support, and mutation support. A read does not use the keyed decoder, while a keyed operation does not use the read decoder. Operations without relevant fields also receive unused scalar and mutation helpers. Timestamp-column constants appear for every model, although the inspected application caller uses only the purchase-order constant.

Proposal: Emit support from each operation’s declared fields and envelope kind. Emit timestamp-column metadata only for a consumer that needs it.

The allowances on generated `run`, `validate`, `decode`, and `map_error` do not establish dead implementations. Export adapters call those functions at `crates/schema/generator/src/generate/wit_adapters.rs:176` and `:181`; generated handlers call the mapper at `crates/schema/generator/src/generate/component.rs:116`. The SQLx compile-only allowance at `crates/schema/generator/src/sqlx_metadata.rs:341` and generated blobstore bindings at `crates/platform/runtime/src/plugins/wamn_blobstore/bindings.rs:33` also have valid purposes.

Deletes: Unused emitted helpers and constants. Generated outputs were unavailable, so no aggregate line count is claimed.

Risk: Preserve codec tests for UUID normalization, string-encoded integers, absent/null/value mutations, and keyed versus read envelopes. Preserve the timestamp-history test consumer at `apps/wamn_receiving/data/src/read.rs:60`.

## Finding A1-12: Portable test infrastructure remains inside production libraries

Severity: low  
Effort: M  
Area: Router, run-state, catalog, provisioning, project-state, and runtime test utilities

Evidence: `crates/execution/workflow/router/src/lib.rs:51`; `crates/execution/workflow/router/tests/walk.rs:21`; `crates/execution/run-state/src/lib.rs:36`; `:42`; `:54`; `crates/catalog/model/src/lib.rs:28`; `crates/control/provision/src/lib.rs:80`; `crates/identity/project-state/src/lib.rs:371`.

Problem: Router and queue invariant evaluators serve tests but remain ordinary production APIs. Shared intent cases, schema-drift assertions, and database fixtures live behind production-crate test features. These helpers can have test-support ownership without deleting their coverage.

Proposal: Move the portable evaluators, intent cases, schema-drift helpers, database fixtures, and synchronous router test driver into test support. Keep small feature-gated hooks that require private state inside their owning crate. The NATS process launcher can move separately from `watched`, which reads private cache state.

Test and benchmark binaries already have explicit consumers or entry points. `wamn-benchmark-gate` belongs to conformance, `wamn-test-postgres` belongs to test support, and the two integration benchmark binaries remain callable tools. Fixture guests are build inputs at `crates/control/lib/src/platform_build/inputs.rs:25`; relocation into test support requires moving their build ownership too.

Deletes: At least 1,331 lines leave production source directories through relocation: 538 invariant lines, 639 case/drift lines, and 154 database-fixture lines. Net repository deletion is approximately zero.

Risk: Preserve router property tests, queue tests, SQLite and PostgreSQL intent cases, schema-drift tests, and database-fixture installation tests. Keep production features `tenant-key`, `ops`, and `wasm_component_model_implements`; their consumers are explicit.

## Finding A1-13: Internal implementation items are exposed as public APIs

Severity: low  
Effort: M  
Area: `wamn-router`, `wamn-run-state-sqlite`, and `wamn-materializer`

Evidence: `crates/execution/workflow/router/src/lib.rs:62`; `crates/execution/workflow/router/src/retry.rs:17`; `crates/execution/run-state-sqlite/src/lib.rs:302`; `apps/platform/events/materializer/src/lib.rs:10`; `apps/platform/events/materializer/src/decide.rs:147`.

Problem: The router has 87 public item declarations and re-exports, excluding fields, but that inventory is not 87 unused implementations. Retry policy, throttle keys, hop-limit helpers, and error details remain internally necessary. Materializer context, tenant, input, and condition helpers also remain active behind its decision functions.

Proposal: Keep the separate pure router boundary, but expose only the workflow driver’s types and operations. Make internal retry helpers private after moving their test consumers.

Make SQLite `finish_text_in` private. Keep `finish_in`, `transact`, text operations, and close notification public because edge execution uses them, including `services/edge/src/samples.rs:340` and `services/edge/src/operation.rs:214`.

Remove materializer helper re-exports without deleting their implementations. Keep the decision API and workflow types: the guest imports decisions at `apps/platform/execution/materializer/src/main.rs:26`, while native admission imports workflow types at `crates/platform/runtime/src/plugins/wamn_postgres/event_batch.rs:4`.

Deletes: Unused public re-export entries. Most implementations remain, so no substantial line saving is claimed.

Risk: Preserve workflow lowering, materializer decisions, event admission, SQLite text replay, and close-notification tests. Avoid changing persisted or WIT error vocabularies.

## Open questions for the owner

1. Is startup-burst measurement still required? It currently has no repository execution path.
2. Is wiring caching still a requirement? The driver currently resolves each workflow claim directly.
3. Does an external consumer use these unpublished Rust APIs? This audit establishes repository callers only.
4. Can root supply the retained feature-tree output and generated packages before dependency deletions are accepted?

## Proposed fix epics

### Delete unreachable measurement and cache code

1. Delete the uncalled startup-burst module
2. Delete the unused router wiring cache
3. Remove unused operator-recovery helpers

### Remove unused dependencies and runtime APIs

1. Remove the confirmed native and guest dependency declarations
2. Delete unused runtime, engine, and generator methods
3. Delete the Kubernetes credential placeholder

### Reduce production and generated surfaces

1. Move portable test utilities into test support
2. Restrict internal router, SQLite, and materializer visibility
3. Emit only operation-required codec support

<!-- lane A2, bead wamn-v3cpv.2 -->

## Part 2: duplication

The findings below give the consolidation order. Savings are estimates unless marked as measured, and they exclude replacement code unless stated otherwise.

Measurements.md section 7 reports 592 clones and 8,548 duplicated lines. That is a search index, not a deletion budget. Its snapshot is `788cdadb4`; this inspection used `8c831349d`. No files changed, and no builds or tests ran.

Three suspected duplicate groups need no consolidation. The three `canonical_json` methods already call one implementation and retain useful error context (`crates/control/lib/src/environment/document.rs:191`, `crates/control/lib/src/platform/document.rs:146`, `crates/control/lib/src/platform/project.rs:249`). Savings: zero.

The seven `sql.rs` modules serve different owners: run state, its queue, provisioning, registry, schema generation, schema control, and event materialization. Their statements differ by purpose (`crates/execution/run-state/src/sql.rs:24`, `crates/execution/run-state/src/queue/sql.rs:42`, `crates/control/provision/src/sql.rs:66`, `crates/control/registry/src/sql.rs:17`, `crates/schema/generator/src/sql.rs:31`, `crates/schema/control/src/sql.rs:4`, `apps/platform/events/materializer/src/sql.rs:2`). Keep those owners. Savings: zero.

The two identity `password.rs` files also have separate roles. The service imports password operations from the identity library, which owns hashing and verification (`services/identity/src/password.rs:25`, `crates/identity/platform/src/password.rs:236`). Keep HTTP behavior in the service and password authority in the library. Savings: zero from merging these files.

## Finding A2-1: Scalar spelling rules already diverge

Severity: high  
Effort: M  
Area: `apps/platform/execution/contract`, `crates/client/core`, `crates/schema/generator`, `crates/platform/runtime`, `apps/platform/data/record-history`

Evidence: `apps/platform/execution/contract/src/canonical_scalar.rs:28`; `crates/client/core/src/request.rs:490`; `crates/schema/generator/src/cursor.rs:307`; `apps/platform/data/record-history/src/lib.rs:310`; `crates/platform/runtime/src/plugins/wamn_postgres/types.rs:198`.

Problem: The three numeric helpers implement different acceptance and normalization rules. The client trims whitespace, accepts `+`, and removes a negative zero sign, while the shared helper does none of those. Four named timestamp helpers and an inline client formatter also implement overlapping rules. Cursor validation deliberately refuses spellings that request normalization accepts.

Proposal: Make `wamn-execution-contract::canonical_scalar` the sole owner of scalar spelling. First establish PostgreSQL-backed examples, then expose normalization and strict validation separately. Keep database binary decoding and history JSON scanning with their existing owners.

Deletes: Approximately 140–180 net lines, including the history date arithmetic and duplicate numeric parsing.

Risk: Changed bytes affect request identity, replay, history, and cursors. Extend `nested_request_bytes_canonicalize_uuid_time_and_numeric_once` and `noncanonical_timestamps_and_numeric_spellings_refuse` with negative zero, scale, offsets, invalid dates, and PostgreSQL comparisons (`crates/client/core/tests/request.rs:136`, `crates/schema/generator/tests/cursor.rs:110`).

## Finding A2-2: Command runners bypass an existing lifecycle owner

Severity: high  
Effort: M  
Area: `crates/control/lib/src`

Evidence: `crates/control/lib/src/platform/build.rs:328`; `crates/control/lib/src/platform/provider.rs:189`; `crates/control/lib/src/platform_build.rs:1417`; `crates/control/lib/src/kubectl.rs:55`; `crates/control/lib/src/owned_command.rs:27`.

Problem: Five runner surfaces repeat process execution or result handling. Several use unbounded `output()` calls, while `owned_command` already provides deadlines, cancellation, and descendant cleanup. The scoped count is 37 files containing `Command::new`, including tests; it does not mean 37 interchangeable runners.

Proposal: Make `owned_command` the sole owner of bounded, one-shot child execution. Move common success handling there, then migrate the repeated runners. Keep command construction, qualification environment filtering, kubectl error classification, and supervised long-running processes with their current owners.

Deletes: Approximately 60–100 net lines from repeated execution and success handling.

Risk: Consolidation can expose Secret input in diagnostics or change cancellation behavior. Preserve kubectl suppression and extend `stuck_children_cancel_and_reap_descendants_with_diagnostics` to cover migrated entry points (`crates/control/lib/src/kubectl.rs:87`, `crates/control/lib/src/owned_command.rs:147`).

## Finding A2-3: Host contracts repeat thousands of authored JSON lines

Severity: medium  
Effort: M  
Area: `crates/catalog/model/src/host_route`

Evidence: `crates/catalog/model/src/host_route/control/contracts/integration/enable.errors.json:1`; `crates/catalog/model/src/host_route/control/contracts/integration/delete.errors.json:1`; `crates/catalog/model/src/host_route/control/contracts/connection/get.result.json:1`; `crates/catalog/model/src/host_route/control/contracts/connection/write.result.json:1`; `crates/schema/generator/src/materialize.rs:755`.

Problem: Current byte comparison finds 71 redundant files among 103 members of duplicate groups, rather than the scoped estimate of 74. Those redundant files contain 3,020 lines. Each route currently needs separate files even when its input, result, or error shape is identical.

Proposal: Keep `wamn-catalog::host_route` as the owner. Author each shared shape once and assign it explicitly to operations. Let the generator consume the resolved contract map and emit per-operation files only as build output.

Deletes: Measured gross deletion of 71 files and 3,020 repeated lines. Estimated net saving: approximately 2,800 lines after shape assignments.

Risk: Equal contracts can later need different behavior, so assignments must remain explicit. Compare resolved contract bytes and generated clients before and after, then run host-route and control-client live tests.

## Finding A2-4: PostgreSQL adapters repeat transaction and tracing machinery

Severity: medium  
Effort: M  
Area: `crates/platform/runtime/src/plugins/wamn_postgres`

Evidence: `crates/platform/runtime/src/plugins/wamn_postgres/resources.rs:571`; `crates/platform/runtime/src/plugins/wamn_postgres/resources.rs:680`; `crates/platform/runtime/src/plugins/wamn_postgres/production_claim.rs:172`; `crates/platform/runtime/src/plugins/wamn_postgres/production_claim.rs:279`; measurements.md section 7.

Problem: Named and default database bindings repeat tracing, timing, result conversion, and resource insertion. Production claim operations repeat connection checkout and transaction setup. Measurements report 1,183 duplicated lines within runtime, but that total includes tests and overlapping matches.

Proposal: Keep one implementation inside `WamnPostgres` for each database behavior and transaction lifecycle. Resolve named or default project authority first, then enter that implementation. Leave binding methods as argument and boundary-error adapters.

Deletes: Approximately 150–220 net lines from the inspected production blocks.

Risk: A shared path can accidentally substitute default authority for named authority or return a dirty connection to the pool. Exercise both binding feature sets and the existing authority, cancellation, transaction, and pool-isolation tests.

## Finding A2-5: Authority SQL remains embedded in four installation artifacts

Severity: medium  
Effort: M  
Area: `deploy/sql`, `crates/control/provision/src/tenant_key.rs`

Evidence: `deploy/sql/catalog-schema-prefix.sql:45`; `deploy/sql/run-state.sql:98`; `deploy/sql/postgres-init.sql:170`; `deploy/sql/app-schema.sql:127`; `crates/control/provision/src/tenant_key.rs:208`.

Problem: Four schema artifacts embed the same 40-line authority bootstrap. The files already identify a shared builder, so this is repeated distribution content rather than four independent designs. Keeping each schema standalone makes every authority change touch several authored artifacts.

Proposal: Keep provisioning as the authority owner and store one SQL template. Compose installation recipes from that template and the schema bodies. Produce standalone SQL files only as build output, preserving role setup and transaction order.

Deletes: Approximately 120–150 net lines from the four embedded copies.

Risk: Installation order, function ownership, database-name substitution, and expression indexes must remain correct. Use `the_bootstrap_and_literal_renderings_install_the_same_object` and the live deployment SQL authority suite (`crates/control/provision/tests/tenant_key_live.rs:512`, `crates/control/provision/tests/deploy_sql_authority.rs:16`).

## Finding A2-6: Database setup has three homes and repeated schema composition

Severity: medium  
Effort: M  
Area: `test-support/infrastructure`, catalog, provisioning, and project-state test utilities

Evidence: `crates/catalog/model/src/test_database.rs:21`; `crates/control/provision/src/test_database.rs:24`; `crates/identity/project-state/src/test_database.rs:11`; `crates/control/provision/src/lib.rs:174`; `crates/control/lib/tests/package_data_access_live.rs:16`.

Problem: The three `test_database.rs` files install different database floors, meaning required starting schemas. They are not interchangeable copies, but their placement spreads test setup across production libraries. The 33 direct `app-schema.sql` inclusions also let routine tests reconstruct installation order independently.

Proposal: Make `test-support/infrastructure` own named system, catalog, and catalog-plus-platform fixtures. Keep `wamn-test-postgres` responsible only for server and database lifetime. Reuse production schema compositions, while retaining direct SQL reads in tests that inspect those artifacts themselves.

Deletes: Move the three modules, totaling 154 lines, out of production libraries. Estimated net saving from shared setup and migrated callers: 60–120 lines.

Risk: Fixtures must retain distinct owners, grants, and shared-role locking. Preserve the three fixture assertions and run provisioning denial tests against the replacement fixtures.

## Finding A2-7: Workflow tests duplicate component and credential fixtures

Severity: medium  
Effort: S  
Area: `crates/control/lib/tests`

Evidence: `crates/control/lib/tests/workflow_manual_run_live.rs:90`; `crates/control/lib/tests/workflow_schedule_live.rs:77`; `crates/control/lib/tests/workflow_manual_run_live.rs:435`; `crates/control/lib/tests/workflow_schedule_live.rs:335`; measurements.md section 7.

Problem: The manual-run and schedule tests repeat a 75-line component fixture and a 45-line credential setup block. Their assertions prove different behavior and must remain separate. Repeating the setup makes authority and component contract changes expensive.

Proposal: Put the common component and scoped credential fixture in `test-support/infrastructure`. Migrate these two tests first, then assess the other test clones from measurements individually.

Deletes: Approximately 90–110 net lines from the inspected pair.

Risk: Shared setup can conceal a missing permission if it grants the union of both tests. Require each caller to request its exact role set, and retain manual-run and schedule denial assertions.

## Finding A2-8: HTTP guests duplicate transport policy helpers

Severity: medium  
Effort: M  
Area: `apps/platform/no-std/http-request`, `apps/platform/no-std/http-page`

Evidence: `apps/platform/no-std/http-request/src/lib.rs:133`; `apps/platform/no-std/http-page/src/lib.rs:110`; `apps/platform/no-std/http-request/src/lib.rs:215`; `apps/platform/no-std/http-page/src/lib.rs:154`; measurements.md section 7.

Problem: The guests repeat header parsing, required-string parsing, retry delays, connection-error classification, and error construction. The detector finds separate 47-line and 58-line matching regions. Request execution and page traversal remain different behaviors.

Proposal: Give a shared HTTP library under `apps/platform/no-std` ownership of transport policy and its shared WIT types. Keep request construction and pagination in their respective guests.

Deletes: Approximately 75–95 net lines from one copy of the helpers.

Risk: Generated WIT types must retain their identity across both guests. Run both guest test suites, especially rate-limit handling and the page-two failure that discards partial results (`apps/platform/no-std/http-page/src/tests.rs:114`).

## Finding A2-9: Web duplication is concentrated in build policy and bundle tests

Severity: medium  
Effort: S  
Area: `web/shell`, `apps/platform_ui`, `apps/wamn_receiving/web`, `web/sync-bench`

Evidence: `apps/platform_ui/test/bundle.test.ts:13`; `apps/wamn_receiving/web/test/bundle.test.ts:13`; `apps/platform_ui/vite.config.ts:8`; `apps/wamn_receiving/web/vite.config.ts:35`; `web/shell/vite.ts:118`.

Problem: The two 38-line bundle tests differ only in comments and temporary-directory names. Vite files also repeat Solid dependency policy and worker output rules. The browser journeys differ substantially, while both `sw.ts` files already delegate to shared `precache` code with different routing policies (`apps/wamn_receiving/web/src/sw.ts:6`, `web/sync-bench/src/sw.ts:5`).

Proposal: Make `web/shell` own the reusable bundle assertion and shared Vite policies. Keep application entries, browser journeys, cache exclusions, and navigation fallback explicit at each application.

Deletes: Approximately 55–75 net lines. Count zero savings from merging `browser.mjs` or the small `sw.ts` entries.

Risk: Shared tooling must preserve worker filenames, Solid version enforcement, and application-specific cache exclusions. Run both bundle tests, both UI browser journeys, and Receiving offline tests.

## Finding A2-10: Four installers repeat the same installation state machine

Severity: medium  
Effort: S  
Area: `tools/install-helm`, `tools/install-kcl`, `tools/install-tofu`, `tools/install-wash`

Evidence: `tools/install-helm:14`; `tools/install-kcl:13`; `tools/install-tofu:14`; `tools/install-wash:12`.

Problem: The four scripts total 172 lines and repeat platform refusal, cached-binary hashing, temporary paths, downloads, cleanup, permissions, and final replacement. Wash downloads a raw binary, while the other installers unpack archives. Those differences belong in artifact declarations.

Proposal: Make one repository installer own the lifecycle. Keep pinned versions, URLs, hashes, and extraction rules in explicit declarations, with thin wrappers preserving existing command paths.

Deletes: Approximately 60–90 net lines.

Risk: A shared installer must retain both archive and extracted-binary verification. Add isolated tests for valid cache, corrupt cache, bad download, failed extraction, and cleanup, using fake download commands and temporary directories.

## Finding A2-11: Identifier quoting has an existing owner that two callers bypass

Severity: low  
Effort: S  
Area: `crates/schema/generator/src/data_access.rs`, `crates/control/lib/src/reconcile_package_data_access.rs`

Evidence: `crates/schema/generator/src/data_access.rs:1045`; `crates/control/lib/src/reconcile_package_data_access.rs:929`; `crates/platform/pg-core/src/lib.rs:162`.

Problem: Both local functions exactly repeat `wamn_pg_core::quote_ident`. These small copies fall below the detector threshold. Identifier acceptance rules are separate from quoting and must remain with their domain owners.

Proposal: Import `wamn_pg_core::quote_ident` directly and delete both local functions. Remove the forwarding wrapper in schema control when migrating its callers (`crates/schema/control/src/run_plane/schema.rs:136`).

Deletes: Six implementation lines, or nine including the forwarding wrapper, before import changes.

Risk: Generated SQL must retain identical bytes. The existing adversarial quoting test covers embedded quotes and identifier limits (`crates/platform/pg-core/src/lib.rs:125`).

## Finding A2-12: Raw digest helpers repeat one wire spelling

Severity: low  
Effort: S  
Area: generator, control publication/build, and scenario-worker storage

Evidence: `crates/control/lib/src/publish_release.rs:1117`; `crates/control/lib/src/platform_build.rs:119`; `crates/schema/generator/src/generate.rs:1087`; `services/scenario-worker/src/store/mod.rs:16`; `apps/platform/execution/contract/src/lib.rs:54`.

Problem: Four helpers independently produce `sha256:<lowercase hex>`. Publication's copy is test-only, but it still repeats the format. Canonical JSON hashing already has one owner, while raw-byte hashing remains scattered.

Proposal: Make `wamn-execution-contract` own raw-byte digest formatting beside canonical JSON hashing. Delete the four helpers and migrate the existing catalog file-digest helper to that function (`crates/catalog/model/src/edge_bundle.rs:46`).

Deletes: Approximately 20–30 net lines across the five implementations.

Risk: Raw bytes must never pass through JSON serialization. Prove equality with fixed empty, binary, and text vectors, then retain release and package digest assertions.

## Finding A2-13: Tests rediscover the repository root despite an existing helper

Severity: low  
Effort: S  
Area: tests across workspaces

Evidence: `test-support/fixture-package/src/lib.rs:28`; `tests/conformance/tests/repo_lint.rs:93`; `apps/wamn_wms/tests/environment.rs:96`; `apps/wamn_receiving/tests/route_authentication_live/environment.rs:119`; `crates/schema/generator/tests/support/client_ts_check.rs:157`.

Problem: The 31 named definitions include production accessors and discovery logic, so they are not 31 duplicate test functions. Twenty-five definitions occur under test paths. These encode directory depth repeatedly, with inconsistent canonicalization and error handling.

Proposal: Use `wamn-fixture-package::repository_root` as the single test owner. Replace local test helpers with imports. Keep production accessors and package-root discovery unchanged.

Deletes: Approximately 120–150 net lines from test helpers.

Risk: Test binaries must still find the checkout from another working directory and inside linked worktrees. Exercise representative conformance, generator, and application tests under both conditions.

## Finding A2-14: Four shell tools repeat failure and usage handling

Severity: low  
Effort: S  
Area: `tools/test-changes`, `tools/contract-diff`, `tools/repo-lint`, `tools/wasmcloud-release-check`

Evidence: `tools/test-changes:6`; `tools/contract-diff:6`; `tools/repo-lint:6`; `tools/wasmcloud-release-check:9`.

Problem: Four tools repeat the same failure and usage functions with a different diagnostic prefix. Nearby checkout discovery also repeats. `train-run` has a fifth, smaller helper with another signature, so it is not an exact fifth copy (`tools/train-run:5`).

Proposal: Make one small sourced module under `tools/lib` own failure, usage, and checkout discovery. Keep each tool's usage text and command behavior local. Leave the one-line `train-run` helper until its interface needs the shared behavior.

Deletes: Approximately 20–35 net lines from the four tools.

Risk: Exit codes, stderr prefixes, quoting, and invocation from another directory must remain stable. Run the existing conformance tests for these tools and their invalid-argument cases.

## Finding A2-15: Four empty attachment documents represent the same absence

Severity: low  
Effort: S  
Area: application publication directories

Evidence: `apps/rest_connector/publication/attachments.json:1`; `apps/netsuite/publication/attachments.json:1`; `apps/modbus_reader/publication/attachments.json:1`; `apps/platform_fixture_overlay/publication/attachments.json:1`; `crates/control/lib/src/publish_release.rs:1651`; `crates/schema/generator/src/route_schema.rs:452`.

Problem: The four identical files contain only `{}`. Publication already accepts an absent document, but the package attachment reader directly reads its path. Deleting the files alone therefore risks inconsistent behavior between readers.

Proposal: Make the schema generator's attachment reader own the rule that a missing authored document means no authored attachments. Reuse that rule across readers, then delete the four empty files. Preserve generated attachments and refuse malformed documents.

Deletes: Four files and four authored lines. Net line saving depends on replacing the inconsistent reader behavior.

Risk: Absence must not hide malformed JSON or remove generated routes. Add package-reader cases for missing, empty, malformed, and generated-only attachments, then exercise generation and publication for these four apps.

## Open questions for the owner

1. Must authored SQL files remain directly executable as standalone artifacts, or can installation recipes and generated bundles replace them?
2. Can record-history depend on the shared scalar contract, or must its current dependency-free build remain a constraint?
3. Can host contract shapes use explicit shared assignments while generated clients and resolved contract bytes remain unchanged?

## Proposed fix epics

### Canonical contract ownership

- Unify scalar normalization and strict validation
- Replace repeated host contract files with shared shapes
- Consolidate PostgreSQL quoting and digest formatting

### Process and guest execution

- Route one-shot control commands through owned execution
- Collapse PostgreSQL adapter and transaction duplication
- Share HTTP guest transport policy

### Schema and test foundations

- Compose authority SQL from one template
- Centralize database and workflow fixtures
- Reuse one test repository-root helper

### Repository and web tooling

- Share web build policies and bundle assertions
- Consolidate pinned installers and shell command helpers
- Delete empty attachment documents after unifying absence handling

<!-- lane A3, bead wamn-v3cpv.3 -->

## Part 3: errors, panics, file size

Source reviewed at `8c831349d`. Shared measurements describe `788cdadb4`. This review applies the CLAUDE.md boundary convention over the general Rust skill. No files changed. No builds or tests ran. Tests named below are proposed validation.

## Finding A3-1: Operation binding turns infrastructure failures into permission denials

Severity: high  
Effort: M  
Area: crates/platform/runtime/src/plugins/wamn_postgres  
Evidence: `crates/platform/runtime/src/plugins/wamn_postgres/operation.rs:340`, `crates/platform/runtime/src/plugins/wamn_postgres/operation.rs:739`, `crates/platform/runtime/src/plugins/wamn_postgres/claims.rs:1800`.

Problem: `binding_error` translates every binding failure into `PgError::PermissionDenied`. Binding includes a database read that can fail because the connection or table is unavailable. The guest therefore receives an authorization refusal for an infrastructure failure. The checkout path also converts its typed error into display text before this translation.

Proposal: Preserve a contextual claim error with its cause and category. Translate once into the existing `PgError` cases at the operation boundary. Keep actual authorization denials distinct from unavailable infrastructure.

Deletes: The unconditional permission-denied mapping and conversion through `error.to_string()`.

Risk: Retry behavior changes. Add operation boundary tests for an unavailable principal database, a missing principal, and refused statement authority.

## Finding A3-2: Authority cleanup can panic during unwinding

Severity: high  
Effort: L  
Area: crates/platform/runtime/src/plugins/wamn_postgres/claims.rs, crates/execution/host/src/operation/native_policy.rs  
Evidence: `crates/platform/runtime/src/plugins/wamn_postgres/claims.rs:223`, `crates/platform/runtime/src/plugins/wamn_postgres/claims.rs:1873`, `crates/execution/host/src/operation/native_policy.rs:549`, `crates/execution/host/src/operation/native_policy.rs:572`.

Problem: The 57 panic sites in `claims.rs` mostly acquire shared locks with `expect`. Cleanup removes authority from separate registries and can panic before removal finishes. `NativeAuthorityGuard::drop` calls this cleanup, so another panic during unwinding can abort the host. This establishes a containment risk after an internal failure, not a demonstrated guest-controlled trigger.

Proposal: Replace the parallel claim maps with one scope record and one removal operation. Split the 2,564-line file into scope ownership, SQL restrictions, principal authorization, and connection-effect snapshots. Make cleanup infallible and prevent reuse of an unhealthy authority owner.

Deletes: Parallel claim maps, repeated setters and removals, and lock-poison `expect` calls in cleanup.

Risk: Claim lifetime and revocation change. Add subprocess tests for poisoned state during binding and unwinding. Extend cancellation tests to prove that later calls inherit no authority.

## Finding A3-3: Blobstore exposes backend prose and classifies timeouts by text

Severity: medium  
Effort: M  
Area: crates/platform/runtime/src/plugins/wamn_blobstore  
Evidence: `crates/platform/runtime/src/plugins/wamn_blobstore/wit_error.rs:32`, `crates/platform/runtime/src/plugins/wamn_blobstore/wit_error.rs:60`, `crates/platform/runtime/src/plugins/wamn_blobstore/wit_error.rs:70`, `crates/platform/runtime/src/plugins/wamn_blobstore/store.rs:21`.

Problem: Blobstore translates at one boundary, but several `Other` payloads contain backend display text. Those messages can disclose backend context and change after dependency updates. Timeout classification searches that same prose for words instead of preserving a typed cause.

Proposal: Keep contextual storage errors inside the adapter. Map them to the existing WIT variants and fixed, redacted fallback messages. Preserve the distinction between failed dispatch and a lost response.

Deletes: Display-text timeout matching and backend `to_string()` calls in wire payloads.

Risk: Guests can depend on current message text. Add tests with backend errors that contain private paths and misleading timeout words. Preserve the existing lost-body reread behavior.

## Finding A3-4: Durable workflow decisions depend on hidden error-chain types

Severity: medium  
Effort: M  
Area: crates/execution/workflow/src  
Evidence: `crates/execution/workflow/src/queue.rs:508`, `crates/execution/workflow/src/queue.rs:527`, `crates/execution/workflow/src/queue.rs:627`, `crates/execution/workflow/src/router_driver.rs:421`.

Problem: Queue settlement discovers terminal refusals and deadline adjustments by inspecting `anyhow` error chains. Recognized errors produce persisted terminal outcomes. Other errors leave the lease for redelivery. A producer that replaces a typed cause with text can therefore change durable behavior without changing its public signature.

Proposal: Return an explicit execution result that carries deadline adjustments on success and failure. Keep terminal refusal and infrastructure failure distinct. Translate into the existing persisted codes and `FailType` values at queue settlement.

Deletes: `DeadlineAdjustments` as an error-context carrier and settlement downcasts.

Risk: Runs can complete or retry differently. Add tests for every terminal refusal, infrastructure redelivery, and deadline adjustments on failed execution.

## Finding A3-5: Anyhow crosses public library boundaries across ten owners

Severity: medium  
Effort: L  
Area: crates  
Evidence: `crates/control/lib/src/publish_release.rs:315`, `crates/catalog/package-artifact/src/lib.rs:432`, `crates/platform/engine/src/operation.rs:97`, `crates/platform/runtime/src/connectors.rs:180`, `crates/execution/host/src/router_delivery.rs:60`, `crates/execution/workflow/src/queue.rs:66`, `crates/identity/client/src/lib.rs:158`, `crates/identity/platform/src/connection_changes.rs:111`, `crates/platform/component-composer/src/lib.rs:39`, `crates/schema/generator/src/receipt.rs:153`.

Problem: These public APIs expose `anyhow`, including the generator receipt API through an imported `Result` alias. Callers must inspect chains or display text to distinguish refusals from infrastructure failures. This violates the public boundary convention even when a later wire adapter returns fixed cases. The sampled route adapter correctly returns `DeliveryError::ExecutionFailed` instead of sending arbitrary `anyhow` text (`crates/execution/host/src/router_delivery.rs:285`).

Proposal: Remove public visibility from implementation helpers. Give true public operations owner-defined result contracts, with contextual errors below them and one translation at each boundary. Keep `anyhow` at process entry points.

Deletes: Unnecessary public helpers, public `anyhow` signatures, and caller-side error inspection.

Risk: Rust callers require migration. Add contract tests that distinguish refusal, absence, malformed artifacts, and unavailable infrastructure without matching display text.

## Finding A3-6: SQLx maintains a second copy of the PostgreSQL wire taxonomy

Severity: medium  
Effort: S  
Area: apps/platform/data/postgres-sqlx/src/lib.rs  
Evidence: `apps/platform/data/postgres-sqlx/src/lib.rs:709`, `apps/platform/data/postgres-sqlx/src/lib.rs:734`, `crates/platform/runtime/wit/deps/wamn-postgres-0.4/package.wit:70`.

Problem: `WamnPgError` repeats every case from generated `wire::PgError`. A conversion match copies each wire value into the parallel enum. Exhaustive matching prevents silent omissions, but the project explicitly prohibits a parallel wire taxonomy.

Proposal: Store the generated PostgreSQL error directly in `WamnDatabaseError`. Expose that canonical type through the adapter and retain only SQLx-specific behavior.

Deletes: `WamnPgError` and its conversion match.

Risk: Consumers of `pg_error()` require migration. Exercise every WIT case through SQLx and retain its SQLSTATE, constraint, and connection classification.

## Finding A3-7: Diagnostic wrappers discard their original causes

Severity: medium  
Effort: M  
Area: services/identity/src/lib.rs, crates/control/lib/src/dev/activation.rs  
Evidence: `services/identity/src/lib.rs:163`, `services/identity/src/lib.rs:452`, `crates/control/lib/src/dev/activation.rs:287`, `crates/control/lib/src/dev/activation.rs:295`.

Problem: Identity connection failures discard the cause and retain only a fixed sentence. `NativeBackendError` retains an `anyhow::Error` but exposes no `source()`, which truncates normal error-chain traversal. Redaction at public boundaries is useful, but these implementation choices remove diagnostic evidence before that boundary.

Proposal: Preserve contextual causes internally. Keep public diagnostics redacted through an explicit boundary translation. Remove the activation wrapper if the backend can return the owning contextual error directly.

Deletes: Cause-discarding conversions and the opaque activation wrapper.

Risk: Diagnostic output can expose credentials if formatting becomes too broad. Add tests that preserve cause traversal while keeping public output and safe logs free of secrets.

## Finding A3-8: Credential validators duplicate error vocabularies and parser scaffolding

Severity: medium  
Effort: M  
Area: crates/control/provision/src  
Evidence: `crates/control/provision/src/system_reader.rs:104`, `crates/control/provision/src/control_author.rs:111`, `crates/control/provision/src/management_admitter.rs:118`, `crates/control/provision/src/session_role_reader.rs:16`, `crates/control/provision/src/identity_issuer.rs:156`.

Problem: Five error families repeat absent, malformed, scheme, database, role, and extra-input categories. Their parsers repeat corresponding URL predicates. These are concrete duplication candidates within the supplied 124-error inventory. The inventory also includes test errors and generated source text, so its total does not represent 124 competing production contracts (`crates/control/lib/src/dev/activation.rs:1731`, `crates/schema/generator/src/generate/data.rs:1166`).

Proposal: Use one contextual credential-input error and shared parsing predicates within provisioning. Keep each capability’s role and database authority rules distinct. Preserve genuinely different artifact, session, invocation, and workflow contracts.

Deletes: Repeated error enums, formatting implementations, and common parser branches.

Risk: Consolidation can accidentally accept broader credentials. Run the existing credential refusal matrices and add cross-family credential swaps and redaction cases.

## Finding A3-9: Client error decoding invents missing contract data

Severity: medium  
Effort: S  
Area: crates/client/core/src/error.rs  
Evidence: `crates/client/core/src/error.rs:75`, `crates/client/core/src/error.rs:83`, `crates/client/core/src/error.rs:95`, `crates/client/core/src/error.rs:204`.

Problem: Missing error codes become `internal_error`, missing operation names become empty strings, and malformed conflict revisions become zero. The decoder therefore presents malformed responses as meaningful domain refusals. An outcome containing both a value and an error also passes through as an error.

Proposal: Reject malformed envelopes and missing required details with the existing `MalformedResponse` case. Preserve valid unknown operation literals and their details.

Deletes: Defaults that fabricate error codes, operation names, and revisions.

Risk: Previously tolerated responses become explicit transport contract failures. Add cases for missing codes, invalid revisions, absent operations, and simultaneous value and error members.

## Finding A3-10: Native service files combine several independent lifecycles

Severity: medium  
Effort: M  
Area: services/host/src/host.rs, services/cdc-reader/src/lib.rs  
Evidence: `services/host/src/host.rs:936`, `services/host/src/host.rs:2385`, `services/host/src/host.rs:2486`, `services/cdc-reader/src/lib.rs:733`, `services/cdc-reader/src/lib.rs:1352`, `services/cdc-reader/src/lib.rs:1931`.

Problem: `host.rs` contains 3,669 lines, including a startup function that extends beyond 2,000. The CDC library contains 3,048 lines across replication admission, reconnection, decoding, publication, and monitoring. Both files also contain substantial inline tests. Host and claims are frequent change sites in measurements.md section 9.

Proposal: Split host into argument admission, resource composition, local workload loading, and shutdown supervision. Split CDC into replication admission, session recovery, transaction decoding, publication acknowledgments, and slot monitoring. Move existing inline tests with their owners.

Deletes: The large root implementations and duplicated setup exposed during extraction. Moving code alone does not reduce total code.

Risk: Startup ordering, shutdown budgets, and CDC acknowledgment ordering can change. Retain host lifecycle tests and CDC tests for the first unacknowledged retry and bounded publication.

## Finding A3-11: Release control files mix orchestration with implementation owners

Severity: medium  
Effort: L  
Area: crates/control/lib/src/publish_release.rs, crates/control/lib/src/upgrade_environment.rs, crates/control/lib/src/dev/activation.rs  
Evidence: `crates/control/lib/src/publish_release.rs:778`, `crates/control/lib/src/publish_release.rs:1595`, `crates/control/lib/src/publish_release.rs:2925`, `crates/control/lib/src/upgrade_environment.rs:108`, `crates/control/lib/src/upgrade_environment.rs:1238`, `crates/control/lib/src/dev/activation.rs:234`.

Problem: Publication spans 3,427 lines, environment upgrade spans 2,879, and activation spans 2,994. Each combines sequencing with transport, process, or projection details. Publication already has child modules, but component projection and manifest transfer remain in the parent.

Proposal: Keep publication as a coordinator over composition, projection, policy admission, and artifact transfer. Separate upgrade stage decisions from build, cluster, credential, and database adapters. Separate activation protocol state from the native process backend and workload rendering.

Deletes: Monolithic parent bodies, the activation error wrapper, and duplicate infrastructure helpers where ownership already exists.

Risk: Release identity, retry decisions, and process cleanup can change. Retain canonical publication, upgrade-resume, and activation failure-path tests.

## Finding A3-12: Generator files need semantic splits, not blanket panic removal

Severity: medium  
Effort: M  
Area: crates/schema/generator/src/manifest.rs, crates/schema/generator/src/generate/wit.rs  
Evidence: `crates/schema/generator/src/manifest.rs:601`, `crates/schema/generator/src/manifest.rs:1717`, `crates/schema/generator/src/generate/wit.rs:23`, `crates/schema/generator/src/generate/wit.rs:2042`, `crates/schema/generator/src/generate/wit.rs:2853`.

Problem: Manifest parsing and validation occupy 3,114 lines. WIT and codec emission occupy 2,902. Of the 182 matching panic lines in `wit.rs`, 116 explicitly concern writing to a `String`, which does not fail for these formats. Other sites encode generation-order assumptions or emit runtime JSON assertions, so the whole count cannot be dismissed as formatting noise.

Proposal: Split manifest declarations from operation, SQL, identity, and model validation. Split WIT declaration emission from CRUD codecs, custom codecs, and field conversion. Pass typed intermediate results between emitters instead of recovering earlier results from generated JSON.

Deletes: Generation-order JSON lookups, repeated codec fragments, and the large parent implementations. Keep justified formatting assertions.

Risk: Generated contracts can change byte-for-byte. Use existing generation fixtures and guest compilation tests. Add malformed JSON result coverage for emitted codec assertions.

## Finding A3-13: Routing and serving manifests combine policy, representation, and mechanics

Severity: medium  
Effort: M  
Area: crates/platform/engine/src/flow_http_routing.rs, crates/catalog/model/src/serving_manifest.rs  
Evidence: `crates/platform/engine/src/flow_http_routing.rs:132`, `crates/platform/engine/src/flow_http_routing.rs:280`, `crates/platform/engine/src/flow_http_routing.rs:1179`, `crates/catalog/model/src/serving_manifest.rs:475`, `crates/catalog/model/src/serving_manifest.rs:624`, `crates/catalog/model/src/serving_manifest.rs:1293`.

Problem: Routing occupies 2,687 lines across concurrency limits, schema admission, credentials, authentication, and WIT bindings. Serving manifests occupy 2,571 lines across serialized records, canonicalization, attachment conversion, and admission. These seams carry different contracts and failure behavior.

Proposal: Split routing into admission limits, input schemas, credential extraction, authentication, and the WIT adapter. Split serving manifests into wire representation, canonical admission, and projection helpers. Preserve one canonical serialized representation.

Deletes: Large root bodies and repeated projection or validation code exposed by the split.

Risk: Authentication behavior and release digests must remain stable. Retain canonical-byte rejection, digest fixtures, credential refusal, CSRF, and route concurrency tests.

## Finding A3-14: The expect-over-allow policy does not hold across source and generated output

Severity: low  
Effort: S  
Area: Cargo.toml, tools/repo-lint, crates/schema/generator  
Evidence: `Cargo.toml:37`, `services/cdc-reader/src/lib.rs:1702`, `crates/platform/runtime/src/plugins/wamn_blobstore/bindings.rs:33`, `crates/schema/generator/src/generate/wit_adapters.rs:71`, `tools/repo-lint:52`.

Problem: The workspace states that overrides use `expect` with reasons, but handwritten source and generated templates still use `allow`. Some suppressions cover entire modules. The lint command rejects warnings but does not itself enforce the declared override policy.

Proposal: Enforce the policy for handwritten source. Keep unavoidable generated suppressions narrowly attached to generated items, with an explicit generated-code policy. Do not mechanically insert expectations into templates whose output varies by application.

Deletes: Unnecessary suppressions and blanket module exemptions.

Risk: Different generated shapes emit different warnings. Exercise small and full generated packages, and prove that stale handwritten expectations fail the lint gate.

## Open questions for the owner

1. Must poisoned authority state terminate the process, or must the host isolate the affected owner and become unready?
2. Which blobstore `Other` messages are frozen consumer contracts before backend prose is replaced?

## Proposed fix epics

### Preserve boundary meaning

1. Preserve operation binding failure categories
2. Return explicit workflow failures and deadline adjustments
3. Replace public anyhow results with owner contracts

### Consolidate errors without new wire taxonomies

1. Remove the duplicate SQLx PostgreSQL enum
2. Consolidate credential input refusals
3. Preserve internal causes and reject malformed client errors

### Make authority cleanup and storage errors predictable

1. Consolidate claim scope ownership and infallible revocation
2. Freeze redacted blobstore fallback messages
3. Enforce the handwritten lint override policy

### Split files by existing ownership seams

1. Separate native service composition and lifecycle owners
2. Separate release orchestration from infrastructure adapters
3. Separate generator and routing contract owners
