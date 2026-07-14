# The T1 system cluster (wamn-q3n.2)

The **T1 system cluster** is the platform's control-plane Postgres: one HA
CloudNativePG `Cluster` that holds the org/project/env registry and the rest of
the control-plane state, kept as a **distinct plane** from every cluster that
holds tenant data. This note records the shipped infrastructure; the design
rationale is `docs/postgres-topology.md` §T1 (the source of truth).

- **Issue:** `wamn-q3n.2` `[D6/E1]` (epic `wamn-q3n`, foundation). **Gates**
  `wamn-q3n.3` (system-DB registry schema + the four testable invariants).
- **Manifest:** `deploy/wamn-sysdb.yaml` — a CNPG `Cluster` (operator 1.29.2,
  `deploy/cnpg-operator.yaml`). This is **infra**, not a crate: there is no
  cargo gate; verification is the live in-cluster standup below.
- **Substrate:** D6 — CloudNativePG in-cluster (`wamn-dxi`).

## What it is

Exactly **one T1 cluster per platform environment** (platform dev / staging /
prod each get their own; `deploy/wamn-sysdb.yaml` is the kind/dev instance). It
is provisioned by Epic-1 Helm/IaC — `kubectl apply` of a `Cluster` CR — because
**it cannot be provisioned by the provisioner it backs** (`wamn-host
provision-project` reads the registry that lives here).

| Property | Value | Why |
|---|---|---|
| Name | `wamn-sysdb` | Platform-minted; distinct from the `wamn-pg` T3 pool and the `wamn-system` namespace. |
| Instances | **3 (HA day-one)** | Shared infrastructure — unlike org clusters where HA is a tier knob. |
| System DB / owner | `wamn_system` / `wamn_system` | An **empty** bootstrapped DB (`initdb`); `.3` applies the registry DDL into it. |
| Superuser access | enabled | The platform's own admin path (registry DDL, saga bootstrap) — **not** a tenant credential. |
| CPU limit | **none** | The DB-serving path must not be CFS-throttled (S2 lesson). |
| pg_hba | `host all all all scram-sha-256` | The whole repo connects NoTls. |

## Distinct plane — two clusters, always

T1 stands up **alongside** the T3 trials pool (`deploy/cnpg-cluster.yaml`
`wamn-pg`) and the legacy S2–S6 gate pod (`deploy/postgres.yaml`) — never
replacing them. They are different planes (control-plane state vs. real tenant
data) with different blast radii, backup postures, and security profiles. The
deployment is **additive**; the shared-cluster guardrail means `wamn-pg` and
`postgres.yaml` are never touched. Teardown of this cluster deletes **only**
`wamn-sysdb`.

The T1 cluster has its own everything: `wamn-sysdb-{rw,ro,r}` Services, a
`wamn-sysdb-superuser` Secret, and three `wamn-sysdb-{1,2,3}` PVCs — disjoint
from the pool's `wamn-pg-*` resources.

## What it holds (control-plane only)

The registry (`wamn-q3n.1` model → `wamn-q3n.3` tables), provisioning-saga state
(10.1), platform RBAC (8.1), plan/quota definitions, billing rollups, and
platform-level audit. **Excluded by design:** no tenant data (catalogs, run
state, payloads, application users live in org/pool clusters); no credentials
(R8b — the registry stores Secret *references*, resolved by RBAC-holding
components); no request-path reads (a T1 outage freezes provisioning and
promotions, never the data plane). Those subsystems merely *live* on T1 — none
is modeled by `.2`, which stops at "an HA control-plane Postgres exists with an
empty bootstrapped system DB."

## Standing it up (the gate of record)

Needs the kind `wamn` cluster + the CNPG operator (`deploy/cnpg-operator.yaml`).

```bash
kubectl apply -f deploy/wamn-sysdb.yaml
kubectl -n wamn-system wait --for=jsonpath='{.status.readyInstances}'=3 \
  cluster/wamn-sysdb --timeout=300s
```

Verification (all asserted live for the kind/dev instance):

- **HA** — `kubectl -n wamn-system get cluster wamn-sysdb -o wide` shows `3/3`,
  "Cluster in healthy state", primary `wamn-sysdb-1`; `pg_stat_replication`
  reports two streaming replicas.
- **Distinct plane** — own `-rw/-ro/-r` Services, own `wamn-sysdb-superuser`
  Secret, own three PVCs; `wamn-pg` and `postgres` stay `1/1` healthy.
- **Bootstrap** — `wamn_system` database exists, owned by the `wamn_system`
  role, and is empty (0 user tables).
- **No CPU limit** — the pod's container carries requests but no limits.

Instance spread is best-effort (`preferred` pod anti-affinity): on kind the
control-plane node is tainted `NoSchedule`, so the three instances pack onto the
two schedulable workers (a production T1 with ≥3 schedulable nodes spreads one
per node). Replication is async streaming; a production T1 may adopt synchronous
replication for registry/saga durability — a platform-env knob, not part of
`.2`.

## Scope — what `.2` is *not*

- **`.3`** (**shipped**) — the system-DB registry **tables/DDL**
  (`deploy/system-schema.sql`, from the `wamn-q3n.1` `wamn-registry` model) applied
  into this DB as the `wamn_system` owner, plus the four testable invariants
  (references-only / no tenant data / request-path-free / dev ≠ prod recovery
  domain) and a minimal provisioning-saga table. `.2` shipped an *empty* system
  DB, the way `deploy/catalog-schema.sql` followed `wamn-catalog`;
  `docs/registry-model.md` §Storage schema documents what `.3` fills it with.
- **`.4`** — the fuller platform-plan amendment.
- **`.5`** — amend `wamn-schema` (3.4) `Environment` for the triple + `canary`.
- Multi-platform-env templating (each platform env its own T1) is a future note;
  `.2` ships one manifest (the kind/dev instance).

## References

- Design: `docs/postgres-topology.md` (§T1, §"The four tiers").
- The registry model this cluster will hold: `docs/registry-model.md`
  (`crates/wamn-registry`, `wamn-q3n.1`).
- The T3 pool it stands beside: `deploy/cnpg-cluster.yaml`,
  `docs/provisioning.md`.
