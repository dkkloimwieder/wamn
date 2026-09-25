/**
 * The load state of one table (wamn-xtz2.2).
 */

import { describe, expect, it } from "vitest";

import { emptyLoad, finishLoad, loadLimit, startLoad, type LoadPage } from "../src/load.js";
import { refusalSentence } from "../src/refusal.js";
import type { Outcome } from "../src/wire.js";

interface Row {
  readonly id: string;
}

const START = new Date(0);
const END = new Date(1000);

const rows = (...ids: string[]): Row[] => ids.map((id) => ({ id }));

const page = (item: Row[], nextCursor: string | null): Outcome<LoadPage<Row>> => ({
  status: "completed",
  value: { item, nextCursor },
});

describe("the load state", () => {
  it("reads one page of the cap, or of the page maximum when that is lower", () => {
    expect(loadLimit(1000, 100)).toBe(100);
    expect(loadLimit(50, 100)).toBe(50);
    expect(loadLimit(100, 100)).toBe(100);
  });

  it("is fully read when no cursor follows the page, and records when the load ran", () => {
    const started = startLoad(emptyLoad<Row>(1000), undefined, START);
    expect(started).toMatchObject({ busy: true, fullyRead: false, startedAt: START, endedAt: null });
    const done = finishLoad(started, started.generation, page(rows("a", "b"), null), "id", END);
    expect(done).toMatchObject({
      rows: rows("a", "b"),
      fullyRead: true,
      busy: false,
      refusal: null,
      startedAt: START,
      endedAt: END,
    });
  });

  it("is not fully read when a cursor is left over", () => {
    const started = startLoad(emptyLoad<Row>(2));
    const done = finishLoad(started, started.generation, page(rows("a", "b"), "c1"), "id");
    expect(done.rows).toEqual(rows("a", "b"));
    expect(done.fullyRead).toBe(false);
  });

  it("fails the load on a duplicate row id, with a sentence that names it", () => {
    const started = startLoad(emptyLoad<Row>(1000));
    const done = finishLoad(started, started.generation, page(rows("a", "b", "a"), null), "id");
    expect(done.rows).toEqual([]);
    expect(done.fullyRead).toBe(false);
    expect(done.busy).toBe(false);
    expect(done.refusal).toContain("a");
    expect(done.refusal).toMatch(/twice/);
  });

  it("starts a new generation on a cap change, and keeps the cap otherwise", () => {
    const state = emptyLoad<Row>(1000);
    const capped = startLoad(state, 50);
    expect(capped.cap).toBe(50);
    expect(capped.generation).toBe(state.generation + 1);
    // A scope change or a refresh starts a load with the cap in force.
    const again = startLoad(capped);
    expect(again.cap).toBe(50);
    expect(again.generation).toBe(capped.generation + 1);
  });

  it("drops the result of an older generation", () => {
    const first = startLoad(emptyLoad<Row>(1000));
    const second = startLoad(first);
    const late = finishLoad(second, first.generation, page(rows("old"), null), "id");
    expect(late).toBe(second);
    const done = finishLoad(late, second.generation, page(rows("new"), null), "id");
    expect(done.rows).toEqual(rows("new"));
  });

  it("reads a refusal as the forms read one, and keeps no rows", () => {
    const loaded = finishLoad(
      startLoad(emptyLoad<Row>(1000)),
      1,
      page(rows("a"), null),
      "id",
    );
    const started = startLoad(loaded);
    const refused = finishLoad(
      started,
      started.generation,
      { status: "refused", code: "permission_denied", detail: null },
      "id",
    );
    expect(refused).toMatchObject({
      rows: [],
      fullyRead: false,
      busy: false,
      refusal: refusalSentence("permission_denied"),
    });
    const uncertain = finishLoad(
      started,
      started.generation,
      { status: "uncertain", reason: "the connection closed", retryRefusal: null },
      "id",
    );
    expect(uncertain.refusal).toBe("the connection closed");
  });
});
