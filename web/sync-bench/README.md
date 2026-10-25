# Sync bench

This package renders the WMS packaging screen from a `@wfe/sync` collection and measures two read shapes.

[`src/packaging-feed.ts`](src/packaging-feed.ts) is the feed and the collection of the `packaging` model.
`list` reads every page of the generated `query` read, up to 1000 rows, and returns the feed cursor of the first page.
`changes` calls the generated `changes` binding: the same read with its feed part.
A `cursor_expired` refusal throws `CursorExpired`, so the collection lists again.
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
Each run mounts one shape on a new page. The shapes alternate their order in each repeat, and the table shows the load average.

```bash
cd web/sync-bench
pnpm run build
node bench/run.mjs --out <results.json>
```

`bench/both.mjs` opens both shapes side by side at 100,000 rows and prints what each shows.

## Live screen

[`live.html`](live.html) is the packaging screen on a live host: shape A over the real feed.
Open `/urn:wamn:project-env:<org>:<project>:<env>:<instance>/live.html`, the audience path that application screens use.
Vite serves `live.html` at that path. A static deployment must do the same.
The outbox database uses the project and environment from that path and `userId` from the cookie session.
The page renews the session on load and offers sign-in when renewal fails.
The page calls the release under `/wamn` on its own origin. `?poll=` sets the follow interval in milliseconds.
The create form records a request with a UUIDv7 id. The row shows as tentative until the feed returns that id.
The status line shows pending and refused counts, the last send, the last feed read, and storage protection.
The page reads `navigator.storage.persisted()` and never requests storage protection.
The Not uploaded panel shows each refused request beside the current row.
Resubmit records the edited create with the same id and a new request hash before it discards the refused entry.
Discard removes the entry and reverts its tentative row.
[`bench/follow.mjs`](bench/follow.mjs) opens it in headless Chrome and forwards `/wamn` to the host with the caller's token.
That fixture supplies `WAMN_USER_ID` from its operator principal and `WAMN_AUD` from its environment coordinates with the test-only instance suffix `localapp`.
The driver uses them for its mocked session reply and page route because the fixture has no identity service.
The test `local_feed` in `apps/wamn_wms/tests` runs it against a local WMS host:

```bash
cd web/sync-bench && pnpm run build && cd ../..
WAMN_APPLICATION_COMPONENTS="$PWD/apps/target/virtualized/std-empty-environment" \
WAMN_FLOW_HTTP_COMPONENT="$PWD/apps/target/wasm32-wasip2/release/http_route.wasm" \
WAMN_NATIVE_C_NATS_BIN="$(command -v nats-server)" \
  cargo test --locked --offline -p wamn-wms-tests --lib local_feed:: -- --ignored
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
