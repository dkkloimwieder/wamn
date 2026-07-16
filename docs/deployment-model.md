# Generic deployment model — env policies, org placement, unified copy (D6 env-model, reopened)

**Design (wamn-8df.2).** Reopens the D6 *environment* model. This note replaces the
closed `Env` / `Tier` enums (and their DB `CHECK` literals) with a data-driven,
configurable model, and unifies "promote", "deploy", "clone", and "tier-move" into
one `copy(src → dst)` operation. It **precedes code** (the topology-precedes-code
pattern, as `docs/postgres-topology.md` preceded provisioning); the implementation
children are `wamn-8df.3` (env slug + policy + org placement), `wamn-8df.4`
(templates), `wamn-8df.5` (unified copy).

Candidate decision-table row **D18**. Supersedes the *encoding* of the D6 env
dimension (`docs/postgres-topology.md` §Environments); the four-tier topology
itself is unchanged — see §"The four tiers survive as configurations".

## Status

**Design forks decided 2026-07-16 (dkk):**

1. **Env policies are standalone / self-contained** (each carries its full knob
   set); the *template* layer that would stamp them is deferred to `wamn-8df.4`.
   Ship **two** hand-written policies now — `dev` and `prod` — for testing.
   `canary` leaves the built-in set (it becomes just another addable policy).
2. **Named platform-level policies** in `registry.env_policies` (keyed by name),
   referenced by a project-env's `env` slug. Per-org overrides arrive with
   templates (`.4`).
3. **Drop the `Tier` enum**; an org carries a **minimal placement descriptor**
   (`pooled(<pool>)` | `dedicated`). The concrete cluster is derived from
   `org.placement` + the env policy's recovery-domain.
4. **Drop the `Env` enum**; `env` becomes a validated slug. The default set is
   data, not a type. (Reintroduce a richer type later only once stabilized.)
5. **`copy(src → dst)` unifies** deploy / promote / clone / tier-move. Ship
   **whole-database, snapshot mode with quiesce-for-cutover** now; carry
   `scope: whole|subset` and `mode: snapshot|live-cutover` as first-class axes
   whose non-default cases are **specified here but built later**.

## Why reopen — the brittleness

The shipped model hard-codes two closed dimensions and special-cases the third
environment onto them:

- `Env` = closed enum `{Dev, Canary, Prod}` and `Tier` = closed enum
  `{Trials, Standard, Dedicated}` (`crates/wamn-registry/src/types.rs`), each
  `as_str()` **drift-guarded** against the `CHECK IN (…)` literals in
  `deploy/system-schema.sql`. Adding one env or tier = a new enum variant + a
  schema migration + a `CHECK` edit + drift-guard edits + every `match` arm.
- **Placement is a special case that already had to be patched.** `Env::side()`
  collapses `canary → prod`-side — *except* for the regulated tier, where
  `wamn-q3n.14` bolted on `Org::canary_cluster: Option<ClusterRef>` +
  `Org::cluster_for_env()` **superseding** `Env::side()`. That patch is the
  brittleness proving itself: the model could not express "canary gets its own
  recovery domain" without growing a parallel, tier-conditional code path.

The cost is rising with the repo (cheap-now-compounds-later): every new
provisioning feature drift-guards the enums further in. The owner's objection is
concrete — the dev/canary/prod + trials/standard/dedicated shape is too rigid;
tenant/env types should be **generic and configurable per customer need**, and
there should be a first-class API to **copy deployments and data**. This note
generalizes before more features accrete against the enums.

The review corroborates: `cjv.20` (registry `validate()` never checks
canary/invariant-4/org-id charset — the DB `CHECK`s are the only enforcement, yet
the `from_json` import path `wamn-2ib` will use never touches the DB); `cjv.21`
(the org cluster renderer hard-codes storage/resources/image — only instance
count varies by tier); `cjv.7` (tier-move has no quiesce step — an automated saga
loses every write in the dump→flip window).

## The model

### 1. Env is a validated slug

`Env` stops being an enum. A project-env's `env` is a **validated lowercase slug**
(the schema-transparent-newtype pattern already used for `OrgId`/`ProjectId`, and
the slug discipline of `wamn-provision` / wi4 / 66x). The `Triple` is unchanged in
shape — `(org, project, env)` — only `env`'s type widens from `Env` to a slug
newtype. `host_label()` (`<project>--<env>.<org>`) is unaffected (a slug is
DNS-safe).

The default env set `dev`, `prod` is **data** (two rows in `env_policies`), not a
type. `canary` is no longer privileged — it is a policy you add. The
`project_envs.env` `CHECK IN ('dev','canary','prod')` is dropped; validity is
instead: the slug is well-formed **and** it names a known policy (§5).

### 2. Env policy — standalone, named, self-contained

