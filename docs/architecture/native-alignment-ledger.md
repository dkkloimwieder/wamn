# Native-alignment ledger — wamn vs wasmCloud

Principle (ratified): **native capability first — a wamn deviation must name
what it buys and where it re-converges.** Pin: fork `wamn/2.8.0` @ `5c4ec4a3`
(zero commits over upstream `v2.8.0`). The tag includes `a5e7d5a`, upstream's
instance-pool panic/poison fix. Sync cadence: per upstream minor, with a shrink
audit each sync.

## Part 1 — Platform-model deviations

| # | Deviation (wamn vs native) | What it buys | Disposition / re-convergence trigger |
|---|---|---|---|
| 1 | `wamn:postgres` capability seam — guests never hold sockets or credentials (native: guest sockets + allowed-hosts) | Host-injected credential generations, RLS identity set by trusted code, span per effect. This is the product. | **Keep.** Implementation rides `component-model-async`; never re-converges. |
| 2 | Publish-time import allowlist, 4 elements (native: runtime non-satisfaction of ungranted caps) | Auditable tenant contract; refusal before distribution, not at wiring. | **Keep** — additive over native model, composes cleanly. |
| 3 | Wirings as gated tenant rows + hot pointer flip (native: lattice links / wadm manifests / CRDs) | Tenancy, minutes-scale user churn, gate + rollback + provenance. | **Keep (ruled).** Trigger to revisit: upstream ships a multi-tenant, per-link-authz link store. |
| 4 | Wasmtime's pooling **allocator is on**, while the router still constructs and drops a fresh store per invocation; wash-runtime's warm `InstancePool` dispatch remains unwired. | Fast allocator-backed fresh instances without guest state surviving a call. Allocator capacity and store reuse are separate controls. | **Keep the allocator; keep reuse off.** Admission rejects `poolSize > 0`, so every workload stays `InstancePolicy::Ephemeral`. The `.17.x` router/native-dispatch wiring remains open only if it preserves that fresh-store rule. Revisit warm reuse with explicit affinity/windowed state; `a5e7d5a` is already present for that future step. |
| 5 | Guest builds: `no_std` + second workspace (native: std, `wasm32-wasip2`, one build unit per component, `wash build`) | The 4-element allowlist (row 2) stays closed with **zero exemption machinery**. Linking std adds 14 imports per guest, 10 of them `wasi:cli`; the imports come from std itself, so **no build-unit shape avoids them** — per-component invocation and `panic = "abort"` both leave the import surface identical (measured, `wamn-1yj4` + wave 69). | **Keep — deviation kept, amended 2026-08-25.** Superseded reading: *"Bend to native — `.11.56` resolves as std + per-component invocation; `no_std` and the split workspace retire."* That named the buy but not the constraint. The only paths to std are widening the tenant allowlist (row 2 says keep it closed; irreversible once components ship) or a first-party exemption lane (new admission machinery — refused). **Re-convergence trigger:** upstream/wasi ships std without ambient `wasi:cli` imports, or a std build profile that drops them — then fold back per the superseded text. |
| 6 | Our `tools/build-components` instead of `wash build` | Palette publish integration (digest, admission). | **Watch.** Re-converge when `wash build` + OCI push covers admission hooks; not blocking. |
| 7 | Already aligned (no deviation): OCI distribution + digest pinning, `implements`/maps bindings for connection requirements, runtime-operator CRDs, HPA scaling, wasip2 + P3 + component-model-async, JetStream at-least-once ack-after-process. | — | Cited in exe-model; keep tracking upstream defaults each sync. |

## Part 2 — Fork ledger (`wamn/2.8.0` @ `5c4ec4a3`)

**Process law:** every fork patch carries a bead id under the fork's current
retarget epic, a row here, and an exit condition. No un-ledgered patches. The
current branch has **zero** patches: its tip is the peeled upstream `v2.8.0` tag.

