# WAMN HTTP and auth proposal

**Status:** owner-approved step 1 and probe scope · 2026-09-07

**Scope:** HTTP transport, caller authentication, and operation permissions.

## 1. Direction

**HTTP: probe native pooling, then adopt if it preserves WAMN's guarantees.**
**Auth: simplify and measure fresh checks first; let the owner decide whether the remaining cost justifies caching.** No cache lifetime is selected by this proposal.

WAMN is greenfield; compatibility is not a reason to retain a worse design. But this work does **not** authorize a database redesign. Delete replaced code rather than adding compatibility layers. The current no-cache rule remains until the owner explicitly changes it. [1]

**Database scope: a small query replacement only.** Keep the existing databases, tables, indexes, writers, connections, and privileges. Add no schema objects, migrations, cross-database access, copied auth data, synchronization, or new service. Do not move tenant permissions or change grants or RLS.

This constraint applies to the fresh-auth increment (`wamn-ctc8.12`).
The separate identity prerequisite (`wamn-ctc8.19`) owns the explicitly
approved system-database membership fact, provisioning writer, and route
reader. It lands first; step 1 then measures its before/after on that boundary.

Keep the identity split: org-level identity and PAT authority, plus project-role assignments, remain in the system database; environment-level application bindings and permissions remain in the tenant database. Changing that isolation boundary requires a separate owner ruling, not a performance-lane choice.

## 2. Auth: establish the fresh-check baseline

### First, make one small query change and measure it

The reviewed route performs three serial reads: PAT/principal and project roles in the system database, then permissions in the project database. [2]

Replace the first two with **one prepared SELECT over the existing system tables**, using a role-existence check. Leave the project-database permission read unchanged. Keep the reads sequential in this increment; do not add parallel-read orchestration.

```text
System database: PAT + principal + required project role
→ existing tenant-database permission read
→ authenticated caller
```

Preserve full-token verification, expiry, revocation, principal status, expected identity, and project scope. Keep token verification in the existing host code. Remove the superseded separate role read from this route.

The target is **two fresh reads, with no database-layout or privilege change**. A combined SELECT changes SQL; it is not literally zero added query complexity. Keep it short and reviewable.

**Stop rule:** if this needs new database objects, extra authority, or substantial SQL machinery, keep the existing reads and report the cost. Do not expand scope just to achieve two reads. Keeping three simple reads is preferable to adding infrastructure for a small saving.

Use the existing throughput bench before and after. The historical report measured auth at 1.66 ms of a 4.80 ms average request; those are context, not a new baseline or a promised saving. The earlier ~0.5 ms one-read estimate is unverified and is not a target. [4]

### Then, make the cache decision

The owner chooses fresh checks or bounded host caching after reviewing the measured cost and implementation simplicity. No lifetime is selected here. Further database optimization or redesign is **not** a prerequisite for that decision. A cache, if approved, starts without a database change or shared cache service.

Until then, preserve next-request revocation: authorization started after a committed revocation must not use the removed authority. Started work is not undone. Existing revocation tests must still pass.

### If caching is approved

Use a bounded host cache outside guest memory. Store the verified principal, exact permissions, token expiry, and evidence deadline. Key by a fingerprint of the **complete token**, scoped to authority, tenant, project, environment, and policy/release identity. Do not store raw tokens.

Set the deadline when authoritative validation starts, not when its result is cached. Hits never extend it; token expiry may shorten it. Discard late fills. Share concurrent refreshes for one key and bound refresh work. Missing or expired evidence must refresh or refuse. The owner also decides whether unexpired evidence may serve during an outage. RFC 7662 supports the trade-off, not a particular lifetime. [7]

Check exact permissions at every registered operation, including nested calls. Fresh-only operations cannot inherit cached approval. Transactional business rules, privileges, and RLS remain independent.

## 3. HTTP: probe native transport before adopting it

The pinned runtime provides `WorkloadClients` pooling for P2 and P3. Probe how its workload identity maps to WAMN's connection scopes before adopting it. [5]

```text
Outbound call
→ WAMN invocation, release, and binding authorization
→ credential selection and destination approval
→ matching reusable native transport
→ bounded response and effect telemetry
```

**Stable scope:** never key pools by invocation. Separate incompatible tenant scopes, connection instances and credential generations, approved peers, and TLS settings. Keep caller identity, credential headers, bodies, and trace context on individual requests. No pool hit bypasses authorization.

**Lifecycle:** newly authorized calls select the permitted generation; started calls may finish within their deadline. Bound retained clients, idle and active connections, and concurrent requests. Draining old connections still count against the shared limits.

**Authority proof:** verify the connected peer, not just a separate DNS lookup. Preserve nested identity and frozen candidate bindings; never substitute a current binding for a pinned one. Include the shared blobstore candidate-binding path and every admitted protocol in the regression tests. Native pooling alone does not supply these WAMN guarantees. [5, 6]

**Limits and outcomes:** bound headers, request and response bytes, total duration, and concurrency. Enforce byte limits while reading. Preserve the existing outcome vocabulary. No hidden mutation retries, and a timeout does not mean the upstream did nothing.

Update repo-lint when the replacement is proven. Its purpose stays preventing unsafe reuse; it guards scoped pooling rather than fresh-client construction. Keep fresh application stores. [5]

### Separate probe: standard WASI HTTP

Add a named probe for replacing custom HTTP WIT with standard WASI HTTP and native hooks. Verify aliases, credential hiding, invocation authority, destinations, protocols, and tracing. `OutgoingHandler` is a transport hook, not proof these rules already exist. [5]

Adopt the smaller design if proven; component changes are allowed. Otherwise keep the necessary WAMN adapter and name the native gap. This probe need not block transport reuse.

