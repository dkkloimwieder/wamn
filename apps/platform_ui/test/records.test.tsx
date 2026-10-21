import { fireEvent, render, screen, within } from "@solidjs/testing-library";
import { createSignal, flush } from "solid-js";
import { describe, expect, it } from "vitest";
import { Records } from "../src/records";

import "./fixtures";

describe("Records pagination", () => {
  it("pages returned rows, keeps row actions attached, and handles shrinking and empty results", () => {
    const all = Array.from({ length: 51 }, (_, index) => ({ id: String(index + 1) }));
    const [rows, setRows] = createSignal(all);
    let opened = "";
    render(() => (
      <Records
        refresh={() => {}}
        id="records"
        title="Records"
        rows={rows()}
        rowKey={(row) => row.id}
        columns={[{ id: "id", header: "Record" }]}
        cells={{ id: (cell) => <button onClick={() => (opened = cell.row.id)}>{cell.row.id}</button> }}
      />
    ));
    const table = screen.getByRole("table", { name: "Records" });
    const pages = within(screen.getByRole("navigation", { name: "Table pages" }));
    const previous = pages.getByRole("button", { name: "Previous page" }) as HTMLButtonElement;
    const next = pages.getByRole("button", { name: "Next page" }) as HTMLButtonElement;
    const visible = () =>
      within(table)
        .queryAllByRole("button")
        .map((button) => button.textContent);
    expect(visible()).toEqual(all.slice(0, 25).map((row) => row.id));
    expect(pages.getByRole("status").textContent).toBe("Page 1 of 3");
    expect(previous.disabled).toBe(true);
    fireEvent.click(next);
    flush();
    expect(visible()).toEqual(all.slice(25, 50).map((row) => row.id));
    fireEvent.click(within(table).getByRole("button", { name: "26" }));
    expect(opened).toBe("26");
    fireEvent.click(next);
    flush();
    expect(visible()).toEqual(["51"]);
    expect(next.disabled).toBe(true);
    expect(pages.getByRole("status").textContent).toBe("Page 3 of 3");
    fireEvent.click(previous);
    flush();
    expect(visible()).toEqual(all.slice(25, 50).map((row) => row.id));
    setRows(all.slice(0, 2));
    flush();
    expect(visible()).toEqual(["1", "2"]);
    expect(previous.disabled).toBe(true);
    expect(next.disabled).toBe(true);
    expect(pages.getByRole("status").textContent).toBe("Page 1 of 1");
    setRows([]);
    flush();
    expect(visible()).toEqual([]);
    expect(previous.disabled).toBe(true);
    expect(next.disabled).toBe(true);
  });
  it("keeps all child rows reachable through the shared pagination and refreshes expanded details", () => {
    const [children, setChildren] = createSignal(Array.from({ length: 26 }, (_, index) => String(index + 1)));
    render(() => (
      <Records
        refresh={() => {}}
        id="parents"
        title="Parents"
        rows={["parent"]}
        rowKey={(row) => row}
        columns={[{ id: "name", header: "Name", accessorFn: (row) => row }]}
        details={() => (
          <Records
            refresh={() => {}}
            id="children"
            title="Children"
            rows={children()}
            rowKey={(row) => row}
            columns={[{ id: "name", header: "Name", accessorFn: (row) => row }]}
          />
        )}
      />
    ));
    fireEvent.click(screen.getByRole("button", { name: "Expand details for row parent" }));
    flush();
    const child = within(screen.getByRole("table", { name: "Children" }));
    expect(child.getAllByRole("cell")).toHaveLength(25);
    expect(screen.getAllByRole("button", { name: "Refresh results" })).toHaveLength(1);
    const pages = screen.getAllByRole("navigation", { name: "Table pages" });
    expect(pages).toHaveLength(2);
    const childPages = pages.find((page) => page.closest("td"))!;
    fireEvent.click(within(childPages).getByRole("button", { name: "Next page" }));
    flush();
    expect(child.getAllByRole("cell").map((cell) => cell.textContent)).toEqual(["26"]);
    setChildren(["changed"]);
    flush();
    expect(child.getAllByRole("cell").map((cell) => cell.textContent)).toEqual(["changed"]);
    expect(screen.getAllByRole("navigation", { name: "Table pages" })).toHaveLength(1);
  });
});
