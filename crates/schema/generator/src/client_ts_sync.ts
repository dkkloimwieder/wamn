
/** The row returned by the list binding. */
export type $ROWRow = $STEMRow;

/** List arguments without paging. */
export type $ROWScope = Omit<$STEMRequest, "cursor" | "limit" | "sort" | "feed">;

/** The default row cap for a collection. */
export const RECORD_CAP = 1000;

/** The largest page the list read serves. */
const PAGE_MAXIMUM = 100;

function toChange(change: $STEMChange): Change<$ROWRow> {
  return change.deleted || change.data === undefined
    ? { id: change.id, row_version: change.$REVISION, deleted: true }
    : { id: change.id, row_version: change.$REVISION, deleted: false, data: change.data };
}

/** Read pages up to the cap and follow changes from the first page's cursor. */
export function $PREFIXFeed(transport: Transport, cap: number = RECORD_CAP): Feed<$ROWRow, $ROWScope> {
  return {
    async list(scope, signal) {
      const rows: $ROWRow[] = [];
      let complete: boolean;
      let cursor: string | undefined;
      let page: string | undefined;
      for (;;) {
        const limit = Math.min(PAGE_MAXIMUM, cap - rows.length);
        const outcome = await $QUERY(transport, [page === undefined ? { ...scope, limit } : { ...scope, limit, cursor: page }], signal === undefined ? undefined : { signal });
        if (outcome.status !== "completed") throw new Error(`$MODEL $QUERY ${outcome.status}`);
        cursor ??= outcome.value.cursor;
        rows.push(...outcome.value.item);
        const next = outcome.value.nextCursor;
        complete = next === null;
        if (next === null || rows.length >= cap) break;
        page = next;
      }
      if (cursor === undefined) throw new Error("$MODEL $QUERY returned no feed cursor");
      return { rows, cursor, complete };
    },
    async changes(scope, cursor, limit, signal) {
      const outcome = await changes(transport, scope, cursor, limit, signal === undefined ? undefined : { signal });
      if (outcome.status === "refused" && outcome.code === "cursor_expired") throw new CursorExpired();
      if (outcome.status !== "completed") throw new Error(`$MODEL changes ${outcome.status}`);
      return {
        rows: outcome.value.rows.map(toChange),
        next_cursor: outcome.value.nextCursor,
        complete: outcome.value.complete,
      };
    },
  };
}

/** Sort object keys so equal scopes produce equal text. */
export function canonicalJson(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  if (value !== null && typeof value === "object") {
    const entries = Object.entries(value)
      .filter(([, member]) => member !== undefined)
      .sort(([left], [right]) => (left < right ? -1 : left > right ? 1 : 0));
    return `{${entries.map(([key, member]) => `${JSON.stringify(key)}:${canonicalJson(member)}`).join(",")}}`;
  }
  return JSON.stringify(value);
}

/** The key includes the model, row shape and canonical scope. */
export function $PREFIXKey(scope: $ROWScope): string {
  return collectionKey("$MODEL", $DIGEST, canonicalJson(scope));
}

/** Follow a scope with the binding's row version accessor. */
export function $PREFIXCollection(
  feed: Feed<$ROWRow, $ROWScope>,
  scope: $ROWScope,
  follow: { readonly pollMs?: number; readonly persistence?: Persistence } = {},
) {
  return collection(feed, scope, { key: $PREFIXKey(scope), $GET_KEYversion: (row) => row.$REVISION, ...follow });
}

/** Record a write for this model through the supplied outbox. */
export function $PREFIXWrite(c: ReturnType<typeof $PREFIXCollection>, o: Outbox$KEY_TYPE$TRANSPORT) {
  $WRITE_BODY
}
