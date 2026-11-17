/**
 * The cookie session: the carrier each transport mode sends, the login calls,
 * and the keeper that renews the session over a fake clock.
 */

import { describe, expect, it } from "vitest";

import {
  keepSession,
  renew,
  signIn,
  signOut,
  type Clock,
  type SessionState,
} from "../src/session.js";
import { createTransport, type BearerOptions, type CookieOptions, type TransportOptions } from "../src/transport.js";
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
    return reply === undefined ? Promise.reject(new Error("no reply is queued")) : Promise.resolve(reply());
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
    type: "get",
    transaction: "implicit",
    reads: [],
    writes: null,
  };
  const request = {
    operation: "platform-fixture:widget/get@1.0.0",
    method: "POST",
    template: "/widget/get",
    contract,
    items: [{ request_id: "r1" }],
  };
  const completed = (): Response => new Response('[{"request_id":"r1","value":{"id":"a"}}]', { status: 200 });

  async function send(
    options: Omit<BearerOptions, "baseUrl"> | Omit<CookieOptions, "baseUrl">,
  ): Promise<Seen | undefined> {
    const stub = stubFetch([completed]);
    const transport = createTransport({
      ...options,
      baseUrl: "https://example.test",
      fetch: stub.fetch,
    } as TransportOptions);
    {
      const observed = await transport.invoke(request);
      expect(observed).toHaveProperty(["status"], "completed");
      expect(observed).toHaveProperty(["value", "id"], "a");
    }
    return stub.seen[0];
  }

  it("sends the cookies and the CSRF token, and no authorization, in cookie mode", async () => {
    const call = await send({
      cookie: true,
      cookies: () => "theme=dark; __Host-wamn-web-csrf=c5rf; other=x",
    });
    expect(call?.init.credentials).toBe("include");
    expect(new Headers(call?.init.headers).has("authorization")).toBe(false);
    {
      const observed = headersOf(call);
      expect(observed).toHaveProperty(["content-type"], "application/json");
      expect(observed).toHaveProperty(["x-wamn-csrf"], "c5rf");
    }
  });

  it("reads the CSRF cookie again on each request", async () => {
    let jar = "__Host-wamn-web-csrf=first";
    const stub = stubFetch([completed, completed]);
    const transport = createTransport({
      baseUrl: "",
      cookie: true,
      cookies: () => jar,
      fetch: stub.fetch,
    });
    await transport.invoke(request);
    jar = "__Host-wamn-web-csrf=second";
    await transport.invoke(request);
    {
      const observed = stub.seen.map((call) => headersOf(call)["x-wamn-csrf"]);
      expect(observed).toHaveProperty(["length"], 2);
      expect(observed).toHaveProperty(["0"], "first");
      expect(observed).toHaveProperty(["1"], "second");
    }
  });

  it("sends no CSRF header when the cookie is absent, and the router decides", async () => {
    const call = await send({ cookie: true, cookies: () => "theme=dark" });
    expect(new Headers(call?.init.headers).has("x-wamn-csrf")).toBe(false);
    expect(call?.init.credentials).toBe("include");
    expect(new Headers(call?.init.headers).has("authorization")).toBe(false);
    {
      const observed = headersOf(call);
      expect(observed).toHaveProperty(["content-type"], "application/json");
    }
  });

  it("sends the bearer token and no cookies in bearer mode", async () => {
    const call = await send({ credential: "token" });
    expect(call?.init.credentials).toBeUndefined();
    expect(new Headers(call?.init.headers).has("x-wamn-csrf")).toBe(false);
    {
      const observed = headersOf(call);
      expect(observed).toHaveProperty(["content-type"], "application/json");
      expect(observed).toHaveProperty(["authorization"], "Bearer token");
    }
  });

  it("sends no credential when a bearer caller holds none", async () => {
    const call = await send({});
    expect(new Headers(call?.init.headers).has("authorization")).toBe(false);
    expect(call?.init.credentials).toBeUndefined();
    expect(new Headers(call?.init.headers).has("x-wamn-csrf")).toBe(false);
    {
      const observed = headersOf(call);
      expect(observed).toHaveProperty(["content-type"], "application/json");
    }
  });

  it("sends a read as a GET with its one item in the query and no CSRF header", async () => {
    const stub = stubFetch([
      () => new Response('[{"value":{"id":"a"}}]', { status: 200 }),
      () => new Response('[{"request_id":"r1","value":{"id":"a"}}]', { status: 200 }),
    ]);
    const transport = createTransport({
      baseUrl: "https://example.test",
      cookie: true,
      cookies: () => "__Host-wamn-web-csrf=c5rf",
      fetch: stub.fetch,
    });
    const read = { ...request, method: "GET", items: [{ id: "a" }] };
    {
      const observed = await transport.invoke(read);
      expect(observed).toHaveProperty(["status"], "completed");
      expect(observed).toHaveProperty(["value", "id"], "a");
    }
    const call = stub.seen[0];
    expect(call?.url).toBe("https://example.test/widget/get?id=%22a%22");
    expect(call?.init.method).toBe("GET");
    expect(call?.init.body).toBeUndefined();
    expect(call?.init.credentials).toBe("include");
    expect(new Headers(call?.init.headers).has("authorization")).toBe(false);
    const headers = new Headers(call?.init.headers);
    expect(headers.has("x-wamn-csrf")).toBe(false);
    expect(headers.has("content-type")).toBe(false);
    // A read carries no request identity, so an outcome with one matches nothing.
    expect((await transport.invoke(read)).status).toBe("uncertain");
  });

  it("refuses to send a read with more than one item", async () => {
    const stub = stubFetch([completed]);
    const transport = createTransport({ baseUrl: "", fetch: stub.fetch });
    const read = { ...request, method: "GET", items: [{ id: "a" }, { id: "b" }] };
    expect((await transport.invoke(read)).status).toBe("uncertain");
    {
      const observed = stub.seen;
      expect(observed).toHaveProperty(["length"], 0);
    }
  });
});

