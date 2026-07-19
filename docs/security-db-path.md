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
directly checkable on the wasm artifacts. `wamn-gates egressbench` compiles each
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

## In-band claim integrity within the plugin path (wamn-cjv.2)

The verdict above is about *reaching* the DB (raw socket vs the plugin). A
distinct vector, not considered by the original 2.6 review, lives **inside** the
plugin path: a guest that legitimately reaches the plugin can try to rewrite its
host-injected tenant claim in-band and defeat RLS.

Tenant identity is injected by a single fully-bound
`set_config('app.tenant', $1, true)` (the `SET LOCAL` equivalent) at `BEGIN` —
the value travels as a bind parameter, so there is no interpolation path and an
injection-shaped tenant is *unrepresentable*, not merely validated (R2/R16; the
`valid_*` charset checks are demoted to an identity-format contract). RLS keys on
`NULLIF(current_setting('app.tenant', true), '')`.
`app.tenant` is an unreserved GUC that the `wamn_app` login role
(`NOSUPERUSER NOBYPASSRLS`) may freely `SET`, so a guest on the transaction API
doing `begin()` → `execute("SET app.tenant = 'victim'")` → `query(...)` — or the
`set_config('app.tenant', …)` equivalent — would read/write another tenant's
rows. Not reachable on shipped default paths (standard nodes emit only
parameterized SQL; the raw-SQL node is flag-OFF; custom nodes are unshipped), but
directly exploitable once the raw-SQL node (`wamn-1nd`) is enabled or custom
nodes (`wamn-bd5`) ship.

**Shipped guard (cjv.2):** `reject_claim_mutation` rejects any guest statement
whose first keyword is `SET`/`RESET` (covers `SET LOCAL`/`SET SESSION`/`SET ROLE`/
`SET SESSION AUTHORIZATION`) or that calls `set_config`, on the
`query`/`execute`/`open_cursor`/`one_shot` surface. The extended-query protocol
forbids statement chaining, so the *reachable* txn-API override can only arrive
as a standalone such statement — which the guard catches. A new `pgbench --mode
attack` gate drives both mechanisms in both directions and asserts zero
cross-tenant rows are ever visible (the mandatory stop-the-line S2 security
gate).

**Limitation — this is defense-in-depth, not a structural close.** The guard is
a blocklist: raw dynamic SQL (`DO`/`EXECUTE`) can still construct a claim
mutation at runtime, which no text guard defeats. The structural close re-keys
RLS onto an identity the guest cannot rewrite — a per-tenant DB role reached via
`SET ROLE` (or connection-per-tenant), with RLS keyed on `current_user` instead
of the settable GUC — so a guest `SET app.tenant` is inert. That work
(per-tenant-role **provisioning** + the RLS re-key across the 3.2 floor emitter,
3.5 `wamn-rls`, the a45 hardening, and the hand-written schemas) lands with
`wamn-1nd`, and **raw-SQL / custom-node enablement is gated behind it**. Until
then the tenant claim is trusted only on the parameterized standard-node path.

## Build-time DDL expression splicing (wamn-cjv.5)

A sibling of the in-band claim vector, one layer up: two **author-supplied**
expression fields are spliced verbatim into DDL that the migration/copy drivers
apply through `batch_execute` (the simple protocol, which honours multiple
`;`-separated statements) — a catalog `Constraint::Check`
(`ADD CONSTRAINT … CHECK (<expr>)`, 3.2 `emit.rs`) and an RLS `RolePredicate`
(`… OR (<expr>)`, 3.5 `wamn-rls/compile.rs`). Validation previously checked only
non-emptiness, so a `Check` expression such as
`1=1); DROP TABLE app_system.users; --` closed the wrapping paren early and
chained arbitrary statements at **migration-role** privilege (blast radius = the
migrate connection's grants, which reach `app_system`/`wamn_run` in the same DB).
Not reachable on shipped default paths — catalog/policy authorship is trusted
platform code today — but it goes live the moment a multi-author flow or a
self-serve schema editor lets an untrusted author supply a `Check` or
`RolePredicate` expression.

**Shipped guard (cjv.5).** The authored expression **fragment** is validated at
design time, before emission, by `wamn_catalog::unsafe_expression_reason` — a
literal-aware lexical scan that rejects a top-level statement terminator,
unbalanced parentheses, or a comment-open (`--` / `/*`), plus dollar-quoting and
stray backslashes. A single boolean expression never legitimately contains any of
these, and a `;` inside a string/identifier literal (`note <> 'a;b'`) stays legal.
The guard fires from the two pure validators (`wamn-catalog` for `Check`,
`wamn-rls` for `RolePredicate`), and `compile()`/`migrate()` validate first — so
every consumer (migrate, copy, publish, dm1, poc) is covered and a rejected
expression never reaches Postgres. Critically the guard targets the *fragment*,
not the assembled `Operation.sql`: the 3.2/3.5 emitters deliberately pack several
`;`-separated statements into one op, so a blanket "no `;` in op.sql" rule would
break legitimate DDL.

**Limitation — defense-in-depth, not the structural close.** Raw dynamic SQL
(`DO` / `EXECUTE`) inside an expression could still build a chaining payload at
runtime that a lexical scan cannot see (the fragment guard rejects `$`/`\` to
blunt this, but does not parse SQL). The structural close is fix part 2 —
applying migrations under a **least-privileged DDL/migrate role** with no
`app_system`/`wamn_run` grants (a build-time mirror of `wamn-1nd`), so a chained
statement cannot reach those tables regardless. That role work touches
provisioning and both exec paths (migrate + copy) and is deferred to its own bead
(an AR1 prerequisite for the multi-author-authorship future).

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
- In-band claim guard (cjv.2): `reject_claim_mutation` /
  `statement_mutates_session` in `wamn_postgres.rs`; gate
  `crates/wamn-gates/src/pgbench.rs` (`--mode attack`) + pgprobe ops 7/8/9;
  structural close deferred to `wamn-1nd`.
- Expression-chaining guard (cjv.5): `wamn_catalog::unsafe_expression_reason`
  (`crates/wamn-catalog/src/validate.rs`), wired into the `Check` validator
  (`wamn-catalog`) and the `RolePredicate` validator (`wamn-rls`); splice sites
  `wamn-ddl/src/emit.rs` + `wamn-rls/src/compile.rs`; exec paths
  `migrate_catalog.rs` + `copy_project_env.rs`; live proof
  `crates/wamn-ddl/tests/ddl.rs::chaining_check_expression_never_reaches_postgres`;
  least-privileged migrate role deferred to its own bead.
- HTTP egress chokepoint: memory `wamn-s6-testhost-facts` (egress spy).
- Gate: `crates/wamn-host/src/egressbench.rs`.
