# wamn offline and sync

Oct 23, 2026 · @D Kloimwieder

## Purpose and scope

Operational screens keep working on a spotty or absent network, and every screen loads from a local copy that is kept current by a change feed instead of re-reading pages. Both come from one mechanism: a browser store fed by per-model changes, with writes recorded locally and replayed as ordinary operations.

In scope:

- a change feed per model, served by the host over the existing read route layer
- a browser store with live queries, incremental over changes, with a Solid 2 adapter owned by wamn
- a write outbox: local recording, ordered replay, refusal handling
- the app shell and local storage rules for Chrome on PC and tablet; graceful degradation elsewhere
- the generator additions that make all of this generated, not hand-written per screen

Out of scope: a native app, conflict merging beyond refusal-and-resolve, server-side incremental views, cross-user collaboration on one record, and the local (SQLite) target. Those are listed under Deferred.

## Terms

| Term | Meaning |
| --- | --- |
| change feed | A read, generated per model, that returns the rows changed since a cursor, newest last, plus the next cursor. |
| cursor | An opaque string a client hands back to continue a change feed. Never a Postgres value. |
| snapshot | The result of a list read used to fill a collection before the feed takes over. |
| collection | The browser-side set of rows for one model, keyed by id, held in `@tanstack/db`. |
| live query | A query over one or more collections whose result updates incrementally per change. |
| adapter | The wamn-owned Solid 2 code that turns a collection or live query into a store the UI reads. |
| outbox | The local, durable queue of operation calls recorded while the host could not be reached. |
| replay | Sending outbox entries to the host in recorded order after the network returns. |
| refusal | A non-2xx reply to an operation with a code the UI shows; the entry leaves the outbox. |
| shell | The built app assets (HTML, JS, CSS) a service worker serves without a network. |
| take offline | An explicit user action that fills collections for a chosen set of records before leaving coverage. |

## Requirements: the two cases

Two cases, one mechanism. The second adds durability and a bootstrap step; it does not add a second design.

|  | Case 1: spotty network | Case 2: isolated field work |
| --- | --- | --- |
| Outage length | seconds to minutes | hours |
| Tab | stays open | may be closed and reopened with no network |
| Reads | serve the local collection; refresh when the feed resumes | same, over a set the user took offline beforehand |
| Data shape | any operational screen | a small set of static orders, read-only locally |
| Writes | any operation; queued and replayed | new records against an order; no edits to the order itself |
| Conflict rate | low; `row_version` refusals possible | near zero: append-only against a static parent |
| Needs beyond case 1 | none | service worker for the shell, persistent storage, explicit take-offline, upload status on every screen |

Stated requirements:

- A screen that was open when the network dropped keeps rendering its data and accepts writes.
- A write made offline is never lost: it is durable before the UI reports it recorded.
- Replay sends entries in recorded order; an entry is sent at most once effectively (the host dedupes on the request hash).
- A refusal on replay stops nothing but its own entry; the user sees it and resolves it.
- Every screen that loads from a collection shows data on first paint when a local copy exists, then updates.
- Supported devices are Chrome on PC and tablet. Other browsers get the online behavior and a one-line notice; nothing breaks.
- The frontend never learns backend execution semantics. It sends operations and reads rows; whether a resend is safe is the host's concern.

## Architecture

The host stays Rust and generated; nothing new runs beside it. The browser gains a store, a feed client and an outbox: the generic engine as `@wfe/sync`, the wamn-specific parts in `web/runtime` and the generated packages (see Ownership).

&#91;embedded content: offline and sync · browser and host parts\]

A screen never calls the host directly: reads come from collections, writes go through the outbox, and both reach the host over the route layer that already exists. Incremental work runs in the browser (`@tanstack/db` live queries); the host only produces changes.

