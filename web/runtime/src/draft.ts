/**
 * Reading and writing one member of a draft that an operator is filling.
 *
 * The screen plan states each input as a contract path, for example
 * `change.code`. The generator turns that into the member names the bindings
 * declare and writes them as a literal, so nothing parses a path at run time.
 *
 * Every write returns a new draft, because a component stores the draft
 * wherever its framework keeps state.
 */

import type { JsonValue } from "./wire.js";

/** The member names that reach one value, outermost first. */
export type MemberPath = readonly string[];

/** One member's value, or undefined when the draft does not carry it. */
export function readMember(draft: unknown, path: MemberPath): JsonValue | undefined {
  let value: unknown = draft;
  for (const name of path) {
    if (value === null || typeof value !== "object") {
      return undefined;
    }
    value = (value as { [key: string]: unknown })[name];
  }
  return value as JsonValue | undefined;
}

/**
 * One draft with that member set, and every other member unchanged.
 *
 * A member on the way that the draft does not carry becomes an object, so a
 * form can fill a nested member before its parent exists.
 */
export function writeMember<Draft>(draft: Draft, path: MemberPath, value: JsonValue): Draft {
  const [name, ...rest] = path;
  if (name === undefined) {
    return value as unknown as Draft;
  }
  const object: { [key: string]: unknown } =
    draft !== null && typeof draft === "object" ? { ...(draft as object) } : {};
  object[name] = rest.length === 0 ? value : writeMember(object[name] ?? {}, rest, value);
  return object as Draft;
}

/** One draft with that member absent, which an omittable input needs. */
export function clearMember<Draft>(draft: Draft, path: MemberPath): Draft {
  const [name, ...rest] = path;
  if (name === undefined || draft === null || typeof draft !== "object") {
    return draft;
  }
  const object: { [key: string]: unknown } = { ...(draft as object) };
  if (rest.length === 0) {
    delete object[name];
  } else {
    object[name] = clearMember(object[name], rest);
  }
  return object as Draft;
}
