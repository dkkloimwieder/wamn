# POC-DM1 — data model via the catalog API

The **API-first build** of the POC "Material Receiving with Quality Hold" data
model (`docs/poc-material-receiving.md` §"Data model"), no UI — the P1 half of
the "built twice" requirement (DM2 rebuilds the same catalog through the schema
designer). It is also the **end-to-end acceptance test of the 2.5 migration
engine**: define a catalog → migrate it live → get data tables + RLS + seed.

- **Issue:** wamn-521 `[POC-DM1]`; **Epic:** wamn-8va (POC Material Receiving).
- **Crate:** `poc/dm1` (`wamn-dm1`) — composition + the live-apply gate.
- **Feeds:** wamn-srz `[POC-DM2]` (designer-UI rebuild of the same catalog).

## What it is — composition, not new logic

DM1 adds no engine code. It **composes the shipped tools** over three promoted
`deploy/` artifacts:

| Tool | Role | Artifact |
| --- | --- | --- |
| **2.5** `wamn-migrate` | migrate the catalog live (DDL + lifecycle advance + history, one txn) | `deploy/poc-material-receiving.catalog.json` |
| **3.5** `wamn-rls` | per-role RLS (site-scoping + ERP gate) | `deploy/poc-material-receiving.rls.json` |
| **3.6** `wamn-seed` | reference/seed data | `deploy/poc-material-receiving.seed.dataset.json` |
| **2.4** `app_system` | the personas' roles + the ERP api-key | `deploy/app-schema.sql` |

The catalog artifact is a **promotion of the wamn-catalog fixture**
(`crates/wamn-catalog/tests/fixtures/poc-receiving.catalog.json`), kept identical
by a drift guard — the same 8-entity model the crate tests validate is the one
DM1 migrates. `wamn_dm1::provisioning_sql(tenant)` composes migrate → RLS → seed
into one runnable script.

## The data model

The full POC catalog: `users` (**system entity** + a `cert_level` extension —
the "hard path"), `sites`, `suppliers` (pricing `standard_cost` flagged
`sensitive`), `materials` (exact-decimal, unit-bound specs — `numeric(5,2)`
`unit: pct`, `numeric(8,3)` `unit: kg`, never float), `receipts` (composite
unique `(receipt_no, supplier_id)`), `receipt_lines`, `quality_holds` (status
`open`/`disposed`/`escalated`, carries `site_id`), `dispositions`. Every table
gets the 3.2 tenant floor.

## RLS

Two rules, layered `AS RESTRICTIVE` on the tenant floor (they narrow within a
tenant, never widen):

- **Inspector hold site-scoping** — a `RolePredicate` on `quality_holds` for role
  `inspector`: `site_id = NULLIF(current_setting('app.site', true), '')::uuid`.
  Only the inspector role is constrained; a `quality-manager` is unrestricted
  ("managers unrestricted"); an inspector with no site claim is fail-closed.
- **ERP receipts gate** — a `RoleCommands` INSERT gate on `receipts`: only `erp`
  / `quality-manager` may insert receipts (an inspector INSERT is denied by the
  `WITH CHECK`).

## Two carried limitations

- **System-entity extension lands as a data-schema table.** wamn-ddl (3.2) emits
  a plain `CREATE TABLE` for every catalog entity, so the `is-system` `users`
  entity migrates to a data-schema `users` table carrying `cert_level` — a
  parallel table to the 2.4 `app_system.users`, not an `ALTER` of it. The
  extension is exercised at the catalog + DDL level; wiring it onto
  `app_system.users` is a **follow-up** (wamn-5x0.3).
- **Role/site RLS claims are inert until 4.2.** The `wamn:postgres` plugin injects
  only `app.tenant` today; the site-scoping (a new `app.site` claim) and the ERP
  `app.role` gate are correct SQL but deny until claim injection lands (the
  documented 3.5 deploy-order hazard). The gate proves them by setting the claims
  by hand — exactly as the plugin will (4.2).

The **field-level pricing mask** (inspectors cannot see `suppliers.standard_cost`)
is 4.3: DM1 migrates the `sensitive` flag; the mask itself is that item's work.

## Verification

```sh
cargo test -p wamn-dm1
cargo clippy -p wamn-dm1 --all-targets && cargo fmt -p wamn-dm1 --check
```

- **Pure (no DB):** the drift guard (promoted catalog == fixture), and compile
  checks that the RLS policy and the seed compile over the catalog and the
  migrate plans the full model (composite unique, `numeric(5,2)` + `unit`, the
  `users` extension, the tenant floor).
- **Live-apply gate** (`WAMN_DM1_PG_URL`, a superuser URL; skips when unset):
  applies `catalog-schema.sql` + `app-schema.sql`, runs the composed provisioning
  (migrate → RLS → seed), seats the `app_system` personas + ERP key, inserts the
  transactional fixtures, and asserts — under hand-set session claims — the
  migrate/seed landed, the composite unique fires, exact-decimal + unit specs
  survived, and the RLS enforces site-scoped reads (inspector at hq sees 2 holds,
  at west 1, a manager all 3, no-site 0), site-scoped writes (`WITH CHECK`), and
  the ERP receipts gate (erp may insert, inspector may not).

```sh
docker run -d --rm --name wamn-dm1-pg -p 5463:5432 -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=wamn postgres:18
WAMN_DM1_PG_URL=postgres://postgres:postgres@127.0.0.1:5463/wamn cargo test -p wamn-dm1
docker stop wamn-dm1-pg
```

- **Mutation** (`scratchpad/mutate_dm1.py`): four mutants (the site-scoping
  predicate, the ERP gate roles, an exact-decimal spec, the promoted-catalog
  drift) each fail a named test.

Nothing in-cluster — like the wamn-migrate / wamn-rls / wamn-seed gates, DM1 is a
catalog + schema deliverable proven by a throwaway Postgres; applying it
in-cluster would mutate a shared DB (the shared-cluster guardrail). The composite
unique / exact-decimal / tenant-floor DDL is wamn-ddl's, already mutation-tested
there; DM1's gate asserts they hold end to end.

## References

- POC spec: `docs/poc-material-receiving.md` (§Data model, traceability matrix).
- The tools: `docs/migration-engine.md` (2.5), `docs/rls-builder.md` (3.5),
  `docs/seed-data.md` (3.6), `docs/app-schema.md` (2.4).
