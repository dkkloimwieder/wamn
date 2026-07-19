# wasmCloud utilization — Posture B (k8s-native runtime, standard interfaces, lattice deferred)

## Status

**Decided 2026-07-16 (dkk).** Posture **B** chosen; Posture **C** (the wasmCloud lattice /
wRPC / wadm distributed model) is deferred behind an explicit trigger. This record
makes explicit a stance that until now lived implicitly across D5, D7, and the
fork ledger. Decision-table row **D17** added to `docs/platform-plan.md` (wamn-8df.1, 2026-07-16).

## Context — how much of wasmCloud we actually use

wasmCloud is two separable things:

1. A **component runtime** — wasmtime + the WASI 0.2 component model, capability
   security through WIT worlds, per-component sandboxing. This is the valuable
   part and we use it fully (via the `wash-runtime` washlet embedding).
2. A **distributed lattice** — capability *providers* reached over **wRPC**,
   location-transparent component-to-component invocation over NATS/wRPC, and
   **wadm** managing declarative OAM applications to desired state across the
   lattice.

We use (1) fully and (2) **not at all** on the data path — Kubernetes does the
distribution instead. That is a deliberate division of labour, not an oversight.

Three postures were considered:

- **A — Stay k8s-native, formalized.** Change nothing mechanically; record the stance.
- **B — Keep the k8s-native runtime; adopt the ecosystem's *interface* surface where
  it fits; defer the lattice.** ← chosen.
- **C — Move onto the lattice** (wRPC providers + wadm links + NATS as a data plane).
  Buys multi-region/edge/independent-provider-scaling; costs a rewrite, an added
  network hop on every call, lattice operations, and a harder multi-tenant hardening
  problem. Deferred — no product driver today.

## Decision

**Posture B.** Concretely:

- Keep the **k8s-native runtime**: capabilities are in-process host plugins;
  component invocation is HTTP + Kubernetes `Service` (and host→component
  typed-function calls); scheduling is the runtime-operator's `WorkloadDeployment`.
- **Prefer ecosystem-standard WIT interfaces; justify every custom exception** in
  this document.
- **Defer the lattice** (wRPC providers, wadm/OAM, NATS as a data plane) behind an
  explicit trigger, and keep our WITs standard so that migration stays per-capability
  rather than a rewrite.

## Current state (grounded)

| Concern | Mechanism today | Ecosystem standard | Verdict under B |
|---|---|---|---|
| Component runtime | `wash-runtime` washlet (wasmtime, WASI 0.2) | wasmCloud runtime | **Using fully** |
| Capability access | in-process host plugins via the linker | wRPC providers (emerging) | **Keep co-located** (latency; the S2 CFS lesson) |
| Component invocation | `wasi:http` in/out + k8s `Service`; host→component typed-func | wRPC over the lattice | **Keep k8s-native** (D7; 33µs hop) |
| Deployment scheduling | runtime-operator `WorkloadDeployment` (`runtime.wasmcloud.dev/v1alpha1`) | **this *is* the k8s-native standard** | **Already correct** — control plane generates per-project-env instances |
| Higher-level app model | none | wadm / OAM `Application` | **Deferred to C** (lattice-oriented; tied to wasmcloud-operator, not runtime-operator) |
| Control plane / wake | operator NATS + dispatcher doorbell hint | NATS lattice | **Control only, not a data path** — unchanged |
| Event capture (D19 v3, 2026-07-18) | **native CDC reader** (dispatcher-family service: pg_walstream logical decoding → JetStream) holding **replication credentials — a privilege tier above query credentials** (see the R8b role-scoping in `docs/archive/review-findings.md`) | — (no ecosystem equivalent) | **Native by design** — a long-lived replication session owning a slot does not fit the per-invocation component model; a parser-only wasm decoder is a future seam (event-plane v3 §6), not planned |

## Interface policy — prefer standard, justify exceptions

