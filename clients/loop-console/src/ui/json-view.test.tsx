import { cleanup, render } from "@solidjs/testing-library";
import { flush } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  effectUncertainRun,
  failingRun,
  finalizedReport,
  passingRun,
  unattributedDraft,
} from "../reader/fixtures";
import type { CapturedValue, JsonValue } from "../reader/types";
import { JsonView } from "./json-view";

afterEach(() => {
  cleanup();
  Reflect.deleteProperty(navigator, "clipboard");
});

/** The fixtures' capture slots are a five-state union; these tests want the value. */
function captured(value: CapturedValue): JsonValue {
  return value.state === "present" ? value.value : null;
}

/** §2.3's one JsonView: the terminal-respond body of the failing case. */
function terminalRespondBody(): JsonValue {
  const cases = finalizedReport.state === "finalized" ? finalizedReport.cases : [];
  for (const reportCase of cases) {
    for (const failed of reportCase.failedAssertions) {
      if (failed.expected.family === "terminal-respond") {
        return failed.expected.body;
      }
    }
  }
  return null;
}

/** Records what the copy affordance actually handed the clipboard. */
function stubClipboard(): string[] {
  const writes: string[] = [];
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    value: {
      writeText: (text: string) => {
        writes.push(text);
        return Promise.resolve();
      },
    },
  });
  return writes;
}

/** §2.2's `▸ raw`, the run failure panel's disclosure. */
const failureRaw: JsonValue = failingRun.failure?.raw ?? null;

describe("JsonView", () => {
  it("renders every scalar and empty container as JSON, with nothing to open", () => {
    // shapes, not domain data: no fixture carries a bare scalar or an empty array
    const shapes: ReadonlyArray<{ value: JsonValue; text: string }> = [
      { value: null, text: "null" },
      { value: true, text: "true" },
      { value: 503, text: "503" },
      { value: "upstream 503", text: '"upstream 503"' },
      { value: [], text: "[]" },
      { value: {}, text: "{}" },
    ];

    for (const { value, text } of shapes) {
      const { container, queryByRole } = render(() => <JsonView value={value} subject="output" />);
      expect(container.querySelector(".json-tree")).toHaveTextContent(text);
      expect(queryByRole("button", { name: /^expand/ })).toBeNull();
      cleanup();
    }
  });

  it("collapses the failing run's raw failure past the depth it is given, and opens it", () => {
    const { container, getByRole } = render(() => (
      <JsonView value={failureRaw} subject="raw failure" collapseDepth={1} />
    ));
    const tree = container.querySelector(".json-tree");

    expect(tree).toHaveTextContent('"kind": "retry-exhausted"');
    expect(tree).toHaveTextContent('"attempts": 4');
    // deeper than the collapse depth, so it starts closed and says what it holds
    expect(tree).toHaveTextContent('"last": { 2 keys }');
    expect(tree).not.toHaveTextContent("inventory.internal");

    // a real button, in tab order and free on Enter and Space — §3's floor
    const expander = getByRole("button", { name: "expand raw failure.last" });
    expect(expander).toHaveAttribute("type", "button");
    expect(expander).toHaveAttribute("aria-expanded", "false");

    expander.click();
    flush();

    expect(getByRole("button", { name: "collapse raw failure.last" })).toHaveAttribute(
      "aria-expanded",
      "true",
    );
    expect(tree).toHaveTextContent('"upstream": "inventory.internal"');
  });

  it("renders the passing run's captured output, booleans and nesting included", () => {
    const { container } = render(() => (
      <JsonView value={captured(passingRun.facts[3].output)} subject="respond output" />
    ));
    const tree = container.querySelector(".json-tree");

    expect(tree).toHaveTextContent('"status": 201');
    expect(tree).toHaveTextContent('"body": {');
    expect(tree).toHaveTextContent('"reserved": true');
  });

  it("renders the report's terminal-respond body", () => {
    const { container } = render(() => (
      <JsonView value={terminalRespondBody()} subject="expected body" />
    ));
    const tree = container.querySelector(".json-tree");

    expect(tree).toHaveTextContent('"created": true');
    expect(tree).toHaveTextContent('"backordered": false');
  });

  it("renders an array of objects, whose objects start collapsed at the default depth", () => {
    const definition = JSON.parse(unattributedDraft.definition) as JsonValue;
    const { container, getByRole } = render(() => (
      <JsonView value={definition} subject="definition" />
    ));
    const tree = container.querySelector(".json-tree");

    expect(tree).toHaveTextContent('"nodes": [');
    expect(getByRole("button", { name: "expand definition.nodes[0]" })).toHaveAttribute(
      "aria-expanded",
      "false",
    );
    expect(tree).not.toHaveTextContent("ingress");

    getByRole("button", { name: "expand definition.nodes[0]" }).click();
    flush();

    expect(tree).toHaveTextContent('"id": "ingress"');
  });

  it("renders an array of scalars, one row per item", () => {
    const meaning: JsonValue = effectUncertainRun.uncertainty?.meaning ?? [];
    const { container } = render(() => <JsonView value={meaning} subject="meaning" />);

    expect(container.querySelectorAll(".json-children > .json-row")).toHaveLength(2);
    expect(container.querySelector(".json-tree")).toHaveTextContent(
      "may have escaped without a durable outcome",
    );
  });

  it("names every control after its subject, so two views on one screen do not collide", () => {
    // §2.2 is that screen: the failure raw, and a captured output per disclosed fact
    const { getByRole } = render(() => (
      <>
        <JsonView value={failureRaw} subject="raw failure" collapseDepth={1} />
        <JsonView value={captured(passingRun.facts[3].output)} subject="respond output" />
      </>
    ));

    // getByRole is unique-or-throw, so this fails on any name shared by both views
    expect(getByRole("button", { name: "copy raw failure" })).toBeInTheDocument();
    expect(getByRole("button", { name: "copy respond output" })).toBeInTheDocument();
    expect(getByRole("button", { name: "expand raw failure.last" })).toBeInTheDocument();
    expect(getByRole("button", { name: "collapse respond output.body" })).toBeInTheDocument();
  });

  it("copies the value as pretty JSON, and says that it landed", async () => {
    const writes = stubClipboard();
    const { getByRole } = render(() => <JsonView value={failureRaw} subject="raw failure" />);

    const copy = getByRole("button", { name: "copy raw failure" });
    expect(copy).toHaveAttribute("type", "button");

    copy.click();
    await vi.waitFor(() => {
      flush();
      // announced, not merely painted: the outcome is a live region
      expect(getByRole("status")).toHaveTextContent("copied");
    });

    expect(writes).toEqual([JSON.stringify(failureRaw, null, 2)]);
  });
});
