/**
 * The views of a QueryTable, in memory and in the URL (wamn-9v2r.2).
 *
 * A view is a named copy of the table state. A table with a URL key reads its
 * state from the URL when it draws, and writes it back in one canonical form:
 * the parts in a fixed order, and only the parts that differ from the
 * definition. The URL can name only what the table declares.
 */

import { cleanup, fireEvent, screen } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { TableColumn } from "@wamn/ui";

import { bodyRows, pickChoice, theButton } from "./dom.js";
import { painted, queryTable, settled } from "./tables.js";

interface Row {
  readonly id: string;
  readonly code: string;
  readonly qty: number;
  readonly note: string;
  readonly at: string;
}

const COLUMNS: readonly TableColumn<Row>[] = [
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

/** The table, whose read takes the code as its one scope filter. */
async function table(shape: { more?: () => boolean; urlKey?: string } = {}) {
  const { asked } = await queryTable({
    name: "lots",
    columns: COLUMNS,
    rows: () => ROWS,
    scopeFilters: ["code"],
    ...(shape.more === undefined ? {} : { more: shape.more }),
    ...(shape.urlKey === undefined ? {} : { urlKey: shape.urlKey }),
  });
  /** The cap of the last load, and its scope. */
  const last = () => asked.at(-1)!;
  return { asked, last };
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
  it("sets every part of the state before the first load, and writes it back the same", async () => {
    at(`?${FULL}`);
    const { asked } = await table({ urlKey: "lots" });
    expect(search()).toBe(`?${FULL}`);
    // Grouping puts the grouped column first after the pinned one, as the table shows it.
    expect(headers()).toEqual(["note", "at", "code", "qty"]);
    expect(theButton("menu note").closest("th")?.getAttribute("data-pinned")).toBe("start");
    expect(theButton("menu qty").closest("th")?.getAttribute("data-pinned")).toBe("end");
    expect(document.querySelector("table")?.style.getPropertyValue("--col-code-size")).toBe("200");
    expect((screen.getByLabelText("search") as HTMLInputElement).value).toBe("e");
    expect(theButton("remove filter qty")).toBeDefined();
    // The read keeps codes a and b, and the refine filter keeps a quantity of at least 3: b alone.
    await painted();
    expect(document.querySelector('[data-slot="table-total"][data-field="qty"]')?.textContent).toBe("avg 6");
    // One load, with the scope and the cap of the URL.
    expect(asked).toEqual([{ limit: 500, filter: { code: ["a", "b"] } }]);
  });

  it("writes the parts in the fixed order, only those that differ, and keeps other keys", async () => {
    at("?other=1&lots.cap=500&lots.hidden=&lots.sort=qty:desc&lots.search=");
    await table({ urlKey: "lots" });
    expect(search()).toBe("?other=1&lots.sort=qty:desc&lots.cap=500");
    press("qty");
    expect(search()).toBe("?other=1&lots.sort=qty:asc&lots.cap=500");
  });

  it("ignores what the table does not declare or cannot read, and says so once", async () => {
    at(
      "?lots.bogus=1&lots.sort=nope:asc&lots.cap=500&lots.scope.qty=3&lots.filter.code=weird:x&lots.group=note:day:value:asc",
    );
    const { last } = await table({ urlKey: "lots" });
    expect(last().limit).toBe(500);
    expect(search()).toBe("?lots.cap=500");
    const report = document.querySelectorAll('[data-slot="table-url-ignored"]');
    expect(report.length).toBe(1);
    expect(report[0]?.textContent).toContain(
      "lots.bogus=1, lots.sort=nope:asc, lots.scope.qty=3, lots.filter.code=weird:x, lots.group=note:day:value:asc",
    );
  });

  it("leaves the URL alone for a table without a URL key", async () => {
    at("?lots.cap=500");
    const { last } = await table();
    press("qty");
    expect(last().limit).toBe(1000);
    expect(search()).toBe("?lots.cap=500");
  });
});

describe("the views", () => {
  it("keep every part of the state, and reset applies the definition", async () => {
    at(`?${FULL}`);
    const { last } = await table({ urlKey: "lots" });
    saveView("wide");
    press("reset view");
    await settled();
    expect(search()).toBe("");
    expect(headers()).toEqual(["code", "qty", "note", "at", "id"]);
    expect(last()).toEqual({ limit: 1000 });
    expect(codes()).toEqual(["a", "b", "c"]);
    await pickChoice(/^view/, "wide");
    await settled();
    expect(search()).toBe(`?${FULL}`);
    expect(last()).toEqual({ limit: 500, filter: { code: ["a", "b"] } });
  }, 20_000);

  it("are renamed and deleted", async () => {
    await table();
    saveView("first");
    fireEvent.input(screen.getByLabelText("view name"), { target: { value: "second" } });
    press("rename");
    await pickChoice(/^view/, "second");
    press("delete");
    expect(screen.queryByRole("option", { name: "second" })).toBeNull();
    expect(theButton("delete").hasAttribute("disabled")).toBe(true);
  }, 20_000);

  it("apply the scope and the cap on a set that is not fully read, and the set parts wait", async () => {
    const [more, setMore] = createSignal(true);
    at("?lots.filter.qty=range:3,&lots.scope.code=a&lots.scope.code=b&lots.cap=500");
    const { last } = await table({ more, urlKey: "lots" });
    expect(last()).toEqual({ limit: 500, filter: { code: ["a", "b"] } });
    expect(codes()).toEqual(["a", "b"]);
    setMore(false);
    press("refresh");
    await settled();
    expect(codes()).toEqual(["b"]);
    // The refine filter waited in the URL the whole time.
    expect(search()).toContain("lots.filter.qty=range:3,");
  });
});
