---
status: draft
genre: upgrade plan
date: 2026-07-28
current-fork: dkkloimwieder/wasmCloud @ wamn/2.5.2 = 981fdc56 (verified on remote)
wamn-verified-against: d4f7689
target: upstream v2.6.0 @ 9bf8e97 → new branch wamn/2.6.0
---

# Upgrading the `wash-runtime` fork to wasmCloud v2.6.0

**Do it, and do it before items 1 and 2A start.** Not for the features — for
sequencing. Both of those items are built *against the runtime*: item 1 designs the
payload and checkpoint boundary, 2A decides execution-bundle packaging. Building
them on 2.5.2 and re-porting afterwards is rework in the two most expensive items on
the roadmap, which is the exact failure the plan's H1 discipline exists to prevent.

Treat it as a **policy re-port**, not a dependency bump: upstream heavily changed
the same files the fork patches (linked calls, HTTP handlers, store construction,
runtime context). Replaying six diffs line-for-line would compile and mean nothing.

---

## 1. Where the fork actually stands

Verified, because two things in circulation are wrong:

- **The remote is current.** `refs/heads/wamn/2.5.2 = 981fdc56…`, exactly the rev
  pinned in the root `Cargo.toml`. All six carried commits are pushed.
- **The ledger is current**; only its header summary is stale, still naming three
  commits ("epoch-deadline, memory-limiter, and outbound-traceparent") where the
  table below it lists six. One-line fix during branch creation.
- **`ExecutionHost` is finite and re-arming**, at `d4f7689`: the store takes
  `deadline_ticks(ttl_ms)` at instantiation and is re-armed before `check-flow`,
  `run-next`, and `execute-claimed`; a trap disposes the instance (`self.live.take()`).
  **`NodeRuntime` still sets `u64::MAX / 2`.** (This file pins a wamn SHA because an
  earlier draft described a 29-commit-stale checkout.)

  Upgrade obligation, therefore: **preserve `ExecutionHost`'s finite re-arming and
  trap disposal**; `NodeRuntime`'s missing deadline stays under the separately tracked
  H9 work (§3).

Current pins: `wash-runtime` @ `981fdc56` (`default-features = false`),
`wasmtime-wasi` / `wasmtime-wasi-http` @ git `7535c025`, `async-nats` 0.47.

---

## 2. Carried commits — disposition

Each already records its own exit condition in `docs/wash-runtime-fork.md`. Check
the disposition against *those*, not against a fresh judgment.

| Commit | Exit condition | v2.6.0 verdict |
|---|---|---|
| `94bf77f` epoch deadline | upstream ships native epoch-deadline support | **re-port as-is** for the base upgrade; per-checkout re-arming is a *reusable-store adoption* prerequisite, not base scope (§3) |
| `5b158ff` memory limiter (D16) | upstream plumbs `memory_limit_mb` into a Store limiter | **keep, broaden** — see §3 |
| `d3d83f3` outbound W3C trace inject | upstream injects trace context in its default outgoing handler | **keep, and now doubles** — P2 *and* P3 HTTP paths |
| `8b76869` deny `TcpConnect` (E13) | upstream gates socket linking on `host_interfaces`, or consults egress policy | **keep** — `allowIpNameLookup` does not meet it |
| `eef76cd` deny raw UDP, tighten `UdpBind` (E15/E16) | same | **keep** |
| `981fdc5` limiter accessors + `wamn.api.requests` | upstream exposes accessors **AND** an inbound request-count metric | **split** — the exit is an AND; upstream now satisfies at most the metric half |

**Split `981fdc5` and split its exit condition with it.** The ledger already
describes it as two seams, "(1)" accessors and "(2)" the counter. **Carry both in the first re-port.** Richer HTTP status / body-size / span-status
attributes may *permit* a collector to derive a request count — that is a candidate
replacement, not satisfaction of an exit condition that names an inbound request-count
metric. Remove `wamn.api.requests` only once dashboards, SLOs, **and** its mutation
gate run on a demonstrated equivalent.

