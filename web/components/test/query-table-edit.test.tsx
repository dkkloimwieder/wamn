/**
 * Row edit of a QueryTable (wamn-8iul.4, wamn-v50u).
 *
 * An edit button opens a row, and its save sends the changed values through the
 * definition's update. A refusal marks the changed cells, a conflict marks the
 * row and keeps the typed text, and an open edit holds every new load until it
 * closes.
 */

import { cleanup, fireEvent, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import type { TableColumn } from "@wamn/ui";
import type { JsonValue, Outcome } from "@wamn/web-runtime";

import { memoryRoute } from "../gallery/memory.js";
import { choose, selector } from "./choose.js";
import { theButton } from "./dom.js";
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

describe("row edit", () => {
  it("opens every editable cell of a row, and saves the changed values at once with its key and revision", async () => {
    const { updates } = await table(() => ({ status: "completed", value: null }));
    fireEvent.click(theButton("edit r1"));
    expect(screen.queryByLabelText("row version r1")).toBeNull();
    // One row is open at a time, and no load runs while it is open.
    expect(theButton("edit r0").disabled).toBe(true);
    expect(theButton("refresh").disabled).toBe(true);
    type("qty r1", "7");
    type("code r1", "c");
    fireEvent.click(theButton("save"));
    await waitFor(() => expect(screen.queryByLabelText("qty r1")).toBeNull());
    expect(updates).toEqual([{ id: "r1", expected: 4, change: { code: "c", qty: 7 } }]);
    expect(theButton("refresh").disabled).toBe(false);
  });

  it("sends only the fields that changed, and closes a row that changed none without a call", async () => {
    const { updates } = await table(() => ({ status: "completed", value: null }));
    fireEvent.click(theButton("edit r0"));
    fireEvent.click(theButton("save"));
    expect(screen.queryByLabelText("code r0")).toBeNull();
    fireEvent.click(theButton("edit r0"));
    type("qty r0", "5");
    fireEvent.click(theButton("save"));
    await waitFor(() => expect(screen.queryByLabelText("qty r0")).toBeNull());
    expect(updates).toEqual([{ id: "r0", expected: 1, change: { qty: 5 } }]);
  });

  it("marks the cell a refusal names, and keeps the editor open", async () => {
    await table(() => refused("unique_violation"));
    fireEvent.click(theButton("edit r0"));
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
    fireEvent.click(theButton("edit r0"));
    type("code r0", "typed");
    fireEvent.click(theButton("save"));
    await waitFor(() =>
      expect(document.querySelector('tr[data-row-id="r0"] [data-slot="table-row-conflict"]')).not.toBeNull(),
    );
    expect((screen.getByLabelText("code r0") as HTMLInputElement).value).toBe("typed");
  });

  it("refuses a value that does not fit the column type before it calls the update", async () => {
    const { updates } = await table(() => ({ status: "completed", value: null }));
    fireEvent.click(theButton("edit r0"));
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
    fireEvent.click(theButton("edit r0"));
    // Another session's write asks for a load, and so does a new cap.
    for (const write of writes) {
      write(null);
    }
    fireEvent.change(screen.getByLabelText("cap"), { target: { value: "50" } });
    expect(asked).toHaveLength(1);
    fireEvent.keyDown(screen.getByLabelText("code r0"), { key: "Escape" });
    await settled();
    expect(asked.map((request) => request.limit)).toEqual([1000, 50]);
  });
});

interface MakerRow {
  readonly id: string;
  readonly code: string;
  readonly makerId: string;
  readonly rowVersion: number;
}

describe("a reference field", () => {
  const MAKERS = memoryRoute("makers");

  it("edits through a select over the records it can name, and saves the chosen key", async () => {
    const update = updating(() => ({ status: "completed", value: null }));
    await queryTable<MakerRow>({
      columns: [
        { field: "code", label: "code", type: "text" },
        { field: "makerId", label: "maker", type: "uuid", role: "reference" },
      ],
      rows: () => [{ id: "r0", code: "a", makerId: "m1", rowVersion: 1 }],
      definition: (declared) => {
        const updated = withUpdate<MakerRow>(["makerId"])(declared);
        const choices = {
          read: { route: MAKERS, request: {}, result: { next_cursor: "nextCursor" } },
          rows: "item",
          keyField: "id",
          displayField: "name",
        };
        return { ...updated, update: { ...updated.update!, fields: [{ ...updated.update!.fields[0]!, choices }] } };
      },
      transport: (memory) => {
        const inner = update.transport(memory);
        return {
          ...inner,
          invoke: (request) =>
            request.operation === MAKERS.operation
              ? Promise.resolve({
                  status: "completed",
                  value: {
                    item: [
                      { id: "m1", name: "Acme" },
                      { id: "m2", name: "Globex" },
                    ],
                    next_cursor: null,
                  },
                })
              : inner.invoke(request),
        };
      },
    });
    fireEvent.click(theButton("edit r0"));
    await waitFor(() => expect(selector("maker r0").value).toBe("Acme"));
    await choose("maker r0", "Globex");
    fireEvent.click(theButton("save"));
    await waitFor(() => expect(screen.queryByRole("combobox", { name: "maker r0" })).toBeNull());
    expect(update.updates).toEqual([{ id: "r0", expected: 1, change: { makerId: "m2" } }]);
  });

  it("reads its records again only after a write that changes their relation", async () => {
    const update = updating(() => ({ status: "completed", value: null }));
    let makerReads = 0;
    await queryTable<MakerRow>({
      columns: [
        { field: "code", label: "code", type: "text" },
        { field: "makerId", label: "maker", type: "uuid", role: "reference" },
      ],
      rows: () => [{ id: "r0", code: "a", makerId: "m1", rowVersion: 1 }],
      definition: (declared) => {
        const updated = withUpdate<MakerRow>(["makerId"])(declared);
        const choices = {
          read: { route: MAKERS, request: {}, result: { next_cursor: "nextCursor" } },
          rows: "item",
          keyField: "id",
          displayField: "name",
        };
        return { ...updated, update: { ...updated.update!, fields: [{ ...updated.update!.fields[0]!, choices }] } };
      },
      transport: (memory) => {
        const inner = update.transport(memory);
        return {
          ...inner,
          invoke: (request) => {
            if (request.operation !== MAKERS.operation) {
              return inner.invoke(request);
            }
            makerReads += 1;
            return Promise.resolve({
              status: "completed",
              value: { item: [{ id: "m1", name: "Acme" }], next_cursor: null },
            });
          },
        };
      },
    });
    fireEvent.click(theButton("edit r0"));
    await waitFor(() => expect(selector("maker r0").value).toBe("Acme"));
    expect(makerReads).toBe(1);
    for (const write of update.writes) {
      write(["gallery.widgets"]);
    }
    await settled();
    expect(makerReads).toBe(1);
    for (const write of update.writes) {
      write(["gallery.makers"]);
    }
    await waitFor(() => expect(makerReads).toBe(2));
  });
});
