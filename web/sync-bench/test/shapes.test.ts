import { describe, expect, it } from "vitest";
import { createRoot, createSignal, flush } from "solid-js";
import { applyChanges } from "@wfe/sync/internal";
import { fakeFeed, makeRows, randomChanges, seeded } from "../src/fake";
import { packagingCollection, type PackagingRow } from "../src/packaging-feed";
import type { StatusFilter } from "../src/order";
import { shapeA } from "../src/shape-a";
import { shapeB } from "../src/shape-b";
import { exactSum } from "../src/totals";
import { shown } from "../src/view";

const settle = async () => {
  for (let step = 0; step < 5; step++) {
    await new Promise((resolve) => setTimeout(resolve, 0));
    flush();
  }
};

/** What the screen shows: rows in order, groups and totals, with totals as displayed. */
function screen(source: ReturnType<typeof shapeA> | ReturnType<typeof shapeB>) {
  return {
    ids: [...source.ids()],
    groups: source.groups().map(({ status, type, count, total }) => ({ status, type, count, total: shown(total) })),
    totals: { count: source.totals().count, total: shown(source.totals().total) },
  };
}

describe("shapes A and B", () => {
  it("show the same rows, groups and totals after fill and after change batches", async () => {
    const rows = makeRows(1_000, 7);
    const [filter, setFilter] = createSignal<StatusFilter>("all");
    const { a, b, c, dispose } = createRoot((dispose) => {
      const c = packagingCollection(
        fakeFeed(() => rows),
        {},
      );
      return {
        a: shapeA(c, filter),
        b: shapeB(
          fakeFeed(() => rows),
          {},
          filter,
        ),
        c,
        dispose,
      };
    });
    await settle();
    expect(a.status()).toBe("ready");
    expect(b.status()).toBe("ready");
    expect(screen(a)).toEqual(screen(b));
    expect(a.totals().count).toBe(1_000);
    expect(shown(b.totals().total)).toBe(exactSum(rows.map((row) => row.rowVersion)));

    const random = seeded(3);
    const current = new Map(rows.map((row) => [row.id, row]));
    for (let batch = 0; batch < 3; batch++) {
      const changes = randomChanges([...current.values()], random, 1_000 + batch * 20);
      for (const change of changes)
        if (change.deleted) current.delete(change.id);
        else current.set(change.id, change.data);
      applyChanges(c, changes);
      b.apply(changes);
      await settle();
      expect(screen(a)).toEqual(screen(b));
    }
    expect(a.totals().count).toBe(1_030);

    setFilter("held");
    await settle();
    expect(a.ids()).toEqual(b.ids());
    expect(a.ids().every((id) => (a.get(id) as PackagingRow).status === "held")).toBe(true);
    dispose();
  });

  it("ignores a stale change in shape B, as applyChanges does", async () => {
    const rows = makeRows(3, 1);
    const [filter] = createSignal<StatusFilter>("all");
    const { b, dispose } = createRoot((dispose) => ({
      b: shapeB(
        fakeFeed(() => rows),
        {},
        filter,
      ),
      dispose,
    }));
    await settle();
    const row = rows[0]!;
    b.apply([{ id: row.id, row_version: 3, deleted: false, data: { ...row, row_version: 3, rowVersion: 3 } }]);
    b.apply([{ id: row.id, row_version: 2, deleted: false, data: { ...row, row_version: 2, rowVersion: 2 } }]);
    await settle();
    expect(b.get(row.id)?.row_version).toBe(3);
    b.apply([{ id: "missing", row_version: 1, deleted: true }]);
    await settle();
    expect(b.totals().count).toBe(3);
    dispose();
  });
});
