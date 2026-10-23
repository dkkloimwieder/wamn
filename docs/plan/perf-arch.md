# Request path performance: host CPU and Postgres options

Oct 20, 2026 · @D Kloimwieder

## Scope and evidence

The host, not Postgres, is the bottleneck for reads. A get costs about 2.4 ms of host CPU and about 0.1 ms of Postgres CPU. For writes, Postgres commit cost dominates latency, and the host adds a similar tax on top. This doc lists the choices for both sides without picking.

Evidence is main at `92df3a0` (2026-10-20). Numbers come from `tests/bench/local-0.1.28.json`, `tests/bench/local-pg-0.1.28.json`, `tests/bench/platform-0.1.2{5,6}.json`, `docs/operations/benchmark.md`, `docs/plan/request-round-trips.md`, and the beads `wamn-9jvm1`, `wamn-lflrw`, `wamn-h2r2l`, `wamn-spv0f`, `wamn-stxy9`. The function-level profiles from `wamn-lflrw` and `wamn-h2r2l` live only in `/var/tmp` on the bench machine, so their percentages are quoted from the bead text.

Local run shape: 8 cores, host pinned to cores 0–3, Postgres to 4–5, client to 6–7, concurrency 16, 60 s per call, Receiving with the 1000-order seed. Local benchmarks inform, never decide (owner rule 2026-10-18).

| Call, c16 | Full path | Postgres floor (same statements, pgbench) | Statements per request |
| --- | --- | --- | --- |
| get | 1,559/s, p50 9.4 ms | 19,469/s, p50 0.78 ms | 1 |
| update | 576/s, p50 26.5 ms | 1,386/s, p50 11.6 ms (simple-update: 2,489/s, p50 6.2 ms) | 7 |
| query | 700/s, p50 21.9 ms | not replayed | 5.2 |
| record\_receipt | 334/s, p50 46.6 ms | not replayed | 6 |
| mix | 1,023/s, p50 15.2 ms | not replayed | 3.45 |

Marking: a line is a fact unless it says inference. An inference is a reading of the code or a number that was not measured in isolation.

## Label index

Every option in this doc has a label. The letter says which section defines it; the rows there carry the full description.

| Label | Section | What it is |
| --- | --- | --- |
| H1–H8 | Host options | per-request host changes: H1 native base routes (fallback only), H2 warm ingress Store, H3 one parse per request, H4 prepared route matcher, H5 tracing, H6 mimalloc, H7 pooling residency knobs, H8 taps off the hot path |
| P1–P8 | Postgres options | write-transaction changes: P1 generated per-relation triggers, P2 statement-level triggers, P3 history spelling at read time, P4 write log as one write, P5 one set\_config, P6 CTE shape, P7 commit\_delay, P8 no triggers |
| M1–M8 | Measurement plan | experiments that size the options before code |
| O1–O10 | Other candidates | smaller items found alongside |
| A1–A5 | Architecture with wasmCloud kept | A5 application as the HTTP workload (ruled direction), A1 native base routes (fallback), A2 shared host groups, A3 taps and NATS off the request path (the architecture form of H8), A4 one serialization per hop |
| T1–T5 | A3 (H8): taps | the five ways to source a live view once base routes stop passing the bridge |

## The request path today

A base route runs two Wasm instances per request and crosses the component boundary about eight times. The ingress guest is a shell: every decision it makes is a host import.

A get, in order (code on main):

1. wash-runtime's HTTP server takes the request and instantiates the `http-route` guest (`apps/platform/ingress/http-route`). Locally `pool_size` is 0, so this is a fresh Store per request; GCP has `poolSize: 4` in the chart, unmeasured (`wamn-6rsfg`).
2. The guest reads the body through wasi-http 0.3 streams, parses it (`serde_json::from_slice`), maps it, and serializes it again (`lib.rs:427-449`).
3. Import `routes(method, authority)`: the host rebuilds the route table for that method from the manifest on every call and lowers the whole `Vec<RouteDefinition>` into guest memory (`flow_http_routing.rs:652`, `:806`). The guest matches the path.
4. Import `authenticate`: answered from host memory since `wamn-stxy9.2` (0.02 ms p50).
5. Import `validate_input(attachment_id, payload: String)`: the payload is copied to the host and parsed again for the schema check.
6. Import `try_acquire`: the route permit.
7. Import `deliver(DeliveryRequest)`: payload copied to the host again, parsed again (`router_delivery.rs:183`), published as a JetStream tap with the payload (`:205`), target resolved by a linear scan of `manifest().routes` (`:217`).
8. `run_route` → native dispatch: input serialized once more (`intent.rs:318` wraps it in an array, `operation.rs:145`), copied into the app component's Store (fresh, `wamn.operation.instantiate` 0.19 ms), the guest parses it, runs one `wamn:postgres` statement (the one trip), serializes its outcome.
9. The host parses the outcome (`native_call.rs:389`, `intent.rs:414`), taps it again, returns the result as a String to the ingress guest.
10. The ingress guest parses `committed_result` (`lib.rs:902`), wraps it, serializes (`to_vec`), and streams it out through wasi-http.

An update takes the same path, with two pipelined Postgres trips instead of one (section 5).

Count per get: 2 instantiations, 5 host imports, about 6 JSON parses and 5 serializations, at least 6 payload copies across the boundary, 2 tap publishes. The tap publish is non-blocking (`poll_immediate`), so it is a broker and serialization cost, not a wait.

