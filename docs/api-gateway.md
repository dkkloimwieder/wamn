# Generated REST API Gateway (4.1)

A per-project gateway that turns a project's **catalog** (3.1) into a REST
surface over the tables the **DDL compiler** (3.2) generates. A request becomes
**injection-safe parameterized SQL**, runs through the `wamn:postgres` capability
(2.2) under the host-injected `app.tenant` claim + the tenant floor (RLS), and
the row-set is shaped back into JSON.

- **Issue:** wamn-759 `[4.1]`; **Epic:** E4 Generated API.
- **Crate:** `crates/wamn-api` — the pure gateway logic (no host, no DB, no Wasm).
- **Component:** `components/api-gateway` — the `wasi:http` ⇆ `wamn:postgres` shell.
- **Gate:** `wamn-host apibench` — drives the component end to end against Postgres.
- **Consumers:** POC-F1 (the hold queue / disposition / ERP receipt flows), the
  SPA (6.x), and every generated-API consumer. Blocks 4.4/4.5/4.6/4.7.

## Shape

One gateway instance serves **one project** — a shared gateway would hold every
tenant's catalog and DB credentials (the worst blast radius); Wasm density +
scale-to-zero make per-project nearly free. The gateway reads a catalog
**snapshot** once from its project database (the `wamn_catalog` row) and memoizes
it (the NATS hot-reload doorbell is 4.4).

```
GET    /api/rest/{entity}            list   (filter / sort / paginate / expand)
GET    /api/rest/{entity}/{id}       get    (+ expand)
POST   /api/rest/{entity}            create (body = JSON object) -> 201 + the row
PUT    /api/rest/{entity}/{id}       replace (full body)         -> 200 + the row
PATCH  /api/rest/{entity}/{id}       update  (partial body)      -> 200 + the row
DELETE /api/rest/{entity}/{id}       delete                      -> 204
```

`PUT` is a **full replace**: every writable field is set, an omitted optional
field is reset to its column `DEFAULT` (NULL for a nullable no-default column,
else its declared default — a keyword, never a param), and an omitted *required*
field is a `400` (rejected exactly as create rejects it). `PATCH` is a **partial
merge**: only the fields present in the body are set (an empty body is a `400`).

### Query surface (list)

PostgREST-ish, all validated against the catalog:

| Feature   | Syntax                              | Notes |
|-----------|-------------------------------------|-------|
| filter    | `?col=eq.val` or `?col=val` (= eq)  | operators `eq neq lt lte gt gte like in`; `in` is `?col=in.a,b,c` |
| sort      | `?sort=col,-col2`                   | `-` = descending; default order is `id` |
| paginate  | `?limit=&offset=`                   | `limit` capped at a max page size (the hard limiter is 4.6) |
| expand    | `?expand=rel,rel2`                  | one level; a to-one relation embeds an object, a to-many an array |

Unknown entity / field / relation, or a bad value (non-uuid id, non-exact
decimal, enum not a variant), is rejected `4xx` **before any SQL is built**.

## Safety invariants (the S2 injection lesson, by construction)

- **Values are always `$n` parameters.** Every request value — a filter value,
  an `id`, a body field — is bound (never string-interpolated). The compiler
  returns `(sql_template, params)`; a `ParamBuilder` keeps placeholder numbers in
  lockstep with the parameter vector.
- **Identifiers are always catalog-allowlisted.** Every table/column/relation
  name comes from the catalog and is quoted with `wamn_ddl::sql::quote_ident`
  (the single quoting source of truth). A request string that does not resolve to
  a catalog field/relation never becomes an identifier.
- **Tenant isolation is the database's job.** Every query runs under the injected
  `app.tenant` claim + the 3.2 floor's RLS policy. Writes set
  `tenant_id = current_setting('app.tenant', true)` **server-side** — no tenant
  value is ever taken from the request (so the floor's `WITH CHECK` is satisfied
  without a param and without changing 3.2). `UPDATE`/`DELETE` scope through the
  policy's `USING` clause.
- **`tenant_id` is never projected;** `numeric` stays an exact-decimal **string**
  end to end — in a bound parameter and in the response — honoring the 3.1
  no-float rule.

