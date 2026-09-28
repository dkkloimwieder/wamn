/**
 * What a QueryTable does after a write (wamn-8iul.5).
 *
 * An inline edit of a field that is neither a scope filter nor a sort field
 * puts its row in place. Every other write loads again.
 */

import { cleanup, fireEvent, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import type { QueryTableDefinition, TableColumn } from "@wamn/ui";
import type { JsonValue, Outcome, Transport } from "@wamn/web-runtime";

import { pickChoice, theButton } from "./dom.js";
import { queryTable, settled, updating, withUpdate } from "./tables.js";

afterEach(cleanup);

interface Row {
  readonly id: string;
  readonly code: string;
  readonly note: string;
  readonly site: string;
  readonly rowVersion: number;
}

const COLUMNS: readonly TableColumn<Row>[] = [
  { field: "code", label: "code", type: "text" },
  { field: "note", label: "note", type: "text" },
  { field: "site", label: "site", type: "text" },
];

const ROWS: readonly Row[] = [
  { id: "r0", code: "a", note: "n0", site: "s0", rowVersion: 1 },
  { id: "r1", code: "b", note: "n1", site: "s1", rowVersion: 1 },
];

/** The archive form: one button that answers every row as done. */
const ArchiveForm = (props: { rows: readonly object[]; onEach: (outcomes: readonly Outcome<unknown>[]) => void }) => (
  <button type="button" onClick={() => props.onEach(props.rows.map(() => ({ status: "completed", value: null })))}>
    submit
  </button>
);

async function table() {
  // The update answers the row as the write left it.
  const update = updating(({ id, change }) => {
    const row = ROWS.find((candidate) => candidate.id === id)!;
    return { status: "completed", value: { ...row, ...change, rowVersion: row.rowVersion + 1 } as JsonValue };
  });
  const { asked } = await queryTable({
    columns: COLUMNS,
    rows: () => ROWS,
    // code sorts on the server, and site scopes it.
    sortFields: ["code"],
    scopeFilters: ["site"],
    definition: (definition): QueryTableDefinition<Row> => ({
      ...withUpdate<Row>(["code", "note", "site"])(definition),
      actions: [
        {
          operation: "fixture:row/archive@1.0.0",
          label: "archive",
          many: true,
          opens: "form",
          fill: [],
          form: async () => ({ default: ArchiveForm }),
        },
      ],
    }),
    transport: (memory): Transport => ({ ...update.transport(memory), invokeEach: async () => [] }),
  });
  return { asked };
}

async function editAndSave(label: string, value: string) {
  fireEvent.click(theButton(`edit ${label} r0`));
  fireEvent.input(screen.getByLabelText(`${label} r0`), { target: { value } });
  fireEvent.click(theButton("save"));
  await waitFor(() => expect(screen.queryByLabelText(`${label} r0`)).toBeNull());
  await settled();
}

describe("after a write", () => {
  it("puts the row in place after an edit of a field that neither scopes nor sorts", async () => {
    const { asked } = await table();
    await editAndSave("note", "changed");
    expect(document.querySelector('tr[data-row-id="r0"]')?.textContent).toContain("changed");
    expect(asked).toHaveLength(1);
  });

  it("loads again after an edit of a scope filter or of a sort field", async () => {
    const { asked } = await table();
    await editAndSave("site", "s9");
    await editAndSave("code", "z");
    expect(asked).toHaveLength(3);
  });

  it("loads again after a bulk action", async () => {
    const { asked } = await table();
    fireEvent.click(screen.getByLabelText("select row r1"));
    await pickChoice("bulk action", "archive");
    fireEvent.click(theButton("run on 1 selected"));
    await waitFor(() => expect(screen.getByText("submit")).toBeDefined());
    fireEvent.click(theButton("submit"));
    await waitFor(() => expect(asked).toHaveLength(2));
  });
});
