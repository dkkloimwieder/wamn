# wamn offline and sync

## Purpose and scope

The platform handles intermittent network failure lasting seconds to minutes, with the tab open or reopened during the outage.
A browser store keeps saved rows, follows changes, and records writes locally for replay through ordinary operations.
Full offline use of an application, or part of one, requires an application design.
The platform supplies the mechanism, and sync-bench tests it through take-offline.

The platform scope includes:

- A change feed on each list read that a sync screen uses, generated or authored.
- Browser collections and live queries, with a Solid 2 adapter in `@wfe/sync`.
- A durable outbox, ordered replay, and refusal handling.
- A cached shell and storage support for Chrome on PCs and tablets.
- Generator emission for application bindings and screen wiring, as specified under Epic 5.

Native applications, automatic conflict merging, server-side incremental views, shared optimistic state, and the local target remain deferred.

## Terms

| Term | Meaning |
| --- | --- |
| change feed | The list read's mode that returns changed result rows and a continuation cursor. |
| root | The model whose id keys each result row of a feeding query. |
| cursor | An opaque string that the client returns unchanged. |
| snapshot | Rows and a feed cursor from a list statement. |
| collection | Browser result rows of one query and argument set, keyed by root key in `@tanstack/db`. |
| live query | A query whose result updates when its input collections change. |
| adapter | The `@wfe/sync` code that exposes collections as Solid stores. |
| outbox | The durable local queue of operation calls awaiting upload or resolution. |
| replay | Sending pending entries in order within each record. |
| refusal | A final rejection that keeps the entry for correction or discard. |
| shell | Built application assets that a service worker can serve without a network. |
| take offline | An application action that saves a chosen data set before an outage. |
| shape digest | A hash of generated row fields, types, and nullability. |
| xmin | The snapshot's lower bound for transactions that remain open. |

## Requirements: the two cases

Both cases use collections, saved rows, an outbox, and a cached shell.
Only case 1 defines the platform's availability target.

| | Case 1: intermittent failure | Case 2: application-defined offline use |
| --- | --- | --- |
| Outage length | Seconds to minutes | The application defines its duration |
| Tab | Open or reopened during the outage | The application defines reopening requirements |
| Reads | Saved rows first, corrected when the network returns | A chosen local set with application-defined completeness |
| Data shape | Operational screens with collections | The application selects records and references |
| Writes | Locally recorded calls, then ordinary replay | The application selects operations that work against its local set |
| Conflicts | Ordinary revision refusals | The application defines its conflict policy within ordinary host rules |
| Scope | Platform behavior | Per-application design, tested through take-offline in sync-bench |

A screen with saved rows renders them before the network read resolves.
The outbox reports a write recorded only after its IndexedDB transaction commits.
The host deduplicates retries through the request identity.
Refusals remain visible and do not block other records.
Within a record, a refusal holds later entries except those marked `append_only`.

Chrome on PCs and tablets supplies the supported browser environment.
Missing browser capabilities follow the degradation rules below.
The frontend sends operations and reads rows without learning backend execution behavior.

## Architecture

The host emits list pages and changes through generated routes.
The browser runs collections, live queries, and the outbox.
Screens that use sync read collections and record writes through the outbox.

`@tanstack/db` stays as the collection and live-query engine.
`liveRows` serves table screens, and `liveList` serves small sets.
The read-shape rule below governs the adapter.

## Ownership: `@wfe/sync` and wamn

`@wfe/sync` owns the generic collection, Solid adapter, outbox, shell registration, and worker cache behavior.
wamn owns model names, routes, operations, headers, database names, session handling, and generated bindings.
The package receives `Feed`, `Send`, `Persistence`, and `probe` implementations without constructing URLs or reading credentials.

### What `@wfe/sync` is told about its consumer

wamn supplies generated reads over GET and writes over POST.
Reads use ETags and 304 responses.
Writes carry a request hash for deduplication.
Rows have ids and integer versions, read through the collection's version accessor.
The browser adapter targets Solid 2.

### `@wfe/sync` public surface

The surface below comes from wfe commit `248556349cb29a1483b35d8022734c317b67368b`, pinned by the `web/wfe` entry.
[`index.ts`](../../web/wfe/packages/sync/src/index.ts) exports the main entry.
[`internal.ts`](../../web/wfe/packages/sync/src/internal.ts) exports `applyChanges`.
[`worker.ts`](../../web/wfe/packages/sync/src/worker.ts) exports `precache` and `skipWaitingMessage`.

[`package.json`](../../web/wfe/packages/sync/package.json), `exports`, declares these entry conditions:

| Entry | `types` | `wfe-source` | `default` |
| --- | --- | --- | --- |
| `.` | `./dist/index.d.ts` | `./src/index.ts` | `./dist/index.js` |
| `./internal` | `./dist/internal.d.ts` | `./src/internal.ts` | `./dist/internal.js` |
| `./worker` | `./dist/worker.d.ts` | `./src/worker.ts` | `./dist/worker.js` |

These declarations preserve exported names, fields, optional members, and function signatures.
Supporting aliases appear separately because the main entry does not re-export them.

