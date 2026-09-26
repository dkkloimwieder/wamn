/**
 * The testing method for a page that reads again after a write, used once.
 *
 * The transport tells each listener when a write settles. A detail and a
 * table show what the write changed (`docs/architecture/execution.md`).
 */

import { cleanup, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import type { JsonValue, Outcome, Transport, WireRequest } from "@wamn/web-runtime";

import { WidgetCreateForm, WidgetQueryTable } from "../fixture/components/widget.js";
import { WidgetMakerGetDetail, WidgetMakerListTable } from "../fixture/components/widget_maker.js";
import { MAKER, WIDGET } from "../stubs/index.js";

afterEach(cleanup);

/**
 * A transport whose maker is renamed by `write`, which then tells every
 * listener, as a transport does after a write settles.
 */
function stub() {
  const sent: WireRequest[] = [];
  const listeners = new Set<() => void>();
  let name = "Northwind";
  const transport: Transport = {
    invoke: (request: WireRequest) => {
      sent.push(request);
      const maker = { id: MAKER, name, edit_version: "1", created_at: "2026-09-25T00:00:00Z" };
      const widget = {
        id: WIDGET,
        code: "standard",
        maker_id: MAKER,
        note: null,
        edit_version: "1",
        created_at: "2026-09-25T00:00:00Z",
      };
      const reply: Outcome<JsonValue> = request.operation.includes("widget-maker/get")
        ? { status: "completed", value: maker }
        : request.operation.includes("widget/query")
          ? { status: "completed", value: { item: [widget], next_cursor: null } }
        : request.operation.includes("widget-maker/list")
          ? { status: "completed", value: { rows: [maker] } }
          : { status: "completed", value: { item: [maker], nextCursor: null } };
      return Promise.resolve(reply);
    },
    onWrite: (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
  };
  const write = (renamed: string) => {
    name = renamed;
    for (const listener of listeners) {
      listener();
    }
  };
  const reads = (operation: string) =>
    sent.filter((request) => request.operation.includes(operation)).length;
  return { transport, write, reads, listeners };
}

describe("a write on the page's transport", () => {
  it("makes a shown detail read its record again", async () => {
    const { transport, write } = stub();
    render(() => <WidgetMakerGetDetail transport={transport} input={{ id: MAKER }} />);
    await waitFor(() => expect(screen.getByText("Northwind")).toBeDefined());
    write("Southwind");
    await waitFor(() => expect(screen.getByText("Southwind")).toBeDefined());
  });

  it("makes a bounded table load its rows again", async () => {
    const { transport, write, reads } = stub();
    render(() => <WidgetMakerListTable transport={transport} />);
    await waitFor(() => expect(screen.getByText("Northwind")).toBeDefined());
    write("Southwind");
    await waitFor(() => expect(screen.getByText("Southwind")).toBeDefined());
    expect(reads("widget-maker/list")).toBe(2);
  });

  it("makes a form's selector read its list again, and stops when the form goes", async () => {
    const { transport, write, reads, listeners } = stub();
    const { unmount } = render(() => <WidgetCreateForm transport={transport} />);
    await waitFor(() => expect(reads("widget-maker/query")).toBe(1));
    write("Southwind");
    await waitFor(() => expect(reads("widget-maker/query")).toBe(2));
    unmount();
    expect(listeners.size).toBe(0);
  });

  it("makes a table's label cell read the record it names again", async () => {
    const { transport, write, reads } = stub();
    render(() => <WidgetQueryTable transport={transport} />);
    await waitFor(() => expect(screen.getByText("Northwind")).toBeDefined());
    expect(reads("widget-maker/get")).toBe(1);
    write("Southwind");
    await waitFor(() => expect(screen.getByText("Southwind")).toBeDefined());
    expect(reads("widget-maker/get")).toBe(2);
  });
});
