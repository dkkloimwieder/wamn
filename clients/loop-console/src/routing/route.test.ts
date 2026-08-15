import { describe, expect, it } from "vitest";

import { MAX_ID_LENGTH, parseHash, toHash, type Route } from "./route";

const awkwardIds = [
  "01J9X3F2K",
  "a/b",
  "a?b",
  "a b",
  "a%b",
  "ключ",
  "🙂",
  "a".repeat(MAX_ID_LENGTH),
];

describe("parseHash/toHash", () => {
  it("round-trips every route", () => {
    const routes: Route[] = [
      { kind: "start" },
      ...awkwardIds.flatMap((id): Route[] => [
        { kind: "run", id },
        { kind: "report", id },
        { kind: "draft", id, revision: id },
      ]),
      { kind: "draft", id: "orders", revision: "17" },
      { kind: "not-found", hash: "#/nope" },
    ];
    for (const route of routes) {
      expect(parseHash(toHash(route))).toEqual(route);
    }
  });

  it("reads the empty hash as start", () => {
    expect(parseHash("")).toEqual({ kind: "start" });
    expect(parseHash("#")).toEqual({ kind: "start" });
    expect(parseHash("#/")).toEqual({ kind: "start" });
    expect(toHash({ kind: "start" })).toBe("#/");
  });

  it("encodes ids that would otherwise break the hash", () => {
    expect(toHash({ kind: "run", id: "a/b#c d" })).toBe("#/run/a%2Fb%23c%20d");
    expect(parseHash("#/run/a%2Fb%23c%20d")).toEqual({ kind: "run", id: "a/b#c d" });
  });

  it("rejects empty, oversized, and malformed ids", () => {
    expect(parseHash("#/run/")).toEqual({ kind: "not-found", hash: "#/run/" });
    expect(parseHash("#/report/")).toEqual({ kind: "not-found", hash: "#/report/" });
    const tooLong = "a".repeat(MAX_ID_LENGTH + 1);
    expect(parseHash(`#/run/${tooLong}`).kind).toBe("not-found");
    expect(parseHash(`#/draft/ok/${tooLong}`).kind).toBe("not-found");
    expect(parseHash("#/run/%")).toEqual({ kind: "not-found", hash: "#/run/%" });
    expect(parseHash("#/draft/%E0%A4%A/1").kind).toBe("not-found");
  });

  it("rejects unknown and malformed paths", () => {
    for (const hash of [
      "#/draft/x",
      "#/draft/x/1/2",
      "#/run/a/b",
      "#/runs/a",
      "#/logs",
      "#run/a",
      "#/run",
    ]) {
      expect(parseHash(hash)).toEqual({ kind: "not-found", hash });
    }
  });

  it("treats the draft revision as an opaque id, not a number", () => {
    expect(parseHash("#/draft/orders/head%20rev")).toEqual({
      kind: "draft",
      id: "orders",
      revision: "head rev",
    });
  });
});
