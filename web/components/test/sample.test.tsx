/**
 * The harness proof: a component renders, a stub transport answers, and the
 * rows reach the document.
 *
 * `wamn-rs5b.7` adds the example test for a generated component. This one
 * exists so that a broken harness fails here, where the cause is obvious.
 */

import { cleanup, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import type { JsonValue, Outcome, Transport, WireRequest } from "@wamn/web-runtime";

import { Sample } from "../src/sample.js";

function stub(outcome: Outcome<JsonValue>): Transport {
  return {
    invoke: (_request: WireRequest) => Promise.resolve(outcome),
  };
}

// The library registers its own cleanup only when the test globals exist, and
// this project imports what it uses instead.
afterEach(cleanup);

describe("the harness", () => {
  it("renders one row for each record the transport returns", async () => {
    const transport = stub({
      status: "completed",
      value: { rows: [{ id: "a", edit_version: "1", attributes: {} }] },
    });
    render(() => <Sample transport={transport} selector={{}} />);
    await waitFor(() => expect(screen.getByRole("listitem")).toBeDefined());
    expect(screen.getByRole("listitem").textContent).toBe("a");
  });

  it("shows the outcome when the reply is not a completion", async () => {
    const transport = stub({ status: "uncertain", reason: "no reply", retryRefusal: null });
    render(() => <Sample transport={transport} selector={{}} />);
    await waitFor(() => expect(screen.getByText("uncertain")).toBeDefined());
    expect(screen.queryByRole("listitem")).toBeNull();
  });
});