## Catalog cross-references vs SQL identifiers

The catalog references entities/fields by **id** (`Reference{entity}`,
`Relation.from/to/through` are entity ids, `from_field` is a field id), while the
physical SQL identifiers 3.2 emits are the **names**. The router resolves by id
and emits by name — the one subtlety worth stating.

## Component

`components/api-gateway` exports the standard `wasi:http/incoming-handler`
(the `wasi:http/proxy` world), so wasmCloud routes HTTP straight to it, and
imports only `wamn:postgres` for data. It has **no `wasi:sockets` and no outbound
`wasi:http`** — the 2.6 DB-path egress boundary holds (the `wasi:cli`/`random`
imports are the Rust std shim). The routing/SQL/shaping logic is the `wamn-api`
crate compiled in; the component only moves bytes across the two capability
boundaries (parse request → compile → `client::query`/`execute` → shape).

The catalog snapshot lives in a `wamn_catalog(tenant_id, document jsonb)` table in
the project database; the gateway reads it under RLS. The control plane / hosting
(2.x) writes that row when it deploys the gateway.

## Scope (what 4.1 is NOT)

CRUD + one-level relation expansion only. **Not**: GraphQL (`/api/graphql`, P2);
aggregations / arbitrary joins / computed views (post-GA); authentication —
JWT / API-key → `app.user_id`/`app.role` claims (4.2, so v1 is tenant-scoped but
not user-authenticated); field-level read/write masks — the `sensitive` flag is
carried through, not applied (4.3); the hot-reload doorbell (4.4, v1 reads the
snapshot once); OpenAPI / SDK generation (4.5); rate/cost limits (4.6, v1 only
caps page size); the in-process invocation path (4.7).

## Verification

```sh
cargo test -p wamn-api
cargo clippy -p wamn-api --all-targets && cargo fmt -p wamn-api --check
```

The crate tests assert the emitted SQL + params over the POC catalog (CRUD
shapes, filter/sort/paginate, both expansion directions, exact-decimal
round-trip) and the security negatives (an injection value stays a parameter, an
unknown identifier is a typed 4xx, managed columns cannot be set).

The `apibench` gate proves the whole path against a real Postgres — it emits the
3.2 floor for a small catalog, provisions a fresh ephemeral schema through a
superuser, seeds two tenants, and drives the component through its wasi:http
export:

```sh
# Local iteration (throwaway container):
docker run -d --rm --name wamn-api-pg -p 5455:5432 -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=wamn postgres:18
REL=components/target/wasm32-wasip2/release
WAMN_PG_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:5455/wamn \
  ./target/release/wamn-host --log-level error apibench \
  --api-gateway $REL/api_gateway.wasm \
  --database-url postgres://wamn_app:wamn_app@127.0.0.1:5455/wamn --mode all
docker stop wamn-api-pg
```

The in-cluster gate of record is `deploy/apibench-job.yaml` (co-located with
Postgres, no CPU limit — the S2 CFS lesson; `WAMN_PG_ADMIN_URL` is the superuser
used only to provision the ephemeral schema).

## Serving (4.1b)

`apibench` drives the component *in-process* (via `ProxyPre`, the exact mechanism
wash-runtime uses) to prove the SQL/CRUD/RLS semantics. 4.1b makes it actually
**serve over the network**: the gateway is deployed as a real `wasi:http`
`WorkloadDeployment`, one instance per project.

**No new serving code is needed.** wash-runtime ships an inbound HTTP server
(`--http-addr`, on port 80 in `deploy/values-wamn.yaml`); its `DynamicRouter`
routes a request whose `Host` header matches a workload's
`hostInterfaces[].config.host` to that component's `wasi:http/incoming-handler`
export. So the gateway deploys exactly like `deploy/hello-workload.yaml` — a
`Service` + a `WorkloadDeployment` — with the DB claims in
`components[].localResources.config`:

- `wamn.tenant` → the `app.tenant` claim the plugin injects (RLS isolation);
- `wamn.project` → the per-project pool (`default` → `WAMN_PG_URL`);
- `wamn.schema` → the `search_path` the gateway's unqualified queries resolve in.

