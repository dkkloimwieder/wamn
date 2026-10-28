import { describe, expect, it } from "vitest";

import { occurredAt, writeSupplied } from "../src/supplied.js";

describe("supplied inputs", () => {
  it("spells occurredAt with microseconds", () => {
    expect(occurredAt(new Date("2026-01-02T03:04:05.678Z"))).toBe("2026-01-02T03:04:05.678000Z");
  });

  it("writes a fresh requestId per call", () => {
    const item = { requestId: "old" };
    const supplied = [{ input: ["requestId"], type: "requestId" }] as const;
    const first = writeSupplied(item, supplied);
    const second = writeSupplied(item, supplied);
    expect(first.requestId).toMatch(/^[0-9a-f-]{36}$/);
    expect(second.requestId).toMatch(/^[0-9a-f-]{36}$/);
    expect(first.requestId).not.toBe(second.requestId);
    expect(item.requestId).toBe("old");
  });
});