Not on the path any more: actor-label query (`wamn-stxy9.1`), per-request auth queries (`.2`), separate claim and finish trips (`.3`), per-request linking (`wamn-9jvm1.2`), SQL cursors for bounded reads (`wamn-7fdyb`), 100% trace export (`wamn-spv0f`).

## Where host CPU goes

The profile is flat: no symbol above 2.5% self time. The cost is copies, allocations and boundary crossings spread over the whole path, not one hot function.

Measured parts of one get in the host, p50 ms (platform 0.1.19, GCP, c16, from `benchmark.md` "Named parts"; the auth and actor-label rows are deleted since):

| Part | p50 ms | Status |
| --- | --- | --- |
| wash-runtime invoke: Store, instantiation and guest run of `http-route` | 6.15 | still there, never split further |
| record\_actor\_labels query | 4.01 | deleted |
| identity + permission + principal queries | 5.49 | deleted (auth memory) |
| session settings trip | 1.75 | deleted (pipelined) |
| application statement + decode | 1.73 | stays |
| app component instantiate + call + dispatch | 0.84 | stays |
| route match, validate, permit | 0.06 | stays |
| taps, pool acquire, span bookkeeping | 0.06 | stays |

The 6.15 ms is inflated by CPU contention (2 vCPU node at c16), but it is the largest part on every run and the only one with no span inside it.

CPU shares from the local profiles (bead text, `--app-path`, host at 93% of 4 cores):

- Wasmtime: 18.8% (`wamn-h2r2l.1`)
- allocation family (`malloc`, `free`, `_int_malloc`, `_int_free_chunk`): about 14% (`wamn-9jvm1.1`)
- tracing and OTel export: 13.8% before `wamn-spv0f`; now sampled at the host
- `memmove` 2.9%, `memcmp` 2.5% after `wamn-lflrw`'s three fixes (`wamn-w9kyr`)
- guest wasm code: 8.9% (0.1.25 GCP flamegraph)
- `serde_json::Value::eq` 0.81%, TLB shootdown `smp_call_function_many_cond` 1.42% (0.1.25). Inference: the shootdowns are Store memory unmap on drop; the pooling allocator should hide most of this and the keep-resident knobs are the dial.

Why, structurally (inference from the code in section 2):

- Two Stores per request where one would do. The ingress guest adds no decision the host does not make itself.
- JSON is parsed and re-serialized at every layer boundary because each layer takes `String` and owns a `Value`.
- The route table is rebuilt and copied per request. Receiving is small, so this is cheap today; it is O(routes) and grows with application size.
- wasi-http 0.3 is an async stream API. Body bytes move through futures and channels into guest memory and back out, which is far heavier than a `Bytes` handed to a native handler.

What this means for more applications: per-request host CPU is the number that scales. At 2.4 ms per get, one core serves about 400 gets/s. Each application host pays this per request, plus its fixed per-pod cost (collector, NATS, pools). Postgres at 0.1 ms CPU per get is not in the picture for reads.

## Host options

The options are independent and stack. H1 and H2 are alternatives for the same 6 ms; the rest apply either way.

| Option | Removes | Keeps | Cost and risk | Expected gain (inference) |
| --- | --- | --- | --- | --- |
| H1 Native base routes (fallback only; A5 is the ruled direction): a hyper handler in the host does match, auth, validate, permit, `run_route`, response headers; the `http-route` guest stays only where a Wasm ingress is wanted (edge, or workflows) | one Store, 5 imports, 2–3 JSON passes, the route-table copy, the wasi-http stream machinery | every host plugin as is; the guest code's logic moves, not changes; the route table, cache headers and ETag rules already live in the host | the ingress is no longer wasmCloud-native; two code paths if the guest stays for edge; the Receiving/Acme flow tests exercise the guest path and need a second target | the whole 6 ms part minus the native handler's own cost; largest single gain |
| H2 Warm Store for the trusted `http-route` digest | instantiation and Store setup per request | the guest, the imports, the copies | config only (`WarmReuse`, `poolSize`); a warm guest holds request-local state across calls, which the trust list already accepts for this digest; `max_concurrency = 1` serializes calls per instance | 0.1.23 warm pool 16 gave get 645→855/s on GCP; local runs at pool 0 so the local numbers overstate instantiation |
| H3 One parse per request | the repeated `String` → `Value` → `String` at validate, bridge, intent, outcome, ingress | the schema check and the wire contracts | touches every layer's signature (`&[u8]` or `Bytes` through bridge → intent → native call; outcome forwarded as bytes); `serde_json::Value::eq` and the array wrap in `intent.rs:318` need a bytes-level equivalent | part of the 14% allocation and 5% memmove/memcmp; roughly 20–30% of host CPU |
| H4 Prepared route matcher per loaded release | per-request `route_definitions` rebuild and `manifest().routes` linear scans | the manifest as the source | small; a radix or `matchit` table built at release load, keyed by method + path | negligible for Receiving; keeps cost flat as route counts grow |
| H5 Keep tracing head-sampled; measure span creation cost of the remaining `info_span!` per import | span bookkeeping at 100% rate | sampled export | already done for export; spans are still created and entered per import call | small, a few percent |
| H6 Global allocator (mimalloc) | part of the 14% in glibc malloc | everything | memory up, CPU down; deferred by owner (`wamn-9jvm1.1`) | 5–10% CPU, not measured |
| H7 Pooling allocator residency knobs (`WASMTIME_POOLING_*_KEEP_RESIDENT`, `MAX_UNUSED_WASM_SLOTS`) | munmap/TLB shootdown on Store drop | fresh Stores | config only; already allowed through `ALLOWED_POOLING_ENV` | 1–3%, measurable with `perf` on `smp_call_function_many_cond` |
| H8 Taps deleted (ruled 2026-10-20; see A3) | 2 serializations and 2 NATS publishes per request | the live view, if it can read from the write log or a sampled tap | the loop console and previews depend on taps; a design question, not a perf fix | host: small; broker: linear in request rate |

