# Native-alignment ledger — wamn vs wasmCloud

Principle (ratified): **native capability first — a wamn deviation must name
what it buys and where it re-converges.** Pin: fork `wamn/2.7.0` @ `daba602`
(16 commits over upstream `v2.7.0`). Upstream `main` has moved since the pin;
notably `a5e7d5a` fixes instance-pool panic/poison under concurrent load —
directly relevant to our pool adoption. Sync cadence: per upstream minor,
with a shrink audit each sync.

## Part 1 — Platform-model deviations

| # | Deviation (wamn vs native) | What it buys | Disposition / re-convergence trigger |
|---|---|---|---|
| 1 | `wamn:postgres` capability seam — guests never hold sockets or credentials (native: guest sockets + allowed-hosts) | Host-injected credential generations, RLS identity set by trusted code, span per effect. This is the product. | **Keep.** Implementation rides `component-model-async`; never re-converges. |
| 2 | Publish-time import allowlist, 4 elements (native: runtime non-satisfaction of ungranted caps) | Auditable tenant contract; refusal before distribution, not at wiring. | **Keep** — additive over native model, composes cleanly. |
| 3 | Wirings as gated tenant rows + hot pointer flip (native: lattice links / wadm manifests / CRDs) | Tenancy, minutes-scale user churn, gate + rollback + provenance. | **Keep (ruled).** Trigger to revisit: upstream ships a multi-tenant, per-link-authz link store. |
| 4 | Instance pooling **currently off** — per-invocation instantiate/destroy at tip (native: `InstancePool`, warm instances) | Nothing — this is drift, not a decision; per-digest pool work (`.17.x`) landed but is unwired at the router driver. | **Bend to native now.** Adopt upstream pool via the `.17.x` wiring; upstream `a5e7d5a` is the sync argument. Reuse/affinity stays off until windowed state (ruled). |
| 5 | Guest builds: `no_std` + second workspace (native: std, `wasm32-wasip2`, one build unit per component, `wash build`) | Nothing security-relevant (allowlist is the boundary; `wamn-mrfr` proved env isolation). | **Bend to native** — `.11.56` resolves as std + per-component invocation; `no_std` and the split workspace retire. |
| 6 | Our `tools/build-components` instead of `wash build` | Palette publish integration (digest, admission). | **Watch.** Re-converge when `wash build` + OCI push covers admission hooks; not blocking. |
| 7 | Already aligned (no deviation): OCI distribution + digest pinning, `implements`/maps bindings for connection requirements, runtime-operator CRDs, HPA scaling, wasip2 + P3 + component-model-async, JetStream at-least-once ack-after-process. | — | Cited in exe-model; keep tracking upstream defaults each sync. |

## Part 2 — Fork ledger (g2br series @ `daba602`)

**Process law:** every fork patch carries a `wamn-g2br.N` id, a row here, and
an upstream disposition — `upstreamable` | `fork-only` | `drop-on-parity`.
No un-ledgered patches.

| Patch | What / why | Disposition |
|---|---|---|
| g2br.2 epoch deadline policy | Restore deny-by-default runaway-guest bound upstream loosened. | fork-only (policy); re-check each sync |
| g2br.3 memory limiter policy | Same class — restore memory bound. | fork-only; re-check each sync |
| g2br.5 raw TCP denial · g2br.6 raw UDP denial | Deny guest raw sockets without explicit opt-in; upstream default is permissive-with-allowed-hosts. | **upstreamable** — propose at 2.8 sync (deny-without-opt-in is defensible upstream) |
| g2br.15 plugin raw-socket opt-in gate | Plugins must opt in to raw sockets; closes the E13 hole where the runtime wires sockets unconditionally. | **upstreamable** — same proposal set |
| g2br.14 drop egress state on workload stop | Correctness: stopped workloads leaked egress state. | **upstreamable** (bug-fix class) |
| g2br.4 outbound trace injection | OTel-as-record needed traceparent on egress before upstream had propagation. Upstream 2.3+ ships cross-host propagation + exporters. | **drop-on-parity** — shrink audit at 2.8 sync; thin or delete |
| g2br.7 limiter accessors · g2br.8 inbound API request counts | Observability accessors the platform reads. | fork-only unless upstream grows equivalents; re-check |
| g2br.12 surface P3 service failures | P3-era failures were swallowed; we surface them typed. | **upstreamable** candidate |
| g2br.16 per-run isolation kill-switch | Superseded in granularity: ruling `.17.4(ii)` puts the per-capability clamp wamn-side; fork mechanism retained but governs the fork path only. | fork-only, documented as such; re-evaluate at pool adoption |
| g2br.13 / .17 / .18 rustdoc + fmt · g2br.19 fixture isolation | Fork-maintenance hygiene; zero semantic delta. | fork-only, trivial rebase cost |

## Standing triggers
1. **2.8 sync:** trace-seam shrink audit (g2br.4), upstream proposals (g2br.5/6/14/15, maybe .12), re-check .2/.3/.7/.8 against upstream deltas, pick up `a5e7d5a`.
2. **Pool adoption (`.17.x` wiring):** re-evaluate g2br.16 vs the wamn-side clamp.
3. **CA accessor:** dropped (zero call sites); returns only as ordinary sync work if a consumer appears.
4. **New deviation rule:** any proposed fork patch or model deviation lands with its ledger row in the same change, or it refuses.
