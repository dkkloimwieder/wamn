/**
 * Child tables of a QueryTable (wamn-8iul.6).
 *
 * A child renders in the detail of its parent row, scoped by the row's id
 * alone. It mounts, and so loads, on the first expand, keeps its rows across
 * a collapse, and ends when a load drops its row.
 */

import { cleanup, fireEvent, screen, waitFor } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { afterEach, describe, expect, it } from "vitest";

import type { TableColumn } from "@wamn/ui";
import type { Transport } from "@wamn/web-runtime";

import { type MemoryRequest, memoryDefinition, memoryTransport } from "../gallery/memory.js";
import { button, theButton } from "./dom.js";
import { queryTable, settled } from "./tables.js";

afterEach(cleanup);

interface Row {
  readonly id: string;
  readonly code: string;
}

interface Line {
  readonly id: string;
  readonly parentId: string;
  readonly text: string;
}

const COLUMNS: readonly TableColumn<Row>[] = [{ field: "code", label: "code", type: "text" }];

const ROWS: readonly Row[] = [
  { id: "r0", code: "a" },
  { id: "r1", code: "b" },
];

const LINES = memoryDefinition<Line>("line", [{ field: "text", label: "text", type: "text" }], {
  scopeFilters: ["parentId"],
});

/**
 * Renders the table, whose child reads the lines of a row, and runs `run`
 * with the row of each child load so far. Each load of a child is one request
 * of the line read.
 */
async function withChildren(run: (loads: () => (string | undefined)[]) => Promise<void>, rows?: () => readonly Row[]) {
  const mounts: MemoryRequest[] = [];
  const lines = memoryTransport<Line>(
    () => ROWS.map((row) => ({ id: `l${row.id}`, parentId: row.id, text: `lines of ${row.id}` })),
    { asked: mounts },
  );
  await queryTable({
    columns: COLUMNS,
    rows: rows ?? (() => ROWS),
    definition: (definition) => ({
      ...definition,
      childTables: [{ label: "lines", table: () => LINES, scopeFilter: "parentId" }],
    }),
    transport: (memory): Transport => ({
      invoke: (request) => (request.operation === LINES.read.route.operation ? lines : memory).invoke(request),
    }),
  });
  await run(() => mounts.map((request) => request.filter?.["parentId"]?.[0]));
}

describe("child tables", () => {
  it("mount on the first expand, scoped by the row's id", async () => {
    await withChildren(async (loads) => {
      expect(loads()).toEqual([]);
      fireEvent.click(theButton("expand r1"));
      await waitFor(() => expect(screen.getByText("lines of r1")).toBeDefined());
      expect(screen.getByText("lines")).toBeDefined();
      expect(loads()).toEqual(["r1"]);
    });
  });

  it("keep their rows across a collapse, with no new mount", async () => {
    await withChildren(async (loads) => {
      fireEvent.click(theButton("expand r0"));
      await waitFor(() => expect(screen.getByText("lines of r0")).toBeDefined());
      fireEvent.click(theButton("collapse r0"));
      expect(screen.queryByText("lines of r0")).toBeNull();
      fireEvent.click(theButton("expand r0"));
      expect(screen.getByText("lines of r0")).toBeDefined();
      expect(loads()).toEqual(["r0"]);
    });
  });

  it("end when a load drops their row", async () => {
    const [rows, setRows] = createSignal<readonly Row[]>(ROWS);
    // eslint-disable-next-line solid/reactivity -- the helper takes the accessor and reads it in its table.
    await withChildren(async (loads) => {
      fireEvent.click(theButton("expand r0"));
      await waitFor(() => expect(screen.getByText("lines of r0")).toBeDefined());
      setRows([ROWS[1]!]);
      fireEvent.click(theButton("refresh"));
      await settled();
      expect(button("collapse r0")).toBeNull();
      setRows(ROWS);
      fireEvent.click(theButton("refresh"));
      await settled();
      // The row stays expanded by its id, and its child mounts again.
      await waitFor(() => expect(screen.getByText("lines of r0")).toBeDefined());
      expect(loads()).toEqual(["r0", "r0"]);
    }, rows);
  });

  it("stay out of the column panel", async () => {
    await withChildren(async () => {
      fireEvent.click(theButton("columns"));
      const panel = Array.from(document.querySelectorAll("li[data-column]")).map((item) =>
        item.getAttribute("data-column"),
      );
      expect(panel).toEqual(["code"]);
    });
  });
});