**`allowIpNameLookup` is more interesting than a complement.** The `8b76869`
ledger entry rejected allowlist matching because *"`TcpConnect` sees a post-DNS
`SocketAddr`, so proper matching would need an `ip_name_lookup` hook and name→IP
allowlists are fragile."* Upstream has now built that hook — so it is **the missing primitive for that
investigation**, not evidence that retirement is likely. Before either patch retires,
a replacement policy must show that an approved lookup cannot be bypassed by a literal
socket address; that a resolved address cannot be substituted; that DNS change does not
silently widen authority; that TCP connect and UDP send/bind obey one declared policy;
and that P2 and P3 socket surfaces reach the same decision. Until then: adopt
`allowIpNameLookup` defaulting to `[]`, and keep raw sockets denied independently.

**D23 status:** runtime-maintainer posture was accepted at six commits and the
escalation ceiling retired. What is new is that **v2.6.0 maintains parallel P2 and P3
host surfaces**, so a policy at a boundary implemented separately for both generations
needs dual coverage. Not hypothetical — `eef76cd` already had to cover `host_udp.rs`
*and* the P3 mirror `host_udp_p3.rs`. Trace injection is the HTTP instance of the same
problem, and the strongest concrete argument for upstreaming that commit.

---

## 3. What must be re-ported, and how far

Upstream v2.6.0 adds **more kinds of long-lived and reusable stores** — pooled
instances, trigger services, host-component plugins. The fork's patches assume one
central store-construction site (`new_store_from_templates`, one call site *by
design*, per the ledger).

**Two scope rules, and they are not in tension.** *Store-lifecycle* policies — memory
limiting, epoch configuration — are re-ported only for store paths the current
deployment uses. *Host-boundary* policies — trace injection, socket restrictions —
cover every **compiled, guest-reachable** P2 and P3 surface, even where wamn deploys
no P3 service workload, because a guest that imports a P3 interface reaches it
regardless of what wamn deploys.

**Scope the store-lifecycle re-port to paths that are actually enabled.** With pooling and
`host-component-plugins` off (§6), the store paths in play are three:

```
fork:  new_store_from_templates        (the single production site)
wamn:  ExecutionHost store             (crates/execution/host)
wamn:  NodeRuntime store               (crates/platform/node-runtime)
```

Trigger-service and plugin stores do not exist in wamn's deployment until an
experiment enables them. Building a general `StorePolicy` abstraction now means
designing against paths that are off, through an API those experiments will change.
Cover the three; let the experiments produce the generalisation.

**Base-upgrade scope — preserve, do not extend:**

```
fork-created stores   carried epoch configuration and memory policy preserved
ExecutionHost         finite deadline, re-arming, and trap disposal preserved
NodeRuntime           behaviour preserved; its missing finite deadline stays H9 work
pooling / triggers /
host-component plugins  disabled
```

**Feature-adoption prerequisite — before any *reusable-store* path is enabled:**

```
per-checkout deadline re-arming
invocation context refresh and retention
aggregate memory budgeting
retirement after interruption, policy change, or max_invocations
```

The reusable-store prerequisite is what stops a later experiment relying on a
creation-time deadline that a reused store makes meaningless.

**`NodeRuntime` is out of scope, both gaps together.** It attaches no memory limiter
and no finite deadline at the pinned revision. Those are the same class of gap on the
same store, so splitting them — memory into the upgrade, deadline into H9 — would be
worse than keeping them together. **Both stay with H9 / plan item 4.** The upgrade
preserves `NodeRuntime` behaviour and adds nothing to it.

**Aggregate memory is a distinct budget.** Per-store ceilings do not bound
`pool_size × per-store worst case`, nor the workload total. Name the two separately
before pooling is ever enabled.

### Deadline enforcement is *not* upgrade scope

At the pinned revision `ExecutionHost` already enforces finite per-call epoch
deadlines and disposes a trapped instance; **the upgrade preserves that**.
`NodeRuntime` remains effectively unbounded and is owned by H9 — FLOW-SPEC §10.5
records the gap with its full remediation list, scheduled as Phase 3 / plan item 4.
The broader cancellation and deadline programme stays outside this dependency
upgrade.

The upgrade neither causes nor fixes it. **Do not absorb it into the dependency
bump.** It must happen whether or not the fork moves, and filing it here lets an
unrelated security item ride along untracked. What *is* upgrade scope: leave
pooling disabled for any workload depending on a finite deadline, since a
store-creation-time deadline is meaningless for a reused store.

