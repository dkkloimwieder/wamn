import { describe, expect, it } from "vitest";

import type { ScreenContribution } from "../app/screen-actions";
import type { VisitedEntry } from "../store/visited";
import {
  draftAddress,
  flatCommands,
  paletteGroups,
  relativeTime,
  type PaletteCommand,
  type PaletteGroup,
} from "./commands";

/**
 * The ranking, over plain data. Everything §6 step 7 promises about *order* is
 * decided here, so it is proved here — without a render, and without the
 * overlay's keys standing between the assertion and the answer.
 */

const NOW = Date.UTC(2026, 7, 15, 12, 0, 0);
const MINUTE = 60_000;

const olderRun: VisitedEntry = {
  kind: "run",
  id: "01J9X4T7QW3B8ZC5NDR6MK3F2K",
  revision: null,
  label: "01J9X4T7QW3B8ZC5NDR6MK3F2K",
  verdict: "fail",
  visitedAt: NOW - 120 * MINUTE,
};

const newerReport: VisitedEntry = {
  kind: "report",
  id: "01J9T5C8NB2XQ7ZD4RM9KV8Q11",
  revision: null,
  label: "01J9T5C8NB2XQ7ZD4RM9KV8Q11",
  verdict: "ok",
  visitedAt: NOW - 2 * MINUTE,
};

const draft: VisitedEntry = {
  kind: "draft",
  id: "orders",
  revision: 17,
  label: "orders@17",
  verdict: "none",
  visitedAt: NOW - 30 * MINUTE,
};

/** Most recent first, which is the order the store hands them over in. */
const visited: readonly VisitedEntry[] = [newerReport, draft, olderRun];

const noScreen: ScreenContribution = { anchors: [], actions: [] };

function groups(over: {
  query?: string;
  visited?: readonly VisitedEntry[];
  contribution?: ScreenContribution;
}): readonly PaletteGroup[] {
  return paletteGroups({
    query: over.query ?? "",
    visited: over.visited ?? visited,
    contribution: over.contribution ?? noScreen,
    now: NOW,
  });
}

function texts(commands: readonly PaletteCommand[]): string[] {
  return commands.map((command) => command.text);
}

function group(all: readonly PaletteGroup[], name: string): PaletteGroup {
  const found = all.find((candidate) => candidate.name === name);
  if (found === undefined) {
    throw new Error(`no ${name} group`);
  }
  return found;
}

describe("the three groups", () => {
  it("orders them go to · this screen · console, and numbers every row once", () => {
    const all = groups({});
    expect(all.map((each) => each.name)).toEqual(["go to", "this screen", "console"]);

    const flat = flatCommands(all);
    expect(flat.map((command) => command.index)).toEqual(flat.map((_, index) => index));
    // every row is reachable by its own place in the list, which is what ↑↓ walk
    expect(new Set(flat.map((command) => command.id)).size).toBe(flat.length);
  });

  it("puts the most recently visited entity first, so ⌘K ↵ reopens it", () => {
    const first = flatCommands(groups({}))[0];
    expect(first.group).toBe("go to");
    expect(first.text).toBe(`open report ${newerReport.id}`);
    expect(first.action).toEqual({
      kind: "navigate",
      route: { kind: "report", id: newerReport.id },
    });
  });

  it("keeps the store's recency order and says when each was seen", () => {
    const goTo = group(groups({}), "go to");
    expect(texts(goTo.commands)).toEqual([
      `open report ${newerReport.id}`,
      "open draft orders@17",
      `open run ${olderRun.id}`,
    ]);
    expect(goTo.commands[0].detail).toBe("ok · seen 2m ago");
    expect(goTo.commands[2].detail).toBe("failed · seen 2h ago");
  });

  it("names a draft's flow id, which its address does not carry", () => {
    const draftRow = group(groups({}), "go to").commands[1];
    // the address is the draft id and its revision; `orders@17` here is the
    // label an author recognizes, and it is said rather than dropped
    expect(draftRow.detail).toContain("a draft has no verdict");
    expect(draftRow.action).toEqual({
      kind: "navigate",
      route: { kind: "draft", id: "orders", revision: "17" },
    });
  });
});

