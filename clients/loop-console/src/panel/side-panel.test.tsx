import { cleanup, fireEvent, render } from "@solidjs/testing-library";
import type { JSX } from "@solidjs/web";
import { flush } from "solid-js";
import { afterEach, describe, expect, it } from "vitest";

import { contributeScreen } from "../app/screen-actions";
import type { Route } from "../routing/route";
import { clearVisited, recordVisit, type VisitedEntry } from "../store/visited";
import { panelStorageKey, resetPanelState } from "./panel-state";
import { SidePanel } from "./side-panel";

const RUN_ID = "01J9QK3ZC0W7HG4M2NPX8V3F2K";

const run: VisitedEntry = {
  kind: "run",
  id: RUN_ID,
  revision: null,
  label: RUN_ID,
  verdict: "fail",
  visitedAt: Date.now(),
};

afterEach(() => {
  cleanup();
  clearVisited();
  resetPanelState();
  window.location.hash = "";
});

/**
 * The panel beside a screen that contributes sections, which is the only way
 * level 3 exists at all — the anchors are the mounted screen's own.
 */
function panel(route: Route, anchors: ReadonlyArray<{ id: string; label: string; unavailable?: string }> = []) {
  function Host(): JSX.Element {
    contributeScreen(() => ({ anchors, actions: [] }));
    return (
      <>
        <SidePanel route={route} />
        <div id="run-failure" tabindex="-1" />
        <div id="run-execution" tabindex="-1" />
      </>
    );
  }
  const view = render(() => <Host />);
  flush();
  return view;
}

/** Every row a reader can walk, in the order the tree draws them. */
function rows(container: HTMLElement): HTMLElement[] {
  return [...container.querySelectorAll<HTMLElement>('[role="treeitem"]')];
}

function rowNamed(container: HTMLElement, text: string): HTMLElement {
  const row = rows(container).find((element) => (element.textContent ?? "").includes(text));
  if (row === undefined) {
    throw new Error(`no row saying ${text}`);
  }
  return row;
}

