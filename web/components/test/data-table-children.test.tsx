/**
 * Child tables of the DataTable (wamn-8iul.6).
 *
 * A child renders in the expanded area of its parent row, scoped by the row's
 * id alone. It mounts, and so loads, on the first expand, keeps its rows
 * across a collapse, and ends when a load drops its row.
 */

import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { createSignal, onMount } from "solid-js";
import { afterEach, describe, expect, it } from "vitest";

import { DataTable, type DataTableColumn } from "@wamn/ui";

import { button, theButton } from "./dom.js";

afterEach(cleanup);

interface Row {
  readonly id: string;
  readonly code: string;
}

const COLUMNS: readonly DataTableColumn<Row>[] = [{ field: "code", label: "code", type: "text" }];

const ROWS: readonly Row[] = [
  { id: "r0", code: "a" },
  { id: "r1", code: "b" },
];

function table(rows: () => readonly Row[] = () => ROWS) {
  const mounts: string[] = [];
  render(() => (
    <DataTable
      name="rows"
      columns={COLUMNS}
      rowId="id"
      rows={rows()}
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
      scopeFilters={[]}
      onScopeChange={() => {}}
      childTables={[
        {
          label: "lines",
          render: (value) => {
            // A child's load starts when it mounts.
            onMount(() => mounts.push(value));
            return <p>lines of {value}</p>;
          },
        },
      ]}
    />
  ));
  return mounts;
}

describe("child tables", () => {
  it("mount on the first expand, scoped by the row's id", () => {
    const mounts = table();
    expect(mounts).toEqual([]);
    fireEvent.click(theButton("expand r1"));
    expect(screen.getByText("lines of r1")).toBeDefined();
    expect(screen.getByText("lines")).toBeDefined();
    expect(mounts).toEqual(["r1"]);
  });

  it("keep their rows across a collapse, with no new mount", () => {
    const mounts = table();
    fireEvent.click(theButton("expand r0"));
    fireEvent.click(theButton("collapse r0"));
    expect(screen.queryByText("lines of r0")).toBeNull();
    fireEvent.click(theButton("expand r0"));
    expect(screen.getByText("lines of r0")).toBeDefined();
    expect(mounts).toEqual(["r0"]);
  });

  it("end when a load drops their row", () => {
    const [rows, setRows] = createSignal<readonly Row[]>(ROWS);
    const mounts = table(rows);
    fireEvent.click(theButton("expand r0"));
    setRows([ROWS[1]!]);
    expect(button("collapse r0")).toBeNull();
    setRows(ROWS);
    // The row stays expanded by its id, and its child mounts again.
    expect(screen.getByText("lines of r0")).toBeDefined();
    expect(mounts).toEqual(["r0", "r0"]);
  });

  it("stay out of the column panel", () => {
    table();
    fireEvent.click(theButton("columns"));
    const panel = Array.from(document.querySelectorAll("li[data-column]")).map((item) =>
      item.getAttribute("data-column"),
    );
    expect(panel).toEqual(["code"]);
  });
});
