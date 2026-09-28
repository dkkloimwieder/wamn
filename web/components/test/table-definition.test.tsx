/**
 * A QueryTable renders from the fixture's emitted table definition, and loads
 * with the definition's page maximum (wamn-xtz2.3). Its sort fields name the
 * row member and the wire name (wamn-vfvx.1). A filter states its match mode,
 * and a band its default days (wamn-3nsf.4).
 */

import { cleanup, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import { QueryTable } from "@wamn/ui";

import { WIDGET_QUERY_TABLE } from "../fixture/components/widget.js";
import { WIDGET_MAKER_QUERY_TABLE } from "../fixture/components/widget_maker.js";
import { page, tableStub } from "../stubs/index.js";

afterEach(() => {
  cleanup();
  window.history.replaceState(null, "", "/");
});

const definition = WIDGET_QUERY_TABLE;

/** Render the definition's table at `cap`, which the URL names, over one page from the stub. */
function loaded(cap: number, ids: string[], cursor: string | null) {
  const { transport, sent } = tableStub([page(ids, cursor)]);
  window.history.replaceState(null, "", `/?widget.cap=${cap}`);
  render(() => <QueryTable definition={definition} transport={transport} label="widgets" urlKey="widget" />);
  return sent;
}

describe("the table definition of the widget query", () => {
  it("names the read, its row id, its page maximum, its scope and its columns", () => {
    expect(definition.read.route.operation).toBe("platform-fixture:widget/query@1.0.0");
    expect(definition.rows).toBe("item");
    expect(definition.rowId).toEqual(["id"]);
    expect(definition.pageMaximum).toBe(100);
    expect(definition.scopeFilters).toEqual(["code", "makerId", "note"]);
    // Each filter states how it matches when not exactly.
    expect(definition.filters).toEqual([
      { field: "code", input: ["filter", "code"], list: true },
      { field: "makerId", input: ["filter", "makerId"], list: false, match: "is_null" },
      { field: "note", input: ["filter", "note"], list: true, match: "prefix" },
    ]);
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
    // The scope bar shows the label too, so this reads the column header's menu.
    expect(screen.getByLabelText("menu Widget code")).toBeDefined();
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

describe("the table definition of the widget maker query", () => {
  it("states its band with the default days, and its server search", () => {
    expect(WIDGET_MAKER_QUERY_TABLE.filters).toEqual([
      {
        field: "createdAt",
        input: ["filter", "createdAt"],
        list: false,
        match: "range",
        type: "timestamptz",
        required: true,
        defaultLastDays: 30,
      },
      { field: "name", input: ["filter", "name"], list: true, match: "contains" },
    ]);
    expect(WIDGET_MAKER_QUERY_TABLE.search).toEqual({ input: ["search"], fields: ["name"] });
  });
});
