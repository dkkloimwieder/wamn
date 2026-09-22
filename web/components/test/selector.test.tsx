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
