import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  allPassReport,
  finalizedReport,
  parsingDraft,
  passingRun,
  pendingReport,
  terminalizedRun,
} from "../reader/fixtures";
import { parseHash, toHash } from "../routing/route";
import {
  VISITED_CAP_PER_KIND,
  clearVisited,
  decodeVisited,
  encodeVisited,
  recordVisit,
  reportVerdictState,
  routeOfVisit,
  runVerdictState,
  visitFromDraft,
  visitFromReport,
  visitFromRun,
  visitedEntries,
  visitedOfKind,
  visitedStorageKey,
  type VisitedEntry,
} from "./visited";

beforeEach(() => {
  clearVisited();
});

const run = (id: string, at: number): VisitedEntry => ({
  kind: "run",
  id,
  revision: null,
  label: id,
  verdict: "ok",
  visitedAt: at,
});

describe("the visited store", () => {
  it("keeps the most recently visited entity first", () => {
    recordVisit(run("first", 1));
    recordVisit(run("second", 2));

    expect(visitedEntries().map((entry) => entry.id)).toEqual(["second", "first"]);
  });

  it("replaces an entity on re-visit rather than listing it twice", () => {
    recordVisit(run("a", 1));
    recordVisit(run("b", 2));
    // the same run, seen again, and failing this time
    recordVisit({ ...run("a", 3), verdict: "fail" });

    expect(visitedEntries()).toHaveLength(2);
    expect(visitedEntries()[0]).toMatchObject({ id: "a", verdict: "fail", visitedAt: 3 });
    // §2.0's cached display text is refreshed by the visit, not left as it was
    expect(visitedEntries().map((entry) => entry.id)).toEqual(["a", "b"]);
  });

  it("distinguishes two revisions of one draft", () => {
    recordVisit(visitFromDraft(parsingDraft, 1));
    recordVisit({ ...visitFromDraft(parsingDraft, 2), revision: 16, label: "orders@16" });

    expect(visitedOfKind("draft").map((entry) => entry.label)).toEqual([
      "orders@16",
      "orders@17",
    ]);
  });

  it("caps each kind on its own, so a run of runs cannot evict a draft", () => {
    recordVisit(visitFromDraft(parsingDraft, 0));
    for (let index = 1; index <= VISITED_CAP_PER_KIND + 5; index += 1) {
      recordVisit(run(`run-${index}`, index));
    }

    expect(visitedOfKind("run")).toHaveLength(VISITED_CAP_PER_KIND);
    // the oldest runs went, the draft stayed
    expect(visitedOfKind("run").map((entry) => entry.id)).not.toContain("run-1");
    expect(visitedOfKind("draft")).toHaveLength(1);
  });

  it("survives a reload, because the record reached storage", () => {
    recordVisit(run("kept", 7));

    // exactly what a fresh page load reads back
    const reloaded = decodeVisited(window.localStorage.getItem(visitedStorageKey));
    expect(reloaded).toEqual(visitedEntries());
  });

  it("clears both the visible store and the stored one", () => {
    recordVisit(run("gone", 1));
    clearVisited();

    expect(visitedEntries()).toEqual([]);
    expect(window.localStorage.getItem(visitedStorageKey)).toBeNull();
  });

  it("still records when storage refuses the write", () => {
    const setItem = vi.spyOn(window.localStorage, "setItem").mockImplementation(() => {
      throw new Error("QuotaExceededError");
    });

    // A browser that refuses storage loses the history across reloads, and
    // nothing else: the read the author came for still renders.
    expect(() => recordVisit(run("volatile", 1))).not.toThrow();
    expect(visitedEntries().map((entry) => entry.id)).toEqual(["volatile"]);
    setItem.mockRestore();
  });
});

describe("decoding a stored store", () => {
  it("reads back what it wrote", () => {
    const entries = [run("a", 1), visitFromDraft(parsingDraft, 2)];
    expect(decodeVisited(encodeVisited(entries))).toEqual(entries);
  });

  it("treats an absent, unparsable, or non-array value as no history", () => {
    expect(decodeVisited(null)).toEqual([]);
    expect(decodeVisited("{not json")).toEqual([]);
    expect(decodeVisited('{"kind":"run"}')).toEqual([]);
  });

  it("drops the entries it cannot use and keeps the rest", () => {
    const good = run("good", 1);
    const raw = JSON.stringify([
      good,
      { ...good, kind: "flow" }, // a kind this build has no screen for
      { ...good, verdict: "maybe" }, // a glyph state that does not exist
      { ...good, id: "" }, // addresses nothing
      { ...good, visitedAt: "recently" }, // not a time
    ]);

    // One bad entry costs the reader that entry, never the whole history.
    expect(decodeVisited(raw)).toEqual([good]);
  });

  it("refuses a draft with no revision and a run that carries one", () => {
    const draft = visitFromDraft(parsingDraft, 1);
    const raw = JSON.stringify([
      { ...draft, revision: null }, // a draft route needs both segments
      { ...run("r", 1), revision: 3 }, // a run route has no revision to carry
    ]);

    expect(decodeVisited(raw)).toEqual([]);
  });
});

describe("what a visit records", () => {
  it("addresses the route the entry came from", () => {
    const draft = routeOfVisit(visitFromDraft(parsingDraft, 1));
    // round-trips: the store's entry and the router agree on one address
    expect(parseHash(toHash(draft))).toEqual({ kind: "draft", id: "orders", revision: "17" });

    const runRoute = routeOfVisit(visitFromRun(passingRun, 1));
    expect(parseHash(toHash(runRoute))).toEqual({ kind: "run", id: passingRun.runId });
  });

  it("keeps a terminalized run showing as uncertain, not as an ordinary failure", () => {
    // status `failed`, kind still `effect-uncertain` — §2.0's one purple state
    expect(terminalizedRun.status).toBe("failed");
    expect(runVerdictState(terminalizedRun)).toBe("uncertain");
    expect(runVerdictState(passingRun)).toBe("ok");
  });

  it("gives a report that has not finalized no verdict rather than a failing one", () => {
    expect(reportVerdictState(pendingReport)).toBe("none");
    expect(reportVerdictState(allPassReport)).toBe("ok");
    expect(reportVerdictState(finalizedReport)).toBe("fail");
  });

  it("gives a draft no verdict, whether or not it parses", () => {
    // §2.4: a half-finished draft is a normal object, not a failure
    expect(visitFromDraft(parsingDraft, 1).verdict).toBe("none");
    expect(visitFromDraft(parsingDraft, 1).label).toBe("orders@17");
  });
});
