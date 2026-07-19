# `wamn:node` Contract — Design Notes (v0.1 — FROZEN)

> **§1.9a audit (2026-07-19): amendments are additive — base sound.**

Companion to `wamn-node.wit`. Load-bearing for runner dispatch (5.2/5.6), trace propagation (9.2), and testability (11.5). Draft 2 resolved the four open questions from draft 1.

**FROZEN 0.1.0 (2026-07-12, plan 5.4).** Three deltas the 5.3 standard library
surfaced were folded in before the freeze, so the WIT and its Rust-native
mirror (`crates/wamn-node-sdk`) coincide from day one:

1. **`run` returns an `emission` record `{payload, port: option<string>}`**
   (absent = `main`) — the engine routes ported edges (branch nodes emit
   `"true"`/`"false"`); a bare payload could not express that. The reserved
   `error` port is never emitted: errors travel as `node-error`.
2. **`traceparent` is `option<string>`** — the host tracing plumbing (9.2) is
   not wired yet; a required field every runner must fabricate would freeze a
   lie. Present once a trace is active; SDKs propagate it invisibly.
3. **`rate-limit-detail` gained `target-host: option<string>`** — the shared
   throttle is keyed by (node type, credential, target host), and for a custom
   node's own outbound calls the error is the only carrier of that host.

Compatibility: 0.1.x is additive/clarifying only; breaking changes wait for
0.2 (first candidate: the WASI 0.3 native-async revision, 5.16). The optional
imports are frozen but have NO host implementations yet — `payloads` (5.10),
`credentials` (5.9), `control` (5.12); linking one fails instantiation until
its host side lands. Vendored copies of the WIT (three S4 guests, the
`wamn-node-guest` scaffolding, the host bindgen copy) are drift-guarded by
`crates/wamn-node-sdk/tests/wit_coherence.rs`, which also pins the exact WIT
lines the SDK mirrors. The `nodebench` gate proves the ABI cross-language
(Rust + JS/JCO + wac-composed) and drives the scaffolding-built sample node
through every taxonomy variant, port selection, and the streamed refusal.

## Design decisions and rationale

**1. Payloads: `inline(json)` | `streamed(payload-ref)` — baked into 0.1.**
Changing `run`'s signature later is exactly the breaking change versioning exists to avoid, so the variant ships in 0.1 even though the streaming backend can land after the inline path. Streaming is a *record stream* (NDJSON framing), not "one giant JSON document" — that's what analytics workloads actually are, and stream order is inherently write-order, which matters below. The payload store is run-scoped and content-addressed (backed by object storage / host-local spill); refs are invalid outside their run, so payloads can't leak across tenants or runs by construction.

**Limits are a first-class platform primitive:** global inline cap (default 4 MiB) and streamed cap (default 1 GiB), each overridable per-flow within plan quotas (Epic 10.2), enforced at the host on write (`limit-exceeded` carries the byte count), metered into billing (9.11). Run history captures inline payloads fully; streamed payloads as head-preview + size + hash — which also keeps fixture generation (11.3) bounded: a fixture references a stored payload snapshot, not an unbounded blob in Postgres.

**2. No `run-batch` — streaming dissolved it.**
Batch existed to amortize per-invocation overhead; a record stream through one `run` invocation amortizes identically *and* gives in-order processing for free. Ordering is therefore an orchestration policy, not a contract concern. Per-node in flow config:

| Policy | Semantics | Runner behavior |
|---|---|---|
| `strict` | Total order | One in-flight execution per node |
| `partitioned(key)` | Order per key, parallel across keys | Hash key expr → per-partition serial dispatch (the Kafka model; right default for tag/asset data) |
| `unordered` | None | Free parallelism up to concurrency limit |

The node stays a pure function under all three; only the runner's dispatch changes. This also means frozen flows (5.10) inherit ordering semantics unchanged.

