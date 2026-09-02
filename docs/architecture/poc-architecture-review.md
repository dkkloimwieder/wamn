# POC architecture review — scale, constraint, and extensibility

Status: RULED 2026-09-01 · measured at `f57b4e0b` · companion to
`component-artifact-boundary.md` · sequencing: priorities P1–P4 are
post-slice-iv work items; triggers are recorded, not scheduled.

## 1. Scale failure points

| Finding | Evidence | Disposition |
|---|---|---|
| Guest weight: std+sqlx+virtualization per component (~26 MiB) | 16 s cold compile, ~1 s warm link, 10yt.8 | **P1 — SQL by reference.** Guest sends `(statement_digest, binds)`; SQLx remains the native verifier, while the host resolves the pinned exact bytes and executes them through its existing claim-aware runner (owner ruling, `wamn-10yt.9.2`). The two-sibling weld already pins the bytes; the driver leaves the guest. Guests become tiny (likely `no_std` again for free). |
| Fresh instance per request on large components | Latency floor per call | Revisit instance reuse for package operations (stateless by construction) after P1; likely moot once guests are thin. |
| Generated CRUD `create` has no idempotency key | At-least-once delivery → duplicate rows on retry | **P3 — idempotency key on generated creates**, same semantics as commands. Before any real client. |
| Database-per-env at thousands of tenants | Connection multiplication, CREATE DATABASE race | Pooled tier trigger already filed. No action. |

## 2. Over-engineered (carry, do not extend)

- Package lifecycle (sealing, lineage, dual-plane hash): frozen on `.7`.
- RLS inside a single-tenant database (91 predicates, expression indexes,
  immutable tenant-key function): generated, cheap to carry, guards a
  neighbor that cannot exist. No new arms.
- Five auth layers per call (PAT → route-caller principal → project role
  → app permission → operation token): the route-caller/project-role
  layer duplicates org membership. Collapse when the identity epic lands.

## 3. Over-constrained during churn — rules

- **R-A: package-local vocabularies expand freely.** Enum values, error
  detail keys, manifest names, operation names expand under validator
  rules with no owner ruling. Only platform vocabularies (WIT types,
  effect kinds, auth modes, refusal literals) require a ruling.
- **R-B: `wamn:postgres` type expansion pre-authorized once:** arrays of
  existing scalars, text-backed enums. Anything else by ruling.
- **R-C: a new constraint must name the cost it prevents.** Taxonomy is
  not a cost. (Two constraints reversed this week failed this test.)

## 4. Under-constrained — rules

- **R-D: no environment data in packages.** Validator refuses hosts,
  URLs, secret references, environment names in package content.
  Generalizes the hostname fix.
- **R-E: per-tenant resource quotas** (instances, CPU/epoch budget,
  memory) — trigger: second tenant on a shared host.
- **R-F: wiring authorization** — who may wire which package operations;
  effect budget per wiring. Trigger: first non-technical user wiring
  production. Must land before slice v's editor.
- **R-G: per-user row enforcement** — trigger: first multi-site client.

## 5. Extensibility — where the thesis is unproven

The security half is proven. The "easier" half is not:

- **P2 — `wamn dev`: one command, watch mode.** The loop
  (migrate → introspect → generate → build → virtualize → admit → gate →
  publish → apply → ACL → release → activate) is ~12 steps. One programmatic
  stage engine owns orchestration; `wamn dev` and the loop console are clients,
  never separate control loops (owner ruling, `wamn-10yt.10.1`, 2026-09-02).
  Without this,
  components are more secure *and* harder — a failed thesis.
  The fixed Admit → Gate → Publish → Apply order uses a disposable
  verification-world projection because Admit is pure and persists nothing
  while Publish is otherwise the first writer. It writes only the opaque
  admission receipt's exact project facts; Publish later exact-replays that
  project leg as a no-op,
  without widening the projection into OCI or control (`wamn-10yt.10.11`).
- **Slice v exit criterion added:** at least one wiring composes a
  package operation with palette components, authored through the
  editor, gated, published, and routed. Direct route attachments alone
  do not prove the low-code claim.
- **Seam template:** new effect types (blobstore next) require a platform
  seam — acceptable, but seam authoring becomes a template (WIT + host
  plugin skeleton + admission facts) so it is a day, not a wave.
- **Schema evolution is fresh-install-only.** First real client upgrade
  is the largest post-POC risk. It is its own epic, not a refinement.

## Priorities

1. SQL by reference (thin guests) — closes 10yt.8 at the root.
2. `wamn dev` one-command loop.
3. CRUD idempotency + env-data validator (R-D).
4. Vocabulary pre-authorization (R-A/R-B) — reduces ruling round-trips.

Everything else is a recorded trigger.
