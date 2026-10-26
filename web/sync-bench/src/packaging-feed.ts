/**
 * The feed and the collection of the WMS `packaging` model, over its
 * generated bindings. This is the one file per model that the generator will
 * emit later (docs/plan/wamn-sync.md, "Generator and bindings").
 */
import { CursorExpired, collection, type Change, type Feed, type Persistence } from "@wfe/sync";
import { collectionKey, type Transport } from "@wamn/web-runtime";
import {
  changes,
  query,
  PACKAGING_SHAPE_DIGEST,
  type PackagingQueryChange,
  type PackagingQueryRequest,
  type PackagingQueryRow,
} from "@wamn/wms-client/packaging.js";

/** A packaging row as the generated binding returns it. */
export type PackagingRow = PackagingQueryRow;

/** The list arguments that a collection covers: the filters, without paging. */
export type PackagingScope = Omit<PackagingQueryRequest, "cursor" | "limit" | "sort" | "feed">;

/** The rows a list reads at most: the table's default per-table cap. */
export const RECORD_CAP = 1000;

/** The largest page the packaging list read serves. */
const PAGE_MAXIMUM = 100;

/** One host change as a `@wfe/sync` change. */
function toChange(change: PackagingQueryChange): Change<PackagingRow> {
  return change.deleted || change.data === undefined
    ? { id: change.id, row_version: change.rowVersion, deleted: true }
    : { id: change.id, row_version: change.rowVersion, deleted: false, data: change.data };
}

/**
 * The packaging feed over the generated list read. `list` reads every page up
 * to `cap` rows and returns the feed cursor of its first page, so the feed
 * also covers what changed between pages. `changes` reads the list with its
 * feed part. The feed has no `moved`: the package calls `changes` on its
 * timer, and an unchanged feed is a 304.
 */
export function packagingFeed(transport: Transport, cap: number = RECORD_CAP): Feed<PackagingRow, PackagingScope> {
  return {
    async list(scope) {
      const rows: PackagingRow[] = [];
      let complete = true;
      let cursor: string | undefined;
      let page: string | undefined;
      for (;;) {
        const limit = Math.min(PAGE_MAXIMUM, cap - rows.length);
        const outcome = await query(transport, [page === undefined ? { ...scope, limit } : { ...scope, limit, cursor: page }]);
        if (outcome.status !== "completed") throw new Error(`packaging query ${outcome.status}`);
        cursor ??= outcome.value.cursor;
        rows.push(...outcome.value.item);
        const next = outcome.value.nextCursor;
        complete = next === null;
        if (next === null || rows.length >= cap) break;
        page = next;
      }
      if (cursor === undefined) throw new Error("packaging query returned no feed cursor");
      return { rows, cursor, complete };
    },
    async changes(scope, cursor, limit) {
      const outcome = await changes(transport, scope, cursor, limit);
      if (outcome.status === "refused" && outcome.code === "cursor_expired") throw new CursorExpired();
      if (outcome.status !== "completed") throw new Error(`packaging changes ${outcome.status}`);
      return {
        rows: outcome.value.rows.map(toChange),
        next_cursor: outcome.value.nextCursor,
        complete: outcome.value.complete,
      };
    },
  };
}

/** JSON with object keys in sorted order, so equal arguments give equal text. */
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

/** The collection key includes its row shape and canonical list arguments. */
export function packagingKey(scope: PackagingScope): string {
  return collectionKey("packaging", PACKAGING_SHAPE_DIGEST, canonicalJson(scope));
}

/** The packaging collection of one scope. It follows the feed every `pollMs`, 10 s by default. */
export function packagingCollection(
  feed: Feed<PackagingRow, PackagingScope>,
  scope: PackagingScope,
  follow: { readonly pollMs?: number; readonly persistence?: Persistence } = {},
) {
  return collection(feed, scope, { key: packagingKey(scope), version: (row) => row.rowVersion, ...follow });
}
