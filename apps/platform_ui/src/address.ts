import { createSignal, onCleanup } from "solid-js";

/** Browser history belongs to the shell. Screens receive open and close callbacks. */
export function createAddress(mount?: string) {
  const read = () => new URL(window.location.href);
  const [url, setUrl] = createSignal(read());
  const changed = () => setUrl(read());
  window.addEventListener("popstate", changed);
  onCleanup(() => window.removeEventListener("popstate", changed));
  const navigate = (path: string, replace = false, opened = false) => {
    if (mount !== undefined && !path.startsWith(mount)) {
      window.location.assign(path);
      return;
    }
    window.history[replace ? "replaceState" : "pushState"]({ opened }, "", path);
    changed();
  };
  return { url, navigate };
}

export type Address = ReturnType<typeof createAddress>;

/** Keep the env named "envs" distinct from the project list at /envs. */
export function envSegment(name: string): string {
  return name === "envs" ? "%65nvs" : encodeURIComponent(name);
}

export function matchPath(pattern: string, path: string): Record<string, string> | undefined {
  const expected = pattern.split("/");
  const actual = path.split("/");
  if (expected.length !== actual.length) return undefined;
  const params: Record<string, string> = {};
  for (const [index, part] of expected.entries()) {
    const value = actual[index]!;
    if (part.startsWith(":")) {
      try {
        params[part.slice(1)] = decodeURIComponent(value);
      } catch {
        return undefined;
      }
    } else if (part !== value) return undefined;
  }
  return params;
}

/** Keep the first value of each query member, as the Solid 1 shell does. */
export function queryValues(query: URLSearchParams): Record<string, string> {
  const values: Record<string, string> = {};
  for (const [name, value] of query) if (!Object.hasOwn(values, name)) values[name] = value;
  return values;
}
