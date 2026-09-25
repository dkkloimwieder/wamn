/**
 * The DataTable renders from the fixture's emitted table definition, and loads
 * through the load state with the definition's page maximum (wamn-xtz2.3). Its
 * sort fields name the row member and the wire name (wamn-vfvx.1).
 */

import { cleanup, render, screen, waitFor } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { afterEach, describe, expect, it } from "vitest";

import { DataTable } from "@wamn/ui";
import { emptyLoad, finishLoad, loadLimit, startLoad, type LoadState } from "@wamn/web-runtime";

import { WIDGET_QUERY_TABLE } from "../fixture/components/widget.js";
import { query, type WidgetQueryRow } from "../fixture/widget.js";
import { page, tableStub } from "../stubs/index.js";

afterEach(cleanup);

const definition = WIDGET_QUERY_TABLE;

/** Render the definition's table and run one load at `cap` over the stub. */
function loaded(cap: number, ids: string[], cursor: string | null) {
  const { transport, sent } = tableStub([page(ids, cursor)]);
  const [state, setState] = createSignal<LoadState<WidgetQueryRow>>(emptyLoad(cap));
  const next = startLoad(state());
  setState(next);
  void query(transport, [{ limit: loadLimit(next.cap, definition.pageMaximum) }]).then((outcome) =>
    setState((current) => finishLoad(current, next.generation, outcome, definition.rowId)),
  );
  render(() => (
    <DataTable
      name="widgets"
      columns={definition.columns}
      rowId={definition.rowId}
      rows={state().rows}
      fullyRead={state().fullyRead}
      busy={state().busy}
      refusal={state().refusal}
      cap={state().cap}
      onCapChange={() => {}}
      onRefresh={() => {}}
      startedAt={state().startedAt}
      endedAt={state().endedAt}
      sortFields={definition.sortFields}
      sortMaxFields={definition.sortMaxFields}
      onSortChange={() => {}}
    />
  ));
  return sent;
}

describe("the table definition of the widget query", () => {
  it("names the read, its row id, its page maximum, its scope and its columns", () => {
    expect(definition.read).toBe("query");
    expect(definition.rowId).toBe("id");
    expect(definition.pageMaximum).toBe(100);
    expect(definition.scopeFilters).toEqual(["code"]);
    expect(definition.sortFields).toEqual([{ field: "createdAt", wire: "created_at" }]);
    expect(definition.sortMaxFields).toBe(1);
    expect(definition.columns.map((column) => column.field)).toEqual([
      "code",
      "createdAt",
      "editVersion",
      "id",
      "makerId",
      "note",
    ]);
    expect(definition.columns.find((column) => column.field === "makerId")).toMatchObject({
      type: "uuid",
      displayField: "name",
    });
  });

  it("renders its column labels and reads one page of the page maximum at the default cap", async () => {
    const sent = loaded(1000, ["w1", "w2"], null);
    await waitFor(() => expect(screen.getByText("w1")).toBeDefined());
    expect(screen.getByText("Widget code")).toBeDefined();
    expect(sent[0]?.items[0]).toMatchObject({ limit: definition.pageMaximum });
    expect(screen.queryByText("Full dataset cannot be loaded")).toBeNull();
  });

  it("reads the cap when it is below the page maximum, and a leftover cursor is not fully read", async () => {
    const sent = loaded(2, ["w1", "w2"], "c1");
    await waitFor(() => expect(screen.getByText("w2")).toBeDefined());
    expect(sent[0]?.items[0]).toMatchObject({ limit: 2 });
    expect(screen.getByText("Full dataset cannot be loaded")).toBeDefined();
  });
});
