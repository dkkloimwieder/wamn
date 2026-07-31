# DB-Path Egress Review (wasmCloud v2.6.1)

Review of the claim that the `wamn:postgres` host plugin is the **only** path a
workload component has to Postgres — components never get `wasi:sockets` to open
a raw TCP connection that would bypass the plugin's tenant-claim / RLS injection.

- **Issue:** wamn-wv3 `[2.6] Egress/security review: plugin is the only DB path`
- **Plan:** `docs/platform-plan.md` item 2.6 (and 8.2 tenant isolation)
- **Depends on:** the 2.2 production plugin (wamn-ui3) — see `wamn_postgres.rs`,
  memory `wamn-2.2-postgres-production-facts`
- **Follow-up filed:** wamn-7j0.1 (enforce the boundary at deploy)

## Verdict

**The DB-path guarantee now has two independent application layers.** First,
published workload components are refused if their world imports
`wasi:sockets`; this applies to both P2 and P3 socket packages. Second, the
pinned wasmCloud v2.6.1 runtime denies raw TCP and UDP connect/send operations
unless the workload has the explicit `wamn.allow-raw-sockets` opt-in. The opt-in
does not widen bind authority: `UdpBind` remains service-loopback-only.

WIT-world composition still matters, but it is not the whole enforcement story.
Composition determines whether a guest can name a socket interface at all; the
publish policy refuses that surface, and the runtime policy is an independent
backstop if a socket-importing component nevertheless reaches a store. For
ordinary published workloads, `wamn:postgres` (plus separately controlled
`wasi:http` egress) remains the host-mediated DB path.

## How egress actually works in the runtime

The layers below are verified against the pinned v2.6.1 `wash-runtime`.
`docs/wash-runtime-fork.md` is authoritative for the moving branch, revision,
carried-commit ledger, configuration precedence, and policy exit conditions;
this review records the resulting security posture rather than duplicating that
ledger.

| Mechanism | What it gates | Default / behavior | The DB path |
|---|---|---|---|
| **WIT-world composition** | Which interfaces a component can name and call | A component can call only imports declared in its world | Shipped standard worlds omit `wasi:sockets` |
| **Publish-time component policy** | Whether a socket-importing artifact is admitted | P2 and P3 `wasi:sockets` packages are refused | Prevents a published workload from acquiring the raw DB-bypass surface |
| **Runtime raw-socket policy** | Whether an admitted raw-socket operation proceeds | `TcpConnect`, `UdpConnect`, and `UdpOutgoingDatagram` deny by default and require explicit opt-in; `UdpBind` is service-loopback-only | Independent defense if a socket-importing component reaches the runtime |
| **`allowed_hosts`** (wasi:http only) | HTTP egress destinations | Per-workload allowlist; S6 `HostHandler` egress spy | Governs `wasi:http` only; never consulted for raw sockets |
| **Kubernetes network policy** | Pod-level destinations and ports | Deployment defense in depth; enforcement depends on the cluster network provider | Can limit the host pod's DB destinations, but cannot identify which in-process guest or host plugin opened a connection |

### 1. WIT-world composition — reachability, not permission

`wasi:sockets` is registered on every workload linker for P2 and P3. Linker
registration makes an imported interface satisfiable; it does not add that
import to a component or grant permission to call it. `host_interfaces`
separately controls host plugins such as `wamn:postgres`; WASI built-ins do not
become host-plugin grants.

A standard world that omits `wasi:sockets` cannot issue a raw socket call.
Conversely, composition alone does not make a socket-importing world safe, which
is why publication and runtime each enforce a separate policy.

### 2. Publish-time socket refusal

The component import policy screens artifacts before publication. A component
importing the P2 socket interfaces or P3's consolidated socket interfaces is
refused, while a standard clocks/I/O/HTTP world remains admissible. This is a
policy decision over the component's declared imports, not a claim that the
runtime linker lacks socket implementations.

`socketguard` proves both refusal arms with synthesized valid components, one P2
and one P3, plus the standard-world positive control. `egressbench` separately
walks real shipped artifacts. Neither proof substitutes for the runtime test:
publish refusal proves admission, while runtime denial proves what an admitted
socket call can do.

### 3. Runtime raw TCP/UDP policy — default deny, explicit opt-in

The production store policy covers every compiled, guest-reachable raw-socket
surface:

- P2 and P3 `TcpConnect`;
- P2 and P3 `UdpConnect`;
- P2 and P3 `UdpOutgoingDatagram`; and
- P2 and P3 `UdpBind`.

`TcpConnect`, `UdpConnect`, and `UdpOutgoingDatagram` deny when the raw-socket
capability is absent and proceed only when `wamn.allow-raw-sockets` is
explicitly enabled. `AllowedIPNameLookups` (JSON/YAML
`allowedIpNameLookups`) and `allowed_hosts` are independent authorities;
neither grants raw TCP or UDP access.

`UdpBind` is service-loopback-only: only service workloads may bind, and only
to loopback. A non-service loopback bind and every non-loopback bind are denied.
Raw-egress opt-in never widens that bind rule. P3 policy is load-bearing even
though wamn deploys no P3 service workload: a guest importing a P3 socket
interface reaches the P3 host surface regardless of deployment-level P3
adoption.

