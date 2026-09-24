/**
 * The testing method for a delete screen, used once.
 *
 * A removal cannot be undone, so the screen asks in an alert dialog first. It
 * sends the key and the revision of the record the page displayed, and reads
 * nothing, so a change that a second writer makes after the page read the
 * record refuses as a conflict, and the screen reports it (wamn-k2d4).
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

/** The record as a table row displayed it. */
const DISPLAYED = { id: WIDGET, editVersion: "7" };

const confirm = async () => {
  fireEvent.click(screen.getByRole("button", { name: "delete" }));
  await waitFor(() => expect(screen.getByRole("alertdialog")).toBeDefined());
  fireEvent.click(screen.getByRole("button", { name: "confirm" }));
};

describe("the generated delete", () => {
  it("sends nothing when the operator cancels", async () => {
    const { transport, sent } = stub();
    render(() => <WidgetDeleteDelete transport={transport} record={DISPLAYED} />);
    fireEvent.click(screen.getByRole("button", { name: "delete" }));
    await waitFor(() => expect(screen.getByRole("alertdialog")).toBeDefined());
    expect(screen.getByText("remove this record?")).toBeDefined();

    fireEvent.click(screen.getByRole("button", { name: "cancel" }));
    await waitFor(() => expect(screen.queryByRole("alertdialog")).toBeNull());
    expect(sent).toHaveLength(0);
  });

  it("removes the record with the revision the page displayed once the operator confirms", async () => {
    const { transport, sent } = stub();
    const seen: Outcome<unknown>[] = [];
    render(() => (
      <WidgetDeleteDelete
        transport={transport}
        record={DISPLAYED}
        onSubmitted={(outcome) => seen.push(outcome)}
      />
    ));
    await confirm();
    await waitFor(() => expect(seen).toHaveLength(1));
    // One removal, and no read.
    expect(sent).toHaveLength(1);
    const removal = sent[0]?.items[0] as { [key: string]: JsonValue };
    expect(removal["id"]).toBe(WIDGET);
    expect(removal["expected_edit_version"]).toBe("7");
    expect(seen[0]?.status).toBe("completed");
  });

  it("reports a conflict when a second writer changed the record after the page displayed it", async () => {
    const { transport, sent, write } = stub();
    const seen: Outcome<unknown>[] = [];
    render(() => (
      <WidgetDeleteDelete
        transport={transport}
        record={DISPLAYED}
        onSubmitted={(outcome) => seen.push(outcome)}
      />
    ));
    write();
    await confirm();
    await waitFor(() => expect(seen).toHaveLength(1));

    expect(sent).toHaveLength(1);
    expect(seen[0]).toMatchObject({ status: "refused", code: "concurrency_conflict" });
    await waitFor(() =>
      expect(screen.getAllByText("concurrency_conflict").length).toBeGreaterThan(0),
    );
  });
});
