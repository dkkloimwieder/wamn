# Web cache invalidation

Updated through: 2026-10-01, `main` at `43497e343`.

## 1. Goal

A write marks stale only the stored reads whose relations it writes. Today the browser read store marks every stored reply stale on any write, so each unchanged read then costs one 304. After this change, a write to `wms.location` leaves a stored read of `wms.product` fresh.

The rule uses facts the contracts already state. A read names the relations it selects from, and an authored command names the relations it inserts into and updates. The change makes the generated create, update and delete state theirs in the same form, and carries both sides into the client's `ResponseContract`.

## 2. Fixed rules

- The granularity is `schema.table`. The client does not compare columns.
- A read's relations are every relation its contract lists. A history table counts as its model table, the rule that the release already applies to a read's ETag (`crates/control/lib/src/publish_release/package_sources.rs:318-323`).
- A write's relations are the listed relations with insert fields, update fields or `delete: true`.
- A relation with `lock: true` and no insert or update fields is not written, and invalidates nothing.
- A write marks a stored read stale when the write's relations and the read's relations share one `schema.table`.
- A write whose contract names no relation marks every stored read stale, as today. This is the fallback, not a mode.
- A read whose contract names no relation is marked stale by every write, because the client cannot tell what it reads.
- The generated create, update and delete state their relations in the same member and the same form as an authored command. No reader branches on who wrote the contract.
- Bindings change only in `ResponseContract`. Every client that declares `client_package` is regenerated in the same commit as the emitter change.
- Known limit: a materializer runs after the write's response and can write other tables. The client cannot see those writes. A stored read of such a table stays fresh until its `max-age` ends, and then its ETag revalidation reads the change.

## 3. Current state

Measured on `main` at `43497e343` on 2026-10-01.

| Place | Today |
| --- | --- |
| Read store | `createReadStore` (`web/runtime/src/readCache.ts:47-135`) keys each reply by operation and GET target. `invalidate()` takes no argument, adds one to the generation and sets `freshUntil = 0` on every entry (`:121-126`). |
| Write path | `exchange` calls `reads.invalidate()` and then every `onWrite` listener after each non-GET request, whatever its outcome (`web/runtime/src/transport.ts:531-541`). The comment there names `wamn-fjdo`. |
| Listener | `Transport.onWrite?(listener: () => void)` (`web/runtime/src/wire.ts:123`). The listener gets no fact about the write. |
| Record labels | `createRecordLabels` reads every key asked so far again after each write (`web/ui/src/record-labels.ts:45-51`). Its read is the column's `recordRead.read`, a `get` (`web/ui/src/table/query-table.tsx:255-261`). |
| Other listeners | A generated detail (`readAgain`), a table load (`web/ui/src/table/table-load.ts:300-307`), an edit cell (`edit-cell.tsx:168`) and each form selector list read again after every write. The detail and the selectors go through the store. A table load streams and sends its own ETag. |
| Response headers | A `get` answers `no-cache`. A `query` or `projection` answers `max-age=10, stale-while-revalidate=60` (`crates/platform/engine/src/flow_http_routing.rs:851-870`). |
| `ResponseContract` | `resultClass`, `partialSchema`, `errors`, `replay`, `direct`, `type`, `transaction` (`web/runtime/src/wire.ts:30-42`). It carries no relation. |
| Read contracts | Every read contract lists `relations`. The generator writes them for a generated get and query (`crates/schema/generator/src/generate/contracts.rs:813-836`), and the author declares them for a projection. |
| Write contracts | An authored command lists `relations` with `insert_fields`, `update_fields`, `delete` and `lock` (`apps/wamn_receiving/generated/contracts/receiving/record_receipt.operation.json`). A generated create, update and delete carry `record.relation` only. |
| Served routes | Receiving serves 8 reads and 3 writes, and WMS serves 11 reads and 9 writes. All 31 routes are direct. |

What the rule changes, computed from the contracts on `main`, with each generated write counted as writing its own model table:

