import { describe, expect, it } from "vitest";

import type { Route } from "../routing/route";
import type { VisitedEntry } from "../store/visited";
import {
  buildTree,
  parentKeys,
  sectionInView,
  visibleRows,
  type TreeInput,
  type TreeNode,
} from "./panel-model";

/** A ULID-shaped id, so §2.6's middle-ellipsis has something to bite on. */
const RUN_ID = "01J9QK3ZC0W7HG4M2NPX8V3F2K";

const run: VisitedEntry = {
  kind: "run",
  id: RUN_ID,
  revision: null,
  label: RUN_ID,
  verdict: "fail",
  visitedAt: 1_000_000,
};

const draft: VisitedEntry = {
  kind: "draft",
  id: "drf-0001",
  revision: 17,
  label: "orders@17",
  verdict: "none",
  visitedAt: 900_000,
};

function input(overrides: Partial<TreeInput> = {}): TreeInput {
  return {
    route: { kind: "start" },
    visited: [],
    anchors: [],
    closedKinds: [],
    collapsed: false,
    now: 1_060_000,
    ...overrides,
  };
}

function find(nodes: readonly TreeNode[], key: string): TreeNode {
  const row = visibleRows(nodes).find((node) => node.key === key);
  if (row === undefined) {
    throw new Error(`no row ${key}`);
  }
  return row;
}

describe("§2.0's three levels", () => {
  it("always stands start and the three kinds, and teaches the mechanic when they are empty", () => {
    const tree = buildTree(input());

    expect(tree.map((node) => node.label)).toEqual(["start", "RUNS", "REPORTS", "DRAFTS"]);
    // §2.0: "the tree shows the three kind labels with `nothing visited yet`
    // beneath" — and an empty kind is a leaf, not a disclosure that opens onto
    // nothing.
    expect(tree.slice(1).map((node) => node.emptyNote)).toEqual([
      "nothing visited yet",
      "nothing visited yet",
      "nothing visited yet",
    ]);
    expect(tree.slice(1).map((node) => node.expanded)).toEqual([null, null, null]);
  });

  it("draws a visited entity with its glyph, its ellipsized id and its relative time", () => {
    const tree = buildTree(input({ visited: [run, draft] }));
    const entity = find(tree, `run:${RUN_ID}@`);

    expect(entity.level).toBe(2);
    expect(entity.verdict).toBe("fail");
    // §2.6's middle-ellipsis, first and last four
    expect(entity.label).toBe("01J9…3F2K");
    expect(entity.full).toBe(RUN_ID);
    // §2.0's relative time, computed against the clock and never cached
    expect(entity.detail).toBe("1m ago");

    // §6 step 6's "grouped under flow name for drafts" is the store's own
    // label; a draft is not a fourth level and its name is not an id, so it is
    // never middle-ellipsized
    expect(find(tree, "draft:drf-0001@17").label).toBe("orders@17");
  });

  it("hangs the screen's sections under the active entity, and under no other", () => {
    const route: Route = { kind: "run", id: RUN_ID };
    const other: VisitedEntry = { ...run, id: "01J9OTHERRUNXXXXXXXXXXXXXX", label: "01J9OTHERRUNXXXXXXXXXXXXXX" };
    const tree = buildTree(
      input({
        route,
        visited: [run, other],
        anchors: [
          { id: "run-failure", label: "failure" },
          { id: "run-execution", label: "execution" },
        ],
      }),
    );

    const active = find(tree, `run:${RUN_ID}@`);
    expect(active.active).toBe(true);
    expect(active.children.map((node) => node.label)).toEqual(["failure", "execution"]);
    expect(active.children.map((node) => node.level)).toEqual([3, 3]);
    // "shown expanded only for the active entity"
    expect(find(tree, "run:01J9OTHERRUNXXXXXXXXXXXXXX@").children).toEqual([]);
  });

  it("pins the entity the route is on even when no visit was ever stored", () => {
    // A refused read records nothing, but the route is still where the reader
    // is — §2.0 pins it in place rather than answering "you are nowhere".
    const tree = buildTree(input({ route: { kind: "run", id: RUN_ID }, visited: [] }));
    const pinned = find(tree, `run:${RUN_ID}@`);

    expect(pinned.active).toBe(true);
    expect(pinned.verdict).toBe("none");
    // and it says why it carries no time, rather than leaving the blank to be read
    expect(pinned.detail).toBe("not recorded");
    expect(pinned.hint).toContain("recorded no visit");
    expect(find(tree, "kind:run").emptyNote).toBeNull();
  });

  it("dims a section the screen says cannot be reached, with the screen's own reason", () => {
    // §2.0's `structure`, dimmed with a tooltip when the draft doesn't parse.
    const tree = buildTree(
      input({
        route: { kind: "draft", id: "drf-0001", revision: "17" },
        visited: [draft],
        anchors: [
          { id: "draft-source", label: "source" },
          { id: "draft-structure", label: "structure", unavailable: "does not parse — line 41" },
        ],
      }),
    );
    const sections = find(tree, "draft:drf-0001@17").children;

    expect(sections[0].unavailable).toBeNull();
    expect(sections[1].unavailable).toBe("does not parse — line 41");
    expect(sections[1].hint).toBe("does not parse — line 41");
  });
});

