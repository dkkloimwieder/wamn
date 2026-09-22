/**
 * The testing method for a repeated group, used once.
 *
 * The group's declared bounds reach the controls, and a value that travels as
 * text is checked against the spelling the release accepts before the request
 * goes out.
 */

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import type { JsonValue, Outcome, Transport, WireRequest } from "@wamn/web-runtime";

import { WidgetRecordBatchForm } from "../fixture/components/widget.js";

afterEach(cleanup);

const WIDGET = "0f1e2d3c-4b5a-4968-8778-695a4b3c2d1e";

function stub(): { transport: Transport; sent: WireRequest[] } {
  const sent: WireRequest[] = [];
  return {
    sent,
    transport: {
      invoke: (request: WireRequest) => {
        sent.push(request);
        const reply: Outcome<JsonValue> = {
          status: "completed",
          value: { rows: [{ id: WIDGET, code: "priority", name: "Northwind" }] },
        };
        return Promise.resolve(reply);
      },
    },
  };
}

describe("a repeated group", () => {
  it("stops adding at the declared maximum", async () => {
    const { transport } = stub();
    render(() => <WidgetRecordBatchForm transport={transport} />);
    const add = screen.getByRole("button", { name: "add" });
    for (let element = 0; element < 10; element += 1) {
      fireEvent.click(add);
    }
    await waitFor(() => expect((add as HTMLButtonElement).disabled).toBe(true));
  });

  it("reads the label its line bound declares", () => {
    const { transport } = stub();
    render(() => <WidgetRecordBatchForm transport={transport} />);
    expect(screen.getByText("Batch lines")).toBeDefined();
  });

  it("refuses a value whose spelling the release would refuse", async () => {
    const { transport, sent } = stub();
    render(() => <WidgetRecordBatchForm transport={transport} />);
    fireEvent.click(screen.getByRole("button", { name: "add" }));
    // Every other control is filled, so the quantity is the only value left
    // for the schema to refuse. Each one is chosen from its own list, and the
    // maker comes first, because the line list narrows by it.
    const maker = screen.getByLabelText("Maker") as HTMLSelectElement;
    await waitFor(() => expect(maker.options.length).toBeGreaterThan(1));
    fireEvent.change(maker, { target: { value: WIDGET } });
    const line = screen.getByLabelText("Line") as HTMLSelectElement;
    await waitFor(() => expect(line.options.length).toBeGreaterThan(1));
    fireEvent.change(line, { target: { value: WIDGET } });
    fireEvent.input(screen.getByLabelText("Batch note"), { target: { value: "a note" } });

    const quantity = screen.getByLabelText("Quantity received") as HTMLInputElement;
    fireEvent.input(quantity, { target: { value: "1e5" } });
    fireEvent.submit(screen.getByRole("button", { name: "submit" }).closest("form")!);

    await waitFor(() =>
      expect(screen.getAllByRole("emphasis").map((mark) => mark.textContent)).toContain(
        "expected decimal text",
      ),
    );
    expect(sent.filter((request) => request.operation.includes("record-batch"))).toHaveLength(0);
  });
});
