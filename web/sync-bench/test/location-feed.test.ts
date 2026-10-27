import { expect, it } from "vitest";
import { CursorExpired } from "@wfe/sync";
import type { Transport, WireRequest } from "@wamn/web-runtime";
import { locationFeed } from "../src/generated/wamn_wms/location.js";

it("reads all 20,002 locations in 201 requests and keeps the first feed cursor", async () => {
  const seen: WireRequest[] = [];
  const transport = {
    async invoke(request: WireRequest) {
      seen.push(request);
      const start = (seen.length - 1) * 100;
      return {
        status: "completed",
        value: {
          item: Array.from({ length: Math.min(100, 20_002 - start) }, (_, i) => ({
            id: String(start + i),
            location_code: `LOC-${start + i}`,
            row_version: 1,
            created_at: "2026-01-01T00:00:00.000Z",
            description: null,
          })),
          next_cursor: start + 100 >= 20_002 ? null : `page-${seen.length + 1}`,
          cursor: `feed-${seen.length}`,
        },
      };
    },
  } as Transport;
  const page = await locationFeed(transport, Infinity).list({});
  expect(page.rows).toHaveLength(20_002);
  expect(page.cursor).toBe("feed-1");
  expect(page.complete).toBe(true);
  expect(seen).toHaveLength(201);
  expect(seen[0]?.items).toEqual([{ limit: 100 }]);
  expect(seen[200]?.items).toEqual([{ limit: 100, cursor: "page-201" }]);
});

it("maps location changes and cursor expiry through the generated feed binding", async () => {
  const seen: WireRequest[] = [];
  const feed = locationFeed({
    async invoke(request: WireRequest) {
      seen.push(request);
      return {
        status: "completed",
        value: {
          rows: [
            {
              id: "1",
              row_version: 2,
              deleted: false,
              data: {
                id: "1",
                row_version: 2,
                location_code: "NEW",
                description: null,
                created_at: "2026-01-01T00:00:00Z",
              },
            },
            { id: "2", row_version: 3, deleted: true },
          ],
          next_cursor: "c2",
          complete: true,
        },
      };
    },
  } as Transport);
  const page = await feed.changes({}, "c1", 100);
  expect(seen[0]?.items).toEqual([{ feed: { cursor: "c1", limit: 100 } }]);
  expect(page.rows[0]).toMatchObject({ id: "1", row_version: 2, data: { rowVersion: 2, locationCode: "NEW" } });
  expect(page.rows[1]).toEqual({ id: "2", row_version: 3, deleted: true });
  expect(page.next_cursor).toBe("c2");
  const expired = locationFeed({
    async invoke() {
      return { status: "refused", code: "cursor_expired", detail: null };
    },
  } as Transport);
  await expect(expired.changes({}, "c1")).rejects.toBeInstanceOf(CursorExpired);
});