---

## 4. P3 — what it is, and what wamn wants from it

### The distinction that matters

**P2 has no native Component Model async functions, task model, futures, or
streams.** Asynchronous behaviour is expressed through resources, pollables,
subscriptions, and start/finish-style APIs. **P3 adds native async functions,
`future<T>`, `stream<T>`, and runtime-managed task waiting** — `pollable` becomes
`future<T>`, resource streams become `stream<u8>`, `poll()` becomes an awaited task.

**Neither ABI decides whether a host serves requests concurrently.** P2 hosts can and
do run many workloads at once, in separate stores or host tasks.

**Cross-store values (new in v2.6.0)** bridge stream and future values across eligible
store boundaries **without buffering the complete value** — via extraction, injection,
and bounded backpressured pumps, not by moving a handle. Resources and error contexts
stay store-bound. Bytes still move incrementally, so any gain is in ABI shape and
runtime path; size it by measuring wamn's topology.

### Where wamn sits

wamn's component contracts use **WASI 0.2** interfaces; exact package versions and
imported interfaces vary by component (`wasi:io/streams@0.2.6` in the node ABI,
`@0.2.12` elsewhere, and not every component imports HTTP at all). `wash-runtime`
registers **both** P2 and P3 host bindings (`add_p3_to_linker`, the `host_udp_p3.rs`
mirror), so guest imports decide which surface a component uses — and **wamn has not
intentionally adopted a P3 workload ABI at the pinned revision**.

### P3 is two adoptions, not one

**P3 as data movement — a decision, not a forced bump.** `wamn:node@0.1.0` **already
carries a streaming contract**: a `streamed(payload-ref)` payload case, an optional
`payloads` interface over `wasi:io/streams@0.2.6`, and a header that explicitly defers
"the WASI 0.3 native-async revision" to 0.2. The host side is incomplete, but the P2
shape exists. So item 1 has two real options:

```
A   implement the existing 0.1 P2 payload/stream host contract
B   deliberately introduce wamn:node@0.2 with a P3-native contract
```

> **Item 1 designs the bulk boundary and the node ABI together, and records which
> option it took.** Doing that on 2.6.0 rather than 2.5.2 is what puts B on the table
> — the concrete sequencing consequence of this upgrade. It is cheap to decide now,
> since no client-authored nodes exist, and dearer once they do.

Bulk already routes *around* the guest — the node orchestrates, the host transfers —
so the P2/P3 difference is in ABI shape and runtime path, not in whether bytes move
incrementally. Both do. Size the difference by measuring wamn's topology, not by
counting chunk crossings.

**P3 service-style concurrency has a plausible consumer at ingress and no current consumer inside the runner.**

Three concurrencies get conflated; only one is wanted in the runner:

```
many runs, one process     pool of instances     ✓ density — what warm pooling buys
many tasks, one instance   P3 task concurrency   ✗ shared memory + credential context
one run, many nodes        parallel dispatch     — separate engine question
```

`ExecutionState` holds `current: Option<Active>` and the loop pops a token only
when it is `None`, so one node is active per run by construction. **Density comes
from a pool of instances, each with its own store** — not from tasks sharing one.
Two runs in one store would share its linear memory and its mutable credential and
egress context. **But upstream `pool_size` is not wamn executor density.** It covers linked calls and
P3 HTTP dispatch — not P2 HTTP dispatch, and not a manually constructed
`ExecutionHost`. Four distinct things:

```
upstream linked-call pool   warm callee stores for linked component calls
upstream P3 HTTP pool       warm stores serving P3 HTTP dispatch
P3 service workload         one long-lived store serving concurrent tasks
wamn executor pool          multiple wamn-owned ExecutionHost instances driving runs
```

The density need is real — one `ExecutionHost` per executor, driven through `&mut`, so
one replica runs one run at a time — but the mechanism is the **fourth** row, which wamn
builds. 2A establishes which upstream path is even on wamn's execution path; setting
`pool_size` does not parallelise the manual executor.

`flow-http` is the opposite: many genuinely independent requests, so concurrency
there has a real consumer, and handling one at a time is the same shape as H9's
sequential accept loop.

