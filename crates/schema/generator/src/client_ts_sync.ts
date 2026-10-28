
$ROW_ALIAS;

/** List arguments without paging. */
export type $ROWScope = Omit<$STEMRequest, "cursor" | "limit" | "sort" | "feed">;

/** The default row cap for a collection. */
export const $CAP = 1000;

$PAGE_DECLARATION
// Part of the wamn-nsufz workaround: the exact snapshot version of a change.
const $SNAPSHOTS = new WeakMap<Change<$ROWRow>, Int64>();

function $TO_CHANGE(change: $STEMChange): Change<$ROWRow> {
  const converted: Change<$ROWRow> =
    change.deleted || change.data === undefined
      ? { id: change.id, row_version: Number(change.rowVersion), deleted: true }
      : { id: change.id, row_version: Number(change.rowVersion), deleted: false, data: change.data };
  $SNAPSHOTS.set(converted, change.rowVersion);
  return converted;
}

/** Read pages up to the cap and follow changes from the first page's cursor. */
export function $PREFIXFeed(transport: Transport, cap: number = $CAP): Feed<$ROWRow, $ROWScope> {
  return {
    async list(scope, signal) {
      const rows: $ROWRow[] = [];
      let complete: boolean;
      let cursor: string | undefined;
$PAGE_STATE
      for (;;) {
$PAGE_LIMIT
        const outcome = await $QUERY(
          transport,
          [$REQUEST],
          signal === undefined ? undefined : { signal },
        );
        if (outcome.status !== "completed") throw new Error(`$MODEL $QUERY ${outcome.status}`);
        cursor ??= outcome.value.cursor;
        rows.push(...outcome.value.item);
        const next = outcome.value.nextCursor;
        complete = next === null;
        if (next === null || rows.length >= cap) break;
$PAGE_ADVANCE
      }
      if (cursor === undefined) throw new Error("$MODEL $QUERY returned no feed cursor");
      return { rows, cursor, complete };
    },
    async changes(scope, cursor, limit, signal) {
      const outcome = await $CHANGES(transport, scope, cursor, limit, signal === undefined ? undefined : { signal });
      if (outcome.status === "refused" && outcome.code === "cursor_expired") throw new CursorExpired();
      if (outcome.status !== "completed") throw new Error(`$MODEL changes ${outcome.status}`);
      return {
        rows: outcome.value.rows.map($TO_CHANGE),
        next_cursor: outcome.value.nextCursor,
        complete: outcome.value.complete,
      };
    },
  };
}

/** Sort object keys so equal scopes produce equal text. */
export function $CANONICAL(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map($CANONICAL).join(",")}]`;
  if (value !== null && typeof value === "object") {
    const entries = Object.entries(value)
      .filter(([, member]) => member !== undefined)
      .sort(([left], [right]) => (left < right ? -1 : left > right ? 1 : 0));
    return `{${entries.map(([key, member]) => `${JSON.stringify(key)}:${$CANONICAL(member)}`).join(",")}}`;
  }
  return JSON.stringify(value);
}

/** The key includes the model, row shape and canonical scope. */
export function $PREFIXKey(scope: $ROWScope): string {
  return collectionKey("$COLLECTION", $DIGEST, $CANONICAL(scope));
}

/** Follow a scope with the binding's row version accessor. */
export function $PREFIXCollection(
  feed: Feed<$ROWRow, $ROWScope>,
  scope: $ROWScope,
  follow: { readonly pollMs?: number; readonly persistence?: Persistence } = {},
) {
  // Workaround for wamn-nsufz until the wfe pin carries its fix: the package
  // ignores an equal version, so accepted deliveries get a local order.
  const delivery = Symbol();
  type Delivered = $ROWRow & { readonly [delivery]?: number };
  type Batch = Map<string | number, Delivered | null>;
  let serial = 0;
  const accept = (row: $ROWRow, pending: Batch): Delivered => {
    const held = pending.has(row.$ID) ? pending.get(row.$ID) : c.get(row.$ID);
    const accepted =
      held != null && BigInt(row.rowVersion) < BigInt(held.rowVersion) ? held : { ...row, [delivery]: ++serial };
    pending.set(row.$ID, accepted);
    return accepted;
  };
  const ordered: Feed<$ROWRow, $ROWScope> = {
    ...feed,
    async list(...args) {
      const page = await feed.list(...args);
      const pending: Batch = new Map();
      return { ...page, rows: page.rows.map((row) => accept(row, pending)) };
    },
    async changes(...args) {
      const page = await feed.changes(...args);
      const pending: Batch = new Map();
      return {
        ...page,
        rows: page.rows.flatMap<Change<$ROWRow>>((change) => {
          const held = pending.has(change.id) ? pending.get(change.id) : c.get(change.id);
          if (change.deleted) {
            if (held != null && BigInt($SNAPSHOTS.get(change) ?? change.row_version) < BigInt(held.rowVersion))
              return [];
            pending.set(change.id, null);
            return [change];
          }
          return [{ ...change, data: accept(change.data, pending) }];
        }),
      };
    },
  };
  const c: SyncCollection<$ROWRow> = collection(ordered, scope, {
    key: $PREFIXKey(scope),
    $GET_KEYversion: (row) => (row as Delivered)[delivery] ?? 0,
    ...follow,
  });
  return c;
}

/** Record a write for this model through the supplied outbox. */
export function $PREFIXWrite(c: SyncCollection<$ROWRow>, o: Outbox$KEY_TYPE$TRANSPORT) {
  $WRITE_BODY;
}
