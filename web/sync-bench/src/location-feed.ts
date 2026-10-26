import { CursorExpired, type Feed } from "@wfe/sync";
import { collectionKey, type Transport } from "@wamn/web-runtime";
import { changes, query, LOCATION_SHAPE_DIGEST, type LocationQueryRow } from "@wamn/wms-client/location.js";

export type LocationRow = LocationQueryRow;
export const locationKey = collectionKey("location", LOCATION_SHAPE_DIGEST, "{}");

/** Read the whole location model in 100-row pages, then follow from the first cursor. */
export function locationFeed(transport: Transport): Feed<LocationRow, Record<string, never>> {
  return {
    async list(_scope, signal) {
      const rows: LocationRow[] = [];
      let cursor: string | undefined;
      let page: string | undefined;
      for (;;) {
        const outcome = await query(transport, [{ limit: 100, ...(page === undefined ? {} : { cursor: page }) }], { signal });
        if (outcome.status !== "completed") throw new Error(`location query ${outcome.status}`);
        cursor ??= outcome.value.cursor;
        rows.push(...outcome.value.item);
        if (outcome.value.nextCursor === null) break;
        page = outcome.value.nextCursor;
      }
      if (cursor === undefined) throw new Error("location query returned no feed cursor");
      return { rows, cursor, complete: true };
    },
    async changes(scope, cursor, limit, signal) {
      const outcome = await changes(transport, scope, cursor, limit, { signal });
      if (outcome.status === "refused" && outcome.code === "cursor_expired") throw new CursorExpired();
      if (outcome.status !== "completed") throw new Error(`location changes ${outcome.status}`);
      return {
        rows: outcome.value.rows.map((change) => change.deleted || change.data === undefined
          ? { id: change.id, row_version: change.rowVersion, deleted: true as const }
          : { id: change.id, row_version: change.rowVersion, deleted: false as const, data: change.data }),
        next_cursor: outcome.value.nextCursor,
        complete: outcome.value.complete,
      };
    },
  };
}
