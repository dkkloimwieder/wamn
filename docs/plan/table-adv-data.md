# wamn advanced table — data loading proposal

Sep 24, 2026 · @D Kloimwieder

## Goal and scope

One platform table in `web/ui`, on TanStack Table v9 (`@tanstack/solid-table ^9.2.4`, Solid `^1.9.15`) and the Zaidan data grid. This spec settles how rows reach the browser, when client operations are allowed, and the features built on that.

- **Loading:** one streamed query per table, up to a per-table cap.
- **Features:** sort, filters, search, multi-level grouping and aggregates, totals row, pivot, column arrangement, child tables, row actions, bulk actions, inline edit, views, CSV export.
- **Reports:** custom authored operations; the table displays their rows.

## Current state

Today a table reads one keyset page of at most 100 rows at a time, and the client computes nothing.

| Fact | Evidence |
| --- | --- |
| Table features hold state only; no client row models. The release answers sort, filter and page. | `web/ui/src/grid.ts:1-13` |
| A query result is a whole list, `list<row>` plus `next-cursor`, so a reply passes through the component in one piece. | `crates/schema/generator/src/generate/wit.rs:236-241` |
| Component-model streams already work in the runtime: async functions return `stream<u8>` and `stream<object-name>`, drained with a bound. | `crates/platform/runtime/wit/deps/wasmcloud-blobstore/package.wit:98-104`, `plugins/wamn_blobstore/drain.rs` |
| The server reads `limit + 1` rows and returns a cursor only when more exist. | `apps/wamn_wms/data/src/page.rs:54-64` |
| The page maximum is 100, hand-written in 6 app files and repeated in the contract. | `MAX_PAGE_SIZE` in fixture, WMS, Receiving; `apps/wamn_wms/wamn.json:48` |
| Filters are per-field `IN` arrays. Sort is one declared field and direction. | `apps/wamn_wms/generated/client-ts/pallet.ts:205-232` |
| Sort is a form control above the grid, not a header click. | `apps/wamn_wms/generated/client-ts/components/pallet.tsx:524-533` |
| No text match mode; a search matches the whole value. | bead `wamn-yxm6` |
| Paging state is framework-free in the runtime; `web/ui` renders it. | `web/runtime/src/page.ts` |

## Core rule

Server scope decides which rows arrive. Client operations run only on a fully read set.

- **Scope** = server filters and sort, declared in the contract. Changing scope starts a new load.
- **Fully read** = the load ended with its outcome line, and the server reported no rows beyond the cap.
- A streamed load is one query, so a fully read set is consistent as of that query. This holds if the guest runs it as one statement; increment 1 verifies it.
- **Client operations** = sort, refine filters, search, group, aggregate, totals, pivot, export. Enabled only when fully read.
- A set that is not fully read shows rows and column arrangement only. No client total is ever shown on a partial set.

## Data modes

Three modes. Small data and banded data are the same mode: load up to the cap.

| Mode | Server | Client | Use |
| --- | --- | --- | --- |
| Load | scope filters and sort; streams rows up to the cap | everything, when fully read | small lists; large lists narrowed by a date or other band |
| Browse | scope filters and sort; keyset pages (today) | column arrangement only | very large lists, lookup |
| Report | a custom authored operation returns aggregate rows, with every level's subtotals | display, sort, hide, format, drill-down, export | reports, metrics |

- A band is a scope filter. Making one filter required (for example a date range) is increment 6.

## Loading

Each table loads its scope as one streamed query, up to a cap set per table in the UI.

### Contract

- **One operation, one export.** Every query export yields its rows as a stream, then one final record carrying the cursor (browse) or `more` (load). No new CRUD action, no second operation per model.
- **The host shapes the reply.** For a page it reads `limit` rows and returns `list` + cursor, as today. For a load it streams rows out as they arrive.
- **The request picks the shape.** One GET URL per query; the canonical query string carries `shape=page` or `shape=stream`. The URL alone keys the browser cache, so no `Vary: Accept`.
- **Bindings:** one binding per query with two calls, page and load.
- **Migration:** generated model queries move first; hand-written queries (fixture, WMS, Receiving data layers) move next. No query keeps a list-only export.