**3. Cancellation: one operation, many initiators, two layers.**
Control-plane API: `cancel(run-id, reason, [node-scope])` — invoked identically by the user (editor stop), platform policy (quota breach, misbehavior), scheduler (maintenance window, run TTL), or the runner itself (sibling branch failed). Reason propagates end-to-end: API → run state → `control.cancelled()` → run history → audit log.
- **Hard layer (always available):** Wasmtime epoch interruption kills any instance with zero cooperation. This is the platform's guarantee against poorly-behaved nodes — no contract linkage required.
- **Cooperative layer (optional import):** nodes poll `control.cancelled()` at checkpoints to abort in-flight external calls cleanly and return `node-error::cancelled`. SDKs make this invisible (TS: `ctx.signal` as an `AbortSignal` wired to the poll; Rust: checked inside the SDK HTTP client and stream iterators).
- **Semantics documented honestly:** `cancelled` is a distinct terminal run status, not a failure; error branches don't fire. Cancellation does NOT roll back external side effects — redelivery safety is already the job of `attempt` + `idempotency-key`.

**4. Config is a JSON document.**
`config: json`, validated against the node's config JSON Schema (from the OCI manifest) *before* dispatch — nodes can assume shape-valid config. Secret-typed fields are replaced with credential handles by the runner.

**5. No secrets in `run-context` — lazy `credentials.get(handle)`.**
(a) *Platform-managed* credentials are structurally absent from captured inputs (run history 9.6, fixtures 11.3) — but note the honest limit: user data flowing through nodes can itself contain secrets (a flow reading a table of API keys), so collector-level pattern scrubbing remains load-bearing, not redundant; (b) each credential access is a discrete audit event with (run-id, node-id, handle); (c) the test host swaps in a fixture-backed implementation with zero node changes.

