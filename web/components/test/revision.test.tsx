/**
 * The testing method for a selector that supplies a revision, used once.
 *
 * The guarded batch takes the revision of the inspector it names. A row
 * action can fill the inspector without a pick, so the revision comes from
 * the row that carries the filled key: a listed row, or the record read for a
 * key off the list (wamn-fuda).
 *
 * The subject is the guarded fixture's batch form, which
 * `check_client_components` writes into `fixture/guarded/` before this runs.
 */

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import type { JsonValue, Outcome } from "@wamn/web-runtime";

import { WidgetRecordBatchForm } from "../fixture/guarded/components/widget.js";
import { choose, selector } from "./choose.js";
import { MAKER, SOUTH, batchStub as stub } from "../stubs/index.js";

afterEach(cleanup);

/** Fills every other control, submits, and returns the batch that went out. */
async function submitBatch(inspector: string, shown: string) {
  const { transport, sent } = stub();
  const seen: Outcome<unknown>[] = [];
  render(() => (
    <WidgetRecordBatchForm
      transport={transport}
      initial={{ value: { grade: "first", inspectorId: inspector } }}
      onSubmitted={(outcome) => seen.push(outcome)}
    />
  ));
  await waitFor(() => expect(selector("Inspector").value).toBe(shown));
  fireEvent.click(screen.getByRole("button", { name: "add" }));
  await choose("Maker", "Northwind");
  await choose("Line", "priority");
  fireEvent.input(screen.getByLabelText("Batch note"), { target: { value: "a note" } });
  fireEvent.input(screen.getByLabelText("Quantity received"), { target: { value: "5" } });
  fireEvent.submit(screen.getByRole("button", { name: "submit" }).closest("form")!);
  await waitFor(() => expect(seen).toHaveLength(1));
  const batch = sent.find((request) => request.operation.includes("record-batch"));
  return (batch?.items[0] as { value: { [key: string]: JsonValue } }).value;
}

describe("a filled selector that supplies a revision", () => {
  it("sends the revision of the listed row that carries the filled key", async () => {
    const value = await submitBatch(MAKER, "Northwind");
    expect(value["inspector_id"]).toBe(MAKER);
    expect(value["expected_edit_version"]).toBe("1");
  });

  it("sends the revision of the record read for a key off the list", async () => {
    const value = await submitBatch(SOUTH, "Southwind");
    expect(value["inspector_id"]).toBe(SOUTH);
    expect(value["expected_edit_version"]).toBe("3");
  });
});
