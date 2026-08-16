import { cleanup, render } from "@solidjs/testing-library";
import type { JSX } from "@solidjs/web";
import { createSignal, flush } from "solid-js";
import { afterEach, describe, expect, it } from "vitest";

import { contributeScreen } from "../app/screen-actions";
import { clearVisited, recordVisit, type VisitedEntry } from "../store/visited";
import { CommandPalette } from "./palette";

/**
 * The overlay's interaction. What it *lists* is `commands.test.ts`'s; what is
 * proved here is that ⌘K reaches it wherever focus is, that the keys move and
 * activate what the reader is looking at, and that the reader is handed back the
 * focus the palette borrowed.
 */

afterEach(() => {
  cleanup();
  // Module state, and it outlives a test: two suites sharing a history would
  // rank each other's rows.
  clearVisited();
  window.location.hash = "";
});

const RUN_ID = "01J9X4T7QW3B8ZC5NDR6MK3F2K";
const REPORT_ID = "01J9T5C8NB2XQ7ZD4RM9KV8Q11";

function visit(over: Pick<VisitedEntry, "kind" | "id"> & Partial<VisitedEntry>): VisitedEntry {
  return {
    revision: over.kind === "draft" ? (over.revision ?? 17) : null,
    label: over.id,
    verdict: "none",
    visitedAt: Date.now(),
    ...over,
  };
}

/** A key, sent from wherever focus actually is — which is what the console sees. */
function press(key: string, init: KeyboardEventInit = {}): KeyboardEvent {
  const event = new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true, ...init });
  (document.activeElement ?? document).dispatchEvent(event);
  flush();
  return event;
}

function dialog(container: HTMLElement): HTMLElement | null {
  return container.querySelector<HTMLElement>("[role='dialog']");
}

/** Every row a reader can reach in the open palette, in the order they are drawn. */
function rows(container: HTMLElement): HTMLButtonElement[] {
  return [...container.querySelectorAll<HTMLButtonElement>("[role='dialog'] button")];
}

function rowText(container: HTMLElement): string[] {
  return rows(container).map((row) => (row.textContent ?? "").replace(/\s+/g, " ").trim());
}

function activeRow(container: HTMLElement): number {
  return rows(container).findIndex((row) => row.getAttribute("aria-current") === "true");
}

/** Every ranked group in the open palette, in the order they are drawn. */
function groups(container: HTMLElement): HTMLElement[] {
  return [...container.querySelectorAll<HTMLElement>("[role='dialog'] [role='group']")];
}

/**
 * The heading a group is named by, read through the association rather than by
 * position: jsdom computes no accessible name, so what is checked is the link
 * that would produce one.
 */
function groupName(group: HTMLElement): string {
  const heading = document.getElementById(group.getAttribute("aria-labelledby") ?? "");
  if (heading === null || !group.contains(heading)) {
    throw new Error("a palette group is not labelled by a heading inside it");
  }
  if (heading.tagName !== "H2") {
    throw new Error(`a palette group is named by a <${heading.tagName.toLowerCase()}>`);
  }
  return (heading.textContent ?? "").trim();
}

/** The one group that goes by this name, by the name a reader would hear. */
function group(container: HTMLElement, name: string): HTMLElement {
  const found = groups(container).filter((candidate) => groupName(candidate) === name);
  if (found.length !== 1) {
    throw new Error(`the palette draws ${found.length} groups named ${JSON.stringify(name)}`);
  }
  return found[0];
}

/** Types into the palette's one input, as a reader narrowing the list does. */
function type(container: HTMLElement, text: string): void {
  const input = container.querySelector("input");
  if (input === null) {
    throw new Error("the palette has no input");
  }
  input.value = text;
  input.dispatchEvent(new Event("input", { bubbles: true }));
  flush();
}

/**
 * The console around the palette: something to focus, an optional screen, and
 * the palette itself.
 *
 * The screen arrives as a thunk and not as an element. A component built outside
 * `render` runs with no owner, so its `onCleanup` registers nowhere — a screen
 * created that way would contribute to the palette and never release, and the
 * next test would rank the previous test's sections.
 */
