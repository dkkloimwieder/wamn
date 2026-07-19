# Control-plane registry model (wamn-q3n.1, generalized by wamn-8df.3 / D18)

The registry is the platform's **system-of-record for identity and placement**.
It is the foundation of the Postgres topology (`docs/postgres-topology.md`,
epic `wamn-q3n`): it names who exists and answers *where does this database live
and how is it credentialed* — without any tooling parsing a provisioned name.

`wamn-8df.3` replaced the original closed `Env` / `Tier` enums with the **D18
generic deployment model** (`docs/deployment-model.md`): `env` is a validated
slug resolving a named **env policy**, an org carries a minimal **placement**
descriptor, and the concrete cluster is **derived** by one rule, `cluster_of`.
This doc describes the model as shipped; the design rationale is
`docs/deployment-model.md`.

- **Issue:** `wamn-q3n.1` `[D6]` (epic `wamn-q3n`, foundation). **Gates**
  `wamn-q3n.3` (system-DB schema + invariants), `.4` (plan amendment), `.5`
  (3.4 lifecycle amendment), and the provisioning rework (`.6`/`.7`).
- **Crate:** `crates/wamn-registry` — a **pure model** (SR6 rule 1: no DB, clock,
  or wasm; deps `serde` + `serde_json`), following the `wamn-catalog` /
  `wamn-flow` / `wamn-run-store` house pattern.
- **This is a model, not a contract.** Like `wamn-run-store`, it ships
  `validate()` + serde import/export but **no** published JSON-Schema file (the
  registry is the shape of the system-DB tables `.3` builds, not a
  cross-language document).

## The identity triple

```
Triple { org, project, env }
```

`(org, project, env)` is the first-class control-plane identity **every**
subsystem speaks — registry rows, provisioning, subdomain routing, dispatcher
registration, and promotion tooling (3.4 draft→staged→applied; 11.2's
suites-promote-with-flows). Carrying the triple structurally from day one is the
point: tooling keys off it and never parses a provisioned name. Routing is
*derived*, not parsed — `Triple::host_label()` yields `<project>--<env>.<org>`
(the caller appends the platform base domain).

`org` and `project` are **lowercase slugs** (`[a-z0-9-]`, start/end alphanumeric,
≤ 40 bytes, not under the reserved `wamn` prefix — wamn-66x), because they embed
into cluster / Secret / subdomain names. This is the same discipline as
`wamn-provision::validate_project_id` and wi4 flow ids; it is *inlined* here (a
few lines) to keep this foundational crate's dependency closure tiny and to avoid
a registry → provisioning coupling (the `wamn-provision`-inlines-`quote_ident`
precedent, SR7).

## Environments

`Env` is a **validated slug newtype** (D18) — the schema-transparent-newtype
pattern, like the org/project ids. The default set `dev`, `prod` is **data** (two
seeded `env_policies` rows), not a type; `canary`, `staging`, … are added as
policies, never as enum variants. A project-env's `env` slug both identifies it
in the `Triple` and **resolves its policy** — validity is: the slug is
well-formed *and* names a known policy.

### Env policies

```
EnvPolicy { name: Env, recovery_domain, promotion_rank,
            instances, storage, cpu, memory, image,
            backup_cadence, wal_retention, hibernation }
RecoveryDomain = Own | SharedWith(Env)
```

A named, self-contained policy — the D18 replacement for the closed `Tier`
sizing/backup semantics. `recovery_domain` drives placement: `own` = the env gets
its own cluster on a dedicated org; `shared-with(x)` = it co-locates in env `x`'s
recovery domain (`canary` shared-with `prod` reproduces the shipped T2 canary
with no enum variant; `canary` own reproduces the T4 third cluster).
`EnvPolicy::owner()` names the recovery-domain owner (itself when `own`, else the
target). The remaining fields are the sizing / HA / backup / hibernation knobs
`provision-org` renders each cluster from (the cjv.21 fix), and
`promotion_rank` orders promotion (the retired `Env::ALL` order). Policies are
**standalone** (no inheritance) and — since `wamn-8df.4` — **org-scoped**:

```
OrgEnvPolicy { org: OrgId, policy: EnvPolicy }
```

Each org owns its policy rows (`EnvPolicy` stays an org-free *value* so templates
can carry it). Org-scoping is load-bearing, not cosmetic: the cluster renderer
consumes an org's whole policy set, so under platform-global policies one
`canary(own)` row would have forced a canary cluster on *every* dedicated org —
a T2 org (canary shared-with prod) and a T4 org (canary own) could not coexist.

