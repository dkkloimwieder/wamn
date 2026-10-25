import { afterEach, describe, expect, it, vi } from "vitest";

import { createTransport } from "../src/transport.js";

afterEach(() => vi.useRealTimers());

describe("probe", () => {
  it.each([204, 500])("reports whether status %s is successful without sending credentials", async (status) => {
    const fetch = vi.fn<typeof globalThis.fetch>(async () => new Response(null, { status }));
    const transport = createTransport({ baseUrl: "https://app.test", credential: "secret", fetch });
    expect(await transport.probe()).toBe(status === 204);
    expect(fetch).toHaveBeenCalledWith("https://app.test/api/health", {
      method: "HEAD",
      cache: "no-store",
      credentials: "omit",
      signal: expect.any(AbortSignal),
    });
  });

  it("omits cookies and never reads the CSRF cookie", async () => {
    const fetch = vi.fn<typeof globalThis.fetch>(async () => new Response(null, { status: 204 }));
    const cookies = vi.fn(() => "__Host-wamn-csrf=secret");
    const transport = createTransport({ baseUrl: "", cookie: true, cookies, fetch });
    expect(await transport.probe()).toBe(true);
    expect(fetch.mock.calls[0]?.[1]?.credentials).toBe("omit");
    expect(cookies).not.toHaveBeenCalled();
  });

  it("returns false on a network error", async () => {
    const transport = createTransport({
      baseUrl: "",
      fetch: async () => {
        throw new TypeError("Failed to fetch");
      },
    });
    expect(await transport.probe()).toBe(false);
  });

  it("aborts a fetch without a reply after three seconds", async () => {
    vi.useFakeTimers();
    let signal: AbortSignal | null | undefined;
    const fetch: typeof globalThis.fetch = (_url, init) =>
      new Promise((_resolve, reject) => {
        signal = init?.signal;
        signal?.addEventListener("abort", () => reject(new DOMException("Aborted", "AbortError")));
      });
    const pending = createTransport({ baseUrl: "", fetch }).probe();
    await vi.advanceTimersByTimeAsync(2_999);
    expect(signal?.aborted).toBe(false);
    await vi.advanceTimersByTimeAsync(1);
    expect(await pending).toBe(false);
    expect(signal?.aborted).toBe(true);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("forwards cancellation and clears the deadline", async () => {
    vi.useFakeTimers();
    const controller = new AbortController();
    const fetch: typeof globalThis.fetch = (_url, init) =>
      new Promise((_resolve, reject) => {
        init?.signal?.addEventListener("abort", () => reject(new DOMException("Aborted", "AbortError")));
      });
    const transport = createTransport({ baseUrl: "", fetch });
    const pending = transport.probe(controller.signal);
    controller.abort();
    expect(await pending).toBe(false);
    expect(vi.getTimerCount()).toBe(0);
    expect(await transport.probe(controller.signal)).toBe(false);
  });
});
