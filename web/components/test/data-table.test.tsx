/**
 * The DataTable renders the load state it is given (wamn-xtz2.1).
 *
 * The document has no layout, so the test gives the scroll box and each row
 * the height a browser would measure, as `window.test.tsx` does.
 */

import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import { DataTable, WINDOW_FROM, type DataTableColumn } from "@wamn/ui";

const BOX = 480;
const ROW = 48;

const measured = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "offsetHeight");

beforeAll(() => {
  Object.defineProperty(HTMLElement.prototype, "offsetHeight", {
    configurable: true,
    get(this: HTMLElement) {
      if (this.dataset["slot"] === "scroll-area-viewport") {
        return BOX;
      }
      return this.dataset["index"] === undefined ? 0 : ROW;
    },
  });
});

afterAll(() => {
  if (measured !== undefined) {
    Object.defineProperty(HTMLElement.prototype, "offsetHeight", measured);
  }
});

afterEach(cleanup);

interface Row {
  readonly id: string;
  readonly code: string;
}

const COLUMNS: readonly DataTableColumn<Row>[] = [{ field: "code", label: "code", type: "text" }];

const rows = (count: number): Row[] =>
  Array.from({ length: count }, (_, index) => ({
    id: `r${index}`,
    code: `c${String(index).padStart(4, "0")}`,
  }));

function table(state: { rows: Row[]; fullyRead: boolean; busy: boolean }, onCapChange = () => {}) {
  render(() => (
    <DataTable
      columns={COLUMNS}
      rowId="id"
      rows={state.rows}
      fullyRead={state.fullyRead}
      busy={state.busy}
      cap={1000}
      onCapChange={onCapChange}
      startedAt={new Date(0)}
      endedAt={state.busy ? null : new Date(1000)}
    />
  ));
}

const MESSAGE = "Full dataset cannot be loaded";

describe("the data table", () => {
  it("renders every row at the windowing limit", () => {
    table({ rows: rows(WINDOW_FROM), fullyRead: true, busy: false });
    // One header row and one row for each record.
    expect(screen.getAllByRole("row")).toHaveLength(WINDOW_FROM + 1);
  });

  it("renders a window of its rows above the windowing limit", () => {
    table({ rows: rows(1000), fullyRead: true, busy: false });
    expect(screen.getByText("c0000")).toBeDefined();
    expect(screen.getAllByRole("row").length).toBeLessThan(50);
    expect(screen.queryByText("c0999")).toBeNull();
  });

  it("calls onCapChange with a committed cap, and ignores one that is not a positive integer", () => {
    const onCapChange = vi.fn();
    table({ rows: rows(3), fullyRead: true, busy: false }, onCapChange);
    const input = screen.getByLabelText("cap");
    fireEvent.change(input, { target: { value: "0" } });
    fireEvent.change(input, { target: { value: "2.5" } });
    fireEvent.change(input, { target: { value: "5000" } });
    expect(onCapChange.mock.calls).toEqual([[5000]]);
  });

  it("shows the message when the load ended and the set is not fully read", () => {
    table({ rows: rows(3), fullyRead: false, busy: false });
    expect(screen.getByText(MESSAGE)).toBeDefined();
    expect(screen.getByText(/^ended /)).toBeDefined();
  });

  it("shows no message when the set is fully read", () => {
    table({ rows: rows(3), fullyRead: true, busy: false });
    expect(screen.queryByText(MESSAGE)).toBeNull();
  });

  it("shows a loading state and no message while busy", () => {
    table({ rows: [], fullyRead: false, busy: true });
    expect(screen.getAllByText("Loading...").length).toBeGreaterThan(0);
    expect(screen.queryByText(MESSAGE)).toBeNull();
    expect(screen.getByText(/^started /)).toBeDefined();
    expect(screen.queryByText(/^ended /)).toBeNull();
  });
});
