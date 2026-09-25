/**
 * The testing method for a row that opens a form, used once.
 *
 * The table hands the values of one row to a callback, and the form renders
 * with those values already in its controls. Nothing here decides what
 * opening a row means: the page does.
 */

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import {
  WidgetCreateForm,
  WidgetRecordBatchForm,
  type WidgetCreateFormInitial,
} from "../fixture/components/widget.js";
import { WidgetMakerQueryTable } from "../fixture/components/widget_maker.js";
import { selector } from "./choose.js";
import { MAKER, prefillStub as stub } from "../stubs/index.js";

afterEach(cleanup);

describe("a row that opens a form", () => {
  it("hands the declared pair to the form, which starts with it", async () => {
    const transport = stub();
    let carried: WidgetCreateFormInitial | undefined;
    render(() => (
      <WidgetMakerQueryTable transport={transport} onFillWidgetCreate={(initial) => {
        carried = initial;
      }} />
    ));
    await waitFor(() => expect(screen.getByText("Northwind")).toBeDefined());

    fireEvent.click(screen.getByRole("button", { name: "create" }));
    expect(carried).toEqual({ makerId: MAKER });
    const initial = carried as WidgetCreateFormInitial;

    render(() => <WidgetCreateForm transport={transport} initial={initial} />);
    // The selector shows the row whose key the form started with.
    await waitFor(() => expect(selector("maker id").value).toBe("Northwind"));
  });

  it("fills no form in which two inputs name its model", async () => {
    // The batch form names a maker twice, as its maker and its inspector. No
    // declared path says which one a maker row is, so the row offers no batch
    // form, and the operator chooses both (wamn-6jcm). The callback is passed
    // untyped, because the table no longer declares it.
    const transport = stub();
    const batch: unknown[] = [];
    const untyped = { onFillWidgetRecordBatch: (initial: unknown) => batch.push(initial) };
    render(() => (
      <WidgetMakerQueryTable transport={transport} onFillWidgetCreate={() => {}} {...untyped} />
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