```ts
// @wfe/sync: types.ts, re-exported by index.ts
export interface Row {
  id: string
  row_version?: number
}

export type Change<T extends Row> =
  | { id: string; row_version: number; deleted: true }
  | { id: string; row_version: number; deleted: false; data: T }

export interface Feed<T extends Row, S> {
  list(
    scope: S,
    signal?: AbortSignal,
  ): Promise<{ rows: T[]; cursor: string; complete?: boolean }>
  changes(
    scope: S,
    cursor: string,
    limit?: number,
    signal?: AbortSignal,
  ): Promise<{ rows: Change<T>[]; next_cursor: string; complete: boolean }>
  moved?(scope: S, since: string, signal?: AbortSignal): Promise<boolean>
}

export interface Persistence {
  load(
    key: string,
  ): Promise<{ rows: unknown[]; cursor: string; saved_at: string } | undefined>
  save(key: string, rows: unknown[], cursor: string): Promise<void>
  remove(key: string): Promise<void>
}

export interface Trigger {
  subscribe(scopeKey: string, fire: () => void): () => void
}

// Supporting declarations, not re-exported by index.ts.
// types.ts
export type Version<T extends Row> = (row: T) => number
export const rowVersion = (row: Row): number => row.row_version as number

// collection.ts
type Utils = {
  refill: () => Promise<void>
  pull: () => Promise<void>
  error: () => unknown
  currentAt: () => string | undefined
  complete: () => boolean
}
export type SyncCollection<T extends Row> = Collection<T, string, Utils, never>

// writer.ts
export type AnyCollection<T extends Row> = Collection<
  T,
  string,
  UtilsRecord,
  never
>

// @wfe/sync: collection.ts, re-exported by index.ts
export function collection<T extends Row, S>(
  feed: Feed<T, S>,
  scope: S,
  options: {
    key: string
    persistence?: Persistence
    trigger?: Trigger
    pollMs?: number
    pageSize?: number
    version?: Version<T>
  },
): SyncCollection<T>

// @wfe/sync: solid.ts, re-exported by index.ts
export type KeyedCollection<T extends object> = Pick<
  Collection<T, string | number>,
  'entries' | 'subscribeChanges' | 'status' | 'on' | 'onFirstReady' | 'toArray'
>
export function liveRows<T extends object>(
  c: KeyedCollection<T>,
): Store<Record<string, T>>
export function liveList<T extends object>(c: KeyedCollection<T>): Store<T[]>
export function syncStatus<T extends object>(
  c: KeyedCollection<T>,
): Accessor<'loading' | 'ready' | 'error'>
export function syncComplete<T extends object>(
  c: KeyedCollection<T>,
): Accessor<boolean>
export function syncCurrentAt<T extends object>(
  c: KeyedCollection<T>,
): Accessor<string | undefined>
export function outboxStatus(o: Outbox): Accessor<OutboxCounts>
export function recordWrite<T extends Row>(
  c: SyncCollection<T>,
  o: Outbox,
  options?: { model?: string },
): {
  rows: Store<Record<string, T>>
  write: (
    op: OperationCall,
    expected: Partial<T> & { id: string },
  ) => Promise<void>
}

// @wfe/sync: outbox.ts, re-exported by index.ts
export type OperationCall = {
  operation: string
  request: unknown
  request_hash: string
  expect: { model: string; id: string }
  append_only: boolean
}
export type Reply =
  | { ok: true }
  | { ok: false; retriable: boolean; refusal?: { code: string; text: string } }
export interface Send {
  (entry: OutboxEntry, signal: AbortSignal): Promise<Reply>
}
export type OutboxEntry = OperationCall & {
  expected: unknown
  seq: number
  recorded_at: string
  state: 'pending' | 'sending' | 'refused'
  refusal?: { code: string; text: string }
}
export type OutboxCounts = {
  pending: number
  sending: number
  refused: number
  lastSentAt: string | undefined
}
export interface Outbox {
  enqueue(op: OperationCall, expected?: unknown): Promise<OutboxEntry>
  replay(): Promise<void>
  resubmit(seq: number): Promise<void>
  discard(seq: number): Promise<void>
  entries(): OutboxEntry[]
  watch(fn: (counts: OutboxCounts) => void): () => void
  close(): void
}
export function outbox(options: {
  database: string
  send: Send
  probe: () => Promise<boolean>
  concurrency?: number
}): Outbox

// @wfe/sync: errors.ts, re-exported by index.ts
export class CursorExpired extends Error {
  constructor(message?: string, options?: ErrorOptions)
}
export class OutboxRefused extends Error {
  readonly code: string
  readonly text: string
  readonly seq: number
  constructor(
    seq: number,
    refusal?: { code: string; text: string },
    options?: ErrorOptions,
  )
}
export class OutboxDiscarded extends Error {
  readonly seq: number
  constructor(seq: number, options?: ErrorOptions)
}

// @wfe/sync: shell.ts, re-exported by index.ts
export interface ShellResult {
  worker: boolean
  persisted: boolean
  quota: { usage: number; quota: number }
}
export async function shell(
  options?: {
    persist?: boolean
    onUpdate?: (reload: () => void) => void
    workerUrl?: string
  },
): Promise<ShellResult>

// @wfe/sync/internal: applyChanges.ts
export function applyChanges<T extends Row>(
  c: AnyCollection<T>,
  changes: Change<T>[],
): void

// @wfe/sync/worker: worker.ts
export const skipWaitingMessage = 'wfe-sync:skip-waiting'
export function precache(
  manifest: string[],
  options?: {
    buildId?: string
    bypassPrefixes?: string[]
    apiPrefix?: string
    navigationFallback?: string
  },
): void
```

`Collection` and `UtilsRecord` come from `@tanstack/db`.
`Accessor` and `Store` come from Solid.
`pollMs` defaults to 10000, persistence saves wait 250 ms, and outbox concurrency defaults to four records.
`version` defaults to `rowVersion`, so generated camel-case rows pass `version: row => row.rowVersion` without copying.

`refill` lists again, and `pull` runs a follow pass.
`error` exposes the last read failure.
`currentAt` reports the last successful list or completed changes pass, not the opaque cursor's time.
`complete` reports the last successful list's completeness and defaults to true.
`syncComplete` exposes that value reactively.

### Package consumption

wamn installs the built wfe packages with NodeNext resolution, without `paths` for wfe packages or a source condition.
[`pnpm-workspace.yaml`](../../pnpm-workspace.yaml), `packages`, includes the submodule packages.
[`web/runtime/package.json`](../../web/runtime/package.json), `dependencies`, and [`web/sync-bench/package.json`](../../web/sync-bench/package.json), `dependencies`, use `workspace:*`.

The root [`package.json`](../../package.json) owns the web workspace.

### What wamn builds on it

wamn supplies:

- A `Feed` that reads all list pages up to the table cap and maps the generated changes result.
- `Send`, which calls generated bindings with the current session and maps host outcomes.
- `probe`, which calls `HEAD /api/health`.
- IndexedDB persistence and database names scoped by application, environment, and user.
- Shape keys, worker build assets, reload control, and the status surface.
- Generated collection and write helpers under Epic 5.

### Section ownership

