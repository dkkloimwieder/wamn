/**
 * The one query-string encoding of a read request item.
 *
 * A read route is an HTTP GET, so its one request item travels in the query
 * string. Each top-level member is one parameter, and its value is the
 * canonical JSON text of the member, strings included. Parameters and object
 * keys stand in byte order of their UTF-8 names, and every byte outside the
 * RFC 3986 unreserved set is escaped as `%XX` with uppercase hex. The router
 * refuses any other spelling, and the Rust encoder in
 * `apps/platform/execution/contract/src/read_query.rs` reads the same fixture
 * vectors as the test of this module.
 */

import type { JsonValue } from "./wire.js";

const UTF8 = new TextEncoder();

/** Encode one read item as its canonical query string, without the `?`. */
export function encodeReadQuery(item: { readonly [name: string]: JsonValue }): string {
  return byteOrder(Object.keys(item))
    .map((name) => `${escape(name)}=${escape(canonicalJson(item[name] as JsonValue))}`)
    .join("&");
}

/** Compact JSON with every object's keys in byte order of their UTF-8 names. */
function canonicalJson(value: JsonValue): string {
  if (Array.isArray(value)) {
    return `[${value.map(canonicalJson).join(",")}]`;
  }
  if (value !== null && typeof value === "object") {
    const object = value as { readonly [name: string]: JsonValue };
    return `{${byteOrder(Object.keys(object))
      .map((name) => `${JSON.stringify(name)}:${canonicalJson(object[name] as JsonValue)}`)
      .join(",")}}`;
  }
  return JSON.stringify(value);
}

function byteOrder(names: readonly string[]): string[] {
  return [...names].sort((left, right) => {
    const a = UTF8.encode(left);
    const b = UTF8.encode(right);
    for (let index = 0; index < Math.min(a.length, b.length); index += 1) {
      if (a[index] !== b[index]) {
        return (a[index] as number) - (b[index] as number);
      }
    }
    return a.length - b.length;
  });
}

function escape(text: string): string {
  let out = "";
  for (const byte of UTF8.encode(text)) {
    const unreserved =
      (byte >= 0x30 && byte <= 0x39) ||
      (byte >= 0x41 && byte <= 0x5a) ||
      (byte >= 0x61 && byte <= 0x7a) ||
      byte === 0x2d ||
      byte === 0x2e ||
      byte === 0x5f ||
      byte === 0x7e;
    out += unreserved
      ? String.fromCharCode(byte)
      : `%${byte.toString(16).toUpperCase().padStart(2, "0")}`;
  }
  return out;
}
