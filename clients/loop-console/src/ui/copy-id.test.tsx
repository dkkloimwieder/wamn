import { cleanup, fireEvent, render } from "@solidjs/testing-library";
import { flush } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  FAILING_RUN_ID,
  FINALIZED_REPORT_ID,
  failingRun,
  finalizedReport,
} from "../reader/fixtures";
import { CopyId } from "./copy-id";

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  Reflect.deleteProperty(navigator, "clipboard");
});

/** jsdom has no clipboard at all, so both outcomes have to be installed. */
function stubClipboard(writeText: (text: string) => Promise<void>): void {
  Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
}

/** Let the copy settle, then flush the write its resolution made. */
async function settle(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 0));
  flush();
}

describe("CopyId", () => {
  it("ellipsizes the middle at first and last 4, and leaves short ids whole", () => {
    const eight = FAILING_RUN_ID.slice(0, 8);
    const nine = FAILING_RUN_ID.slice(0, 9);
    const { getByRole, getByTitle } = render(() => (
      <>
        <CopyId id={FAILING_RUN_ID} label="run" />
        <CopyId id={FINALIZED_REPORT_ID} label="report" />
        <CopyId id={finalizedReport.testSetHash} label="test-set" />
        <CopyId id={eight} label="run" />
        <CopyId id={nine} label="run" />
      </>
    ));

    // §2.2's `run 01J9…3F2K` and §2.3's `report 01J9…8Q11`
    expect(getByTitle(FAILING_RUN_ID)).toHaveTextContent("01J9…3F2K");
    expect(getByTitle(FINALIZED_REPORT_ID)).toHaveTextContent("01J9…8Q11");
    expect(getByTitle(finalizedReport.testSetHash)).toHaveTextContent("sha2…39f2");
    expect(getByTitle(nine)).toHaveTextContent(`${nine.slice(0, 4)}…${nine.slice(-4)}`);

    // §2.6 reveals on hover only when truncated: an id already whole has nothing to reveal
    const whole = getByRole("button", { name: `copy run id ${eight}` });
    expect(whole).toHaveTextContent(eight);
    expect(whole).not.toHaveAttribute("title");
  });

  it("keeps the whole id reachable, in the name and on hover", () => {
    const { getByRole } = render(() => <CopyId id={failingRun.traceId} label="trace" />);

    const button = getByRole("button", { name: `copy trace id ${failingRun.traceId}` });
    expect(button).toHaveAttribute("title", failingRun.traceId);
    expect(button).toHaveTextContent("4bf9…d8f3");
  });

  it("copies the whole id, never the display text, and says it landed", async () => {
    const writeText = vi.fn(async () => {});
    stubClipboard(writeText);
    const { container, getByRole } = render(() => <CopyId id={FAILING_RUN_ID} label="run" />);

    const outcome = () => container.querySelector("[aria-live='polite']");
    expect(outcome()).toBeEmptyDOMElement();

    fireEvent.click(getByRole("button"));
    await settle();
    expect(writeText).toHaveBeenCalledWith(FAILING_RUN_ID);
    expect(outcome()).toHaveTextContent("copied");
  });

  it("settles the confirmation back to nothing on its own", async () => {
    vi.useFakeTimers();
    stubClipboard(vi.fn(async () => {}));
    const { container, getByRole } = render(() => <CopyId id={FAILING_RUN_ID} label="run" />);
    const outcome = () => container.querySelector("[aria-live='polite']");

    fireEvent.click(getByRole("button"));
    await vi.advanceTimersByTimeAsync(0);
    flush();
    expect(outcome()).toHaveTextContent("copied");

    await vi.advanceTimersByTimeAsync(1200);
    flush();
    expect(outcome()).toBeEmptyDOMElement();
  });

  it("says so, distinctly, when the clipboard refuses", async () => {
    stubClipboard(
      vi.fn(async () => {
        throw new Error("write permission denied");
      }),
    );
    const { container, getByRole } = render(() => <CopyId id={FAILING_RUN_ID} label="run" />);

    fireEvent.click(getByRole("button"));
    await settle();
    expect(container.querySelector("[aria-live='polite']")).toHaveTextContent("copy failed");
  });
});
