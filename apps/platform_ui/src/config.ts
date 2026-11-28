/** The path of the org configuration at the host root. */
export const CONFIG_PATH = "/config.json";

/** The org of one deployment and the mount of an application page. */
export interface Config {
  readonly org: string;
  readonly mount?: string;
}

/** Read the org configuration at the host root. */
export function readConfig(fetcher: typeof fetch = fetch): Promise<Config> {
  return readConfigAt(fetcher);
}

/** Read app configuration from the first two segments of the page URL. */
export async function readAppConfig(
  pathname = window.location.pathname,
  fetcher: typeof fetch = fetch,
): Promise<Config> {
  const [, project, app] = pathname.split("/", 3);
  if (!project || !app) throw new Error("open the page at its application mount");
  return readConfigAt(fetcher, `/${project}/${app}/`);
}

async function readConfigAt(fetcher: typeof fetch, mount?: string): Promise<Config> {
  const path = mount === undefined ? CONFIG_PATH : `${mount}config.json`;
  const response = await fetcher(path, { cache: "no-cache" });
  if (!response.ok) throw new Error(`${path} answered ${response.status}`);
  const body: unknown = await response.json();
  const org = typeof body === "object" && body !== null ? (body as Record<string, unknown>).org : undefined;
  if (typeof org !== "string" || org === "") throw new Error(`${path} has no org`);
  return mount === undefined ? { org } : { org, mount };
}
