import { fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { createSignal, flush } from "solid-js";
import { describe, expect, it } from "vitest";
import { createServerPage, Records, type ServerPage } from "../src/records";

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

describe("Records server pages", () => {
  it("keeps accepted rows on failure, follows cursor history, and resets on reload", async () => {
    const requests: { limit: number; cursor?: string }[] = [];
    const waiting: { resolve: (page: ServerPage<{ id: string }>) => void; reject: (error: Error) => void }[] = [];
    let page!: ReturnType<typeof createServerPage<{ id: string }>>;
    function Harness() {
      page = createServerPage<{ id: string }>((input) => {
        requests.push(input);
        return new Promise((resolve, reject) => waiting.push({ resolve, reject }));
      });
      return (
        <Records
          id="server"
          title="Server"
          rows={page.rows()}
          paging={page}
          refresh={page.refresh}
          rowKey={(row) => row.id}
          columns={[{ id: "id", header: "ID", accessorKey: "id" }]}
        />
      );
    }
    render(() => <Harness />);
    void page.refresh();
    flush();
    const next = () => screen.getByRole("button", { name: "Next page" }) as HTMLButtonElement;
    const previous = () => screen.getByRole("button", { name: "Previous page" }) as HTMLButtonElement;
    expect(requests).toEqual([{ limit: 25 }]);
    expect(next().disabled).toBe(true);
    waiting.shift()!.resolve({ rows: [{ id: "first" }], nextCursor: "second", total: null });
    await screen.findByRole("cell", { name: "first" });
    expect(page.pagination()).not.toHaveProperty("totalRows");
    expect(within(screen.getByRole("navigation", { name: "Table pages" })).getByRole("status").textContent).toBe(
      "Page 1",
    );
    fireEvent.click(next());
    flush();
    expect(next().disabled).toBe(true);
    expect(previous().disabled).toBe(true);
    expect(screen.getByRole("cell", { name: "first" })).toBeTruthy();
    waiting.shift()!.reject(new Error("page refused"));
    await screen.findByRole("alert");
    expect(screen.getByRole("cell", { name: "first" })).toBeTruthy();
    expect(previous().disabled).toBe(true);
    fireEvent.click(next());
    waiting.shift()!.resolve({ rows: [{ id: "second" }], nextCursor: "third", total: null });
    await screen.findByRole("cell", { name: "second" });
    expect(screen.queryByRole("cell", { name: "first" })).toBeNull();
    fireEvent.click(next());
    waiting.shift()!.resolve({ rows: [{ id: "third" }], nextCursor: null, total: null });
    await screen.findByRole("cell", { name: "third" });
    expect(next().disabled).toBe(true);
    expect(within(screen.getByRole("navigation", { name: "Table pages" })).getByRole("status").textContent).toBe(
      "Page 3",
    );
    fireEvent.click(previous());
    waiting.shift()!.resolve({ rows: [{ id: "second" }], nextCursor: "third", total: null });
    await screen.findByRole("cell", { name: "second" });
    fireEvent.click(previous());
    waiting.shift()!.resolve({ rows: [{ id: "first" }], nextCursor: "second", total: null });
    await screen.findByRole("cell", { name: "first" });
    expect(requests).toEqual([
      { limit: 25 },
      { limit: 25, cursor: "second" },
      { limit: 25, cursor: "second" },
      { limit: 25, cursor: "third" },
      { limit: 25, cursor: "second" },
      { limit: 25 },
    ]);
    fireEvent.click(screen.getByRole("button", { name: "Refresh results" }));
    waiting.shift()!.resolve({ rows: [], nextCursor: null, total: 0 });
    await waitFor(() => expect(page.pagination().totalRows).toBe(0));
    expect(within(screen.getByRole("table")).queryAllByRole("cell")).toHaveLength(0);
    expect(previous().disabled).toBe(true);
    expect(next().disabled).toBe(true);
  });

  it("discards an old scope response after a new traversal starts", async () => {
    const waiting: ((page: ServerPage<string>) => void)[] = [];
    let page!: ReturnType<typeof createServerPage<string>>;
    function Harness() {
      page = createServerPage<string>(() => new Promise((resolve) => waiting.push(resolve)));
      return (
        <Records
          id="scope"
          title="Scope"
          rows={page.rows()}
          paging={page}
          refresh={page.refresh}
          rowKey={(row) => row}
          columns={[{ id: "id", header: "ID", accessorFn: (row) => row }]}
        />
      );
    }
    render(() => <Harness />);
    const old = page.refresh();
    const current = page.refresh();
    waiting[1]!({ rows: ["new scope"], nextCursor: null, total: 1 });
    await current;
    waiting[0]!({ rows: ["old scope"], nextCursor: "old cursor", total: null });
    await old;
    flush();
    expect(screen.getByRole("cell", { name: "new scope" })).toBeTruthy();
    expect(screen.queryByRole("cell", { name: "old scope" })).toBeNull();
    expect(page.pagination()).toMatchObject({ hasPrevious: false, hasNext: false, pending: false, totalRows: 1 });
  });
});
