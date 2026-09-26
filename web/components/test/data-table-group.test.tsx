/**
 * The grouping, aggregates and totals row of the DataTable (wamn-vfvx.4).
 *
 * The rows nest by the group bar's levels, each group sorts by its value or
 * by an aggregate, and the footer aggregates every kept row. All of it applies
 * only to a fully read set.
 */

import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { afterEach, describe, expect, it } from "vitest";

import { DataTable, type DataTableColumn } from "@wamn/ui";

import { bodyRows, pickChoice, pickMenu, theButton } from "./dom.js";

afterEach(cleanup);

interface Row {
  readonly id: string;
  readonly region: string | null;
  readonly code: string;
  readonly qty: number;
  readonly weight: string;
  readonly ratio: number;
  readonly at: string;
  readonly version: string;
  readonly maker: string | null;
  readonly ref: number | null;
}

const COLUMNS: readonly DataTableColumn<Row>[] = [
  { field: "region", label: "region", type: "text" },
  { field: "code", label: "code", type: "text" },
  { field: "qty", label: "qty", type: "int32" },
  { field: "weight", label: "weight", type: "numeric" },
  { field: "ratio", label: "ratio", type: "float64" },
  { field: "at", label: "at", type: "timestamptz" },
  { field: "version", label: "version", type: "int64", role: "revision" },
  { field: "maker", label: "maker", type: "uuid", role: "reference" },
  { field: "ref", label: "ref", type: "int32", role: "reference" },
  { field: "id", label: "id", type: "uuid", role: "key" },
];

/** 2026-09-21 is a Monday. */
const ROWS: readonly Row[] = [
  { id: "r0", region: "east", code: "a", qty: 5, weight: "0.1", ratio: 0.5, at: "2026-09-21T10:00:00.000000Z", version: "1", maker: "m1", ref: 7 },
  { id: "r1", region: "west", code: "b", qty: 2, weight: "0.25", ratio: 1.5, at: "2026-09-22T10:00:00.000000Z", version: "2", maker: "m1", ref: 8 },
  { id: "r2", region: "east", code: "c", qty: 1, weight: "0.2", ratio: 2.5, at: "2026-09-28T10:00:00.000000Z", version: "3", maker: null, ref: 9 },
  { id: "r3", region: null, code: "d", qty: 10, weight: "1", ratio: 0.25, at: "2026-10-01T10:00:00.000000Z", version: "4", maker: "m2", ref: null },
  { id: "r4", region: "", code: "e", qty: 3, weight: "0.05", ratio: 1, at: "2026-10-05T10:00:00.000000Z", version: "5", maker: "m2", ref: 1 },
];

interface Shape {
  readonly rows?: () => readonly Row[];
  readonly fullyRead?: () => boolean;
  readonly groupedFields?: readonly (keyof Row & string)[];
  readonly weekStart?: number;
}

function table(shape: Shape = {}) {
  render(() => (
    <DataTable
      name="regions"
      columns={COLUMNS}
      rowId={["id"]}
      rows={shape.rows?.() ?? ROWS}
      fullyRead={shape.fullyRead?.() ?? true}
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
      scopeFilters={[]}
      onScopeChange={() => {}}
      groupedFields={shape.groupedFields}
      timeZone="UTC"
      weekStart={shape.weekStart}
    />
  ));
}

const CODES = new Set(ROWS.map((row) => row.code));

/** Each body row: a group as "value (count)", or a row as its code. */
const shown = () =>
  bodyRows().map((row) => {
    const group = row.querySelector('[data-slot="data-table-group-value"]');
    return group === null
      ? (Array.from(row.querySelectorAll("td")).find((cell) => CODES.has(cell.textContent ?? ""))
          ?.textContent ?? "")
      : `${group.textContent} ${group.nextElementSibling?.textContent}`;
  });

const groups = () => shown().filter((text) => text.endsWith(")"));

const press = (name: string) => fireEvent.click(theButton(name));

const total = (field: string) =>
  document.querySelector(`[data-slot="data-table-total"][data-field="${field}"]`)?.textContent;

/** Choose one column's aggregate in its header menu. */
const aggregate = (field: string, name: string) => pickMenu(field, name);

