/**
 * The refine filters of a SetTable (wamn-vfvx.2).
 *
 * Each column type gets its filter, every type can be empty or not empty, and
 * the filters apply before the sort. A QueryTable shows them only over a fully
 * read set, and keeps them while it is not.
 */

import { cleanup, fireEvent, screen } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { afterEach, describe, expect, it } from "vitest";

import type { TableColumn } from "@wamn/ui";

import { bodyRows, button, theButton } from "./dom.js";
import { queryTable, setTable, settled } from "./tables.js";

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

const COLUMNS: readonly TableColumn<Row>[] = [
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

const table = () => setTable({ columns: COLUMNS, rows: ROWS });

/** The codes of the body rows, in the order they show. */
const shown = () =>
  bodyRows()
    .map((row) => row.querySelector("td")?.textContent)
    .filter((code) => ROWS.some((row) => row.code === code));

const open = (label: string) => fireEvent.click(theButton(`filter ${label}`));

const type = (label: string, value: string) => fireEvent.input(screen.getByLabelText(label), { target: { value } });

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

  it("leave a set that is not fully read, and come back with their state when it is", async () => {
    const [more, setMore] = createSignal(false);
    await queryTable({ columns: COLUMNS, rows: () => ROWS, more });
    open("code");
    type("contains", "alpha");
    expect(shown()).toEqual(["Alpha", "ALPHABET"]);
    setMore(true);
    press("refresh");
    await settled();
    expect(shown()).toEqual(["Alpha", "beta", "ALPHABET", "gamma"]);
    expect(button("filter code")).toBeNull();
    expect(button("remove filter code")).toBeNull();
    setMore(false);
    press("refresh");
    await settled();
    expect(shown()).toEqual(["Alpha", "ALPHABET"]);
    expect(button("remove filter code")).not.toBeNull();
  });
});
