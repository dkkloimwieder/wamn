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
  WidgetListTable,
  WidgetRecordBatchForm,
  type WidgetCreateFormInitial,
  type WidgetRecordBatchFormInitial,
} from "../fixture/components/widget.js";
import { WidgetMakerQueryTable } from "../fixture/components/widget_maker.js";
import { selector } from "./choose.js";
import { MAKER, WIDGET, prefillStub as stub } from "../stubs/index.js";

afterEach(cleanup);

describe("a row that opens a form", () => {
  it("hands the declared pair to the form, which starts with it", async () => {
    const transport = stub();
    let carried: WidgetCreateFormInitial | undefined;
    render(() => (
      <WidgetMakerQueryTable transport={transport} onFill={{ "platform-fixture:widget/create@1.0.0": (initial) => {
        carried = initial;
      } }} />
    ));
    await waitFor(() => expect(screen.getByText("Northwind")).toBeDefined());

    fireEvent.click(screen.getByRole("button", { name: "create" }));
    expect(carried).toEqual({ makerId: MAKER });
    const initial = carried as WidgetCreateFormInitial;

    render(() => <WidgetCreateForm transport={transport} initial={initial} />);
    // The selector shows the row whose key the form started with.
    await waitFor(() => expect(selector("maker id").value).toBe("Northwind"));
  });

  it("fills one element of a repeated group, which the form shows as one line", async () => {
    // The batch form holds its lines as a list, so a widget row fills the
    // widget of one line, never an object in place of the list (wamn-yviq).
    const widget = { id: WIDGET, code: "standard", attributes: null, edit_version: "1" };
    const transport: Transport = {
      invoke: (request: WireRequest) => {
        const reply: Outcome<JsonValue> = request.operation.includes("widget/list")
          ? { status: "completed", value: { rows: [widget] } }
          : { status: "completed", value: { item: [], nextCursor: null } };
        return Promise.resolve(reply);
      },
    };
    let carried: WidgetRecordBatchFormInitial | undefined;
    render(() => (
      <WidgetListTable transport={transport} onFill={{ "platform-fixture:widget/record-batch@1.0.0": (initial) => {
        carried = initial;
      } }} />
    ));
    await waitFor(() => expect(screen.getByText("standard")).toBeDefined());

    fireEvent.click(screen.getByRole("button", { name: "record-batch" }));
    expect(carried).toEqual({ value: { line: [{ widgetId: WIDGET }] } });
    cleanup();

    const initial = carried as WidgetRecordBatchFormInitial;
    render(() => <WidgetRecordBatchForm transport={transport} initial={initial} />);
    expect(selector("Line")).toBeDefined();
  });

  it("fills no form in which two inputs name its model", async () => {
    // The batch form names a maker twice, as its maker and its inspector. No
    // declared path says which one a maker row is, so the row offers no batch
    // form, and the operator chooses both (wamn-6jcm). A handler for it shows
    // no button, because the definition names no such action.
    const transport = stub();
    const batch: unknown[] = [];
    render(() => (
      <WidgetMakerQueryTable
        transport={transport}
        onFill={{ "platform-fixture:widget/create@1.0.0": () => {}, "platform-fixture:widget/record-batch@1.0.0": (initial) => void batch.push(initial) }}
      />
    ));
    await waitFor(() => expect(screen.getByText("Northwind")).toBeDefined());

    expect(screen.getByRole("button", { name: "create" })).toBeDefined();
    expect(screen.queryByRole("button", { name: "record-batch" })).toBeNull();
    expect(batch).toHaveLength(0);

    render(() => <WidgetRecordBatchForm transport={transport} />);
    expect(selector("Maker").value).toBe("");
    expect(selector("Inspector").value).toBe("");
  });
});
