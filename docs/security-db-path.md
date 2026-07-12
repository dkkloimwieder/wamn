# DB-Path Egress Review (2.6)

Review of the claim that the `wamn:postgres` host plugin is the **only** path a
workload component has to Postgres — components never get `wasi:sockets` to open
a raw TCP connection that would bypass the plugin's tenant-claim / RLS injection.

- **Issue:** wamn-wv3 `[2.6] Egress/security review: plugin is the only DB path`
- **Plan:** `docs/platform-plan.md` item 2.6 (and 8.2 tenant isolation)
- **Depends on:** the 2.2 production plugin (wamn-ui3) — see `wamn_postgres.rs`,
  memory `wamn-2.2-postgres-production-facts`
- **Follow-up filed:** wamn-7j0.1 (enforce the boundary at deploy)

## Verdict

**The guarantee holds for everything shippable in P1 — but it is enforced by
WIT-world composition at build/publish time, not by the host at deploy time.**
The standard flow-runner and every generated/custom-node workload we ship do
**not** import `wasi:sockets`, so the raw-socket interface — though present on
the linker — is never reachable, and the plugin (plus the `allowed_hosts`-gated,
egress-spied `wasi:http` chokepoint from S6) is the only egress. This is asserted
by a static gate (`egressbench`, below).

The finding is that **the host provides no defense-in-depth**: if a component
whose world imports `wasi:sockets` ever reaches the runtime (the production
custom-code-node path, 5.6 / wamn-bd5), the runtime currently permits it to
connect to Postgres directly. We recommend closing that with a deploy-time guard
(wamn-7j0.1) and a K8s NetworkPolicy (infra), tracked below — but no host code
changes in 2.6, which stays a review.

## How egress actually works in the runtime

Three mechanisms govern whether a component can reach the network. Verified
against the pinned wash-runtime (fork `wamn/2.5.2`, `crates/wash-runtime` —
see `docs/wash-runtime-fork.md`).

| Mechanism | What it gates | Default / behavior | The DB path |
|---|---|---|---|
| **WIT-world composition** | Which interfaces a component can *call* | A component can only call imports declared in its world | **The effective boundary.** Shipped worlds don't import `wasi:sockets`, so raw TCP is unreachable |
| **`allowed_network_uses` + `socket_addr_check`** (raw sockets) | Whether a *socket call* is permitted | TCP **allow-all** for outbound connect (see below) | Provides **no** second layer — if sockets are imported, PG:5432 connect is allowed |
| **`allowed_hosts`** (wasi:http only) | HTTP egress destinations | Per-workload allowlist; S6 `HostHandler` egress spy | Governs `wasi:http` only; never consulted for raw sockets |

### 1. WIT-world composition — the effective boundary

`wasi:sockets` is registered on **every** workload linker unconditionally
(`engine/mod.rs:146-190`: `sockets::tcp / udp / tcp_create_socket /
instance_network / network / ip_name_lookup`, plus `add_p3_to_linker` at
`:184`). This registration is **not** gated by `host_interfaces` — that allowlist
(`engine/workload.rs:1418`, `bind_plugins`) only matches host *plugins* (like
`wamn:postgres`) to component imports; wasi built-ins bypass it entirely.

So having `wasi:sockets` on the linker is harmless *only because* a component
that does not **import** it in its world can never reference it. The boundary is
the component's world, established at build/publish time by the wamn SDK world +
builder lint (5.4 / 5.6) — not an enforced host check.

### 2. Raw-socket policy is allow-all for outbound TCP

The production store path (`engine/linked_call.rs:176-191`,
`build_ctx_from_template`) sets:

```rust
socket_addr_check: SocketAddrCheck::new(move |addr, reason| Box::pin(async move {
    match reason {
        SocketAddrUse::TcpBind if is_service => addr.ip().is_loopback(),
        SocketAddrUse::TcpBind => false,
        SocketAddrUse::UdpBind => addr.ip().is_loopback() || addr.ip().is_unspecified(),
        SocketAddrUse::TcpConnect
        | SocketAddrUse::UdpConnect
        | SocketAddrUse::UdpOutgoingDatagram => true,   // any address
    }
})),
```

and fills `allowed_network_uses` from `Default` (`sockets/mod.rs:68`:
`tcp: true, udp: true`). `check_allowed_tcp()` (`sockets/mod.rs:90`) is coarse —
an on/off boolean, default on. **Outbound TCP connect is permitted to any
address, and the check never consults `allowed_hosts`.** So the host offers no
second layer: a component that imports `wasi:sockets` can `connect(PG_HOST:5432)`
and speak the Postgres wire protocol under its own credentials, bypassing the
plugin's `SET LOCAL app.tenant` claim and RLS.

(The bench harnesses build stores with `wasmtime_wasi::p2::add_to_linker_async`
over a default `WasiCtx`, whose `socket_addr_check` denies by default — that is a
bench convenience, **not** the production grant. The production template path
above is the one that matters, and it is allow-all.)

### 3. `allowed_hosts` gates only `wasi:http`