describe("the grouping", () => {
  it("nests two levels in the group bar's order, and empty values form one group", () => {
    table({ groupedFields: ["region", "maker"] });
    expect(shown()).toEqual(["east (2)", "west (1)", "(none) (2)"]);
    press("expand all region");
    expect(groups()).toEqual([
      "east (2)",
      "m1 (1)",
      "(none) (1)",
      "west (1)",
      "m1 (1)",
      "(none) (2)",
      "m2 (2)",
    ]);
    press("move maker out");
    expect(groups().slice(0, 3)).toEqual(["m1 (2)", "m2 (2)", "(none) (1)"]);
  });

  it("expands one group on its own, and each level all at once", () => {
    table({ groupedFields: ["region", "maker"] });
    fireEvent.click(screen.getByText("east").closest("button")!);
    expect(shown()).toEqual(["east (2)", "m1 (1)", "(none) (1)", "west (1)", "(none) (2)"]);
    press("expand all maker");
    expect(shown()).toContain("a");
    expect(shown()).not.toContain("b");
    press("expand all region");
    press("expand all maker");
    expect(shown().filter((text) => !text.endsWith(")"))).toEqual(["a", "c", "b", "d", "e"]);
    press("collapse all region");
    expect(shown()).toEqual(["east (2)", "west (1)", "(none) (2)"]);
  });

  it("keeps a group expanded across a new load", () => {
    const [rows, setRows] = createSignal<readonly Row[]>(ROWS);
    table({ rows, groupedFields: ["region"] });
    fireEvent.click(screen.getByText("east").closest("button")!);
    setRows(ROWS.map((row) => ({ ...row, qty: row.qty + 1 })));
    expect(shown()).toEqual(["east (2)", "a", "c", "west (1)", "(none) (2)"]);
  });

  it("groups a time by day, by week from the week start, and by month", () => {
    table({ groupedFields: ["at"] });
    expect(groups()).toEqual([
      "2026-09-21 (1)",
      "2026-09-22 (1)",
      "2026-09-28 (1)",
      "2026-10-01 (1)",
      "2026-10-05 (1)",
    ]);
    press("week");
    expect(groups()).toEqual(["week of 2026-09-21 (2)", "week of 2026-09-28 (2)", "week of 2026-10-05 (1)"]);
    press("month");
    expect(groups()).toEqual(["2026-09 (3)", "2026-10 (2)"]);
    cleanup();
    table({ groupedFields: ["at"], weekStart: 7 });
    press("week");
    expect(groups()).toEqual(["week of 2026-09-20 (2)", "week of 2026-09-27 (2)", "week of 2026-10-04 (1)"]);
  });

  it("sorts the groups of a level by their value or by an aggregate, and the rows keep the table sort", async () => {
    table({ groupedFields: ["region"] });
    press("region groups ascending");
    expect(groups()).toEqual(["(none) (2)", "west (1)", "east (2)"]);
    press("region groups descending");
    // The trigger's name also holds the choice it shows.
    await pickChoice(/^sort region groups by/, "qty sum");
    expect(groups()).toEqual(["west (1)", "east (2)", "(none) (2)"]);
    press("expand all region");
    press("qty");
    press("qty");
    expect(shown().slice(0, 3)).toEqual(["west (1)", "b", "east (2)"]);
    expect(shown().slice(3, 5)).toEqual(["a", "c"]);
    // A choice opens and picks through Kobalte; on a loaded machine this takes more than 5 s.
  }, 20_000);

  it("does not group json or bytes, and is disabled and says why on a set that is not fully read", () => {
    const [fullyRead, setFullyRead] = createSignal(true);
    table({ fullyRead, groupedFields: ["region"] });
    setFullyRead(false);
    expect(document.querySelector('[data-slot="data-table-group-value"]')).toBeNull();
    expect(screen.getByText("Grouping applies only to a fully read set.")).toBeDefined();
    expect(theButton("remove group region").hasAttribute("disabled")).toBe(true);
    expect(total("qty")).toBeUndefined();
    setFullyRead(true);
    expect(groups()).toEqual(["east (2)", "west (1)", "(none) (2)"]);
  });
});

describe("the aggregates and the totals row", () => {
  it("default by type and role, and every level of a group shows them", () => {
    table({ groupedFields: ["region"] });
    expect(total("qty")).toBe("sum 21");
    expect(total("weight")).toBe("sum 1.60");
    expect(total("ratio")).toBe("sum 5.75");
    expect(total("at")).toBe("max 2026-10-05T10:00:00.000000Z");
    expect(total("version")).toBe("count 5");
    expect(total("maker")).toBe("count 4");
    expect(total("ref")).toBe("count 4");
    expect(total("id")).toBe("count 5");
    expect(total("code")).toBe("count 5");
    const east = screen.getByText("east").closest("tr")!;
    expect(Array.from(east.querySelectorAll("td")).map((cell) => cell.textContent)).toContain("6");
  });

  it("offer each aggregate their type allows", () => {
    table();
    for (const [name, qty, weight, ratio] of [
      ["count", "5", "5", "5"],
      ["min", "1", "0.05", "0.25"],
      ["max", "10", "1.00", "2.5"],
      ["avg", "4.2", "0.3200", "1.15"],
      ["sum", "21", "1.60", "5.75"],
    ] as const) {
      aggregate("qty", name);
      aggregate("weight", name);
      aggregate("ratio", name);
      expect([total("qty"), total("weight"), total("ratio")]).toEqual([
        `${name} ${qty}`,
        `${name} ${weight}`,
        `${name} ${ratio}`,
      ]);
    }
    aggregate("at", "min");
    expect(total("at")).toBe("min 2026-09-21T10:00:00.000000Z");
    // The revision column allows only count, so its menu offers no choice.
    expect(() => pickMenu("version", "count")).toThrow();
    // Sixteen menus open and close; on a loaded machine this takes more than 5 s.
  }, 20_000);

  it("sum and average decimals exactly, rounding an average half away from zero", () => {
    const decimals = (weights: readonly string[]) =>
      weights.map((weight, index) => ({ ...ROWS[0]!, id: `d${index}`, weight }));
    table({ rows: () => decimals(["0.1", "0.2"]) });
    expect(total("weight")).toBe("sum 0.3");
    cleanup();
    const eighth = ["0.01", "0.00", "0.00", "0.00", "0.00", "0.00", "0.00", "0.00"];
    table({ rows: () => decimals(eighth) });
    aggregate("weight", "avg");
    expect(total("weight")).toBe("avg 0.0013");
    cleanup();
    table({ rows: () => decimals(eighth.map((weight) => `-${weight}`)) });
    aggregate("weight", "avg");
    expect(total("weight")).toBe("avg -0.0013");
  });

  it("totals every row the search keeps, whatever the grouping", () => {
    table({ groupedFields: ["region"] });
    fireEvent.input(screen.getByLabelText("search"), { target: { value: "east" } });
    expect(total("qty")).toBe("sum 6");
  });
});
