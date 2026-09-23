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

import type { JsonValue, Outcome } from "@wamn/web-runtime";

import { WidgetDeleteDelete } from "../fixture/components/widget.js";
import { WIDGET, deleteStub as stub } from "../stubs/index.js";

afterEach(cleanup);

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