| Former patch | v2.8 disposition | Native or WAMN-owned replacement |
|---|---|---|
| g2br.2 — epoch deadline policy | **Dropped** | Vanilla enables epoch interruption, starts its ticker, and hardens abandoned calls. WAMN starts no ticker; its manual router stores still set invocation deadlines. |
| g2br.3 + g2br.7 — per-store memory limiter and accessors | **Dropped** | Vanilla `HostMemoryBudgets` carries the host total, per-memory heap ceiling, and allocator instance count. WAMN needs one fixed heap ceiling, not a fork-level per-component policy system. |
| g2br.4 — outbound `wasi:http` trace injection | **Dropped** | WAMN's real outbound-effect path, `wamn:connection/http`, injects the active span context itself. |
| g2br.5 + g2br.6 — raw TCP/UDP denial | **Dropped** | Vanilla's centralized `SocketPolicy` is installed with `EgressMode::Enforce`; WAMN admission also rejects tenant `wasi:sockets` imports. |
| g2br.15 — plugin raw-socket opt-in | **Dropped** | `wamn.allow-raw-sockets` and `WAMN_ALLOW_RAW_SOCKETS` are retired. Every guest inherits the same host socket policy. Vanilla defaults deny special/link-local/metadata ranges and allow private ranges. |
| g2br.8 — `wamn.api.requests` counter | **Dropped; series retired** | No current metricbench or dashboard consumes the series. `wamn.router.delivery.attempts` and `.errors` are the live router-owned metrics; they deliberately do not claim the old HTTP `status_class` semantics. |
| g2br.12 — surface terminal P3 service failures | **Dropped** | WAMN has no `workload_status` consumer and admission excludes P3 service WorkloadDeployments. Reconsider only when a real consumer requires terminal service status. |
| g2br.14 — clear HTTP egress state on every stop | **Dropped** | WAMN outbound HTTP uses its invocation-scoped `ConnectionHttp` plugin, not vanilla pooled `wasi:http` egress. Shipped flow HTTP imports WAMN routing/invocation rather than `wasi:http/outgoing-handler`. |
| g2br.16 — reusable-instance kill switch | **Dropped** | Workload generation/admission enforces `poolSize = 0`; no host environment switch duplicates a manifest value WAMN controls. This does not disable Wasmtime's pooling allocator. |
| g2br.13, .17, .18, .19 — rustdoc, formatting, Git fixture isolation | **Dropped** | WAMN does not modify upstream for local maintenance gates. `tools/fork-sync-check` runs the upstream fixture with global/system Git configuration isolated. |

The g2br.8 consumer audit found no metricbench, dashboard, alert, or code
consumer of `wamn.api.requests`. Exact relocation is also not available through
the vanilla v2.8 public API: `Router` runs before dispatch and cannot observe
the final response status, while the status-recording choke point inside
`Ingress` is private. Deriving a similarly named series from sampled INFO spans
would undercount and would not preserve the counter's contract. WAMN therefore
keeps its existing router-owned attempt/error metrics and retires the unused
series instead of carrying a patch or manufacturing an approximate metric. Per
owner directive, no upstream issue or PR is filed.

The v2.8 async-messaging interface is also taken exactly as upstream ships it:
`wasmcloud:messaging@0.3.0`, streaming bodies, in-flight limits, and 0.2
compatibility require no fork integration. WAMN's JetStream capability remains
a separate WAMN-owned interface.

Two address-policy subjects remain deliberately separate:

- `wamn-d0w4` is the Phase-B posture item for a future deliberately admitted
  raw-socket guest. It does not govern `wasi:http` or `ConnectionHttp`, and it
  must land before any such guest is admitted.
- `ConnectionHttp` currently delegates address confinement to its internal
  `ExternallyEnforcedNetworkPolicy`. Whether that plugin needs an in-process
  address-range check is a separate WAMN security concern; vanilla
  `SocketPolicy` does not cover it and this fork sync does not decide it.

## Standing triggers

1. **Each upstream minor:** start from the peeled tag, run behavior parity in WAMN, and keep the branch at zero patches unless a red, consumed behavior proves a patch is required.
2. **Router/native pool wiring (`.17.x`):** preserve `InstancePolicy::Ephemeral`; allocator adoption is already complete, warm-store reuse is not.
3. **Raw-socket admission:** resolve `wamn-d0w4` before the first guest is allowed to import `wasi:sockets`.
4. **ConnectionHttp address policy:** adjudicate independently of vanilla raw-socket defaults if the external enforcement boundary changes.
5. **CA accessor:** dropped (zero call sites); returns only as ordinary sync work if a consumer appears.
6. **Epoch cadence (`wamn-6evd`):** every tagged-release sync re-runs `the_manual_store_epoch_tick_still_mirrors_the_runtime_ticker` (`tests/conformance/src/runtime_inventory.rs`). It pins the VALUE of wash-runtime's `pub(crate) EPOCH_TICK` against `MANUAL_STORE_EPOCH_TICK` in `crates/execution/host/src/router_driver.rs`; an unmirrored upstream retune rescales **every node deadline** silently, because both halves still compile. This is one of the two source reads `wamn-hopk` R5 exempts as identity pins. **Re-converge:** when upstream makes the constant public, import it, compare it directly, and delete the scan.
7. **New deviation rule:** any proposed fork patch or model deviation lands with its ledger row in the same change, or it refuses.
8. **Row-5 lineage (why the rule exists).** Row 5 was written from upstream's *posture* — std, per-component builds — without re-deriving our *constraint*. It named what native buys but not what the deviation was holding, and the disposition it reached was mechanically incompatible with row 2 of this same document. A measurement caught it, not a review: wave 69 rebuilt both guests without `no_std` and read the actual import surface. Resolved in row 2's favour — **the allowlist is the contract; the build shape serves it.** Any future row asserting "nothing security-relevant" must cite the measurement that establishes it.