`registry.env_policies` holds named, self-contained policies. The env slug **is**
the policy name (a project-env's `env` both identifies it in the Triple and
resolves its policy). Sketch:

```
registry.env_policies
  name             text PRIMARY KEY        -- the env slug: "dev", "prod", ...
  recovery_domain  jsonb                   -- own | { shared-with: "<env>" }
  promotion_rank   int                     -- ordering for promote (dev<prod); optional
  instances        int                     -- HA / replica count
  storage          text                    -- e.g. "100Gi"
  cpu              text                    -- e.g. "2"
  memory           text                    -- e.g. "8Gi"
  image            text                    -- e.g. "postgresql:18"
  backup_cadence   text                    -- base-backup schedule (cron)
  wal_retention    text                    -- PITR window (e1g knob)
  hibernation      text                    -- "off" | "eligible"
  -- future knobs (region, connection_limit, …) are additive columns, no enum
```

Two rows ship now:

```
"dev"  → { recovery-domain: own,               promotion-rank: 10,
           instances: 1, storage: "10Gi",  hibernation: eligible, … }
"prod" → { recovery-domain: own,               promotion-rank: 30,
           instances: 3, storage: "100Gi", hibernation: off,      backup: 6h, … }
```

`canary` — when wanted — is simply added as `{ recovery-domain: { shared-with:
"prod" }, promotion-rank: 20, … }`, which reproduces the shipped T2 canary with
**no** enum variant and **no** `Option<ClusterRef>` patch. A regulated org that
wants canary isolated instead defines/overrides canary with `recovery-domain:
own`. The T2-vs-T4 canary distinction is now a **one-field policy difference**,
not two code paths.

Policies are **standalone** (no inheritance) for now. The **template** layer that
would stamp/parameterize them (a named preset an org instantiates and customizes
per-env) is `wamn-8df.4` — the real successor to `Tier`.

Scope note: policies are platform-global for now (the `recovery-domain` /
`promotion-rank` / sizing knobs are org-independent; the concrete cluster is
derived per-org at resolve time — §3). Org-scoped overrides land with templates
(`.4`).

### 3. Org placement + cluster derivation (Tier dropped)

`Tier` is dropped. An org carries a **minimal placement descriptor** — the residue
of what `Tier` *actually decided* (own clusters vs. the shared pool), stripped of
the sizing/backup semantics that now live in the env policy:

```
registry.orgs
  id             text PRIMARY KEY
  placement_kind text   -- 'pooled' | 'dedicated'   (a small structural CHECK)
  pool_cluster   text   -- set iff placement_kind='pooled'  (e.g. 'wamn-pg')
  -- prod_cluster / dev_cluster / canary_cluster are GONE (derived, §below)
```

`placement_kind` is a two-value structural `CHECK` — deliberately **not** the rich
`Tier` it replaces: it no longer couples placement to sizing/HA/backup (those are
env-policy knobs). It answers only "does this org share the pool, or own its
clusters?"

The concrete cluster holding a project-env is **derived**, replacing
`cluster_name(org, side)` + `canary_cluster_name(org)` + `Env::side()` +
`Org::for_pair/for_pool` + `Org::cluster_for_env()` with **one rule**:

```
cluster_of(org, env_policy):
  match org.placement_kind:
    'pooled'    => org.pool_cluster                 -- every env → the pool
    'dedicated' => "{org.id}-{owner(env_policy)}"   -- one cluster per recovery domain

owner(policy) =
  match policy.recovery_domain:
    own              => policy.name         -- e.g. "prod"  → <org>-prod
    shared_with(x)   => x                   -- canary shares prod → <org>-prod
```

This reproduces the shipped naming exactly and generalizes it:

| org | env | recovery-domain | cluster (derived) | matches shipped |
|---|---|---|---|---|
| dedicated `acme` | prod | own | `acme-prod` | T2 prod ✓ |
| dedicated `acme` | dev | own | `acme-dev` | T2 dev ✓ |
| dedicated `acme` | canary | shared_with(prod) | `acme-prod` | T2 canary ✓ |
| dedicated `acme` | canary | **own** | `acme-canary` | T4 canary ✓ (q3n.14) |
| pooled `trialco` | *any* | — | `wamn-pg` | T3 pool ✓ |

The `wamn-q3n.14` T4 canary cluster — which needed a stored `canary_cluster`
column, a biconditional `CHECK`, a distinctness `CHECK`, and `cluster_for_env`
superseding `side()` — becomes **canary's `recovery-domain: own`**. Three DB
constraints and a bifurcated resolver collapse into the derivation rule + the
policy field.

