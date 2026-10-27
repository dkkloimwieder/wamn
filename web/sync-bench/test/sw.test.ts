import { afterEach, expect, it, vi } from "vitest";

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  vi.resetModules();
});

it("excludes API and identity paths from installation and fetch interception", async () => {
  const excluded = ["/wamn", "/wamn/rows?cursor=1", "/password", "/password/renew"];
  const included = ["./", "live.html", "assets/live.js", "/wamn-extra", "/passwords"];
  const base = "https://bench.test/";
  vi.stubGlobal("registration", { scope: base });
  vi.stubGlobal("__PRECACHE_MANIFEST__", [...excluded, ...included]);
  const addAll = vi.fn(async (_urls: string[]) => {});
  vi.stubGlobal("caches", { open: async () => ({ addAll, match: async () => new Response("cached") }) });
  const listeners = new Map<string, (event: unknown) => void>();
  vi.stubGlobal("addEventListener", (name: string, listener: (event: unknown) => void) =>
    listeners.set(name, listener),
  );
  await import("../src/sw.js");
  const work: Promise<unknown>[] = [];
  listeners.get("install")!({ waitUntil: (promise: Promise<unknown>) => work.push(promise) });
  await Promise.all(work);
  expect(addAll).toHaveBeenCalledExactlyOnceWith(included.map((path) => new URL(path, base).href));
  for (const path of [...excluded, ...included]) {
    const respondWith = vi.fn();
    listeners.get("fetch")!({
      request: { url: new URL(path, base).href, method: "GET", mode: "navigate" },
      respondWith,
    });
    expect(respondWith).toHaveBeenCalledTimes(excluded.includes(path) ? 0 : 1);
    await respondWith.mock.calls[0]?.[0];
  }
});
