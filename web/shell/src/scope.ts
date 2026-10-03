/**
 * The org and the project the shell signs in to, read when the page loads.
 *
 * `wamn web upload` writes `config.json` beside `index.html` at the release
 * path, and the dev server answers it from `dev.json`. The built files are
 * the same bytes for every org and project (wamn-l2fi).
 */

/** The path of the file, on the page's own origin. */
export const SCOPE_PATH = "/config.json";

/** The org and the project of one deployment of the application. */
export interface Scope {
  readonly org: string;
  readonly project: string;
}

/** Read `config.json`, and refuse a file without a non-empty org and project. */
export async function readScope(fetcher: typeof fetch = fetch): Promise<Scope> {
  const response = await fetcher(SCOPE_PATH, { cache: "no-cache" });
  if (!response.ok) {
    throw new Error(`${SCOPE_PATH} answered ${response.status}; wamn web upload writes it`);
  }
  const body: unknown = await response.json();
  const value = (key: string): string => {
    const field = typeof body === "object" && body !== null ? (body as Record<string, unknown>)[key] : undefined;
    if (typeof field !== "string" || field === "") {
      throw new Error(`${SCOPE_PATH} has no ${key}`);
    }
    return field;
  };
  return { org: value("org"), project: value("project") };
}
