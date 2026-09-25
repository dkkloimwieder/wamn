/**
 * The testing method for a generated component, used once.
 *
 * A component test renders the emitted component with a stub transport and
 * reads the document. It needs no browser and no server, and it states what an
 * operator would see.
 *
 * The subject is the fixture's page table, written by
 * `crates/schema/generator/src/client_component.rs`. The command
 * `check_client_components` writes it into `fixture/` before this runs.
 */

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import { createTransport, type JsonValue, type Outcome } from "@wamn/web-runtime";

import { WidgetQueryTable, WidgetQueryTableLabel } from "../fixture/components/widget.js";
import { GONE, MAKER, SOUTH, makerStub, page, tableStub as stub } from "../stubs/index.js";

afterEach(cleanup);

describe("the generated table for a page", () => {
  it("shows one row for each record the release returned", async () => {
    const { transport } = stub([page(["a", "b"], null)]);
    render(() => <WidgetQueryTable transport={transport} />);
    fireEvent.click(screen.getByText("read"));
    await waitFor(() => expect(screen.getAllByRole("row")).toHaveLength(3));
    // One header row, and one row for each record.
    expect(screen.getByText("a")).toBeDefined();
    expect(screen.getByText("b")).toBeDefined();
    // The plan's columns are the headers, in contract order. A column whose
    // model authors a label reads that text, and one that does not keeps its
    // field name with spaces. The row link and the row form are one column
    // each, after the plan's columns, with no header text.
    expect(screen.getAllByRole("columnheader").map((header) => header.textContent)).toEqual([
      "Widget code",
      "created at",
      "edit version",
      "id",
      "maker id",
      "Operator note",
      "",
      "",
    ]);
  });

  it("shows the maker a widget names by its name, reading each maker once (wamn-zrrg)", async () => {
    const { transport, sent } = makerStub();
    render(() => <WidgetQueryTable transport={transport} />);
    fireEvent.click(screen.getByText("read"));
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

  it("names the screen without rendering a heading", () => {
    // The label is an exported constant. A component does not own a heading,
    // because the page that places it does.
    expect(WidgetQueryTableLabel).toBe("Find widgets");
    const { transport } = stub([page(["a"], null)]);
    const { container } = render(() => <WidgetQueryTable transport={transport} />);
    expect(container.querySelectorAll("h1, h2, h3")).toHaveLength(0);
  });

  it("asks for the next page with the cursor the last reply returned", async () => {
    const { transport, sent } = stub([page(["a"], "c1"), page(["b"], null)]);
    render(() => <WidgetQueryTable transport={transport} />);
    const next = screen.getByRole("button", { name: "next page" }) as HTMLButtonElement;
    // The control stays in place and waits for a cursor.
    expect(next.disabled).toBe(true);
    fireEvent.click(screen.getByText("read"));
    await waitFor(() => expect(next.disabled).toBe(false));

    fireEvent.click(next);
    await waitFor(() => expect(sent).toHaveLength(2));
    const second = sent[1]?.items[0] as { [key: string]: JsonValue };
    expect(second["cursor"]).toBe("c1");
    // The page appends, so both records stay on the screen.
    await waitFor(() => expect(screen.getAllByRole("row")).toHaveLength(3));
    expect(screen.getByText("a")).toBeDefined();
    expect(screen.getByText("b")).toBeDefined();
    // The last page sent no cursor, so the control is disabled again.
    expect(next.disabled).toBe(true);
  });

  it("clears the rows and reads again when a control changes", async () => {
    const { transport, sent } = stub([page(["a"], "c1"), page(["b"], null)]);
    render(() => <WidgetQueryTable transport={transport} />);
    fireEvent.click(screen.getByText("read"));
    await waitFor(() => expect(screen.getByText("a")).toBeDefined());

    fireEvent.change(screen.getByLabelText("Widget code"), { target: { value: "x,y" } });
    await waitFor(() => expect(screen.getByText("b")).toBeDefined());
    // The old rows go, because the cursor named a place in the old list.
    expect(screen.queryByText("a")).toBeNull();
    const second = sent[1]?.items[0] as { [key: string]: JsonValue };
    expect(second["filter"]).toEqual({ code: ["x", "y"] });
    expect(second["cursor"]).toBeUndefined();
  });

  it("sends no filter member once the operator empties the filter (wamn-oya5)", async () => {
    const { transport, sent } = stub([page(["a"], null)]);
    render(() => <WidgetQueryTable transport={transport} />);
    const code = screen.getByLabelText("Widget code");
    fireEvent.change(code, { target: { value: "x" } });
    await waitFor(() => expect(sent).toHaveLength(1));
    expect((sent[0]?.items[0] as { [key: string]: JsonValue })["filter"]).toEqual({ code: ["x"] });

    // An empty list would ask for no record, so the read leaves the filter out.
    fireEvent.change(code, { target: { value: "" } });
    await waitFor(() => expect(sent).toHaveLength(2));
    expect(sent[1]?.items[0] as { [key: string]: JsonValue }).not.toHaveProperty("filter");
  });

  it("sends a sort only once both its field and its direction are chosen (wamn-2ut3)", async () => {
    const { transport, sent } = stub([page(["a"], null)]);
    render(() => <WidgetQueryTable transport={transport} />);
    const pick = async (label: string, name: string) => {
      const trigger = screen.getByRole("button", { name: label });
      await waitFor(() => {
        if (trigger.getAttribute("aria-expanded") !== "true") {
          fireEvent.pointerDown(trigger, { pointerType: "mouse", button: 0 });
          fireEvent.pointerUp(trigger, { pointerType: "mouse", button: 0 });
          fireEvent.click(trigger);
          throw new Error(`the choice ${label} did not open`);
        }
      });
      const option = await waitFor(() => screen.getByRole("option", { name }));
      fireEvent.pointerDown(option, { pointerType: "mouse", button: 0 });
      fireEvent.pointerUp(option, { pointerType: "mouse", button: 0 });
      fireEvent.click(option);
    };

    // A direction with no field is not a sort the release accepts.
    await pick("direction", "ascending");
    await waitFor(() => expect(sent).toHaveLength(1));
    expect(sent[0]?.items[0] as { [key: string]: JsonValue }).not.toHaveProperty("sort");

    await pick("field", "created at");
    await waitFor(() => expect(sent).toHaveLength(2));
    expect((sent[1]?.items[0] as { [key: string]: JsonValue })["sort"]).toEqual({
      field: "created_at",
      direction: "ascending",
    });
  });

  it("states an outcome that is not a completion in place of the empty message", async () => {
    const { transport } = stub([
      { status: "refused", code: "permission_denied", detail: null },
    ]);
    const seen: Outcome<unknown>[] = [];
    render(() => (
      <WidgetQueryTable transport={transport} onOutcome={(outcome) => seen.push(outcome)} />
    ));
    fireEvent.click(screen.getByText("read"));
    await waitFor(() => expect(seen).toHaveLength(1));
    expect(seen[0]?.status).toBe("refused");
    // The header row, and the one row the grid shows when it holds no record,
    // which states the refusal (wamn-jh80).
    expect(screen.queryAllByRole("row")).toHaveLength(2);
    expect(screen.getAllByText("You do not have permission to do this.").length).toBeGreaterThan(0);
    expect(screen.queryByText("permission_denied")).toBeNull();
    expect(screen.queryByText("No data available")).toBeNull();
  });

  it("says no data only when a read completed with no row", async () => {
    const { transport } = stub([page([], null)]);
    render(() => <WidgetQueryTable transport={transport} />);
    fireEvent.click(screen.getByText("read"));
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

    fireEvent.click(screen.getByText("read"));
    await waitFor(() => expect(seen).toHaveLength(1));
    expect(seen[0]).toMatchObject({ status: "refused", code: "unauthenticated" });
    await waitFor(() =>
      expect(screen.getAllByText("You are not signed in.").length).toBeGreaterThan(0),
    );
    expect(screen.queryByText("No data available")).toBeNull();

    fireEvent.click(screen.getByText("read"));
    await waitFor(() => expect(seen).toHaveLength(2));
    expect(seen[1]?.status).toBe("uncertain");
    await waitFor(() =>
      expect(screen.getAllByText(/the server reported internal_error/).length).toBeGreaterThan(0),
    );
    expect(screen.queryByText("No data available")).toBeNull();
  });
});
