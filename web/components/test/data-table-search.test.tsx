/**
 * The search box of the DataTable (wamn-vfvx.3).
 *
 * It matches the shown text of the visible columns, in any case, but not json
 * or bytes. It applies with the refine filters, before the sort, and only to a
 * fully read set.
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
  readonly at: string;
  readonly secret: string;
  readonly body: string;
}

const COLUMNS: readonly DataTableColumn<Row>[] = [
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

function table(fullyRead: () => boolean = () => true) {
  render(() => (
    <DataTable
      name="codes"
      columns={COLUMNS}
      rowId="id"
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
      hiddenFields={["secret"]}
    />
  ));
}

/** The codes of the body rows, in the order they show. */
const shown = () =>
  bodyRows()
    .map((row) => row.querySelector("td")?.textContent)
    .filter((code) => ROWS.some((row) => row.code === code));

const search = (value: string) =>
  fireEvent.input(screen.getByLabelText("search"), { target: { value } });

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

  it("is disabled and says why on a set that is not fully read, and keeps its text", () => {
    const [fullyRead, setFullyRead] = createSignal(true);
    table(fullyRead);
    search("alpha");
    setFullyRead(false);
    expect(shown()).toEqual(["Alpha", "beta", "ALPHABET", "gamma"]);
    const box = screen.getByLabelText("search") as HTMLInputElement;
    expect(box.hasAttribute("disabled")).toBe(true);
    expect(box.value).toBe("alpha");
    expect(screen.getByText("Search applies only to a fully read set.")).toBeDefined();
    setFullyRead(true);
    expect(shown()).toEqual(["Alpha", "ALPHABET"]);
  });
});
