# Control-plane registry model (wamn-q3n.1)

The registry is the platform's **system-of-record for identity and placement**.
It is the foundation of the four-tier Postgres topology (`docs/postgres-topology.md`,
epic `wamn-q3n`): it names who exists and answers *where does this database live
and how is it credentialed* — without any tooling parsing a provisioned name.

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

`Env` is a **closed enum** — `dev`, `canary`, `prod` — the default set from the
topology note. It is not open-ended in v1; preview/scratch envs are a later
extension (`docs/postgres-topology.md` §Environments notes them dev-side and
disposable).

Each env resolves to a **recovery-domain side** via `Env::side()`:

| Env | Side | Cluster (T2) |
|---|---|---|
| `prod` | prod | `<org>-prod` |
| `canary` | prod | `<org>-prod` (canary deliberately shares prod's failure domain) |
| `dev` | dev | `<org>-dev` (its own recovery domain — "dev never rewinds prod") |

The side is what makes the T2 prod/dev split load-bearing: `resolve()` uses it to
pick which of an org's two clusters holds the database.

## Tiers and placement

```
Org        { id, tier, prod_cluster: ClusterRef, dev_cluster: ClusterRef }
Project    { org, id }
ProjectEnv { triple: Triple, db_secret: SecretRef }
Registry   { schema_version, orgs, projects, project_envs }
```

- **`Tier`** — `trials` (T3), `standard` (T2), `dedicated` (T4). The T1 system
  cluster (which holds *this* registry) is not an org tier.
- **`ClusterRef`** — a reference (a name) to a CNPG `Cluster`. An org holds two:
  the prod-side and dev-side clusters. For a **T3 trials** org both point at the
  shared pool (`wamn-pg`); for **T2 standard** they are `<org>-prod` /
  `<org>-dev`. T4 dedicated (per-env clusters) is modeled on the same two-cluster
  shape in v1 and refined by `wamn-q3n.14`.
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
Resolution { tier, cluster, secret }
```

`resolve` is the reason the registry exists: it looks up the org (for tier +
clusters), confirms the project and the provisioned project-env exist, picks the
cluster by the env's side, and returns the placement. It fails with a typed
`RegistryError` (`UnknownOrg` / `UnknownProject` / `UnknownProjectEnv`) — an enum
mirroring the failure modes (SR6 rule 2), never `Error(String)`.

## Validation

`validate(&Registry) -> Vec<Issue>` (with `Registry::{issues, is_valid,
validate}`) checks well-formedness — it is structural and pure; the *live*
DB-enforced invariants (references-only, no tenant data, request-path-free, dev
≠ prod recovery domain) are `wamn-q3n.3`'s job. Error codes:

- `bad-schema-version` / `unsupported-schema-version` — `0.1.x` additive-freeze
  compatibility (mirrors `wamn-flow`).
- `empty-org-id` / `invalid-org-id` / `reserved-org-id` (and the `project`
  counterparts) — the slug + reserved-prefix discipline.
- `duplicate-org` / `duplicate-project` (per org) / `duplicate-project-env`
  (per triple) — uniqueness.
- `unknown-org` (a project names an unregistered org) / `unknown-project` (a
  project-env names an unregistered project) — referential integrity.
- `empty-cluster-name` / `invalid-cluster-name` / `empty-secret-name` /
  `invalid-secret-name` — the placement references are DNS-1123 labels.

The reserved-prefix rule, the env→cluster routing (`Env::side`), and referential
integrity are the load-bearing behaviors; each is mutation-tested (apply / test /
restore, debug builds).

## Storage schema (wamn-q3n.3)

`deploy/system-schema.sql` persists the model as tables in the **T1 system DB**
(`wamn_system`, on the cluster `wamn-q3n.2` bootstraps) — the way
`deploy/catalog-schema.sql` follows `wamn-catalog`. It is a **standalone
artifact**, deliberately *not* wired into `deploy/postgres-init.sql` (which builds
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
  (`id`, `tier`, `prod_cluster`, `dev_cluster`), `projects` (`org`→org, `id`),
  `project_envs` (`org`/`project`→project, `env`, `secret_name`,
  `secret_namespace`). FK integrity + the composite keys mirror `validate()`;
  the `tier`/`env` CHECK literals come from the model (`Tier::as_str` /
  `Env::as_str`).
- **`provisioning`** — `sagas`: a **minimal** exactly-once / resumable saga-state
  table (consumed by `.6` provision-org / `.7`). `target` is decoupled text (a
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
| 4 | dev ≠ prod recovery domain | `orgs` CHECK `tier = 'trials' OR prod_cluster <> dev_cluster` (T3 pool shares; T2/T4 must differ) | a rejected bad-standard-org insert; mirrors `Env::side`/`resolve()` |

Tests live in `crates/wamn-registry/tests/storage.rs`: a DDL↔model **drift guard**
(table/column shape, the tier/env/saga CHECK literals, `SCHEMA_VERSION`, the
dev≠prod expression pinned verbatim), the invariant-1 grep, and a **live-apply
gate** (`WAMN_REGISTRY_PG_URL`, applied as `wamn_system`; skips when unset) that
proves invariants 2/3/4 + FK integrity + saga exactly-once. The load-bearing
asserts are mutation-tested (drop the dev≠prod CHECK, add a credential column, add
a tenant-data table, break a drift-guard column — each killed).

## Scope — what `.1` is *not*

`.1` is the **model only**. Deliberately deferred to its own epic children:

- **`.3`** — the live system-DB tables on the T1 cluster and the four testable
  invariants (DDL + storage), the way `deploy/catalog-schema.sql` followed
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

See the `[D6/wamn-q3n.1]` block in `CLAUDE.md` / `AGENTS.md` for the exact
commands (`cargo test -p wamn-registry` + clippy/fmt).

## References

- Topology: `docs/postgres-topology.md` (§T1, §Environments, §Reversibility).
- The reversibility seam: `crates/wamn-host/src/plugins/wamn_postgres.rs`
  (`CredentialProvider` / `ProjectConfig`).
- Slug discipline: `crates/wamn-provision` (`validate_project_id`), wamn-66x,
  wi4.