Notes on H1. `RouteDelivery` and `RouteAuthenticator` are already traits with host implementations, and `HostRouteDelivery` already answers fixed routes natively. A native base-route handler is a third implementation of the same seam, not a new architecture. The question for the owner is whether the ingress must stay a Wasm component for its own sake.

Notes on H2 vs H1. H2 is a one-line experiment that sizes H1: if warm reuse recovers most of the 6 ms, H1's gain is the crossings and copies only, and H3 may be the better next step.

## The update transaction today

The benched update changes one field (`supplier_id`) on one row of `receiving.purchase_order`, 9 columns, with `row_version` bumped. Each worker owns its orders; there is no contention. Postgres turns that one-field change into 4 heap writes, 1 logical message, about 5 catalog lookups, about 6 jsonb renders in plpgsql, and one fsync.

Two pipelined trips, 7 statements, from the captured replay `tools/bench/pg-floor/update.sql`:

1. Trip 1: `BEGIN`; one `SELECT` with six `set_config` calls (`statement_timeout`, `search_path`, `app.runner`, `app.role`, `app.user_id`, `app.operation`); `INSERT INTO platform.write_log … ON CONFLICT DO NOTHING RETURNING` (the claim).
2. The command statement: `WITH target AS MATERIALIZED (SELECT … FOR UPDATE), updated AS (UPDATE purchase_order … RETURNING …) SELECT outcome …`.
3. Trip 2: `UPDATE platform.write_log SET result = $1` (the full row as JSON text); `pg_logical_emit_message(true, 'wamn.causation', …)`; `COMMIT`.

Two row triggers fire on the one-row `UPDATE` (`deploy/sql/record-history.sql`):

- `wamn_history.stamp_row` (BEFORE): `to_jsonb(NEW)`, `to_jsonb(OLD)`, a text compare of both minus the stamp columns, then `jsonb_populate_record` to write the stamps back.
- `wamn_history.log_row_change` (AFTER): `row_image(OLD)` and `row_image(NEW)`, each a `to_jsonb` plus a `pg_type` ⋈ `pg_attribute` query for timestamptz columns; a `pg_index` ⋈ `pg_attribute` query for the primary key; a `pg_attribute` lookup for `tenant_id`; a jsonb text compare; a `jsonb_each` diff; then a dynamic `EXECUTE … USING` insert into `purchase_order_history` (`before`, `after` as jsonb, `pg_current_xact_id`).

WAL per update: the row, the history row, two `write_log` versions, the logical message, plus index entries. The history table's primary key is `(row_key jsonb, position)`.

Measured: the replay runs at 1,386/s, p50 11.6 ms; pgbench simple-update (3 statements, one row, commit) at 2,489/s, p50 6.2 ms, on the same Postgres and cores. So the transaction shape costs about 5.4 ms over the commit floor at c16. The full path adds another 15 ms of host and client on top (p50 26.5 ms).

Not broken out here: which of the 5.4 ms is triggers, which is the write log, which is the logical message. `wamn-h2r2l.2` captured per-statement `pg_stat_statements`, `auto_explain` and WAL deltas for exactly this, in `/var/tmp` on the bench machine. During the GCP update run, `pg_stat_activity` showed no lock waits; sessions waited on the host, `WALWrite`, `WalSync` and CPU.

Not measured anywhere: a multi-field or multi-table update. `record_receipt` (receipt + lines, 6 statements, 334/s, p50 46.6 ms) is the closest, and every inserted line fires both triggers once.

## Postgres options

Reads are done: one statement, one trip, nothing left on the platform side. The options below are about the write transaction. P1–P3 are about the history triggers, P4–P6 about the platform rows, P7 about commit, P8 is the alternative to triggers altogether.

