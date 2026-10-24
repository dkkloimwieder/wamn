/**
 * The feed and the collection of the WMS `packaging` model, over its
 * generated bindings. This is the one file per model that the generator will
 * emit later (docs/plan/wamn-sync.md, "Generator and bindings").
 */
import { CursorExpired, collection, type Change, type Feed, type Row } from "@wfe/sync";
import type { Transport } from "@wamn/web-runtime";
import {
  changes,
  query,
  type PackagingQueryChange,
  type PackagingQueryRequest,
  type PackagingQueryRow,
} from "@wamn/wms-client/packaging.js";

/**
 * A packaging row as the collection holds it. The bindings name the version
 * `rowVersion`, and `@wfe/sync` reads `row_version`, so each row carries both
 * (wfe finding (i), bead wamn-7e86p).
 */
export type PackagingRow = PackagingQueryRow & Row;

/** The list arguments that a collection covers: the filters, without paging. */
export type PackagingScope = Omit<PackagingQueryRequest, "cursor" | "limit" | "sort" | "feed">;

/** The rows a list reads at most: the table's default per-table cap. */
export const RECORD_CAP = 1000;

/** The largest page the packaging list read serves. */
const PAGE_MAXIMUM = 100;

/** Adds `row_version` beside `rowVersion`. This costs one object per row on every `list`. */
export function withRowVersion(row: PackagingQueryRow): PackagingRow {
  return { ...row, row_version: row.rowVersion };
}

/** One host change as a `@wfe/sync` change. */
function toChange(change: PackagingQueryChange): Change<PackagingRow> {
  return change.deleted || change.data === undefined
    ? { id: change.id, row_version: change.rowVersion, deleted: true }
    : { id: change.id, row_version: change.rowVersion, deleted: false, data: withRowVersion(change.data) };
}

/** The packaging feed, and whether its last list read every row of its scope. */
export interface PackagingFeed extends Feed<PackagingRow, PackagingScope> {
  /** False when the last list stopped at the cap: "Full dataset cannot be loaded". */
  fullyRead(): boolean;
}

/**
 * The packaging feed over the generated list read. `list` reads every page up
 * to `cap` rows and returns the feed cursor of its first page, so the feed
 * also covers what changed between pages. `changes` reads the list with its
 * feed part. The feed has no `moved`: the package calls `changes` on its
 * timer, and an unchanged feed is a 304.
 */
export function packagingFeed(transport: Transport, cap: number = RECORD_CAP): PackagingFeed {
  let fullyRead = true;
  return {
    fullyRead: () => fullyRead,
    async list(scope) {
      const rows: PackagingRow[] = [];
      let cursor: string | undefined;
      let page: string | undefined;
      for (;;) {
        const limit = Math.min(PAGE_MAXIMUM, cap - rows.length);
        const outcome = await query(transport, [page === undefined ? { ...scope, limit } : { ...scope, limit, cursor: page }]);
        if (outcome.status !== "completed") throw new Error(`packaging query ${outcome.status}`);
        cursor ??= outcome.value.cursor;
        rows.push(...outcome.value.item.map(withRowVersion));
        const next = outcome.value.nextCursor;
        fullyRead = next === null;
        if (next === null || rows.length >= cap) break;
        page = next;
      }
      if (cursor === undefined) throw new Error("packaging query returned no feed cursor");
      return { rows, cursor };
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

/** The collection key: the model, then the list arguments as canonical JSON. */
export function packagingKey(scope: PackagingScope): string {
  return `packaging:${canonicalJson(scope)}`;
}

/** The packaging collection of one scope. It follows the feed every `pollMs`, 10 s by default. */
export function packagingCollection(
  feed: Feed<PackagingRow, PackagingScope>,
  scope: PackagingScope,
  follow: { readonly pollMs?: number } = {},
) {
  return collection(feed, scope, { key: packagingKey(scope), ...follow });
}