`egressbench` drives the P2 operations through the production store path and
pins the corresponding P2 and P3 policy call sites on the exact linked fork.
Its named mutations fail if any connect/send arm becomes unconditional or if
the bind rule widens.

### 4. `allowed_hosts` gates only `wasi:http`

`allowed_hosts` is carried on the ctx (`engine/ctx.rs` `CtxHttpHooks`) and
enforced for `wasi:http` egress — the S6 `HostHandler` chokepoint that the
egress-spy test exercises (memory `wamn-s6-testhost-facts`). It has no effect on
raw sockets and is not the raw-socket opt-in.

## Kubernetes defense in depth

A production cluster should also enforce egress policy beneath the component
runtime: constrain host pods to the approved Postgres endpoints and ports, and
deny unrelated destinations. This limits blast radius if an application-layer
control regresses.

Kubernetes policy is not a substitute for either application layer. The
`wamn:postgres` plugin and guest stores are co-located in the host process, so a
pod-level policy normally cannot distinguish a plugin connection from a raw
socket opened on behalf of a guest in the same pod. A more granular
plugin-only network identity would require a different deployment boundary.
NetworkPolicy enforcement must also be proven on the production CNI; a manifest
that the cluster network provider ignores supplies no protection. NetworkPolicy
implementation remains an infra / Epic 8 concern, outside this review.

## Standing proof set

- `egressbench` checks real artifact imports, executes P2 raw-socket denial and
  opt-in behavior through the production host store path, and pins all P2/P3
  TCP/UDP policy call sites.
- `socketguard` proves that valid adversarial P2 and P3 socket worlds are
  refused at publish, and that a standard world still publishes.
- `wamn-component-policy` unit tests pin the shared import classifier used at
  the publication boundary.

The local unit commands and in-cluster jobs are recorded in
`docs/build-and-test.md`. The runtime gate and publish gate remain independent:
passing one never compensates for losing the other.

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
3.5 `wamn-schema-compiler`, the a45 hardening, and the hand-written schemas) lands with
`wamn-1nd`, and **raw-SQL / custom-node enablement is gated behind it**. Until
then the tenant claim is trusted only on the parameterized standard-node path.

## Build-time DDL expression splicing (wamn-cjv.5)

A sibling of the in-band claim vector, one layer up: two **author-supplied**
expression fields are spliced verbatim into DDL that the migration/copy drivers
apply through `batch_execute` (the simple protocol, which honours multiple
`;`-separated statements) — a catalog `Constraint::Check`
(`ADD CONSTRAINT … CHECK (<expr>)`, 3.2 `emit.rs`) and an RLS `RolePredicate`
(`… OR (<expr>)`, 3.5 `wamn-schema-compiler/compile.rs`). Validation previously checked only
non-emptiness, so a `Check` expression such as
`1=1); DROP TABLE app_system.users; --` closed the wrapping paren early and
chained arbitrary statements at **migration-role** privilege (blast radius = the
migrate connection's grants, which reach `app_system`/`wamn_run` in the same DB).
Not reachable on shipped default paths — catalog/policy authorship is trusted
platform code today — but it goes live the moment a multi-author flow or a
self-serve schema editor lets an untrusted author supply a `Check` or
`RolePredicate` expression.

**Shipped guard (cjv.5).** The authored expression **fragment** is validated at
design time, before emission, by `wamn_schema_model::unsafe_expression_reason` — a
literal-aware lexical scan that rejects a top-level statement terminator,
unbalanced parentheses, or a comment-open (`--` / `/*`), plus dollar-quoting and
stray backslashes. A single boolean expression never legitimately contains any of
these, and a `;` inside a string/identifier literal (`note <> 'a;b'`) stays legal.
The guard fires from the two pure validators (`wamn-schema-model` for `Check`,
`wamn-schema-compiler` for `RolePredicate`), and `compile()`/`migrate()` validate first — so
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

- Runtime branch, revision, carried-policy details, and exit conditions:
  `docs/wash-runtime-fork.md`.
- Plugin: `crates/platform/runtime/src/plugins/wamn_postgres/mod.rs`; memory
  `wamn-2.2-postgres-production-facts`, `wamn-postgres-wit-0.1-frozen`.
- In-band claim guard (cjv.2): `reject_claim_mutation` /
  `statement_mutates_session` in `wamn_postgres.rs`; gate
  `tests/integration/src/pgbench.rs` (`--mode attack`) + pgprobe ops 7/8/9;
  structural close deferred to `wamn-1nd`.
- Expression-chaining guard (cjv.5): `wamn_schema_model::unsafe_expression_reason`
  (`crates/schema/model/src/validate.rs`), wired into the `Check` validator
  (`wamn-schema-model`) and the `RolePredicate` validator (`wamn-schema-compiler`); splice sites
  `crates/schema/compiler/src/emit.rs` +
  `crates/schema/compiler/src/rls/compile.rs`; exec paths
  `migrate_catalog.rs` + `copy_project_env.rs`; live proof
  `crates/schema/compiler/tests/ddl.rs::chaining_check_expression_never_reaches_postgres`;
  least-privileged migrate role deferred to its own bead.
- HTTP egress chokepoint: memory `wamn-s6-testhost-facts` (egress spy).
- Gates: `tests/conformance/src/egressbench.rs`,
  `tests/conformance/src/socketguard.rs`, and
  `crates/platform/component-policy/src/lib.rs`.
