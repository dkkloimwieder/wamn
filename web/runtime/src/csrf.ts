/** The header that carries the readable CSRF cookie back to the web proxy. */
export const CSRF_HEADER = "x-wamn-csrf";

export function readCsrf(cookies = globalThis.document?.cookie ?? ""): string | null {
  for (const pair of cookies.split(";")) {
    const at = pair.indexOf("=");
    if (at !== -1 && pair.slice(0, at).trim() === "__Host-wamn-web-csrf") {
      return pair.slice(at + 1).trim();
    }
  }
  return null;
}
