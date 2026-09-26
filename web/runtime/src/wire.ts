/**
 * The wire contract that every generated binding imports.
 *
 * It declares the type aliases, the field map that carries the member names,
 * the request envelope, the four outcomes of one intent, and the transport
 * interface that an application supplies. It holds no name rule: the generator
 * decides every member name and emits a field map beside each operation.
 *
 * This file is hand-written. `crates/schema/generator/src/client_ts.rs` emits
 * the modules that import it, and neither one repeats the other.
 */

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
  /** Every refusal the operation declares. */
  readonly errors: readonly ErrorCase[];
  /** The replay guarantee the release serves. */
  readonly replay: "claim" | "state" | null;
  /** Whether the release serves this operation itself, without a handler. */
  readonly direct: boolean;
  /** The operation kind, for example `get`, `create` or `command`. */
  readonly kind: string;
  /** The declared transaction boundary, or null when the release states none. */
  readonly transaction: string | null;
}

/** One refusal the operation declares. */
export interface ErrorCase {
  /** The wire literal a caller branches on. */
  readonly literal: string;
  /** Detail members that are always present. */
  readonly required: readonly string[];
  /** Declared origins of this refusal. */
  readonly sources: readonly string[];
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
  | {
      readonly status: "refused";
      readonly code: string | null;
      readonly detail: JsonValue;
    }
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
  /**
   * Sends one write that carries many items, each with its own request
   * identity, and returns one outcome for each item, in order. The release
   * runs each item on its own. A transport without it sends one item a call.
   */
  invokeEach?(request: WireRequest): Promise<readonly Outcome<JsonValue>[]>;
  /**
   * Calls `listener` after each write settles, whatever its outcome, and
   * returns the function that stops it. A page reads its shown reads again
   * this way. A transport without it tells nobody about a write.
   */
  onWrite?(listener: () => void): () => void;
}

/**
 * What one declared shape calls its members, on the wire and in TypeScript.
 *
 * The key is the wire key. A string value is the member name. An object value
 * is a declared object or array, and its `fields` describe each value inside
 * it. A key that no map declares keeps its spelling, so the inside of a `json`
 * value is never renamed.
 */
export type FieldMap = { readonly [wireKey: string]: string | NestedFields };

/** One declared object or array, and the members inside it. */
export interface NestedFields {
  /** The TypeScript member name. */
  readonly member: string;
  /** What each value inside carries. */
  readonly fields: FieldMap;
}

/** The renamed key, and the map to walk with, or null to copy the value. */
type Rename = (fields: FieldMap, name: string) => readonly [string, FieldMap | null];

function walk(value: unknown, fields: FieldMap, rename: Rename): JsonValue {
  if (Array.isArray(value)) {
    return value.map((item) => walk(item, fields, rename));
  }
  if (value !== null && typeof value === "object") {
    const converted: { [name: string]: JsonValue } = {};
    for (const [name, member] of Object.entries(value)) {
      const [renamed, nested] = rename(fields, name);
      converted[renamed] =
        nested === null ? (member as JsonValue) : walk(member, nested, rename);
    }
    return converted;
  }
  return value as JsonValue;
}

/** Convert the wire keys of one declared shape to TypeScript members. */
export function fromWire(value: unknown, fields: FieldMap): JsonValue {
  return walk(value, fields, (map, name) => {
    const entry = map[name];
    if (entry === undefined) {
      return [name, null];
    }
    return typeof entry === "string" ? [entry, null] : [entry.member, entry.fields];
  });
}

/** Convert the TypeScript members of one declared shape to wire keys. */
export function toWire(value: unknown, fields: FieldMap): JsonValue {
  return walk(value, fields, (map, member) => {
    for (const [name, entry] of Object.entries(map)) {
      if (typeof entry === "string") {
        if (entry === member) {
          return [name, null];
        }
      } else if (entry.member === member) {
        return [name, entry.fields];
      }
    }
    return [member, null];
  });
}

/**
 * Rename the keys inside one outcome and state its result type.
 *
 * The cast is unchecked, exactly as the Rust client returns a JSON value that
 * the caller reads through its declared result type. The transport already
 * held the response to the operation's contract.
 *
 * A refusal detail and a failed outcome keep their wire spelling. Neither one
 * declares a field tree, so nothing states which of their keys is a name.
 */
export function reviveOutcome<T>(outcome: Outcome<JsonValue>, fields: FieldMap): Outcome<T> {
  switch (outcome.status) {
    case "completed":
      return { status: "completed", value: fromWire(outcome.value, fields) as T };
    case "partiallyCompleted":
      return {
        status: "partiallyCompleted",
        committedResult: fromWire(outcome.committedResult, fields) as T,
        failedOutcome: outcome.failedOutcome,
      };
    default:
      return outcome;
  }
}
