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

/**
 * One draft with a row's value filled in at one input path.
 *
 * A `"[]"` in the path marks a repeated member. The fill writes a list that
 * holds one element, and the rest of the path is a member of that element.
 */
export function fillMember<Draft>(draft: Draft, path: MemberPath, value: JsonValue): Draft {
  const at = path.indexOf("[]");
  if (at < 0) {
    return writeMember(draft, path, value);
  }
  const rest = path.slice(at + 1);
  const element = rest.length === 0 ? value : fillMember({}, rest, value);
  return writeMember(draft, path.slice(0, at), [element as JsonValue]);
}

/**
 * One draft with the members of `over` written over `draft`.
 *
 * Objects merge member by member, and lists merge element by element, so a
 * row that fills one member of the first element of a list keeps the other
 * members the operator typed there. Any other value of `over` wins.
 */
export function mergeMembers<Draft>(draft: Draft, over: unknown): Draft {
  if (Array.isArray(over)) {
    const base: readonly unknown[] = Array.isArray(draft) ? draft : [];
    const length = Math.max(base.length, over.length);
    return Array.from({ length }, (_, index) =>
      index < over.length ? mergeMembers(base[index], over[index]) : base[index],
    ) as Draft;
  }
  if (over !== null && typeof over === "object") {
    const merged: { [key: string]: unknown } =
      draft !== null && typeof draft === "object" && !Array.isArray(draft) ? { ...(draft as object) } : {};
    for (const [name, value] of Object.entries(over)) {
      merged[name] = mergeMembers(merged[name], value);
    }
    return merged as Draft;
  }
  return (over === undefined ? draft : over) as Draft;
}

/**
 * One draft with a page control's value, or with that member absent when the
 * control is empty.
 *
 * An empty text or an empty list means the operator asked for nothing, and a
 * member sent empty asks for something else: an empty filter list matches no
 * record. A parent that the removal leaves empty goes too, so a read with no
 * filter carries no filter member.
 */
export function writeControl<Draft>(draft: Draft, path: MemberPath, value: JsonValue): Draft {
  const empty = value === "" || (Array.isArray(value) && value.length === 0);
  return empty ? pruneMember(draft, path) : writeMember(draft, path, value);
}

/**
 * One draft that carries two members only when both are set.
 *
 * A sort names a field and a direction, and the release refuses one without
 * the other. The page keeps each choice, and a read sends neither until the
 * operator chose both.
 */
export function completePair<Draft>(draft: Draft, first: MemberPath, second: MemberPath): Draft {
  if (readMember(draft, first) !== undefined && readMember(draft, second) !== undefined) {
    return draft;
  }
  return pruneMember(pruneMember(draft, first), second);
}

/** One draft with that member absent, and every parent it leaves empty. */
function pruneMember<Draft>(draft: Draft, path: MemberPath): Draft {
  const [name, ...rest] = path;
  if (name === undefined || draft === null || typeof draft !== "object") {
    return draft;
  }
  const object: { [key: string]: unknown } = { ...(draft as object) };
  const child = rest.length === 0 ? undefined : pruneMember(object[name], rest);
  const emptyObject =
    child !== null && typeof child === "object" && !Array.isArray(child) && Object.keys(child).length === 0;
  if (child === undefined || emptyObject) {
    delete object[name];
  } else {
    object[name] = child;
  }
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
