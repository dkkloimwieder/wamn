import { createSignal, onCleanup } from "solid-js";
import type { Scope } from "./scope";

export type Kind = "application" | "control";

/** An audience belongs to this deployment's org. */
export function audienceKind(aud: string, scope: Scope): Kind | undefined {
  if (aud.startsWith(`urn:wamn:project-env:${scope.org}:`)) return "application";
  return aud === `urn:wamn:control:${scope.org}` ? "control" : undefined;
}

/** Browser history belongs to the shell. Screens receive open and close callbacks. */
export function createAddress() {
  const read = () => new URL(window.location.href);
  const [url, setUrl] = createSignal(read());
  const changed = () => setUrl(read());
  window.addEventListener("popstate", changed);
  onCleanup(() => window.removeEventListener("popstate", changed));
  const navigate = (path: string, replace = false, opened = false) => {
    window.history[replace ? "replaceState" : "pushState"]({ opened }, "", path);
    changed();
  };
  return { url, navigate };
}

export type Address = ReturnType<typeof createAddress>;

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
