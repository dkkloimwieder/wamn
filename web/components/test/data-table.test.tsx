/**
 * The DataTable renders the load state it is given (wamn-xtz2.1, wamn-xtz2.2),
 * and a header click sorts (wamn-vfvx.1).
 *
 * The document has no layout, so the test gives the scroll box and each row
 * the height a browser would measure, as `window.test.tsx` does.
 */

import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import { DataTable, WINDOW_FROM, type DataTableColumn, type DataTableSort } from "@wamn/ui";

import { bodyRows, button } from "./dom.js";

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

const COLUMNS: readonly DataTableColumn<Row>[] = [
  { field: "code", label: "code", type: "text" },
  { field: "rank", label: "rank", type: "int32" },
];

const rows = (count: number): Row[] =>
  Array.from({ length: count }, (_, index) => ({
    id: `r${index}`,
    code: `c${String(index).padStart(4, "0")}`,
    rank: count - index,
  }));

interface Shown {
  readonly rows: Row[];
  readonly fullyRead: boolean;
  readonly busy: boolean;
  readonly refusal?: string;
  readonly sortFields?: readonly { readonly field: keyof Row & string }[];
  readonly sortMaxFields?: number;
  readonly onSortChange?: (sort: readonly DataTableSort<Row>[]) => void;
}

function table(state: Shown, onCapChange = () => {}, onRefresh = () => {}) {
  render(() => (
    <DataTable
      columns={COLUMNS}
      rowId="id"
      rows={state.rows}
      fullyRead={state.fullyRead}
      busy={state.busy}
      cap={1000}
      onCapChange={onCapChange}
      refusal={state.refusal ?? null}
      onRefresh={onRefresh}
      startedAt={new Date(0)}
      endedAt={state.busy ? null : new Date(1000)}
      sortFields={state.sortFields ?? []}
      sortMaxFields={state.sortMaxFields ?? 1}
      onSortChange={state.onSortChange ?? (() => {})}
    />
  ));
}

const MESSAGE = "Full dataset cannot be loaded";

/** The codes of the body rows, in the order they show. */
const shown = () =>
  bodyRows().map((row) => row.textContent?.slice(0, 5));

const header = (label: string) => button(label);

