/**
 * The testing method for a delete screen, used once.
 *
 * A removal cannot be undone, so the screen asks in an alert dialog first. It
 * reads the record when the operator confirms, and sends the revision it read.
 *
 * The subject is the fixture's delete screen, written by
 * `crates/schema/generator/src/client_component.rs`. The command
 * `check_client_components` writes it into `fixture/` before this runs.
 */

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import type { JsonValue, Outcome, Transport, WireRequest } from "@wamn/web-runtime";

import { WidgetDeleteDelete } from "../fixture/components/widget.js";

afterEach(cleanup);

const WIDGET = "0f1e2d3c-4b5a-4968-8778-695a4b3c2d1e";

/** One transport that answers the read with a record and the removal with an empty result. */
function stub(): { transport: Transport; sent: WireRequest[] } {
  const sent: WireRequest[] = [];
  return {
    sent,
    transport: {
      invoke: (request: WireRequest) => {
        sent.push(request);
        const reply: Outcome<JsonValue> = request.operation.includes("/get@")
          ? {
              status: "completed",
              value: {
                id: WIDGET,
                code: "standard",
                note: null,
                edit_version: "7",
                created_at: "2026-09-21T12:00:00.000000Z",
              },
            }
          : { status: "completed", value: {} };
        return Promise.resolve(reply);
      },
    },
  };
}

describe("the generated delete", () => {
  it("sends nothing when the operator cancels", async () => {
    const { transport, sent } = stub();
    render(() => <WidgetDeleteDelete transport={transport} key={{ id: WIDGET }} />);
    fireEvent.click(screen.getByRole("button", { name: "delete" }));
    await waitFor(() => expect(screen.getByRole("alertdialog")).toBeDefined());
    expect(screen.getByText("remove this record?")).toBeDefined();

    fireEvent.click(screen.getByRole("button", { name: "cancel" }));
    await waitFor(() => expect(screen.queryByRole("alertdialog")).toBeNull());
    expect(sent).toHaveLength(0);
  });

  it("removes the record with the revision it read once the operator confirms", async () => {
    const { transport, sent } = stub();
    const seen: Outcome<unknown>[] = [];
    render(() => (
      <WidgetDeleteDelete
        transport={transport}
        key={{ id: WIDGET }}
        onSubmitted={(outcome) => seen.push(outcome)}
      />
    ));
    fireEvent.click(screen.getByRole("button", { name: "delete" }));
    await waitFor(() => expect(screen.getByRole("alertdialog")).toBeDefined());

    fireEvent.click(screen.getByRole("button", { name: "confirm" }));
    await waitFor(() => expect(seen).toHaveLength(1));
    // One read, then one removal.
    expect(sent).toHaveLength(2);
    const removal = sent[1]?.items[0] as { [key: string]: JsonValue };
    expect(removal["id"]).toBe(WIDGET);
    expect(removal["expected_edit_version"]).toBe("7");
  });
});
