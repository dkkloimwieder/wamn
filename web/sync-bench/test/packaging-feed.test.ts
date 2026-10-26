import { describe, expect, it } from "vitest";
import type { InvokeOptions, JsonValue, Transport, WireRequest } from "@wamn/web-runtime";
import { CursorExpired } from "@wfe/sync";
import { PACKAGING_SHAPE_DIGEST } from "@wamn/wms-client/packaging.js";
import { canonicalJson, packagingFeed, packagingKey } from "../src/packaging-feed.js";

const wireRow = {
  created_at: "2026-01-01T00:00:00Z",
  created_by: "00000000-0000-7000-8000-000000000001",
  id: "00000000-0000-7000-8000-000000000002",
  located_at: "2026-01-01T00:00:00Z",
  location_id: "00000000-0000-7000-8000-000000000003",
  packaging_code: "PK-1",
  row_version: 4,
  status: "available",
  type: "pallet",
  updated_at: "2026-01-01T00:00:00Z",
  updated_by: "00000000-0000-7000-8000-000000000001",
};

/** A transport that answers each request with the next value, and records it. */
function transport(values: JsonValue[], seen: WireRequest[] = []): Transport {
  return {
    async invoke(request, _options?: InvokeOptions) {
      seen.push(request);
      const value = values.shift();
      if (value === undefined) throw new Error("no answer left");
      return { status: "completed", value };
    },
  } as Transport;
}

describe("packagingFeed", () => {
  it("lists every page and returns the feed cursor of the first", async () => {
    const seen: WireRequest[] = [];
    const feed = packagingFeed(
      transport(
        [
          { item: [wireRow], next_cursor: "p2", cursor: "c1" },
          { item: [{ ...wireRow, id: "00000000-0000-7000-8000-000000000009" }], next_cursor: null, cursor: "c2" },
        ],
        seen,
      ),
    );
    const page = await feed.list({ filter: { status: ["available"] } });
    expect(page.cursor).toBe("c1");
    expect(page.rows).toHaveLength(2);
    expect(page.rows[0]).toMatchObject({ id: wireRow.id, rowVersion: 4, packagingCode: "PK-1" });
    expect(page.complete).toBe(true);
    expect(seen.map((request) => request.items)).toEqual([
      [{ filter: { status: ["available"] }, limit: 100 }],
      [{ filter: { status: ["available"] }, limit: 100, cursor: "p2" }],
    ]);
  });

  it("stops at the cap and reports that the full set is not read", async () => {
    const seen: WireRequest[] = [];
    const feed = packagingFeed(transport([{ item: [wireRow], next_cursor: "p2", cursor: "c1" }], seen), 1);
    const page = await feed.list({});
    expect(page.rows).toHaveLength(1);
    expect(page.complete).toBe(false);
    expect(seen[0]?.items).toEqual([{ limit: 1 }]);
  });

  it("refuses a list that did not complete", async () => {
    const feed = packagingFeed({
      async invoke(_request: WireRequest, _options?: InvokeOptions) {
        return { status: "refused", code: "permission_denied", detail: null };
      },
    } as Transport);
    await expect(feed.list({})).rejects.toThrow("packaging query refused");
  });

  it("reads changes through the list with its feed part", async () => {
    const seen: WireRequest[] = [];
    const feed = packagingFeed(
      transport(
        [
          {
            rows: [
              { id: wireRow.id, row_version: 5, deleted: false, data: { ...wireRow, row_version: 5 } },
              { id: "00000000-0000-7000-8000-000000000009", row_version: 2, deleted: true },
            ],
            next_cursor: "c2",
            complete: true,
          },
        ],
        seen,
      ),
    );
    const page = await feed.changes({ filter: { status: ["held"] } }, "c1", 50);
    expect(seen[0]?.items).toEqual([{ filter: { status: ["held"] }, feed: { cursor: "c1", limit: 50 } }]);
    expect(seen[0]?.contract.resultClass).toBe("changes");
    expect(page.next_cursor).toBe("c2");
    expect(page.complete).toBe(true);
    expect(page.rows[0]).toMatchObject({ id: wireRow.id, row_version: 5, deleted: false, data: { rowVersion: 5 } });
    expect(page.rows[1]).toEqual({ id: "00000000-0000-7000-8000-000000000009", row_version: 2, deleted: true });
  });

  it("throws CursorExpired when the host no longer holds the cursor", async () => {
    const feed = packagingFeed({
      async invoke(_request: WireRequest, _options?: InvokeOptions) {
        return { status: "refused", code: "cursor_expired", detail: null };
      },
    } as Transport);
    await expect(feed.changes({}, "c1")).rejects.toBeInstanceOf(CursorExpired);
  });
});

describe("packagingKey", () => {
  it("encodes the list arguments as canonical JSON", () => {
    const left = packagingKey({ filter: { status: ["held"], packagingCode: ["A"] } });
    const right = packagingKey({ filter: { packagingCode: ["A"], status: ["held"] } });
    expect(left).toBe(right);
    expect(JSON.parse(left)).toEqual(["packaging", PACKAGING_SHAPE_DIGEST, '{"filter":{"packagingCode":["A"],"status":["held"]}}']);
  });

  it("keeps array order, because the arguments are ordered", () => {
    expect(canonicalJson({ status: ["held", "available"] })).not.toBe(canonicalJson({ status: ["available", "held"] }));
  });
});
