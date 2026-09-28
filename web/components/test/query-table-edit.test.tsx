/**
 * Inline edit of a QueryTable (wamn-8iul.4).
 *
 * An editable cell edits in place and sends the typed value through the
 * definition's update. A refusal marks the cell, a revision conflict marks the
 * row and keeps the typed text, and an open edit holds every new load until it
 * closes.
 */

import { cleanup, fireEvent, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import type { TableColumn } from "@wamn/ui";
import type { JsonValue, Outcome } from "@wamn/web-runtime";

import { button, theButton } from "./dom.js";
import { queryTable, settled, type UpdateItem, updating, withUpdate } from "./tables.js";

afterEach(cleanup);

interface Row {
  readonly id: string;
  readonly code: string;
  readonly qty: number;
  readonly rowVersion: number;
}

const COLUMNS: readonly TableColumn<Row>[] = [
  { field: "code", label: "code", type: "text" },
  { field: "qty", label: "qty", type: "int32" },
  { field: "rowVersion", label: "row version", type: "int32", role: "revision" },
];

const ROWS: readonly Row[] = [
  { id: "r0", code: "a", qty: 2, rowVersion: 1 },
  { id: "r1", code: "b", qty: 3, rowVersion: 4 },
];

async function table(answer: (item: UpdateItem) => Outcome<JsonValue>) {
  const update = updating(answer);
  const { asked } = await queryTable({
    columns: COLUMNS,
    rows: () => ROWS,
    definition: withUpdate(["code", "qty"]),
    transport: update.transport,
  });
  return { asked, ...update };
}

const type = (label: string, value: string) => fireEvent.input(screen.getByLabelText(label), { target: { value } });

const refused = (text: string): Outcome<JsonValue> => ({
  status: "refused",
  code: "invalid_input",
  detail: null,
  text,
});

describe("inline edit", () => {
  it("edits only the editable cells, and saves the typed value with its row's key and revision", async () => {
    const { updates } = await table(() => ({ status: "completed", value: null }));
    expect(button("edit row version r0")).toBeNull();
    fireEvent.click(theButton("edit qty r1"));
    // One edit is open at a time, and no load runs while it is open.
    expect(theButton("edit code r0").disabled).toBe(true);
    expect(theButton("refresh").disabled).toBe(true);
    type("qty r1", "7");
    fireEvent.click(theButton("save"));
    await waitFor(() => expect(screen.queryByLabelText("qty r1")).toBeNull());
    expect(updates).toEqual([{ id: "r1", expected: 4, change: { qty: 7 } }]);
    expect(theButton("refresh").disabled).toBe(false);
  });

  it("marks the cell a refusal names, and keeps the editor open", async () => {
    await table(() => refused("unique_violation"));
    fireEvent.click(theButton("edit code r0"));
    type("code r0", "b");
    fireEvent.click(theButton("save"));
    await waitFor(() => expect(screen.getByText("unique_violation")).toBeDefined());
    expect(screen.getByLabelText("code r0").getAttribute("aria-invalid")).toBe("true");
    expect(document.querySelector('[data-slot="table-row-conflict"]')).toBeNull();
  });

  it("marks the row on a revision conflict, and keeps the typed value", async () => {
    await table(() => ({
      status: "refused",
      code: "concurrency_conflict",
      detail: null,
      text: "the row changed since it loaded",
    }));
    fireEvent.click(theButton("edit code r0"));
    type("code r0", "typed");
    fireEvent.click(theButton("save"));
    await waitFor(() =>
      expect(document.querySelector('tr[data-row-id="r0"] [data-slot="table-row-conflict"]')).not.toBeNull(),
    );
    expect((screen.getByLabelText("code r0") as HTMLInputElement).value).toBe("typed");
  });

  it("refuses a value that does not fit the column type before it calls the update", async () => {
    const { updates } = await table(() => ({ status: "completed", value: null }));
    fireEvent.click(theButton("edit qty r0"));
    type("qty r0", "two");
    fireEvent.keyDown(screen.getByLabelText("qty r0"), { key: "Enter" });
    expect(screen.getByText("not a whole number")).toBeDefined();
    expect(updates).toEqual([]);
    fireEvent.keyDown(screen.getByLabelText("qty r0"), { key: "Escape" });
    expect(screen.queryByLabelText("qty r0")).toBeNull();
  });
});

describe("a held load", () => {
  it("waits while an edit is open, and then runs the last load asked for", async () => {
    const { asked, writes } = await table(() => ({ status: "completed", value: null }));
    expect(asked).toHaveLength(1);
    fireEvent.click(theButton("edit code r0"));
    // Another session's write asks for a load, and so does a new cap.
    for (const write of writes) {
      write();
    }
    fireEvent.change(screen.getByLabelText("cap"), { target: { value: "50" } });
    expect(asked).toHaveLength(1);
    fireEvent.keyDown(screen.getByLabelText("code r0"), { key: "Escape" });
    await settled();
    expect(asked.map((request) => request.limit)).toEqual([1000, 50]);
  });
});