describe("typed ids", () => {
  it("ranks the kinds by which one this browser was last reading", () => {
    // a report was the last thing visited, so a bare id most likely names one
    const goTo = group(groups({ query: "01J9NEW" }), "go to");
    expect(texts(goTo.commands)).toEqual(["open report 01J9NEW", "open run 01J9NEW"]);

    // with a run in front the ranking follows
    const runFirst = group(groups({ query: "01J9NEW", visited: [olderRun] }), "go to");
    expect(texts(runFirst.commands)).toEqual(["open run 01J9NEW", "open report 01J9NEW"]);

    // nothing visited at all: the declared order, and nothing inferred from it
    const nothing = group(groups({ query: "01J9NEW", visited: [] }), "go to");
    expect(texts(nothing.commands)).toEqual(["open run 01J9NEW", "open report 01J9NEW"]);
  });

  it("offers no draft for a bare id, and says why instead of leaving a blank", () => {
    const goTo = group(groups({ query: "orders" }), "go to");
    expect(texts(goTo.commands)).not.toContain("open draft orders");
    expect(goTo.note).toBe(
      "a draft is addressed by an id and a revision, so a bare id cannot name one — type id@revision",
    );
  });

  it("names the rule the typed address actually broke, not a different one", () => {
    // `orders@017` *is* id@revision, so telling this author that a bare id
    // cannot name a draft is instruction they have already followed: they would
    // retype the same string and be refused again, with the rule that rejected
    // it never said anywhere on the screen
    const leadingZero = group(groups({ query: "orders@017" }), "go to").note;
    expect(leadingZero).toBe(
      "017 does not name a revision — a revision is a plain whole number, written with no sign, no leading zero and no exponent, and small enough to count exactly",
    );
    expect(leadingZero).not.toContain("bare id");

    // every other spelling `parseRevision` refuses reaches that same rule
    for (const query of ["charges@1e3", "orders@-1", "orders@3x", "orders@0x11"]) {
      expect(group(groups({ query }), "go to").note).toContain("does not name a revision");
    }

    // and a half-typed address says which half is missing
    expect(group(groups({ query: "orders@" }), "go to").note).toBe(
      "a draft is addressed by an id and a revision, and there is nothing after the @ to be a revision — type id@revision",
    );
    expect(group(groups({ query: "@17" }), "go to").note).toBe(
      "a draft is addressed by an id and a revision, and there is nothing before the @ to be an id — type id@revision",
    );
  });

  it("addresses a draft from id@revision, and never invents the revision", () => {
    expect(draftAddress("orders")).toBeNull();
    expect(draftAddress("orders@")).toBeNull();
    expect(draftAddress("@17")).toBeNull();
    // parseRevision's canonical form: "017" and "1e3" name no revision
    expect(draftAddress("orders@017")).toBeNull();
    // revision 0 is a revision, and it is the one a truthiness check loses
    expect(draftAddress("orders@0")).toEqual({ id: "orders", revision: "0" });

    const goTo = group(groups({ query: "charges@3" }), "go to");
    // the @ form can only mean a draft, so it leads
    expect(texts(goTo.commands)[0]).toBe("open draft charges@3");
    expect(goTo.commands[0].action).toEqual({
      kind: "navigate",
      route: { kind: "draft", id: "charges", revision: "3" },
    });
    expect(goTo.note).toBeNull();
  });

  it("does not restate a recent as a typed candidate", () => {
    const goTo = group(groups({ query: olderRun.id }), "go to");
    // one row for one route, and it is the one that also says what happened
    expect(texts(goTo.commands)).toEqual([
      `open run ${olderRun.id}`,
      `open report ${olderRun.id}`,
    ]);
    expect(goTo.commands[0].detail).toBe("failed · seen 2h ago");
  });

  it("offers nothing for text a route could not carry", () => {
    const goTo = group(groups({ query: "run 3" }), "go to");
    expect(goTo.commands).toEqual([]);
    expect(goTo.note).toBe("nothing visited matches, and this is not an id a route can carry");
  });
});

