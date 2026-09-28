/**
 * The bulk actions of a QueryTable (wamn-8iul.3).
 *
 * Select all takes the loaded rows only, and says so on a set that is not
 * fully read. One submit opens the action's form over the selected rows once,
 * in the loaded order, and each row shows its own result.
 */

import { cleanup, fireEvent, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import type { QueryTableDefinition, TableColumn } from "@wamn/ui";
import type { Outcome, Transport } from "@wamn/web-runtime";

import { button, pickChoice, theButton } from "./dom.js";
import { queryTable } from "./tables.js";

afterEach(cleanup);

interface Row {
  readonly id: string;
  readonly code: string;
}

const COLUMNS: readonly TableColumn<Row>[] = [{ field: "code", label: "code", type: "text" }];

const ROWS: readonly Row[] = [
  { id: "r0", code: "a" },
  { id: "r1", code: "b" },
  { id: "r2", code: "c" },
];

const MOVE = "fixture:row/move@1.0.0";

/**
 * The bulk form: one button that hands over one outcome for each row, as the
 * emitted form does once its call returns. `results` answers each row by its
 * code, and `forms` keeps the rows each opened form took.
 */
function bulkForm(results: (code: string) => Outcome<unknown>, forms: (readonly object[])[]) {
  return (props: { rows: readonly object[]; onEach: (outcomes: readonly Outcome<unknown>[]) => void }) => {
    forms.push(props.rows);
    return (
      <button
        type="button"
        onClick={() => props.onEach(props.rows.map((row) => results((row as { value: { code: string } }).value.code)))}
      >
        submit
      </button>
    );
  };
}

async function table(results: (code: string) => Outcome<unknown>, more = false) {
  const forms: (readonly object[])[] = [];
  const Form = bulkForm(results, forms);
  await queryTable({
    columns: COLUMNS,
    rows: () => ROWS,
    more: () => more,
    definition: (definition): QueryTableDefinition<Row> => ({
      ...definition,
      actions: [
        {
          operation: MOVE,
          label: "move",
          many: true,
          opens: "form",
          fill: [{ field: "code", input: ["value", "code"] }],
          form: () => Form,
        },
      ],
    }),
    // A bulk action needs a transport that sends many outer inputs in one call.
    transport: (memory): Transport => ({ ...memory, invokeEach: async () => [] }),
  });
  return { forms };
}

const check = (label: string) => fireEvent.click(screen.getByLabelText(label));

/** The result each row shows, by its code. */
const results = () =>
  Object.fromEntries(
    Array.from(document.querySelectorAll("tbody tr")).flatMap((row) => {
      const code = Array.from(row.querySelectorAll("td")).find((cell) => /^[abc]$/.test(cell.textContent ?? ""));
      const result = row.querySelector('[data-slot="table-row-result"]');
      return code === undefined ? [] : [[code.textContent, result?.getAttribute("data-status") ?? null]];
    }),
  );

const completed: Outcome<unknown> = { status: "completed", value: null };

describe("the bulk actions", () => {
  it("select all loaded rows, and run once over the selected rows in the loaded order", async () => {
    const { forms } = await table(() => completed);
    expect(theButton("run on 0 selected").disabled).toBe(true);
    check("select all loaded rows");
    await pickChoice("bulk action", "move");
    fireEvent.click(theButton("run on 3 selected"));
    await waitFor(() => expect(forms).toHaveLength(1));
    expect(forms[0]).toEqual(ROWS.map((row) => ({ value: { code: row.code } })));
    fireEvent.click(theButton("submit"));
    await waitFor(() => expect(results()).toEqual({ a: "completed", b: "completed", c: "completed" }));
  });

  it("mark only the row an input was refused for", async () => {
    await table((code) =>
      code === "c"
        ? { status: "refused", code: "concurrency_conflict", detail: null, text: "concurrency_conflict" }
        : completed,
    );
    check("select row r0");
    check("select row r2");
    await pickChoice("bulk action", "move");
    fireEvent.click(theButton("run on 2 selected"));
    await waitFor(() => expect(button("submit")).not.toBeNull());
    fireEvent.click(theButton("submit"));
    await waitFor(() => expect(results()).toEqual({ a: "completed", b: null, c: "refused" }));
    expect(screen.getByText("refused: concurrency_conflict")).toBeDefined();
  });

  it("say that select all takes the loaded rows only on a set that is not fully read", async () => {
    await table(() => completed, true);
    expect(screen.getByText(/Select all takes the loaded rows only/)).toBeDefined();
    check("select all loaded rows");
    expect(button("run on 3 selected")).not.toBeNull();
  });

  it("keep the selection column out of the column panel", async () => {
    await table(() => completed);
    fireEvent.click(theButton("columns"));
    const panel = Array.from(document.querySelectorAll("li[data-column]")).map((item) =>
      item.getAttribute("data-column"),
    );
    expect(panel).toEqual(["code"]);
  });
});