| Option | Removes | Keeps | Cost and risk | Expected gain (inference) |
| --- | --- | --- | --- | --- |
| P1 Generated per-relation trigger functions: the applier emits `<relation>_stamp()` and `<relation>_log()` with the PK columns, timestamptz columns, tenant flag and history table name as static SQL | all catalog lookups per row (`pg_index`, `pg_attribute` ×3), the dynamic `EXECUTE` re-plan | the trigger model, the history shape, the stamp rules | the applier already knows the shape; functions must be regenerated on schema change (the applier already diffs triggers, `apply_package/record_history.rs`); one function per relation instead of one shared | largest share of the 5.4 ms; the catalog queries are plan + execute per row |
| P2 Statement-level log trigger with transition tables (`REFERENCING OLD TABLE … NEW TABLE … FOR EACH STATEMENT`) | per-row trigger invocation; N inserts become one `INSERT … SELECT` | row-level stamp (must stay BEFORE row) | PK-change rows (delete + insert under two keys) need the same split in set form; transition tables cost memory for very large statements | little for the 1-row update; large for `record_receipt` and any bulk command |
| P3 Store `to_jsonb` spelling in `before`/`after`; canonicalize timestamps at read time | `row_image` entirely (two catalog queries and a `to_char` per timestamptz column per row) | the canonical spelling on the read path (`load_purchase_order_history.sql` already calls `timestamptz_image`) | stored images change spelling: existing history rows carry the old form unless migrated; every history reader must canonicalize | one of the cheaper wins; pairs with P1 |
| P4 Write log as one heap write | the second `write_log` version (UPDATE) and its WAL | the claim before the statements, which blocks a concurrent retry | the finish must still mark completion; options are a result-less finish for operations whose result is the row itself, a result hash, or a claim row that carries `result` NULL and a finish that writes it (today). The never-replay law needs the stored outcome for effect-uncertain operations, so this is per operation kind | about one heap tuple and its WAL per command |
| P5 One `set_config` instead of six, or one custom GUC holding the claims as JSON | five function calls per transaction | the triggers' reads of `app.user_id` and `app.operation` | triggers and RLS read the settings by name; a JSON claim moves one parse into each trigger | small; the six calls are one statement already |
| P6 Drop `MATERIALIZED` on the `target` CTE, or fold `FOR UPDATE` into the UPDATE | one extra scan per command | the outcome mapping (`not_found` vs `concurrency_conflict`) | the materialized CTE exists to read `target.row_version` after the UPDATE; the planner may already pick the same plan; needs `auto_explain` from `h2r2l.2` to say | small to none |
| P7 `commit_delay` / `commit_siblings` group commit | fsync per transaction at c16 | `synchronous_commit = on` | p50 rises by the delay; throughput rises with concurrency; a cluster-wide knob (CNPG parameters) | throughput at c16 up, latency not; measure before adopting |
| P8 No triggers: the generator emits the history and stamp writes inside the command statement (a `history` CTE) | plpgsql, both triggers, every per-row cost | the history table and contract | hand-written SQL and overlays must emit their own history rows or publish must verify it; a write that bypasses the generator writes no history; fits the one-statement-command direction (`wamn-udcol`, `wamn-y424j`) | everything P1–P3 give and more; the biggest model change |

Keep as is: `synchronous_commit = on`, `pg_logical_emit_message` (CDC causation needs it), the claim before the statements.

Notes on P1 vs P8. P1 keeps triggers as the safety net for any writer; P8 trusts the generator and publish. The owner's standing rule is less restriction early, which argues for P1 now and P8 only when overlay composition settles. Both are compatible with P2 and P3.

Scaling note: every per-row trigger cost multiplies by rows per command. A WMS move that touches packaging, locations and a transaction log pays P1's catalog lookups per row per table today.

## Measurement plan

Each experiment is cheap, isolates one option, and uses `tools/bench/local.sh` as it stands. Run paired against main, keep a result only if it repeats.

| Experiment | Sizes | How | Decides |
| --- | --- | --- | --- |
| M1 Warm ingress Store locally | H2, and how much of the 6 ms is instantiation | set the local activation `pool_size` for the `http-route` digest to the chart's 4 (and 16), `--call get --no-edge` | H1 vs H2 vs H3 priority |
| M2 Split the 6 ms | H1, H3 | add `wamn.route.*` spans around the guest's five imports and the body read/write, `--app-path --profile`; or `perf` with the perfmap on the guest and read `http_route` self time | what the guest itself costs vs. the imports |
| M3 Count the parses | H3 | `perf` inclusive time of `serde_json::de` and `::ser` on `--call get --profile`, attributed by caller | H3's share |
| M4 Triggers off | P1–P3, P8 | the `--pg-floor` update replay with both triggers disabled on `purchase_order` (one `ALTER TABLE … DISABLE TRIGGER` line in the replay setup) | the trigger share of the 5.4 ms |
| M5 Per-statement breakdown | P4–P6 | read the existing `wamn-h2r2l.2` output (`pg_stat_statements` rows, `auto_explain`, WAL delta) from `/var/tmp`; or rerun `--call update` which captures the same | which platform row costs what |
| M6 `commit_delay` | P7 | the replay with `commit_delay = 1000`, `commit_siblings = 5` at c16 and c4 | whether group commit helps at the target concurrency |
| M7 Rows per command | P2 | `--call record_receipt` with 1, 5 and 20 lines per receipt; compare statement time per row | whether statement-level triggers matter now |
| M8 Residency knobs | H7 | `WASMTIME_POOLING_LINEAR_MEMORY_KEEP_RESIDENT` and `ASYNC_STACK_KEEP_RESIDENT` set, `perf` on `smp_call_function_many_cond` | config-only gain |

Order suggested by cost: M1, M4 and M5 need no code and answer the two biggest questions (how much of the host part is instantiation; how much of the Postgres part is triggers). M2 and M3 need small instrumentation. M6–M8 are one-line config runs.

Gate: `wamn-benchmark-gate --local` refuses a 20% throughput drop on any call; every option that lands runs it.

## Other candidates

Found while reading around the request path. None is measured; each is a small, independent check.

