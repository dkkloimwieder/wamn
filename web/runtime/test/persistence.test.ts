import "fake-indexeddb/auto";
import { expect, it, vi } from "vitest";
import { rowsDatabase, rowsPersistence } from "../src/sync/index.js";

it("restores rows, the cursor and save time after reopening, and replaces and removes one collection", async () => {
  const database = rowsDatabase("wms", "dev", crypto.randomUUID());
  const first = rowsPersistence(database);
  expect(await first.load("packaging:{}")).toBeUndefined();
  const rows = [{ id: "row-1", rowVersion: 2, label: "Box" }];
  const before = new Date().toISOString();
  await first.save("packaging:{}", rows, "cursor-2");
  rows[0]!.label = "Changed in memory";
  const reopened = rowsPersistence(database);
  const saved = await reopened.load("packaging:{}");
  expect(saved?.rows).toEqual([{ id: "row-1", rowVersion: 2, label: "Box" }]);
  expect(saved?.cursor).toBe("cursor-2");
  expect(saved!.saved_at >= before).toBe(true);
  expect(saved!.saved_at <= new Date().toISOString()).toBe(true);
  await reopened.save("other:{}", [], "other-cursor");
  await reopened.save("packaging:{}", [], "cursor-3");
  expect(await first.load("packaging:{}")).toMatchObject({ rows: [], cursor: "cursor-3" });
  await reopened.remove("packaging:{}");
  expect(await first.load("packaging:{}")).toBeUndefined();
  expect(await first.load("other:{}")).toMatchObject({ rows: [], cursor: "other-cursor" });
});

it("isolates application, environment and user databases", async () => {
  const user = crypto.randomUUID();
  const name = rowsDatabase("wms", "dev", user);
  expect(name).toBe(`wamn-rows:wms:dev:${user}`);
  await rowsPersistence(name).save("packaging:{}", [{ id: "private" }], "cursor");
  for (const database of [rowsDatabase("other", "dev", user), rowsDatabase("wms", "prod", user), rowsDatabase("wms", "dev", "other")]) {
    expect(await rowsPersistence(database).load("packaging:{}")).toBeUndefined();
  }
});

it("rejects a failed transaction and keeps the previous snapshot", async () => {
  const persistence = rowsPersistence(rowsDatabase("wms", "dev", crypto.randomUUID()));
  await persistence.save("packaging:{}", [{ id: "saved" }], "old");
  const put = vi.spyOn(IDBObjectStore.prototype, "put").mockImplementation(function (this: IDBObjectStore) {
    this.transaction.abort();
    return {} as IDBRequest<IDBValidKey>;
  });
  try {
    await expect(persistence.save("packaging:{}", [], "new")).rejects.toThrow("aborted");
  } finally {
    put.mockRestore();
  }
  expect(await persistence.load("packaging:{}")).toMatchObject({ rows: [{ id: "saved" }], cursor: "old" });
});
