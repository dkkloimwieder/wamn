import { describe, expect, it } from "vitest";
import type { InvokeOptions, JsonValue, Transport, WireRequest } from "@wamn/web-runtime";
import { CursorExpired } from "@wfe/sync";
import { PACKAGING_SHAPE_DIGEST } from "../src/generated/wamn_wms/packaging.js";
import { canonicalJson, packagingFeed, packagingKey } from "../src/generated/wamn_wms/packaging.js";

const wireRow = {
  created_at: "2026-01-01T00:00:00Z",
  created_by: "00000000-0000-7000-8000-000000000001",
  id: "00000000-0000-7000-8000-000000000002",
  located_at: "2026-01-01T00:00:00Z",
  location_id: "00000000-0000-7000-8000-000000000003",
  packaging_code: "PK-1",
  row_version: "4",
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
    {
      const observed = page.rows[0];
      expect(observed).toHaveProperty(["id"], wireRow.id);
      expect(observed).toHaveProperty(["rowVersion"], "4");
      expect(observed).toHaveProperty(["packagingCode"], "PK-1");
    }
    expect(page.complete).toBe(true);
    {
      const observed = seen.map((request) => request.items);
      expect(observed).toHaveProperty(["length"], 2);
      expect(observed).toHaveProperty(["0", "length"], 1);
      expect(observed).toHaveProperty(["0", "0", "filter", "status", "length"], 1);
      expect(observed).toHaveProperty(["0", "0", "filter", "status", "0"], "available");
      expect(observed).toHaveProperty(["0", "0", "limit"], 100);
      expect(observed).toHaveProperty(["1", "length"], 1);
      expect(observed).toHaveProperty(["1", "0", "filter", "status", "length"], 1);
      expect(observed).toHaveProperty(["1", "0", "filter", "status", "0"], "available");
      expect(observed).toHaveProperty(["1", "0", "limit"], 100);
      expect(observed).toHaveProperty(["1", "0", "cursor"], "p2");
    }
  });

  it("stops at the cap and reports that the full set is not read", async () => {
    const seen: WireRequest[] = [];
    const feed = packagingFeed(transport([{ item: [wireRow], next_cursor: "p2", cursor: "c1" }], seen), 1);
    const page = await feed.list({});
    expect(page.rows).toHaveLength(1);
    expect(page.complete).toBe(false);
    {
      const observed = seen[0]?.items;
      expect(observed).toHaveProperty(["length"], 1);
      expect(observed).toHaveProperty(["0", "limit"], 1);
    }
  });

  it("refuses a list that did not complete", async () => {
    const feed = packagingFeed({
      async invoke(_request: WireRequest, _options?: InvokeOptions) {
        return { status: "refused", code: "permission_denied", detail: null };
      },
    } as Transport);
    await expect(feed.list({})).rejects.toThrow();
  });

  it("reads changes through the list with its feed part", async () => {
    const seen: WireRequest[] = [];
    const feed = packagingFeed(
      transport(
        [
          {
            rows: [
              { id: wireRow.id, row_version: "5", deleted: false, data: { ...wireRow, row_version: "5" } },
              { id: "00000000-0000-7000-8000-000000000009", row_version: "2", deleted: true },
            ],
            next_cursor: "c2",
            complete: true,
          },
        ],
        seen,
      ),
    );
    const page = await feed.changes({ filter: { status: ["held"] } }, "c1", 50);
    {
      const observed = seen[0]?.items;
      expect(observed).toHaveProperty(["length"], 1);
      expect(observed).toHaveProperty(["0", "filter", "status", "length"], 1);
      expect(observed).toHaveProperty(["0", "filter", "status", "0"], "held");
      expect(observed).toHaveProperty(["0", "feed", "cursor"], "c1");
      expect(observed).toHaveProperty(["0", "feed", "limit"], 50);
    }
    expect(seen[0]?.contract.resultClass).toBe("changes");
    expect(page.next_cursor).toBe("c2");
    expect(page.complete).toBe(true);
    {
      const observed = page.rows[0];
      expect(observed).toHaveProperty(["id"], wireRow.id);
      expect(observed).toHaveProperty(["row_version"], 5);
      expect(observed).toHaveProperty(["deleted"], false);
      expect(observed).toHaveProperty(["data", "rowVersion"], "5");
    }
    {
      const observed = page.rows[1];
      expect(observed).toHaveProperty(["id"], "00000000-0000-7000-8000-000000000009");
      expect(observed).toHaveProperty(["row_version"], 2);
      expect(observed).toHaveProperty(["deleted"], true);
    }
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
    {
      const observed = JSON.parse(left);
      expect(observed).toHaveProperty(["length"], 3);
      expect(observed).toHaveProperty(["0"], "packaging");
      expect(observed).toHaveProperty(["1"], PACKAGING_SHAPE_DIGEST);
      expect(observed).toHaveProperty(["2"], '{"filter":{"packagingCode":["A"],"status":["held"]}}');
    }
  });

  it("keeps array order, because the arguments are ordered", () => {
    expect(canonicalJson({ status: ["held", "available"] })).not.toBe(canonicalJson({ status: ["available", "held"] }));
  });
});