| Candidate | Where | What it costs today | Choice | Size (inference) |
| --- | --- | --- | --- | --- |
| O1 Task per statement | `wamn_postgres/run_stream.rs:191` spawns a tokio task and three channels for every statement, bounded reads included | a spawn and channel hops per get | direct await for non-streaming reads; keep the task for `run-stream` | a few percent of host CPU |
| O2 Row-at-a-time streaming | query rows cross three boundaries one record at a time: `wamn:postgres` stream → app guest → `deliver-stream` → ingress guest → wasi-http | query p50 21.9 ms vs get 9.4 ms for 20 rows; grows with page size | batch N rows per crossing, or H1 removes the ingress crossing | large for table reads |
| O3 Schema validation per request | `validate_input` runs a JSON-schema check on every payload (`flow_http_routing.rs:691`); the generated guest typechecks the same payload again | one validator pass per request | measure the validator's share; compiled validator, or skip for generated clients | unknown until measured |
| O4 No `wasm-opt` on guests | `components.md:73` | guest wasm is 8.9% self time on the 0.1.25 profile | run `wasm-opt -O3` on the Receiving guest and compare | up to a few percent of the guest share |
| O5 Host release profile | `Cargo.toml` `[profile.release]` is `lto = "thin"` only | default `codegen-units`, unwinding panics | `lto = "fat"`, `codegen-units = 1`, `panic = "abort"` for the host binary; build time up | a few percent |
| O6 Edge upstream connections | `deploy/platform/edge/templates/configmap.yaml`: `worker_processes 1`, `proxy_http_version 1.1`, no `upstream { keepalive }` | a new TCP connection to the host per proxied request; client p50 8.5 ms above host p50 on GCP (LB, edge, network together) | upstream keepalive block; worker count; the local bench mirrors the chart, so change the chart first | part of the 8.5 ms; config only |
| O7 History table primary key | `(row_key jsonb, position)` from `create_history_table` | a btree over jsonb maintained on every history insert | `(position)` as the key plus an index `(row_key, position)`, or a typed row key | per history row; pairs with P1–P3 |
| O8 Write log key | unique `(operation text, idempotency_key text)`; `operation` is a \~45-byte sealed id per row | index and WAL bytes per command | an operation lookup table or a hash column | small |
| O9 HOT updates on model tables | `row_version` and `updated_at` change on every update | HOT applies only if no index covers a changed column; otherwise each update writes index entries | check `pg_stat_user_tables.n_tup_hot_upd` vs `n_tup_upd` after a run; set `fillfactor` below 100 on hot tables | per update; measurable in one query |
| O10 Fixed cost per host pod | one host pod per environment: exporter, NATS connections, per-tenant pools, auth memory | unknown idle CPU and memory | `kubectl top` on wamn-dev before the WMS and Acme environments multiply | a planning number, not a request-path gain |

O2 is the one that changes with application size the way the owner's concern names: table screens read pages, and every row pays three crossings today.

## Architecture with wasmCloud kept

wasmCloud is not the limit. Its HTTP workload was built to serve a guest that is the application; wamn put a platform-code guest in front of the application and orchestrates between the two in the host, and that split is the per-request cost. The direction (owner, 2026-10-20) is A5: serve the application directly, keeping wasmCloud as the runtime, the HTTP server and the plugin host. A1, the front door in the host, is recorded only as a fallback.

What wasmCloud does for wamn today (fact, from `wash_runtime::` use in the tree and `deploy/`):

| Piece | Job | Used on the request path? |
| --- | --- | --- |
| Engine, pooling allocator, `InstancePre`, memory limiter, `HostPlugin` seam | runs the application component, links the plugins | yes, and it stays |
| HTTP ingress workload (wasi-http 0.3 server) | serves the `http-route` guest | yes; it is the 6 ms part |
| runtime-operator, 5 CRDs, washlet lifecycle over the scheduler NATS | Workload CRs → host groups, start/stop/drain, OCI pull | no; control plane only |
| WASI impls, egress and allowed-hosts policy, probes, OCI, OTLP wiring | platform plumbing | indirectly |

The inversion. The operator's model is hosts as a fleet with workloads placed by selector. wamn pins each release to its own host group (`release.hostSelector` → `hostgroup: <release name>`), binds the host to one environment (`--environment`, `WAMN_EVT_ORG/PROJECT/ENV`), and runs one replica of each. Every Workload therefore resolves to the one pod it could only ever resolve to, and the chain Helm → CR → operator → NATS → washlet → OCI pull is a 1:1 mapping a Deployment does with less. The operator earns its place only when a host group serves several environments.

The moves, all with wasmCloud kept:

| Move | What changes | What wasmCloud keeps doing | Depends on |
| --- | --- | --- | --- |
| A5 Application as the HTTP workload (the model fix) | publish composes the route layer into the application component, which exports `wasi:http/handler` beside its operation exports; the request enters the guest once; host responsibilities become imports (auth, permit, `begin-operation`, `commit`); the delivery bridge and `wamn:router-delivery` leave the base path | serving the guest over wasi-http, routing by host, pooling, plugins: the job it was built for | a spike (below); the composition spec's `operation_transaction()` shape, applied to the entry |
| A1 Native base routes (fallback only) | a hyper listener in `services/host` implements `http-route`'s `Backend` trait natively and calls the plugins directly; the guest stays for the edge or as an option | instantiating and pooling the application component, plugins | H1; `http-route` is already an `rlib` generic over `Backend` |
| A2 Shared host groups | a host group serves N environments; `self.release` becomes a map keyed by environment; auth memory, write-ahead scope, event-stream config per environment; host groups by tier, not by release | scheduling workloads onto hosts, which it was built for | a spike: the Postgres plugin is already keyed per component (`require_tenant`, `schema_for`, per-tenant pools); the route plugin and host config are not |
| A3 Taps deleted, NATS off the request path (ruled 2026-10-20) | taps are deleted (nothing reads them; writes are in the write log; a dev body preview, if wanted, is a host flag logging to the local trace file); auth drops, guest registration and the materializer stay on NATS; one cluster if the scheduler and event roles can share | the scheduler NATS for the washlet | H8; ruled, no dependency left |
| A4 One serialization per hop | bytes through bridge → intent → guest; outcome forwarded as bytes | nothing changes | H3; A5 makes most of it moot, because the host no longer sees the payload |