### Wire

- A chunked body, one tagged JSON value per line: `{"row":{…}}` for each row, then `{"outcome":{…}}` last. Never read by shape. This is a normal response read in pieces, not SSE.
- The server reads cap + 1 rows and stops. The outcome is completed with `more: true | false`, refused, or uncertain.
- **Before the first row:** a refusal (grant, input, limit above the ceiling) keeps normal HTTP status and the normal refusal body.
- **After the first row:** a failure becomes the terminal outcome line.
- A body that ends without the outcome line is a failed load, never a short complete set.
- **Cancel:** stopping at cap + 1, a browser abort and a client disconnect each close the component stream and end the database query.
- **Server ceiling:** 100,000 rows until measurement sets the real value. A larger request is refused before the first row.
- **Caching (Epic 16):** a stream response is `private`. It carries the weak list ETag from the per-model version table, known before the first row. A refresh of unchanged data gets a 304 with no body.
- **No buffering on the path:** the response sends `X-Accel-Buffering: no`, and compression flushes per batch, so no proxy or compressor holds rows back. A slow query must not hit a load balancer idle timeout before its first row.

### Browser

- `web/runtime` reads the body with `fetch`, a `TextDecoderStream` (a chunk can split a multi-byte character) and a line buffer (a chunk can split a line). A malformed line fails the load; it is never skipped.
- It owns the load state: rows, fully read, busy, refusal, load start and end times. It is framework-free, like `page.ts`; `web/ui` renders it.
- **Batching:** rows reach the table in batches, at most every 50 ms or once per animation frame, never one by one. A new data reference rebuilds the whole TanStack row model, so a per-row handover would rebuild it once per row.
- Rows render as they arrive, batch by batch. Client operations wait for the outcome line.
- Rows are keyed by row id. A duplicate row id fails the load: it means a broken query, and replacing would hide it.
- Each load has a generation number. A new load aborts the old stream, and any row or outcome from an old generation is dropped. Correctness does not depend on the abort.

### Cap

- A number input in the table toolbar, per table, held in memory. Default 1000. At most the server ceiling.
- `more: true` shows "Full dataset cannot be loaded". Client operations stay off.
- Changing the cap or the scope starts a new load.
- The measured ceiling replaces 100,000 after increment 1. Later the cap moves to client config, then platform config.
- The table shows when the load started and ended, and has a manual refresh.

## Table definition

The generator emits a table definition as data. The platform table in `web/ui` owns all behavior. Each increment adds only the fields it uses.

| Field | Added in increment |
| --- | --- |
| Mode: load or browse (default load) | 2 |
| Read binding, scope filters, sort fields | 2 |
| Row id (primary key) | 2 |
| Columns: field, label, type, display field for references | 2 |
| Default cap | 2 |
| Allowed and default aggregate per column, by type | 3 |
| Editable fields, actions (and whether each takes many rows), child tables with their scoping field | 5 |
| Report row contract: kind (row, subtotal, total) and group path | 7 |

Views and child tables refer to a table definition by name.

## Table rules

Rules every table follows, in every mode.

- **Feature bundle:** one static wamn bundle built from `gridFeatures`, plus the client row models the table uses (sorted, filtered, grouped, expanded, faceted). No client pagination row model. Modes switch through options (`manualSorting`, `manualFiltering`), never by swapping bundles.
- **Row identity:** rows are keyed by the definition's row id (TanStack `getRowId`). Selection, expansion, inline edit and reload use it.
- **Rendering:** only the rows on screen are drawn; the rest are drawn as the user scrolls. Epic 17 builds this list windowing; increment 2 starts after it.
- **Aggregates:** type decides which aggregates a column allows and which is its default. Numbers (int32, decimal, float) default to sum; dates and timestamps to max; everything else to count. Keys, references and the revision column are numbers but not measures, so they default to count; the plan already marks them. The user picks another per column in the UI, and the choice is part of the view.
- **`int64` columns:** opaque by platform rule. Group by and count only.
- **Reference columns:** show the display field, not the UUID. Group and pivot label by the display field and key by the id.
- **Reload:** a new load keeps selection and expanded rows for rows that still exist, by row id. A child table keeps its rows only while its scope value is unchanged. An edit in progress blocks a new load until it is saved or dropped.
- **Select all:** selects loaded rows only. On a set that is not fully read, the UI says so.