describe("what a reader can reach", () => {
  it("keeps a closed kind's entities out of the walk without forgetting them", () => {
    const tree = buildTree(input({ visited: [run], closedKinds: ["run"] }));

    expect(find(tree, "kind:run").expanded).toBe(false);
    expect(find(tree, "kind:run").children).toHaveLength(1);
    expect(visibleRows(tree).some((row) => row.key === `run:${RUN_ID}@`)).toBe(false);
  });

  it("names each row's parent, which is where ← goes from a row it cannot collapse", () => {
    const tree = buildTree(
      input({
        route: { kind: "run", id: RUN_ID },
        visited: [run],
        anchors: [{ id: "run-execution", label: "execution" }],
      }),
    );
    const parents = parentKeys(tree);

    expect(parents.get(`run:${RUN_ID}@`)).toBe("kind:run");
    expect(parents.get(`run:${RUN_ID}@/run-execution`)).toBe(`run:${RUN_ID}@`);
    expect(parents.get("kind:run")).toBeUndefined();
  });
});

describe("§2.0's 44px rail", () => {
  it("spends its width on the kind initials and the active entity's glyph only", () => {
    const tree = buildTree(
      input({
        route: { kind: "run", id: RUN_ID },
        visited: [run, draft],
        anchors: [{ id: "run-execution", label: "execution" }],
        collapsed: true,
      }),
    );

    // three-letter initials, because RUNS and REPORTS share a first letter and
    // a rail of two identical marks would be worse than no rail
    expect(tree.map((node) => node.label)).toEqual(["◈", "RUN", "RPT", "DFT"]);
    // the words are still what a reader hears
    expect(tree.map((node) => node.full)).toEqual(["start", "runs", "reports", "drafts"]);

    const active = find(tree, `run:${RUN_ID}@`);
    expect(active.label).toBe("");
    expect(active.verdict).toBe("fail");
    expect(active.detail).toBeNull();
    // no third level and no empty notes: neither fits, and a truncated sentence
    // teaches nothing
    expect(active.children).toEqual([]);
    expect(find(tree, "kind:draft").children).toEqual([]);
    expect(find(tree, "kind:draft").emptyNote).toBeNull();
  });
});

describe("which section is in view", () => {
  const order = ["run-failure", "run-execution", "run-output"];

  it("marks the topmost section touching the viewport", () => {
    expect(sectionInView(order, ["run-output", "run-execution"], null)).toBe("run-execution");
  });

  it("holds the previous mark while nothing intersects, and drops it once it is gone", () => {
    // mid-scroll between two sections is not "in none of them"
    expect(sectionInView(order, [], "run-execution")).toBe("run-execution");
    // but a section that has left the screen's anchors is no longer on the page
    expect(sectionInView(order, [], "draft-source")).toBeNull();
    expect(sectionInView([], [], "run-execution")).toBeNull();
    expect(sectionInView(order, [], null)).toBeNull();
  });

  it("ignores anything visible that is not one of this screen's sections", () => {
    expect(sectionInView(order, ["draft-source"], null)).toBeNull();
    expect(sectionInView(order, ["draft-source", "run-output"], null)).toBe("run-output");
  });
});
