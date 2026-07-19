# Per-project system schema v1 (2.4)

The application-facing **auth / RBAC / config** tables that live in a project
database: `users`, `roles` (+ the `user_roles` linkage), `permissions`,
`configurations`, `audit_log`, `api_keys`. Shipped as the standalone DDL
`deploy/sql/app-schema.sql`, modeled and drift-guarded by the pure crate
`crates/wamn-sysschema` (bead wamn-as5, `docs/platform-plan.md` §2.4).

## Scope — the auth/RBAC half

Item 2.4 lists two halves. The **platform-metadata** half — `entities`,
`fields`, `relations`, `flows` — is **already shipped and is referenced here,
not redefined**:

- `catalog.entities` / `catalog.fields` / `catalog.relations` —
  `deploy/sql/catalog-schema.sql` (the 3.1 `wamn-catalog` model's storage).
- `wamn_run.flows` — `deploy/sql/flows.sql` (the POC-F1 flow registry).

A `deployments` table is **deferred**: a live workload is a Kubernetes
`WorkloadDeployment` CR, so a registry table would duplicate cluster state until
there is a concrete reader (a follow-up when 5.8 / hosting need it). So 2.4's
genuine new work — this file — is the auth/RBAC half.

## Distinct from the T1 control-plane registry

Do not confuse this with `deploy/sql/system-schema.sql` (wamn-q3n.3):

|                | `app-schema.sql` (this)            | `system-schema.sql` (T1 registry) |
| -------------- | ---------------------------------- | --------------------------------- |
| Plane          | a project database                 | the platform system DB (`wamn_system`) |
| Scope          | **per-project, tenant-scoped**     | **platform-global**               |
| Owner / grantee| `wamn_app` (non-owner), under RLS  | `wamn_system`                     |
| RLS floor      | yes (`app.tenant`)                 | none                              |
| Holds          | app users / roles / keys / audit   | orgs / projects / envs / sagas    |

Different plane, owner, and security model — hence a different file, and
deliberately **not** named `system-schema.sql`. The schema is `app_system`.

## Security shape

Every table mirrors the 3.2 tenant floor with the a45 empty-claim hardening
(identical to `catalog-schema.sql`):

- `tenant_id text NOT NULL CHECK (tenant_id <> '')` — no `''`-tenant row.
- `ENABLE` + `FORCE ROW LEVEL SECURITY`, policy
  `USING/WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant', true), ''))`
  — an absent claim resets the GUC to `''`, which `NULLIF` turns to `NULL`, so it
  matches no row (fail-closed).
- `GRANT SELECT, INSERT, UPDATE, DELETE … TO wamn_app` (never the owner).

This is the **substrate**, not the auth logic: there is **no password hashing,
JWT, or session management** here — that is 4.2 (AuthN) / 8.1 (IdP). The claims
(`app.tenant` / `app.user_id` / `app.role`) are injected by the `wamn:postgres`
plugin from a resolved session; this schema is what those claims key on.

## Claim integration (3.5 / 4.2)

The columns are shaped to be the exact targets the 3.5 RLS builder
(`crates/wamn-rls`) reads:

- **`users.id`** is a `uuid` — the ownership target the builder reads as
  `NULLIF(current_setting('app.user_id', true), '')::uuid`. A data table's owner
  column referencing a user resolves against it.
- **`roles.name`** (text) is the role-gate target the builder reads as
  `COALESCE(current_setting('app.role', true), '') IN (…)`.

The live-apply gate proves this end-to-end: it compiles a **real** 3.5
`RowOwnership` policy over a data table whose owner uuids are `app_system.users`
ids and whose exempt role is an `app_system.roles` name, then asserts the policy
filters rows under `app.user_id` / `app.role` claims.

## Tables

| Table            | Purpose                                                        | Notes |
| ---------------- | ------------------------------------------------------------- | ----- |
| `users`          | application accounts; `id` (uuid) = the `app.user_id` target  | identity only, no credential material; `status ∈ {active, disabled, invited}` |
| `roles`          | named roles; `name` = the `app.role` target                   | `is_system` = platform-provided |
| `user_roles`     | user↔role linkage (many-to-many)                              | 4.2 reads it to compute a user's role; FK both sides `ON DELETE CASCADE` |
| `permissions`    | role → permission string (e.g. `receipts:read`)               | 4.3 AuthZ reads it; FK roles `ON DELETE CASCADE` |
| `configurations` | per-project settings (`config_key` → jsonb `config_value`)    | opaque value |
| `audit_log`      | append-only trail (`actor_id`, `action`, jsonb `detail`)      | `actor_id` is a bare uuid, **not FK'd** — immutable history survives user deletion; indexed by `(tenant_id, occurred_at)` |
| `api_keys`       | api-key substrate — `key_hash` (one-way digest) + `prefix`    | raw key never stored (hashing is 4.2); FK users `ON DELETE CASCADE` |

## The model crate (`wamn-sysschema`)

A pure crate (no dependencies) holding the single source for the schema name
(`SCHEMA_NAME = "app_system"`), the table/column manifest (`TABLES`), the
`UserStatus` CHECK literals, and the claim GUC names — what downstream (4.2
AuthN, 4.3 AuthZ, 2.5 migrations) references so they never hard-code the schema
name or the status literals. It emits no DDL and holds no connection; the DDL is
the authoritative artifact and the model is tied to it by a drift guard.

## Verification

- **Unit** (`cargo test -p wamn-sysschema`): the `UserStatus` literals and the
  table manifest.
- **Drift guard** (`tests/schema.rs`): `deploy/sql/app-schema.sql` must mirror the
  model — the schema name, every table + its pinned columns, the RLS floor + a45
  hardening (one `CHECK (tenant_id <> '')` per table), the `users.status` CHECK
  literals from `UserStatus::as_str`, and the FK cascades (plus that `audit_log`
  does **not** FK `actor_id`).
- **Live-apply gate** (`WAMN_SYSSCHEMA_PG_URL`, a superuser URL — the harness
  provisions `wamn_app`; skips when unset): applies the DDL and asserts tenant
  RLS isolation across two tenants, the empty-claim fail-closed, the FK cascades
  **and** audit-log immutability (deleting a user prunes its grants/keys but
  keeps its audit rows), the `status` / `''`-tenant CHECKs, that `users.id` is a
  uuid, and the compiled-3.5-policy claim integration above.
- **Mutation** (`scratchpad/mutate_as5.py`): five mutants (status literal, model
  literal, an FK cascade, the tenant policy predicate, an added audit FK) each
  fail a named test.

Nothing in-cluster: like `catalog-schema.sql` / `system-schema.sql`, this is a
standalone schema file, not a cluster resource, and applying it in-cluster would
mutate a shared DB (the shared-cluster guardrail).

## Downstream

Closing 2.4 unblocks **2.5** (`wamn-d8u`, the migration engine — the live
transactional apply of the DDL `wamn-ddl` emits) and **POC-DM1** (`wamn-521`).
4.2 (AuthN) populates `users` / `user_roles` / `api_keys` and injects the claims;
4.3 (AuthZ) reads `roles` / `permissions`.
