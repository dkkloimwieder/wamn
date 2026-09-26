/**
 * The testing method for a generated component, used once.
 *
 * A component test renders the emitted component with a stub transport and
 * reads the document. It needs no browser and no server, and it states what an
 * operator would see.
 *
 * The subject is the fixture's page table, written by
 * `crates/schema/generator/src/client_component.rs`. It renders the DataTable
 * over its table definition (wamn-oi59). The command `check_client_components`
 * writes it into `fixture/` before this runs.
 */

import { cleanup, fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import { createTransport, type JsonValue, type Outcome } from "@wamn/web-runtime";

import {
  WIDGET_QUERY_TABLE,
  WidgetQueryTable,
  WidgetQueryTableLabel,
} from "../fixture/components/widget.js";
import { GONE, MAKER, SOUTH, makerStub, page, tableStub as stub } from "../stubs/index.js";

afterEach(cleanup);

/** Types one value into the scope bar control of a filter and adds it with Enter. */
function addScope(label: string, value: string) {
  const bar = document.querySelector<HTMLElement>("[data-slot=data-table-scope]")!;
  const input = within(bar).getByLabelText(label) as HTMLInputElement;
  input.value = value;
  fireEvent.keyDown(input, { key: "Enter" });
}

/** The input of the `index`th request the table sent. */
const item = (sent: readonly { items: readonly unknown[] }[], index: number) =>
  sent[index]?.items[0] as { [key: string]: JsonValue };

describe("the generated table for a page", () => {
  it("loads when it mounts and shows one row for each record the release returned", async () => {
    const { transport } = stub([page(["a", "b"], null)]);
    render(() => <WidgetQueryTable transport={transport} />);
    // One header row, one row for each record, and the totals row of a fully
    // read set.
    await waitFor(() => expect(screen.getAllByRole("row")).toHaveLength(4));
    expect(screen.getByText("a")).toBeDefined();
    expect(screen.getByText("b")).toBeDefined();
    // The totals have one cell for each column of the definition, and none
    // for the row buttons.
    expect(document.querySelectorAll("[data-slot=data-table-total]")).toHaveLength(
      WIDGET_QUERY_TABLE.columns.length,
    );
    // The definition's columns are the headers, in contract order. A column
    // whose model authors a label reads that text, and one that does not keeps
    // its field name with spaces. The row buttons are one last column with no
    // header text.
    expect(screen.getAllByRole("columnheader").map((header) => header.textContent)).toEqual([
      "Widget code",
      "created at",
      "edit version",
      "id",
      "maker id",
      "Operator note",
      "",
    ]);
  });

  it("shows the maker a widget names by its name, reading each maker once (wamn-zrrg)", async () => {
    const { transport, sent } = makerStub();
    render(() => <WidgetQueryTable transport={transport} />);
    await waitFor(() => expect(screen.getAllByText("Northwind")).toHaveLength(2));
    expect(screen.getByText("Southwind")).toBeDefined();
    // A key that no read finds shows the key, so the cell still names it.
    await waitFor(() => expect(screen.getByText(GONE)).toBeDefined());
    expect(screen.queryByText(MAKER)).toBeNull();
    // One table read, then one read for each maker the rows name. Two rows
    // name the same maker, and a row that names none asks for nothing.
    const makers = sent.filter((request) => request.operation.includes("widget-maker"));
    expect(makers.map((request) => (request.items[0] as { id: string }).id).sort()).toEqual(
      [GONE, MAKER, SOUTH].sort(),
    );
    expect(makers.every((request) => request.operation.includes("/get@"))).toBe(true);
  });

  it("opens the record of a row from its row link, in the last column", async () => {
    const { transport } = stub([page(["a", "b"], null)]);
    const opened: string[] = [];
    render(() => (
      <WidgetQueryTable
        transport={transport}
        onOpen={{ "platform-fixture:widget/get@1.0.0": (row) => void opened.push(row.id) }}
      />
    ));
    await waitFor(() => expect(screen.getAllByRole("button", { name: "get" })).toHaveLength(2));
    fireEvent.click(screen.getAllByRole("button", { name: "get" })[1]!);
    expect(opened).toEqual(["b"]);
    // A page that passes no callback gets no button.
    expect(screen.queryByRole("button", { name: "record-batch" })).toBeNull();
  });

  it("names the screen without rendering a heading", () => {
    // The label is an exported constant. A component does not own a heading,
    // because the page that places it does.
    expect(WidgetQueryTableLabel).toBe("Find widgets");
    const { transport } = stub([page(["a"], null)]);
    const { container } = render(() => <WidgetQueryTable transport={transport} />);
    expect(container.querySelectorAll("h1, h2, h3")).toHaveLength(0);
  });

  it("reads one page at the page maximum, and a cursor left over is not fully read", async () => {
    const { transport, sent } = stub([page(["a"], "c1")]);
    render(() => <WidgetQueryTable transport={transport} />);
    await waitFor(() => expect(screen.getByText("a")).toBeDefined());
    // The default cap is above the page maximum, so the read asks for the page
    // maximum, and it follows no cursor.
    expect(item(sent, 0)["limit"]).toBe(WIDGET_QUERY_TABLE.pageMaximum);
    expect(item(sent, 0)["cursor"]).toBeUndefined();
    expect(sent).toHaveLength(1);
    expect(screen.getByText("Full dataset cannot be loaded")).toBeDefined();
  });

  it("starts a new load with a scope filter when the scope bar adds a value", async () => {
    const { transport, sent } = stub([page(["a"], null), page(["b"], null)]);
    render(() => <WidgetQueryTable transport={transport} />);
    await waitFor(() => expect(screen.getByText("a")).toBeDefined());

    addScope("Widget code", "x");
    await waitFor(() => expect(screen.getByText("b")).toBeDefined());
    expect(screen.queryByText("a")).toBeNull();
    expect(item(sent, 1)["filter"]).toEqual({ code: ["x"] });
    expect(item(sent, 1)["limit"]).toBe(WIDGET_QUERY_TABLE.pageMaximum);
    addScope("Widget code", "y");
    await waitFor(() => expect(sent).toHaveLength(3));
    expect(item(sent, 2)["filter"]).toEqual({ code: ["x", "y"] });
  });

  it("sends no filter member once the operator removes the last value (wamn-oya5)", async () => {
    const { transport, sent } = stub([page(["a"], null)]);
    render(() => <WidgetQueryTable transport={transport} />);
    await waitFor(() => expect(sent).toHaveLength(1));
    addScope("Widget code", "x");
    await waitFor(() => expect(sent).toHaveLength(2));
    expect(item(sent, 1)["filter"]).toEqual({ code: ["x"] });

    // An empty list would ask for no record, so the read leaves the filter out.
    fireEvent.click(screen.getByRole("button", { name: "remove Widget code x" }));
    await waitFor(() => expect(sent).toHaveLength(3));
    expect(item(sent, 2)).not.toHaveProperty("filter");
  });

  it("sends the sort of a header click as a new load when the set is not fully read", async () => {
    const { transport, sent } = stub([page(["a"], "c1")]);
    render(() => <WidgetQueryTable transport={transport} />);
    await waitFor(() => expect(screen.getByText("a")).toBeDefined());
    expect(item(sent, 0)).not.toHaveProperty("sort");

    // The request names the field by its wire name.
    fireEvent.click(screen.getByRole("button", { name: "created at" }));
    await waitFor(() => expect(sent).toHaveLength(2));
    expect(item(sent, 1)["sort"]).toEqual({ field: "created_at", direction: "ascending" });
    fireEvent.click(screen.getByRole("button", { name: "created at" }));
    await waitFor(() => expect(sent).toHaveLength(3));
    expect(item(sent, 2)["sort"]).toEqual({ field: "created_at", direction: "descending" });
  });

  it("states an outcome that is not a completion in place of the empty message", async () => {
    const { transport } = stub([
      { status: "refused", code: "permission_denied", detail: null },
    ]);
    const seen: Outcome<unknown>[] = [];
    render(() => (
      <WidgetQueryTable transport={transport} onOutcome={(outcome) => seen.push(outcome)} />
    ));
    await waitFor(() => expect(seen).toHaveLength(1));
    expect(seen[0]?.status).toBe("refused");
    // The header row, and the one row the grid shows when it holds no record,
    // which states the refusal (wamn-jh80).
    await waitFor(() => expect(screen.queryAllByRole("row")).toHaveLength(2));
    expect(screen.getAllByText("You do not have permission to do this.").length).toBeGreaterThan(0);
    expect(screen.queryByText("permission_denied")).toBeNull();
    expect(screen.queryByText("No data available")).toBeNull();
  });

  it("says no data only when a read completed with no row", async () => {
    const { transport } = stub([page([], null)]);
    render(() => <WidgetQueryTable transport={transport} />);
    await waitFor(() => expect(screen.getByText("No data available")).toBeDefined());
  });

  it("shows a refusal that the release answered over HTTP (wamn-jh80)", async () => {
    // The two replies the WMS checklist saw: a 401 before any item ran, and a
    // 200 whose one item is an undeclared refusal. The real transport reads
    // each one, so the case covers the path from HTTP to the screen.
    const replies = [
      () => new Response(JSON.stringify({ error: { code: "unauthorized" } }), { status: 401 }),
      () =>
        new Response(JSON.stringify([{ error: { code: "internal_error" } }]), {
          status: 200,
        }),
    ];
    let next = 0;
    const transport = createTransport({
      baseUrl: "http://stub",
      credential: "token",
      // A read is a GET with no body, and its outcomes match by position.
      fetch: () => {
        const reply = replies[next] ?? replies[replies.length - 1];
        next += 1;
        return Promise.resolve(reply!());
      },
    });
    const seen: Outcome<unknown>[] = [];
    render(() => (
      <WidgetQueryTable transport={transport} onOutcome={(outcome) => seen.push(outcome)} />
    ));

    await waitFor(() => expect(seen).toHaveLength(1));
    expect(seen[0]).toMatchObject({ status: "refused", code: "unauthenticated" });
    await waitFor(() =>
      expect(screen.getAllByText("You are not signed in.").length).toBeGreaterThan(0),
    );
    expect(screen.queryByText("No data available")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "refresh" }));
    await waitFor(() => expect(seen).toHaveLength(2));
    expect(seen[1]?.status).toBe("uncertain");
    await waitFor(() =>
      expect(screen.getAllByText(/the server reported internal_error/).length).toBeGreaterThan(0),
    );
    expect(screen.queryByText("No data available")).toBeNull();
  });
});
