import "fake-indexeddb/auto";
import { expect, it, vi } from "vitest";
import { outbox } from "@wfe/sync";
import { collectionKey, outboxDatabase, rowsDatabase, rowsPersistence } from "../src/sync/index.js";

it("drops obsolete model shapes, keeps unchanged rows and leaves the outbox intact", async () => {
  const user = crypto.randomUUID();
  const database = rowsDatabase("wms", "dev", user);
  const oldKey = collectionKey("packaging", "shape-old", "{}");
  const newKey = collectionKey("packaging", "shape-new", "{}");
  const unchanged = collectionKey("location", "shape-location", "{}");
  const first = rowsPersistence(database, ["shape-old", "shape-location"]);
  await first.save(oldKey, [{ id: "old" }], "old-cursor");
  await first.save(unchanged, [{ id: "location" }], "location-cursor");
  const options = {
    database: outboxDatabase("wms", "dev", user),
    send: async () => ({ ok: false as const, retriable: true }),
    probe: async () => false,
  };
  const before = outbox(options);
  const entry = await before.enqueue({
    operation: "packaging.create", request: { label: "old shape" },
    request_hash: "hash", expect: { model: "packaging", id: "pending" }, append_only: true,
  });
  before.close();
  const next = rowsPersistence(database, ["shape-new", "shape-location"]);
  expect(await next.load(newKey)).toBeUndefined();
  expect(await next.load(oldKey)).toBeUndefined();
  expect(await next.load(unchanged)).toMatchObject({ rows: [{ id: "location" }], cursor: "location-cursor" });
  const after = outbox(options);
  try {
    await vi.waitFor(() => expect(after.entries()).toHaveLength(1));
    expect(after.entries()[0]).toMatchObject({ ...entry, state: "pending" });
  } finally {
    after.close();
  }
});

it("restores rows, the cursor and save time after reopening, and replaces and removes one collection", async () => {
  const database = rowsDatabase("wms", "dev", crypto.randomUUID());
  const first = rowsPersistence(database, ["shape-a"]);
  expect(await first.load(collectionKey("packaging", "shape-a", "{}"))).toBeUndefined();
  const rows = [{ id: "row-1", rowVersion: 2, label: "Box" }];
  const before = new Date().toISOString();
  await first.save(collectionKey("packaging", "shape-a", "{}"), rows, "cursor-2");
  rows[0]!.label = "Changed in memory";
  const reopened = rowsPersistence(database, ["shape-a"]);
  const saved = await reopened.load(collectionKey("packaging", "shape-a", "{}"));
  expect(saved?.rows).toEqual([{ id: "row-1", rowVersion: 2, label: "Box" }]);
  expect(saved?.cursor).toBe("cursor-2");
  expect(saved!.saved_at >= before).toBe(true);
  expect(saved!.saved_at <= new Date().toISOString()).toBe(true);
  await reopened.save(collectionKey("other", "shape-a", "{}"), [], "other-cursor");
  await reopened.save(collectionKey("packaging", "shape-a", "{}"), [], "cursor-3");
  expect(await first.load(collectionKey("packaging", "shape-a", "{}"))).toMatchObject({ rows: [], cursor: "cursor-3" });
  await reopened.remove(collectionKey("packaging", "shape-a", "{}"));
  expect(await first.load(collectionKey("packaging", "shape-a", "{}"))).toBeUndefined();
  expect(await first.load(collectionKey("other", "shape-a", "{}"))).toMatchObject({ rows: [], cursor: "other-cursor" });
});

it("isolates application, environment and user databases", async () => {
  const user = crypto.randomUUID();
  const name = rowsDatabase("wms", "dev", user);
  expect(name).toBe(`wamn-rows:wms:dev:${user}`);
  await rowsPersistence(name, ["shape-a"]).save(collectionKey("packaging", "shape-a", "{}"), [{ id: "private" }], "cursor");
  for (const database of [rowsDatabase("other", "dev", user), rowsDatabase("wms", "prod", user), rowsDatabase("wms", "dev", "other")]) {
    expect(await rowsPersistence(database, ["shape-a"]).load(collectionKey("packaging", "shape-a", "{}"))).toBeUndefined();
  }
});

it("rejects a failed transaction and keeps the previous snapshot", async () => {
  const persistence = rowsPersistence(rowsDatabase("wms", "dev", crypto.randomUUID()), ["shape-a"]);
  await persistence.save(collectionKey("packaging", "shape-a", "{}"), [{ id: "saved" }], "old");
  const put = vi.spyOn(IDBObjectStore.prototype, "put").mockImplementation(function (this: IDBObjectStore) {
    this.transaction.abort();
    return {} as IDBRequest<IDBValidKey>;
  });
  try {
    await expect(persistence.save(collectionKey("packaging", "shape-a", "{}"), [], "new")).rejects.toThrow("aborted");
  } finally {
    put.mockRestore();
  }
  expect(await persistence.load(collectionKey("packaging", "shape-a", "{}"))).toMatchObject({ rows: [{ id: "saved" }], cursor: "old" });
});
