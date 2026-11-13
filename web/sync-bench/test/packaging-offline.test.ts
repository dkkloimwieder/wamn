import "fake-indexeddb/auto";
import { afterEach, expect, it, vi } from "vitest";
import type { Feed, Persistence } from "@wfe/sync";
import { rowsPersistence } from "@wamn/web-runtime";
import { PACKAGING_SHAPE_DIGEST } from "../src/generated/wamn_wms/packaging.js";
import { LOCATION_SHAPE_DIGEST } from "../src/generated/wamn_wms/location.js";
import { packagingKey, type PackagingRow, type PackagingFilter } from "../src/generated/wamn_wms/packaging.js";
import { locationKey, type LocationRow } from "../src/generated/wamn_wms/location.js";
import {
  collectionBytes,
  needsConnection,
  packagingOffline,
  packagingFilter,
  requireStorage,
} from "../src/packaging-offline.js";
import { makeRows } from "../src/fake.js";

const cleanups: Array<() => Promise<void>> = [];
afterEach(async () => {
  for (const close of cleanups.splice(0)) await close();
  vi.restoreAllMocks();
});

function fixture() {
  const rows = makeRows(3).map((row, i) => ({ ...row, status: i === 0 ? ("held" as const) : ("available" as const) }));
  let locations = [
    {
      id: rows[0]!.locationId,
      locationCode: "LOC-A",
      rowVersion: "1",
      description: null,
      createdAt: rows[0]!.createdAt,
    },
  ] as LocationRow[];
  const packaging: Feed<PackagingRow, PackagingFilter> = {
    list: vi.fn(async (filter) => ({
      rows: rows.filter((row) => !filter.filter?.status || filter.filter.status.includes(row.status)),
      cursor: "p1",
    })),
    changes: vi.fn(async () => ({ rows: [], next_cursor: "p2", complete: true })),
  };
  const location: Feed<LocationRow, Record<string, never>> = {
    list: vi.fn(async () => ({ rows: locations, cursor: "l1" })),
    changes: vi.fn(async () => ({ rows: [], next_cursor: "l2", complete: true })),
  };
  const saved = new Map<string, { rows: unknown[]; cursor: string; saved_at: string }>();
  const persistence: Persistence = {
    load: vi.fn(async (key) => saved.get(key)),
    save: vi.fn(async (key, rows, cursor) => {
      saved.set(key, { rows, cursor, saved_at: new Date().toISOString() });
    }),
    remove: vi.fn(async (key) => {
      saved.delete(key);
    }),
  };
  const estimate = vi.fn(async () => ({ quota: 100_000_000, usage: 0 }));
  let online = true;
  function open(filter: PackagingFilter = {}) {
    const model = packagingOffline({
      packaging,
      location,
      filter,
      persistence,
      estimate,
      online: () => online,
      pollMs: 1_000_000,
    });
    cleanups.push(model.close);
    return model;
  }
  return {
    rows,
    packaging,
    location,
    saved,
    persistence,
    estimate,
    open,
    locations: (value: LocationRow[]) => {
      locations = value;
    },
    offline: () => {
      online = false;
    },
  };
}

it("fills the filter filter and the whole location model, then follows both feeds", async () => {
  const h = fixture();
  const filter = packagingFilter("held");
  expect(filter).toEqual({ filter: { status: ["held"] } });
  expect(packagingFilter("all")).toEqual({});
  const model = h.open(filter);
  await model.packaging.c.preload();
  const result = await model.takeOffline();
  expect(result.packaging.count).toBe(1);
  expect(result.location.count).toBe(1);
  expect(h.packaging.list).toHaveBeenLastCalledWith(filter);
  expect(h.location.list).toHaveBeenCalledExactlyOnceWith({});
  expect(h.saved.get(packagingKey(filter))?.rows).toEqual([h.rows[0]]);
  expect(h.saved.get(locationKey({}))?.rows).toHaveLength(1);
  expect(result.needed).toBe(Math.ceil((result.packaging.bytes + result.location.bytes) * 1.2));
  await model.packaging.c.utils.pull();
  await model.location!.c.utils.pull();
  expect(h.packaging.changes).toHaveBeenCalledOnce();
  expect(h.location.changes).toHaveBeenCalledOnce();
});