**`provision-org`** (for a `dedicated` org) renders one CNPG `Cluster` CR per
**distinct `owner(env)`** across the org's env set (default `{dev, prod}` →
`<org>-dev`, `<org>-prod`; add a canary-own policy → `<org>-canary`). Each cluster
is **sized by the policy of its owner env** (`instances`/`storage`/`cpu`/`memory`/
`image`) — which fixes `cjv.21` (sizes are policy-driven, not hard-coded). Backup
CRs (e1g) read `backup_cadence`/`wal_retention` from the same policy. A `pooled`
org renders no cluster CRs (the pool already exists).

### 4. Unified copy — deploy / promote / clone / tier-move

One operation over **arbitrary** `(org, project, env)` triples — same-org or
**cross-org**:

```
copy(src: Triple, dst: Triple, {
  include: definition | data | both,
  scope:   whole | subset(predicate),      -- subset = specified, built later
  mode:    snapshot | live-cutover,         -- live-cutover = specified, built later
})
```

- **`include: definition`** = the app's structure — catalog (3.1), flows (5.1),
  RLS policies (3.5), config. This *is* the existing promotion machinery
  (`wamn-schema` `promote_catalog`), generalized from same-org dev→prod to any
  src→dst. **"Deploy an app" is `copy(definition)` into a fresh `dst`** — including
  a system-maintained app → a customer (`src.org` ≠ `dst.org`). "Promote dev→prod"
  is `copy(definition)` within one org.
- **`include: data`** = the rows — the `pg_dump -Fd` / `pg_restore` artifact
  (`wamn-q3n.10`/`.11`). Separable from definition, per the owner's ask ("move
  dev→prod, then separately move data / a subset of data").
- **`include: both`** on a *move to a different cluster* = tier-move
  (`wamn-q3n.13`) generalized: copy to a `dst` on a new cluster, then repoint +
  deprovision-old.

**Consistency (fixes `cjv.7`):** the step set depends on whether the src keeps
taking writes.

- **Clone into a fresh `dst`** (deploy; clone-into-new): **no quiesce** — the src
  stays live, the dst is new, nobody cuts over.
