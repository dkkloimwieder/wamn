import { describe, expect, it } from "vitest";
import type { JsonValue, Transport, WireRequest } from "@wamn/web-runtime";
import { NO_CURSOR, canonicalJson, packagingFeed, packagingKey } from "../src/packaging-feed";

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

function transport(value: JsonValue, seen: WireRequest[] = []): Transport {
  return {
    async invoke(request) {
      seen.push(request);
      return { status: "completed", value };
    },
  } as Transport;
}

describe("packagingFeed", () => {
  it("lists through the generated read and carries row_version", async () => {
    const seen: WireRequest[] = [];
    const feed = packagingFeed(transport({ item: [wireRow], next_cursor: null }, seen));
    const page = await feed.list({ limit: 25, filter: { status: ["available"] } });
    expect(page.cursor).toBe(NO_CURSOR);
    expect(page.rows).toHaveLength(1);
    expect(page.rows[0]).toMatchObject({ id: wireRow.id, rowVersion: 4, row_version: 4, packagingCode: "PK-1" });
    expect(seen[0]?.items).toEqual([{ limit: 25, filter: { status: ["available"] } }]);
  });

  it("takes the cursor from the outcome when the host returns one", async () => {
    const feed = packagingFeed(transport({ item: [], next_cursor: null, cursor: "c1" }));
    expect((await feed.list({})).cursor).toBe("c1");
  });

  it("refuses a list that did not complete", async () => {
    const feed = packagingFeed({
      async invoke() {
        return { status: "refused", code: "permission_denied", detail: null };
      },
    } as Transport);
    await expect(feed.list({})).rejects.toThrow("packaging query refused");
  });

  it("answers changes with no rows and the same cursor", async () => {
    const feed = packagingFeed(transport({ item: [], next_cursor: null }));
    expect(await feed.changes({}, "c1")).toEqual({ rows: [], next_cursor: "c1", complete: true });
  });
});

describe("packagingKey", () => {
  it("encodes the list arguments as canonical JSON", () => {
    const left = packagingKey({ limit: 25, filter: { status: ["held"], packagingCode: ["A"] } });
    const right = packagingKey({ filter: { packagingCode: ["A"], status: ["held"] }, limit: 25 });
    expect(left).toBe(right);
    expect(left).toBe('packaging:{"filter":{"packagingCode":["A"],"status":["held"]},"limit":25}');
  });

  it("keeps array order, because the arguments are ordered", () => {
    expect(canonicalJson({ status: ["held", "available"] })).not.toBe(canonicalJson({ status: ["available", "held"] }));
  });
});