describe("filtering", () => {
  it("keeps only the rows the query names, across every group", () => {
    const contribution: ScreenContribution = {
      anchors: [{ id: "run-execution", label: "execution" }],
      actions: [{ id: "expand-all", label: "expand all execution rows", run: () => undefined }],
    };
    const all = groups({ query: "execution", contribution });

    expect(texts(group(all, "this screen").commands)).toEqual([
      "go to execution",
      "expand all execution rows",
    ]);
    expect(group(all, "console").commands).toEqual([]);
    expect(group(all, "console").note).toBe("nothing in the console matches");
  });

  it("matches a recent on its kind, its label and its address", () => {
    // ids are opaque (routing/route.ts), so `draft` is also text a run could be
    // addressed by — the matching recent leads, and the candidates follow it
    expect(texts(group(groups({ query: "draft" }), "go to").commands)).toEqual([
      "open draft orders@17",
      "open report draft",
      "open run draft",
    ]);
    expect(texts(group(groups({ query: "MK3F2K" }), "go to").commands)[0]).toBe(
      `open run ${olderRun.id}`,
    );
  });
});

describe("what each group says when it has nothing", () => {
  it("says nothing is visited rather than drawing an empty list", () => {
    const goTo = group(groups({ visited: [] }), "go to");
    expect(goTo.commands).toEqual([]);
    expect(goTo.note).toBe("nothing visited yet");
  });

  it("says the mounted screen offered nothing", () => {
    const screen = group(groups({}), "this screen");
    expect(screen.commands).toEqual([]);
    expect(screen.note).toBe("the screen you are on offers the palette no sections and no actions");
  });
});

describe("the console group", () => {
  it("carries start and clear visited, and neither a panel toggle nor a refresh", () => {
    const console = group(groups({}), "console");
    expect(texts(console.commands)).toEqual(["go to start", "clear visited"]);
    // the side panel is step 6 and does not exist; a row for it would name an
    // act the console cannot perform
    expect(texts(console.commands).join(" ")).not.toContain("panel");
    // the screen owns its one read, and contributes `refresh this screen` itself
    expect(texts(console.commands).join(" ")).not.toContain("refresh");
  });

  it("surfaces the screen's own refresh, so it is still reachable by keyboard", () => {
    let refreshed = 0;
    const contribution: ScreenContribution = {
      anchors: [],
      actions: [{ id: "refresh", label: "refresh this screen", run: () => (refreshed += 1) }],
    };
    const row = group(groups({ contribution }), "this screen").commands[0];
    expect(row.text).toBe("refresh this screen");
    if (row.action.kind !== "screen") {
      throw new Error("a screen action must carry the screen's own callback");
    }
    row.action.run();
    expect(refreshed).toBe(1);
  });
});

describe("relative time", () => {
  it("reads in the largest unit that still says something", () => {
    expect(relativeTime(NOW, NOW)).toBe("0s ago");
    expect(relativeTime(NOW - 45_000, NOW)).toBe("45s ago");
    expect(relativeTime(NOW - 2 * MINUTE, NOW)).toBe("2m ago");
    expect(relativeTime(NOW - 150 * MINUTE, NOW)).toBe("2h ago");
    expect(relativeTime(NOW - 26 * 60 * MINUTE, NOW)).toBe("1d ago");
    // a clock that moved backwards reports no visit from the future
    expect(relativeTime(NOW + MINUTE, NOW)).toBe("0s ago");
  });
});
