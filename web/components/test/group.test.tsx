/**
 * The testing method for a repeated group, used once.
 *
 * The group's declared bounds reach the controls, and a value that travels as
 * text is checked against the spelling the release accepts before the request
 * goes out.
 */

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import { WidgetRecordBatchForm } from "../fixture/components/widget.js";
import { choose } from "./choose.js";
import { groupStub as stub } from "../stubs/index.js";

afterEach(cleanup);

describe("a repeated group", () => {
  it("stops adding at the declared maximum", async () => {
    const { transport } = stub();
    render(() => <WidgetRecordBatchForm transport={transport} valueExpectedEditVersion="1" />);
    const add = screen.getByRole("button", { name: "add" });
    for (let element = 0; element < 10; element += 1) {
      fireEvent.click(add);
    }
    await waitFor(() => expect((add as HTMLButtonElement).disabled).toBe(true));
  });

  it("reads no line until the maker that narrows the line list is chosen", async () => {
    const { transport, sent } = stub();
    render(() => <WidgetRecordBatchForm transport={transport} valueExpectedEditVersion="1" />);
    fireEvent.click(screen.getByRole("button", { name: "add" }));
    await waitFor(() =>
      expect(sent.some((request) => request.operation.includes("widget-maker"))).toBe(true),
    );
    const lineReads = () =>
      sent.filter((request) => !request.operation.includes("widget-maker")).length;
    // The line list has no input to read until a maker is chosen.
    expect(lineReads()).toBe(0);

    await choose("Maker", "Northwind");
    await choose("Line", "priority");
    expect(lineReads()).toBeGreaterThan(0);
  });

  it("reads the label its line bound declares", () => {
    const { transport } = stub();
    render(() => <WidgetRecordBatchForm transport={transport} valueExpectedEditVersion="1" />);
    expect(screen.getByText("Batch lines")).toBeDefined();
  });

  it("refuses a value whose spelling the release would refuse", async () => {
    const { transport, sent } = stub();
    render(() => <WidgetRecordBatchForm transport={transport} valueExpectedEditVersion="1" />);
    fireEvent.click(screen.getByRole("button", { name: "add" }));
    // Every other control is filled, so the quantity is the only value left
    // for the schema to refuse. Each one is chosen from its own list, and the
    // maker comes first, because the line list narrows by it.
    await choose("Maker", "Northwind");
    await choose("Line", "priority");
    fireEvent.input(screen.getByLabelText("Batch note"), { target: { value: "a note" } });

    const quantity = screen.getByLabelText("Quantity received") as HTMLInputElement;
    fireEvent.input(quantity, { target: { value: "1e5" } });
    fireEvent.submit(screen.getByRole("button", { name: "submit" }).closest("form")!);

    // The refusal marks the quantity control in place.
    await waitFor(() =>
      expect(screen.getAllByRole("alert").map((mark) => mark.textContent)).toContain(
        "expected decimal text",
      ),
    );
    expect(quantity.getAttribute("aria-invalid")).toBe("true");
    expect(sent.filter((request) => request.operation.includes("record-batch"))).toHaveLength(0);
  });
});
