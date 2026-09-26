/**
 * The testing method for a populated input, used once.
 *
 * A selector reads the list the plan chose and offers its rows. The test
 * renders the emitted form with a stub transport, picks one option, and reads
 * the key that the submission carried.
 *
 * The subject is the fixture's create form, written by
 * `crates/schema/generator/src/client_component.rs`. The command
 * `check_client_components` writes it into `fixture/` before this runs.
 */

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { JsonValue } from "@wamn/web-runtime";

import { WidgetCreateForm } from "../fixture/components/widget.js";
import { choose, openSelector, selector } from "./choose.js";
import { MAKER, SOUTH, paged, selectorStub as stub } from "../stubs/index.js";

afterEach(cleanup);

describe("a populated input", () => {
  it("offers the rows of the list the plan chose", async () => {
    const { transport } = stub();
    render(() => <WidgetCreateForm transport={transport} />);
    await openSelector("maker id");
    await waitFor(() => expect(screen.getByRole("option", { name: "Northwind" })).toBeDefined());
    // The option stands for the row's key, and shows its display field.
    expect(screen.getByRole("option", { name: "Northwind" }).getAttribute("data-key")).toBe(MAKER);
  });

  it("sends the key of the row the operator picked", async () => {
    const { transport, sent } = stub();
    // The widget code is required, so it starts filled and the maker is the
    // one value the operator picks.
    render(() => <WidgetCreateForm transport={transport} initial={{ code: "standard" }} />);
    await choose("maker id", "Northwind");
    fireEvent.submit(screen.getByRole("button", { name: "submit" }).closest("form")!);

    await waitFor(() => expect(sent).toHaveLength(2));
    const submission = sent[1]?.items[0] as { [key: string]: JsonValue };
    expect(submission["maker_id"]).toBe(MAKER);
  });
});

describe("a selector that holds a record its list did not return", () => {
  it("reads that record and shows its text (wamn-1jrv)", async () => {
    const { transport, sent } = paged();
    // A row action filled Southwind, which is on the second page.
    render(() => (
      <WidgetCreateForm transport={transport} initial={{ code: "standard", makerId: SOUTH }} />
    ));
    await waitFor(() => expect(selector("maker id").value).toBe("Southwind"));
    const reads = sent.filter((request) => request.operation.includes("widget-maker/get@"));
    expect(reads.map((request) => (request.items[0] as { id: string }).id)).toEqual([SOUTH]);
  });

  it("reads the value inside the owner, so the read leaks no computation (wamn-16ii)", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    try {
      const { transport } = paged();
      render(() => (
        <WidgetCreateForm transport={transport} initial={{ code: "standard", makerId: SOUTH }} />
      ));
      await waitFor(() => expect(selector("maker id").value).toBe("Southwind"));
      const leaks = warn.mock.calls.filter((call) => String(call[0]).includes("outside a `createRoot`"));
      expect(leaks).toEqual([]);
    } finally {
      warn.mockRestore();
    }
  });
});

describe("a selector over a list that declares its display filter", () => {
  it("searches by the value the operator typed", async () => {
    const { transport, sent } = paged();
    render(() => <WidgetCreateForm transport={transport} />);
    const input = await openSelector("maker id");
    await waitFor(() => expect(screen.getByRole("option", { name: "Northwind" })).toBeDefined());

    // The search goes out after a pause in typing, as one request.
    fireEvent.input(input, { target: { value: "Southwind" } });

    await waitFor(() => expect(screen.getByRole("option", { name: "Southwind" })).toBeDefined());
    expect(sent).toHaveLength(2);
    expect(screen.queryByRole("option", { name: "Northwind" })).toBeNull();
    const searched = sent[1]?.items[0] as { filter: { name: string[] }; cursor?: string };
    expect(searched.filter.name).toEqual(["Southwind"]);
    expect(searched.cursor).toBeUndefined();
  });

  it("keeps the typed search when a record is already chosen", async () => {
    const { transport, sent } = paged();
    render(() => <WidgetCreateForm transport={transport} />);
    await choose("maker id", "Northwind");
    const input = await openSelector("maker id");

    fireEvent.input(input, { target: { value: "Southwind" } });

    await waitFor(() => expect(sent).toHaveLength(2));
    // The reply replaces the options, and the input still shows the search.
    await openSelector("maker id");
    await waitFor(() => expect(screen.getByRole("option", { name: "Southwind" })).toBeDefined());
    expect(input.value).toBe("Southwind");
  });

  it("appends the next page to the options it already offers", async () => {
    const { transport, sent } = paged();
    render(() => <WidgetCreateForm transport={transport} />);
    await openSelector("maker id");
    await waitFor(() => expect(screen.getByRole("option", { name: "Northwind" })).toBeDefined());

    fireEvent.click(screen.getByLabelText("maker id next page"));

    await waitFor(() => expect(screen.getByRole("option", { name: "Southwind" })).toBeDefined());
    // The first page stays above the second.
    expect(screen.getAllByRole("option").map((option) => option.textContent)).toEqual([
      "Northwind",
      "Southwind",
    ]);
    expect((sent[1]?.items[0] as { cursor: string }).cursor).toBe("page-2");
  });
});
