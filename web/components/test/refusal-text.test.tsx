/**
 * A refusal whose code the package declares text for reads as that text
 * (wamn-ly11.2). The text travels from `wamn.json` through the errors
 * contract and the client IR into the generated contract, and the transport's
 * classifier puts it on the refused outcome.
 */

import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import { classify, type Outcome, type Transport, type WireRequest } from "@wamn/web-runtime";

import { WidgetArchiveForm } from "../fixture/components/widget.js";
import { WIDGET } from "../stubs/index.js";

afterEach(cleanup);

describe("a declared refusal with authored text", () => {
  it("shows the text in place of the code's words, at the field it names", async () => {
    // The release refuses the one item and names its id. The runtime's own
    // classifier reads the reply against the generated contract.
    // The widget list offers the widget with its revision, and the archive
    // sends that revision (wamn-x5q4).
    const sent: WireRequest[] = [];
    const transport: Transport = {
      invoke: (request: WireRequest) => {
        sent.push(request);
        return Promise.resolve(
          request.operation.includes("/list@")
            ? {
                status: "completed",
                value: { rows: [{ id: WIDGET, code: "standard", edit_version: "1", attributes: {} }] },
              }
            : classify(request.contract, null, {
                status: 200,
                body: JSON.stringify([{ error: { code: "already_archived", detail: { field: "id" } } }]),
              }),
        );
      },
    };
    const seen: Outcome<unknown>[] = [];
    render(() => (
      <WidgetArchiveForm
        transport={transport}
        initial={{ id: WIDGET }}
        onSubmitted={(outcome) => seen.push(outcome)}
      />
    ));

    await screen.findByDisplayValue("standard");
    fireEvent.click(screen.getByRole("button", { name: "submit" }));
    await waitFor(() => expect(seen).toHaveLength(1));
    const archive = sent.find((request) => request.operation.includes("/archive@"));
    expect(archive?.items).toEqual([expect.objectContaining({ id: WIDGET, expected_edit_version: "1" })]);
    expect(seen[0]).toMatchObject({
      status: "refused",
      code: "already_archived",
      text: "This widget is already archived.",
    });
    expect(await screen.findByText("This widget is already archived.")).toBeDefined();
    expect(screen.queryByText("Already archived.")).toBeNull();
  });
});