`allowed_hosts` is carried on the ctx (`engine/ctx.rs` `CtxHttpHooks`) and
enforced for `wasi:http` egress — the S6 `HostHandler` chokepoint that the
egress-spy test exercises (memory `wamn-s6-testhost-facts`). It has no effect on
raw sockets.

## Why the P1 guarantee holds today — the gate

Because the boundary is "does the shipped component import `wasi:sockets`," it is
directly checkable on the wasm artifacts. `wamn-host egressbench` compiles each
component and walks its import list, asserting:

- the DB-touching **flow-runner** imports `wamn:postgres` (the DB path) and
  **not** `wasi:sockets`;
- every swept component imports **no** `wasi:sockets` and no unexpected
  host-plugin egress interface (`wasi:blobstore/keyvalue/messaging`).

It is a **static** check — a pure function of the wasm bytes, no socket opened,
no Postgres touched. Its result is identical in-cluster and locally, so unlike
the timing/DB gates there is **no separate in-cluster Job of record**; it runs in
CI / locally / in the image build.

Result (local, `flowrunner` + custom-node / probe / trivial shapes + the 4.1
api-gateway serving workload):

```
# wamn-host 2.6 egressbench — DB-path egress review (static)
  flow-runner  .../flowrunner.wasm
    egress imports: allowed=["wamn:postgres", "wasi:http"] raw-socket=[] other=[]
    PASS: no raw-socket surface; wamn:postgres is the DB path
  component    .../pgprobe.wasm
    egress imports: allowed=["wamn:postgres"] raw-socket=[] other=[]
    PASS: no raw-socket surface
  component    .../node_rs.wasm
    egress imports: allowed=[] raw-socket=[] other=[]
    PASS: no raw-socket surface
  component    .../flow_composed.wasm
    egress imports: allowed=[] raw-socket=[] other=[]
    PASS: no raw-socket surface
  component    .../hello.wasm
    egress imports: allowed=[] raw-socket=[] other=[]
    PASS: no raw-socket surface
  component    .../api_gateway.wasm
    egress imports: allowed=["wamn:postgres", "wasi:http"] raw-socket=[] other=[]
    PASS: no raw-socket surface

egressbench complete — overall PASS: true
```

The flow-runner's only egress is `wamn:postgres` (the DB plugin) and `wasi:http`
(the `allowed_hosts`-gated, egress-spied S6 chokepoint) — both host-mediated. The
4.1 **api-gateway** serving workload has the same surface (`wamn:postgres` +
`wasi:http`, no raw sockets), so a future import regression there — e.g. adding
`wasi:sockets` or an unexpected host-plugin egress — now fails the standing gate.
No shipped workload has a raw-socket surface. The gate's FAIL path is unit-tested
(`egressbench::tests`): a `wasi:sockets` import, an unexpected egress import, and
a DB workload missing `wamn:postgres` each correctly fail — the assertion is not
vacuous.

## Recommended controls

1. **Deploy-time host guard (wamn-7j0.1, recommended).** Reject any workload
   whose component world imports `wasi:sockets` (unless explicitly allowlisted),
   turning the build-time convention into an enforced host check. Likely a
   carried wash-runtime patch in the `workload_start` path (inspect imports
   before instantiate) or an admission check in the operator / control plane.
   This is defense-in-depth ahead of the production custom-code-node path (5.6 /
   wamn-bd5), which is why wamn-bd5 now depends on it.
2. **K8s NetworkPolicy (defer to infra / 8.x).** Deny pod → Postgres:5432 except
   from the plugin's egress identity — a network-layer belt beneath the runtime
   control. Deployment/infra concern; recommended, not built in 2.6.
3. **Custom-code-node world (5.6).** The residual risk lives here: the custom
   node SDK world must exclude `wasi:sockets`, and the publish gate (5.4 builder
   lint / 11.5) must reject a custom node that declares it. `egressbench` is that
   gate's artifact-level check.

## Scope

2.6 is the **DB-path** egress specifically. Out of scope, tracked elsewhere:

- broad per-workload egress policy (`allowed_hosts` deny-all defaults +
  `host_interfaces` allowlists) — 8.2 tenant isolation (wamn-5ts);
- threat model / pen-test — 8.7;
- the NetworkPolicy manifest itself — infra / Epic 8.

## References

- Runtime: `engine/mod.rs:146-190` (unconditional sockets linking),
  `engine/linked_call.rs:176-191` (allow-all `socket_addr_check`),
  `sockets/mod.rs:68,90` (`AllowedNetworkUses` default + coarse `check_allowed_tcp`),
  `engine/workload.rs:1418` (`host_interfaces` gates plugins only).
- Plugin: `crates/wamn-host/src/plugins/wamn_postgres.rs`; memory
  `wamn-2.2-postgres-production-facts`, `wamn-postgres-wit-0.1-frozen`.
- HTTP egress chokepoint: memory `wamn-s6-testhost-facts` (egress spy).
- Gate: `crates/wamn-host/src/egressbench.rs`.
