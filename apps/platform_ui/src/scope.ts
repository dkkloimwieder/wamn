/**
 * The org the shell signs in to, read when the page loads.
 *
 * `wamn web upload` writes `config.json` beside `index.html` at the platform version
 * path. The built files are the same bytes for every org.
 */

/** The path of the file, on the page's own origin. */
export const SCOPE_PATH = "/config.json";

/** The org of one deployment of the platform UI. */
export interface Scope {
  readonly org: string;
}

/** Read `config.json`, and refuse a file without a non-empty org. */
export async function readScope(fetcher: typeof fetch = fetch): Promise<Scope> {
  const response = await fetcher(SCOPE_PATH, { cache: "no-cache" });
  if (!response.ok) {
    throw new Error(`${SCOPE_PATH} answered ${response.status}; wamn web upload writes it`);
  }
  const body: unknown = await response.json();
  const org = typeof body === "object" && body !== null ? (body as Record<string, unknown>).org : undefined;
  if (typeof org !== "string" || org === "") {
    throw new Error(`${SCOPE_PATH} has no org`);
  }
  return { org };
}