What these conflict with: A1 keeps a non-wasm request path beside the wasm one, which sits oddly with the premise that the application is what wasmCloud serves; A5 conflicts with nothing ruled and extends the 2026-09-24 composition ruling to the entry; A2 changes what a Helm release selects (today one release = one environment = one host group) while keeping `helm upgrade` / `helm rollback` as ruled 2026-10-04. A3 and A4 conflict with nothing ruled.

What dropping wasmCloud would add beyond A1–A4: independence from upstream churn (2.6 → 2.9 → 2.10 broke the per-call shim and renamed seams within two months), a smaller control plane (no operator, CRDs or scheduler NATS), and ownership of the policy code. It would not add throughput that A1–A4 do not. Cost is volume, not difficulty: `host.rs`, `services/edge`, dev activation, the cluster gates, and a trust review of policy code wamn would then own. Not proposed.

A5 is the direction; A1 is kept in the table only as the fallback if the spike finds a serious issue. Order: the A5 spike first; A3 is ruled (taps deleted) and can land on its own; A2 after its own spike; A4 only in the fallback case.

### A5: application as the HTTP workload

wasmCloud's HTTP workload assumes the guest is the application: the request enters once over wasi-http, the guest calls imports for effects, the guest answers. The application component already has that shape when the host invokes it. What breaks the model is the second guest in front and the host orchestrating between the two.

Per get under A5: wasi-http in, one `authenticate` import, one `wamn:postgres` statement, wasi-http out. One fresh Store from the pool, no delivery report, no outcome JSON parsed by the host, no `validate_input` (the guest parses its own contract once).

What moves:

| Today | Under A5 |
| --- | --- |
| `http-route` is its own workload; `deliver` crosses to the host, which dispatches the application | publish composes the route layer (the `http-route` rlib or its generated equivalent) with the application packages into one component exporting `wasi:http/handler` and the operation exports |
| the host opens the transaction before it calls the export, from the admitted operation's declaration | the composed entry calls `begin-operation(sealed-operation-id, idempotency-key)` and `commit`; the host checks the id against the release's admitted facts and the caller's grant, pipelines the claims, writes the write-log claim and finish, emits the causation message, and holds the transaction for the Store's lifetime |
| `intent.rs`, `native_call.rs` interpret the outcome; the bridge builds the delivery report | the guest answers HTTP; the host reads nothing of the payload |
| a routed read's ETag comes from a platform query in the bridge | an import on the Postgres instance returns the WAL position inside the same trip |
| `deliver-stream` carries rows from the application guest to the ingress guest | the guest writes rows to the wasi-http body as they arrive from `run-stream` |
| warm reuse trusts the `http-route` digest | no trusted-warm list; every Store is fresh, which the law already requires for application code |

Invariants A5 must keep (each is already a ruling or a refusal today):

- `begin-operation` and `commit` exist only on the entry's import instance. No package member imports them: "read access but no transaction access" applied to the entry/member split. Publish refuses a member that imports them, and refuses an application that supplies its own `wasi:http/handler` export.
- The transaction is keyed by Store, never by member instance. Base and overlay share one invocation, one connection, one commit, and the overlay still sequences base-exported steps before, between and after its own as component-model calls. A package with its own database (analytics) gets its own connection and transaction, as today. A cross-application call is host-mediated through the router with no shared transaction, as today.
- `run` / `run-stream` stay refused inside a host transaction.
- Authorization stays at publish; the only runtime check is the entry's grant, once, at `begin-operation` or at `authenticate`.
- The composed component's digest is a function of source bytes only; `wac` is deterministic, and the route layer's bytes are pinned like any member.
- The guest supplies no trust data (ruled 2026-10-20 from the spike, `wamn-cbqzl`). The host mints the delivery id at `begin-operation`. `authenticate` returns an opaque caller handle, and `begin-operation` checks it. The request hash and the result are the application's idempotency data, which the host stores without interpreting.

Cost: a contract change across publish, the generator, the Postgres plugin's transaction surface, the edge host, and the flow tests. Weeks, not days. It touches the security model, so it gets the review the composition spec got.

Spike: compose `http-route` into Receiving by hand with `begin-operation` / `commit` stubbed on the Postgres plugin, run get and update c16 locally against the current path. Done in `wamn-cbqzl`: A5 was ruled on its result (decision 1). Its results and its list of shortcuts are in that issue's notes.

### A3 (H8): taps, what they are, who reads them, and the options

A tap is a debugging preview of one request, published by the host onto NATS so that a developer tool can watch an environment without reading its database. Facts from the code:

