/**
 * The testing method for a long table, used once.
 *
 * A table that holds 1000 rows renders only the rows in view, and scrolling
 * brings later rows in. Paging does not change: the second page appends to
 * the first (wamn-ly11.1).
 *
 * The document has no layout, so the test gives the scroll box and each row
 * the height a browser would measure. The subject is the fixture's page table, which
 * `check_client_components` writes into `fixture/` before this runs.
 */

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterAll, afterEach, beforeAll, describe, expect, it } from "vitest";

import { WidgetQueryTable } from "../fixture/components/widget.js";
import { page, tableStub as stub } from "../stubs/index.js";

/** The height the scroll box measures, which holds about ten rows. */
const BOX = 480;

/** The height each rendered row measures. */
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

const ids = (from: number, count: number) =>
  Array.from({ length: count }, (_, index) => `w${String(from + index).padStart(4, "0")}`);

describe("a table that holds 1000 rows", () => {
  it("renders a window of its rows, and scrolling brings later rows in", async () => {
    const { transport, sent } = stub([page(ids(0, 500), "c2"), page(ids(500, 500), null)]);
    render(() => <WidgetQueryTable transport={transport} />);
    fireEvent.click(screen.getByText("read"));
    await waitFor(() => expect(screen.getByText("w0000")).toBeDefined());
    fireEvent.click(screen.getByRole("button", { name: "next page" }));
    await waitFor(() => expect(sent).toHaveLength(2));

    // The second page appended, so the first row is still the first.
    await waitFor(() => expect(screen.getByText("w0000")).toBeDefined());
    const rendered = () => screen.getAllByRole("row").length;
    expect(rendered()).toBeLessThan(50);
    expect(screen.queryByText("w0999")).toBeNull();

    const box = document.querySelector<HTMLElement>('[data-slot="scroll-area-viewport"]')!;
    box.scrollTop = 1000 * ROW;
    fireEvent.scroll(box);

    await waitFor(() => expect(screen.getByText("w0999")).toBeDefined());
    expect(screen.queryByText("w0000")).toBeNull();
    expect(rendered()).toBeLessThan(50);
  });

  it("renders every row of a short table", async () => {
    const { transport } = stub([page(ids(0, 60), null)]);
    render(() => <WidgetQueryTable transport={transport} />);
    fireEvent.click(screen.getByText("read"));
    await waitFor(() => expect(screen.getByText("w0059")).toBeDefined());
    // One header row and one row for each record.
    expect(screen.getAllByRole("row")).toHaveLength(61);
  });
});