| Application | Write | Reads marked stale |
| --- | --- | --- |
| Receiving | `supplier/create` | 1 of 8 |
| Receiving | `purchase-order/update` | 4 of 8 |
| Receiving | `receiving/record-receipt` | 6 of 8 (`receiving.location` is lock-only) |
| WMS | `location/create`, `location/update`, `product/create`, `product/update` | 2 of 11 |
| WMS | `packaging/create`, `inventory/move` | 3 of 11 |
| WMS | `inventory/adjust`, `inventory/merge`, `inventory/split` | 7 of 11 |

The saving depends on the response headers. A stored `query` or `projection` that stays fresh answers with no request during its 10 seconds. A stored `get` revalidates on every read because of `no-cache`, so the store saves it nothing. A record label is different: a label that does not read again sends no request at all.

## 4. Design

### 4.1 Generator

The generated create, update and delete contracts gain `relations`, in the member and form that `StaticSqlRelationDeclaration` already gives a read and an authored command.

| Action | Relation |
| --- | --- |
| create | The model table. `insert_fields` are the columns the generated insert writes. |
| update | The model table. `update_fields` are the columns the generated update writes. |
| delete | The model table, with `delete: true`. |

`select_fields`, `lock` and `constraints` follow what the generated statement does. The contract bytes of these operations change, so their contract digests change. The read contracts do not change.

### 4.2 Client IR and bindings

The client IR reads `relations` from the served operation's contract. It carries two lists into the route's response:

- `reads`: for a read, its relations as `schema.table`, with a history table named as its model table. Empty for a write.
- `writes`: for a write, its written relations as `schema.table`. Null when the contract names no relation. Null for a read.

The TypeScript emitter writes them as two `ResponseContract` members, `reads: readonly string[]` and `writes: readonly string[] | null`, sorted, after `transaction`. No other byte of a binding changes.

### 4.3 Read store

- An entry keeps the `reads` of the request that stored it.
- `invalidate(writes)` takes the write's `writes`. Null marks every entry stale, as today. A list marks stale only the entries whose `reads` share one relation with it, or whose `reads` is empty.
- The generation rule stays as it is: a read that started before any write shares no request with a later read, and stores a reply that is not fresh. The narrowing applies to stored entries, not to reads in flight.
- `exchange` passes `request.contract.writes`.

### 4.4 Listeners and record labels

`onWrite` listeners receive the write's `writes`: `onWrite?(listener: (writes: readonly string[] | null) => void)`. A listener that takes no argument keeps its behavior, so no generated component changes.

`createRecordLabels` reads its keys again only when the write touches the label read's relations, by the same rule as the store. It takes the relations from `recordRead.read.contract.reads`. The intersection is one exported function of `web/runtime`, so the store and the labels use one definition.

## 5. Issues

One chain, the routes agent. Each issue lands with its tests.

1. Generator and IR. The generated create, update and delete contracts state `relations`. The client IR carries `reads` and `writes` into the response, and the emitter writes them into `ResponseContract`. Generator tests on the fixture cover the three generated write contracts, an authored command with a lock-only relation, and the history-table read.
2. `web/runtime` store rule. `ResponseContract` gains the two members. The store and `onWrite` take the written relations. Tests: an intersecting write marks the read stale. A non-intersecting write leaves it fresh. A lock-only relation invalidates nothing. A write with null `writes` marks everything stale. A record label reads again only on an intersecting write. Hand-written contracts in the runtime and component tests and the gallery gain the two members.
3. Regenerate the served applications. Receiving and WMS, and the component fixture. Their binding bytes change only in `ResponseContract`. Their contracts change only in the generated write contracts.
4. Closeout. `docs/architecture/execution.md` states the rule in place of "Any write marks every stored read stale", with the materializer limit. Workspace test run, times on the bead. This plan is archived when no unbuilt work remains.

## 6. Out of scope

- Column granularity.
- The materializer's later writes, described in section 2.
- The other `onWrite` listeners. A generated detail, a table load, an edit cell and a form selector list still read again after every write. Section 3 measures what this costs.
- A write by another session or another client. ETag revalidation covers it, as today.