### Templates — the `Tier` successor (wamn-8df.4)

```
Template { name, pooled, policies: Vec<EnvPolicy> }
Template::{trials, standard, dedicated, by_name}
Template::stamp(org_id, pool) -> (Org, Vec<OrgEnvPolicy>)
```

A named **code preset** (versioned with the model, not registry rows) that stamps
an org's placement *and* its initial policy set in one step — the retired closed
`Tier`, re-provided as data:

| Template | Old tier | Placement | Policy set |
|---|---|---|---|
| `trials` | T3 | pooled (shares `--pool`) | `dev`, `prod` |
| `standard` | T2 | dedicated | `dev`(own), `canary`(shared-with `prod`), `prod`(own) |
| `dedicated` | T4 | dedicated | `dev`(own), `canary`(**own**), `prod`(own) |

Stamping is **instantiate-and-own** (`sql::stamp_env_policy_sql`,
insert-if-absent): the org gets its own copy, customizes it per-env, and a
re-provision (or a later template edit) never clobbers a customized row — it only
adds envs the org is missing.

## Placement and cluster derivation

```
Org        { id, placement: Placement }
Placement  = Pooled { pool } | Dedicated
Project    { org, id }
ProjectEnv { triple: Triple, db_secret: SecretRef }
Registry   { schema_version, env_policies: Vec<OrgEnvPolicy>, orgs, projects, project_envs }
```

- **`Placement`** — the minimal descriptor replacing `Tier`: does this org share
  the pool (`pooled(<pool>)`, the T3-style shared pool) or own its clusters
  (`dedicated`)? Sizing / HA / backup are env-policy knobs, deliberately not
  placement's.
- **`cluster_of(org, env_policy) -> ClusterRef`** — the **one rule** replacing
  `cluster_name` / `canary_cluster_name` / `Env::side` / `Org::for_pair` /
  `Org::for_pool` / `Org::cluster_for_env`: a pooled org places every env on its
  pool; a dedicated org's env lives on `<org>-<owner(policy)>`. Both the cluster
  renderer (`wamn-provision`) and `resolve()` derive names from it, so a
  provisioned cluster and a resolved triple always agree.
- **`ClusterRef`** — a reference (a name) to a CNPG `Cluster`. Derived, no longer
  stored per-org (the retired `prod_cluster`/`dev_cluster`/`canary_cluster`
  columns); a pooled org stores only its `pool_cluster`.
- **`SecretRef`** — a **reference** to the K8s Secret credentialing a project-env
  database (`name` + optional `namespace`), **never the credential itself**
  (R8b: the registry stores references; actual material lives in Secrets resolved
  by components holding the matching RBAC). Cluster and Secret names *may* carry
  the `wamn` prefix (`wamn-pg`, `wamn-db-<project>`) — they are platform-minted,
  so the reserved-prefix rule applies only to org/project **ids**, not to these
  names.

### Resolution

```
Registry::resolve(&Triple) -> Result<Resolution, RegistryError>
Resolution { cluster, secret }
```

`resolve` is the reason the registry exists: it looks up the org (for its
placement), confirms the project and the provisioned project-env exist, derives
the cluster via `cluster_of` (the org's placement + the env's policy **in that
org's set** — never another org's), and returns the placement. It fails with a
typed `RegistryError` (`UnknownOrg` / `UnknownProject` / `UnknownProjectEnv` /
`UnknownEnvPolicy`) — an enum mirroring the failure modes (SR6 rule 2), never
`Error(String)`.

## Validation

`validate(&Registry) -> Vec<Issue>` (with `Registry::{issues, is_valid,
validate}`) checks well-formedness — it is structural and pure, and with the DB
`CHECK` enumerations gone it is the enforcement that holds on the **in-memory
`from_json` import path** (the cjv.20 fix); the *live* DB-enforced invariants
(references-only, no tenant data, request-path-free) are `wamn-q3n.3`'s job.
Error codes:

- `bad-schema-version` / `unsupported-schema-version` — `0.1.x` additive-freeze
  compatibility (mirrors `wamn-flow`).
- `empty-org-id` / `invalid-org-id` / `reserved-org-id` (and the `project`
  counterparts) — the slug + reserved-prefix discipline.
- `empty-env-policy-name` / `invalid-env-policy-name` / `duplicate-env-policy` —
  policy names are slugs, unique **per org** (the same name in two orgs is two
  independent rows — org-scoping).
