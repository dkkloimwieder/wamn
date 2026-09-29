/**
 * The QueryTable renders its load (wamn-xtz2.1, wamn-xtz2.2), and a header
 * click sorts (wamn-vfvx.1): in the set table when every row is read, and
 * through a new load otherwise (wamn-5pzt).
 *
 * The document has no layout, so the test gives the scroll box and each row
 * the height a browser would measure, as `window.test.tsx` does.
 */

import { cleanup, fireEvent, screen } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { afterAll, afterEach, beforeAll, describe, expect, it } from "vitest";

import { type TableColumn, WINDOW_FROM } from "@wamn/ui";

import { bodyRows, button, theButton } from "./dom.js";
import { queryTable, settled } from "./tables.js";

const BOX = 480;
const ROW = 48;

const measured = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "offsetHeight");

beforeAll(() => {
  Object.defineProperty(HTMLElement.prototype, "offsetHeight", {
    configurable: true,
    get(this: HTMLElement) {
      if (this.dataset["slot"] === "scroll-area-viewport") {
        return BOX;
      }
      return this.dataset["index"] === undefined ? 0 : ROW;
    },
  });
});

afterAll(() => {
  if (measured !== undefined) {
    Object.defineProperty(HTMLElement.prototype, "offsetHeight", measured);
  }
});

afterEach(cleanup);

interface Row {
  readonly id: string;
  readonly code: string;
  /** Falls as the code rises, so a sort by rank reverses the rows. */
  readonly rank: number;
}

const COLUMNS: readonly TableColumn<Row>[] = [
  { field: "code", label: "code", type: "text" },
  { field: "rank", label: "rank", type: "int32" },
];

const rows = (count: number): Row[] =>
  Array.from({ length: count }, (_, index) => ({
    id: `r${index}`,
    code: `c${String(index).padStart(4, "0")}`,
    rank: count - index,
  }));

const MESSAGE = "Full dataset cannot be loaded";

/** The codes of the body rows, in the order they show. */
const shown = () => bodyRows().map((row) => row.textContent?.slice(0, 5));

const header = (label: string) => button(label);

/** The sort the scope bar shows. */
const sortChips = () =>
  Array.from(document.querySelectorAll('[data-slot="table-scope-sort"] [data-slot="badge"]')).map(
    (chip) => chip.textContent,
  );

