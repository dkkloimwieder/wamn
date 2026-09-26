/**
 * What the DataTable does after a write (wamn-8iul.5).
 *
 * An inline edit of a field that is neither a scope filter nor a sort field
 * puts its row in place. Every other write asks the source for a new load.
 */

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";

import { DataTable, type DataTableColumn, type DataTableEditResult } from "@wamn/ui";

import { pickChoice, theButton } from "./dom.js";

afterEach(cleanup);

interface Row {
  readonly id: string;
  readonly code: string;
  readonly note: string;
  readonly site: string;
  readonly rowVersion: number;
}

const COLUMNS: readonly DataTableColumn<Row>[] = [
  { field: "code", label: "code", type: "text" },
  { field: "note", label: "note", type: "text" },
  { field: "site", label: "site", type: "text" },
];

const ROWS: readonly Row[] = [
  { id: "r0", code: "a", note: "n0", site: "s0", rowVersion: 1 },
  { id: "r1", code: "b", note: "n1", site: "s1", rowVersion: 1 },
];

function table(onRowChange: (row: Row) => void, onReload: () => void) {
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
      // code sorts on the server, and site scopes it.
      sortFields={[{ field: "code" }]}
      sortMaxFields={1}
      onSortChange={() => {}}
      scopeFilters={["site"]}
      onScopeChange={() => {}}
      editableFields={["code", "note", "site"]}
      onEdit={async (row, field, value): Promise<DataTableEditResult<Row>> => ({
        status: "completed",
        row: { ...row, [field]: value, rowVersion: row.rowVersion + 1 },
      })}
      onRowChange={onRowChange}
      onReload={onReload}
      bulkActions={[{ operation: "fixture:row/archive@1.0.0", label: "archive" }]}
      onBulk={async (_operation, rows) => rows.map(() => ({ status: "completed" as const }))}
    />
  ));
}

async function editAndSave(label: string, value: string) {
  fireEvent.click(theButton(`edit ${label} r0`));
  fireEvent.input(screen.getByLabelText(`${label} r0`), { target: { value } });
  fireEvent.click(theButton("save"));
  await waitFor(() => expect(screen.queryByLabelText(`${label} r0`)).toBeNull());
}

describe("after a write", () => {
  it("puts the row in place after an edit of a field that neither scopes nor sorts", async () => {
    const onRowChange = vi.fn();
    const onReload = vi.fn();
    table(onRowChange, onReload);
    await editAndSave("note", "changed");
    expect(onRowChange).toHaveBeenCalledWith({ ...ROWS[0], note: "changed", rowVersion: 2 });
    expect(onReload).not.toHaveBeenCalled();
  });

  it("reloads after an edit of a scope filter or of a sort field", async () => {
    const onRowChange = vi.fn();
    const onReload = vi.fn();
    table(onRowChange, onReload);
    await editAndSave("site", "s9");
    await editAndSave("code", "z");
    expect(onReload).toHaveBeenCalledTimes(2);
    expect(onRowChange).not.toHaveBeenCalled();
  });

  it("reloads after a bulk action", async () => {
    const onReload = vi.fn();
    table(() => {}, onReload);
    fireEvent.click(screen.getByLabelText("select row r1"));
    await pickChoice("bulk action", "archive");
    fireEvent.click(theButton("run on 1 selected"));
    await waitFor(() => expect(onReload).toHaveBeenCalledTimes(1));
  });
});
