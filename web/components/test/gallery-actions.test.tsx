/**
 * The gallery's app-shaped table with its actions (wamn-8iul.7), over its
 * stub transport: the row buttons, the bulk action through the emitted batch
 * form (wamn-sa7d.3), the editable cells, the after-write rules and the child
 * table.
 */

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import { ActionTable } from "../gallery/table-actions.js";
import { choose } from "./choose.js";
import { pickChoice, theButton } from "./dom.js";

afterEach(cleanup);

const id = (index: number) => `00000000-0000-4000-8000-${String(index).padStart(12, "0")}`;

async function shown() {
  render(() => (
    <div style={{ height: "800px" }}>
      <ActionTable size={8} />
    </div>
  ));
  await waitFor(() => expect(document.querySelector(`tr[data-row-id="${id(7)}"]`)).not.toBeNull());
}

async function editAndSave(field: string, index: number, value: string) {
  fireEvent.click(theButton(`edit ${field} ${id(index)}`));
  fireEvent.input(screen.getByLabelText(`${field} ${id(index)}`), { target: { value } });
  fireEvent.click(theButton("save"));
}

/** The text of one cell of one row, by the column's order in the definition. */
const cellText = (index: number, text: string) =>
  Array.from(document.querySelectorAll(`tr[data-row-id="${id(index)}"] td`)).some(
    (cell) => cell.textContent?.includes(text) === true,
  );

describe("the gallery's app-shaped table", () => {
  it("opens an action that takes one row from its button", async () => {
    await shown();
    const row = document.querySelector(`tr[data-row-id="${id(2)}"]`)!;
    fireEvent.click(Array.from(row.querySelectorAll("button")).find((button) => button.textContent === "get")!);
    expect(screen.getByText(`get opened for ${id(2)}`)).toBeDefined();
  });

  it("edits a note in place, refuses a code outside the domain, and shows a conflict on the row", async () => {
    await shown();
    await editAndSave("Operator note", 1, "checked");
    await waitFor(() => expect(cellText(1, "checked")).toBe(true));
    await editAndSave("Widget code", 2, "urgent");
    await waitFor(() => expect(screen.getByText("A value is not valid.")).toBeDefined());
    fireEvent.click(theButton("drop"));
    fireEvent.click(theButton("change the first widget elsewhere"));
    await editAndSave("Operator note", 0, "late");
    await waitFor(() =>
      expect(document.querySelector(`tr[data-row-id="${id(0)}"] [data-slot="data-table-row-conflict"]`)).not.toBeNull(),
    );
    expect((screen.getByLabelText(`Operator note ${id(0)}`) as HTMLInputElement).value).toBe("late");
  });

  it("records a batch for every selected row, and refuses only the held widget", async () => {
    await shown();
    fireEvent.click(screen.getByLabelText("select all loaded rows"));
    await pickChoice("bulk action", "record-batch");
    fireEvent.click(theButton("run on 8 selected"));
    // The batch form asks for what no row fills, and each row fills its widget.
    await waitFor(() => expect(screen.getByText("record-batch on 8 rows")).toBeDefined());
    await pickChoice("Grade", "first");
    await choose("Inspector", "Northwind");
    fireEvent.click(theButton("add"));
    fireEvent.input(screen.getByLabelText("Quantity received"), { target: { value: "1.00" } });
    fireEvent.click(theButton("submit"));
    await waitFor(() =>
      expect(document.querySelectorAll('[data-slot="data-table-row-result"]').length).toBe(8),
    );
    const refusedRows = Array.from(document.querySelectorAll('[data-status="refused"]')).map(
      (result) => result.closest("tr")?.getAttribute("data-row-id"),
    );
    expect(refusedRows).toEqual([id(3)]);
  });

  it("shows the events of a widget in its expanded row", async () => {
    await shown();
    fireEvent.click(theButton(`expand ${id(4)}`));
    await waitFor(() => expect(screen.getByText("4.50")).toBeDefined());
    expect(screen.getByText("5.50")).toBeDefined();
  });
});
