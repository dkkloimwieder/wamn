/**
 * The column arrangement and the scope bar of a QueryTable (wamn-9v2r.1).
 *
 * The header menu sorts, hides and pins a column. The column panel shows and
 * hides columns and orders them. A header edge drag sets a width, which the
 * table state keeps. The scope bar asks the read for the scope filters, and
 * the table applies none itself.
 */

import { cleanup, fireEvent, screen } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { afterEach, describe, expect, it } from "vitest";

import type { TableColumn } from "@wamn/ui";

import { bodyRows, pickMenu, theButton } from "./dom.js";
import { queryTable, settled } from "./tables.js";

afterEach(cleanup);

interface Row {
  readonly id: string;
  readonly code: string;
  readonly qty: number;
  readonly note: string;
}

const COLUMNS: readonly TableColumn<Row>[] = [
  { field: "code", label: "code", type: "text" },
  { field: "qty", label: "qty", type: "int32" },
  { field: "note", label: "note", type: "text" },
];

const ROWS: readonly Row[] = [
  { id: "r0", code: "a", qty: 2, note: "red" },
  { id: "r1", code: "b", qty: 3, note: "blue" },
  { id: "r2", code: "c", qty: 1, note: "green" },
];

const table = (rows: () => readonly Row[] = () => ROWS) =>
  queryTable({ columns: COLUMNS, rows, scopeFilters: ["code", "qty"] });

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
const pinned = (label: string) => theButton(`menu ${label}`).closest("th")?.getAttribute("data-pinned") ?? null;

const press = (name: string) => fireEvent.click(theButton(name));

describe("the header menu", () => {
  it("sorts a column ascending and descending", async () => {
    await table();
    pickMenu("qty", "sort ascending");
    expect(codes()).toEqual(["c", "a", "b"]);
    pickMenu("qty", "sort descending");
    expect(codes()).toEqual(["b", "a", "c"]);
  });

  it("hides a column", async () => {
    await table();
    pickMenu("note", "hide");
    expect(headers()).toEqual(["code", "qty"]);
  });

  it("pins a column left or right, and unpins it", async () => {
    await table();
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
  it("orders the columns by arrows and by a drag, and resets to the definition's order", async () => {
    await table();
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

  it("hides and shows a column by its switch, and shows every column", async () => {
    await table();
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
  it("follows a header edge drag, and a new load keeps it", async () => {
    const [rows, setRows] = createSignal<readonly Row[]>(ROWS);
    await table(rows);
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
    press("refresh");
    await settled();
    expect(width()).toBe(after);
  });
});

describe("the search over the visible columns", () => {
  it("filters again when a column is hidden or shown", async () => {
    await table();
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
  it("asks the read for every scope filter that holds a value, and filters no row in the table", async () => {
    const { asked } = await table();
    const add = async (label: string, value: string) => {
      const input = screen.getByLabelText(label, { selector: "input:not([type=search])" }) as HTMLInputElement;
      input.value = value;
      fireEvent.keyDown(input, { key: "Enter" });
      await settled();
    };
    await add("code", "a");
    expect(asked.at(-1)?.filter).toEqual({ code: ["a"] });
    expect(codes()).toEqual(["a"]);
    await add("code", "b");
    await add("qty", "3");
    expect(asked.at(-1)?.filter).toEqual({ code: ["a", "b"], qty: ["3"] });
    // The read keeps the rows, and the table shows each one it returned.
    expect(codes()).toEqual(["b"]);
    press("remove code a");
    await settled();
    press("remove code b");
    await settled();
    expect(asked.at(-1)?.filter).toEqual({ qty: ["3"] });
    // The first load, and one for each of the five changes.
    expect(asked).toHaveLength(6);
  });

  it("shows the current sort as a chip", async () => {
    await table();
    expect(document.querySelector('[data-slot="table-scope-sort"]')).toBeNull();
    press("qty");
    expect(document.querySelector('[data-slot="table-scope-sort"]')?.textContent).toBe("sortqty ascending");
  });
});
