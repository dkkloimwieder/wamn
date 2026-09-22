/**
 * The testing method for a row that opens a form, used once.
 *
 * The table hands the values of one row to a callback, and the form renders
 * with those values already in its controls. Nothing here decides what
 * opening a row means: the page does.
 */

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import type { JsonValue, Outcome, Transport, WireRequest } from "@wamn/web-runtime";

import {
  WidgetCreateForm,
  type WidgetCreateFormInitial,
} from "../fixture/components/widget.js";
import { WidgetMakerQueryTable } from "../fixture/components/widget_maker.js";

afterEach(cleanup);

const MAKER = "0f1e2d3c-4b5a-4968-8778-695a4b3c2d1e";

function stub(): Transport {
  return {
    invoke: (request: WireRequest) => {
      const reply: Outcome<JsonValue> = request.operation.includes("widget-maker")
        ? {
            status: "completed",
            value: { item: [{ id: MAKER, name: "Northwind" }], nextCursor: null },
          }
        : { status: "completed", value: { rows: [] } };
      return Promise.resolve(reply);
    },
  };
}

describe("a row that opens a form", () => {
  it("hands the declared pair to the form, which starts with it", async () => {
    const transport = stub();
    let carried: WidgetCreateFormInitial | undefined;
    render(() => (
      <WidgetMakerQueryTable transport={transport} onFillWidgetCreate={(initial) => {
        carried = initial;
      }} />
    ));
    fireEvent.click(screen.getByText("read"));
    await waitFor(() => expect(screen.getByText("Northwind")).toBeDefined());

    fireEvent.click(screen.getByRole("button", { name: "create" }));
    expect(carried).toEqual({ makerId: MAKER });
    const initial = carried as WidgetCreateFormInitial;

    render(() => <WidgetCreateForm transport={transport} initial={initial} />);
    await waitFor(() => {
      const chosen = screen
        .getAllByRole("combobox")
        .map((control) => (control as HTMLSelectElement).value);
      expect(chosen).toContain(MAKER);
    });
  });
});