function mount(screen?: () => JSX.Element) {
  const view = render(() => (
    <>
      <button type="button" id="before">
        something else on the page
      </button>
      {screen?.()}
      <CommandPalette />
    </>
  ));
  flush();
  const before = view.container.querySelector<HTMLButtonElement>("#before");
  before?.focus();
  return { ...view, before };
}

describe("opening and closing", () => {
  it("opens on ⌘K and on Ctrl-K, and takes the key from the browser", () => {
    const { container } = mount();
    expect(dialog(container)).toBeNull();

    const meta = press("k", { metaKey: true });
    expect(meta.defaultPrevented).toBe(true);
    expect(dialog(container)).not.toBeNull();

    // the same key closes it again
    press("k", { metaKey: true });
    expect(dialog(container)).toBeNull();

    const ctrl = press("k", { ctrlKey: true });
    expect(ctrl.defaultPrevented).toBe(true);
    expect(dialog(container)).not.toBeNull();
  });

  it("names itself, is modal, and moves focus into its one input", () => {
    const { container } = mount();
    press("k", { ctrlKey: true });

    const overlay = dialog(container);
    expect(overlay).toHaveAttribute("aria-label", "command palette");
    // spelled, not boolean: @solidjs/web drops a boolean aria value entirely
    expect(overlay).toHaveAttribute("aria-modal", "true");
    expect(container.querySelectorAll("input")).toHaveLength(1);
    expect(document.activeElement).toBe(container.querySelector("input"));
  });

  it("closes on Esc and hands focus back to what had it", () => {
    const { container, before } = mount();
    expect(document.activeElement).toBe(before);

    press("k", { metaKey: true });
    expect(document.activeElement).not.toBe(before);

    const escape = press("Escape");
    expect(escape.defaultPrevented).toBe(true);
    expect(dialog(container)).toBeNull();
    expect(document.activeElement).toBe(before);
  });
});

describe("walking the list", () => {
  it("moves the cursor with ↑↓ and activates the row with Enter", () => {
    recordVisit(visit({ kind: "run", id: RUN_ID }));
    recordVisit(visit({ kind: "report", id: REPORT_ID }));
    const { container } = mount();

    press("k", { metaKey: true });
    expect(activeRow(container)).toBe(0);
    expect(rowText(container)[0]).toContain(`open report ${REPORT_ID}`);

    const down = press("ArrowDown");
    expect(down.defaultPrevented).toBe(true);
    expect(activeRow(container)).toBe(1);
    expect(rowText(container)[1]).toContain(`open run ${RUN_ID}`);

    press("ArrowUp");
    expect(activeRow(container)).toBe(0);

    // ↑ from the top wraps rather than sticking
    press("ArrowUp");
    expect(activeRow(container)).toBe(rows(container).length - 1);

    press("ArrowDown");
    expect(activeRow(container)).toBe(0);
    const enter = press("Enter");
    expect(enter.defaultPrevented).toBe(true);
    expect(window.location.hash).toBe(`#/report/${REPORT_ID}`);
    expect(dialog(container)).toBeNull();
  });

  it("⌘K ↵ reopens the last entity", () => {
    recordVisit(visit({ kind: "report", id: REPORT_ID }));
    recordVisit(visit({ kind: "draft", id: "orders", revision: 17, label: "orders@17" }));
    const { container, before } = mount();

    press("k", { metaKey: true });
    press("Enter");
    // the draft was the last thing looked at, and its route carries its revision
    expect(window.location.hash).toBe("#/draft/orders/17");
    expect(dialog(container)).toBeNull();
    // this harness has no column for the navigation to land in, so the palette
    // does not claim it placed focus: the reader is handed back what they had
    // rather than left on <body> by a claim that was not true
    expect(document.activeElement).toBe(before);
  });

  it("activates the row a reader clicks, wherever the cursor is", () => {
    recordVisit(visit({ kind: "run", id: RUN_ID }));
    const { container } = mount();
    press("k", { metaKey: true });

    const row = rows(container).find((candidate) =>
      (candidate.textContent ?? "").includes("clear visited"),
    );
    row?.click();
    flush();
    expect(dialog(container)).toBeNull();
  });
});

