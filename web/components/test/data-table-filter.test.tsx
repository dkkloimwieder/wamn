/**
 * The refine filters of the DataTable (wamn-vfvx.2).
 *
 * Each column type gets its filter, every type can be empty or not empty, and
 * the filters apply only to a fully read set, before the sort.
 */

import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { afterEach, describe, expect, it } from "vitest";

import { DataTable, type DataTableColumn } from "@wamn/ui";

import { bodyRows, theButton } from "./dom.js";

afterEach(cleanup);

interface Row {
  readonly id: string;
  readonly code: string;
  readonly rank: number;
  readonly weight: string;
  readonly ratio: number;
  readonly at: string;
  readonly flag: boolean;
  readonly version: string;
  readonly note: string | null;
  readonly body: string | null;
}

const COLUMNS: readonly DataTableColumn<Row>[] = [
  { field: "code", label: "code", type: "text" },
  { field: "rank", label: "rank", type: "int32" },
  { field: "weight", label: "weight", type: "numeric" },
  { field: "ratio", label: "ratio", type: "float64" },
  { field: "at", label: "at", type: "timestamptz" },
  { field: "flag", label: "flag", type: "boolean" },
  { field: "id", label: "id", type: "uuid" },
  { field: "version", label: "version", type: "int64" },
  { field: "note", label: "note", type: "text" },
  { field: "body", label: "body", type: "json" },
];

const uuid = (index: number) => `00000000-0000-4000-8000-${String(index).padStart(12, "0")}`;

/** Four rows. The ranks fall as the codes rise, and the days rise with them. */
const ROWS: Row[] = [0, 1, 2, 3].map((index) => ({
  id: uuid(index),
  code: ["Alpha", "beta", "ALPHABET", "gamma"][index]!,
  rank: 4 - index,
  weight: ["1.50", "2.25", "10.00", "0.75"][index]!,
  ratio: [0.5, 1.5, 2.5, 3.5][index]!,
  at: `2026-09-${String(10 + index * 5).padStart(2, "0")}T12:00:00.000000Z`,
  flag: index % 2 === 0,
  version: ["1", "20", "300", "9007199254740993"][index]!,
  note: index === 1 ? null : `n${index}`,
  body: index === 3 ? null : "{}",
}));

function table(fullyRead: () => boolean = () => true) {
  render(() => (
    <DataTable
      name="rows"
      columns={COLUMNS}
      rowId={["id"]}
      rows={ROWS}
      fullyRead={fullyRead()}
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
    />
  ));
}

/** The codes of the body rows, in the order they show. */
const shown = () =>
  bodyRows()
    .map((row) => row.querySelector("td")?.textContent)
    .filter((code) => ROWS.some((row) => row.code === code));

const open = (label: string) =>
  fireEvent.click(theButton(`filter ${label}`));

const type = (label: string, value: string) =>
  fireEvent.input(screen.getByLabelText(label), { target: { value } });

const press = (name: string) => fireEvent.click(theButton(name));

/** Open one column's filter, run `act`, and return the codes shown. */
function filtered(column: string, act: () => void) {
  table();
  open(column);
  act();
  return shown();
}

describe("the refine filters", () => {
  it("keep text that contains the value, in any case", () => {
    expect(filtered("code", () => type("contains", "alpha"))).toEqual(["Alpha", "ALPHABET"]);
  });

  it("keep int32, numeric and float64 values between a minimum and a maximum", () => {
    expect(filtered("rank", () => type("min", "2"))).toEqual(["Alpha", "beta", "ALPHABET"]);
    cleanup();
    expect(
      filtered("weight", () => {
        type("min", "1");
        type("max", "3");
      }),
    ).toEqual(["Alpha", "beta"]);
    cleanup();
    expect(filtered("ratio", () => type("max", "1.5"))).toEqual(["Alpha", "beta"]);
  });

  it("keep times between a start and an end", () => {
    expect(
      filtered("at", () => {
        type("from", "2026-09-13T00:00");
        type("to", "2026-09-22T00:00");
      }),
    ).toEqual(["beta", "ALPHABET"]);
  });

  it("keep booleans that are true, or false", () => {
    expect(filtered("flag", () => press("true"))).toEqual(["Alpha", "ALPHABET"]);
    press("false");
    expect(shown()).toEqual(["beta", "gamma"]);
  });

  it("keep a uuid or an int64 that equals the value", () => {
    expect(filtered("id", () => type("equals", uuid(2).toUpperCase()))).toEqual(["ALPHABET"]);
    cleanup();
    expect(filtered("version", () => type("equals", "9007199254740993"))).toEqual(["gamma"]);
  });

  it("keep empty or not empty values of every type, and json offers only those", () => {
    expect(filtered("note", () => press("is empty"))).toEqual(["beta"]);
    press("is not empty");
    expect(shown()).toEqual(["Alpha", "ALPHABET", "gamma"]);
    cleanup();
    expect(filtered("body", () => press("is empty"))).toEqual(["gamma"]);
    // The open filter holds no value box. The toolbar has its own boxes.
    expect(document.querySelector('[data-slot="popover-content"] input')).toBeNull();
  });

  it("show a chip that names the column and the value, and clear all removes them", () => {
    table();
    open("code");
    type("contains", "alpha");
    open("rank");
    type("min", "3");
    expect(screen.getByText('code contains "alpha"')).toBeDefined();
    expect(screen.getByText("rank at least 3")).toBeDefined();
    expect(shown()).toEqual(["Alpha"]);
    press("clear all");
    expect(screen.queryByText('code contains "alpha"')).toBeNull();
    expect(shown()).toEqual(["Alpha", "beta", "ALPHABET", "gamma"]);
  });

  it("run before the sort", () => {
    table();
    open("rank");
    type("min", "2");
    press("rank");
    expect(shown()).toEqual(["ALPHABET", "beta", "Alpha"]);
  });

  it("are disabled and say why on a set that is not fully read, and keep their state", () => {
    const [fullyRead, setFullyRead] = createSignal(true);
    table(fullyRead);
    open("code");
    type("contains", "alpha");
    expect(shown()).toEqual(["Alpha", "ALPHABET"]);
    setFullyRead(false);
    expect(shown()).toEqual(["Alpha", "beta", "ALPHABET", "gamma"]);
    // The load that lands closes an open filter.
    open("code");
    expect(screen.getByLabelText("contains").hasAttribute("disabled")).toBe(true);
    expect(theButton("remove filter code").hasAttribute("disabled")).toBe(
      true,
    );
    expect(theButton("clear all").hasAttribute("disabled")).toBe(true);
    expect(screen.getAllByText(/Filters apply only to a fully read set/).length).toBeGreaterThan(0);
    setFullyRead(true);
    expect(shown()).toEqual(["Alpha", "ALPHABET"]);
  });
});
