/**
 * The platform login, carried by cookie.
 *
 * `/password/environments` lists what the account can reach, and
 * `/password/session` signs in to one of them. With the carrier "cookie" the
 * identity service sets the session, CSRF and renewal cookies, and the body
 * holds only the two expiry times. The page holds no token and writes nothing
 * to browser storage. The keeper renews the session on load and again before
 * it expires, so a reload does not ask for the password.
 */

import { z } from "zod";

/** One project environment the account can reach. */
export interface Environment {
  readonly aud: string;
  readonly org: string;
  readonly project: string;
  readonly env: string;
}

/** When the session token and the login behind it expire, in Unix seconds. */
export interface SessionTimes {
  readonly expiresAt: number;
  readonly loginExpiresAt: number;
}

/** What a caller supplies to reach the identity service. */
export interface SessionOptions {
  /** Where the identity service is served. The same origin is the default. */
  readonly baseUrl?: string;
  /** The fetch to call. The global one is the default. */
  readonly fetch?: typeof globalThis.fetch;
}

/** The cookie mode body of a session and of a renewal. */
const TIMES = z.looseObject({
  expires_at: z.number(),
  login_expires_at: z.number(),
});

async function post(path: string, body: unknown, options: SessionOptions): Promise<Response> {
  const call = options.fetch ?? globalThis.fetch;
  return call(`${options.baseUrl ?? ""}${path}`, {
    method: "POST",
    credentials: "include",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
}

function refusedBy(path: string, response: Response): Error {
  return new Error(`${path} answered ${response.status}`);
}

async function times(response: Response): Promise<SessionTimes> {
  const document = TIMES.parse(await response.json());
  return { expiresAt: document.expires_at, loginExpiresAt: document.login_expires_at };
}

/** Every environment this account can sign in to. */
export async function environments(
  email: string,
  password: string,
  options: SessionOptions = {},
): Promise<Environment[]> {
  const response = await post("/password/environments", { email, password }, options);
  if (!response.ok) {
    throw refusedBy("/password/environments", response);
  }
  const document = (await response.json()) as { environments?: Environment[] };
  return document.environments ?? [];
}

/** Sign in to one environment. The identity service sets the cookies. */
export async function signIn(
  email: string,
  password: string,
  aud: string,
  options: SessionOptions = {},
): Promise<SessionTimes> {
  const response = await post(
    "/password/session",
    { email, password, aud, carrier: "cookie" },
    options,
  );
  if (!response.ok) {
    throw refusedBy("/password/session", response);
  }
  return times(response);
}

/**
 * Renew the session from its renewal cookie, or null when the identity service
 * refuses, because the login ended or the browser holds no session.
 */
export async function renew(aud: string, options: SessionOptions = {}): Promise<SessionTimes | null> {
  const response = await post("/password/renew", { aud, carrier: "cookie" }, options);
  if (response.status === 401) {
    return null;
  }
  if (!response.ok) {
    throw refusedBy("/password/renew", response);
  }
  return times(response);
}

/** End the login. The identity service revokes it and clears the cookies. */
export async function signOut(aud: string, options: SessionOptions = {}): Promise<void> {
  const response = await post("/password/logout", { aud, carrier: "cookie" }, options);
  if (!response.ok) {
    throw refusedBy("/password/logout", response);
  }
}

/** The session as the keeper reports it. */
export type SessionState =
  | ({ readonly status: "signedIn" } & SessionTimes)
  | { readonly status: "signedOut" }
  | { readonly status: "failed"; readonly reason: string };

/** The clock the keeper schedules against. */
export interface Clock {
  /** The current time, in milliseconds since the Unix epoch. */
  now(): number;
  /** Run once after the delay, in milliseconds. The result cancels it. */
  after(delay: number, run: () => void): () => void;
}

/** What the keeper needs besides the identity service. */
export interface KeeperOptions extends SessionOptions {
  /** The environment the page serves. One origin serves one environment. */
  readonly aud: string;
  /** Called on every change of the session. */
  readonly onState: (state: SessionState) => void;
  /** The clock to schedule against. The system clock is the default. */
  readonly clock?: Clock;
}

/** The running keeper of one session. */
export interface SessionKeeper {
  /** Sign in, and keep the new session. It throws when the service refuses. */
  signIn(email: string, password: string): Promise<void>;
  /** Sign out, and stop renewing. */
  signOut(): Promise<void>;
  /** Stop renewing, and keep the session as it is. */
  stop(): void;
}

/** How long before the token expires the keeper renews it, in milliseconds. */
const RENEW_BEFORE = 60_000;

const SYSTEM_CLOCK: Clock = {
  now: () => Date.now(),
  after: (delay, run) => {
    const handle = setTimeout(run, delay);
    return () => clearTimeout(handle);
  },
};

/**
 * Keep one cookie session alive.
 *
 * It renews at once, which restores the session after a reload, and again a
 * minute before each token expires. A refused renewal reports the page signed
 * out and stops. Any other failure reports why and stops too, so the page asks
 * again rather than guess.
 */
export function keepSession(options: KeeperOptions): SessionKeeper {
  const clock = options.clock ?? SYSTEM_CLOCK;
  // Each start, sign in, sign out and stop moves the generation, so a reply
  // that arrives after one of them changes nothing.
  let generation = 0;
  let cancel: (() => void) | null = null;

  function halt(): number {
    cancel?.();
    cancel = null;
    generation += 1;
    return generation;
  }

  function keep(current: number, session: SessionTimes): void {
    if (current !== generation) {
      return;
    }
    options.onState({ status: "signedIn", ...session });
    const delay = Math.max(0, session.expiresAt * 1000 - RENEW_BEFORE - clock.now());
    cancel = clock.after(delay, () => void renewal(current));
  }

  function fail(current: number, error: unknown): void {
    if (current === generation) {
      halt();
      options.onState({ status: "failed", reason: String(error) });
    }
  }

  async function renewal(current: number): Promise<void> {
    cancel = null;
    try {
      const session = await renew(options.aud, options);
      if (current !== generation) {
        return;
      }
      if (session === null) {
        halt();
        options.onState({ status: "signedOut" });
        return;
      }
      keep(current, session);
    } catch (error) {
      fail(current, error);
    }
  }

  void renewal(halt());

  return {
    async signIn(email, password) {
      // A refused password throws to the form that sent it, and the state
      // stays as it was.
      const current = halt();
      keep(current, await signIn(email, password, options.aud, options));
    },
    async signOut() {
      const current = halt();
      try {
        await signOut(options.aud, options);
        if (current === generation) {
          options.onState({ status: "signedOut" });
        }
      } catch (error) {
        fail(current, error);
      }
    },
    stop() {
      halt();
    },
  };
}