describe("navigating", () => {
  /**
   * The shell's own column (§1.4), which is where a route change delivers the
   * next screen — and the only thing on the page that outlives the swap.
   */
  function mountWithColumn() {
    const view = render(() => (
      <>
        <main tabindex="-1">
          <button type="button" id="before">
            a control on the screen being left
          </button>
        </main>
        <CommandPalette />
      </>
    ));
    flush();
    const before = view.container.querySelector<HTMLButtonElement>("#before");
    before?.focus();
    return { ...view, before };
  }

  it("leaves the reader in the column, not on the control the route destroys", () => {
    const { container, before } = mountWithColumn();
    press("k", { metaKey: true });

    const row = rows(container).find((candidate) =>
      (candidate.textContent ?? "").includes("go to start"),
    );
    row?.click();
    flush();

    expect(window.location.hash).toBe("#/");
    expect(dialog(container)).toBeNull();
    // the control focus came from belongs to the screen this navigation
    // unmounts, so handing focus back to it strands the reader on <body> — one
    // Tab from the top of the document, after the console's only route to
    // navigation
    expect(document.activeElement).toBe(container.querySelector("main"));
    expect(document.activeElement).not.toBe(before);
    expect(document.activeElement).not.toBe(document.body);
  });
});

describe("the cursor, while the list changes under it", () => {
  /**
   * A screen whose offer grows and shrinks after it has contributed, which is
   * what a real one does: `run-screen.tsx` reads the run's failure *inside* its
   * thunk, so a read settling adds a section to a palette that is already open.
   */
  function LiveScreen(props: { failure: () => boolean }): JSX.Element {
    contributeScreen(() => ({
      anchors: [
        ...(props.failure() ? [{ id: "run-failure", label: "failure" }] : []),
        { id: "run-execution", label: "execution" },
      ],
      actions: [{ id: "expand-all", label: "expand all execution rows", run: () => undefined }],
    }));
    return null;
  }

  it("stays on the command it was on when a group above it grows", () => {
    recordVisit(visit({ kind: "run", id: RUN_ID }));
    const [failure, setFailure] = createSignal(false);
    const { container } = mount(() => <LiveScreen failure={failure} />);
    press("k", { metaKey: true });

    // down to `go to start`, which sits below the `this screen` group
    press("ArrowDown");
    press("ArrowDown");
    press("ArrowDown");
    expect(rowText(container)[activeRow(container)]).toContain("go to start");

    // the screen's read settles and it turns out the run failed: a section
    // appears above the cursor and every row below it shifts down one
    setFailure(true);
    flush();
    expect(rowText(container).join(" ")).toContain("go to failure");
    expect(rowText(container)[activeRow(container)]).toContain("go to start");

    // and Enter runs the command the reader was looking at, not the one that
    // took its place in the list
    press("Enter");
    expect(window.location.hash).toBe("#/");
  });

  it("returns to the top when the command it was on leaves the list", () => {
    recordVisit(visit({ kind: "run", id: RUN_ID }));
    const [failure, setFailure] = createSignal(true);
    const { container } = mount(() => <LiveScreen failure={failure} />);
    press("k", { metaKey: true });

    press("ArrowDown");
    expect(rowText(container)[activeRow(container)]).toContain("go to failure");

    // a refresh answers that the run is no longer failing, and the section the
    // cursor was on is gone
    setFailure(false);
    flush();
    expect(rowText(container).join(" ")).not.toContain("go to failure");
    // the top, and deliberately not the row that slid into the empty place: the
    // reader's choice is no longer on offer, and its neighbour is not it
    expect(activeRow(container)).toBe(0);
    expect(rowText(container)[0]).toContain(`open run ${RUN_ID}`);

    // whatever the cursor names is still a row a reader can be pointed at
    const named = container.querySelector("input")?.getAttribute("aria-activedescendant");
    expect(named).not.toBeNull();
    expect(document.getElementById(named ?? "")).toBe(rows(container)[0]);

    press("Enter");
    expect(window.location.hash).toBe(`#/run/${RUN_ID}`);
  });

  it("points at no row, and activates nothing, when nothing matches", () => {
    const { container } = mount();
    press("k", { metaKey: true });

    const input = container.querySelector("input");
    if (input === null) {
      throw new Error("the palette has no input");
    }
    input.value = "no row says this";
    input.dispatchEvent(new Event("input", { bubbles: true }));
    flush();

    expect(rows(container)).toHaveLength(0);
    // nothing to point assistive tech at, so it is not pointed at anything
    expect(input).not.toHaveAttribute("aria-activedescendant");

    press("Enter");
    // no route was taken and the reader is left where they can retype
    expect(window.location.hash).toBe("");
    expect(dialog(container)).not.toBeNull();
  });
});

