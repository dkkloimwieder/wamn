import { createRoot, flush } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vitest";

import { FAILING_RUN_ID, FINALIZED_REPORT_ID } from "../reader/fixtures";
import { createRoute } from "./location";

afterEach(() => {
  vi.restoreAllMocks();
  window.location.hash = "";
});

/**
 * jsdom queues `hashchange` for a later task instead of firing it on
 * assignment, so deliver it here: assigning first means the listener reads the
 * new hash, as it would in a browser, and the test stays off the event loop.
 */
function navigate(hash: string): void {
  const oldURL = window.location.href;
  window.location.hash = hash;
  const newURL = window.location.href;
  window.dispatchEvent(new HashChangeEvent("hashchange", { oldURL, newURL }));
}

/** `createRoute` registers cleanup, so it needs an owner and a teardown. */
function mount() {
  return createRoot((dispose) => ({ route: createRoute(), dispose }));
}

describe("createRoute", () => {
  it("reads the hash the page loaded with", () => {
    window.location.hash = `#/report/${FINALIZED_REPORT_ID}`;
    const { route, dispose } = mount();

    expect(route()).toEqual({ kind: "report", id: FINALIZED_REPORT_ID });
    dispose();
  });

  it("follows every hashchange to the new route", () => {
    const { route, dispose } = mount();
    expect(route()).toEqual({ kind: "start" });

    navigate("#/draft/orders/17");
    flush();
    expect(route()).toEqual({ kind: "draft", id: "orders", revision: "17" });

    navigate("#/logs");
    flush();
    expect(route()).toEqual({ kind: "not-found", hash: "#/logs" });
    dispose();
  });

  it("removes the listener on cleanup", () => {
    const addListener = vi.spyOn(window, "addEventListener");
    const removeListener = vi.spyOn(window, "removeEventListener");
    const { route, dispose } = mount();
    const listener = addListener.mock.calls.find(([type]) => type === "hashchange")?.[1];

    navigate(`#/run/${FAILING_RUN_ID}`);
    flush();
    expect(route()).toEqual({ kind: "run", id: FAILING_RUN_ID });

    dispose();
    expect(listener).toBeTypeOf("function");
    expect(removeListener).toHaveBeenCalledWith("hashchange", listener);

    // a disposed memo still recomputes when read, so a listener that outlived
    // the root would move the route here
    navigate(`#/report/${FINALIZED_REPORT_ID}`);
    flush();
    expect(route()).toEqual({ kind: "run", id: FAILING_RUN_ID });
  });
});
