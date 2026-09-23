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
import { choose, openSelector } from "./choose.js";

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
    await openSelector("maker id");
    await waitFor(() => expect(screen.getByRole("option", { name: "Northwind" })).toBeDefined());
    // The option stands for the row's key, and shows its display field.
    expect(screen.getByRole("option", { name: "Northwind" }).getAttribute("data-key")).toBe(MAKER);
  });

  it("sends the key of the row the operator picked", async () => {
    const { transport, sent } = stub();
    render(() => <WidgetCreateForm transport={transport} />);
    await choose("maker id", "Northwind");
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
    const input = await openSelector("maker id");
    await waitFor(() => expect(screen.getByRole("option", { name: "Northwind" })).toBeDefined());

    // The search goes out after a pause in typing, as one request.
    fireEvent.input(input, { target: { value: "Southwind" } });

    await waitFor(() => expect(screen.getByRole("option", { name: "Southwind" })).toBeDefined());
    expect(sent).toHaveLength(2);
    expect(screen.queryByRole("option", { name: "Northwind" })).toBeNull();
    const searched = sent[1]?.items[0] as { filter: { name: string[] }; cursor?: string };
    expect(searched.filter.name).toEqual(["Southwind"]);
    expect(searched.cursor).toBeUndefined();
  });

  it("keeps the typed search when a record is already chosen", async () => {
    const { transport, sent } = paged();
    render(() => <WidgetCreateForm transport={transport} />);
    await choose("maker id", "Northwind");
    const input = await openSelector("maker id");

    fireEvent.input(input, { target: { value: "Southwind" } });

    await waitFor(() => expect(sent).toHaveLength(2));
    // The reply replaces the options, and the input still shows the search.
    await openSelector("maker id");
    await waitFor(() => expect(screen.getByRole("option", { name: "Southwind" })).toBeDefined());
    expect(input.value).toBe("Southwind");
  });

  it("appends the next page to the options it already offers", async () => {
    const { transport, sent } = paged();
    render(() => <WidgetCreateForm transport={transport} />);
    await openSelector("maker id");
    await waitFor(() => expect(screen.getByRole("option", { name: "Northwind" })).toBeDefined());

    fireEvent.click(screen.getByLabelText("maker id next page"));

    await waitFor(() => expect(screen.getByRole("option", { name: "Southwind" })).toBeDefined());
    // The first page stays above the second.
    expect(screen.getAllByRole("option").map((option) => option.textContent)).toEqual([
      "Northwind",
      "Southwind",
    ]);
    expect((sent[1]?.items[0] as { cursor: string }).cursor).toBe("page-2");
  });
});
