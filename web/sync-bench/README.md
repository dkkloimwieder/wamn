# Sync bench

This package holds Epic 1b of [wamn offline and sync](../../docs/plan/wamn-sync.md).
It renders the WMS packaging screen from a `@wfe/sync` collection and measures two read shapes.

[`src/packaging-feed.ts`](src/packaging-feed.ts) is the feed and the collection of the `packaging` model.
It is the one file per model that the generator will emit later.
`list` calls the generated `query` read with the screen's list arguments.
`changes` returns no rows until the host serves the change feed (Epic 2b).
The collection key is `packaging:` and the list arguments as canonical JSON.

## Shapes

Both shapes render the generated packaging columns through one view, [`src/view.tsx`](src/view.tsx): virtual rows, then the groups by status and type and a totals row.

- Shape A, [`src/shape-a.ts`](src/shape-a.ts), reads the collection through `liveRows`. Groups and totals come from `@tanstack/db` live queries.
- Shape B, [`src/shape-b.ts`](src/shape-b.ts), keeps a Solid store keyed by id. Sort, filter, groups and totals are memos over it.

The summed column is `row_version`, a stand-in, because packaging has no other numeric column.

## Measurement page

[`src/main.tsx`](src/main.tsx) is the page. It fills the shapes from a fake feed of synthetic rows in the generated row type.
`?size=10000` or `?size=100000` picks the row count. `?shape=a` or `?shape=b` mounts one shape, and no `shape` mounts both side by side.
The apply button sends one batch of 100 changes (70 updates, 20 inserts, 10 deletes) to shape A through `applyChanges` and to shape B through its setter.

[`bench/run.mjs`](bench/run.mjs) drives the page in headless Chrome and prints the median of 5 runs per shape, size and metric.
Each run mounts one shape on a new page. The shapes alternate their order in each repeat, and the table records the load average.

```bash
cd web/sync-bench
pnpm run build
node bench/run.mjs --out <results.json>
```

## Inputs

The package reads the generated WMS client from `apps/target/wamn/wamn_wms/client-ts`.
Build it with `wamn build apps/wamn_wms`, as [building](../../docs/operations/building.md#wamn-build) describes.

## Checks

```bash
cd web/sync-bench
pnpm run check
pnpm test
```
