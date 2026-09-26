/**
 * The three values a caller writes into an item that the operator never types.
 *
 * The screen plan names the input paths that carry them:
 * `crates/schema/generator/src/client_plan.rs` lists `request_id`,
 * `idempotency_key` and `occurred_at`. A generated component writes the exact
 * path and takes the value from here.
 */

import { type MemberPath, writeMember } from "./draft.js";

/**
 * One submission attempt's identity.
 *
 * The release echoes it, and the transport matches the echo before it calls a
 * reply a completion. Every attempt gets a new one.
 */
export function newRequestId(): string {
  return crypto.randomUUID();
}

/**
 * The replay key of one submission intent.
 *
 * A retry of the same intent keeps the key it started with, so the release
 * returns the first outcome instead of acting twice.
 */
export function newIdempotencyKey(): string {
  return crypto.randomUUID();
}

/**
 * The time at which the operator started the intent, in UTC.
 *
 * The contract spells a timestamp as RFC 3339 with microseconds, and a browser
 * clock reads milliseconds, so the last three digits are zeros.
 */
export function occurredAt(now: Date = new Date()): string {
  return `${now.toISOString().slice(0, -1)}000Z`;
}

/** One input the operator never types: its path, and the value it takes. */
export interface SuppliedInput {
  readonly input: MemberPath;
  readonly kind: "requestId" | "idempotencyKey" | "occurredAt";
}

/** Writes a fresh value into each supplied input of one item. */
export function writeSupplied<Item>(item: Item, supplied: readonly SuppliedInput[]): Item {
  let next = item;
  for (const { input, kind } of supplied) {
    const value =
      kind === "requestId" ? newRequestId() : kind === "idempotencyKey" ? newIdempotencyKey() : occurredAt();
    next = writeMember(next, input, value);
  }
  return next;
}