describe("typing", () => {
  it("filters the list, and offers a typed id in the ranked kind order", () => {
    recordVisit(visit({ kind: "report", id: REPORT_ID }));
    const { container } = mount();
    press("k", { metaKey: true });

    const input = container.querySelector("input");
    if (input === null) {
      throw new Error("the palette has no input");
    }
    input.value = "01J9NEW";
    input.dispatchEvent(new Event("input", { bubbles: true }));
    flush();

    const drawn = rowText(container);
    // a report was the last kind read, so a bare id most likely names one
    expect(drawn[0]).toContain("open report 01J9NEW");
    expect(drawn[1]).toContain("open run 01J9NEW");
    // the recent no longer matches, and neither console command does
    expect(drawn.join(" ")).not.toContain(REPORT_ID);
    expect(drawn.join(" ")).not.toContain("clear visited");
    // and the absence a bare id leaves is stated rather than left blank
    expect(dialog(container)).toHaveTextContent("a draft is addressed by an id and a revision");
  });
});

describe("the three ranked groups", () => {
  it("names each group with a heading it is labelled by, in step 7's fixed order", () => {
    recordVisit(visit({ kind: "run", id: RUN_ID }));
    const { container } = mount();
    press("k", { metaKey: true });

    // the ranking is the point of step 7, so it is a thing a reader is told and
    // not only a thing they can see
    expect(groups(container).map(groupName)).toEqual(["go to", "this screen", "console"]);
  });

  it("puts a group's rows in a list of that group's own, still as real buttons", () => {
    recordVisit(visit({ kind: "run", id: RUN_ID }));
    recordVisit(visit({ kind: "report", id: REPORT_ID }));
    const { container } = mount();
    press("k", { metaKey: true });

    const goTo = group(container, "go to");
    const list = goTo.querySelector("[role='list']");
    expect(list?.tagName).toBe("UL");
    // two recents, and the list says how many there are to walk
    expect(list?.children).toHaveLength(2);
    for (const item of [...(list?.children ?? [])]) {
      expect(item.tagName).toBe("LI");
    }

    // and every row on the whole palette stands inside the list of the one
    // group it belongs to — which is what a reader was hearing nothing of
    for (const row of rows(container)) {
      expect(row.tagName).toBe("BUTTON");
      const owner = row.closest("[role='group']");
      expect(owner).not.toBeNull();
      expect(row.parentElement?.tagName).toBe("LI");
      expect(row.parentElement?.parentElement).toBe(owner?.querySelector("[role='list']"));
    }

    // the console group is the last of the three and holds its own rows
    expect(group(container, "console").querySelectorAll("button").length).toBeGreaterThan(0);
  });

  it("leaves the rows reachable and the group semantics off every control", () => {
    recordVisit(visit({ kind: "run", id: RUN_ID }));
    const { container } = mount();
    press("k", { metaKey: true });

    // the floor's rule, restated where the roles were added: nothing that is a
    // control plays a role it is not, and nothing structural became a control
    for (const element of container.querySelectorAll<HTMLElement>("[role='dialog'] *")) {
      const role = element.getAttribute("role");
      if (role === "group" || role === "list" || role === "note") {
        expect(["DIV", "UL", "P"]).toContain(element.tagName);
        expect(element.getAttribute("tabindex")).toBeNull();
      }
    }
    for (const row of rows(container)) {
      expect(row.getAttribute("tabindex") ?? "0").not.toMatch(/^-/);
      expect(row.getAttribute("role")).toBeNull();
    }
  });
});

