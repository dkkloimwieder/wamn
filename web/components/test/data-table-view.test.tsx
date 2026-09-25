/**
 * The views of the DataTable, in memory and in the URL (wamn-9v2r.2).
 *
 * A view is a named copy of the table state. A table with a URL key reads its
 * state from the URL when it draws, and writes it back in one canonical form:
 * the parts in a fixed order, and only the parts that differ from the
 * definition. The URL can name only what the table declares.
 */

import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { DataTable, type DataTableColumn } from "@wamn/ui";

import { bodyRows, pickChoice, theButton } from "./dom.js";

interface Row {
  readonly id: string;
  readonly code: string;
  readonly qty: number;
  readonly note: string;
  readonly at: string;
}

const COLUMNS: readonly DataTableColumn<Row>[] = [
  { field: "code", label: "code", type: "text" },
  { field: "qty", label: "qty", type: "int32" },
  { field: "note", label: "note", type: "text" },
  { field: "at", label: "at", type: "timestamptz" },
  { field: "id", label: "id", type: "uuid", role: "key" },
];

const ROWS: readonly Row[] = [
  { id: "r0", code: "a", qty: 2, note: "red", at: "2026-09-21T10:00:00.000000Z" },
  { id: "r1", code: "b", qty: 6, note: "blue", at: "2026-09-22T10:00:00.000000Z" },
  { id: "r2", code: "c", qty: 4, note: "green", at: "2026-09-29T10:00:00.000000Z" },
];

/** Every part of the state, each unlike the definition, in the canonical order. */
const FULL = [
  "lots.order=note,code,qty,at,id",
  "lots.hidden=id",
  "lots.width=code:200",
  "lots.left=note",
  "lots.right=qty",
  "lots.sort=qty:desc",
  "lots.scope.code=a",
  "lots.scope.code=b",
  "lots.filter.qty=range:3,",
  "lots.search=e",
  "lots.group=at:week:value:desc",
  "lots.aggregate=qty:avg",
  "lots.cap=500",
].join("&");

const at = (search: string) => window.history.replaceState(null, "", `/${search}`);
const search = () => decodeURIComponent(window.location.search);

beforeEach(() => at(""));
afterEach(cleanup);

function table(
  shape: { fullyRead?: () => boolean; urlKey?: string; onScopeChange?: (filters: unknown) => void } = {},
) {
  const [cap, setCap] = createSignal(1000);
  const onScopeChange = vi.fn(shape.onScopeChange ?? (() => {}));
  render(() => (
    <DataTable
      name="lots"
      columns={COLUMNS}
      rowId="id"
      rows={ROWS}
      fullyRead={shape.fullyRead?.() ?? true}
      busy={false}
      cap={cap()}
      onCapChange={setCap}
      refusal={null}
      onRefresh={() => {}}
      startedAt={null}
      endedAt={null}
      sortFields={[]}
      sortMaxFields={1}
      onSortChange={() => {}}
      scopeFilters={["code"]}
      onScopeChange={onScopeChange}
      urlKey={shape.urlKey}
    />
  ));
  return { cap, onScopeChange };
}

/** The labels of the column headers, in the order they show. */
const headers = () =>
  Array.from(document.querySelectorAll("thead th [aria-label^='menu ']")).map((menu) =>
    menu.getAttribute("aria-label")!.slice(5),
  );

/** The codes of the data rows, in the order they show. */
const codes = () =>
  bodyRows()
    .map((row) => Array.from(row.querySelectorAll("td")).find((cell) => /^[abc]$/.test(cell.textContent ?? "")))
    .flatMap((cell) => (cell === undefined ? [] : [cell.textContent]));

const press = (name: string) => fireEvent.click(theButton(name));

function saveView(name: string) {
  fireEvent.input(screen.getByLabelText("view name"), { target: { value: name } });
  press("save view");
}

