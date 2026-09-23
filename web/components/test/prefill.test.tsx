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
    fireEvent.click(screen.getByText("read"));
    await waitFor(() => expect(screen.getByText("Northwind")).toBeDefined());

    fireEvent.click(screen.getByRole("button", { name: "create" }));
    expect(carried).toEqual({ makerId: MAKER });
    const initial = carried as WidgetCreateFormInitial;

    render(() => <WidgetCreateForm transport={transport} initial={initial} />);
    // The selector shows the row whose key the form started with.
    await waitFor(() => expect(selector("maker id").value).toBe("Northwind"));
  });
});