## 4. Alternatives

| Alternative | Reason to choose it / trade-off |
|---|---|
| **Fresh auth** | Target two reads through the small query replacement; retain three if combining them needs more machinery. Keep the database split and revocation behavior. |
| **Bounded host auth cache** | Removes database reads on hits. Requires an owner-approved revocation window. |
| **Cache plus invalidation, or short-lived signed tokens** | Later alternatives requiring separate decisions. Do not add notification infrastructure, database triggers, or token changes in this work. |
| **Standard WASI HTTP** | May remove custom WIT and adapters. Adopt only after the named authority probe passes. |
| **A scoped reusable client outside native transport** | Fallback if native integration is more complex. Preserve the same properties and avoid permanent duplicate production transports. |

**Auth-store consolidation is outside this work.** Putting all auth records in one store would change the deliberate identity/tenant-data split, not merely save a read. It requires its own owner decision with the multitenancy posture and data ownership reviewed. It is not a step-1 option or a prerequisite for caching. Cross-database access is not a shortcut around that decision. [2, 3]

P3, warm instances, and persistent host plugins remain separate options for a measured need.

## 5. Incremental work

| Step | Work | Completion evidence |
|---|---|---|
| **1. Small fresh-auth change and measurement** | Replace the two system reads with one prepared SELECT; leave the tenant permission read alone. Apply the stop rule above. | No schema, ownership, privilege, or infrastructure change. Count reads and report latency, CPU, and load. Expiry, revocation, scope, and nested permissions still pass. |
| **2. Owner auth decision** | Review residual cost and simplicity; do not begin another database optimization project. | Record keep-fresh or approve-cache. Set evidence age, exceptions, and outage behavior only if caching is approved. |
| **3. Probe native HTTP** | Drive the real capability through native pooling. Keep the separate standard-WASI probe non-blocking. | Observe reuse. Peer, generation, candidate, nested-call, protocol, and quota tests pass; gaps are named. |
| **4. Adopt and simplify** | Adopt proven HTTP changes; add caching only if approved. Update guards and docs; delete replaced paths. | End-to-end tests pass. Any cache proves expiry, late fills, outages, and multi-host revocation bounds. |
| **5. Compare with the same bench** | Extend the existing throughput bench and journey, not a new harness. | Report latency percentiles, throughput, CPU, memory, auth reads, connection churn, and any cache results. Include cold traffic, failures, and rotation. |

Measurement starts in step 1 and accompanies each increment. Use `tools/receiving-cluster-journey-run --apply --throughput`, `wamn-throughput`, and the existing evidence layout. Compare matched release builds and workloads, changing one factor at a time. [4]

HTTP probing may proceed independently of the auth decision. Neither half waits for an unrelated redesign.

The owner-approved work belongs to `wamn-ctc8`: the first identity membership
prerequisite (`wamn-ctc8.19`), then fresh-auth step 1
(`wamn-ctc8.12`), the native HTTP probe (`wamn-ctc8.13`), the separate WASI
HTTP probe (`wamn-ctc8.14`), and session tokens (`wamn-ctc8.15`).
The native probe uses a throwaway crate after `wamn-b2m6.7`, which changes
the shared `connection_http.rs` identity context. Adoption is separate
(`wamn-ctc8.16`). The WASI probe blocks neither transport reuse nor sessions.
A measured three-read stop-rule result also satisfies the session baseline
prerequisite. The companion proposal records the approved constants,
membership authority, and separate `wamn-identity` deployable. Its §8 proof
list remains verbatim.

**Initial target:** a simpler fresh-auth path where feasible, proven native transport reuse, and bounded resources—without new database machinery. Caching remains an explicit owner decision. No durable effect ledger or production replay is added.

## Evidence

This is a proposal, not an implementation report. Auth and benchmark source checks: WAMN `fc7fb819bcc3eb070cb9925f79c58864ec8889de`. HTTP/runtime basis: the original proposal's WAMN `207ab3224724eb8e070a4fe296bd14696a649f06` and runtime fork `735b57982545358409a7d965a22549b08487ca09`. This revision applies the supplied reviewer correction and the owner's concern about additional database complexity. It adds no new repository verification or benchmark result.

[1] `docs/exe-model.md`, authorization rule: fresh token, principal, role, and permission reads; next-request revocation.

[2] `crates/identity/platform/src/lib.rs`, identity ownership and SQL; `crates/platform/runtime/src/plugins/flow_http_routing.rs`, `RouteAuthentication` and `authenticate`: system identity reads followed by project permission lookup.

[3] PostgreSQL 18 documentation, section 5.10, Schemas: a client connection accesses one database. Cross-database access requires an additional mechanism.

[4] `evidence/perf/2026.09/7-release-host.md`, phase table and Method: historical timings, existing throughput commands, and evidence layout. The proposed optimized timings have not been measured.

[5] Runtime `crates/wash-runtime/src/host/http.rs` and `host/http_client.rs`: `OutgoingHandler`, `DefaultOutgoingHandler`, `WorkloadClients`, connector behavior, lifecycle, and quotas. WAMN `docs/architecture/native-alignment-ledger.md`: transport versus guest reuse.

[6] WAMN `connection_http.rs`, `wamn_postgres/claims.rs`, `wamn_blobstore/plugin.rs`, and `crates/execution/host/src/router_driver.rs`: current effect authority, destination pinning, candidate bindings, and nested invocation identity. Authority tests are requirements for adoption, not claims of completed fixes.

[7] RFC 7662, section 4: token-introspection caching trades freshness for performance and must not outlive token expiry. No OAuth migration or cache lifetime is selected here.