- What is published. The router-delivery bridge publishes two per delivery: `Accepted` with the request payload before dispatch, and `Settled` with the result and an outcome label after it (`router_delivery.rs:205`, `:254`, `:390`; the workflow path publishes the same two in `wiring_delivery.rs:102`, `:234`). The record is `RouterTapRecord`: delivery id, source, target (route or wiring), phase, outcome, and the payload as JSON (`wamn_jetstream.rs:242`). The payload is scrubbed of secret-looking keys and dropped to null above a 64 KiB ceiling (`wamn_run_state::redaction`, `wamn_jetstream.rs:565-578`).
- Where it goes. Subject `tap.<org>.<project>.<env>.<target>.<delivery>` on the event NATS, into the `WAMN_TAP` JetStream stream: memory storage, one replica, 5-minute max age, 16 messages per subject, 64 MiB cap (`deploy/gcp/nats-jetstream.yaml:161-210`). It is a five-minute window, not a record; a broker restart drops it. Guests may not publish on `tap.*`.
- Cost. Two JSON serializations of the payload per request on the host, two publishes per request, fire-and-forget (`poll_immediate`, `wamn_jetstream.rs:1241`). One known failure mode: without the `WAMN_TAP` stream, pending previews exhaust the publisher queue and stall requests (`benchmark.md`). `upgrade_environment.rs` runs the stream Job at every environment upgrade so the stream exists.
- Who reads it today. One reader exists: the development loop (`wamn dev`) subscribes to the environment's `tap.>` filter, decodes each record, and keeps the newest 256 in memory as `DevTapObservation` (`dev/observations.rs:78-193`, `dev/read.rs:274-361`). Nothing displays them: the only consumer of that snapshot in product code is `services/ctl/src/dev/command.rs:76`, which reads the runtime endpoint and not the taps. The TUI run screen that showed them (`wamn-dggp.10`) was deleted on 2026-10-16 (`wamn-z7j99.1`). The remaining readers are tests (`tests/integration/src/router_tap_live.rs`, `apps/wamn_receiving/tests/postcommit.rs`).
- Who was meant to read it. The "router-edge live view" (`wamn-0h0g.24.5`): a run screen in the studio or loop console that shows a delivery's input and output as it happens, and later per-edge previews inside a workflow walk, which were explicitly not built because they would put a publish on every router step (`wamn_jetstream.rs:406-414`).
- Not related, named wrongly earlier in this doc: "draft-diff" is the loop console's diff of an authored draft against the applied manifest (`wamn-jvzx.5`). It reads manifests, not taps. The loop console's "activity feed for drafts, runs and suites" reads run and suite state, not taps either. Taps serve exactly one thing: a live preview of request and response bodies.

Ruled 2026-10-20: delete taps. Nothing reads them, writes are already in the write log, and a dev-time body preview belongs behind a host flag writing to the local trace file, not a publish on every request. The options table below is kept as the record of what was weighed. Deletion covers: the `tap` calls in `router_delivery.rs` and `wiring_delivery.rs`; `publish_router_tap`, `RouterTapRecord`, `RouterTapPreview` and the `tap.*` subject grammar and guest refusal in `wamn_jetstream.rs`; the `WAMN_TAP` stream ConfigMap and Job in `deploy/gcp/nats-jetstream.yaml` and `deploy/infra/nats-jetstream.yaml`; the tap Job step in `upgrade_environment.rs`; the tap reader and `DevTapObservation` in `dev/observations.rs` and `dev/read.rs`; the tap credentials in `event_broker_files`; `tests/integration/src/router_tap_live.rs` and the tap assertions in `apps/wamn_receiving/tests/postcommit.rs`; the benchmark and gcp docs. Under A5 the dependency this section named disappears with it.

| Option | What the live view gets | What it costs | What it loses |
| --- | --- | --- | --- |
| T1 Keep taps as they are, published by an import the entry calls under A5 | everything: reads and writes, request and result, refusals, timing | 2 serializations + 2 publishes per request on every host; broker storage and retention proportional to traffic; the stall mode stays unless publishing is made shed-on-full | nothing functional; it is the most expensive option per request |
| T2 Taps without payload | activity: operation, caller, outcome label, timing, ids | 2 small publishes per request; no serialization of the body | previews: a write's payload can be fetched from the write log by id; a read's payload is gone, because reads write nothing |
| T3 Full taps only while a watcher is attached | everything, but only when someone is looking: the loop console sets a presence flag (NATS KV or a subscriber count the host polls), the host publishes payload taps while it is set | zero per-request cost in an environment nobody watches; one flag read per request from host memory; a short attach race (the first requests after attach are missed) | nothing in dev; in production, history before attach |
| T4 Write log and CDC as the source | every write with request and result, durably, after commit, through the existing CDC reader | nothing new per request; the rows exist already | reads entirely; refused or rolled-back writes; previews lag by the commit |
| T5 Sampled payload taps (ratio per environment) | a statistical view | proportional to the ratio | any specific request; useless for previews, fine for dashboards |
|  |  |  |  |

The options combine. T2 or T4 as the always-on source and T3 for previews gives the loop console everything it has today and the production path nothing per request. T1 is the only option that preserves today's behaviour exactly, and it is the one the per-request numbers argue against. The decision is which users of the live view need reads: if the loop console must preview a read's body, T3 or T1; if activity suffices, T2 with T4.

What the choice changes elsewhere: under A1 or today's path, T2/T3/T4 are a change in `RouterDeliveryBridge::tap` and the publisher; under A5 they are the policy behind one `tap` import on the entry's instance, and the host decides. The loop console build-out on the horizon is the consumer that should rule this.

### Re-review: wamn's proposed functions against A5 and A2

Each row is a function the platform promises today or has ruled it will have, and whether the redesign handicaps it.

