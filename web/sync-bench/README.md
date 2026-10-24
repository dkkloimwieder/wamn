# Sync bench

This package holds Epic 1b of [wamn offline and sync](../../docs/plan/wamn-sync.md).
It renders the WMS packaging screen from a `@wfe/sync` collection and measures two read shapes.

[`src/packaging-feed.ts`](src/packaging-feed.ts) is the feed and the collection of the `packaging` model.
It is the one file per model that the generator will emit later.
`list` calls the generated `query` read with the screen's list arguments.
`changes` returns no rows until the host serves the change feed (Epic 2b).
The collection key is `packaging:` and the list arguments as canonical JSON.

## Inputs

The package reads the generated WMS client from `apps/target/wamn/wamn_wms/client-ts`.
Build it with `wamn build apps/wamn_wms`, as [building](../../docs/operations/building.md#wamn-build) describes.

## Checks

```bash
cd web/sync-bench
pnpm run check
pnpm test
```