**6. Error taxonomy is part of the contract.**
`retryable` / `rate-limited` / `terminal` / `invalid-input` / `cancelled` drives runner behavior mechanically — no string matching. `invalid-input` never burns retry budget (it's an upstream bug); `cancelled` routes to cancel status, not the error branch. `rate-limited` (added after external review) carries an optional source-authoritative `retry-after-ms` and triggers a **shared runner throttle keyed by (node type, credential handle, target host)**: all parallel executions against the same limited system back off together — no retry stampede against a struggling ERP/MES — while unrelated flows proceed. Deliberately NOT global queue backpressure; one throttled upstream must not stall the platform.

**7. Capability grants are derived, not declared twice.**
The builder reads the component's actual WIT imports and emits `hostInterfaces` (prompting for `allowedHosts` when `wasi:http/outgoing-handler` appears). World `node` imports nothing → empty grants → physically incapable of I/O. `payloads` and `control` are grants like any other.

**8. Node metadata lives in OCI annotations, not a WIT export.**
Display name, config JSON Schema, input/output schemas, declared ordering-policy support → `wamn.node.manifest` annotation. Registry-scannable node palette; no instantiation to browse. **SHIPPED (5.4):** `crates/wamn-node-manifest` is the annotation's canonical model (types, validation, `ANNOTATION_KEY`), with the language-neutral contract generated from it at `docs/wamn-node-manifest.schema.json` (the wamn-flow pattern: fixture round-trip + boon conformance + drift guard). Declared output ports ride along (edge affordances for the editor; `error` is reserved and rejected). Capability grants are deliberately NOT in the manifest — note 7: derived from actual imports, never declared twice. The builder (5.5) writes the annotation at push.

**9a. Trace propagation is host-enforced, not convention.**
Nodes SHOULD propagate `traceparent` (SDKs do it invisibly in their HTTP/DB helpers), but the guarantee doesn't depend on it: every outbound call already traverses a host plugin (`wasi:http/outgoing-handler`, `wamn:postgres`), and the host stamps trace context onto any outgoing request that lacks it, using the executing node's span. A user bypassing the SDK cannot break trace continuity — the capability boundary doubles as the telemetry boundary. SDK-set context is preferred (more precise parent spans); host injection is the floor.

**9b. Config parse cost: memoize + constant-fold, keep strict JSON.**
First, a clarification three external reviews have tripped on: **standard library nodes never touch the JSON codec.** They are compiled into the runner — same binary, no WIT boundary — and the runner parses flow config once at flow load (hot reload) into typed structs in the in-memory plan; dispatch passes references. The `json` config crosses a boundary only for dynamically loaded custom components, where it is the price of language neutrality. There, config is immutable per `(flow-version, node-id)` — both already in `run-context` — so SDKs parse once and memoize across invocations in a warm instance. In frozen flows, config is known at composition time and the `wac` pipeline constant-folds it into pre-parsed native structs: parse cost is zero exactly on the compute-bound hot paths where it would matter. Cold-start parse share was benchmark-gated in P0 (S4, 2026-07-10): config JSON parse measured ~6% of the *tightest* cold dispatch (~1.2 µs parse vs ~19 µs pooled instantiate+run) — marginally over the 5% line, so the memoize + constant-fold mitigation above **stays** (decision recorded, benign: exposure is bounded and the share is ≪1% once component load/compile is counted). See `docs/p0-results.md` S4. Schema-validated JSON stays; no contract loosening.

**9. Determinism rules (testability contract).**
Time only via `wasi:clocks`, randomness only via `wasi:random` — so the test host can virtualize time (24h delay resolves instantly) and seed randomness. Streamed-payload reads are deterministic given the fixture store. Builder lints imports against the allowlist. **Specified at the 5.4 freeze:** the custom-node import allowlist v1 is `wamn:node/{payloads,credentials,control}` (inert until 5.10/5.9/5.12), `wasi:clocks`, `wasi:random`, `wasi:io`, the `wasi:cli`/`wasi:filesystem` std shims, and `wasi:http/outgoing-handler` (which prompts for `allowedHosts`); `wasi:sockets` is forbidden outright (the 2.6 DB-path boundary). The MECHANICAL lint lands with the 5.5 builder — `egressbench` (2.6) already does host-side artifact import classification and is the extension point / publish-gate backstop.

## Versioning policy

- `wamn:node@0.x` pre-GA; breaking changes bump minor pre-1.0, major after. Runner supports current + previous major, instantiating against the version in the node's OCI manifest.
- Additive evolution (new optional imports — e.g., a future `emit` interface for mid-run progress events on long streams) ships as new interface versions; compiled nodes keep working.
- The dual-target standard library (5.10) tracks head and is the canary for every change.

## Guest ergonomics

TypeScript (SDK hides all WIT):
```ts
import { defineNode } from "@wamn/node-sdk";

export default defineNode({
  async run(ctx, input) {
    // Streamed input: async iterator of records, in write order.
    let anomalies = ctx.output("ndjson");
    for await (const reading of input.records()) {
      ctx.signal.throwIfAborted();            // cooperative cancellation
      if (reading.value > ctx.config.threshold)
        await anomalies.write({ ...reading, flagged: true });
    }
    return anomalies;                          // -> streamed(payload-ref)
  },
});
```

Rust:
```rust
#[wamn_node]
fn run(ctx: RunContext, input: Payload) -> Result<Payload, NodeError> {
    let threshold = ctx.config()["threshold"].as_f64().unwrap();
    let mut out = ctx.create_output(Framing::Ndjson)?;
    for record in input.records()? {           // ordered; polls control internally
        let r: Reading = record?;
        if r.value > threshold { out.write(&r.flag())?; }
    }
    Ok(out.into_payload())
}
```

SDKs own: binding generation, payload inline/stream duality (same iterator API either way), JSON codec, error variants, traceparent propagation, deadline plumbing, cancellation polling inside iterators and HTTP clients.

**Shipped at 5.4 (Rust):** `crates/wamn-node-guest` — a custom node implements
the SAME `wamn_node_sdk::Node` trait the standard library uses, and
`wamn_node_guest::export_node!(MyNode)` is the entire componentization
(binding generation, JSON codec, taxonomy + port + run-context conversion,
streamed-payload refusal until 5.10). `components/samples/sample-node` is the
reference node and conformance fixture; the `#[wamn_node]` macro sketch above
is superseded by the trait + macro pair. The TS `defineNode` SDK is deferred
to the builder (5.5) / POC-F2 — the S4 `node-ts` fixture already proves the
jco path against the frozen ABI.

## Remaining open items (non-blocking for 0.1)

1. **Mid-run progress events** for long stream jobs (UI progress bars) — future `emit` import, additive.
2. **Payload store backend** — object storage vs host-local spill with TTL; pick during 5.11 implementation, invisible to the contract.
3. **WASI 0.3 async migration** — native `stream<T>`/futures would replace the poll-based cancellation and resource streams; plan as a 0.2/1.0 contract revision once Wasmtime 46+ behavior is proven in our hosts.
