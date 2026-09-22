/**
 * The testing method for a populated input, used once.
 *
 * A selector reads the list the plan chose and offers its rows. The test
 * renders the emitted form with a stub transport, picks one option, and reads
 * the key that the submission carried.
 *
 * The subject is the fixture's create form, written by
 * `crates/schema/generator/src/client_component.rs`. The command
 * `check_client_components` writes it into `fixture/` before this runs.
 */

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import type { JsonValue, Outcome, Transport, WireRequest } from "@wamn/web-runtime";

import { WidgetCreateForm } from "../fixture/components/widget.js";

afterEach(cleanup);

const MAKER = "0f1e2d3c-4b5a-4968-8778-695a4b3c2d1e";

/** One transport that answers each operation from its own reply. */
function stub(): { transport: Transport; sent: WireRequest[] } {
  const sent: WireRequest[] = [];
  return {
    sent,
    transport: {
      invoke: (request: WireRequest) => {
        sent.push(request);
        const reply: Outcome<JsonValue> = request.operation.includes("widget-maker")
          ? {
              status: "completed",
              value: { item: [{ id: MAKER, name: "Northwind" }], nextCursor: null },
            }
          : { status: "completed", value: { id: "written", edit_version: 1 } };
        return Promise.resolve(reply);
      },
    },
  };
}

describe("a populated input", () => {
  it("offers the rows of the list the plan chose", async () => {
    const { transport } = stub();
    render(() => <WidgetCreateForm transport={transport} />);
    await waitFor(() => expect(screen.getByRole("option", { name: "Northwind" })).toBeDefined());
    const option = screen.getByRole("option", { name: "Northwind" }) as HTMLOptionElement;
    expect(option.value).toBe(MAKER);
  });

  it("sends the key of the row the operator picked", async () => {
    const { transport, sent } = stub();
    render(() => <WidgetCreateForm transport={transport} />);
    await waitFor(() => expect(screen.getByRole("option", { name: "Northwind" })).toBeDefined());

    const selector = screen.getByRole("combobox") as HTMLSelectElement;
    fireEvent.change(selector, { target: { value: MAKER } });
    fireEvent.submit(screen.getByRole("button", { name: "submit" }).closest("form")!);

    await waitFor(() => expect(sent).toHaveLength(2));
    const submission = sent[1]?.items[0] as { [key: string]: JsonValue };
    expect(submission["maker_id"]).toBe(MAKER);
  });
});

const SOUTH = "1a2b3c4d-5e6f-4a7b-8c9d-0e1f2a3b4c5d";

/** One transport that answers a search and a next page from the same list. */
function paged(): { transport: Transport; sent: WireRequest[] } {
  const sent: WireRequest[] = [];
  return {
    sent,
    transport: {
      invoke: (request: WireRequest) => {
        sent.push(request);
        if (!request.operation.includes("widget-maker")) {
          return Promise.resolve<Outcome<JsonValue>>({
            status: "completed",
            value: { id: "written", edit_version: 1 },
          });
        }
        const item = request.items[0] as {
          filter?: { name?: string[] };
          cursor?: string;
        };
        if (item.filter?.name !== undefined) {
          return Promise.resolve<Outcome<JsonValue>>({
            status: "completed",
            value: { item: [{ id: SOUTH, name: "Southwind" }], nextCursor: null },
          });
        }
        if (item.cursor === "page-2") {
          return Promise.resolve<Outcome<JsonValue>>({
            status: "completed",
            value: { item: [{ id: SOUTH, name: "Southwind" }], nextCursor: null },
          });
        }
        return Promise.resolve<Outcome<JsonValue>>({
          status: "completed",
          value: { item: [{ id: MAKER, name: "Northwind" }], nextCursor: "page-2" },
        });
      },
    },
  };
}

describe("a selector over a list that declares its display filter", () => {
  it("searches by the value the operator typed", async () => {
    const { transport, sent } = paged();
    render(() => <WidgetCreateForm transport={transport} />);
    await waitFor(() => expect(screen.getByRole("option", { name: "Northwind" })).toBeDefined());

    const search = screen.getByLabelText("maker id search") as HTMLInputElement;
    fireEvent.change(search, { target: { value: "Southwind" } });

    await waitFor(() => expect(screen.getByRole("option", { name: "Southwind" })).toBeDefined());
    expect(screen.queryByRole("option", { name: "Northwind" })).toBeNull();
    const searched = sent[1]?.items[0] as { filter: { name: string[] }; cursor?: string };
    expect(searched.filter.name).toEqual(["Southwind"]);
    expect(searched.cursor).toBeUndefined();
  });

  it("appends the next page to the options it already offers", async () => {
    const { transport, sent } = paged();
    render(() => <WidgetCreateForm transport={transport} />);
    await waitFor(() => expect(screen.getByRole("option", { name: "Northwind" })).toBeDefined());

    fireEvent.click(screen.getByLabelText("maker id next page"));

    await waitFor(() => expect(screen.getByRole("option", { name: "Southwind" })).toBeDefined());
    expect(screen.getByRole("option", { name: "Northwind" })).toBeDefined();
    expect((sent[1]?.items[0] as { cursor: string }).cursor).toBe("page-2");
  });
});