describe("what a group says about what it has not got", () => {
  it("draws the empty-state primitive only where a group has no rows", () => {
    recordVisit(visit({ kind: "report", id: REPORT_ID }));
    const { container } = mount();
    press("k", { metaKey: true });
    // an id nothing has been visited under: `go to` offers the two routes it
    // could name, and says in the same breath why neither is a draft
    type(container, "01J9NEW");

    const goTo = group(container, "go to");
    expect(goTo.querySelectorAll("button").length).toBeGreaterThan(0);
    // the region is not empty, so the region-is-empty primitive is not drawn
    // over it — the bug this replaced announced the opposite of what is there
    expect(goTo.querySelector(".empty-state")).toBeNull();
    expect(goTo.querySelector("[role='note']")).toHaveTextContent(
      "a draft is addressed by an id and a revision",
    );

    // and the two groups that really are empty still state it, in words
    const screen = group(container, "this screen");
    expect(screen.querySelectorAll("button")).toHaveLength(0);
    expect(screen.querySelector(".empty-state")).toHaveTextContent("nothing on this screen matches");
    expect(group(container, "console").querySelector(".empty-state")).toHaveTextContent(
      "nothing in the console matches",
    );
  });

  it("renders a group with no rows as empty, and never as a note", () => {
    const { container } = mount();
    press("k", { metaKey: true });

    const goTo = group(container, "go to");
    expect(goTo.querySelectorAll("button")).toHaveLength(0);
    expect(goTo.querySelector(".empty-state")).toHaveTextContent("nothing visited yet");
    // one primitive per absence: the note is what a populated group uses
    expect(goTo.querySelector("[role='note']")).toBeNull();
  });

  it("says nothing beside rows that have nothing to explain", () => {
    recordVisit(visit({ kind: "run", id: RUN_ID }));
    const { container } = mount();
    press("k", { metaKey: true });

    // an empty query offers the recent and raises no rule against it
    const goTo = group(container, "go to");
    expect(goTo.querySelectorAll("button").length).toBeGreaterThan(0);
    expect(goTo.querySelector(".empty-state")).toBeNull();
    expect(goTo.querySelector("[role='note']")).toBeNull();
  });
});

describe("this screen", () => {
  /** A screen that offers the palette one section and one action, as a real one does. */
  function Screen(props: { onExpand: () => void }): JSX.Element {
    contributeScreen(() => ({
      anchors: [{ id: "run-execution", label: "execution" }],
      actions: [{ id: "expand-all", label: "expand all execution rows", run: props.onExpand }],
    }));
    return (
      <div id="run-execution" tabindex="-1">
        execution
      </div>
    );
  }

  it("runs the screen's own callback rather than a second implementation", () => {
    let expanded = 0;
    const { container, before } = mount(() => <Screen onExpand={() => (expanded += 1)} />);
    press("k", { metaKey: true });

    const row = rows(container).find((candidate) =>
      (candidate.textContent ?? "").includes("expand all execution rows"),
    );
    row?.click();
    flush();

    expect(expanded).toBe(1);
    expect(dialog(container)).toBeNull();
    // the action changed the screen, not where the reader was standing
    expect(document.activeElement).toBe(before);
  });

  it("moves the reader to a section, and does not drag them back", () => {
    const { container } = mount(() => <Screen onExpand={() => undefined} />);
    press("k", { metaKey: true });

    const row = rows(container).find((candidate) =>
      (candidate.textContent ?? "").includes("go to execution"),
    );
    row?.click();
    flush();

    expect(dialog(container)).toBeNull();
    expect(document.activeElement).toBe(container.querySelector("#run-execution"));
  });

  it("says so when the mounted screen offers nothing", () => {
    const { container } = mount();
    press("k", { metaKey: true });
    expect(dialog(container)).toHaveTextContent(
      "the screen you are on offers the palette no sections and no actions",
    );
  });
});