- `unknown-shared-with-target` / `shared-with-cycle` — recovery-domain
  integrity **within an org's set**: a `shared-with(x)` targets one of that
  org's own policies and the per-org graph has no cycle. Two `own`-domain envs
  can never collapse onto one cluster — the derivation `<org>-<owner>` keys on
  the (per-org-unique) policy name, so "dev never rewinds prod" holds by
  construction.
- `empty-env` / `invalid-env` / `unknown-env` — a project-env's slug is
  well-formed **and resolves to a policy in its org's set** (the `CHECK IN (…)`
  replacement; another org's policy never satisfies it).
- `duplicate-org` / `duplicate-project` (per org) / `duplicate-project-env`
  (per triple) — uniqueness.
- `unknown-org` (a project — or a policy row — names an unregistered org) /
  `unknown-project` (a project-env names an unregistered project) — referential
  integrity.
- `empty-cluster-name` / `invalid-cluster-name` (a pooled org's `pool_cluster`) /
  `empty-secret-name` / `invalid-secret-name` — the placement references are
  DNS-1123 labels.

The reserved-prefix rule, the env→policy resolution, the `cluster_of` derivation,
and referential integrity are the load-bearing behaviors; each is mutation-tested
(apply / test / restore, debug builds).

`validate()` is the **primary** id/name guard and runs on the in-memory
`from_json` path a direct control-plane writer takes. As a defense-in-depth
**backstop** for a writer that skips *both* `provision-org` **and**
`Registry::validate()` — a malformed id otherwise flows straight into K8s object
names and WAL paths — the stored slug/name columns in `deploy/sql/system-schema.sql`
also carry a charset/length `CHECK` mirroring it (`orgs_id_charset_check` /
`orgs_pool_cluster_charset_check` / `projects_id_charset_check` /
`env_policies_name_charset_check`, cjv.20; the two id columns include the
reserved-`wamn` clause, the cluster/env names do not). `validate_org_id(&str) ->
Result<(), Issue>` exposes the org-id discipline standalone, for a caller
(e.g. `wamn-2ib`) that wants to reject one id without building a whole registry.

## Storage schema (wamn-q3n.3)

`deploy/sql/system-schema.sql` persists the model as tables in the **T1 system DB**
(`wamn_system`, on the cluster `wamn-q3n.2` bootstraps) — the way
`deploy/sql/catalog-schema.sql` follows `wamn-catalog`. It is a **standalone
artifact**, deliberately *not* wired into `deploy/sql/postgres-init.sql` (which builds
the S2–S6 *tenant-data* fixtures — a different plane entirely).

The sharpest difference from `catalog-schema.sql`: the system DB is
**platform-global, not tenant-scoped**. There is **no** `app.tenant` claim, **no**
per-tenant RLS floor, **no** `NULLIF`/`CHECK (tenant_id <> '')` — the registry is
the platform's own single-tenant control-plane state. The top-level key is
`org_id`; it is **applied as, owned by, and used by** the `wamn_system` owner role
(a superuser driving the apply `SET ROLE`s to it first), plus a future
least-privilege control-plane role (the 8.1 RBAC `GRANT` seam).

Two schemas, so each control-plane subsystem is namespaced and the no-tenant-data
table set (invariant 3) is exactly what they hold:

- **`registry`** — `meta` (singleton storage-format version), `orgs`
  (`id`, `placement_kind` `'pooled'|'dedicated'`, `pool_cluster` — set **iff**
  pooled, a structural biconditional CHECK), `env_policies` (**org-scoped**,
  wamn-8df.4: PK `(org, name)`, cascading with its org; `recovery_domain` jsonb
  `"own" | {"shared-with": "<env>"}`, `promotion_rank`, and the
  sizing/backup/hibernation knobs; **no platform-global seed** — rows are stamped
  per org from a `Template`, insert-if-absent), `projects` (`org`→org, `id`),
  `project_envs` (`org`/`project`→project, `env` via the **composite FK
  `(org, env)` → `env_policies(org, name)`** — referential integrity replacing
  the retired `CHECK IN (…)` literals, and another org's policy never satisfies
  it; `secret_name`, `secret_namespace`). The policy FK is deliberately not a
  cascade (an in-use policy cannot be dropped) and is `DEFERRABLE INITIALLY
  IMMEDIATE`; the `env_policies` org-CASCADE FK is added *after* `project_envs`
  so a plain single-statement org `DELETE` tears a whole org down cleanly
  (RI-trigger creation order — the ordering note in the DDL). FK integrity +
  the composite keys mirror `validate()`. `event_readers` (wamn-l5i9.9, D19
  v3) is the CDC capture registration — one row per CDC-enabled project-env
  (PK the triple, FK → `project_envs` ON DELETE CASCADE): `publication`,
  `slot`, `stream` (`EVT_<org>_<env>` by default), and the
  replication-credential Secret **reference**
  (`replication_secret_name`/`_namespace` — invariant 2; the replication
  credential is its own tier above `wamn_app`). The reader service (l5i9.10)
  deserializes its row (`EventReader` in `wamn-registry`).
- **`provisioning`** — `sagas`: a **minimal** exactly-once / resumable saga-state
  table (consumed by `.6` provision-org / `.7`, and by the unified copy's
  `copy` kind — wamn-8df.5's `Quiesce → … → Cutover` pipeline records each step
  here, and the cutover executor re-reads the row (`select_saga_sql`) and
  refuses unless every prior step is recorded). `target` is decoupled text (a
  provision-org saga runs *before* its org row exists); creation is exactly-once
  via the `saga_id` PK; `step` is the durable resume checkpoint (the write-ahead
  pattern — advanced in the same txn as each step's effect). The per-step
  compensation *ledger* is 10.1's to elaborate. RBAC / quota / billing / audit
  are separate subsystems that also live on T1 but land with their owners.

### The four invariants — encoded and tested

| # | Invariant | Encoding | Test |
|---|---|---|---|
| 1 | request-path-free | architectural (no DB constraint) | a static grep: no data-plane manifest references `wamn-sysdb`/`wamn_system` (only the cluster def + control-plane tooling may) |
| 2 | no credentials (R8b) | `project_envs` holds a Secret **reference** (`secret_name`/`secret_namespace`), no credential column | drift-guard + live-apply column-set assertion |
| 3 | no tenant data | the only tables are the control-plane set above | live-apply asserts the exact `registry`+`provisioning` table set |
| 4 | dev ≠ prod recovery domain | the `cluster_of` derivation (distinct `own`-domain envs derive distinct `<org>-<owner>` clusters) + `validate()` recovery-domain integrity — no per-org CHECK (D18; a pooled org's collapse onto the pool is placement, not a domain violation) | `cluster_of` unit + mutation tests; `shared-with` integrity in `validate()` |

Tests live in `crates/wamn-registry/tests/storage.rs`: a DDL↔model **drift guard**
(table/column shape, the placement/saga CHECK literals, the `env_policies`
seed pinned against `EnvPolicy::dev()`/`prod()`, `SCHEMA_VERSION`), the
invariant-1 grep, and a **live-apply gate** (`WAMN_REGISTRY_PG_URL`, applied as
`wamn_system`; skips when unset) that proves invariants 2/3 + the placement
biconditional + the `project_envs.env` FK + the seed + FK integrity + saga
exactly-once. The load-bearing asserts are mutation-tested (break the seed, drop
the env FK, add a credential column, add a tenant-data table, break a drift-guard
column — each killed).

## Scope — what `.1` is *not*

`.1` is the **model only**. Deliberately deferred to its own epic children:

- **`.3`** — the live system-DB tables on the T1 cluster and the four testable
  invariants (DDL + storage), the way `deploy/sql/catalog-schema.sql` followed
  `wamn-catalog`. **Shipped** — see §Storage schema above (`.3` also folds in a
  minimal provisioning-saga table, its one deliberate step past this model).
- **`.5`** — amend `wamn-schema` (3.4) `Environment` for the full triple + the
  `canary` env; `.1` defines the triple so `.5` is a clean extension.
- **`.2`** — the T1 system cluster infrastructure (Helm/IaC).
- **`.4`** — the fuller platform-plan amendment (routing / 3.4 / 10.x).
- Provisioning-saga state, platform RBAC (8.1), quota/plan definitions, billing
  rollups, and platform audit (8.6) live on the same T1 cluster but are separate
  subsystems, **not** part of this identity/placement model.

## Build & test

See the `[D6/wamn-q3n.1]` / `[D6/wamn-q3n.3]` blocks in `docs/build-and-test.md`
for the exact commands (`cargo test -p wamn-registry` + clippy/fmt + the
live-apply gates).

## References

- Topology: `docs/postgres-topology.md` (§T1, §Environments, §Reversibility).
- The reversibility seam: `crates/wamn-host/src/plugins/wamn_postgres.rs`
  (`CredentialProvider` / `ProjectConfig`).
- Slug discipline: `crates/wamn-provision` (`validate_project_id`), wamn-66x,
  wi4.
