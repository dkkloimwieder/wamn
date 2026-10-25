import { describe, expect, it, vi } from "vitest";

import { createCall, hasNumber, pendingNumber, requestHash, shownNumber, uuidv7 } from "../src/sync/index.js";

describe("uuidv7", () => {
  it("encodes milliseconds, version 7, the RFC variant and random bits", () => {
    const now = 1_792_886_400_123;
    const id = uuidv7(() => now);
    expect(id).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
    expect(Number.parseInt(id.replaceAll("-", "").slice(0, 12), 16)).toBe(now);
    expect(uuidv7(() => now)).not.toBe(id);
    expect(id < uuidv7(() => now + 1)).toBe(true);
  });
});

describe("createCall", () => {
  it("uses one id for the request and expected row, and hashes no send metadata", async () => {
    const random = vi.spyOn(crypto, "getRandomValues").mockImplementation((bytes) => bytes);
    const clock = vi.spyOn(Date, "now").mockReturnValue(1_792_886_400_123);
    try {
      const options = { operation: "fixture:row/create@1.0.0", model: "row", append_only: true };
      const first = await createCall({
        ...options,
        request: { label: "Box", idempotencyKey: "old", requestId: "first" },
      });
      const second = await createCall({
        ...options,
        request: { requestId: "second", label: "Box", idempotencyKey: "new" },
      });
      expect(first.call).toEqual({
        operation: options.operation,
        request: { id: first.id, label: "Box" },
        request_hash: await requestHash({ id: first.id, label: "Box" }),
        expect: { model: "row", id: first.id },
        append_only: true,
      });
      expect(first.call.request_hash).toBe(second.call.request_hash);
      expect(first.call.request_hash).not.toBe(await requestHash({ label: "Box" }));
    } finally {
      clock.mockRestore();
      random.mockRestore();
    }
  });
});

describe("numbers", () => {
  const id = "019fd531-287b-7000-8000-00000000AbCd";

  it("spells a pending number that cannot parse as a number", () => {
    expect(pendingNumber(id)).toBe("pending·abcd");
    expect(Number(pendingNumber(id))).toBeNaN();
    expect(Number.parseFloat(pendingNumber(id))).toBeNaN();
  });

  it.each([undefined, null, 0, 42, "PO-0042"])("shows the number when present: %s", (number) => {
    const row = { id, number };
    expect(hasNumber(row, "number")).toBe(number !== undefined && number !== null);
    expect(shownNumber(row, "number")).toBe(number ?? "pending·abcd");
  });
});
