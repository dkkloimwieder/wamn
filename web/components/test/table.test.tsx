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

import type { JsonValue, Outcome, Transport, WireRequest } from "@wamn/web-runtime";

import { WidgetQueryTable, WidgetQueryTableLabel } from "../fixture/components/widget.js";

afterEach(cleanup);

/** One transport that answers from a list and keeps what it was sent. */
function stub(replies: readonly Outcome<JsonValue>[]): {
  transport: Transport;
  sent: WireRequest[];
} {
  const sent: WireRequest[] = [];
  let next = 0;
  return {
    sent,
    transport: {
      invoke: (request: WireRequest) => {
        sent.push(request);
        const reply = replies[Math.min(next, replies.length - 1)];
        next += 1;
        return Promise.resolve(
          reply ?? { status: "uncertain", reason: "the stub ran out", retryRefusal: null },
        );
      },
    },
  };
}

function page(ids: readonly string[], cursor: string | null): Outcome<JsonValue> {
  return {
    status: "completed",
    value: {
      item: ids.map((id) => ({
        id,
        code: "standard",
        note: null,
        edit_version: "1",
        created_at: "2026-09-21T12:00:00.000000Z",
      })),
      next_cursor: cursor,
    },
  };
}

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

  it("states an outcome that is not a completion, and shows no row", async () => {
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
    // The header row, and the one row the grid shows when it holds no record.
    expect(screen.queryAllByRole("row")).toHaveLength(2);
    expect(screen.getByText("No data available")).toBeDefined();
  });
});
