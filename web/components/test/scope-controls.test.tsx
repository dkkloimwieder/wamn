/**
 * The scope bar renders a control for each filter mode, the server search and
 * the band, and each control reaches the request in its contract shape
 * (wamn-3nsf.5). The tables are the fixture's emitted definitions: the widget
 * filters its note by prefix and its maker by whether it is empty, and the
 * maker has a required band of 30 days and a search over its name.
 */

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import { QueryTable } from "@wamn/ui";

import { WIDGET_QUERY_TABLE } from "../fixture/components/widget.js";
import { WIDGET_MAKER_QUERY_TABLE } from "../fixture/components/widget_maker.js";
import { page, tableStub } from "../stubs/index.js";
import { theButton } from "./dom.js";
import { queryTable } from "./tables.js";

afterEach(() => {
  cleanup();
  window.history.replaceState(null, "", "/");
});

/** The item of the last read the table sent. */
function lastItem(sent: readonly { readonly items: readonly unknown[] }[]) {
  return sent.at(-1)?.items[0] as Record<string, unknown>;
}

function enter(input: HTMLInputElement, value: string) {
  input.value = value;
  fireEvent.keyDown(input, { key: "Enter" });
}

describe("the scope controls of the widget table", () => {
  it("sends a prefix as a list and an is-null choice as one boolean", async () => {
    const { transport, sent } = tableStub([page(["w1"], null)]);
    render(() => <QueryTable definition={WIDGET_QUERY_TABLE} transport={transport} label="widgets" />);
    await waitFor(() => expect(sent.length).toBe(1));

    const note = screen.getByPlaceholderText("add a start") as HTMLInputElement;
    enter(note, "N-");
    await waitFor(() => expect(sent.length).toBe(2));
    expect(lastItem(sent)["filter"]).toEqual({ note: ["N-"] });

    fireEvent.click(theButton("is empty"));
    await waitFor(() => expect(sent.length).toBe(3));
    expect(lastItem(sent)["filter"]).toEqual({ note: ["N-"], maker_id: true });

    fireEvent.click(theButton("has a value"));
    await waitFor(() => expect(sent.length).toBe(4));
    expect(lastItem(sent)["filter"]).toEqual({ note: ["N-"], maker_id: false });

    fireEvent.click(theButton("any"));
    await waitFor(() => expect(sent.length).toBe(5));
    expect(lastItem(sent)["filter"]).toEqual({ note: ["N-"] });
  });
});

describe("the scope controls of the widget maker table", () => {
  it("reads the default band until its start is set, and resets to it", async () => {
    const { transport, sent } = tableStub([page(["m1"], null)]);
    render(() => <QueryTable definition={WIDGET_MAKER_QUERY_TABLE} transport={transport} label="makers" />);
    await waitFor(() => expect(sent.length).toBe(1));
    // The first read leaves the band out, so the server reads its default.
    expect(lastItem(sent)["filter"]).toBeUndefined();
    expect(screen.getByText("last 30 days")).toBeDefined();

    const from = screen.getByLabelText("created at from") as HTMLInputElement;
    from.value = "2026-09-01T00:00";
    fireEvent.change(from);
    await waitFor(() => expect(sent.length).toBe(2));
    expect(lastItem(sent)["filter"]).toEqual({
      created_at: { min: new Date("2026-09-01T00:00").toISOString() },
    });

    fireEvent.click(theButton("reset to last 30 days"));
    await waitFor(() => expect(sent.length).toBe(3));
    expect(lastItem(sent)["filter"]).toBeUndefined();
  });

  it("does not send a band that has only its end", async () => {
    const { transport, sent } = tableStub([page(["m1"], null)]);
    render(() => <QueryTable definition={WIDGET_MAKER_QUERY_TABLE} transport={transport} label="makers" />);
    await waitFor(() => expect(sent.length).toBe(1));
    const to = screen.getByLabelText("created at to") as HTMLInputElement;
    to.value = "2026-09-30T00:00";
    fireEvent.change(to);
    await waitFor(() => expect(sent.length).toBe(2));
    expect(lastItem(sent)["filter"]).toBeUndefined();
  });

  it("sends the server search on Enter", async () => {
    const { transport, sent } = tableStub([page(["m1"], null)]);
    render(() => <QueryTable definition={WIDGET_MAKER_QUERY_TABLE} transport={transport} label="makers" />);
    await waitFor(() => expect(sent.length).toBe(1));
    enter(screen.getByLabelText("server search") as HTMLInputElement, "acme");
    await waitFor(() => expect(sent.length).toBe(2));
    expect(lastItem(sent)["search"]).toBe("acme");
    enter(screen.getByLabelText("server search") as HTMLInputElement, "");
    await waitFor(() => expect(sent.length).toBe(3));
    expect(lastItem(sent)["search"]).toBeUndefined();
  });
});

describe("a scope in the URL", () => {
  const SCOPE = "?lots.scope.at.min=2026-09-01T00:00&lots.scope.note.empty=true&lots.find=acme";

  it("reads a range, an is-null choice and the server search before the first load, and writes them back the same", async () => {
    window.history.replaceState(null, "", `/${SCOPE}`);
    const { asked } = await queryTable({
      name: "lots",
      columns: [
        { field: "note", label: "note", type: "text" },
        { field: "at", label: "at", type: "timestamptz" },
        { field: "id", label: "id", type: "uuid", role: "key" },
      ],
      rows: () => [],
      urlKey: "lots",
      definition: (definition) => ({
        ...definition,
        filters: [
          { field: "at", input: ["filter", "at"], list: false, match: "range", type: "timestamptz" },
          { field: "note", input: ["filter", "note"], list: false, match: "is_null" },
        ],
        scopeFilters: ["at", "note"],
        search: { input: ["search"], fields: ["note"] },
      }),
    });
    expect(asked).toEqual([
      {
        limit: 1000,
        filter: { at: { min: new Date("2026-09-01T00:00").toISOString() }, note: true },
        search: "acme",
      },
    ]);
    expect(decodeURIComponent(window.location.search)).toBe(SCOPE);
    expect((screen.getByLabelText("at from") as HTMLInputElement).value).toBe("2026-09-01T00:00");
    expect(theButton("is empty").getAttribute("aria-pressed")).toBe("true");
    expect((screen.getByLabelText("server search") as HTMLInputElement).value).toBe("acme");
  });
});
