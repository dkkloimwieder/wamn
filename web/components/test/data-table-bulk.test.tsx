/**
 * The bulk actions of the DataTable (wamn-8iul.3).
 *
 * Select all takes the loaded rows only, and says so on a set that is not
 * fully read. One submit hands the selected rows over once, in the table's
 * order, and each row shows its own result.
 */

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";

import { DataTable, type DataTableColumn, type DataTableRowResult } from "@wamn/ui";

import { button, pickChoice, theButton } from "./dom.js";

afterEach(cleanup);

interface Row {
  readonly id: string;
  readonly code: string;
}

const COLUMNS: readonly DataTableColumn<Row>[] = [{ field: "code", label: "code", type: "text" }];

const ROWS: readonly Row[] = [
  { id: "r0", code: "a" },
  { id: "r1", code: "b" },
  { id: "r2", code: "c" },
];

const ACTIONS = [{ operation: "fixture:row/move@1.0.0", label: "move" }];

function table(
  onBulk: (operation: string, rows: readonly Row[]) => Promise<readonly DataTableRowResult[]>,
  fullyRead = true,
) {
  render(() => (
    <DataTable
      name="rows"
      columns={COLUMNS}
      rowId={["id"]}
      rows={ROWS}
      fullyRead={fullyRead}
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
      bulkActions={ACTIONS}
      onBulk={onBulk}
    />
  ));
}

const check = (label: string) => fireEvent.click(screen.getByLabelText(label));

/** The result each row shows, by its code. */
const results = () =>
  Object.fromEntries(
    Array.from(document.querySelectorAll("tbody tr")).flatMap((row) => {
      const code = Array.from(row.querySelectorAll("td")).find((cell) => /^[abc]$/.test(cell.textContent ?? ""));
      const result = row.querySelector('[data-slot="data-table-row-result"]');
      return code === undefined ? [] : [[code.textContent, result?.getAttribute("data-status") ?? null]];
    }),
  );

describe("the bulk actions", () => {
  it("select all loaded rows, and run once over the selected rows in the table's order", async () => {
    const onBulk = vi.fn(async (_operation: string, rows: readonly Row[]) =>
      rows.map((): DataTableRowResult => ({ status: "completed" })),
    );
    table(onBulk);
    expect(theButton("run on 0 selected").disabled).toBe(true);
    check("select all loaded rows");
    await pickChoice("bulk action", "move");
    fireEvent.click(theButton("run on 3 selected"));
    await waitFor(() => expect(onBulk).toHaveBeenCalledTimes(1));
    expect(onBulk.mock.calls[0]![0]).toBe("fixture:row/move@1.0.0");
    expect(onBulk.mock.calls[0]![1].map((row) => row.id)).toEqual(["r0", "r1", "r2"]);
    await waitFor(() => expect(results()).toEqual({ a: "completed", b: "completed", c: "completed" }));
  });

  it("mark only the row an input was refused for", async () => {
    table(async (_operation, rows) =>
      rows.map((row): DataTableRowResult =>
        row.id === "r2" ? { status: "refused", message: "concurrency_conflict" } : { status: "completed" },
      ),
    );
    check("select row r0");
    check("select row r2");
    await pickChoice("bulk action", "move");
    fireEvent.click(theButton("run on 2 selected"));
    await waitFor(() => expect(results()).toEqual({ a: "completed", b: null, c: "refused" }));
    expect(screen.getByText("refused: concurrency_conflict")).toBeDefined();
  });

  it("say that select all takes the loaded rows only on a set that is not fully read", () => {
    table(async () => [], false);
    expect(screen.getByText(/Select all takes the loaded rows only/)).toBeDefined();
    check("select all loaded rows");
    expect(button("run on 3 selected")).not.toBeNull();
  });

  it("keep the selection column out of the column panel", () => {
    table(async () => []);
    fireEvent.click(theButton("columns"));
    const panel = Array.from(document.querySelectorAll("li[data-column]")).map((item) =>
      item.getAttribute("data-column"),
    );
    expect(panel).toEqual(["code"]);
  });
});