## Features

Each feature follows the core rule: server work sets scope, client work needs a fully read set.

| Feature | Fully read | Not fully read, or browse | Report | TanStack v9 | Contract change |
| --- | --- | --- | --- | --- | --- |
| Sort | client | server, new load | client on report rows | `rowSortingFeature`, sorted row model | none; multi-sort only if `max_fields > 1` |
| Filters | scope (server) + refine (client) | scope only | scope only | `columnFilteringFeature`, filtered row model | operators beyond `IN` (`wamn-yxm6`) |
| Search | client, visible columns | server, declared fields | off | `globalFilteringFeature`, restricted to visible columns | declared search fields + contains |
| Group + aggregates | client | off | from the operation, never recomputed | `columnGroupingFeature`, `rowAggregationFeature`, grouped and expanded row models | none |
| Totals row | client | off | from the operation, never recomputed | `rowAggregationFeature` | none |
| Pivot | client | off | off | none stock; built from grouping + aggregation | none |
| Column arrangement | client | client | client | existing `gridFeatures` | none |
| Child tables | own table | own table | off | `rowExpandingFeature` | none |
| Row actions, bulk actions, inline edit | on | on | off | `rowSelectionFeature` | none |
| CSV export | on | off | on | none | none |
| Views | all parts | server parts only | columns + scope | table state | none |

### Sort

- Header click sorts. On a fully read set it sorts in the client. Otherwise it changes scope and starts a new load.

### Filters

- **Scope filters:** declared in the contract, run on the server. Changing one starts a new load.
- **Refine filters:** any column, run in the client, only on a fully read set. Instant.
- The UI shows the two layers apart, so the user knows which one reads again.

### Search

- Fully read: one search box over the visible columns, in the client. TanStack's global filter checks every column that allows it, hidden ones included, so the table restricts it to visible columns itself.
- Otherwise: search runs on the server over declared fields. A filter declares the contains match as `"match": "contains"` (`wamn-yxm6`).

### Group and aggregates

- **Multi-level:** group by one or more columns. The order of the group columns is the nesting order; the user reorders them in a group bar above the grid.
- **Expand:** every group row at every level expands and collapses on its own. Expand all and collapse all apply per level. Needs `rowExpandingFeature` and `createExpandedRowModel()`.
- **Aggregates:** the column's chosen aggregate (default by type), shown on the group row at every level.
- **Date buckets:** grouping on a date or timestamp column picks a bucket: day, week or month. Done through a column grouping value, so the row shape does not change. The time zone and week start are configurable: browser, tenant setting or a database setting. The table reads the resolved value and never hard-codes one.
- **Null group:** rows with an empty group value fall into one group labeled "(none)".
- **Group sort:** group rows sort by group value or by one aggregate, per level. Leaf rows keep the table sort.
- **Edits:** editing a grouped field moves the row to its new group. Both groups' aggregates recompute.
- **Expand state:** keyed by the ordered list of (column id, raw value) pairs, never by display labels. It survives a new load.
- **Report mode:** the custom operation returns the group rows, every level's subtotals and the grand total, because an average of averages is wrong. The table never recomputes them; TanStack only nests, sorts and displays them. Each report row states its kind (row, subtotal, total) and its group path, so the table never guesses.
- **Report drill-down:** only on report dimensions that map to declared scope filters of a table. Opening such a group opens that table in load mode, with the group values as scope filters. Other groups do not drill down.