it("refuses before filling when free space lacks 20% headroom and reports needed bytes", async () => {
  expect(() => requireStorage(1000, { usage: 1, quota: 1200 })).toThrow(
    "Need 1200 bytes including 20% headroom; 1199 bytes available.",
  );
  expect(requireStorage(1000, { usage: 1, quota: 1201 })).toBe(1200);
  const h = fixture();
  const model = h.open();
  await model.packaging.c.preload();
  h.estimate.mockResolvedValue({ quota: 0, usage: 0 });
  const refill = vi.spyOn(model.packaging.c.utils, "refill");
  await expect(model.takeOffline()).rejects.toThrow(/Need \d+ bytes including 20% headroom; 0 bytes available/);
  expect(h.estimate).toHaveBeenCalledOnce();
  expect(refill).not.toHaveBeenCalled();
  expect(model.location).toBeUndefined();
  expect(h.saved.has(locationKey({}))).toBe(false);
});

it("refills locations through row_version without deleting the saved entry", async () => {
  const h = fixture();
  const model = h.open();
  await model.takeOffline();
  const row = model.location!.c.toArray[0]!;
  const c = model.location!.c;
  h.locations([{ ...row, locationCode: "NEW", rowVersion: "2" }]);
  await model.takeOffline();
  expect(model.location!.c).toBe(c);
  expect(c.toArray[0]?.locationCode).toBe("NEW");
  h.locations([{ ...row, locationCode: "OLD", rowVersion: "1" }]);
  await model.takeOffline();
  expect(c.toArray[0]?.locationCode).toBe("NEW");
  expect(h.persistence.remove).not.toHaveBeenCalled();
});

it.each([false, true])("opens a local collection without network, empty=%s", async (empty) => {
  const h = fixture();
  const filter = empty ? packagingFilter("consumed") : {};
  const before = h.open(filter);
  await before.takeOffline();
  await before.close();
  const savedAt = h.saved.get(packagingKey(filter))!.saved_at;
  vi.mocked(h.packaging.list).mockClear();
  vi.mocked(h.location.list).mockClear();
  vi.mocked(h.persistence.save).mockClear();
  h.offline();
  const model = h.open(filter);
  await model.restored;
  await model.packaging.c.preload();
  await model.location!.c.preload();
  await model.packaging.c.utils.pull();
  await model.location!.c.utils.pull();
  expect(model.packaging.c.size).toBe(empty ? 0 : 3);
  expect(model.packaging.currentAt()).toBe(savedAt);
  expect(needsConnection(false, model.packaging.local())).toBe(false);
  expect(h.packaging.list).not.toHaveBeenCalled();
  expect(h.location.list).not.toHaveBeenCalled();
  expect(h.packaging.changes).not.toHaveBeenCalled();
  expect(h.location.changes).not.toHaveBeenCalled();
  expect(h.persistence.save).not.toHaveBeenCalled();
});

it("requires a connection when the screen has no local collection", async () => {
  const h = fixture();
  h.offline();
  const model = h.open(packagingFilter("held"));
  await model.packaging.c.preload().catch(() => undefined);
  expect(needsConnection(false, model.packaging.local())).toBe(true);
  expect(String(model.packaging.c.utils.error())).toContain("needs a connection");
  expect(h.packaging.list).not.toHaveBeenCalled();
});

it("does not report completion when storage fails", async () => {
  const h = fixture();
  const model = h.open();
  await model.packaging.c.preload();
  vi.mocked(h.persistence.save).mockRejectedValue(new Error("Storage is full"));
  await expect(model.takeOffline()).rejects.toThrow("Storage is full");
});

it("keeps both generated shapes when the rows store reopens", async () => {
  const database = `offline-${crypto.randomUUID()}`;
  const digests = [PACKAGING_SHAPE_DIGEST, LOCATION_SHAPE_DIGEST];
  const persistence = rowsPersistence(database, digests);
  await persistence.save(packagingKey({}), makeRows(1), "p");
  await persistence.save(locationKey({}), [], "l");
  const reopened = rowsPersistence(database, digests);
  expect((await reopened.load(packagingKey({})))?.rows).toHaveLength(1);
  expect((await reopened.load(locationKey({})))?.rows).toEqual([]);
});

it("counts UTF-8 bytes including the entry key, cursor and as-of time", () => {
  const rows = [{ id: "1", locationCode: "é" }];
  const value = { key: "location", rows, cursor: "c", saved_at: "2026-01-01T00:00:00.000Z" };
  expect(collectionBytes(value.key, rows, value.cursor, value.saved_at)).toBe(Buffer.byteLength(JSON.stringify(value)));
});