Vendored packages, framework-free, MIT: `@tanstack/db` and `@tanstack/db-ivm`; their only dependency is `@tanstack/store`. TanStack Query is not involved (it is a peer of `@tanstack/query-db-collection` only, which is not vendored), so the standing rule "small store, not TanStack Query" holds. Not vendored: `@tanstack/solid-db` (Solid 1 only), `@tanstack/offline-transactions` (the outbox is @wfe/sync's, see Ownership). Whether `@tanstack/db` stays is decided by Epic 1's measurement against a Solid-only alternative (a store map with memo-derived views); the post-write re-read store from the HTTP-reads work is superseded by the feed echo either way.

## Ownership: `@wfe/sync` and wamn

The engine is generic and lives in the wfe repo as a new package, `@wfe/sync`, beside `@wfe/components`, `@wfe/form` and `@wfe/generator`. wamn pins it and supplies everything that names a model, route or operation. The rule for every line of code: if it mentions a wamn model, route, operation, header or database name, it is wamn's; if it takes a `Feed`, a `Collection` or an `OutboxEntry`, it is `@wfe/sync`'s.

### What `@wfe/sync` is told about its consumer

The consumer is wamn: a Rust host serving generated per-model reads (`list`, `changes`) over GET with ETags and 304s, and operations over POST deduplicated by a request hash the client computes. UUID-keyed rows carry a client-generated UUIDv7 `id` and an integer `row_version`. Supported browsers are Chrome on PC and tablet; other browsers must degrade, never break. Solid 2 (`2.0.0-rc.18` or later) is the framework. That is the whole context; nothing else about wamn is needed to build the package.

### `@wfe/sync` public surface

```ts
export interface Row { id: string; row_version: number }

export type Change<T extends Row> =
  | { id: string; row_version: number; deleted: true }
  | { id: string; row_version: number; deleted: false; data: T }

export interface Feed<T extends Row, S> {
  list(scope: S, signal?: AbortSignal): Promise<{ rows: T[]; cursor: string }>
  changes(scope: S, cursor: string, limit?: number, signal?: AbortSignal):
    Promise<{ rows: Change<T>[]; next_cursor: string; complete: boolean }>
  moved?(scope: S, since: string, signal?: AbortSignal): Promise<boolean>   // cheap poll; absent means changes runs on the timer
}

export interface Persistence {
  load(key: string): Promise<{ rows: unknown[]; cursor: string; saved_at: string } | undefined>
  save(key: string, rows: unknown[], cursor: string): Promise<void>
  remove(key: string): Promise<void>
}

export interface Trigger { subscribe(scopeKey: string, fire: () => void): () => void }   // poll now; an event channel later

export function collection<T extends Row, S>(feed: Feed<T, S>, scope: S, options: {
  key: string                      // (model, scope) encoded by the caller; the persistence key
  persistence?: Persistence
  trigger?: Trigger
  pollMs?: number                  // default 10000
}): Collection<T>                  // @tanstack/db Collection, filled and followed

export function liveRows<T extends Row>(c: Collection<T>): Store<Record<string, T>>
export function liveList<T extends Row>(c: Collection<T>): Store<T[]>
export function syncStatus(c: Collection<unknown>): Accessor<'loading' | 'ready' | 'error'>

export type OperationCall = {
  operation: string                // opaque to the package; passed to Send
  request: unknown
  request_hash: string             // computed by the caller
  expect: { model: string; id: string }
  append_only: boolean
}
export type Reply =
  | { ok: true }
  | { ok: false; retriable: boolean; refusal?: { code: string; text: string } }
export interface Send { (entry: OutboxEntry, signal: AbortSignal): Promise<Reply> }

export function outbox(options: {
  database: string                 // IndexedDB name, chosen by the caller
  send: Send
  probe: () => Promise<boolean>    // true when the host answered
  concurrency?: number             // default 4, across records; 1 within a record
}): Outbox

export function recordWrite<T extends Row>(c: Collection<T>, o: Outbox): {
  rows: Store<Record<string, T>>
  write: Action<[OperationCall, Partial<T> & { id: string }]>
}

export function shell(options: {
  persist?: boolean                // call navigator.storage.persist(); default true
  onUpdate?: (reload: () => void) => void
}): Promise<{ worker: boolean; persisted: boolean; quota: { usage: number; quota: number } }>
```

The package never computes a request hash, never builds a URL, never reads a token and never names a header. Those arrive through `Feed`, `Send` and `probe`.

### What wamn builds on it

- per-model `Feed` from the generated bindings: `receiptFeed = { list, changes, moved }`, with `moved` reading the per-model version
- the `Send` that posts the operation with the request hash as the idempotency value and the current session token; maps the host's reply to `Reply`
- the `probe` against `/api/health`
- `Persistence` over IndexedDB, one database per (application, environment, user)
- generated `receiptCollection(scope)` and `recordReceipt` calling the package
- the release digest in `meta` and the reload rule; the take-offline action; the status surface in the app shell

### Section ownership

| Section | Owner |
| --- | --- |
| Change feed (host) | wamn host |
| Browser store and live queries | `@wfe/sync`, except `fill`'s page read and the scope encoding, which are wamn's `Feed` |
| Solid 2 adapter | `@wfe/sync` |
| Writes: outbox, replay, conflicts | `@wfe/sync` for the queue; wamn for `Send`, ids, numbers, the refusal panel |
| Shell, storage, devices | `@wfe/sync` for `shell()` and the worker; wamn for the manifest, database names, take-offline |
| Session and authorization | wamn |
| Release compatibility | wamn |
| Generator and bindings | wamn |

Two specs, two agents: the `@wfe/sync` spec is this document's `@wfe/sync` sections with the consumer context above; the wamn spec is the rest, building to that surface.

## Change feed (host)

Every model with a feed gets one generated read, `changes`, that returns rows changed since a cursor. `changes` is the list read with its feed part (`feed: { cursor, limit }`): one operation, one export and one permission row, on the list's GET route. It has the list's authorization, `private, no-cache` and a weak ETag on `next_cursor`. No new service, no replication slot, no push channel in this increment.

### Contract

```
changes(<the same arguments as list>, cursor: string?, limit: int32?) ->
  { rows: [{ id, row_version, deleted: bool, data: Row? }], next_cursor: string, complete: bool }
```

- `cursor` absent: the read starts from the current end of history and returns no rows. The client takes a snapshot with `list` first, then opens the feed with the cursor that `list` returns in its outcome (see below).
- `rows` follow transaction and id order. The server collapses changes to one entry per id with its latest state.
- `deleted: true` carries `id` and `row_version` only; `data` is absent.
- `complete: false` means the page stopped below xmin and more rows exist. The client continues with `next_cursor` within the same pass.
- `limit` defaults to the model's declared page maximum; the platform ceiling applies.

### Cursor

The cursor is an opaque string that encodes the model name and transaction with URL-safe base64.
The frontend never parses it.
All transactions below the snapshot xmin ended before the snapshot.
A cut page with only rows below xmin carries its last transaction and id.
A page with any visible row at or above xmin returns xmin without an id and sets `complete` to true.
An empty page keeps the incoming cursor.
Postgres internals stay inside the opaque cursor.

### Snapshot handoff

`list` gains a `cursor` field in its outcome: the snapshot xmin of the list statement, encoded as above. A client that stores a `list` result and then calls `changes(cursor)` sees every change committed after that snapshot and none before. This is the only ordering guarantee the feed makes and the only one the browser store needs.

Implementation (Epic 2b): the list statement returns the cursor as a column, so the rows and the cursor come from one snapshot. The generator emits both; nothing is hand-written per model.

### Source

The per-relation history tables and their trigger functions already exist on main.
`changes` reads history entries at or after the cursor transaction.
One snapshot supplies xmin, xmax, and the visibility test.
Entries qualify below xmin, or below xmax when `pg_visible_in_snapshot` marks their transaction as visible.
The statement collapses entries to one row per id and orders them by transaction and id.
Rows below xmin page as before.

When those rows end inside a page, visible rows at or above xmin fill the remaining space up to `limit`.
These rows repeat on later calls until xmin passes them, and the client ignores an equal `row_version`.
Each row joins the live row for `data`.
A row that is gone, or no longer matches the filters, is marked `deleted`.
Migration `0021_remove_model_versions.sql` removed the per-model version table, so no cheap poll signal exists.

### Delivery

Increment 1 is polling: the feed client calls `changes` on a timer (10 s) while a screen holding that model is open. The `Feed` has no `moved` (Epic 2b ruling). Cache headers are `private, no-cache` with a weak ETag on `next_cursor`, so an unchanged feed is a 304.

The feed client is written as "a trigger fires, then call `changes`". Poll is the first trigger. The second is a platform event channel to the browser (SSE or WebSocket, one connection per client, carrying "model X moved" and other platform events), which is its own design, not part of this document: it serves more than the feed, so it is specified where its other consumers are. When it exists, the feed client registers it as a trigger; the contract, the collection and the outbox do not change. Poll stays as the fallback trigger when the channel is down.

### Authorization

`changes` is a read permission on the model, granted by role like `list`. The feed returns only rows the caller's active role may read; `deleted` entries are returned for any id the role could have read. The route layer already resolves the active role per (user, application).

## Browser store and live queries

One collection per model per application, filled by a snapshot and kept current by the feed. Live queries run over collections and update per change; screens read the query result, never the collection by hand.

### Collection

```ts
// @wfe/sync, collection.ts; wamn supplies feed, scope and persistence
export function modelCollection<T extends { id: string; row_version: number }>(
  model: ModelBindings<T>,          // generated: list, changes, key, scope
  scope: Scope,                     // the list arguments this collection covers
  persistence: Persistence,         // IndexedDB adapter, see Shell
): Collection<T>
```

- A collection covers one `scope`: the `list` arguments (filter, band) the screen declared. Two screens with the same scope share the collection; different scopes are different collections. The collection key is (model, list arguments) exactly; \`changes\` takes the same arguments as \`list\`, so the host filters the feed by the same scope and a row leaving the scope arrives as \`deleted\`.
- `getKey` is `row.id`. Every record has an id; there are no keyless records.
- The collection stores `row_version` on every row. An incoming change with `row_version` lower than the stored one is ignored (stale delivery after a reconnect).
- The custom sync source is wamn's (`collection.sync` hook of `@tanstack/db`): it runs `fill` then `follow`.

### Fill and follow

```ts
async function fill(c, model, scope, persistence) {
  const local = await persistence.load(c.key)             // rows + cursor, if any
  if (local) { c.begin(); for (const r of local.rows) c.write({ type: 'insert', value: r }); c.commit(); c.markReady() }
  const page = await model.list(scope)                     // snapshot; may be served from browser cache
  c.begin(); reconcileSnapshot(c, page.rows); c.commit(); c.markReady()
  return page.outcome.cursor
}

async function follow(c, model, cursor, signal) {
  for await (const batch of changes(model, cursor, signal)) {   // poll driven, see Change feed
    c.begin()
    for (const ch of batch.rows) {
      if (ch.deleted) c.write({ type: 'delete', key: ch.id })
      else if (newer(ch, c.get(ch.id))) c.write({ type: c.has(ch.id) ? 'update' : 'insert', value: ch.data })
    }
    c.commit()
    await persistence.save(c.key, c.toArray, batch.next_cursor)
    cursor = batch.next_cursor
  }
}
```

- `reconcileSnapshot` diffs the snapshot against the local rows: rows absent from the snapshot are deleted locally (the scope no longer includes them), rows present are inserted or updated by `row_version`.
- First paint comes from `persistence.load`, before any network. The snapshot then corrects it. A screen shows data in the time it takes to read IndexedDB.
- `follow` restarts from the saved cursor on reopen. If the host answers that the cursor is older than retained history (a refusal code `cursor_expired`), the client runs `fill` again.
- Persistence writes are debounced per collection (250 ms) and are a whole-collection write; collections are bounded by the existing per-table record cap, so a whole write stays small.

### Live queries

Screens declare queries with the `@tanstack/db` query builder: `from`, `where`, `join`, `groupBy`, aggregates, `orderBy`. The IVM engine (`@tanstack/db-ivm`) keeps the result current per change.

- The advanced table's client operations (sort, refine filters, search, grouping, aggregates, totals) move from recomputation per load to live queries over the collection. The existing numeric rules stay (exact decimal sums, half-away-from-zero rounding); they run inside aggregate functions handed to the query builder.
- A query over two models (child rows, references) is a `join` over two collections; the reference collection is filled by its own scope (the ids the parent rows name).
- Query results are collections too; the adapter reads them the same way.

### Memory

A collection holds its rows once; a live query holds its result once. Rows are plain objects shared by reference between the collection and query results. The 100,000-row browser ceiling already measured stays the bound; the per-table cap is the per-collection cap.

## Solid 2 adapter

The adapter lives in `@wfe/sync` and is the only place Solid and `@tanstack/db` meet. `@wfe/components` never imports it; wamn's generated packages do. Two read shapes and one write shape.

### Solid 2 surface the adapter uses

Everything below is `solid-js` at `2.0.0-rc.18` or later (stores are exported from `solid-js`; the web runtime is `@solidjs/web`). The package uses nothing outside this table.

| Primitive | Signature | Use here |
| --- | --- | --- |
| `createStore` | `createStore(value)` → `[store, set]`; `set(draft => { ... })` | `liveRows`: the keyed row map; draft-first setter applies a change batch in one write |
| `createProjection` | `createProjection(fn, seed?, { key })` → read-only derived store; `fn` may return a value, a promise or an async iterable; result reconciled by `key` | `liveList`: ordered rows for small sets |
| `from` | `from(source)` where `source` is `(next, error, complete) => unsubscribe`, an Observable, or an AsyncIterable → AsyncIterable for a computation; keeps the latest value; closes on re-run or disposal | bridges `subscribeChanges` into `createProjection` |
| `createOptimisticStore` | `createOptimisticStore(fn, seed?, { key })` → `[store, set]`; writes inside an `action` are tentative and revert or reconcile when the transition settles | `recordWrite`: the tentative row over the authoritative collection |
| `action` | `action(function* (...args) { ... })`; `yield` a promise or a reactive wait; the transition stays open until the body returns | the write flow: tentative write, enqueue, wait for echo |
| `until` | `until(() => predicate, { timeout?, signal? })` → promise; yielded in an action it holds the transaction until the predicate settles truthy on authoritative data; rejects when the landing is an error | waits for the feed echo of the written row; rejects on refusal |
| `refresh` | `refresh(target)` → promise of the next quiescent state; a quiet re-ask, not pending | re-ask a derived store after `cursor_expired` → `fill` |
| `affects` | `affects(target, key?)` marks data that in-flight work will change, so it reads pending | optional: mark a row while its entry is `sending` |
| `isPending` | `isPending(() => expr)` → boolean while a change to `expr` is in flight | "updating" marks; never for outbox state, which is the entry's own |
| `latest` | `latest(fn)` peeks the in-flight value during a transition | status readers that must not suspend |
| `<Loading>` | `<Loading fallback={...} on={...}>` initial-readiness boundary; `on` controls re-showing on revalidation | first `fill` of a collection |
| `<Errored>` | `<Errored fallback={err => ...}>` | feed and `fill` failures |
| `createEffect` | `createEffect(compute, apply, { defer? })`, apply may return cleanup; `{ effect, error }` bundle form | pushing status to the outbox panel; never to write stores |
| `createMemo` | `createMemo(fn, { lazy?, unobserved? })`; `lazy` autodisposes at zero subscribers | `syncStatus`; sort/filter views in the Solid-only alternative |
| `onCleanup`, `onSettled` | cleanup registration in an owned scope; `onSettled` runs after render settles and may return cleanup | unsubscribe from `subscribeChanges`; worker registration |
| `flush` | `flush()` applies batched writes now | tests only |
| `snapshot` | `snapshot(store)` → plain value | persisting rows; building a request from a store row |
| `reconcile` | `reconcile(value, key)` | the Solid-only alternative's bulk replace |

Rules the adapter must respect, all enforced by dev diagnostics:

- writes are batched by microtask; a setter's value is visible after the flush, so a test that reads right after a write calls `flush()`
- a write inside a reactive scope throws (`PRIMITIVE_IN_EFFECT_CALLBACK`, owned-scope write); subscription callbacks and event handlers are outside any scope and may write
- a store proxy written outside its setter is ignored and warned (`STORE_WRITE_OUTSIDE_SETTER`); every row mutation goes through `set(draft => ...)`
- a top-level reactive read in a component body warns; reads live in JSX, memos or effects
- `createEffect` has no single-argument form; compute and apply are separate
- `createResource`, `batch`, `on`, `produce`, `createComputed`, `Suspense`, `Index`, `onMount`, `mergeProps`, `splitProps` do not exist; the table above is the replacement set

### `liveRows`: delta-applied store (tables)

```ts
import { createStore, onCleanup } from 'solid-js'
import type { Collection } from '@tanstack/db'

export function liveRows<T extends { id: string }>(c: Collection<T>) {
  const [rows, set] = createStore<Record<string, T>>({})
  const unsub = c.subscribeChanges(changes => {
    set(d => {
      for (const ch of changes) {
        if (ch.type === 'delete') delete d[ch.key]
        else d[ch.key] = ch.value
      }
    })
  }, { includeInitialState: true })
  onCleanup(unsub)
  return rows
}
```

- One store write per IVM batch; per-row and per-field tracking for free; no array diff. Cost is O(changes), not O(rows).
- Solid 2 batches by microtask, so a batch of 500 changes is one render pass.
- The subscription callback runs outside any reactive scope, so the store setter is allowed there (writes inside a reactive scope throw in dev).
- Ordering is the live query's job (`orderBy`); the table reads `Object.values(rows)` through a `createMemo` keyed on the query's sort, or the query result collection carries order and the adapter keeps an index array. Measured in the first increment; the choice is recorded there.

### `liveList`: projection (small sets, pickers, record pages)

```ts
import { createProjection, from } from 'solid-js'

const subscribe = <T,>(c: Collection<T>) =>
  (next: (rows: T[]) => void, error: (e: unknown) => void) =>
    c.subscribeChanges(() => next(c.toArray), { includeInitialState: true })   // returns unsubscribe

export const liveList = <T extends { id: string }>(c: Collection<T>) =>
  createProjection<T[]>(() => from(subscribe(c)), undefined, { key: 'id' })
```

- `from(source)` turns a callback subscription into an async iterable for the computation, keeps only the latest value, and closes the source when the computation re-runs or its owner is disposed. No hand-written iterator, no `onCleanup`.
- Seedless derived form: the derive answers a complete snapshot per yield; `createProjection` reconciles each one by `id`, so surviving rows keep identity and only added or removed rows are created or disposed. Swapped or reordered rows no longer alias slots.
- `<Loading>` covers the first value; `isPending` reports an in-flight change.
- O(rows) reconcile per change, so it is for sets under a few hundred rows.

### Readiness and status

```ts
export function syncStatus(c: Collection<unknown>) {
  return createMemo(() => c.status)        // 'loading' | 'ready' | 'error' ; c.status is a signal the sync source sets
}
```

Every screen shows one line from `syncStatus` and the outbox count (see Writes). The feed's last cursor time is shown as "current as of".

### Writes: `action`, optimistic store, `until`

Solid owns optimistic state; `@tanstack/db`'s own optimistic layer is not used (two overlays on one row would flicker). The write shape:

```ts
import { action, createOptimisticStore, until } from 'solid-js'

export function recordWrite<T extends { id: string }>(c: Collection<T>, outbox: Outbox) {
  const [rows, setRows] = createOptimisticStore<Record<string, T>>(() => liveRowsPlain(c), {})
  const write = action(function* (op: OperationCall, expected: Partial<T> & { id: string }) {
    setRows(d => { d[expected.id] = { ...d[expected.id], ...expected } })   // tentative
    const entry = yield outbox.enqueue(op)                                  // durable before this resolves
    yield until(() => confirmed(c, expected.id, entry))                     // the feed brings the row back
  })
  return { rows, write }
}
```

- `until` holds the tentative row until the authoritative collection shows the written row (`row_version` at or past the one the operation reply names). The action's own optimistic write can never satisfy it.
- Offline, `until` waits as long as the outbox entry is pending. The UI shows the row with a "recorded, not uploaded" mark driven by the entry's state, not by `isPending`.
- A refusal on replay rejects the `until` predicate's source: the action throws at the `yield`, the optimistic row reverts, and the refusal code is shown on the record.
- `recordWrite` is generated per model beside the bindings; a screen imports it, it does not write it.

### Pinned versions

The repo pins Solid 2 in `web/package.json` at `2.0.0-rc.18` or later; the adapter targets the signatures above as of that pin. Three behaviors the adapter depends on landed in rc.16 through rc.18 and are kept as regression tests against the pin:

- `until` over a value derived from an unconfirmed optimistic guess waits for the guess to be confirmed or corrected (the feed echo), and rejects when the landing is an error (the refusal).
- a keyed optimistic store keeps its guess whole when the source answers differently while the action is open; two overlapping actions on one list do not tear it, and a confirmed row does not remount (an open editor keeps its draft).
- reconcile by key with rows swapped, reordered or shifted keeps one row per slot.

The tests: delta apply (`liveRows`), projection reconcile by id under a sort change (`liveList`), `until` resolving on feed echo, `until` rejecting on refusal, two overlapping writes on one list.

## Writes: outbox, replay, conflicts

Every write is an operation call recorded in a local outbox before it is sent. Online, the entry is sent at once and leaves the outbox on reply. Offline, it waits. The host sees ordinary operations either way.

### Entry

```ts
type OutboxEntry = {
  seq: number                    // local, monotonic; replay order
  operation: string              // package-qualified operation name
  request: unknown               // the request JSON, exactly as the binding builds it
  request_hash: string           // the hash the write log dedupes on; computed once, here
  recorded_at: string            // ISO, browser clock; a display value only
  state: 'pending' | 'sending' | 'refused'
  refusal?: { code: string; text: string }
  expect: { model: string; id: string }   // what `until` watches
}
```

- `request_hash` is computed from the canonical request JSON at enqueue time and carried unchanged. The write log already dedupes on it; a resend after a lost reply is harmless.
- The entry carries no session token. Replay uses the token current at send time.
- `seq` is a per-database counter in IndexedDB; two tabs share the outbox through a `BroadcastChannel` lock so only one replays at a time.

### Enqueue

```ts
async enqueue(op: OperationCall): Promise<OutboxEntry> {
  const entry = build(op)
  await db.put('outbox', entry)        // durable before the UI reports recorded
  this.kick()                           // try to send now; no-op when offline
  return entry
}
```

The UI reports "recorded" when `enqueue` resolves, not when the host replies.

### Replay

```ts
async replay() {
  for (const e of await db.getAll('outbox', 'pending', byScope(seq))) {
    e.state = 'sending'; await db.put('outbox', e)
    const reply = await send(e)                       // POST through the generated binding
    if (reply.ok)              await db.delete('outbox', e.seq)
    else if (reply.retriable)  { e.state = 'pending'; await db.put('outbox', e); break }   // network, 5xx: stop, wait
    else                       { e.state = 'refused'; e.refusal = reply.refusal; await db.put('outbox', e) }  // keep going
  }
}
```

- Order is `seq` ascending within one target record; entries for different records may send concurrently (bounded to 4). Within a record, a refused entry does not block later entries for that record only when the operation is append-only (declared on the operation, see Generator); otherwise later entries for that record wait as `pending` with the refusal shown on the first.
- Replay triggers: network returns (health probe succeeds), app open, a new enqueue, and a 30 s timer while anything is pending.
- `send` passes the entry's `request_hash` as the request's idempotency value `; the host validates the request as any request` (see Release compatibility).

### Network detection

`navigator.onLine` is advisory only. Online means a `HEAD /api/health` (no-store) returned 2xx within 3 s. The probe runs on `online` events, on replay triggers, and every 15 s while offline.

### Conflicts

The platform's conflict rule is refusal, not merge. A write carries the `row_version` it was made against (already the revision rule for commands that guard a reference). On replay, a stale `row_version` is refused with the existing code; the entry becomes `refused`, the optimistic row reverts, and the record shows the refusal with the current row beside the recorded request. The user resubmits or discards. Nothing is merged automatically.

Append-only operations (a new record against a static parent) carry the parent's id, not its `row_version`, and cannot conflict on the parent. Their own id follows the key rules below.

### Ids and numbers

- UUID record ids are client-generated UUIDv7 values, made by the binding at enqueue. The host accepts the supplied id. Plain integer keys also require a client id. The database generates identity and DEFAULT integer keys, and create returns the id. A numbered field is never the record id: it is a business key, unique and indexed, beside the id.
- Fields declared `number = "<sequence>"` are host-assigned at commit, as the generator already enforces; the client never supplies them. Numbers are assigned in upload order, not record order; `recorded_at` carries record order.
- Until the feed echo brings the number, the field renders as a placeholder that cannot be read as a number: `pending·<last 4 hex of the id>`. Any operation that consumes the number (print, export, a typed reference) waits on it: `yield until(() => rows[id]?.number !== undefined)`.
- For client-supplied keys, references between two offline entries (a line to its header) use the header's id. Both ids exist at enqueue, so nothing is ever remapped.

### Status surface

Every screen's shell shows: pending count, refused count, last successful send time. A refused entry is a row in a "not uploaded" panel with its operation, record, refusal text, resubmit and discard. Discard is the only way an entry leaves the outbox without a 2xx.

## Shell, storage, devices

Supported: Chrome on PC and tablet. Everything below is the Chrome behavior; other browsers get online behavior and one notice.

### Service worker

One service worker per application, generated into the Vite build (`vite-plugin-pwa` or a 60-line hand-written worker; the first increment picks after measuring the plugin's output size).

- Precaches the built shell: `index.html`, the JS and CSS chunks, fonts. Nothing else. API responses are never cached by the worker; the browser HTTP cache and the collections hold data.
- Strategy: cache-first for precached assets; network-only for `/api`. The worker never answers an `/api` request.
- A new build registers a new worker; the page shows "update available, reload" and does not reload on its own while the outbox is non-empty.
- The web manifest exists (name, icon, `display: standalone`). Install is optional and never required for any behavior in this document.

### Storage

- IndexedDB, one database per (application, environment, user): object stores `collections` (key: collection key; value: rows, cursor, saved\_at), `outbox` (key: seq), `meta` (release digest, schema version).
- `navigator.storage.persist()` is called at startup; the result is logged and shown in the status surface as "storage protected" or not. A denial changes nothing else.
- Quota: `navigator.storage.estimate()` at startup and before `take offline`; refuse `take offline` when the estimate cannot hold the chosen set with 20 % headroom.
- No Background Sync dependency. Replay runs while the app is open. Chrome's Background Sync may be registered as an enhancement, never relied on.

### Take offline

An explicit action on a screen whose definition declares `offline: true` (see Generator).

- The user selects records (orders) by the screen's own filters. The client fills the collections the screen and its child tables need for those ids, then every reference collection those rows name, one level deep.
- Completion is reported with counts and the cursor time. The set stays current through the feed while online.
- A screen declared `offline: true` opens from the local copy with no network; a screen without it shows "needs a connection" when the collection is not local.

### Degradation

| Missing | Behavior |
| --- | --- |
| service worker | app works while the tab lives; "offline shell unavailable" shown once |
| `persist()` denied | notice on `take offline`; still allowed |
| IndexedDB unavailable | collections are memory-only; outbox is memory-only; a warning that writes are lost on reload |
| not Chrome | all of the above may apply; nothing is disabled by browser name |

## Session and authorization

The session token is short-lived (15 minutes, renewable). Offline work outlasts it. The outbox and the feed are designed so that the token's lifetime never decides whether work is kept.

- Outbox entries carry no token. Replay attaches the current token; if renewal fails, replay waits and the status surface shows "sign in to upload". A sign-in page that works from the shell cache is part of the shell.
- The host checks the user's active role at replay time, not at record time. A role change between recording and upload produces an ordinary 403 refusal on the entry.
- `recorded_at` is stored by the operation as a field the application declares (`recorded_at: timestamptz` on the record), supplied by the client from its clock. The host's own write time stays the authoritative time. Nothing in the platform reads `recorded_at`.
- Local data is readable without a token: the collection is on the device already. A locked screen (no valid session, offline) shows a read-only view of local data; writes still enqueue, since the token is not needed to record.
- The IndexedDB database is keyed by user. Signing out clears nothing by default; "sign out and clear local data" is a separate action, refused while the outbox is non-empty unless the user discards.

## Release compatibility

A cached shell and a saved outbox can outlive the release that produced them. The host does not special-case this: every request is validated as any request is, and an old entry that no longer decodes or validates is refused with the ordinary code. No release check, no compatibility declaration, no second hash.

- The shell carries the release digest it was built from, in `meta`, for one purpose: when the host's current release differs (read from the health probe), the client shows "update available, reload" and reloads once the outbox is empty. The host never reads it.
- An outbox entry refused for a request-shape reason after a release is an ordinary `refused` entry: the record page shows the stored arguments through the new binding's request builder; fields the new shape needs are highlighted; the user resubmits or discards.
- Collections are versioned by the generated schema version in `meta`. A schema change drops the local collections on first open under the new release and runs `fill`. The outbox is never dropped by a schema change.
- Backup, only if validation-only proves insufficient (a shape that decodes under the new release but means something else): a request-shape hash per operation, emitted by the generator into the bindings, carried on the entry, compared by the host. Not built until a case demands it.

## Generator and bindings

Nothing in this document is hand-written per screen. The generator emits the host read, the TypeScript binding, the collection, the adapter calls and the outbox wiring from the model and screen definitions.

### Manifest (`wamn.k`)

```
model receipt {
  ...
  feed = true                       // emit `changes`; default true for every model with a list read
}

operation receipt.record_line {
  append_only = true                // no row_version guard; later entries for the same record never wait on a refused one
}

screen receiving.lines {
  offline = true                    // take-offline action; opens from local copy with no network
  read = "liveRows"                 // or "liveList"; default by row count: liveRows for table screens
}
```

### Emitted per model

| Artifact | Where | Content |
| --- | --- | --- |
| `changes` statement | generated SQL beside `list` | history-table query, collapsed to latest per id, with the live-row join |
| `changes` route | route emitter | GET, same auth and cache policy as `list`, weak ETag on `next_cursor` |
| `list` outcome cursor | statement + route emitter | position taken in the same read transaction |
| TS binding | `web/bindings` | `changes(cursor, limit)` and `list` with `cursor` in the outcome |
| collection factory | generated components package | `receiptCollection(scope)` calling `modelCollection` with the model's bindings |
| write helper | generated components package | `recordReceipt` calling `recordWrite` with the model's collection and the outbox |

### Emitted per screen

- The screen's query over its collections (the table definition already names fields, sort, filters; it gains the query builder form).
- `offline: true` adds the take-offline action to the screen's toolbar and lists the collections it fills (the screen's own, its child tables', and one level of references).

### Build checks

- A model with `feed = true` and no history table is refused at build (history tables are generated per relation; this is a consistency check, not a performance one).
- An operation with `append_only = true` that declares a `revision` field is refused.

## Invariants and tests

Each rule names what a test measures. A rule without a measurable invariant is not in this document.

| Rule | Invariant | Test |
| --- | --- | --- |
| Feed after snapshot sees every later change and none earlier | For any write W committed after `list`'s snapshot, `changes(list.cursor)` returns W's row; for any W before, it does not | host test: list, write, write, changes; assert ids and versions |
| Feed collapses | A row changed n times since the cursor appears once with its latest `row_version` | host test over the history table |
| Cursor is opaque | No contract field is named `position` or `transaction_id`; the cursor round-trips unchanged | contract test on the generated bindings |
| Delete reaches the client | A deleted row leaves the collection within one feed call | runtime test with a fake feed |
| Stale change ignored | A change with lower `row_version` than stored does not overwrite | runtime unit test |
| First paint from local | A screen with a saved collection renders rows before any fetch resolves | runtime test with fetch blocked |
| Recorded means durable | `enqueue` resolves only after the IndexedDB put commits; a reload after resolve finds the entry | runtime test with reload |
| Replay order | Entries for one record send in `seq` order; no entry sends twice unless its reply was lost | runtime test with a fake host counting requests by hash |
| Refusal isolates | A refused entry leaves the queue state of other records unchanged | runtime test |
| Old entry is ordinary | An entry recorded under a prior release is validated like any request; a shape it no longer accepts is an ordinary refusal, never applied | host test: enqueue under R1 bindings, send to R2 |
| Optimistic row reverts | A refusal makes the optimistic row disappear and the refusal appear on the record | adapter test |
| `until` resolves on echo | After the feed delivers the written row, the action completes and the row is no longer tentative | adapter test |
| Shell opens offline | With the network blocked and a registered worker, the app renders its last screen from local data | browser test (Chrome, headless) |
| Worker never serves `/api` | Any `/api` request with the network blocked fails; none is answered from cache | browser test |
| Memory bound holds | 100,000 rows in one collection with one live query stay under the measured heap ceiling | measured in the first increment, value recorded in `web/runtime` tests |

Testing budget follows the existing rule: build, clippy, unit per issue; live targets touching merged files per merge; one cluster stage only when a file under that stage changed.

## Assumptions

| Assumption | How to verify | Invalidated if false |
| --- | --- | --- |
| False (Epic 2b): the history `position` orders commits. It is an identity value that a write takes at insert time ([`record-history.sql`](../../deploy/sql/record-history.sql)), so a writer that takes a lower position and commits later is missed by a reader that passed it | Read the history table definition | Replaced: the cursor carries the snapshot xmin. `changes` reads entries from the cursor transaction, including visible commits at or above its snapshot xmin. The cursor never advances past those commits. A change can arrive twice, and the client ignores an equal `row_version`. The history table has an index on `transaction_id` |
| Feed latency under a long-open transaction is one page of commits. Beyond that it waits for xmin | Hold one transaction open and commit 200 others. Read with limit 100, then commit the held transaction and drain the feed | Visible rows beyond one page wait until xmin advances |
| History retention covers the longest offline period plus one day | Retention exists: `audit_log.retention` (`none`, `unlimited` or `P<n>D`), pruned daily by the retention task ([data access](../architecture/data-access.md)). WMS packaging declares `unlimited`, so its history is unbounded today | `cursor_expired` becomes common; `fill` runs on every reopen. `cursor_expired` answers a cursor older than the oldest entry the retention keeps, only for a `P<n>D` retention |
| Answered (Epic 2b): `list` and the cursor read share one snapshot | The cursor is a column of the list statement itself, `pg_snapshot_xmin(pg_current_snapshot())`, so one statement reads the rows and the cursor at one snapshot, with no shared transaction. An outer row carries the cursor when the page has no rows | — |
| A model with no `row_version` cannot be synced | A model with a list read gets a feed only when it also has a history table and a revision field on that read | Epic 2b skips such models, and Epic 5 adds the build refusal and `feed = false` |
| `@tanstack/db` collections and `db-ivm` live queries handle 100,000 rows in one collection within the measured heap ceiling | The first increment's measurement at 10k and 100k | The per-collection cap drops below the table cap; large screens stay on the page path |
| `@tanstack/db`'s custom sync source and `subscribeChanges` deliver change batches with `insert`/`update`/`delete` and the row value, as used in the adapter | Read the vendored version's types before the adapter issue | The adapter shape changes; the contract and the outbox do not |
| Solid 2's `createProjection` accepts a from() subscription as a seedless derive and reconciles by key; `until` inside `action` resolves on authoritative data and never on the action's own optimistic writes | Adapter tests on the pinned release | `liveList` becomes a `createStore` with manual reconcile; `until` is replaced by a resolver keyed on `row_version` |
| The write log dedupes on the request hash the client computes, so a resend after a lost reply is harmless | Host test: send the same entry twice, assert one effect | The outbox must wait for a reply before any retry; replay slows but stays correct |
| Chrome on the supported devices grants IndexedDB and does not evict it under normal use | Run the browser tests on the target devices | The take-offline flow must warn on every open; no design change |
| Answered (Epic 2b): `changes` has the authorization of `list` | `changes` is the list read with its feed part, one operation and one permission row, so the check is the same by construction | — |

## Deferred

Each item is deferred until a consumer exists; none is rejected.

- A platform event channel to the browser (SSE or WebSocket) carrying "model moved" and other platform events; the feed registers it as a second trigger, the contract does not change. Its design belongs with its other consumers.
- Automatic merge for concurrent edits to one record; refusal-and-resolve stays the rule.
- Server-side incremental views for reporting screens; aggregates there stay authored operations.
- A native app; the browser shell is the focus.
- The local (SQLite) target consuming the same feed contract for its projections.
- Background Sync registration in Chrome as an enhancement to replay.
- Partial or query-driven sync beyond the scope model (the client asks for a query, the host keeps only those rows flowing).
- Shared, cross-user optimistic state (presence, live cursors).

## Epics

One epic at a time per repo; a wamn epic starts only when its wfe counterpart is on wfe main. Epic 1b is the measurement that decides the adapter's read shape and whether the vendored store stays.

| Epic | Repo | Scope | Done when |
| --- | --- | --- | --- |
| 1a. `@wfe/sync` store and adapters | wfe | New package: `collection` with `fill` only (no `follow`), `liveRows`, `liveList`, `syncStatus`; tests for delta apply, projection reconcile by id under a sort change; a gallery page at 10k and 100k rows with a fake `Feed` | Package published on main with the tests green; the gallery renders both read shapes |
| 1b. First screen and measurement | wamn | Pin `@wfe/sync`; `Feed` from the WMS bindings; one WMS table screen on `liveRows`; the same screen on a Solid-only store map with memo-derived views; measurements at 10k and 100k rows, grouped with totals: time to interactive on reopen, update latency per change, heap | The read shape per screen type and whether `@tanstack/db` stays are ruled from the numbers |
| 2a. Feed follow | wfe | `follow`, `Trigger`, `Persistence` use, `cursor_expired` handling, `until`-on-echo tests | Tests green against a fake `Feed` that replays a change log |
| 2b. Change feed | wamn | `changes` statement, route, binding; `list` outcome cursor; `moved` from the per-model version; the WMS screen updates without a re-read after a write | The invariants "feed after snapshot" and "feed collapses" pass on the host |
| 3a. Outbox and writes | wfe | `outbox`, `recordWrite` with `action` and `until`, refusal states, probe-driven replay; tests: durable before resolve, replay order, refusal isolates, two overlapping writes on one list | Tests green against a fake `Send` |
| 3b. Writes on the screen | wamn | `Send`, `probe`, ids and numbers (`pending·xxxx`), refusal panel, status surface; case 1 on the WMS screen with the network cut mid-session | Outbox invariants pass end to end |
| 4a. Shell | wfe | `shell()`: worker registration, `persist()`, quota; the worker that precaches the shell and never serves `/api` | Browser tests: opens offline, worker never serves `/api` |
| 4b. Take-offline | wamn | Manifest, database names, release digest and reload rule, `offline: true` screens, take-offline action; case 2 end to end | Close the tab, open with no network, record, reconnect, upload; the old-entry invariant passes |
| 5. Generator | wamn | Manifest fields, per-model and per-screen emission, build checks; Receiving and WMS regenerated with no hand edits | Regenerate and diff shows only generated files; build checks fire on the refusal cases |

## Epic 1b measurements

Owner ruling: `@wfe/sync` stays, `liveRows` is the read shape for table screens, and `liveList` is the read shape for small sets.

Measured on 2026-10-24 with [`web/sync-bench`](../../web/sync-bench/README.md) (bead `wamn-hhd77`).
The ruling on the read shape per screen type, and on whether `@tanstack/db` stays, belongs to the owner.

| Input | Version |
| --- | --- |
| wfe pin | `fdeaacc` (wfe main) |
| `@tanstack/db` | 0.15.2 |
| `@tanstack/db-ivm` | 0.2.0 |
| `solid-js`, `@solidjs/web` | 2.0.0-rc.18 |
| Browser | Google Chrome 156.0.8078.47, headless, through Playwright 1.63.0 |
| Machine | 8 cores, shared with other agents |

The screen is the WMS `packaging` table: the generated `PackagingQueryTable` columns over virtual rows, then groups by status and type with count and the sum of `row_version`, and a totals row.
Shape A reads a `@wfe/sync` collection through `liveRows`, and its groups and totals come from `@tanstack/db` live queries.
Shape B keeps a Solid store keyed by id, and its sort, filter, groups and totals are memos.
In both shapes the row order is a memo that sorts by packaging code. The status filter was "all" for every run.

Each cell is the median of 5 runs. Each run mounted one shape on a new page of the production build, filled from a fake feed of seeded synthetic rows.
The shapes alternated their order in each repeat. The other agents held new builds and test runs during the run, from 15:03:20 to 15:05:21 UTC.

- Fill is the time from `list` resolving to the first painted row (the frame after the first row is in the DOM).
- Update is the time from applying one batch of 100 changes (70 updates, 20 inserts, 10 deletes) to the next frame. Each run applied 10 batches, so each median covers 50 batches.
- Heap is `performance.memory.usedJSHeapSize` after a forced garbage collection.

| Shape | Rows | Fill to first paint (ms) | Update per 100 changes (ms) | Heap after fill (MB) | Heap after 10 batches (MB) | Runs finished | Load average |
| --- | --- | --- | --- | --- | --- | --- | --- |
| A | 10,000 | 453.2 | 46.8 | 15.5 | 16.4 | 5/5 | 5.5–5.8 |
| B | 10,000 | 223.4 | 68.6 | 17.2 | 17.9 | 5/5 | 5.5–5.8 |
| A | 100,000 | 3,403.1 | 308.5 | 111.8 | 112.6 | 5/5 | 5.7–7.8 |
| B | 100,000 | 1,634.6 | 933.0 | 141.9 | 142.6 | 5/5 | 5.7–7.8 |

Single updates ranged from 27.5 to 69.6 ms (A) and 58.4 to 137.8 ms (B) at 10,000 rows, and from 224.8 to 583.4 ms (A) and 668.4 to 1,958.8 ms (B) at 100,000 rows.
The fake feed returns rows that already carry `row_version`. The real feed copies every row to add `row_version` beside the generated `rowVersion`.
That copy took a median of 4.9 ms at 10,000 rows and 164.1 ms at 100,000 rows, measured separately at 15:06 UTC with load average 18.6 to 19.1, so it is not part of the fill column.

Conditions that limit these numbers:

- The summed column is `row_version`, a stand-in, because packaging has no other numeric column.
- Shape A sums with the float `sum` of `@tanstack/db`, which has no custom aggregates. The sum is exact here only because the column is int32 and the total stays below 2^53. Shape B sums with an exact BigInt sum inside its memo.
- `@wfe/sync` 1a fills only. The feed has no `follow`, so updates come from `applyChanges`, not from a host feed.
- Shape A reads the live query results through `liveRows` with a cast, and the view uses `Table`, because `ReadOnlyTable` has no body slot for virtual rows.

The wfe findings from this epic are beads `wamn-7e86p` (row version name), `wamn-b4icu` (`liveRows` on a live query result), `wamn-dhggm` (no custom aggregates) and `wamn-5dcm4` (`ReadOnlyTable` body slot).
