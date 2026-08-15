/**
 * The four routes, hash-addressed. Ids are opaque: nothing is assumed about
 * their format beyond being nonempty and bounded once decoded.
 */
export type Route =
  | { readonly kind: "start" }
  | { readonly kind: "run"; readonly id: string }
  | { readonly kind: "report"; readonly id: string }
  | { readonly kind: "draft"; readonly id: string; readonly revision: string }
  | { readonly kind: "not-found"; readonly hash: string };

/** Longest decoded id (or draft revision) a route may carry. */
export const MAX_ID_LENGTH = 128;

/** Decoded, bounded, nonempty — or null, which the caller turns into not-found. */
function decodeSegment(segment: string): string | null {
  let decoded: string;
  try {
    decoded = decodeURIComponent(segment);
  } catch (error) {
    if (error instanceof URIError) {
      return null;
    }
    throw error;
  }
  if (decoded.length === 0 || decoded.length > MAX_ID_LENGTH) {
    return null;
  }
  return decoded;
}

export function parseHash(hash: string): Route {
  const notFound: Route = { kind: "not-found", hash };
  const path = hash.startsWith("#") ? hash.slice(1) : hash;
  if (path === "" || path === "/") {
    return { kind: "start" };
  }

  const segments = path.split("/");
  if (segments[0] !== "") {
    return notFound;
  }
  const [, screen, first, second] = segments;

  if ((screen === "run" || screen === "report") && segments.length === 3) {
    const id = decodeSegment(first);
    return id === null ? notFound : { kind: screen, id };
  }
  if (screen === "draft" && segments.length === 4) {
    const id = decodeSegment(first);
    const revision = decodeSegment(second);
    return id === null || revision === null ? notFound : { kind: "draft", id, revision };
  }
  return notFound;
}

export function toHash(route: Route): string {
  switch (route.kind) {
    case "start":
      return "#/";
    case "run":
    case "report":
      return `#/${route.kind}/${encodeURIComponent(route.id)}`;
    case "draft":
      return `#/draft/${encodeURIComponent(route.id)}/${encodeURIComponent(route.revision)}`;
    case "not-found":
      return route.hash;
  }
}
