/**
 * The QueryTable (wamn-sa7d.1): a DataTable wired from its table definition
 * alone, over the gallery's stub of the widget release.
 *
 * The definition is written by hand here, in the shape the emitter writes.
 */

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import { QueryTable, type QueryTableDefinition } from "@wamn/ui";

import { WIDGET_QUERY_TABLE } from "../fixture/components/widget.js";
import {
  WIDGET_QUERY_REQUEST_FIELDS,
  WIDGET_QUERY_RESULT_FIELDS,
  WIDGET_QUERY_ROUTE,
  WIDGET_UPDATE_REQUEST_FIELDS,
  WIDGET_UPDATE_RESULT_FIELDS,
  WIDGET_UPDATE_ROUTE,
  type WidgetQueryRow,
} from "../fixture/widget.js";
import { actionStub, EVENT_QUERY_ROUTE } from "../gallery/table-actions.js";
import { theButton } from "./dom.js";

afterEach(cleanup);

interface EventRow {
  readonly id: string;
  readonly widgetId: string;
  readonly amount: string;
}

const EVENTS: QueryTableDefinition<EventRow> = {
  name: "widget-event",
  read: { route: EVENT_QUERY_ROUTE, request: {}, result: { next_cursor: "nextCursor" } },
  rows: "item",
  rowId: ["id"],
  pageMaximum: 100,
  limitInput: ["limit"],
  sortFieldInput: null,
  sortDirectionInput: null,
  filters: [{ field: "widgetId", input: ["filter", "widget_id"], list: true }],
  scopeFilters: ["widgetId"],
  sortFields: [],
  sortMaxFields: 1,
  columns: [
    { field: "amount", label: "amount", type: "numeric", role: "value" },
    { field: "widgetId", label: "widget id", type: "uuid", role: "reference" },
    { field: "id", label: "id", type: "uuid", role: "key" },
  ],
  actions: [],
  childTables: [],
};

const WIDGETS: QueryTableDefinition<WidgetQueryRow> = {
  ...WIDGET_QUERY_TABLE,
  name: "widget",
  read: { route: WIDGET_QUERY_ROUTE, request: WIDGET_QUERY_REQUEST_FIELDS, result: WIDGET_QUERY_RESULT_FIELDS },
  rows: "item",
  limitInput: ["limit"],
  sortFieldInput: ["sort", "field"],
  sortDirectionInput: ["sort", "direction"],
  filters: [{ field: "code", input: ["filter", "code"], list: true }],
  update: {
    ...WIDGET_QUERY_TABLE.update,
    binding: { route: WIDGET_UPDATE_ROUTE, request: WIDGET_UPDATE_REQUEST_FIELDS, result: WIDGET_UPDATE_RESULT_FIELDS },
    supplied: [{ input: ["requestId"], kind: "requestId" }],
  },
  actions: [
    { operation: "platform-fixture:widget/get@1.0.0", label: "get", many: false, opens: "record", fill: [] },
    {
      operation: "platform-fixture:widget/archive@1.0.0",
      label: "archive",
      many: false,
      opens: "form",
      fill: [{ field: "id", input: ["value", "widgetId"] }],
    },
  ],
  childTables: [{ label: "events", table: () => EVENTS, scopeFilter: "widgetId" }],
};

const id = (index: number) => `00000000-0000-4000-8000-${String(index).padStart(12, "0")}`;

async function shown(fixed?: object) {
  const stub = actionStub(8);
  const opened: string[] = [];
  const filled: object[] = [];
  render(() => (
    <div style={{ height: "800px" }}>
      <QueryTable
        definition={WIDGETS}
        transport={stub.transport}
        label="widgets"
        fixed={fixed}
        onOpen={{ "platform-fixture:widget/get@1.0.0": (row) => opened.push(row.id) }}
        onFill={{ "platform-fixture:widget/archive@1.0.0": (initial) => filled.push(initial) }}
      />
    </div>
  ));
  await waitFor(() => expect(document.querySelector(`tr[data-row-id="${id(6)}"]`)).not.toBeNull());
  return { stub, opened, filled };
}

async function editAndSave(field: string, index: number, value: string) {
  fireEvent.click(theButton(`edit ${field} ${id(index)}`));
  fireEvent.input(screen.getByLabelText(`${field} ${id(index)}`), { target: { value } });
  fireEvent.click(theButton("save"));
}

const rowButton = (index: number, label: string) =>
  Array.from(document.querySelectorAll(`tr[data-row-id="${id(index)}"] button`)).find(
    (button) => button.textContent?.trim() === label,
  ) as HTMLButtonElement;

describe("QueryTable", () => {
  it("opens a record from a row, and fills a form with the row's key", async () => {
    const { opened, filled } = await shown();
    fireEvent.click(rowButton(2, "get"));
    fireEvent.click(rowButton(3, "archive"));
    expect(opened).toEqual([id(2)]);
    expect(filled).toEqual([{ value: { widgetId: id(3) } }]);
  });

  it("edits a cell through the definition's update, and shows a conflict on the row", async () => {
    const { stub } = await shown();
    await editAndSave("Operator note", 1, "checked");
    await waitFor(() =>
      expect(document.querySelector(`tr[data-row-id="${id(1)}"]`)?.textContent).toContain("checked"),
    );
    stub.changeElsewhere();
    await editAndSave("Operator note", 0, "late");
    await waitFor(() =>
      expect(document.querySelector(`tr[data-row-id="${id(0)}"] [data-slot="data-table-row-conflict"]`)).not.toBeNull(),
    );
  });

  it("shows a child table scoped to the row's key", async () => {
    await shown();
    fireEvent.click(theButton(`expand ${id(4)}`));
    await waitFor(() => expect(screen.getByText("4.50")).toBeDefined());
    expect(screen.getByText("5.50")).toBeDefined();
    expect(screen.queryByText("3.50")).toBeNull();
  });

  it("sends a fixed filter and leaves it out of the scope bar", async () => {
    await shown({ filter: { code: ["priority"] } });
    // Widgets 0, 3 and 6 are priority, so widget 1 is not read.
    expect(document.querySelector(`tr[data-row-id="${id(1)}"]`)).toBeNull();
    expect(document.querySelector('[data-slot="data-table-scope"] [data-field="code"]')).toBeNull();
  });
});
