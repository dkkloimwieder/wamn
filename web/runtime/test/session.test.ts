/**
 * The cookie session: the carrier each transport mode sends, the login calls,
 * and the keeper that renews the session over a fake clock.
 */

import { describe, expect, it } from "vitest";

import {
  environments,
  keepSession,
  renew,
  signIn,
  signOut,
  type Clock,
  type SessionState,
} from "../src/session.js";
import {
  createTransport,
  type BearerOptions,
  type CookieOptions,
  type TransportOptions,
} from "../src/transport.js";
import type { ResponseContract } from "../src/wire.js";

/** One call that a stub fetch saw. */
interface Seen {
  readonly url: string;
  readonly init: RequestInit;
}

/** A fetch that records each call and answers from the queue, in order. */
function stubFetch(replies: Array<() => Response>): {
  fetch: typeof globalThis.fetch;
  seen: Seen[];
} {
  const seen: Seen[] = [];
  const fetch: typeof globalThis.fetch = (url, init) => {
    seen.push({ url: String(url), init: init ?? {} });
    const reply = replies.shift();
    return reply === undefined
      ? Promise.reject(new Error("no reply is queued"))
      : Promise.resolve(reply());
  };
  return { fetch, seen };
}

function headersOf(call: Seen | undefined): { [name: string]: string } {
  return (call?.init.headers ?? {}) as { [name: string]: string };
}

function bodyOf(call: Seen | undefined): unknown {
  return JSON.parse(String(call?.init.body));
}

describe("the transport carrier", () => {
  const contract: ResponseContract = {
    resultClass: "one",
    partialSchema: null,
    errors: [],
    replay: null,
    direct: true,
    kind: "get",
    transaction: "implicit",
  };
  const request = {
    operation: "platform-fixture:widget/get@1.0.0",
    method: "POST",
    template: "/widget/get",
    freshOnly: false,
    contract,
    items: [{ request_id: "r1" }],
  };
  const completed = (): Response =>
    new Response('[{"request_id":"r1","value":{"id":"a"}}]', { status: 200 });

  async function send(
    options: Omit<BearerOptions, "baseUrl"> | Omit<CookieOptions, "baseUrl">,
  ): Promise<Seen | undefined> {
    const stub = stubFetch([completed]);
    const transport = createTransport({
      ...options,
      baseUrl: "https://example.test",
      fetch: stub.fetch,
    } as TransportOptions);
    expect(await transport.invoke(request)).toEqual({ status: "completed", value: { id: "a" } });
    return stub.seen[0];
  }

  it("sends the cookies and the CSRF token, and no authorization, in cookie mode", async () => {
    const call = await send({
      cookie: true,
      cookies: () => "theme=dark; __Host-wamn-csrf=c5rf; other=x",
    });
    expect(call?.init.credentials).toBe("include");
    expect(headersOf(call)).toEqual({ "content-type": "application/json", "x-wamn-csrf": "c5rf" });
  });

  it("reads the CSRF cookie again on each request", async () => {
    let jar = "__Host-wamn-csrf=first";
    const stub = stubFetch([completed, completed]);
    const transport = createTransport({
      baseUrl: "",
      cookie: true,
      cookies: () => jar,
      fetch: stub.fetch,
    });
    await transport.invoke(request);
    jar = "__Host-wamn-csrf=second";
    await transport.invoke(request);
    expect(stub.seen.map((call) => headersOf(call)["x-wamn-csrf"])).toEqual(["first", "second"]);
  });

  it("sends no CSRF header when the cookie is absent, and the router decides", async () => {
    const call = await send({ cookie: true, cookies: () => "theme=dark" });
    expect(call?.init.credentials).toBe("include");
    expect(headersOf(call)).toEqual({ "content-type": "application/json" });
  });

  it("sends the bearer token and no cookies in bearer mode", async () => {
    const call = await send({ credential: "token" });
    expect(call?.init.credentials).toBeUndefined();
    expect(headersOf(call)).toEqual({
      "content-type": "application/json",
      authorization: "Bearer token",
    });
  });

  it("sends no credential when a bearer caller holds none", async () => {
    const call = await send({});
    expect(call?.init.credentials).toBeUndefined();
    expect(headersOf(call)).toEqual({ "content-type": "application/json" });
  });

  it("sends a read as a GET with its one item in the query and no CSRF header", async () => {
    const stub = stubFetch([
      () => new Response('[{"value":{"id":"a"}}]', { status: 200 }),
      () => new Response('[{"request_id":"r1","value":{"id":"a"}}]', { status: 200 }),
    ]);
    const transport = createTransport({
      baseUrl: "https://example.test",
      cookie: true,
      cookies: () => "__Host-wamn-csrf=c5rf",
      fetch: stub.fetch,
    });
    const read = { ...request, method: "GET", items: [{ id: "a" }] };
    expect(await transport.invoke(read)).toEqual({ status: "completed", value: { id: "a" } });
    const call = stub.seen[0];
    expect(call?.url).toBe("https://example.test/widget/get?id=%22a%22");
    expect(call?.init.method).toBe("GET");
    expect(call?.init.body).toBeUndefined();
    expect(call?.init.credentials).toBe("include");
    expect(headersOf(call)).toEqual({});
    // A read carries no request identity, so an outcome with one matches nothing.
    expect((await transport.invoke(read)).status).toBe("uncertain");
  });

  it("refuses to send a read with more than one item", async () => {
    const stub = stubFetch([completed]);
    const transport = createTransport({ baseUrl: "", fetch: stub.fetch });
    const read = { ...request, method: "GET", items: [{ id: "a" }, { id: "b" }] };
    expect((await transport.invoke(read)).status).toBe("uncertain");
    expect(stub.seen).toEqual([]);
  });
});

