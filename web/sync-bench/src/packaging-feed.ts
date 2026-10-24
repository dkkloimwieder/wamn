/**
 * The feed and the collection of the WMS `packaging` model, over its
 * generated bindings. This is the one file per model that the generator will
 * emit later (docs/plan/wamn-sync.md, "Generator and bindings").
 */
import { collection, type Change, type Feed, type Row } from "@wfe/sync";
import type { Transport } from "@wamn/web-runtime";
import { query, type PackagingQueryRequest, type PackagingQueryRow } from "@wamn/wms-client/packaging.js";

/**
 * A packaging row as the collection holds it. The bindings name the version
 * `rowVersion`, and `@wfe/sync` reads `row_version`, so each row carries both
 * (wfe finding (i)).
 */
export type PackagingRow = PackagingQueryRow & Row;

/** The list arguments that a collection covers: the request without its cursor. */
export type PackagingScope = Omit<PackagingQueryRequest, "cursor">;

/** The cursor that `list` reports until the host returns one (Epic 2b). */
export const NO_CURSOR = "none";

/** Adds `row_version` beside `rowVersion`. This costs one object per row on every `list`. */
export function withRowVersion(row: PackagingQueryRow): PackagingRow {
  return { ...row, row_version: row.rowVersion };
}

/** The packaging feed over the generated `query` read. */
export function packagingFeed(transport: Transport): Feed<PackagingRow, PackagingScope> {
  return {
    async list(scope) {
      const outcome = await query(transport, [scope]);
      if (outcome.status !== "completed") throw new Error(`packaging query ${outcome.status}`);
      const value = outcome.value as typeof outcome.value & { readonly cursor?: string };
      return { rows: value.item.map(withRowVersion), cursor: value.cursor ?? NO_CURSOR };
    },
    // The host has no `changes` read yet (Epic 2b).
    async changes(_scope, cursor) {
      return { rows: [] as Change<PackagingRow>[], next_cursor: cursor, complete: true };
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

/** The packaging collection of one scope. */
export function packagingCollection(feed: Feed<PackagingRow, PackagingScope>, scope: PackagingScope) {
  return collection(feed, scope, { key: packagingKey(scope) });
}