describe("the URL", () => {
  it("sets every part of the state when the table draws, and writes it back the same", () => {
    at(`?${FULL}`);
    const { cap, onScopeChange } = table({ urlKey: "lots" });
    expect(search()).toBe(`?${FULL}`);
    // Grouping puts the grouped column first after the pinned one, as the table shows it.
    expect(headers()).toEqual(["note", "at", "code", "qty"]);
    expect(theButton("menu note").closest("th")?.getAttribute("data-pinned")).toBe("start");
    expect(theButton("menu qty").closest("th")?.getAttribute("data-pinned")).toBe("end");
    expect(document.querySelector("table")?.style.getPropertyValue("--col-code-size")).toBe("200");
    expect((screen.getByLabelText("search") as HTMLInputElement).value).toBe("e");
    expect(theButton("remove filter qty")).toBeDefined();
    expect(document.querySelector('[data-slot="data-table-total"][data-field="qty"]')?.textContent).toBe("avg 5");
    expect(onScopeChange).toHaveBeenLastCalledWith([{ field: "code", values: ["a", "b"] }]);
    expect(cap()).toBe(500);
  });

  it("writes the parts in the fixed order, only those that differ, and keeps other keys", () => {
    at("?other=1&lots.cap=500&lots.hidden=&lots.sort=qty:desc&lots.search=");
    table({ urlKey: "lots" });
    expect(search()).toBe("?other=1&lots.sort=qty:desc&lots.cap=500");
    press("qty");
    expect(search()).toBe("?other=1&lots.sort=qty:asc&lots.cap=500");
  });

  it("ignores what the table does not declare or cannot read, and says so once", () => {
    at("?lots.bogus=1&lots.sort=nope:asc&lots.cap=500&lots.scope.qty=3&lots.filter.code=weird:x&lots.group=note:day:value:asc");
    const { cap } = table({ urlKey: "lots" });
    expect(cap()).toBe(500);
    expect(search()).toBe("?lots.cap=500");
    const report = document.querySelectorAll('[data-slot="data-table-url-ignored"]');
    expect(report.length).toBe(1);
    expect(report[0]?.textContent).toContain(
      "lots.bogus=1, lots.sort=nope:asc, lots.scope.qty=3, lots.filter.code=weird:x, lots.group=note:day:value:asc",
    );
  });

  it("leaves the URL alone for a table without a URL key", () => {
    at("?lots.cap=500");
    const { cap } = table();
    press("qty");
    expect(cap()).toBe(1000);
    expect(search()).toBe("?lots.cap=500");
  });
});

describe("the views", () => {
  it("keep every part of the state, and reset applies the definition", async () => {
    at(`?${FULL}`);
    const { cap } = table({ urlKey: "lots" });
    saveView("wide");
    press("reset view");
    expect(search()).toBe("");
    expect(headers()).toEqual(["code", "qty", "note", "at", "id"]);
    expect(cap()).toBe(1000);
    expect(codes()).toEqual(["a", "b", "c"]);
    await pickChoice(/^view/, "wide");
    expect(search()).toBe(`?${FULL}`);
    expect(cap()).toBe(500);
  }, 20_000);

  it("are renamed and deleted", async () => {
    table();
    saveView("first");
    fireEvent.input(screen.getByLabelText("view name"), { target: { value: "second" } });
    press("rename");
    await pickChoice(/^view/, "second");
    press("delete");
    expect(screen.queryByRole("option", { name: "second" })).toBeNull();
    expect(theButton("delete").hasAttribute("disabled")).toBe(true);
  }, 20_000);

  it("apply the scope and the cap on a set that is not fully read, and the client parts wait", () => {
    const [fullyRead, setFullyRead] = createSignal(false);
    at("?lots.filter.qty=range:3,&lots.scope.code=a&lots.cap=500");
    const { cap, onScopeChange } = table({ fullyRead, urlKey: "lots" });
    expect(cap()).toBe(500);
    expect(onScopeChange).toHaveBeenLastCalledWith([{ field: "code", values: ["a"] }]);
    expect(codes()).toEqual(["a", "b", "c"]);
    setFullyRead(true);
    expect(codes()).toEqual(["b", "c"]);
  });
});
