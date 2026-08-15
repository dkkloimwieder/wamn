import { cleanup, fireEvent, render } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";

import { RefreshControl } from "./refresh-control";

afterEach(cleanup);

/** Built from local components, so the wall clock below holds in any zone. */
const refreshedAt = new Date(2026, 7, 14, 12, 4, 31);

describe("RefreshControl", () => {
  it("states the local wall clock and re-issues the read", () => {
    const onRefresh = vi.fn();
    const { getByRole } = render(() => (
      <RefreshControl refreshedAt={refreshedAt} onRefresh={onRefresh} />
    ));

    // §2.6's `refreshed 12:04:31 ↻`
    const button = getByRole("button", { name: /refresh this screen/ });
    expect(button).toHaveTextContent("refreshed 12:04:31");

    fireEvent.click(button);
    expect(onRefresh).toHaveBeenCalledTimes(1);
  });

  it("shows no timestamp when nothing has been read yet", () => {
    const { getByRole } = render(() => <RefreshControl refreshedAt={null} onRefresh={vi.fn()} />);

    const button = getByRole("button", { name: "refresh this screen" });
    expect(button).toHaveTextContent("↻");
    expect(button.textContent).not.toMatch(/\d/);
  });

  it("disables itself while busy, and announces it away from the button", () => {
    const { container, getByRole } = render(() => (
      <RefreshControl refreshedAt={refreshedAt} onRefresh={vi.fn()} busy />
    ));

    expect(getByRole("button", { name: /refresh this screen/ })).toBeDisabled();
    // the disabled button is out of the tab order, so the word cannot live on it
    expect(container.querySelector("[aria-live='polite']")).toHaveTextContent("refreshing");
  });
});
