/**
 * The column arrangement and the scope bar of the DataTable (wamn-9v2r.1).
 *
 * The header menu sorts, hides and pins a column. The column panel shows and
 * hides columns and orders them. A header edge drag sets a width, which the
 * table state keeps. The scope bar calls back with the scope filters and
 * applies none in the table.
 */

import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vitest";

import { DataTable, type DataTableColumn, type DataTableScopeFilter } from "@wamn/ui";

import { bodyRows, pickMenu, theButton } from "./dom.js";

afterEach(cleanup);

interface Row {
  readonly id: string;
  readonly code: string;
  readonly qty: number;
  readonly note: string;
}

const COLUMNS: readonly DataTableColumn<Row>[] = [
  { field: "code", label: "code", type: "text" },
  { field: "qty", label: "qty", type: "int32" },
  { field: "note", label: "note", type: "text" },
];

const ROWS: readonly Row[] = [
  { id: "r0", code: "a", qty: 2, note: "red" },
  { id: "r1", code: "b", qty: 3, note: "blue" },
  { id: "r2", code: "c", qty: 1, note: "green" },
];

function table(
  shape: {
    rows?: () => readonly Row[];
    onScopeChange?: (filters: readonly DataTableScopeFilter<keyof Row & string>[]) => void;
  } = {},
) {
  render(() => (
    <DataTable
      name="rows"
      columns={COLUMNS}
      rowId={["id"]}
      rows={shape.rows?.() ?? ROWS}
      fullyRead={true}
      busy={false}
      cap={1000}
      onCapChange={() => {}}
      refusal={null}
      onRefresh={() => {}}
      startedAt={null}
      endedAt={null}
      sortFields={[]}
      sortMaxFields={1}
      onSortChange={() => {}}
      scopeFilters={["code", "qty"]}
      onScopeChange={shape.onScopeChange ?? (() => {})}
    />
  ));
}

/** The labels of the column headers, in the order they show. */
const headers = () =>
  Array.from(document.querySelectorAll("thead th"))
    .map((cell) => cell.querySelector('[aria-label^="menu "]')?.getAttribute("aria-label")?.slice(5))
    .filter((label) => label !== undefined);

/** The codes of the body rows, in the order they show. */
const codes = () =>
  bodyRows()
    .map((row) => Array.from(row.querySelectorAll("td")).find((cell) => /^[abc]$/.test(cell.textContent ?? "")))
    .map((cell) => cell?.textContent)
    .filter((code) => code !== undefined);

/** The pin side of a column's header cell, or null. */
const pinned = (label: string) =>
  theButton(`menu ${label}`).closest("th")?.getAttribute("data-pinned") ?? null;

const press = (name: string) => fireEvent.click(theButton(name));

describe("the header menu", () => {
  it("sorts a column ascending and descending", () => {
    table();
    pickMenu("qty", "sort ascending");
    expect(codes()).toEqual(["c", "a", "b"]);
    pickMenu("qty", "sort descending");
    expect(codes()).toEqual(["b", "a", "c"]);
  });

  it("hides a column", () => {
    table();
    pickMenu("note", "hide");
    expect(headers()).toEqual(["code", "qty"]);
  });

  it("pins a column left or right, and unpins it", () => {
    table();
    pickMenu("note", "pin left");
    expect(pinned("note")).toBe("start");
    expect(headers()[0]).toBe("note");
    pickMenu("code", "pin right");
    expect(pinned("code")).toBe("end");
    expect(headers().at(-1)).toBe("code");
    pickMenu("note", "unpin");
    expect(pinned("note")).toBeNull();
    expect(headers()).toEqual(["qty", "note", "code"]);
  });
});

describe("the column panel", () => {
  it("orders the columns by arrows and by a drag, and resets to the definition's order", () => {
    table();
    press("columns");
    press("move code down");
    expect(headers()).toEqual(["qty", "code", "note"]);
    press("move note up");
    expect(headers()).toEqual(["qty", "note", "code"]);
    const item = (id: string) => document.querySelector(`[data-column="${id}"]`)!;
    fireEvent.dragStart(item("code"));
    fireEvent.drop(item("qty"));
    expect(headers()).toEqual(["code", "qty", "note"]);
    press("reset order");
    expect(headers()).toEqual(["code", "qty", "note"]);
    press("move qty down");
    press("reset order");
    expect(headers()).toEqual(["code", "qty", "note"]);
  });

  it("hides and shows a column by its switch, and shows every column", () => {
    table();
    press("columns");
    const toggle = (id: string) => fireEvent.click(document.querySelector(`[data-column="${id}"] input`)!);
    toggle("qty");
    expect(headers()).toEqual(["code", "note"]);
    toggle("note");
    expect(headers()).toEqual(["code"]);
    press("show all");
    expect(headers()).toEqual(["code", "qty", "note"]);
  });
});

describe("the column width", () => {
  it("follows a header edge drag, and a new load keeps it", () => {
    const [rows, setRows] = createSignal<readonly Row[]>(ROWS);
    table({ rows });
    const cell = theButton("menu qty").closest("th")!;
    const width = () => cell.closest("table")!.style.getPropertyValue("--col-qty-size");
    const before = width();
    expect(before).not.toBe("");
    const handle = cell.querySelector(".cursor-col-resize")!;
    fireEvent.mouseDown(handle, { button: 0, clientX: 100 });
    fireEvent.mouseMove(document, { clientX: 160 });
    fireEvent.mouseUp(document, { clientX: 160 });
    const after = width();
    expect(after).not.toBe(before);
    setRows(ROWS.map((row) => ({ ...row, qty: row.qty + 1 })));
    expect(width()).toBe(after);
  });
});

describe("the search over the visible columns", () => {
  it("filters again when a column is hidden or shown", () => {
    table();
    fireEvent.input(screen.getByLabelText("search"), { target: { value: "blue" } });
    expect(codes()).toEqual(["b"]);
    pickMenu("note", "hide");
    expect(codes()).toEqual([]);
    press("columns");
    press("show all");
    expect(codes()).toEqual(["b"]);
  });
});

describe("the scope bar", () => {
  it("calls back with every scope filter that holds a value, and filters no row in the table", () => {
    const onScopeChange = vi.fn();
    table({ onScopeChange });
    const add = (label: string, value: string) => {
      const input = screen.getByLabelText(label, { selector: "input:not([type=search])" }) as HTMLInputElement;
      input.value = value;
      fireEvent.keyDown(input, { key: "Enter" });
    };
    add("code", "a");
    expect(onScopeChange.mock.lastCall).toEqual([[{ field: "code", values: ["a"] }]]);
    add("code", "b");
    add("qty", "3");
    expect(onScopeChange.mock.lastCall).toEqual([
      [
        { field: "code", values: ["a", "b"] },
        { field: "qty", values: ["3"] },
      ],
    ]);
    expect(codes()).toEqual(["a", "b", "c"]);
    press("remove code a");
    press("remove code b");
    expect(onScopeChange.mock.lastCall).toEqual([[{ field: "qty", values: ["3"] }]]);
    expect(onScopeChange).toHaveBeenCalledTimes(5);
  });

  it("shows the current sort as a chip", () => {
    table();
    expect(document.querySelector('[data-slot="data-table-scope-sort"]')).toBeNull();
    press("qty");
    expect(document.querySelector('[data-slot="data-table-scope-sort"]')?.textContent).toBe("sortqty ascending");
  });
});