### Totals row

- A footer row with each column's chosen aggregate over all rows, without grouping. In report mode the grand total comes from the operation.

### Pivot

- Pick row fields, one column field, one aggregate.
- Built as columns from the distinct values of the column field, plus grouped aggregation. TanStack v9 has no pivot feature.
- A limit on distinct pivot columns is set by measurement, the same as the cap. Above it, the pivot is refused and the user narrows the scope.

### Column arrangement

- Order, hide, pin, resize. Works in every mode. Part of a view.

### Child tables

- A child table is its own table definition: its own read, columns, cap, features, actions and views.
- It renders in the expanded area of a parent row. The parent row supplies its scope filter, through the Epic 7 `references`/`lists` link.
- It follows the same rules as any table.
- It loads when the row first expands. Collapsing keeps its rows while its scope value is unchanged.
- The parent definition names its child tables. The child knows nothing of the parent beyond the scope filter.

### Row actions, bulk actions, inline edit

- **Row menu:** lists the served operations that take this row. The Epic 7 row-to-form mapping fills the form.
- **Bulk actions:** select rows, pick an operation, submit once. A binding already takes an array of requests (`web/components/src/sample.tsx`, `list(transport, [ … ])`), so one call carries every selected row: one outer input per row, each run independently. It is not one transaction. Each row shows its own result; a refusal marks only its row.
- **Inline edit:** edit a cell in place; submit calls the model's update operation with the row's revision. A refusal marks the cell. A revision conflict shows on the row and keeps the typed value. Depends on the update-form revision fix (`wamn-yzy7`).
- **Editable cells:** fields the update operation accepts and the plan does not supply.
- **After a write:** an inline edit of a field that is neither a scope filter nor the server sort field replaces its row in place. Every other write (bulk actions, row actions, edits to scope or sort fields) starts a new load of the scope.

### Views

- A view is a named table state: columns (order, visible, width, pin), sort, scope filters, refine filters, group, chosen aggregates, cap.
- A view names only declared fields, so it is checked against the contract.
- On a set that is not fully read, only the server parts apply.
- Held in memory and in the URL. Only top-level tables go in the URL, keyed by table definition name (for example `?pallets.sort=created_at:desc&pallets.cap=5000`). Child table state stays in memory.

### Export

- CSV of a fully read set or report rows: visible columns, in view order, after refine filters. Values as shown; references by display field.

## Increments

Each increment is one epic, reviewed before the next is scoped. Increment 1 is platform work; the rest are UI work.

1. **Streamed query (platform).** Built by epic `wamn-utci`. [Request execution](../architecture/execution.md) and [data access](../architecture/data-access.md) describe it, and the epic records its measurements. The owner set the ceiling at 100,000 rows on 2026-09-26. It is a browser bound, and it moves to client configuration with the cap.
2. **Load in the table (UI, after Epic 17 windowing).** Per-table cap control, "Full dataset cannot be loaded", generation guard, load times, refresh.
3. **Client operations on a fully read set.** Sort, refine filters, search, multi-level grouping and aggregates, totals row, CSV export.
4. **Column arrangement and views.** Header menu, column panel, views in memory and the URL.
5. **Actions and child tables.** Row menu, bulk actions, inline edit, child tables in expanded rows.
6. **Scope controls.** Server filters and sort in the UI. Filter operators beyond `IN` (`wamn-yxm6`). Required band filter.
7. **Report display.** The table over a custom report operation: nesting and totals from its rows, drill-down on mapped dimensions.
8. **Pivot.** Client-side on a fully read set.
9. **Saved views.**

## Out of scope

- Tree rows (a self-referencing model, such as a location hierarchy).
- Saving the cap anywhere but table memory.
- Permissions: showing only the actions and edits a user's grants allow. Deferred until the table is built out further.
- Paged loading up to the cap. Load mode streams; browse mode keeps keyset pages.