**Prerequisite for concurrency anywhere:** per-invocation credential and egress
context must become genuinely invocation-scoped. Today it is safe because **wamn
serialises** the relevant stores and dispatch paths — `NodeRuntime` locks one warm
instance, the engine has one active dispatch slot per run, `ExecutionHost` is driven
through `&mut`. None of those is a written invariant. P3 service-style concurrency
would remove that incidental protection.

### The strategic signal

P3 is the clear upstream direction — 2.5.0 made it always-on, 2.6.0 builds trigger
services, cross-store values, and host-component plugins on it. **wamn should not
freeze its next public component ABI without explicitly evaluating P3 and recording
the decision.** Separately, staying P2 does not exempt the fork from the
double-patch tax: upstream implements capabilities on both surfaces regardless.

| Adoption | Verdict | When |
|---|---|---|
| Implement 0.1's P2 streaming contract, or introduce a P3-native 0.2 | **decision required** | during item 1, recorded either way |
| P3 cross-store bridging | prototype only where the proposed bundle topology actually crosses stores | 2A |
| P3 trigger-service ingress | **candidate, not a given** — P2 already permits concurrent serving via multiple stores or host tasks, so ingress concurrency is an instance-count question, not a P3 one | after invocation-scoping the context |
| P3 task concurrency inside the runner | **no** | no engine consumer; revisit only if runner semantics change |

---

## 5. Upgrade sequence

**1 — Branch.** Create `wamn/2.6.0` from the immutable upstream `v2.6.0` tag.
Do **not** merge upstream into `wamn/2.5.2` and continue — a base-version branch
keeps the ledger, conflict history, and exit conditions legible.