const TIMES = '{"sub":"user-1","expires_at":1000,"login_expires_at":28000}';

describe("the session calls", () => {
  it("signs in, renews and signs out through the web proxy", async () => {
    const stub = stubFetch([
      () => new Response(TIMES, { status: 200 }),
      () => new Response(TIMES, { status: 200 }),
      () => new Response(null, { status: 204 }),
    ]);
    const options = { baseUrl: "https://id.test", fetch: stub.fetch };
    for (const observed of [await signIn("me@example.test", "pw", "aud-1", options), await renew("aud-1", options)]) {
      expect(observed).not.toBeNull();
      if (observed === null) throw new Error("session expected");
      expect(observed.userId).toBe("user-1");
      expect(observed.expiresAt).toBe(1000);
      expect(observed.loginExpiresAt).toBe(28000);
    }
    await signOut("aud-1", options);

    {
      const observed = stub.seen.map((call) => call.url);
      expect(observed).toHaveProperty(["length"], 3);
      expect(observed).toHaveProperty(["0"], "https://id.test/password/session");
      expect(observed).toHaveProperty(["1"], "https://id.test/password/renew");
      expect(observed).toHaveProperty(["2"], "https://id.test/password/logout");
    }
    {
      const observed = stub.seen.map(bodyOf);
      expect(observed).toHaveProperty(["length"], 3);
      expect(observed).toHaveProperty(["0", "email"], "me@example.test");
      expect(observed).toHaveProperty(["0", "password"], "pw");
      expect(observed).toHaveProperty(["0", "aud"], "aud-1");
      expect(observed).toHaveProperty(["1", "aud"], "aud-1");
      expect(observed).toHaveProperty(["2", "aud"], "aud-1");
    }
    for (const call of stub.seen) {
      expect(call.init.method).toBe("POST");
      expect(call.init.credentials).toBe("include");
      {
        const observed = headersOf(call);
        expect(observed).toHaveProperty(["content-type"], "application/json");
      }
    }
  });

  it("reads a refused renewal as no session", async () => {
    const stub = stubFetch([() => new Response(null, { status: 401 })]);
    expect(await renew("aud-1", { fetch: stub.fetch })).toBeNull();
  });

  it("throws when the password is refused", async () => {
    const stub = stubFetch([() => new Response(null, { status: 401 })]);
    await expect(signIn("me@example.test", "bad", "aud-1", { fetch: stub.fetch })).rejects.toThrow();
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
  return reply(200, { sub: "user-1", expires_at: expiresAt, login_expires_at: 28_000 });
}

describe("the session keeper", () => {
  it("renews on start and again a minute before the token expires", async () => {
    const clock = fakeClock(100_000);
    const stub = stubFetch([times(1_000), times(1_900)]);
    const states: SessionState[] = [];
    keepSession({ aud: "aud-1", fetch: stub.fetch, clock, onState: (state) => states.push(state) });
    await settle();
    {
      const observed = stub.seen.map((call) => call.url);
      expect(observed).toHaveProperty(["length"], 1);
      expect(observed).toHaveProperty(["0"], "/password/renew");
    }
    {
      const observed = states;
      expect(observed).toHaveProperty(["length"], 1);
      expect(observed).toHaveProperty(["0", "status"], "signedIn");
      expect(observed).toHaveProperty(["0", "userId"], "user-1");
      expect(observed).toHaveProperty(["0", "expiresAt"], 1_000);
      expect(observed).toHaveProperty(["0", "loginExpiresAt"], 28_000);
    }
    {
      const observed = clock.pending();
      expect(observed).toHaveProperty(["length"], 1);
      expect(observed).toHaveProperty(["0"], 940_000);
    }

    clock.advance(939_999);
    await settle();
    expect(stub.seen).toHaveLength(1);
    clock.advance(940_000);
    await settle();
    expect(stub.seen).toHaveLength(2);
    {
      const observed = states.at(-1);
      expect(observed).toHaveProperty(["status"], "signedIn");
      expect(observed).toHaveProperty(["userId"], "user-1");
      expect(observed).toHaveProperty(["expiresAt"], 1_900);
      expect(observed).toHaveProperty(["loginExpiresAt"], 28_000);
    }
    {
      const observed = clock.pending();
      expect(observed).toHaveProperty(["length"], 1);
      expect(observed).toHaveProperty(["0"], 1_840_000);
    }
  });

  it("renews at once when less than a minute is left", async () => {
    const clock = fakeClock(990_000);
    const stub = stubFetch([times(1_000)]);
    keepSession({ aud: "aud-1", fetch: stub.fetch, clock, onState: () => undefined });
    await settle();
    {
      const observed = clock.pending();
      expect(observed).toHaveProperty(["length"], 1);
      expect(observed).toHaveProperty(["0"], 990_000);
    }
  });

  it("stops after a refused renewal", async () => {
    const clock = fakeClock(0);
    const stub = stubFetch([reply(401)]);
    const states: SessionState[] = [];
    keepSession({ aud: "aud-1", fetch: stub.fetch, clock, onState: (state) => states.push(state) });
    await settle();
    {
      const observed = states;
      expect(observed).toHaveProperty(["length"], 1);
      expect(observed).toHaveProperty(["0", "status"], "signedOut");
    }
    {
      const observed = clock.pending();
      expect(observed).toHaveProperty(["length"], 0);
    }
  });

  it("keeps a new sign in and stops after sign out", async () => {
    const clock = fakeClock(0);
    const stub = stubFetch([reply(401), times(1_000), reply(204)]);
    const states: SessionState[] = [];
    const keeper = keepSession({
      aud: "aud-1",
      fetch: stub.fetch,
      clock,
      onState: (state) => states.push(state),
    });
    await settle();
    await keeper.signIn("me@example.test", "pw");
    {
      const observed = clock.pending();
      expect(observed).toHaveProperty(["length"], 1);
      expect(observed).toHaveProperty(["0"], 940_000);
    }
    await keeper.signOut();
    {
      const observed = clock.pending();
      expect(observed).toHaveProperty(["length"], 0);
    }
    {
      const observed = stub.seen.map((call) => call.url);
      expect(observed).toHaveProperty(["length"], 3);
      expect(observed).toHaveProperty(["0"], "/password/renew");
      expect(observed).toHaveProperty(["1"], "/password/session");
      expect(observed).toHaveProperty(["2"], "/password/logout");
    }
    {
      const observed = states;
      expect(observed).toHaveProperty(["length"], 3);
      expect(observed).toHaveProperty(["0", "status"], "signedOut");
      expect(observed).toHaveProperty(["1", "status"], "signedIn");
      expect(observed).toHaveProperty(["1", "userId"], "user-1");
      expect(observed).toHaveProperty(["1", "expiresAt"], 1_000);
      expect(observed).toHaveProperty(["1", "loginExpiresAt"], 28_000);
      expect(observed).toHaveProperty(["2", "status"], "signedOut");
    }
    clock.advance(2_000_000);
    await settle();
    expect(stub.seen).toHaveLength(3);
  });

  it("keeps the session and retries when the renewal cannot reach the service", async () => {
    const clock = fakeClock(100_000);
    const offline = () => {
      throw new TypeError("Failed to fetch");
    };
    const stub = stubFetch([times(1_000), offline, reply(503), times(1_900)]);
    const states: SessionState[] = [];
    keepSession({ aud: "aud-1", fetch: stub.fetch, clock, onState: (state) => states.push(state) });
    await settle();
    clock.advance(940_000);
    await settle();
    expect(states).toHaveLength(1);
    {
      const observed = clock.pending();
      expect(observed).toHaveProperty(["length"], 1);
      expect(observed).toHaveProperty(["0"], 945_000);
    }
    clock.advance(945_000);
    await settle();
    expect(states).toHaveLength(1);
    {
      const observed = clock.pending();
      expect(observed).toHaveProperty(["length"], 1);
      expect(observed).toHaveProperty(["0"], 955_000);
    }
    clock.advance(955_000);
    await settle();
    expect(stub.seen).toHaveLength(4);
    {
      const observed = states;
      expect(observed).toHaveProperty(["length"], 2);
      expect(observed).toHaveProperty(["0", "status"], "signedIn");
      expect(observed).toHaveProperty(["0", "userId"], "user-1");
      expect(observed).toHaveProperty(["0", "expiresAt"], 1_000);
      expect(observed).toHaveProperty(["0", "loginExpiresAt"], 28_000);
      expect(observed).toHaveProperty(["1", "status"], "signedIn");
      expect(observed).toHaveProperty(["1", "userId"], "user-1");
      expect(observed).toHaveProperty(["1", "expiresAt"], 1_900);
      expect(observed).toHaveProperty(["1", "loginExpiresAt"], 28_000);
    }
    {
      const observed = clock.pending();
      expect(observed).toHaveProperty(["length"], 1);
      expect(observed).toHaveProperty(["0"], 1_840_000);
    }
  });

  it("caps the wait between attempts at a minute", async () => {
    const clock = fakeClock(0);
    const offline = () => {
      throw new TypeError("Failed to fetch");
    };
    const stub = stubFetch([offline, offline, offline, offline, offline, offline]);
    const states: SessionState[] = [];
    keepSession({ aud: "aud-1", fetch: stub.fetch, clock, onState: (state) => states.push(state) });
    const waits: number[] = [];
    for (let attempt = 0; attempt < 6; attempt += 1) {
      await settle();
      const [at] = clock.pending();
      waits.push(at! - clock.now());
      clock.advance(at!);
    }
    {
      const observed = waits;
      expect(observed).toHaveProperty(["length"], 6);
      expect(observed).toHaveProperty(["0"], 5_000);
      expect(observed).toHaveProperty(["1"], 10_000);
      expect(observed).toHaveProperty(["2"], 20_000);
      expect(observed).toHaveProperty(["3"], 40_000);
      expect(observed).toHaveProperty(["4"], 60_000);
      expect(observed).toHaveProperty(["5"], 60_000);
    }
    {
      const observed = states;
      expect(observed).toHaveProperty(["length"], 1);
      expect(observed).toHaveProperty(["0", "status"], "unreachable");
      expect(observed).toHaveProperty(["0", "reason"], expect.any(String));
    }
  });

  it("signs out when the service refuses a retried renewal", async () => {
    const clock = fakeClock(0);
    const stub = stubFetch([reply(502), reply(401)]);
    const states: SessionState[] = [];
    keepSession({ aud: "aud-1", fetch: stub.fetch, clock, onState: (state) => states.push(state) });
    await settle();
    {
      const observed = states;
      expect(observed).toHaveProperty(["length"], 1);
      expect(observed).toHaveProperty(["0", "status"], "unreachable");
      expect(observed).toHaveProperty(["0", "reason"], expect.any(String));
    }
    clock.advance(5_000);
    await settle();
    {
      const observed = states.at(-1);
      expect(observed).toHaveProperty(["status"], "signedOut");
    }
    {
      const observed = clock.pending();
      expect(observed).toHaveProperty(["length"], 0);
    }
  });

  it("retries at once when the network returns", async () => {
    const clock = fakeClock(0);
    const offline = () => {
      throw new TypeError("Failed to fetch");
    };
    const stub = stubFetch([offline, times(1_000)]);
    let online: () => void = () => undefined;
    const states: SessionState[] = [];
    const keeper = keepSession({
      aud: "aud-1",
      fetch: stub.fetch,
      clock,
      online: (run) => {
        online = run;
        return () => {
          online = () => undefined;
        };
      },
      onState: (state) => states.push(state),
    });
    await settle();
    {
      const observed = clock.pending();
      expect(observed).toHaveProperty(["length"], 1);
      expect(observed).toHaveProperty(["0"], 5_000);
    }
    online();
    await settle();
    expect(stub.seen).toHaveLength(2);
    {
      const observed = states.at(-1);
      expect(observed).toHaveProperty(["status"], "signedIn");
      expect(observed).toHaveProperty(["userId"], "user-1");
      expect(observed).toHaveProperty(["expiresAt"], 1_000);
      expect(observed).toHaveProperty(["loginExpiresAt"], 28_000);
    }
    {
      const observed = clock.pending();
      expect(observed).toHaveProperty(["length"], 1);
      expect(observed).toHaveProperty(["0"], 940_000);
    }
    // A return of the network while the session is current renews nothing.
    online();
    await settle();
    expect(stub.seen).toHaveLength(2);
    keeper.stop();
  });

  it("fails on an answer it cannot read", async () => {
    const clock = fakeClock(0);
    const stub = stubFetch([reply(400)]);
    const states: SessionState[] = [];
    keepSession({ aud: "aud-1", fetch: stub.fetch, clock, onState: (state) => states.push(state) });
    await settle();
    {
      const observed = states;
      expect(observed).toHaveProperty(["length"], 1);
      expect(observed).toHaveProperty(["0", "status"], "failed");
      expect(observed).toHaveProperty(["0", "reason"], expect.any(String));
    }
    {
      const observed = clock.pending();
      expect(observed).toHaveProperty(["length"], 0);
    }
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
    {
      const observed = states;
      expect(observed).toHaveProperty(["length"], 1);
      expect(observed).toHaveProperty(["0", "status"], "signedOut");
    }
    {
      const observed = clock.pending();
      expect(observed).toHaveProperty(["length"], 0);
    }
  });
});
