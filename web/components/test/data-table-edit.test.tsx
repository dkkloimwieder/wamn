/**
 * Inline edit of the DataTable (wamn-8iul.4).
 *
 * An editable cell edits in place and hands the typed value to the caller. A
 * refusal marks the cell, a revision conflict marks the row and keeps the
 * typed text, and an open edit holds every new load until it closes.
 */

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";

import { createTableLoad, DataTable, type DataTableColumn, type DataTableEditResult } from "@wamn/ui";

import { button, theButton } from "./dom.js";

afterEach(cleanup);

interface Row {
  readonly id: string;
  readonly code: string;
  readonly qty: number;
  readonly rowVersion: number;
}

const COLUMNS: readonly DataTableColumn<Row>[] = [
  { field: "code", label: "code", type: "text" },
  { field: "qty", label: "qty", type: "int32" },
  { field: "rowVersion", label: "row version", type: "int32", role: "revision" },
];

const ROWS: readonly Row[] = [
  { id: "r0", code: "a", qty: 2, rowVersion: 1 },
  { id: "r1", code: "b", qty: 3, rowVersion: 4 },
];

function table(
  onEdit: (row: Row, field: keyof Row & string, value: unknown) => Promise<DataTableEditResult<Row>>,
  onEditing: (editing: boolean) => void = () => {},
) {
  render(() => (
    <DataTable
      name="rows"
      columns={COLUMNS}
      rowId="id"
      rows={ROWS}
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
      editableFields={["code", "qty"]}
      onEdit={onEdit}
      onEditing={onEditing}
    />
  ));
}

const type = (label: string, value: string) =>
  fireEvent.input(screen.getByLabelText(label), { target: { value } });

describe("inline edit", () => {
  it("edits only the editable cells, and saves the typed value with its row", async () => {
    const onEdit = vi.fn(async (): Promise<DataTableEditResult<Row>> => ({ status: "completed" }));
    const onEditing = vi.fn();
    table(onEdit, onEditing);
    expect(button("edit row version r0")).toBeNull();
    fireEvent.click(theButton("edit qty r1"));
    expect(onEditing).toHaveBeenLastCalledWith(true);
    // One edit is open at a time.
    expect(theButton("edit code r0").disabled).toBe(true);
    expect(theButton("refresh").disabled).toBe(true);
    type("qty r1", "7");
    fireEvent.click(theButton("save"));
    await waitFor(() => expect(onEditing).toHaveBeenLastCalledWith(false));
    expect(onEdit).toHaveBeenCalledWith(ROWS[1], "qty", 7);
    expect(screen.queryByLabelText("qty r1")).toBeNull();
  });

  it("marks the cell a refusal names, and keeps the editor open", async () => {
    table(async () => ({ status: "refused", message: "unique_violation" }));
    fireEvent.click(theButton("edit code r0"));
    type("code r0", "b");
    fireEvent.click(theButton("save"));
    await waitFor(() => expect(screen.getByText("unique_violation")).toBeDefined());
    expect(screen.getByLabelText("code r0").getAttribute("aria-invalid")).toBe("true");
    expect(document.querySelector('[data-slot="data-table-row-conflict"]')).toBeNull();
  });

  it("marks the row on a revision conflict, and keeps the typed value", async () => {
    table(async () => ({ status: "conflict", message: "the row changed since it loaded" }));
    fireEvent.click(theButton("edit code r0"));
    type("code r0", "typed");
    fireEvent.click(theButton("save"));
    await waitFor(() =>
      expect(document.querySelector('tr[data-row-id="r0"] [data-slot="data-table-row-conflict"]')).not.toBeNull(),
    );
    expect((screen.getByLabelText("code r0") as HTMLInputElement).value).toBe("typed");
  });

  it("refuses a value that does not fit the column type before it calls the update", () => {
    const onEdit = vi.fn(async (): Promise<DataTableEditResult<Row>> => ({ status: "completed" }));
    table(onEdit);
    fireEvent.click(theButton("edit qty r0"));
    type("qty r0", "two");
    fireEvent.keyDown(screen.getByLabelText("qty r0"), { key: "Enter" });
    expect(screen.getByText("not a whole number")).toBeDefined();
    expect(onEdit).not.toHaveBeenCalled();
    fireEvent.keyDown(screen.getByLabelText("qty r0"), { key: "Escape" });
    expect(screen.queryByLabelText("qty r0")).toBeNull();
  });
});

describe("a held load", () => {
  it("waits while an edit is open, and then runs the last load asked for", async () => {
    const read = vi.fn(async (_limit: number, _sort: unknown) => ({
      status: "completed" as const,
      value: { item: [...ROWS], nextCursor: null },
    }));
    const load = createTableLoad<Row>({ rowId: "id", pageMaximum: 100, sortFields: [] }, read);
    await load.load();
    expect(read).toHaveBeenCalledTimes(1);
    load.hold(true);
    await load.load();
    await load.load(50);
    expect(read).toHaveBeenCalledTimes(1);
    load.hold(false);
    await waitFor(() => expect(read).toHaveBeenCalledTimes(2));
    expect(read.mock.calls[1]![0]).toBe(50);
  });
});
