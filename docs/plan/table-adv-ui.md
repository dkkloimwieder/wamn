# wamn advanced table — presentation

Sep 25, 2026

Companion to [wamn advanced table — data loading proposal](https://claude.ai/code/artifact/667beb8f-34cb-48f5-a0a4-d5a561810d29). This doc holds only the UI work that needs no platform change.

## Goal and boundary

Build the platform table in `web/ui` against today's platform. Nothing here changes a contract, the generator's server side, the runtime crates, or the wire.

- **In:** the table component, its state, its features, the client-side generator output that feeds it (the table definition), and the demo pages that exercise it.
- **Out:** streamed queries, the load shape, the server ceiling, filter operators, required band filters, custom report operations, pivot, saved views. Those live in the loading proposal and wait on platform work.
- The seam between the two is the **load state** below. The table reads only that. When streaming lands, the source behind the seam changes and the table does not.

## The seam: load state

One value in `web/runtime`, framework-free like `page.ts`. The table renders it and never fetches on its own.

| Field | Meaning |
| --- | --- |
| `rows` | the rows loaded so far, keyed by row id |
| `fullyRead` | true when the load ended and the server reported no rows beyond the cap |
| `busy` | a load is in progress |
| `refusal` | the refusal code or reason, or null |
| `startedAt`, `endedAt` | when the load started and ended |
| `cap` | the per-table cap in force |
| `generation` | increments on every new load; results from an older generation are dropped |

Rules the source must keep, whatever it is:

- Rows reach the table in batches, never one at a time. A new data reference rebuilds the whole TanStack row model.
- A duplicate row id fails the load.
- Changing the cap or the scope starts a new load with a new generation.
- Client operations (sort, refine filters, search, group, aggregates, totals, export) are enabled only when `fullyRead`.

## Data source until streaming lands

The existing page binding is the source. No new loader, no client-side paging loop.

- One page read with `limit = min(cap, page maximum)`. Today the page maximum is 100.
- `nextCursor === null` means fully read. A cursor left over means "Full dataset cannot be loaded", the same as `more: true` will.
- So every rule above can be built and tested now on lists of up to 100 rows. Fixture data for the demo stays under that.
- For windowing and client-operation performance, the demo page generates rows in memory (1k, 10k, 50k). That exercises the table, not the platform.
- When the streamed load lands, the source becomes the stream reader. The table does not change.

## Table definition

The client generator emits a table definition as data. The table in `web/ui` owns all behavior. Each increment adds only the fields it uses; nothing is emitted ahead of its consumer.

| Field | Added in increment |
| --- | --- |
| Mode: load or browse (default load) | 1 |
| Read binding, scope filters, sort fields | 1 |
| Row id (primary key) | 1 |
| Columns: field, label, type, display field for references | 1 |
| Default cap | 1 |
| Allowed and default aggregate per column, by type | 2 |
| Editable fields, actions (and whether each takes many rows), child tables with their scoping field | 4 |

Views and child tables refer to a table definition by name.

## Table rules

- **Feature bundle:** one static wamn bundle built from `gridFeatures` (`web/ui/src/grid.ts`), plus the client row models the table uses (sorted, filtered, grouped, expanded, faceted). No client pagination row model. Modes switch through options (`manualSorting`, `manualFiltering`), never by swapping bundles.
- **Row identity:** rows are keyed by the definition's row id (TanStack `getRowId`). Selection, expansion, inline edit and reload use it.
- **Rendering:** rows render through the Epic 17 list windowing. No table draws every row into the DOM.
- **Aggregates:** type decides which aggregates a column allows and which is its default. Numbers (int32, decimal, float) default to sum; dates and timestamps to max; everything else to count. Keys, references and the revision column default to count. The user can pick another per column; the choice is part of the view.
- **`int64` columns:** opaque by platform rule. Group by and count only.
- **Reference columns:** show the display field, not the UUID. Group and pivot label by the display field and key by the id.
- **Reload:** a new load keeps selection and expanded rows for rows that still exist, by row id. A child table keeps its rows only while its scope value is unchanged. An edit in progress blocks a new load until it is saved or dropped.
- **Select all:** selects loaded rows only. On a set that is not fully read, the UI says so.
- **Cap control:** a number input in the table toolbar, per table, held in memory. Default 1000. On a set that is not fully read the table shows "Full dataset cannot be loaded" and when the load ran.

## Features

Each feature follows the core rule: server work sets scope, client work needs a fully read set.

| Feature | Fully read | Not fully read, or browse | TanStack v9 |
| --- | --- | --- | --- |
| Sort | client | server, new load (declared sort fields only) | `rowSortingFeature`, sorted row model |
| Refine filters | client, any column | off | `columnFilteringFeature`, filtered row model |
| Search | client, visible columns | off (server search needs a contract change) | `globalFilteringFeature`, restricted to visible columns |
| Group + aggregates | client | off | `columnGroupingFeature`, `rowAggregationFeature`, grouped and expanded row models |
| Totals row | client | off | `rowAggregationFeature` |
| Column arrangement | client | client | existing `gridFeatures` |
| Child tables | own table | own table | `rowExpandingFeature` |
| Row actions, bulk actions, inline edit | on | on | `rowSelectionFeature` |
| CSV export | on | off | none |
| Views | all parts | server parts only | table state |

### Sort

- Header click sorts. On a fully read set it sorts in the client. Otherwise it changes scope and starts a new load.

### Filters

- **Scope filters:** the contract's declared `IN` filters, run on the server. Changing one starts a new load. Shown in a scope bar.
- **Refine filters:** any column, run in the client, only on a fully read set. Instant.
- The UI shows the two layers apart, so the user knows which one reads again.

### Search

- One search box over the visible columns, in the client. TanStack's global filter checks every column that allows it, hidden ones included, so the table restricts it to visible columns itself.

### Group and aggregates

- **Multi-level:** group by one or more columns. The order of the group columns is the nesting order; the user reorders them in a group bar above the grid.
- **Expand:** every group row at every level expands and collapses on its own. Expand all and collapse all apply per level. Needs `rowExpandingFeature` and `createExpandedRowModel()`.
- **Aggregates:** the column's chosen aggregate (default by type), shown on the group row at every level.
- **Date buckets:** grouping on a date or timestamp column picks a bucket: day, week or month. Done through a column grouping value, so the row shape does not change. The time zone and week start come from a resolved setting (browser, tenant or database); the table never hard-codes one.
- **Null group:** rows with an empty group value fall into one group labeled "(none)".
- **Group sort:** group rows sort by group value or by one aggregate, per level. Leaf rows keep the table sort.
- **Edits:** editing a grouped field moves the row to its new group. Both groups' aggregates recompute.
- **Expand state:** keyed by the ordered list of (column id, raw value) pairs, never by display labels. It survives a new load.

### Totals row

- A footer row with each column's chosen aggregate over all rows, without grouping.

### Column arrangement

- Order, hide, pin, resize. Works in every mode. Part of a view. Header menu for sort, hide and pin; a column panel for order and visibility.

### Child tables

- A child table is its own table definition: its own read, columns, cap, features, actions and views.
- It renders in the expanded area of a parent row. The parent row supplies its scope filter, through the Epic 7 `references`/`lists` link.
- It loads when the row first expands. Collapsing keeps its rows while its scope value is unchanged.
- The parent definition names its child tables. The child knows nothing of the parent beyond the scope filter.

### Row actions, bulk actions, inline edit

- **Row menu:** lists the served operations that take this row. The Epic 7 row-to-form mapping fills the form.
- **Bulk actions:** select rows, pick an operation, submit once. One call carries every selected row as one outer input per row, each run independently. It is not one transaction. Each row shows its own result; a refusal marks only its row.
- **Inline edit:** edit a cell in place; submit calls the model's update operation with the row's revision. A refusal marks the cell. A revision conflict shows on the row and keeps the typed value.
- **Editable cells:** fields the update operation accepts and the plan does not supply.
- **After a write:** an inline edit of a field that is neither a scope filter nor the server sort field replaces its row in place. Every other write starts a new load of the scope.

### Views

- A view is a named table state: columns (order, visible, width, pin), sort, scope filters, refine filters, group, chosen aggregates, cap.
- A view names only declared fields, so it is checked against the contract.
- On a set that is not fully read, only the server parts apply.
- Held in memory and in the URL. Only top-level tables go in the URL, keyed by table definition name (for example `?pallets.sort=created_at:desc&pallets.cap=5000`). Child table state stays in memory.

### Export

- CSV of a fully read set: visible columns, in view order, after refine filters. Values as shown; references by display field.

## Increments

Each increment is one epic, reviewed before the next is scoped. The epic gets one issue at a time. All of it is UI-chain work.

1. **Table shell.** The load state seam in `web/runtime` over the existing page binding. The table definition (mode, read, row id, columns, default cap) emitted by the client generator. The `DataTable` in `web/ui` on the static feature bundle, rendering through list windowing. Cap control, "Full dataset cannot be loaded", load time, refresh, generation guard. A demo page with fixture data and an in-memory 10k-row page.
2. **Client operations.** Sort, refine filters, search, multi-level grouping and aggregates, totals row, CSV export, all enabled only on a fully read set. Allowed and default aggregates emitted per column.
3. **Column arrangement and views.** Header menu, column panel, scope bar, views in memory and in the URL.
4. **Actions and child tables.** Row menu, bulk actions, inline edit, child tables in expanded rows. Editable fields, actions and child tables emitted in the definition.

When the streamed load lands (loading proposal, increment 1), one issue swaps the seam's source to the stream reader. No table change.

## Not in this doc

These wait on platform work and are specified in the loading proposal:

- Streamed queries: the contract, `shape` in the URL, the tagged wire, batching in the reader, caching headers, the 100,000-row server ceiling.
- Server search and filter operators beyond `IN` (`wamn-yxm6`), required band filters.
- Custom report operations and report display.
- Pivot.
- Saved views.
- Permissions, tree rows.