| Interface | Standard? | Decision |
|---|---|---|
| `wasi:http`, `wasi:clocks`, `wasi:io`, `wasi:cli` | yes | Keep. |
| `wasi:logging/logging` | yes (draft) | Keep the standard interface; the host plugin adds tenant/flow enrichment. |
| `wamn:postgres` | **custom** | **Keep — justified:** exact-decimal `sql-value` (no float, a hard POC requirement); host-injected, non-spoofable `tenant`/`project`/`schema`/`runner` claims; `pg-error`→node-retry taxonomy. Align field shapes with the official `wasmcloud:postgres` where they overlap; revisit if that interface gains exact-decimal + claim injection. |
| `wamn:credentials`, `wamn:node` | **custom** | **Keep — justified:** domain-specific (credential vault seam; the frozen flow-node contract). No ecosystem equivalent. |
| Future key-value / blob storage | — | **Default to `wasi:keyvalue` / `wasi:blobstore`.** Do not roll a custom interface without clearing the `wamn:postgres` bar (a documented, load-bearing differentiator). |

## Hosting & invocation

- Capabilities stay **in-process host plugins**. Co-location is load-bearing for the
  DB path (removing the CPU limit took p99 from ~40ms to ~2ms — the S2 CFS lesson);
  a wRPC provider would add a hop we do not want there.
- Component invocation stays **HTTP/`Service` + host typed-func**. No lattice data path.
- `hostInterfaces` on the `WorkloadDeployment` remains the **allowlist** for host
  plugins (WASI built-ins bypass it); a workload that imports `wamn:postgres` must
  declare it or fail to instantiate.

## Deployment surface

- **Keep `WorkloadDeployment`.** It is the runtime-operator's first-party,
  Kubernetes-native resource — the correct surface for B, not a bespoke CRD. The
  control plane **generates per-project-env instances** of it (exactly as it already
  generates CNPG `Cluster` CRs in `provision-org`).
- **Do not adopt wadm/OAM now.** wadm is a lattice desired-state orchestrator; its
  `Application` model belongs to the C migration, not B. (`wit2wadm` — WIT→manifest
  generation — is noted as a C-era tool.)

## Fork policy

The three carried `wash-runtime` commits (epoch interruption, per-component memory
limiter, outbound trace-context injection) are **custom exceptions under the same
"prefer standard" rule** applied to code. Each has an upstream exit condition in
`docs/wash-runtime-fork.md`. Policy (dkk, 2026-07-16): **do not spend effort
upstreaming** — carrying three commits is cheap, and architectural work takes
priority over cleanup. Keep the ledger discipline (review at each sync; escalation
threshold ~4–5 carried commits) so the fork does not silently grow, but treat
upstreaming itself as deferred cleanup, not active work.

## Explicitly deferred — Posture C (the lattice)

**Trigger to reopen:** the first concrete requirement for any of —
(a) **multi-region / data residency**, (b) **edge** deployment, or
(c) **independent per-capability scaling** (a capability whose load profile diverges
sharply from its host's).

**Shape of the C migration when triggered:** adopt **wRPC providers** for the non-DB
capabilities; **wadm/OAM** for application deployment; **lattice** invocation for
component-to-component — while **keeping Postgres co-located** (so the end state is
still hybrid). Because B keeps every interface on a standard WIT, C is a
**per-capability migration, not a rewrite**: choosing B now costs nothing toward C.

**Why deferred:** no multi-region/edge/independent-scaling driver today; the lattice
adds a network hop + serialization to every call, makes NATS a data-plane dependency
with its own failure modes, and turns multi-tenant isolation into a materially harder
hardening problem over a shared lattice.

## Consequences

- **Unblocks:** a clear, recorded default for every future capability decision
  (standard WIT unless a `wamn:postgres`-grade differentiator is documented) and for
  the deployment surface (generate `WorkloadDeployment`, do not reach for wadm).
- **Forecloses (until the trigger):** location-transparent invocation, independent
  provider scaling, and edge/multi-region — accepted, because B keeps them a bounded
  future migration rather than a lost option.
- **Review triggers:** the C trigger above; a `wasmcloud:postgres` that reaches
  feature parity with our exceptions; wadm/runtime-operator convergence upstream.

## Relationship to existing decisions

This is the umbrella the following were implicitly instances of: **D5** (per-host
pooling, not a lattice-shared pool), **D7** (in-cluster HTTP invocation over wRPC),
**D16** (fork-carried per-component memory limiter). **D6** (Postgres topology) is
orthogonal — it concerns data placement, not the wasmCloud runtime/lattice boundary.