describe("§2.0's side panel", () => {
  it("draws the three fixed levels, with the visited entity's glyph, id and time", () => {
    recordVisit(run);
    const { container } = panel({ kind: "run", id: RUN_ID }, [
      { id: "run-failure", label: "failure" },
      { id: "run-execution", label: "execution" },
    ]);

    expect(container.querySelector('[role="tree"]')).toHaveAttribute("aria-label", "visited entities");
    expect(rows(container).map((row) => row.getAttribute("aria-level"))).toEqual([
      "1", // start
      "1", // RUNS
      "2", // the visited run
      "3", // its sections
      "3",
      "1", // REPORTS
      "1", // DRAFTS
    ]);

    const entity = rowNamed(container, "01J9…3F2K");
    // the glyph is never the only carrier — the verdict is said in words too
    expect(entity).toHaveTextContent("verdict fail");
    expect(entity).toHaveTextContent("0s ago");
    // §2.6: hover reveals the full id when truncated
    expect(entity).toHaveAttribute("title", RUN_ID);
    // §2.0's active entity: the 2px signal edge is CSS, the claim behind it is this
    expect(entity).toHaveAttribute("aria-selected", "true");
  });

  it("teaches its own mechanic when nothing has been visited", () => {
    const { container } = panel({ kind: "start" });

    expect(rows(container).map((row) => row.textContent)).toEqual([
      "◈start",
      "RUNSruns: nothing visited yet",
      "REPORTSreports: nothing visited yet",
      "DRAFTSdrafts: nothing visited yet",
    ]);
  });

  it("navigates on a click, and moves to a section without leaving the entity", () => {
    recordVisit(run);
    const { container } = panel({ kind: "run", id: RUN_ID }, [
      { id: "run-execution", label: "execution" },
    ]);

    fireEvent.click(rowNamed(container, "start"));
    flush();
    expect(window.location.hash).toBe("#/");

    fireEvent.click(rowNamed(container, "01J9…3F2K"));
    flush();
    expect(window.location.hash).toBe(`#/run/${RUN_ID}`);

    // "Anchors, not routes — the URL stays the entity" (§2.0)
    fireEvent.click(rowNamed(container, "execution"));
    flush();
    expect(document.activeElement).toBe(document.getElementById("run-execution"));
    expect(window.location.hash).toBe(`#/run/${RUN_ID}`);
  });

  it("holds one tab stop and moves it with the tree's own keys", () => {
    recordVisit(run);
    const { container } = panel({ kind: "run", id: RUN_ID });

    const inOrder = () => rows(container).map((row) => row.getAttribute("tabindex"));
    expect(inOrder()).toEqual(["0", "-1", "-1", "-1", "-1"]);

    const start = rows(container)[0];
    fireEvent.keyDown(start, { key: "ArrowDown" });
    flush();
    expect(document.activeElement).toBe(rows(container)[1]);
    expect(inOrder()).toEqual(["-1", "0", "-1", "-1", "-1"]);

    fireEvent.keyDown(rows(container)[1], { key: "End" });
    flush();
    expect(document.activeElement).toBe(rows(container)[4]);

    fireEvent.keyDown(rows(container)[4], { key: "Home" });
    flush();
    expect(document.activeElement).toBe(rows(container)[0]);
  });

  it("collapses and expands a kind with ←/→, and remembers it", () => {
    recordVisit(run);
    const { container } = panel({ kind: "start" });
    const kind = rowNamed(container, "RUNS");

    expect(kind).toHaveAttribute("aria-expanded", "true");
    fireEvent.keyDown(kind, { key: "ArrowLeft" });
    flush();
    expect(rowNamed(container, "RUNS")).toHaveAttribute("aria-expanded", "false");
    // the entity is hidden, not forgotten
    expect(rows(container)).toHaveLength(4);
    // §2.0: "collapse state persists"
    expect(window.localStorage.getItem(panelStorageKey)).toContain("run");

    fireEvent.keyDown(rowNamed(container, "RUNS"), { key: "ArrowRight" });
    flush();
    expect(rowNamed(container, "RUNS")).toHaveAttribute("aria-expanded", "true");
    expect(rows(container)).toHaveLength(5);
  });

  it("collapses the whole panel to its rail from the footer, and remembers that too", () => {
    recordVisit(run);
    const { container, getByRole } = panel({ kind: "run", id: RUN_ID });

    fireEvent.click(getByRole("button", { name: "collapse the side panel" }));
    flush();

    // §2.0's 44px: kind initials and the active entity's glyph only
    expect(rows(container).map((row) => row.textContent)).toEqual([
      "◈start",
      "RUNruns",
      `✗verdict fail${RUN_ID}`,
      "RPTreports",
      "DFTdrafts",
    ]);
    expect(getByRole("button", { name: "expand the side panel" })).toHaveAttribute(
      "aria-expanded",
      "false",
    );
    expect(window.localStorage.getItem(panelStorageKey)).toContain('"collapsed":true');
  });

  it("dims a section the screen cannot stand, says why, and refuses to act on it", () => {
    recordVisit({ ...run, kind: "draft", id: "drf-1", revision: 16, label: "orders@16", verdict: "none" });
    const { container } = panel({ kind: "draft", id: "drf-1", revision: "16" }, [
      { id: "draft-source", label: "source" },
      { id: "draft-structure", label: "structure", unavailable: "does not parse — line 41" },
    ]);

    const structure = rowNamed(container, "structure");
    expect(structure).toHaveAttribute("aria-disabled", "true");
    expect(structure).toHaveAttribute("title", "does not parse — line 41");
    // a tooltip alone reaches neither a keyboard reader nor a touch one
    expect(structure).toHaveTextContent("does not parse — line 41");

    fireEvent.click(structure);
    flush();
    // §2.0's anchors never change the URL, and this one does not even scroll
    expect(window.location.hash).toBe("");
    expect(document.activeElement).not.toBe(document.getElementById("draft-structure"));
  });

  it("binds no key outside itself, because §6 step 7 leaves none to bind", () => {
    recordVisit(run);
    const { container } = panel({ kind: "start" });
    const before = rows(container).length;

    // §2.6 drafted `[` as a panel toggle; step 7 ruled that the palette is the
    // keyboard's only route and the palette shipped that way, so the panel
    // listens on the document for nothing at all.
    for (const key of ["[", "/"]) {
      const event = new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true });
      document.dispatchEvent(event);
      flush();
      expect(event.defaultPrevented).toBe(false);
    }
    expect(rows(container)).toHaveLength(before);
  });
});