describe("the data table", () => {
  it("fills its container, with the toolbar outside the one element that scrolls (wamn-xtz2.4)", () => {
    table({ rows: rows(3), fullyRead: true, busy: false });
    const section = document.querySelector<HTMLElement>('[data-slot="data-table"]')!;
    const toolbar = section.querySelector('[data-slot="data-table-toolbar"]')!;
    const grid = section.querySelector<HTMLElement>('[data-slot="data-grid"]')!;
    const body = grid.querySelector('[data-slot="scroll-area-viewport"]')!;
    expect(section.classList).toContain("h-full");
    // The grid takes the height the toolbar leaves, in place of its fixed one.
    expect(grid.classList).toContain("flex-1");
    expect(grid.classList).toContain("min-h-0");
    expect(grid.classList).not.toContain("h-[32rem]");
    expect(toolbar.contains(body)).toBe(false);
    expect(body.contains(screen.getByText("c0000"))).toBe(true);
  });

  it("renders every row at the windowing limit", () => {
    table({ rows: rows(WINDOW_FROM), fullyRead: true, busy: false });
    // One header row, one row for each record, and the totals row.
    expect(screen.getAllByRole("row")).toHaveLength(WINDOW_FROM + 2);
  });

  it("renders a window of its rows above the windowing limit", () => {
    table({ rows: rows(1000), fullyRead: true, busy: false });
    expect(screen.getByText("c0000")).toBeDefined();
    expect(screen.getAllByRole("row").length).toBeLessThan(50);
    expect(screen.queryByText("c0999")).toBeNull();
  });

  it("calls onCapChange with a committed cap, and ignores one that is not a positive integer", () => {
    const onCapChange = vi.fn();
    table({ rows: rows(3), fullyRead: true, busy: false }, onCapChange);
    const input = screen.getByLabelText("cap");
    fireEvent.change(input, { target: { value: "0" } });
    fireEvent.change(input, { target: { value: "2.5" } });
    fireEvent.change(input, { target: { value: "5000" } });
    expect(onCapChange.mock.calls).toEqual([[5000]]);
  });

  it("shows the message when the load ended and the set is not fully read", () => {
    table({ rows: rows(3), fullyRead: false, busy: false });
    expect(screen.getByText(MESSAGE)).toBeDefined();
    expect(screen.getByText(/^ended /)).toBeDefined();
  });

  it("shows no message when the set is fully read", () => {
    table({ rows: rows(3), fullyRead: true, busy: false });
    expect(screen.queryByText(MESSAGE)).toBeNull();
  });

  it("shows a loading state and no message while busy", () => {
    table({ rows: [], fullyRead: false, busy: true });
    expect(screen.getAllByText("Loading...").length).toBeGreaterThan(0);
    expect(screen.queryByText(MESSAGE)).toBeNull();
    expect(screen.getByText(/^started /)).toBeDefined();
    expect(screen.queryByText(/^ended /)).toBeNull();
  });

  it("calls onRefresh from its control, which is disabled while busy", () => {
    const onRefresh = vi.fn();
    table({ rows: rows(3), fullyRead: true, busy: false }, undefined, onRefresh);
    fireEvent.click(screen.getByRole("button", { name: "refresh" }));
    expect(onRefresh).toHaveBeenCalledTimes(1);
    cleanup();
    table({ rows: [], fullyRead: false, busy: true });
    expect(screen.getByRole("button", { name: "refresh" }).hasAttribute("disabled")).toBe(true);
  });

  it("shows a refusal in place of the empty message, and not the message", () => {
    table({ rows: [], fullyRead: false, busy: false, refusal: "You are not signed in." });
    expect(screen.getByText("You are not signed in.")).toBeDefined();
    expect(screen.queryByText(MESSAGE)).toBeNull();
  });

  it("sorts a fully read set in the table, and does not call onSortChange", () => {
    const onSortChange = vi.fn();
    table({ rows: rows(3), fullyRead: true, busy: false, onSortChange });
    expect(shown()).toEqual(["c0000", "c0001", "c0002"]);
    fireEvent.click(header("rank")!);
    expect(shown()).toEqual(["c0002", "c0001", "c0000"]);
    fireEvent.click(header("rank")!);
    expect(shown()).toEqual(["c0000", "c0001", "c0002"]);
    expect(onSortChange).not.toHaveBeenCalled();
  });

  it("calls onSortChange with the whole sort on a set that is not fully read, and does not sort", () => {
    const onSortChange = vi.fn();
    table({
      rows: rows(3),
      fullyRead: false,
      busy: false,
      sortFields: [{ field: "rank" }],
      onSortChange,
    });
    fireEvent.click(header("rank")!);
    fireEvent.click(header("rank")!);
    expect(onSortChange.mock.calls).toEqual([
      [[{ field: "rank", direction: "ascending" }]],
      [[{ field: "rank", direction: "descending" }]],
    ]);
    expect(shown()).toEqual(["c0000", "c0001", "c0002"]);
  });

  it("lets only the declared sort fields sort a set that is not fully read", () => {
    table({ rows: rows(3), fullyRead: false, busy: false, sortFields: [{ field: "rank" }] });
    expect(header("rank")).not.toBeNull();
    expect(header("code")).toBeNull();
    expect(screen.getByText("code")).toBeDefined();
    cleanup();
    table({ rows: rows(3), fullyRead: true, busy: false, sortFields: [{ field: "rank" }] });
    expect(header("code")).not.toBeNull();
  });

  it("adds a field on a shift click only when the sort can hold more than one", () => {
    const declared = [{ field: "code" as const }, { field: "rank" as const }];
    for (const [sortMaxFields, last] of [
      [1, [{ field: "code", direction: "ascending" }]],
      [
        2,
        [
          { field: "rank", direction: "ascending" },
          { field: "code", direction: "ascending" },
        ],
      ],
    ] as const) {
      const onSortChange = vi.fn();
      table({
        rows: rows(3),
        fullyRead: false,
        busy: false,
        sortFields: declared,
        sortMaxFields,
        onSortChange,
      });
      fireEvent.click(header("rank")!);
      fireEvent.click(header("code")!, { shiftKey: true });
      expect(onSortChange.mock.lastCall).toEqual([last]);
      cleanup();
    }
  });
});
