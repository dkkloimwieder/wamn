import { describe, expect, it } from "vitest";

import { byteLength, parseDraft } from "./draft-text";

/**
 * §2.4 prints `does not parse — line 41: …` beside the disabled structure tab
 * and tints that line in the source view. Both halves are engine output, so
 * both are pinned here: a line number is never guessed, and the label can never
 * contain the draft it is describing.
 */

describe("parseDraft", () => {
  it("reports the line when the engine gives a position", () => {
    expect(parseDraft('{\n  "a": 1\n  "b": 2\n}\n')).toEqual({
      ok: false,
      line: 3,
      message: "Expected ',' or '}' after property value",
    });
  });

  it("reports no line rather than guessing one when the engine gives no position", () => {
    // V8's other shape for this document carries no position at all. The old
    // fallback — the document's line count — is 4 for a 3-line document, and
    // would tint a line that does not exist.
    const parse = parseDraft('{\n  "a": nope\n}\n');
    expect(parse).toEqual({ ok: false, line: null, message: "Unexpected token 'o'" });
  });

  it("never puts the draft into the message", () => {
    const document = `{\n  "pad": "${"x".repeat(400)}",\n  "a": nope\n}\n`;
    const parse = parseDraft(document);
    if (parse.ok) {
      throw new Error("expected a parse failure");
    }
    expect(parse.message).not.toContain("xxx");
    expect(parse.message).not.toContain("is not valid JSON");
    expect(parse.message.length).toBeLessThan(80);
  });

  it("reports no line for a document that ends before any position is known", () => {
    expect(parseDraft("")).toEqual({
      ok: false,
      line: null,
      message: "Unexpected end of JSON input",
    });
  });

  it("accepts a document that parses", () => {
    expect(parseDraft('{ "a": 1 }')).toEqual({ ok: true });
  });
});

describe("byteLength", () => {
  it("counts UTF-8 bytes, not code units", () => {
    expect(byteLength('{ "name": "café" }')).toBe(19);
    expect('{ "name": "café" }'.length).toBe(18);
  });
});