const TIMES = '{"expires_at":1000,"login_expires_at":28000}';

describe("the session calls", () => {
  it("lists the environments of the account", async () => {
    const stub = stubFetch([
      () =>
        new Response('{"environments":[{"aud":"a","org":"o","project":"p","env":"e"}]}', {
          status: 200,
        }),
    ]);
    const found = await environments("me@example.test", "pw", { fetch: stub.fetch });
    expect(found).toEqual([{ aud: "a", org: "o", project: "p", env: "e" }]);
    expect(stub.seen[0]?.url).toBe("/password/environments");
    expect(bodyOf(stub.seen[0])).toEqual({ email: "me@example.test", password: "pw" });
  });

  it("signs in, renews and signs out with the cookie carrier", async () => {
    const stub = stubFetch([
      () => new Response(TIMES, { status: 200 }),
      () => new Response(TIMES, { status: 200 }),
      () => new Response(null, { status: 204 }),
    ]);
    const options = { baseUrl: "https://id.test", fetch: stub.fetch };
    const expected = { expiresAt: 1000, loginExpiresAt: 28000 };
    expect(await signIn("me@example.test", "pw", "aud-1", options)).toEqual(expected);
    expect(await renew("aud-1", options)).toEqual(expected);
    await signOut("aud-1", options);

    expect(stub.seen.map((call) => call.url)).toEqual([
      "https://id.test/password/session",
      "https://id.test/password/renew",
      "https://id.test/password/logout",
    ]);
    expect(stub.seen.map(bodyOf)).toEqual([
      { email: "me@example.test", password: "pw", aud: "aud-1", carrier: "cookie" },
      { aud: "aud-1", carrier: "cookie" },
      { aud: "aud-1", carrier: "cookie" },
    ]);
    for (const call of stub.seen) {
      expect(call.init.method).toBe("POST");
      expect(call.init.credentials).toBe("include");
      expect(headersOf(call)).toEqual({ "content-type": "application/json" });
    }
  });

  it("reads a refused renewal as no session", async () => {
    const stub = stubFetch([() => new Response(null, { status: 401 })]);
    expect(await renew("aud-1", { fetch: stub.fetch })).toBeNull();
  });

  it("throws when the password is refused", async () => {
    const stub = stubFetch([() => new Response(null, { status: 401 })]);
    await expect(signIn("me@example.test", "bad", "aud-1", { fetch: stub.fetch })).rejects.toThrow(
      "/password/session answered 401",
    );
  });
});

/** A clock that moves only when the test moves it. */
function fakeClock(start: number): Clock & { advance(to: number): void; pending(): number[] } {
  let now = start;
  const timers: Array<{ at: number; run: () => void; live: boolean }> = [];
  return {
    now: () => now,
    after(delay, run) {
      const timer = { at: now + delay, run, live: true };
      timers.push(timer);
      return () => {
        timer.live = false;
      };
    },
    advance(to) {
      now = to;
      for (const timer of timers) {
        if (timer.live && timer.at <= now) {
          timer.live = false;
          timer.run();
        }
      }
    },
    pending: () => timers.filter((timer) => timer.live).map((timer) => timer.at),
  };
}

