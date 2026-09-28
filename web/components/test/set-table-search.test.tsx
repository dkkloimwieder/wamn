/**
 * The search box of a SetTable (wamn-vfvx.3).
 *
 * It matches the shown text of the visible columns, in any case, but not json
 * or bytes. It applies with the refine filters, before the sort. A QueryTable
 * shows it only over a fully read set, and keeps its text while it is not.
 */

import { cleanup, fireEvent, screen } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { afterEach, describe, expect, it } from "vitest";

import type { TableColumn } from "@wamn/ui";

import { bodyRows, theButton } from "./dom.js";
import { queryTable, setTable, settled } from "./tables.js";

afterEach(cleanup);

interface Row {
  readonly id: string;
  readonly code: string;
  readonly rank: number;
  readonly at: string;
  readonly secret: string;
  readonly body: string;
}

const COLUMNS: readonly TableColumn<Row>[] = [
  { field: "code", label: "code", type: "text" },
  { field: "rank", label: "rank", type: "int32" },
  { field: "at", label: "at", type: "timestamptz" },
  { field: "secret", label: "secret", type: "text" },
  { field: "body", label: "body", type: "json" },
];

/** Four rows. The ranks fall as the codes rise. */
const ROWS: Row[] = [0, 1, 2, 3].map((index) => ({
  id: `r${index}`,
  code: ["Alpha", "beta", "ALPHABET", "gamma"][index]!,
  rank: 4 - index,
  at: `2026-09-${String(10 + index * 5).padStart(2, "0")}T12:00:00.000000Z`,
  secret: index === 1 ? "hidden-zzz" : "",
  body: index === 3 ? '{"tag":"qqq"}' : "{}",
}));

const table = () => setTable({ columns: COLUMNS, rows: ROWS, hiddenFields: ["secret"] });

/** The codes of the body rows, in the order they show. */
const shown = () =>
  bodyRows()
    .map((row) => row.querySelector("td")?.textContent)
    .filter((code) => ROWS.some((row) => row.code === code));

const search = (value: string) => fireEvent.input(screen.getByLabelText("search"), { target: { value } });

describe("the search", () => {
  it("matches the shown text of a cell, in any case", () => {
    table();
    search("alpha");
    expect(shown()).toEqual(["Alpha", "ALPHABET"]);
    search("2026-09-15T12");
    expect(shown()).toEqual(["beta"]);
    search("4");
    expect(shown()).toEqual(["Alpha"]);
    search("");
    expect(shown()).toEqual(["Alpha", "beta", "ALPHABET", "gamma"]);
  });

  it("does not search a hidden column, or a json column", () => {
    table();
    expect(screen.queryByText("secret")).toBeNull();
    search("zzz");
    expect(shown()).toEqual([]);
    expect(screen.getByText("No row matches the search and filters.")).toBeDefined();
    search("qqq");
    expect(shown()).toEqual([]);
  });

  it("keeps only the rows that match both the search and a refine filter", () => {
    table();
    search("alpha");
    fireEvent.click(theButton("filter rank"));
    fireEvent.input(screen.getByLabelText("min"), { target: { value: "3" } });
    expect(shown()).toEqual(["Alpha"]);
  });

  it("runs before the sort", () => {
    table();
    search("a");
    fireEvent.click(theButton("rank"));
    expect(shown()).toEqual(["gamma", "ALPHABET", "beta", "Alpha"]);
    search("alpha");
    expect(shown()).toEqual(["ALPHABET", "Alpha"]);
  });

  it("leaves a set that is not fully read, and comes back with its text when it is", async () => {
    const [more, setMore] = createSignal(false);
    await queryTable({ columns: COLUMNS, rows: () => ROWS, more, hiddenFields: ["secret"] });
    search("alpha");
    setMore(true);
    fireEvent.click(theButton("refresh"));
    await settled();
    expect(shown()).toEqual(["Alpha", "beta", "ALPHABET", "gamma"]);
    expect(screen.queryByLabelText("search")).toBeNull();
    setMore(false);
    fireEvent.click(theButton("refresh"));
    await settled();
    expect((screen.getByLabelText("search") as HTMLInputElement).value).toBe("alpha");
    expect(shown()).toEqual(["Alpha", "ALPHABET"]);
  });
});
