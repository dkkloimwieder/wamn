/**
 * The testing method for an update form, used once.
 *
 * The form reads its record when it opens and sends the revision of that
 * read. It reads nothing at submit, so a change that a second writer makes in
 * between refuses as a conflict, and the form reports it (wamn-yzy7).
 *
 * The subject is the fixture's update form, written by
 * `crates/schema/generator/src/client_component.rs`. The command
 * `check_client_components` writes it into `fixture/` before this runs.
 */

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import type { JsonValue, Outcome, WireRequest } from "@wamn/web-runtime";

import { WidgetUpdateForm } from "../fixture/components/widget.js";
import { WIDGET, updateStub as stub } from "../stubs/index.js";

afterEach(cleanup);

const reads = (sent: WireRequest[]) =>
  sent.filter((request) => request.operation.includes("/get@")).length;

const updates = (sent: WireRequest[]) =>
  sent.filter((request) => request.operation.includes("/update@"));

const revision = (request: WireRequest | undefined) =>
  (request?.items[0] as { [key: string]: JsonValue } | undefined)?.["expected_edit_version"];

describe("the generated update", () => {
  it("reports a conflict when a second writer changed the record after the form read it", async () => {
    const { transport, sent, write } = stub();
    const seen: Outcome<unknown>[] = [];
    render(() => (
      <WidgetUpdateForm
        transport={transport}
        key={{ id: WIDGET }}
        initial={{ change: { note: "first writer" } }}
        onSubmitted={(outcome) => seen.push(outcome)}
      />
    ));
    await waitFor(() => expect(reads(sent)).toBe(1));

    write();
    fireEvent.click(screen.getByRole("button", { name: "submit" }));
    await waitFor(() => expect(seen).toHaveLength(1));

    // The submission read nothing, and sent the revision the form opened with.
    expect(reads(sent)).toBe(1);
    expect(updates(sent).map(revision)).toEqual(["7"]);
    expect(seen[0]).toMatchObject({ status: "refused", code: "concurrency_conflict" });
    await waitFor(() =>
      expect(screen.getAllByText("concurrency_conflict").length).toBeGreaterThan(0),
    );
  });

  it("sends the revision its own completed write moved to", async () => {
    const { transport, sent } = stub();
    const seen: Outcome<unknown>[] = [];
    render(() => (
      <WidgetUpdateForm
        transport={transport}
        key={{ id: WIDGET }}
        initial={{ change: { note: "twice" } }}
        onSubmitted={(outcome) => seen.push(outcome)}
      />
    ));
    await waitFor(() => expect(reads(sent)).toBe(1));

    fireEvent.click(screen.getByRole("button", { name: "submit" }));
    await waitFor(() => expect(seen).toHaveLength(1));
    // After its own write, the form reads the record again.
    await waitFor(() => expect(reads(sent)).toBe(2));

    fireEvent.click(screen.getByRole("button", { name: "submit" }));
    await waitFor(() => expect(seen).toHaveLength(2));
    expect(updates(sent).map(revision)).toEqual(["7", "8"]);
    expect(seen.map((outcome) => outcome.status)).toEqual(["completed", "completed"]);
  });
});
