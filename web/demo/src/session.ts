/**
 * The login the platform already has.
 *
 * `/password/environments` lists what the account can reach, and
 * `/password/session` returns the bearer token for one of them. The token stays
 * in memory, in the caller's signal. Nothing is written to browser storage, and
 * the cookie session is a later epic.
 */

/** One project environment the account can reach. */
export interface Environment {
  readonly aud: string;
  readonly org: string;
  readonly project: string;
  readonly env: string;
}

async function post(path: string, body: unknown): Promise<unknown> {
  const response = await fetch(path, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!response.ok) {
    throw new Error(`${path} answered ${response.status}`);
  }
  return response.json();
}

/** Every environment this account can sign in to. */
export async function environments(email: string, password: string): Promise<Environment[]> {
  const document = (await post("/password/environments", { email, password })) as {
    environments?: Environment[];
  };
  return document.environments ?? [];
}

/** The bearer token for one environment, which the transport carries. */
export async function session(email: string, password: string, aud: string): Promise<string> {
  const document = (await post("/password/session", { email, password, aud })) as {
    access_token?: string;
  };
  if (document.access_token === undefined) {
    throw new Error("the session response carries no token");
  }
  return document.access_token;
}