| Function | Under A5 | Under A2 | Handicap? |
| --- | --- | --- | --- |
| Base routes, one component export per route, no router | the designed path | unchanged | no |
| Workflows and wiring attachments through the router (approvals, reporting, dynamic nodes) | a wiring route still calls a host `deliver` import; the router drives operation exports, which the composed component keeps | unchanged | no; the composed guest carries both paths, as the bridge distinguishes Route and Wiring today |
| Cross-application calls (Acme → Receiving) | host-mediated through the router, separate database, no shared transaction | unchanged | no |
| Overlays composed with a base, transaction across both | one Store, one connection, members join the entry's transaction | unchanged | no, if `begin`/`commit` stay entry-only |
| Per-member SQL authority via per-member import instances | unchanged | unchanged | no |
| Write-ahead intent, never-replay, effect-uncertain, park/release | the write-log claim is written at `begin-operation`, before any effect, in the same transaction, as today; a Store drop rolls back | unchanged | no; the guest cannot reach an effect before `begin`, because statements refuse outside a transaction for a declared operation |
| Multi-item requests, one idempotency key per item | the entry calls `begin-operation` per item; the host keeps per-item claims | unchanged | no, but the entry must own the item loop that `intent.rs` owns today |
| Auth in host memory, immediate revocation, 60 s TTL, NATS drops | an import answered from the same memory | memory keyed by environment | no |
| Route concurrency limits, 429 | `try_acquire` import | per-environment limits | no |
| Environment inactive 503, anonymous routes refused writes at publish | import from memory; publish rule unchanged | per-environment status | no |
| Host-run routes (`/wamn_control/...`, platform UI) | the composed entry forwards them to a host import, as `HostRouteDelivery` is reached today | per-environment release | no |
| GET reads, query-string codec, cache headers, ETags, 304 | guest-side today, unchanged; the ETag's WAL position becomes an import on the Postgres instance | unchanged | no |
| Streamed query, `next-cursor`, table pages | simpler: rows go from `run-stream` straight to the wasi-http body; `deliver-stream` goes | unchanged | no, an improvement |
| Taps: the live preview of request and response bodies for a future loop console or studio run screen (no product reader today) | the bridge no longer sees base routes, so taps need a new source: an import the entry calls, or the write log | unchanged | yes unless A3 decides the source first; this is the one dependency |
| CDC reader, causation message, record history | emitted by the host at `commit`, as today | unchanged | no |
| Edge tier: SQLite run-state, bundle of digests, local keys | the edge host implements the same imports over the run-state crate's SQLite adapter; it already runs `http-route` as its ingress, so it runs the composed component instead | not applicable | no; closer to one binary |
| Cron as a workflow trigger, durable execution tail | router-driven operation exports, unchanged | unchanged | no |
| Package upgrade, qualify-release gating active workflows against the candidate | operation exports unchanged; the composed component's digest changes when the route layer changes, so a platform route-layer change re-qualifies every application | per-environment releases on a shared host; rollout is per release, not per pod | no handicap; one new re-qualification trigger to name |
| Generated TypeScript client, control client, route contracts | same HTTP surface | same | no |
| Observability: 22 spans per request, `wamn.api.requests` counter at the host HTTP seam | the host still owns the HTTP server; guest spans via `wasi:otel`; fewer spans | per-environment attributes | no |
| Studio and platform UI as applications dogfooding the platform | unchanged | unchanged | no |
| Memory cap 256 MiB per linear memory, epoch deadline, pooling budget per host group | unchanged | the budget is shared by N environments on one host; noisy-neighbour risk is bounded by per-memory caps and per-environment in-flight limits | A2 needs per-environment in-flight limits, which exist as `WAMN_HTTP_ROUTE_IN_FLIGHT_LIMIT` per host today |
| Event streams per environment (`WAMN_EVT_*`) | unchanged | become per-release configuration instead of host environment variables | A2 change, not a handicap |
| Helm release per environment, `helm rollback` | unchanged | a release is still per environment; the host group it selects is shared | no |

No function on the list is lost under A5 or A2. Batched requests: a request can carry several items, each with its own idempotency key, and today the host loops over them (`intent.rs`: per item, write-log claim, run, finish). Under A5 the component handles the array: the entry loops over the items and calls `begin-operation` and `commit` per item, and the host sees single operations. Resolved as decision 8.

## Open decisions for the owner

1. Ruled 2026-10-20: A5, the application as the HTTP workload. The spike (`wamn-cbqzl`) found no blocker. It cut host CPU per request by about 20% on get and update, with the same Postgres work.
2. Ruled 2026-10-20: taps are deleted (A3, T-none). Nothing reads them; writes are already recorded in the write log; dev request/response bodies, if wanted, come from a host flag that logs to the local trace file, never a hardwired publish. Event NATS stays for auth drops, guest registration and the materializer.
3. Triggers generated per relation (P1) now, or history written by the generated statement (P8) once overlay composition settles? In flight: epic wamn-c8a5v does P1 and P3 (issue .2) scoped by the GCP trigger measurement (issue .1, M4 and M5); P8 is not in the epic and waits on the overlay composition spec.
4. Does an `update`'s stored `result` need the full row (P4), or is the row itself the authoritative outcome for a replay-free retry?
5. Is `commit_delay` acceptable as a cluster parameter (P7), given it trades p50 for throughput? Owner 2026-10-20: not decidable without measurement; M6 runs in wamn-c8a5v.3 with the other Postgres tests, decision after.
6. mimalloc stays deferred (`wamn-9jvm1.1`)?
7. Should a host group serve several environments (A2)? If yes, the operator earns its place; if no, it is a 1:1 mapping and a Deployment would do.
8. Resolved 2026-10-20: under A5 the component handles a batched request. The entry loops over the items and calls `begin-operation` and `commit` per item; the host sees single operations and never knows a batch exists. Per-item claims and transactions stay as today (an item failing does not roll back the others; a retry replays only unfinished keys). Whole-batch atomicity, if an operation wants it, is one begin with one key, declared by that operation's contract. Owner: batch semantics and error handling belong to the application, not the host; the response shapes (per-item errors in a 200, partial completion on a run failure) stay as the entry builds them today, and validate-all-before-run is kept.
9. Which of M1–M8 and the two spikes run first. Suggested: the A5 spike, M4, M5.
