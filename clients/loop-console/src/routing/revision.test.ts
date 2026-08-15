import { describe, expect, it } from "vitest";

import { parseRevision } from "./revision";

describe("parseRevision", () => {
  it("accepts canonical non-negative decimal integers", () => {
    expect(parseRevision("0")).toBe(0);
    expect(parseRevision("17")).toBe(17);
    expect(parseRevision(String(Number.MAX_SAFE_INTEGER))).toBe(Number.MAX_SAFE_INTEGER);
  });

  // one case per spelling, so a regression names the rule it broke and the
  // other rules are still checked in the same run
  const rejected: Array<[string, string]> = [
    ["", "names no revision at all"],
    ["  ", "whitespace names no revision; Number('  ') is 0"],
    [" 17 ", "trimming would invent a revision the URL did not carry"],
    ["+1", "a sign is not part of a revision, and +1 is a second spelling of 1"],
    ["-1", "revisions count up from 0"],
    ["1.0", "a revision is an integer, and 1.0 is a second spelling of 1"],
    ["1e3", "exponent notation is a second spelling of 1000"],
    ["0x11", "hex is a second spelling of 17"],
    ["seventeen", "a word is not a number"],
    ["017", "else #/draft/orders/017 and #/draft/orders/17 are two URLs for one revision"],
    ["9007199254740992", "one past Number.MAX_SAFE_INTEGER: the digits stop being exact"],
  ];

  it.each(rejected)('rejects "%s" — %s', (segment) => {
    expect(parseRevision(segment)).toBeNull();
  });
});