These are host-injected at `on_workload_item_bind` and non-spoofable by the guest.

### Component packaging (local OCI registry)

The operator pulls `components[].image` through wash-runtime's own OCI client
(not containerd), so a `kind load`ed image is invisible to it — the component
must live in a registry the host pods can reach. `deploy/registry.yaml` is a
plain-HTTP registry at `registry.wamn-system.svc.cluster.local:5000`; the host
runs with `--allow-insecure-registries` (a wamn-host flag wired through
`deploy/values-wamn.yaml` `hostGroups[].extraArgs`) so it can pull over HTTP. The
registry addresses artifacts by repo path (hostname-independent), so you `wash
push` via a port-forwarded `localhost` and the host pulls via the Service DNS
name at the same repo path.

### Catalog snapshot (`publish-catalog`)

The gateway loads its catalog from `SELECT document::text FROM wamn_catalog LIMIT
1` (RLS-scoped, `search_path`-resolved). In production the schema-designer→gateway
seam writes that row on catalog apply/promote (3.4); the `publish-catalog`
subcommand provides the mechanism: it reads a catalog JSON, `Catalog::to_json`s
the canonical document, and UPSERTs it under the project's tenant via a superuser
connection (bypassing the snapshot table's RLS + the runtime role's SELECT-only
grant). `--provision` additionally stands up the schema + the 3.2 tenant floor
when absent, and `--seed` loads the bundled two-tenant demo rows — everything
**additive** (`CREATE SCHEMA IF NOT EXISTS`, a dedicated `api_proof` schema, no
drops), so it is safe on a shared, durable Postgres.

### Proof of record (`apiproof`)

`apiproof` drives the **deployed** gateway over real HTTP (same assertions as
`apibench`, over the Service — routed on the `Host` header), from the same
`wamn-host` image (no external client image to pull), mirroring the S4
`nodebench --hop-url` cross-pod gate. `apibench` (in-process) stays the
exhaustive value-semantics regression; `apiproof` proves the network serving path
end to end.

```sh
# In-cluster proof (needs the kind 'wamn' cluster + operator + postgres):
docker build -t wamn-host:dev . && kind load docker-image wamn-host:dev --name wamn
kind load docker-image registry:2 --name wamn
kubectl -n wamn-system apply -f deploy/registry.yaml
kubectl -n wamn-system rollout status deploy/registry --timeout=60s
kubectl -n wamn-system port-forward svc/registry 5000:5000 &
wash push localhost:5000/wamn/api-gateway:dev \
  components/target/wasm32-wasip2/release/api_gateway.wasm --insecure
# The host group gains --allow-insecure-registries + WAMN_PG_URL:
helm upgrade --install -n wamn-system wamn \
  oci://ghcr.io/wasmcloud/charts/runtime-operator --version 2.5.2 \
  -f deploy/values-wamn.yaml
kubectl -n wamn-system rollout status deploy/hostgroup-default --timeout=120s
# Provision the project schema/floor + seed + publish the snapshot:
kubectl -n wamn-system create configmap proof-catalog \
  --from-file=proof-catalog.json=deploy/proof-catalog.json
kubectl -n wamn-system apply -f deploy/publish-catalog-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/publish-catalog --timeout=120s
# Deploy the gateway workload, then prove it serves:
kubectl -n wamn-system apply -f deploy/api-gateway-workload.yaml
kubectl -n wamn-system apply -f deploy/apiproof-job.yaml
kubectl -n wamn-system wait --for=condition=complete job/apiproof --timeout=180s
kubectl -n wamn-system logs job/apiproof
```

Scope: v1 reads the snapshot once (4.4 adds the hot-reload doorbell); it is
tenant-scoped, not user-authenticated (4.2); it points at the shared Postgres as
a single default project (per-project provisioning is 2.3/10.x).

## References

- Plan: `docs/platform-plan.md` §Epic 4 (4.1), §POC (POC-F1).
- Catalog model (routes/columns/types): `docs/catalog-model.md`, `crates/wamn-catalog`.
- Tenant floor (the target tables): `docs/ddl-compiler.md`, `crates/wamn-ddl`.
- Data path: `docs/wamn-postgres.wit`, the S2/2.2 plugin.