- **Move / cutover** (the src's traffic will move to the dst): a mandatory
  ordered pipeline —

  ```
  1  Quiesce(src)   src read-only  (REVOKE writes / default_transaction_read_only,
                                    or a logical-replication publication in cutover mode)
  2  Snapshot(src)  dump {definition | data | both}
  3  Restore(dst)
  4  Verify         row-counts / checksums src vs dst
  5  Cutover        repoint the credential seam / registry to dst
  6  DeprovisionOld drop the retained old DB (after a hold window)
  ```

  The saga (`wamn-2ib`) **refuses cutover until quiesce + verify are recorded** —
  which is exactly what `cjv.7`'s dump→flip write-loss window needs. This makes the
  topology doc's "scheduled downtime window" *enforced*, not merely narrated.

**Axes shipped vs. specified:**

- **Now (`wamn-8df.5`):** `include ∈ {definition, data, both}`, `scope: whole`,
  `mode: snapshot`. This already subsumes promote + tier-move + deploy.
- **Specified here, built later:** `scope: subset(predicate)` (referential-
  integrity-aware slicing of a filtered row set — needed for "move a *subset* of
  data") and `mode: live-cutover` (logical-replication publication/subscription,
  lag-monitored switchover — the near-zero-downtime alternative the topology doc
  already names). Both are first-class in the API shape so adding them is not
  another rewrite.

The saga orchestrator that drives `copy` resumably/with compensation is `wamn-2ib`
(10.1); `8df.5` ships the primitive + the quiesce/verify gate it enforces.

### 5. Validation & enforcement (fixes `cjv.20`)

With the `CHECK` literals gone, enforcement moves into `Registry::validate()` so it
holds on the **in-memory `from_json` import path** `wamn-2ib` uses (not only at DB
insert). `validate()` gains:

- **env slug** well-formed (lowercase slug) **and** resolves to a known
  `env_policies` row;
- **org id** charset/length (a standalone `validate_org_id`, not just the throwaway
  one-org `Registry.validate()` convention `cjv.20/C6-4` flags);
- **placement** well-formed: `pooled` ⟺ `pool_cluster` present; a `dedicated` org's
  derived clusters are distinct where their envs' recovery-domains are `own`;
- **recovery-domain integrity**: `shared_with(x)` targets an existing policy; a
  policy graph has no shared-with cycle; **dev never shares prod's recovery
  domain** becomes "no two `own`-domain envs collapse to one cluster" — enforced by
  the derivation, and asserted in `validate()` rather than a per-org DB `CHECK`.

DB-side, referential integrity replaces the `CHECK`: `project_envs.env` **FK →
`env_policies(name)`**. The removed `orgs_*_check` constraints (tier / recovery-
domain / canary-biconditional / canary-distinctness) are subsumed by the derivation
rule + `validate()`. A schema drift-guard pins `env_policies` / the `orgs`
placement columns against the model (the existing storage-drift-guard pattern),
replacing the retired `Tier::as_str`/`Env::as_str` literal guards.

## The four tiers survive as configurations

`docs/postgres-topology.md`'s T1–T4 topology is **unchanged as deployment reality**;
only its *encoding* changes. The tiers become **configurations** of the generic
model — and, with `wamn-8df.4`, named **template presets**:

| Old tier | Generic-model expression |
|---|---|
| T3 trials | `placement_kind: pooled(wamn-pg)` |
| T2 standard | `placement_kind: dedicated`; policies `dev`(own), `prod`(own), `canary`(shared_with prod) |
| T4 dedicated | `placement_kind: dedicated`; `canary`(own) |
| T1 system | out of scope — the control-plane cluster, not an org placement |

Templates (`.4`) re-provide "standard" / "dedicated" as one-click presets that
stamp a placement + an env-policy set; an org then customizes per-env. The
topology doc's tier language stays valid as the name of a preset, not a type.

## Region (design-for, don't build)

No US/EU region dimension is needed now (dkk). Under this model a region is just
another **knob** — an additive `region` column on `env_policies` (or on the org
placement), incorporated into `cluster_of` when present. Adding it is a column + a
derivation clause; **no new enum, no schema-wide migration**. This is the concrete
payoff of dropping the enums: the next placement axis is data.

## Migration path off the closed enums

The registry is **control-plane state with no production rows** — every shipped
`wamn-q3n` gate provisions a throwaway org and tears it down (`for_pair`/`for_pool`
live-standups all deprovision). So this is a **schema + code rework, not a data
migration**:

1. **`wamn-8df.3`** — the model change:
   - `crates/wamn-registry`: `Env` enum → validated slug newtype; `Tier` enum
     removed; `Org` loses `tier`/`*_cluster` fields, gains `placement_kind` /
     `pool_cluster`; new `EnvPolicy` type + `env_policies` in the registry;
     `cluster_of` replaces `Env::side`/`cluster_name`/`canary_cluster_name`/
     `for_pair`/`for_pool`/`cluster_for_env`; `validate.rs` per §5.
   - `deploy/system-schema.sql`: drop the `tier`/env/canary `CHECK`s; add
     `env_policies` + the `project_envs.env` FK + the `orgs` placement columns;
     seed the `dev`/`prod` policies.
   - Consumers: `provision-org` (render clusters from `owner(env)` + policy sizing),
     `provision-project-env` (resolve via `cluster_of`), `wamn-schema` promote
     (env order via `promotion_rank`, not `Env::ALL`), `move-org-tier`, the
     dispatcher, and every drift-guard.
   - Fixes `cjv.20`; largely fixes `cjv.21` (policy-driven cluster sizing).
2. **`wamn-8df.4`** — templates: named presets (the `Tier` successor) that stamp a
   placement + env-policy set; org/per-env customization. Completes `cjv.21`.
3. **`wamn-8df.5`** — the unified `copy` primitive + quiesce/verify gate; generalize
   `promote` (definition) and dump/restore (data) into it; whole-DB + snapshot now.
   Fixes `cjv.7`; feeds `wamn-2ib`.

`docs/postgres-topology.md` §Environments and the `platform-plan.md` D6 row are
updated to point here when `.3` lands (a landing task, not part of this design).

## Consequences

- **Unblocks:** generic, per-customer env/placement configuration without touching
  Rust types or DB `CHECK`s; one `copy` verb for deploy/promote/clone/tier-move;
  an enforced consistency gate on cutover; a region knob that is a column, not a
  migration.
- **Accepted trade-off:** lose compile-time exhaustiveness on `Env`/`Tier` and the
  DB `CHECK` enumeration; re-express both as validated slugs + `validate()` +
  `env_policies` FK. Net: fewer coupled edit-sites per new env/tier, at the cost of
  runtime-validated (not compile-checked) membership. The owner accepts this and
  explicitly may reintroduce a richer type "down the road once stabilized".
- **Forecloses nothing:** subset-copy and live-cutover are specified axes; a future
  typed env/tier can layer over the slug + policy table.

## References

`docs/postgres-topology.md` (the four tiers, the `CredentialProvider` seam this
copy/repoint rides), `docs/registry-model.md` (the identity model this widens),
`docs/provisioning.md` (`provision-org`/`-project-env`/`dump`/`restore`/`move-org-tier`
generalized here), `docs/schema-lifecycle.md` (3.4 promote = the definition half of
`copy`), `docs/review-2026-07.md` §C6 (`cjv.7`/`.20`/`.21`) and the AR trust-model
theme. Beads: epic `wamn-8df`; children `.3` (env slug + policy + placement), `.4`
(templates), `.5` (unified copy).