/**
 * Let every pending continuation run.
 *
 * One macrotask turn drains the whole microtask queue, however many steps a
 * renewal takes. The fake clock uses no real timer, so nothing else runs here.
 */
async function settle(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 0));
}

/**
 * A reply whose body reads in one microtask.
 *
 * A real `Response` reads its body through the platform stream, whose timing
 * depends on the runtime. The keeper reads only these three members.
 */
function reply(status: number, document?: unknown): () => Response {
  return () =>
    ({
      ok: status >= 200 && status < 300,
      status,
      json: () => Promise.resolve(document),
    }) as Response;
}

function times(expiresAt: number): () => Response {
  return reply(200, { expires_at: expiresAt, login_expires_at: 28_000 });
}

describe("the session keeper", () => {
  it("renews on start and again a minute before the token expires", async () => {
    const clock = fakeClock(100_000);
    const stub = stubFetch([times(1_000), times(1_900)]);
    const states: SessionState[] = [];
    keepSession({ aud: "aud-1", fetch: stub.fetch, clock, onState: (state) => states.push(state) });
    await settle();
    expect(stub.seen.map((call) => call.url)).toEqual(["/password/renew"]);
    expect(states).toEqual([{ status: "signedIn", expiresAt: 1_000, loginExpiresAt: 28_000 }]);
    expect(clock.pending()).toEqual([940_000]);

    clock.advance(939_999);
    await settle();
    expect(stub.seen).toHaveLength(1);
    clock.advance(940_000);
    await settle();
    expect(stub.seen).toHaveLength(2);
    expect(states.at(-1)).toEqual({ status: "signedIn", expiresAt: 1_900, loginExpiresAt: 28_000 });
    expect(clock.pending()).toEqual([1_840_000]);
  });

  it("renews at once when less than a minute is left", async () => {
    const clock = fakeClock(990_000);
    const stub = stubFetch([times(1_000)]);
    keepSession({ aud: "aud-1", fetch: stub.fetch, clock, onState: () => undefined });
    await settle();
    expect(clock.pending()).toEqual([990_000]);
  });

  it("stops after a refused renewal", async () => {
    const clock = fakeClock(0);
    const stub = stubFetch([reply(401)]);
    const states: SessionState[] = [];
    keepSession({ aud: "aud-1", fetch: stub.fetch, clock, onState: (state) => states.push(state) });
    await settle();
    expect(states).toEqual([{ status: "signedOut" }]);
    expect(clock.pending()).toEqual([]);
  });

  it("keeps a new sign in and stops after sign out", async () => {
    const clock = fakeClock(0);
    const stub = stubFetch([
      reply(401),
      times(1_000),
      reply(204),
    ]);
    const states: SessionState[] = [];
    const keeper = keepSession({
      aud: "aud-1",
      fetch: stub.fetch,
      clock,
      onState: (state) => states.push(state),
    });
    await settle();
    await keeper.signIn("me@example.test", "pw");
    expect(clock.pending()).toEqual([940_000]);
    await keeper.signOut();
    expect(clock.pending()).toEqual([]);
    expect(stub.seen.map((call) => call.url)).toEqual([
      "/password/renew",
      "/password/session",
      "/password/logout",
    ]);
    expect(states).toEqual([
      { status: "signedOut" },
      { status: "signedIn", expiresAt: 1_000, loginExpiresAt: 28_000 },
      { status: "signedOut" },
    ]);
    clock.advance(2_000_000);
    await settle();
    expect(stub.seen).toHaveLength(3);
  });

  it("ignores a renewal that answers after sign out", async () => {
    const clock = fakeClock(0);
    let answer: (response: Response) => void = () => undefined;
    const replies: Array<Promise<Response>> = [
      new Promise((resolve) => {
        answer = resolve;
      }),
      Promise.resolve(reply(204)()),
    ];
    const states: SessionState[] = [];
    const keeper = keepSession({
      aud: "aud-1",
      fetch: () => replies.shift() ?? Promise.reject(new Error("no reply is queued")),
      clock,
      onState: (state) => states.push(state),
    });
    await keeper.signOut();
    answer(times(1_000)());
    await settle();
    expect(states).toEqual([{ status: "signedOut" }]);
    expect(clock.pending()).toEqual([]);
  });
});