| Section | Owner |
| --- | --- |
| Query feed (host) | wamn host and generator |
| Browser store and live queries | `@wfe/sync`, with wamn's `Feed`, version accessor, and scope key |
| Solid 2 adapter | `@wfe/sync` |
| Writes | `@wfe/sync` queue, with wamn's send mapping, ids, numbers, and refusal panel |
| Shell and storage | `@wfe/sync` worker mechanism, with wamn's assets, bypass paths, and database names |
| Session and authorization | wamn |
| Release compatibility | Ordinary host validation and wamn's shell reload rule |
| Generator and bindings | wamn |

## Query feed (host)

A feed belongs to a query, not to a model.
It returns the changed result rows of that query.
Joins stay in PostgreSQL, and the browser never joins tables.

### Unit and root

Every list read that a sync screen uses can carry a feed, whether the read is generated or authored.
The feed returns rows of that query's result.
The browser holds those result rows, and it never holds raw joined tables for a screen.

Each feeding query names its root model.
The root is the model whose id keys the result.
The query returns one result row per root key, and the key of a result row is the root's id.
A query that can return two rows for one root key, or a row without a root key, cannot carry a feed.

For an authored read, the root is the model of its `lists` declaration.
[`manifest.rs`](../../crates/schema/generator/src/manifest.rs), `ListDeclaration`, already names that model and the key field.
The key field must be the root's id, and a feed needs one key field.
An authored feeding read selects from one view, as [Read set and linking columns](#read-set-and-linking-columns) states.

| Rule | Invariant | Test |
| --- | --- | --- |
| One unit | A feed belongs to one query and returns that query's result rows | Generated contract test for a generated read and an authored read |
| One row per root | Each result row has a root key, and no two rows share one | Build refusal test for a query with a nullable or repeated key |

### Contract

[The change feed](../architecture/execution.md#change-feed) owns the list contract, the `changes` variant, and the client helper.
The query feed keeps that contract and changes three facts:

- An authored read gets the same `page` and `changes` variants as a generated read.
- `id` in a `changes` row is the root key of a result row.
- The list keeps one permission row for both variants, as it does today.

### changes(cursor)

One `changes` call runs one statement, so every step reads one snapshot.
The statement has three steps.

1. It finds the affected root keys. For every table that the query reads, it takes the history entries in the cursor window. The window, paging, and visibility rules are the rules of the current feed in [`sql.rs`](../../crates/schema/generator/src/sql.rs), `changes`.
2. It maps each entry to root keys. A root table entry maps through its `row_key`. A joined table entry maps through the view's linking column for that table. A deleted joined row, or an update that changes a foreign key, also maps through the foreign key in its history `before` image.
3. It runs the query's own statement again, restricted to `root.id = any($affected)`. A returned key is an upsert. An affected key that the statement does not return is a deletion.

A changed row of a joined table has its own history entry.
So each entry maps through the current rows on its path and its own `before` image.
A key that leaves the query's filters is not returned, so it is a deletion.

The cursor, `next_cursor`, `complete`, and 304 rules stay as they are.
Paging counts root keys, not history entries.
Root keys page by `(transaction, key)`, where the transaction is the greatest one among the key's entries in the window.
If the cursor transaction is at or below the pruning watermark of any history table that the query reads, `cursor_expired` applies.

| Rule | Invariant | Test |
| --- | --- | --- |
| One snapshot | Key mapping and the restricted run read one snapshot | Host test with a write that commits between two feed calls |
| Joined change | A change to any joined row returns its root key with the new result | Host test that updates, inserts, and deletes a joined row |
| Moved join | An update that moves a joined row returns the old and the new root key | Host test that changes a line's purchase order |
| Deletion | An affected key that the restricted run does not return is a deletion | Host test for a deleted root and a root that leaves the filters |
| Expiry | Pruning of any table that the query reads expires an older cursor | Retention test on a joined table |

### Cursor and snapshot handoff

[The change feed](../architecture/execution.md#change-feed) owns the opaque cursor and the snapshot handoff.
The query feed changes three facts:

- The cursor names the query instead of the model.
- A cut page adds its last row's root key.
- An authored read's page gets the same outer row with the snapshot xmin.

| Rule | Invariant | Test |
| --- | --- | --- |
| Opaque cursor | The cursor round-trips unchanged and refuses another query | `feed::encode` and `feed::decode` tests |

### Row version

The `row_version` of a result row is the snapshot xmin of the statement that read it.
A list page and a `changes` call both supply it, from the same `SNAPSHOT_XMIN` value that supplies the cursor.

A later statement sees every commit that an earlier statement saw.
The browser runs startup, pulls, and refills one at a time.
So a row from a later read is never older than a held row, even with two equal xmin values.
If the new version is equal or greater, the browser replaces the held row. It ignores a lower version.

The two example rules fail:

- The greatest transaction id among the participating rows does not grow with commit order. A transaction with a lower id can commit later, so a newer result can carry an equal or lower value.
- A counter per result needs a stored counter for each root key and a write to it on every joined change. That is a server-side view, and it puts every joined write behind one root row lock.

The outbox accepts a write as landed without a version comparison.
A joined write, such as a receipt line, has no root revision that the browser can expect.
A write lands after two events: its send succeeds, and a `changes` call that starts after the reply ends with `complete`.
That call runs after the commit, so it returns every result row that the write changed.
A revision column stays only as the write guard for operations that declare one.
Bead `wamn-3phv6` plans this rule in `@wfe/sync`, and wamn does not edit that package.

| Rule | Invariant | Test |
| --- | --- | --- |
| Version order | A row from a later read replaces a held row. A lower version never does | Collection batch test with equal and lower versions |
| Confirmation | A write lands only after a successful send and a complete `changes` call that starts after the reply | `recordWrite` adapter test with a write to a joined row |

### Read set and linking columns

The host needs, for each feeding query, the tables that it reads and how each table links to the root.

A generated list read reads one table, the model's table, with no join.
[`sql.rs`](../../crates/schema/generator/src/sql.rs), `query`, writes `FROM <table> AS model` and nothing else.
[`manifest.rs`](../../crates/schema/generator/src/manifest.rs), `ModelDeclaration::feed_revision`, has the model.
Its read set is the root table, and it has no links.

An authored feeding read selects from one authored view, with its own filters.
A query that joins puts its join in that view.
[Authored views](authored-views.md#feed-on-a-view) owns the view rules: the root, the linking columns, the shapes that can feed, and the build refusals.
The view's base tables, from `pg_depend`, are the read set.
Each linking column of the view links its table to the root.

The host does not parse an authored statement's joins.
An authored feeding read that names any relation other than one view refuses, naming the relation.

The restricted run wraps an authored statement as `SELECT * FROM (<statement>) AS result WHERE result.<key> = any($n)`.
The build refuses a feed on an authored statement that groups, uses window functions, or carries `LIMIT`, because the wrapper changes their result.
[`sql_lex.rs`](../../crates/schema/generator/src/sql_lex.rs), `groups_or_windows`, already detects the first two.

| Rule | Invariant | Test |
| --- | --- | --- |
| Read set | The feed reads history for exactly the base tables of the read's view, or the model's table | Generator test that compares the feed statement with the profile |
| One view | An authored feeding read selects from one view and nothing else | Generator refusal test for an authored feed that joins |

### Scoping

A query feed covers exactly the query's own filters and arguments.
The restricted run applies them, so a key outside them is a deletion.
No client-side scope rules remain.
A collection's scope is the query's argument set.

| Rule | Invariant | Test |
| --- | --- | --- |
| Scope | Feed rows match the query's filters, and a row that leaves them is a deletion | Host test that changes a filtered column |

### Prerequisites and build refusal

A feeding query needs:

- History on every table that it reads, with retention.
- A unique root id in each result row.
- A link to the root for every joined table, as [Authored views](authored-views.md#feed-on-a-view) states.

The build refuses a feed on a query that reads a table without history, and names that table.
A feed needs no revision column, because the row version is the statement's snapshot xmin.

| Rule | Invariant | Test |
| --- | --- | --- |
| History | The build refuses a feed that reads a table without history, naming it | Generator refusal test |

### Delivery and authorization

The package polls changes every `pollMs`, default 10000, and also responds to its trigger.
wamn has no query-version `moved` read, because unchanged changes cost a 304.
The optional generic `Feed.moved` member stays part of the package surface.
The bench's offline wrapper uses it only to suppress network reads while offline.

[`http-route/src/entry.rs`](../../apps/platform/ingress/http-route/src/entry.rs), `changes_etag`, derives a weak ETag from the returned cursor and route release.
[`http-route/src/lib.rs`](../../apps/platform/ingress/http-route/src/lib.rs), `feed_cache_control`, preserves the list's cache scope and adds `no-cache`.
A matching ETag produces 304.
The read's route supplies authentication and authorization for both result variants.

[`data_access.rs`](../../crates/schema/generator/src/data_access.rs), `FEED_HISTORY_COLUMNS`, limits feed SELECT grants to `transaction_id`, `row_key`, `type`, and `before`.
`derive_data_access_overlay_for_manifest` emits those grants for the history table of every table that a feeding query reads.
[`record-history.sql`](../../deploy/sql/record-history.sql), `wamn_history.create_history_table`, adds an index on `transaction_id`.
Each linking column needs an index on its base column, because the key mapping looks up root rows by it.

[`packaging-feed.ts`](../../web/sync-bench/src/packaging-feed.ts), `packagingFeed`, reads every list page up to its cap.
It keeps the first page's feed cursor, which covers changes between page reads.
It reports `complete: false` when the cap stops a fill with more pages available.
The default `RECORD_CAP` is 1000.
The bench's take-offline path requests the full selected set and the whole location reference collection.

| Rule | Invariant | Test |
| --- | --- | --- |
| Unchanged feed | Empty changes keep the cursor and support a matching weak ETag | Route 304 test |
| Feed privileges | The feed reads only its four granted history columns of each table read, through an indexed transaction range | Application-role SQL execution |

### Removed rules

The query feed removes these rules:

- The model-level `feed = true` and `feed = false`. A per-query `feed` setting replaces them.
- The client-side join path. Screens no longer join collections in `@tanstack/db`.
- Every rule that assumes one collection per model. A model can appear in many query results.

### Cost and measurement

These costs are an inference, not a measurement.
With no changes, the statement does one indexed `transaction_id` range read per table read, finds nothing, and the route answers 304.
With a few changed rows, it adds one indexed lookup per join hop and one restricted run over a few keys.
With many changed rows, the page limit bounds each call to 100 root keys.
So each call does at most one restricted run over 100 keys.
If PostgreSQL pushes the key predicate into the statement, the restricted run costs about as much as the query for those keys.

The first query to measure is `receiving.load_receipt_screen` over the view `receiving.receipt_screen_line`, the order lines with their order and item.
[Authored views](authored-views.md#receipt-screen) defines that view.
Collect these numbers:

- `changes` latency with 0, 10, and 1,000 changed rows, on lines, on orders, and on items.
- The plan (`EXPLAIN (ANALYZE, BUFFERS)`) of the restricted run, to show whether the key predicate reaches the joins.


## Browser store and live queries

### Collection

A collection covers one query, generated shape digest, and canonical argument set.
[`names.ts`](../../web/runtime/src/sync/names.ts), `collectionKey`, encodes those three values.
[`packaging-feed.ts`](../../web/sync-bench/src/packaging-feed.ts), `canonicalJson` and `packagingKey`, make equal argument sets share a key.
Rows use the root key as their collection key.

[`client_ts.rs`](../../crates/schema/generator/src/client_ts.rs), `shape_digest`, hashes generated row names, contract types, nested fields, and nullability in stable order.
`emit_model` exports each model's `*_SHAPE_DIGEST`.
[`materialize.rs`](../../crates/schema/generator/src/materialize.rs), `GENERATOR_ID`, is `wamn-schema-generator/0.1.10` for this emission.

[`persistence.ts`](../../web/runtime/src/sync/persistence.ts), `rowsPersistence`, deletes saved entries with unknown shape digests before its first storage operation.
It also deletes malformed keys.
It never opens the outbox database, because a shape change must not discard recorded requests.

### Fill and follow

[`collection.ts`](../../web/wfe/packages/sync/src/collection.ts), `collection`, owns startup, polling, refills, and persistence saves.
Startup loads saved rows first.
A failed load counts as no saved rows.
It then calls `Feed.list` once for that collection startup, and the list reconciles saved rows.
`Feed.list` itself can make multiple host requests to assemble its page set.

Saved rows supply first paint before the network read resolves.
A failed initial list preserves readable saved rows and exposes the error through `utils.error()`.
After a successful list, follow starts from its cursor.
If the list fails after saved rows load, follow starts from the saved cursor.

Follow runs on the timer, the trigger, or `utils.pull()`.
Follow passes and refills never overlap.
Queued requests coalesce, and a queued refill runs before a queued pull.
`CursorExpired` causes a fresh list.
Other feed errors keep rows and expose the error status until a successful pass.

Saves wait 250 ms after a successful list and after each change page.
Each save writes the current rows and cursor together.
Cleanup flushes a waiting save, stops the timer, unsubscribes the trigger, and aborts current reads.
A failed save changes no status.

### Live queries

Screens build queries over one query feed collection through `from`, `where`, `groupBy`, aggregates, and `orderBy` in `@tanstack/db`.
A screen never joins two collections, because PostgreSQL owns joins.
The engine updates results from change batches.
A query result is also a keyed collection, so both read adapters accept it.

Table screens use `liveRows`.
Small sets use `liveList`.
No custom aggregate can enforce decimal arithmetic inside `@tanstack/db`, because its aggregate surface does not accept custom aggregates.
A live-query float sum is exact only for int32-range totals.
Applications keep exact decimal calculations outside that aggregate path.

### Memory

Collections and live queries each retain their rows or results.
The table cap bounds ordinary fills.
Each screen needs a memory budget for its selected rows.
The take-offline bench sizes its full selected set before saving it.

## Solid 2 adapter

The adapter lives in `@wfe/sync`.
Generated packages consume it, and `@wfe/components` does not import it.
[`solid.ts`](../../web/wfe/packages/sync/src/solid.ts) owns both read shapes and `recordWrite`.

### Solid 2 surface the adapter uses

The adapter targets Solid `2.0.0-rc.18` or later.
Stores come from `solid-js`, and the web runtime comes from `@solidjs/web`.
The table also covers screen and test primitives around the adapter.
It imports `createStore`, `createProjection`, `createOptimisticStore`, `action`, `until`, `from`, `createMemo`, `runWithOwner`, `onSettled`, and `onCleanup`.

| Primitive | Signature | Use here |
| --- | --- | --- |
| `createStore` | `createStore(value)` → `[store, set]`. `set(draft => { ... })` | `liveRows`: the keyed row map. The draft-first setter applies a change batch in one write |
| `createProjection` | `createProjection(fn, seed?, { key })` → read-only derived store. `fn` can return a value, a promise or an async iterable. The result reconciles by `key` | `liveList`: ordered rows for small sets |
| `from` | `from(source)` where `source` is `(next, error, complete) => unsubscribe`, an Observable, or an AsyncIterable → AsyncIterable for a computation. It keeps the latest value. It closes on re-run or disposal | bridges `subscribeChanges` into `createProjection` |
| `createOptimisticStore` | `createOptimisticStore(fn, seed?, { key })` → `[store, set]`. Writes inside an `action` are tentative and revert or reconcile when the transition settles | `recordWrite`: the tentative row over the authoritative collection |
| `action` | `action(function* (...args) { ... })`. `yield` a promise or a reactive wait. The transition stays open until the body returns | the write flow: tentative write, enqueue, wait for echo |
| `until` | `until(() => predicate, { timeout?, signal? })` → promise. The action waits until authoritative data makes the predicate truthy. It rejects when the landing is an error | waits for the feed echo of the written row. It rejects on refusal |
| `refresh` | `refresh(target)` → promise of the next quiescent state | Screen-level refresh. Collection refills use `utils.refill()` |
| `affects` | `affects(target, key?)` marks data that in-flight work will change, so it reads pending | optional: mark a row while its entry is `sending` |
| `isPending` | `isPending(() => expr)` → boolean while a change to `expr` is in flight | "updating" marks. Never for outbox state, which is the entry's own |
| `latest` | `latest(fn)` peeks the in-flight value during a transition | status readers that must not suspend |
| `<Loading>` | `<Loading fallback={...} on={...}>` initial-readiness boundary. `on` controls re-showing on revalidation | first `fill` of a collection |
| `<Errored>` | `<Errored fallback={err => ...}>` | feed and `fill` failures |
| `createEffect` | `createEffect(compute, apply, { defer? })`, apply can return cleanup. `{ effect, error }` bundle form | pushing status to the outbox panel. Never to write stores |
| `createMemo` | `createMemo(fn, { lazy?, unobserved? })`. `lazy` autodisposes at zero subscribers | `syncStatus`, `syncComplete`, and `outboxStatus` |
| `onCleanup`, `onSettled` | cleanup registration in an owned scope. `onSettled` runs after render settles and can return cleanup | unsubscribe from `subscribeChanges`. Worker registration |
| `flush` | `flush()` applies batched writes now | tests only |
| `snapshot` | `snapshot(store)` → plain value | persisting rows. Building a request from a store row |
| `reconcile` | `reconcile(value, key)` | Bulk replacement in the measurement comparison |

Rules for adapter and screen code:

- Solid batches writes by microtask. Tests call `flush()` before immediate assertions.
- Subscription callbacks and event handlers write through setters outside reactive computations.
- Every row mutation uses the store setter. Writes to the proxy outside its setter trigger `STORE_WRITE_OUTSIDE_SETTER`.
- Reactive reads belong in JSX, memos, or effects. Writes inside reactive computations trigger `PRIMITIVE_IN_EFFECT_CALLBACK`.
- `createEffect` separates computation from application.
- Solid 2 does not expose `createResource`, `batch`, `on`, `produce`, `createComputed`, `Suspense`, `Index`, `onMount`, `mergeProps`, or `splitProps`. The table supplies their replacements.

### `liveRows`: a keyed store for tables

`liveRows` subscribes before seeding, because subscription can start a lazy query.
It seeds from collection entries and applies each later batch in one store write.
Keys come from the collection and convert to strings.
Rows need neither an id field nor a version field for this read adapter.

The screen owns display order.
The measurement screen sorts packaging codes in a memo.
The row map does not preserve an ordered array by itself.

### `liveList`: a projection for small sets

`liveList` pushes the collection's whole array after each batch.
It reconciles by the collection's own key, tracked for each row object.
`from` closes the subscription when the computation ends.
The cost grows with the row count, so this shape serves sets under a few hundred rows.

### Readiness and status

`syncStatus` bridges the source's status signal into a memo.
For other keyed collections, it maps ready and error directly and treats every other status as loading.
`syncComplete` bridges list completeness.
`outboxStatus` bridges the counts returned by `watch`.

The screen shows feed status, completeness, pending and refused counts, and the last successful send time.
Its “current as of” value comes from successful read time or the bench's saved timestamp.
It does not decode the cursor as a clock.

### Writes: action, optimistic store, and until

`recordWrite` overlays the expected row through a Solid optimistic store, enqueues the call, and waits with `until`.
Solid owns the optimistic row, because two optimistic layers can disagree about the same row.
The wait reads the authoritative collection, never the optimistic overlay.

[`solid.ts`](../../web/wfe/packages/sync/src/solid.ts), `confirmed`, requires a successful send to remove the entry and the collection to hold its id.
If `expected` supplies a version, the collection's version must be greater.
The collection's accessor reads both versions.
A query feed confirms a write by the rule in [Row version](#row-version), not by this comparison.
Refusal of a new write throws `OutboxRefused`, and discard throws `OutboxDiscarded`.
Either rejection ends the action and reverts its guess.
Restored refusals keep their tentative rows until discard or successful resubmission.

[`outbox.ts`](../../web/wfe/packages/sync/src/outbox.ts), `OutboxEntry`, stores `expected`.
[`solid.ts`](../../web/wfe/packages/sync/src/solid.ts), `recordWrite`, restores tentative rows from the outbox with `{ model }`, after the collection's first committed rows land.

### Pinned versions

[`pnpm-workspace.yaml`](../../pnpm-workspace.yaml), `overrides`, aligns the Solid runtime and compiler at `2.0.0-rc.18`.
Adapter tests cover delta application, keyed projections, feed confirmation, refusal, and overlapping actions.
`until` waits for authoritative confirmation or correction and rejects a failed landing.
Overlapping actions keep their optimistic rows whole while their source changes.
A feed echo must not remount a row that retains its key.
Keyed reconciliation preserves row identity through swaps, reordering, and shifts.

## Writes: outbox, replay, conflicts

Every write enters IndexedDB before the UI reports it recorded.
Online replay starts immediately.
Offline entries wait without a session token.
The host receives ordinary operations in both cases.

### Entry and enqueue

`OutboxEntry` is the exported type above, with no release field.
[`outbox.ts`](../../web/wfe/packages/sync/src/outbox.ts), `outbox`, assigns `seq` through IndexedDB's monotonic key generator.
`enqueue` resolves after the storage transaction commits.
The request, hash, target, and append-only flag stay unchanged during replay.
Only queue state and refusal details change.

`request_hash` comes from [`hash.ts`](../../web/runtime/src/sync/hash.ts), `requestHash`, over canonical request JSON.
The sender carries it as `idempotencyKey` and supplies a fresh transport request id.
The stored entry contains no credential.

### Replay

Replay persists `sending` before it calls `Send`.
On open, it resets stored sending entries to pending under the Web Lock named for the outbox database.
The same lock protects each replay pass and reloads entries from IndexedDB first.
Without Web Locks, two tabs can send one entry, and the host deduplicates through `request_hash`.

Entries send in `seq` order within `expect.id`.
Different records send concurrently, up to four by default.
A refused entry holds later entries for its record except `append_only` entries.
A retriable reply returns the entry to pending and stops the pass.
A successful reply removes the entry.

Replay runs on open, enqueue, a successful probe, and every 30 seconds while entries remain pending.
The probe runs before replay, on an online event, and every 15 seconds while the last probe fails.
`close` aborts sends and preserves their entries as pending.

[`send.ts`](../../web/runtime/src/sync/send.ts), `createSend`, forwards the entry's `AbortSignal` to the generated binding.
[`client_ts.rs`](../../crates/schema/generator/src/client_ts.rs), `emit_operation`, forwards it through `transport.invoke`.
[`transport.ts`](../../web/runtime/src/transport.ts), `createTransport`, passes it to fetch.

### Reply mapping

[`send.ts`](../../web/runtime/src/sync/send.ts), `reply`, owns the mapping.
`createSend` attempts at most one renewal for an unauthenticated reply.

| Host outcome | Outbox reply |
| --- | --- |
| `completed` | `{ ok: true }` |
| `uncertain`, including network failure, 5xx, or unreadable reply | Retriable |
| `retry`, `timeout` | Retriable |
| `unauthenticated` after failed renewal or another unauthenticated reply | Retriable |
| `internal_error` | Refused, with code and text |
| `partiallyCompleted` | Refused, with code and text `partial` |
| Missing operation binding | Refused, with code and text `unknown_operation` |
| Every other refusal | Refused, preserving its code and text |

`internal_error` is refused because retrying a deterministic failure forever blocks the record.
A thrown `Send` is retriable.

### Network detection

`navigator.onLine` is advisory for replay.
[`transport.ts`](../../web/runtime/src/transport.ts), `createTransport.probe`, requires a successful health reply within three seconds.
It sends HEAD with `cache: "no-store"` and no credentials.

### Conflicts

Revision guards produce ordinary refusals when another write changes the row first.
The panel shows the recorded request, current row, and refusal.
The user can correct or discard the entry.
No automatic merge changes the recorded request.

A correction creates a new immutable request entry with the same record id.
The client records that entry before discarding the refused one.
[`packaging-writes.ts`](../../web/sync-bench/src/packaging-writes.ts), `packagingWrites.resubmit`, enforces this order.
`Outbox.resubmit(seq)` retries unchanged arguments and does not edit an entry.

Append-only operations carry the parent id without a parent revision guard when their application contract allows it.
They still undergo all ordinary host validation.

### Ids and numbers

See [generated operations](../architecture/data-access.md#generated-operations) for client UUIDs and database-generated integer keys.
An integer-id model syncs reads, updates, and deletes. Its creates are online only.
[`call.ts`](../../web/runtime/src/sync/call.ts), `createCall`, generates UUIDv7 through [`ids.ts`](../../web/runtime/src/sync/ids.ts), `uuidv7`.

Numbers occupy separate host-assigned fields, because offline records need stable ids before the host assigns numbers.
[`numbers.ts`](../../web/runtime/src/sync/numbers.ts), `pendingNumber`, renders `pending·<last 4 hex>` until the feed supplies the number.
`shownNumber` exposes the real number when present.
Anything that consumes the number waits for `hasNumber`, including print, export, and typed references.
References between pending records use ids and need no remapping.

### Status surface

The shell shows pending and refused counts and the last successful send time.
A refusal panel keeps the operation, record, arguments, and refusal text alongside correction and discard actions.
A successful send or explicit discard removes an entry.
A refusal alone never removes it.

## Shell, storage, devices

### Service worker

[`vite.config.ts`](../../web/sync-bench/vite.config.ts), `precacheManifest`, supplies the built files to the worker.
[`sw.ts`](../../web/sync-bench/src/sw.ts), `precache`, caches the built shell and uses `live.html` as the deep-navigation fallback.
Precached assets use the cache first.
Other navigations use the network first and fall back only on network failure, not an HTTP error response.

The worker bypasses `/wamn` and `/password` on a path-segment boundary.
`precache` receives `bypassPrefixes: ['/wamn', '/password']` and excludes both paths from installation and fetch handling.
The worker never supplies cached application API or password replies.

The web manifest supplies a name, icon, and standalone display mode.
Installation remains optional.
The page calls `shell` with the worker URL resolved from the Vite base.

### Storage

[`names.ts`](../../web/runtime/src/sync/names.ts), `rowsDatabase` and `outboxDatabase`, scope separate databases by application, environment, and user.
The rows database stores `rows`, `cursor`, and `saved_at` under each collection key.
The outbox database stores entries by `seq`.
There is no release metadata store, because row shape keys and ordinary request validation supply the required boundaries.

`shell` requests persistent storage once at startup and reads the storage estimate.
[`shell-status.ts`](../../web/sync-bench/src/shell-status.ts), `shellStatus`, displays worker, persistence, protection, and quota results.
A persistence denial changes no other behavior.
Replay runs while the application is open without a Background Sync dependency.

### Take offline

Take-offline exists in sync-bench as a test of the mechanism.
It does not promise full offline coverage for generated applications.
[`packaging-offline.ts`](../../web/sync-bench/src/packaging-offline.ts), `packagingOffline`, saves the selected packaging scope and the whole location reference model.
[`location-feed.ts`](../../web/sync-bench/src/location-feed.ts), `locationFeed`, reads every reference page in groups of 100.

The action refuses an incomplete set.
`collectionBytes` measures serialized rows, cursor, key, and timestamp before IndexedDB overhead.
`requireStorage` requires an estimate with 20 percent headroom.
The action awaits durable saves before reporting counts, bytes, and read times.
The saved sets follow changes while online.

`storedCollection` restores saved empty sets and their timestamps, and reads saved rows without network calls while offline.
This wrapper exists because the pinned collection ignores empty saved sets and always attempts its initial list.
It awaits explicit saves because the package's debounced save hides persistence failures.
A missing local set shows “needs a connection”.

### Degradation

| Missing capability | Behavior |
| --- | --- |
| Service worker | The open page works. `shellStatus` reports the worker unavailable. |
| Persistent storage grant | The page reports storage unprotected. Take-offline remains available if quota permits. |
| IndexedDB | Collection load failures permit network fills. Durable outbox recording fails rather than claiming that a write is saved. |
| Storage estimate | Take-offline refuses because it cannot establish headroom. |
| Web Locks | Replay runs without the lock and relies on host deduplication. |
| Another browser | Capability results control behavior, not the browser name. |

## Session and authorization

The user id comes from `sub` in sign-in and renewal replies.
[`session.ts`](../../web/runtime/src/session.ts), `times`, maps it to `userId`.
`keepSession` renews on startup and before expiry.
The environment comes from the page route through [`packaging-writes.ts`](../../web/sync-bench/src/packaging-writes.ts), `packagingAddress`.

[`saved-user.ts`](../../web/sync-bench/src/saved-user.ts), `savedUser`, keeps the last signed-in id per application and environment in local storage.
[`live.tsx`](../../web/sync-bench/src/live.tsx), `App`, opens that user's rows and outbox during an offline reopen.
`SessionPackaging` gates upload on the matching signed-in user and shows “sign in to upload” while upload waits.
Local recording needs no token.

Replay uses the host's current authorization. The per-application active role and picker belong to [platform configuration C6](platform-config.md#roles).
A role change can produce an ordinary refusal.
The entry's `recorded_at` is a local display timestamp, not authoritative host time.
Applications can declare a separate business timestamp without changing queue behavior.
Signing out does not itself delete saved rows or entries.

## Release compatibility

A waiting worker shows “update available, reload”.
[`live.tsx`](../../web/sync-bench/src/live.tsx), `SessionPackaging`, waits for outbox loading before applying the reload rule.
[`update.ts`](../../web/sync-bench/src/update.ts), `canReload`, permits reload only when pending and sending counts are zero.
Refused entries survive reload and do not block it.

There is no release digest in the shell, because the router guest cannot read it on the health path and the build has none.
The waiting worker already signals a newer served shell.
The host validates old entries like every request.
A request shape that the host rejects becomes an ordinary refusal, because compatibility does not require a second request taxonomy.

Shape digests select saved row collections independently of request validation.
Unknown digests cause row cleanup without touching the outbox.
A refusal panel can present stored arguments for correction under the current binding.

`HEAD /api/health` returns 204 without authentication.
[`http-route/src/lib.rs`](../../apps/platform/ingress/http-route/src/lib.rs), `try_handle`, answers before route lookup and authentication.
`HttpResponse::cache_headers` supplies `no-store` for this response.

## Generator and bindings

The host generator emits the list feed and client helpers.
Epic 5 owns the remaining application wiring and manifest fields.
The bench's handwritten factories demonstrate that emission and do not replace it.

### Manifest (`wamn.k`)

Epic 5 adds these declarations:

```text
model receipt {
  operations.query.feed = false
}

projection receiving.load_receipt_screen {
  feed = true
}

operation receipt.record_line {
  append_only = true
}

screen receiving.lines {
  offline = true
  read = "liveRows"
}
```

`feed` belongs to one list read, generated or authored, as [Query feed](#query-feed-host) states.
It defaults on for a generated list read and off for an authored read.
An authored read turns it on with `feed = true`, and its view must pass the shape check in [Authored views](authored-views.md#feed-on-a-view).
`append_only` declares whether a prior refusal holds later entries for that record.
`offline` selects application-defined offline wiring without promising whole-application coverage.
`read` selects `liveRows` or `liveList` under the read-shape rule.

### Emitted per model

| Artifact | Owner | Content |
| --- | --- | --- |
| List statement and feed branch | Host generator | One list operation, optional feed part, and page or changes result |
| Snapshot cursor | List SQL | xmin from the list statement itself |
| Client binding | `client_ts.rs`, `emit_operation` and `write_changes` | List and changes helpers with signal forwarding |
| Shape digest | `client_ts.rs`, `shape_digest` and `emit_model` | Stable row shape hash |
| Collection factory, Epic 5 | Generated components | Feed paging, cap, argument key, version accessor, and persistence, for each feeding query |
| Write helper, Epic 5 | Generated components | Outbox and `recordWrite` wiring for the model |

### Emitted per screen

Epic 5 binds each screen to the collection of one feeding query.
The screen sorts, filters, and groups that query's result rows, and joins nothing.
An application-selected offline screen declares the collections and references that its action fills.
The generated action reports completion only after those chosen sets are complete and durable.

### Build refusal rules

Epic 5 refuses a feed by the rules in [Prerequisites and build refusal](#prerequisites-and-build-refusal).
A list read opts out through `feed = false`.
An append-only operation cannot also declare a revision guard.
Receiving and WMS regenerate from their definitions with no hand edits to generated files.

## Invariants and tests

Each row names a measurable rule and its test boundary.

| Rule | Invariant | Test |
| --- | --- | --- |
| One operation | Page and changes use one list permission row | Generated contract and route assertions |
| Feed paging | Below-xmin rows page by transaction and root key. Visible rows above xmin fill one complete page | Host and `Changes::new` tests |
| Feed collapse | Each feed page contains one current result per root key | History updates followed by a feed read |
| Retention | Only bounded retention can expire an older cursor | Retention and `cursor_expired` cases |
| List cap | A capped fill reports incomplete if more pages exist | `packagingFeed` paging test |
| Saved first paint | Saved rows render before fetch resolves | Adapter test with blocked fetch |
| Serialized reads | Startup, pull, and refill never overlap | Controlled feed promises |
| Saved updates | Saves debounce after list and change pages, then flush on cleanup | Persistence timing test |
| Shape cleanup | Unknown digests remove rows without altering outbox entries | `rowsPersistence` storage test |
| Delete delivery | A deletion removes a stored row | Fake feed test |
| Durable recording | Enqueue resolves only after the put commits | IndexedDB reload test |
| Replay recovery | Stored sending entries become pending under the lock | Reopen and shared-database tests |
| Replay order | One record sends in sequence. Other records can progress after its refusal | Controlled sender test |
| Refusal and discard | Typed errors reject the action and revert its guess | `OutboxRefused` and `OutboxDiscarded` tests |
| Immutable correction | A new entry keeps the id and commits before the refused entry disappears | `packagingWrites` correction test |
| Abort forwarding | Closing an outbox aborts the binding's fetch | Sender, binding, and transport tests |
| Client identity | Create requires an id. Server-owned and numbered keys refuse at generation | Generator refusal tests |
| Pending number | Number consumers wait until the feed supplies the host field | Number helper test |
| Offline reopen | The remembered user opens the same scoped rows and outbox before renewal | Browser reopen test |
| Shell fallback | Network failure on a deep navigation serves the fallback | Browser worker test |
| Worker bypass | Exact `/wamn` and `/password` segments never use the worker cache | Worker path-boundary tests |
| Reload | Pending or sending blocks reload. Refused alone does not | `canReload` test |
| Old requests | The current host accepts or refuses old arguments through ordinary validation | Old-entry browser test |
| Health | HEAD returns 204 and no-store before authentication | Route test |

## Assumptions

| Assumption | Evidence or test | Consequence when it does not hold |
| --- | --- | --- |
| A long-open transaction permits one page of visible commits before xmin advances | Hold one transaction, commit more than one page, read, then release the held transaction | Later visible rows wait for xmin |
| A live-query float sum is exact only for int32-range totals | The measurement sums integer `row_version`. `@tanstack/db` accepts no custom aggregates | Decimal or larger totals require a separate exact calculation |
| Generated list pages accept 1–100 rows | `generate/validation.rs`, `validate_query`, fixes the minimum and maximum | A 20,000-row reference fill takes about 200 requests |
| Retention covers the expected outage and recovery window | Retention declarations and host retention tests | Expired cursors require a fresh list |
| A feeding query reads only tables with history, and its rows have a unique root id | [Prerequisites and build refusal](#prerequisites-and-build-refusal) | The build refuses the feed and names the table |
| The selected read shape fits the screen's memory budget | Measure the screen with its selected rows | The application reduces its local set or uses ordinary pages |
| Keyed Solid projections preserve row identity | Adapter tests against the pinned runtime | The adapter needs a correction before adoption |
| Host request identity deduplicates a resend | Send one entry twice and assert one effect | Replay cannot claim effective single application |
| The browser keeps IndexedDB data during ordinary use | Browser tests on supported devices | The UI must report failed recording or unprotected storage |

## Deferred

Each item waits for a consumer:

- A platform event channel through SSE or WebSocket as an additional feed trigger.
- Automatic merge for concurrent edits to one record.
- Server-side incremental views for reporting screens.
- A native application.
- The local target consuming the same feed contract for its projections.
- Background Sync registration as an enhancement to replay.
- Partial sync of a query's result beyond the query's own filters.
- Shared optimistic state across users, including presence and live cursors.
- A request-shape hash if ordinary validation cannot distinguish incompatible meanings.

## Epics

| Epic | Repo | Scope | Done when |
| --- | --- | --- | --- |
| 5. Generator | wamn | Manifest fields, query feeds, collection and screen emission, feed opt-out, and build refusal | Receiving and WMS regenerate without hand edits. Refusal cases fail at build. |

## Read-shape rule

`@wfe/sync` and `@tanstack/db` stay.
Table screens use `liveRows`, and small sets use `liveList`.