describe("the console group", () => {
  it("empties the visited store, and the list says so at once", () => {
    recordVisit(visit({ kind: "run", id: RUN_ID }));
    const { container } = mount();
    press("k", { metaKey: true });
    expect(rowText(container).join(" ")).toContain(`open run ${RUN_ID}`);

    const row = rows(container).find((candidate) =>
      (candidate.textContent ?? "").includes("clear visited"),
    );
    row?.click();
    flush();

    press("k", { metaKey: true });
    expect(rowText(container).join(" ")).not.toContain(`open run ${RUN_ID}`);
    expect(dialog(container)).toHaveTextContent("nothing visited yet");
  });

  it("goes to the start screen", () => {
    const { container } = mount();
    press("k", { metaKey: true });
    const row = rows(container).find((candidate) =>
      (candidate.textContent ?? "").includes("go to start"),
    );
    row?.click();
    flush();
    expect(window.location.hash).toBe("#/");
  });
});

describe("the accessibility floor, on the palette", () => {
  it("makes every row a real button, and no two of them answer to one name", () => {
    recordVisit(visit({ kind: "run", id: RUN_ID }));
    // a screen whose own control is also on the page behind the overlay
    const { container } = mount(() => (
      <>
        <button type="button" aria-label="expand all execution rows">
          expand all
        </button>
        <Contributor />
      </>
    ));
    press("k", { metaKey: true });

    const named = container.querySelectorAll<HTMLElement>("button");
    const names = [...named].map((button) => {
      const label = button.getAttribute("aria-label");
      return (label ?? button.textContent ?? "").replace(/\s+/g, " ").trim();
    });
    for (const name of names) {
      expect(name).toMatch(/\p{L}/u);
    }
    expect(new Set(names).size).toBe(names.length);

    // nothing in the overlay handles events except controls that are buttons
    for (const element of container.querySelectorAll<HTMLElement>("[role='dialog'] *")) {
      const handled = Object.keys(element).some((key) => key.startsWith("$$"));
      if (handled) {
        expect(["BUTTON", "INPUT"]).toContain(element.tagName);
      }
      if (["BUTTON", "INPUT"].includes(element.tagName)) {
        expect(element.getAttribute("tabindex") ?? "0").not.toMatch(/^-/);
      }
    }
  });

  it("tells the active row with a glyph and a state, never with colour alone", () => {
    recordVisit(visit({ kind: "run", id: RUN_ID }));
    const { container } = mount();
    press("k", { metaKey: true });

    const [first, second] = rows(container);
    expect(first).toHaveAttribute("aria-current", "true");
    expect(second).toHaveAttribute("aria-current", "false");
    expect(first.textContent).toContain("▸");
    expect(second.textContent).not.toContain("▸");

    // and the input points assistive tech at whichever row that is
    const input = container.querySelector("input");
    expect(input).toHaveAttribute("aria-activedescendant", first.id);
    press("ArrowDown");
    expect(input).toHaveAttribute("aria-activedescendant", rows(container)[1].id);
  });

  it("keeps Tab inside the dialog, which is what aria-modal claims", () => {
    const { container, before } = mount();
    press("k", { metaKey: true });

    const stops = [container.querySelector<HTMLElement>("input"), ...rows(container)];
    const last = stops[stops.length - 1];
    last?.focus();
    const wrapped = press("Tab");
    expect(wrapped.defaultPrevented).toBe(true);
    expect(document.activeElement).toBe(stops[0]);

    const back = press("Tab", { shiftKey: true });
    expect(back.defaultPrevented).toBe(true);
    expect(document.activeElement).toBe(last);
    expect(document.activeElement).not.toBe(before);
  });
});

/** Contributes the same offer the screen suite uses, without its own markup. */
function Contributor(): JSX.Element {
  contributeScreen(() => ({
    anchors: [{ id: "run-execution", label: "execution" }],
    actions: [{ id: "expand-all", label: "expand all execution rows", run: () => undefined }],
  }));
  return null;
}
