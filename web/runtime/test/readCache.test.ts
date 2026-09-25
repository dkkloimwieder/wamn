/**
 * The read store of the transport, against a fake fetch.
 *
 * Each case states the requests that reach the fetch, because the store is
 * there to send fewer of them. `docs/plan/http-reads.md` section 4.5 states
 * the rules.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { createTransport } from "../src/transport.js";
import type { ResponseContract, Transport, WireRequest } from "../src/wire.js";

const GET = "private, no-cache";
const LIST = "private, max-age=10, stale-while-revalidate=60";

/** One reply that the fake fetch returns. */
interface Stated {
  readonly status: number;
  readonly body?: string;
  readonly cacheControl?: string;
  readonly etag?: string;
}

/** One request that the fake fetch saw. */
interface Seen {
  readonly url: string;
  readonly method: string;
  readonly ifNoneMatch: string | undefined;
  readonly cache: RequestCache | undefined;
}

/**
 * A transport over a fetch that answers from the queue, in order. A reply
 * waits for `release` when `held` is true, so a test can overlap two reads.
 */
function harness(options: { held?: boolean; cookies?: () => string } = {}) {
  const replies: Stated[] = [];
  const seen: Seen[] = [];
  const waiting: Array<() => void> = [];
  const fetch: typeof globalThis.fetch = async (url, init) => {
    const headers = (init?.headers ?? {}) as { [name: string]: string };
    seen.push({
      url: String(url),
      method: init?.method ?? "GET",
      ifNoneMatch: headers["if-none-match"],
      cache: init?.cache,
    });
    const stated = replies.shift();
    if (stated === undefined) {
      throw new Error("no reply is queued");
    }
    if (options.held === true) {
      await new Promise<void>((resolve) => waiting.push(resolve));
    }
    const headersOut = new Headers();
    if (stated.cacheControl !== undefined) {
      headersOut.set("cache-control", stated.cacheControl);
    }
    if (stated.etag !== undefined) {
      headersOut.set("etag", stated.etag);
    }
    return new Response(stated.status === 304 ? null : (stated.body ?? ""), {
      status: stated.status,
      headers: headersOut,
    });
  };
  const transport: Transport =
    options.cookies === undefined
      ? createTransport({ baseUrl: "https://example.test", fetch })
      : createTransport({ baseUrl: "", cookie: true, cookies: options.cookies, fetch });
  // Releases every held reply, or only the one at `index`, in request order.
  const release = (index?: number) => {
    if (index === undefined) {
      for (const resolve of waiting.splice(0)) {
        resolve();
      }
    } else {
      waiting[index]?.();
    }
  };
  return { transport, replies, seen, release };
}

function contract(kind: string, resultClass: string): ResponseContract {
  return {
    resultClass,
    partialSchema: null,
    errors: [],
    replay: null,
    direct: true,
    kind,
    transaction: "implicit",
  };
}

function get(id: string): WireRequest {
  return {
    operation: "fixture:widget/get@1.0.0",
    method: "GET",
    template: "/widget/get",
    freshOnly: false,
    contract: contract("get", "one"),
    items: [{ id }],
  };
}

function list(status: string): WireRequest {
  return {
    operation: "fixture:widget/query@1.0.0",
    method: "GET",
    template: "/widget/query",
    freshOnly: false,
    contract: contract("query", "bounded_list"),
    items: [{ status }],
  };
}

const WRITE: WireRequest = {
  operation: "fixture:widget/update@1.0.0",
  method: "POST",
  template: "/widget/update",
  freshOnly: false,
  contract: contract("update", "one"),
  items: [{ request_id: "r1", id: "a" }],
};

const ROWS = '[{"value":{"rows":[{"id":"a"}]}}]';
const WIDGET = '[{"value":{"id":"a","row_version":1}}]';