describe("the query table", () => {
  it("fills its container, with the toolbar outside the one element that scrolls (wamn-xtz2.4)", async () => {
    await queryTable({ columns: COLUMNS, rows: () => rows(3) });
    const section = document.querySelector<HTMLElement>('[data-slot="query-table"]')!;
    const toolbar = section.querySelector('[data-slot="table-toolbar"]')!;
    const grid = section.querySelector<HTMLElement>('[data-slot="data-grid"]')!;
    const body = grid.querySelector('[data-slot="scroll-area-viewport"]')!;
    expect(section.classList).toContain("h-full");
    // The grid takes the height of its rows, at most the height the toolbar
    // leaves, in place of its fixed one. Its scroll bar then sits under the
    // last row (wamn-n9ha).
    expect(grid.classList).toContain("flex-initial");
    expect(grid.classList).toContain("min-h-0");
    expect(grid.classList).not.toContain("h-[32rem]");
    expect(toolbar.contains(body)).toBe(false);
    expect(body.contains(screen.getByText("c0000"))).toBe(true);
  });

  it("renders every row at the windowing limit", async () => {
    await queryTable({ columns: COLUMNS, rows: () => rows(WINDOW_FROM) });
    // One header row, one row for each record, and the totals row.
    expect(screen.getAllByRole("row")).toHaveLength(WINDOW_FROM + 2);
  });

  it("renders a window of its rows above the windowing limit", async () => {
    await queryTable({ columns: COLUMNS, rows: () => rows(1000) });
    expect(screen.getByText("c0000")).toBeDefined();
    expect(screen.getAllByRole("row").length).toBeLessThan(50);
    expect(screen.queryByText("c0999")).toBeNull();
  });

  it("loads again at a committed cap, and ignores one that is not a positive integer", async () => {
    const { asked } = await queryTable({ columns: COLUMNS, rows: () => rows(3) });
    const input = screen.getByLabelText("cap");
    fireEvent.change(input, { target: { value: "0" } });
    fireEvent.change(input, { target: { value: "2.5" } });
    fireEvent.change(input, { target: { value: "5000" } });
    await settled();
    expect(asked.map((request) => request.limit)).toEqual([1000, 5000]);
  });

  it("shows the message and the one line of the set features when the load ended and the set is not fully read", async () => {
    await queryTable({ columns: COLUMNS, rows: () => rows(3), more: () => true });
    expect(screen.getByText(MESSAGE)).toBeDefined();
    expect(
      screen.getByText(/^Filters, search, grouping, totals and CSV export show when every row is read/),
    ).toBeDefined();
    expect(screen.getByText(/^ended /)).toBeDefined();
    // The set features are not there at all, rather than disabled.
    expect(screen.queryByLabelText("search")).toBeNull();
    expect(button("export CSV")).toBeNull();
    expect(button("filter code")).toBeNull();
  });

  it("places a set table over a fully read set only, and keeps it through a reload", async () => {
    const [more, setMore] = createSignal(true);
    await queryTable({ columns: COLUMNS, rows: () => rows(3), more });
    const setControls = () =>
      document.querySelectorAll('[data-slot="set-table-toolbar"], [data-slot="table-group-bar"]');
    expect(setControls()).toHaveLength(0);
    setMore(false);
    fireEvent.click(theButton("refresh"));
    await settled();
    expect(setControls()).toHaveLength(2);
    const toolbar = setControls()[0];
    // A reload runs with the last set in place, so the set table stays.
    fireEvent.click(theButton("refresh"));
    expect(toolbar?.isConnected).toBe(true);
    await settled();
    expect(toolbar?.isConnected).toBe(true);
  });

  it("shows no message when the set is fully read", async () => {
    await queryTable({ columns: COLUMNS, rows: () => rows(3) });
    expect(screen.queryByText(MESSAGE)).toBeNull();
    expect(button("export CSV")).not.toBeNull();
  });

  it("shows a loading state and no message while busy", async () => {
    await queryTable({ columns: COLUMNS, rows: () => [], never: true });
    expect(screen.getAllByText("Loading...").length).toBeGreaterThan(0);
    expect(screen.queryByText(MESSAGE)).toBeNull();
    expect(screen.getByText(/^started /)).toBeDefined();
    expect(screen.queryByText(/^ended /)).toBeNull();
  });

  it("loads again from its refresh control, which is disabled while busy", async () => {
    const { asked } = await queryTable({ columns: COLUMNS, rows: () => rows(3) });
    fireEvent.click(theButton("refresh"));
    await settled();
    expect(asked).toHaveLength(2);
    cleanup();
    await queryTable({ columns: COLUMNS, rows: () => [], never: true });
    expect(theButton("refresh").hasAttribute("disabled")).toBe(true);
  });

  it("shows a refusal in place of the empty message, and not the message", async () => {
    await queryTable({ columns: COLUMNS, rows: () => [], refuse: "unauthenticated" });
    expect(screen.getByText("You are not signed in.")).toBeDefined();
    expect(screen.queryByText(MESSAGE)).toBeNull();
  });

  it("sorts a fully read set in the table, and does not load again", async () => {
    const { asked } = await queryTable({ columns: COLUMNS, rows: () => rows(3) });
    expect(shown()).toEqual(["c0000", "c0001", "c0002"]);
    fireEvent.click(header("rank")!);
    expect(shown()).toEqual(["c0002", "c0001", "c0000"]);
    fireEvent.click(header("rank")!);
    expect(shown()).toEqual(["c0000", "c0001", "c0002"]);
    expect(asked).toHaveLength(1);
  });

  it("lets every column sort once a set becomes fully read", async () => {
    const [more, setMore] = createSignal(true);
    await queryTable({ columns: COLUMNS, rows: () => rows(3), more });
    expect(header("rank")).toBeNull();
    setMore(false);
    fireEvent.click(theButton("refresh"));
    await settled();
    fireEvent.click(header("rank")!);
    expect(shown()).toEqual(["c0002", "c0001", "c0000"]);
  });

  it("loads again in the new order on a set that is not fully read, and shows the rows as the read returns them", async () => {
    const { asked } = await queryTable({
      columns: COLUMNS,
      rows: () => rows(3),
      more: () => true,
      sortFields: ["rank"],
    });
    fireEvent.click(header("rank")!);
    await settled();
    expect(shown()).toEqual(["c0002", "c0001", "c0000"]);
    fireEvent.click(header("rank")!);
    await settled();
    expect(asked.slice(1).map((request) => request.sort)).toEqual([
      { field: "rank", direction: "ascending" },
      { field: "rank", direction: "descending" },
    ]);
    expect(shown()).toEqual(["c0000", "c0001", "c0002"]);
  });

  it("lets only the declared sort fields sort a set that is not fully read", async () => {
    await queryTable({ columns: COLUMNS, rows: () => rows(3), more: () => true, sortFields: ["rank"] });
    expect(header("rank")).not.toBeNull();
    expect(header("code")).toBeNull();
    expect(screen.getByText("code")).toBeDefined();
    cleanup();
    await queryTable({ columns: COLUMNS, rows: () => rows(3), sortFields: ["rank"] });
    expect(header("code")).not.toBeNull();
  });

  it("adds a field on a shift click only when the sort can hold more than one", async () => {
    for (const [sortMaxFields, last] of [
      [1, ["code ascending"]],
      [2, ["rank ascending", "code ascending"]],
    ] as const) {
      await queryTable({
        columns: COLUMNS,
        rows: () => rows(3),
        more: () => true,
        sortFields: ["code", "rank"],
        sortMaxFields,
      });
      fireEvent.click(header("rank")!);
      await settled();
      fireEvent.click(header("code")!, { shiftKey: true });
      await settled();
      expect(sortChips()).toEqual(last);
      cleanup();
    }
  });
});
