import { cleanup, render } from "@solidjs/testing-library";
import { flush } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vitest";

import { nonParsingDraft, parsingDraft, unattributedDraft } from "../reader/fixtures";
import { SourceView } from "./source-view";

afterEach(() => {
  cleanup();
  Reflect.deleteProperty(navigator, "clipboard");
});

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

function gutter(container: Element): string[] {
  return [...container.querySelectorAll(".source-number")].map((cell) => cell.textContent ?? "");
}

const parse = nonParsingDraft.parse;
const errorLine = parse.ok ? null : parse.line;

describe("SourceView", () => {
  it("numbers every line of a draft, and does not invent one for its trailing newline", () => {
    // 46 pieces, 45 lines: the newline terminates the last line, it starts none
    const lines = parsingDraft.definition.split("\n").length - 1;
    expect(parsingDraft.definition.endsWith("\n")).toBe(true);

    const { container } = render(() => <SourceView text={parsingDraft.definition} />);

    expect(container.querySelectorAll(".source-line")).toHaveLength(lines);
    expect(gutter(container).at(-1)).toBe(String(lines));
    expect(container.querySelectorAll(".source-text")[lines - 1]).toHaveTextContent("}");
  });

  it("numbers a single line that ends in no newline", () => {
    const { container } = render(() => <SourceView text="{}" />);

    expect(gutter(container)).toEqual(["1"]);
    expect(container.querySelector(".source-text")).toHaveTextContent("{}");
  });

  it("tints the line the parse failed on, and names it in words too", () => {
    expect(errorLine).toBe(41);
    const { container } = render(() => (
      <SourceView text={nonParsingDraft.definition} errorLine={errorLine} />
    ));

    const tinted = container.querySelectorAll('.source-line[data-tone="fail"]');
    expect(tinted).toHaveLength(1);

    const row = tinted[0];
    expect(row.querySelector(".source-number")).toHaveTextContent("41");
    expect(row.querySelector(".visually-hidden")).toHaveTextContent("parse error on line 41");
    expect(row.querySelector(".source-text")).toHaveTextContent('"schema-version" "0.1",');
  });

  it("tints nothing without a line, and nothing for a line the source does not have", () => {
    const cases: ReadonlyArray<{ text: string; errorLine?: number | null }> = [
      // it parses, so §2.4 has nothing to point at
      { text: parsingDraft.definition },
      // the engine reported no position — never a guessed line
      { text: unattributedDraft.definition, errorLine: null },
      { text: nonParsingDraft.definition, errorLine: 4000 },
      { text: nonParsingDraft.definition, errorLine: 0 },
    ];

    for (const props of cases) {
      const { container } = render(() => <SourceView {...props} />);
      expect(container.querySelectorAll("[data-tone]")).toHaveLength(0);
      cleanup();
    }
  });

  it("copies the stored bytes verbatim, trailing newline included", async () => {
    const writes = stubClipboard();
    const { getByRole } = render(() => <SourceView text={parsingDraft.definition} />);

    // a real button, in tab order and free on Enter and Space — §3's floor
    const copy = getByRole("button", { name: "copy the exact stored source" });
    expect(copy).toHaveAttribute("type", "button");

    copy.click();
    await vi.waitFor(() => {
      flush();
      // announced, not merely painted: the outcome is a live region
      expect(getByRole("status")).toHaveTextContent("copied");
    });

    expect(writes).toEqual([parsingDraft.definition]);
  });

  it("says so when there is no clipboard to copy to", async () => {
    const { getByRole } = render(() => <SourceView text={parsingDraft.definition} />);

    getByRole("button", { name: "copy the exact stored source" }).click();
    await vi.waitFor(() => {
      flush();
      expect(getByRole("status")).toHaveTextContent("copy failed");
    });
  });
});