describe("the read store", () => {
  beforeEach(() => {
    vi.useFakeTimers({ toFake: ["Date"] });
    vi.setSystemTime(0);
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("sends one request for two equal reads in flight", async () => {
    const { transport, replies, seen, release } = harness({ held: true });
    replies.push({ status: 200, body: WIDGET, cacheControl: GET, etag: '"t1"' });
    const first = transport.invoke(get("a"));
    const second = transport.invoke(get("a"));
    release();
    const outcomes = await Promise.all([first, second]);
    expect(seen).toHaveLength(1);
    expect(outcomes[0]).toEqual(outcomes[1]);
    expect(outcomes[0]).toEqual({ status: "completed", value: { id: "a", row_version: 1 } });
    expect(seen[0]?.cache).toBe("no-store");
  });

  it("answers a fresh list without a request, and revalidates it when stale", async () => {
    const { transport, replies, seen } = harness();
    replies.push({ status: 200, body: ROWS, cacheControl: LIST, etag: 'W/"v1"' });
    await transport.invoke(list("open"));
    vi.setSystemTime(9_000);
    expect(await transport.invoke(list("open"))).toEqual({
      status: "completed",
      value: { rows: [{ id: "a" }] },
    });
    expect(seen).toHaveLength(1);
    vi.setSystemTime(10_000);
    replies.push({ status: 304, cacheControl: LIST, etag: 'W/"v1"' });
    await transport.invoke(list("open"));
    expect(seen).toHaveLength(2);
    expect(seen[1]?.ifNoneMatch).toBe('W/"v1"');
  });

  it("revalidates a get every time, and a 304 reuses the stored body", async () => {
    const { transport, replies, seen } = harness();
    replies.push({ status: 200, body: WIDGET, cacheControl: GET, etag: '"t1"' });
    replies.push({ status: 304, cacheControl: GET, etag: '"t1"' });
    const first = await transport.invoke(get("a"));
    const second = await transport.invoke(get("a"));
    expect(seen.map((call) => call.ifNoneMatch)).toEqual([undefined, '"t1"']);
    expect(second).toEqual(first);
  });

  it("revalidates every stored read after a write, and a 200 replaces the entry", async () => {
    const { transport, replies, seen } = harness();
    replies.push({ status: 200, body: ROWS, cacheControl: LIST, etag: 'W/"v1"' });
    await transport.invoke(list("open"));
    replies.push({ status: 200, body: '[{"request_id":"r1","value":{"id":"a"}}]' });
    await transport.invoke(WRITE);
    const changed = '[{"value":{"rows":[{"id":"a"},{"id":"b"}]}}]';
    replies.push({ status: 200, body: changed, cacheControl: LIST, etag: 'W/"v2"' });
    expect(await transport.invoke(list("open"))).toEqual({
      status: "completed",
      value: { rows: [{ id: "a" }, { id: "b" }] },
    });
    expect(seen[2]?.ifNoneMatch).toBe('W/"v1"');
    vi.setSystemTime(10_000);
    replies.push({ status: 304, cacheControl: LIST, etag: 'W/"v2"' });
    await transport.invoke(list("open"));
    expect(seen[3]?.ifNoneMatch).toBe('W/"v2"');
  });

  it("stores a read that started before a write as stale", async () => {
    const { transport, replies, seen, release } = harness({ held: true });
    replies.push({ status: 200, body: ROWS, cacheControl: LIST, etag: 'W/"v1"' });
    replies.push({ status: 200, body: '[{"request_id":"r1","value":{"id":"a"}}]' });
    const before = transport.invoke(list("open"));
    const write = transport.invoke(WRITE);
    release(1);
    await write;
    release(0);
    await before;
    replies.push({ status: 304, cacheControl: LIST, etag: 'W/"v1"' });
    const after = transport.invoke(list("open"));
    release(2);
    await after;
    expect(seen).toHaveLength(3);
    expect(seen[2]?.ifNoneMatch).toBe('W/"v1"');
  });

  it("stores no failure, and the next read sends no tag", async () => {
    const { transport, replies, seen } = harness();
    replies.push({ status: 200, body: WIDGET, cacheControl: GET, etag: '"t1"' });
    await transport.invoke(get("a"));
    replies.push({ status: 503, body: "unavailable", cacheControl: "no-store" });
    expect((await transport.invoke(get("a"))).status).toBe("uncertain");
    replies.push({ status: 200, body: WIDGET, cacheControl: GET, etag: '"t1"' });
    await transport.invoke(get("a"));
    expect(seen.map((call) => call.ifNoneMatch)).toEqual([undefined, '"t1"', undefined]);
  });

  it("keeps different queries apart", async () => {
    const { transport, replies, seen } = harness();
    replies.push({ status: 200, body: ROWS, cacheControl: LIST, etag: 'W/"v1"' });
    replies.push({ status: 200, body: '[{"value":{"rows":[]}}]', cacheControl: LIST, etag: 'W/"v2"' });
    await transport.invoke(list("open"));
    expect(await transport.invoke(list("closed"))).toEqual({
      status: "completed",
      value: { rows: [] },
    });
    expect(seen.map((call) => call.url)).toEqual([
      "https://example.test/widget/query?status=%22open%22",
      "https://example.test/widget/query?status=%22closed%22",
    ]);
  });

  it("empties itself when the session cookie changes", async () => {
    let cookie = "__Host-wamn-csrf=one";
    const { transport, replies, seen } = harness({ cookies: () => cookie });
    replies.push({ status: 200, body: ROWS, cacheControl: LIST, etag: 'W/"v1"' });
    await transport.invoke(list("open"));
    cookie = "__Host-wamn-csrf=two";
    replies.push({ status: 200, body: ROWS, cacheControl: LIST, etag: 'W/"v1"' });
    await transport.invoke(list("open"));
    expect(seen.map((call) => call.ifNoneMatch)).toEqual([undefined, undefined]);
  });
});
