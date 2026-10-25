import type { JsonValue } from "../wire.js";

/** Hash request JSON with sorted object keys, before adding idempotencyKey and requestId. */
export async function requestHash(request: unknown): Promise<string> {
  const json = JSON.parse(JSON.stringify(request)) as JsonValue;
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(canonical(json)));
  return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, "0")).join("");
}

function canonical(value: JsonValue): string {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (value !== null && typeof value === "object") {
    const object = value as { readonly [key: string]: JsonValue };
    return `{${Object.keys(object)
      .sort()
      .map((key) => `${JSON.stringify(key)}:${canonical(object[key]!)}`)
      .join(",")}}`;
  }
  return JSON.stringify(value);
}
