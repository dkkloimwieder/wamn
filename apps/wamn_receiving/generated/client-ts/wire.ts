// @generated from the client-contract IR; do not edit.

/** A UUID in hyphenated form. */
export type Uuid = string;

/** An RFC 3339 timestamp in UTC, to microseconds. */
export type Timestamptz = string;

/** A 64-bit integer as a decimal string. A number loses precision above 2^53. */
export type Int64 = string;

/** An exact decimal as a canonical string. */
export type Numeric = string;

/** Any JSON value. */
export type JsonValue =
  | null
  | boolean
  | number
  | string
  | readonly JsonValue[]
  | { readonly [key: string]: JsonValue };

/** What one operation's response contract states. A transport classifies with it. */
export interface ResponseContract {
  /** `one`, `bounded_list`, `page`, `none`, or null when the release states none. */
  readonly resultClass: string | null;
  /** The partial completion schema, as published JSON text. */
  readonly partialSchema: string | null;
  /** Every refusal literal the operation declares. */
  readonly errors: readonly string[];
  /** The replay guarantee the release serves. */
  readonly replay: "claim" | "state" | null;
}

/** One request. It carries no host, no base URL and no credential. */
export interface WireRequest {
  /** The exact canonical operation identity. */
  readonly operation: string;
  /** The method the release publishes. */
  readonly method: string;
  /** The path template the release publishes. */
  readonly template: string;
  /** Whether the operation admits only a fresh credential. */
  readonly freshOnly: boolean;
  /** What the response must satisfy. */
  readonly contract: ResponseContract;
  /** The submitted items, in wire spelling. */
  readonly items: readonly JsonValue[];
}

/**
 * The outcome of one submitted intent.
 *
 * The four members carry the whole meaning: the intent completed, it completed
 * in part, the operation refused it, or its completion is unknown. A caller
 * branches on `status` and never catches an exception.
 */
export type Outcome<T> =
  | { readonly status: "completed"; readonly value: T }
  | {
      readonly status: "partiallyCompleted";
      readonly committedResult: T;
      readonly failedOutcome: JsonValue;
    }
  | { readonly status: "refused"; readonly code: string; readonly detail: JsonValue }
  | {
      readonly status: "uncertain";
      readonly reason: string;
      readonly retryRefusal: JsonValue | null;
    };

/** Everything one operation sends except its items. */
export type OperationRoute = Omit<WireRequest, "items">;

/**
 * The transport an application supplies.
 *
 * It owns the URL, the credential, the request envelope and the classification
 * of the response into one `Outcome`. The bindings only state what to send.
 */
export interface Transport {
  invoke(request: WireRequest): Promise<Outcome<JsonValue>>;
}

function convertKeys(value: unknown, key: (name: string) => string): JsonValue {
  if (Array.isArray(value)) {
    return value.map((item) => convertKeys(item, key));
  }
  if (value !== null && typeof value === "object") {
    const converted: { [name: string]: JsonValue } = {};
    for (const [name, member] of Object.entries(value)) {
      converted[key(name)] = convertKeys(member, key);
    }
    return converted;
  }
  return value as JsonValue;
}

function toCamel(name: string): string {
  return name.replace(/_([a-z])/g, (_match, letter: string) => letter.toUpperCase());
}

function toSnake(member: string): string {
  return member.replace(/[A-Z]/g, (letter) => `_${letter.toLowerCase()}`);
}

/** Convert wire keys to TypeScript members. */
export function fromWire(value: unknown): JsonValue {
  return convertKeys(value, toCamel);
}

/** Convert TypeScript members to wire keys. */
export function toWire(value: unknown): JsonValue {
  return convertKeys(value, toSnake);
}

/**
 * Rename the keys inside one outcome and state its result type.
 *
 * The cast is unchecked, exactly as the Rust client returns a JSON value that
 * the caller reads through its declared result type. The transport already
 * held the response to the operation's contract.
 */
export function reviveOutcome<T>(outcome: Outcome<JsonValue>): Outcome<T> {
  switch (outcome.status) {
    case "completed":
      return { status: "completed", value: fromWire(outcome.value) as T };
    case "partiallyCompleted":
      return {
        status: "partiallyCompleted",
        committedResult: fromWire(outcome.committedResult) as T,
        failedOutcome: outcome.failedOutcome,
      };
    default:
      return outcome;
  }
}