Fix all four stale surfaces in the same change: the fork doc's header (names three
commits, ledger has six), the **root `Cargo.toml` fork comment** (names two — "the
carried epoch-deadline and memory-limiter commits"), the pinned revision, and the
base-version branch name. Make the manifest comment point at the ledger rather than
duplicate it, so it cannot go stale again at the seventh commit:

```toml
# Upstream v2.6.0 plus the policies recorded in
# docs/wash-runtime-fork.md. The ledger is authoritative.
```

**2 — Re-port policies, not diffs.** For each seam, reimplement its *policy* against the
new architecture and **re-evaluate it against its existing exit condition**. If the
architectural seam has materially changed, amend the ledger explicitly — preserving the
original security or operational intent and recording why the prior wording no longer
expresses it. Never let a difficult re-port quietly produce a more convenient exit
condition. Do not cherry-pick.

**3 — Align dependencies as one coordinated change:**

```
wash-runtime      → fork on v2.6.0
wasmtime family   → crates.io 47.0.1, dropping the git rev pin:
                    wasmtime, wasmtime-wasi, wasmtime-wasi-io,
                    wasmtime-wasi-http (+ wasmtime-wasi-tls if enabled)
async-nats        → 0.49.1
```

Only `wasmtime-wasi` and `wasmtime-wasi-http` are pinned at workspace level today,
while the comment directly above them already states the requirement — *"Native
sandbox hosts and gates must share one Wasmtime type universe."* The gate should
enforce exactly that.

Moving Wasmtime from a git rev to the same crates.io release `wash-runtime` uses
removes the two-wasmtimes hazard the root `Cargo.toml` comment already documents.
`async-nats` matters because wamn passes concrete `async_nats::Client` values into
runtime plugins — a version split is a type split. Audit OpenTelemetry crates at
the same time. Enforce **one resolved Wasmtime 47 type universe across every native workspace package
that links the runtime or exchanges Wasmtime/WASI types — production services *and* proof
packages — plus one resolved `async-nats`**; verify the production closure separately.
`Cargo.toml:23` already states the requirement: *"Native sandbox hosts and gates must
share one Wasmtime type universe."* Checking the top-level crate alone is insufficient.

**4 — Gates.** All existing gates on the new fork, plus new mutation proofs:

- **each carried policy *seam* retains its own negative mutation** — a commit holding
  several seams needs one independently failing mutation per seam (`981fdc5` needs two)
- one resolved Wasmtime 47 **type universe** across production *and* proof packages,
  and one resolved `async-nats`
- memory limits on ephemeral stores and on `ExecutionHost` (re-port regression);
  `NodeRuntime` is out of scope — see §3
- **`ExecutionHost` deadline regression** — invocation A consumes part of an epoch
  window, invocation B receives a newly armed full one; and a trapped or interrupted
  call disposes the live instance so no later call can reuse it. This proves Wasmtime
  47 preserves behaviour wamn already has; it is not an expansion into H9
- P2 **and** P3 outbound trace-context propagation
- raw TCP and UDP still denied, including the P3 mirrors
- `allowIpNameLookup` **as a runtime primitive** — exact, wildcard, and literal-IP
  cases, and the fork's TCP/UDP restrictions still dominating it when lookup is
  permitted. This proves upstream behaviour; **exposing the field through wamn
  workload generation is the separate post-upgrade milestone** (§7), so the base gate
  and the adoption milestone are not circular
- the callable-flow and F0–F4 gate set green on the new fork
- **a resolved feature and deployed-workload inventory** — `cargo tree -e features`
  per production service depending on `wash-runtime`, plus generated-workload checks
  that `host-component-plugins` is off, `pool_size` unset or zero, `max_invocations`
  inert, and no P3 service workload deployed. This makes "three active store paths"
  a verified statement and stops an upstream default-feature change silently widening
  the fork's policy scope.

**5 — Features, separately and only after green.** In this order:

1. `allowIpNameLookup` in workload generation and architecture checks — and the
   evaluation of whether it can retire `8b76869` / `eef76cd`
2. P3 node-ABI decision, feeding item 1's bulk boundary
3. warm-pool and P3 cross-store-stream experiments, inside 2A's bundle economics
4. one host-component connection provider prototype, for 2B — a low-risk provider,
   never the Postgres execution path
5. trigger services for `flow-http`, after context invocation-scoping
6. async secrets `2.0`, only once the connection and credential contracts settle

---

## 6. Explicitly not in this change

- **No global warm pooling.** An 2A experiment, gated on proving no cross-invocation
  leakage of credentials, egress grants, trace context, tenant or run identity,
  connection generations, config, or guest memory — plus an instance-retirement
  policy for traps, cancellation, deadline interruption, and `max_invocations`.
  **Identical pooled component bytes are not proof that invocation-scoped state was
  reset** — bundle identity and instance hygiene are two separate invariants.
- **No `host-component-plugins`.** Off by default upstream; keep it off. It is the
  most interesting thing in the release for 2B's connection providers — validate
  config at workload bind, establish pools before first call, expose a typed
  capability from a component, receive exact caller identity, tear down at unbind —
  but that is a prototype, not an upgrade.
- **No P3 migration of `flow-http`, `flowrunner`, or `node-host`.**
- **No replacement of the durable run plane with upstream trigger services.** They
  supply component lifetime and transport, not durable admission, queue ownership,
  occurrence identity, recovery classes, child-run semantics, or the caller-outcome
  protocol.
- **No removal of the socket patches** on the strength of `allowIpNameLookup` alone.
- **No deadline-enforcement work** — plan item 4 / Phase 3, tracked separately.

---

## 7. Done when — two milestones, not one

**Base rebase complete**

- fork based on `v2.6.0`; **all carried policy seams re-ported**, with `981fdc5`
  tracked as two seams (limiter accessors, request metric)
- each seam's exit condition re-derived
- one resolved wasmtime and one resolved async-nats
- every existing gate green, plus the new mutation proofs
- the feature and deployed-workload inventory recorded
- pooling, host-component plugins, and P3 service workloads remain disabled
- the fork doc's header and ledger agree

**First post-upgrade adoption complete** — deliberately separate, so "features only
after green" is not circular with the definition of green:

- workload generation and architecture checks expose `allowIpNameLookup`
- its default remains `[]`
- raw TCP and UDP remain independently denied
- the socket-policy retirement investigation may begin

**Follow-up trigger** (not a failure condition — fork ownership is settled and the
upgrade proceeds regardless): if dual-surface maintenance proves disproportionately
error-prone during the re-port, prioritise upstreaming trace propagation and opening the
socket-policy conversation the ledger has flagged since the fourth carried commit.
