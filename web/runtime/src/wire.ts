/**
 * The request and result types shared by browser transports and client bindings.
 *
 * The OpenAPI documents define the wire field names used by generated clients.
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
export type JsonValue = null | boolean | number | string | readonly JsonValue[] | { readonly [key: string]: JsonValue };

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
  /** The operation type, for example `get`, `create` or `command`. */
  readonly type: string;
  /** The declared transaction boundary, or null when the release states none. */
  readonly transaction: string | null;
  /** The relations a read selects from, as `schema.table`. Empty for a write. */
  readonly reads: readonly string[];
  /**
   * The relations a write changes, as `schema.table`, or null when the
   * contract names none. Null for a read.
   */
  readonly writes: readonly string[] | null;
}

/** One refusal the operation declares. */
export interface ErrorCase {
  /** The wire literal a caller branches on. */
  readonly literal: string;
  /** Detail members that are always present. */
  readonly required: readonly string[];
  /** Declared origins of this refusal. */
  readonly sources: readonly string[];
  /** Authored text a screen shows in place of the code's words, or null. */
  readonly text: string | null;
}

/** One request. It carries no host, no base URL and no credential. */
export interface WireRequest {
  /** The exact canonical operation identity. */
  readonly operation: string;
  /** The method the release publishes. */
  readonly method: string;
  /** The path template the release publishes. */
  readonly template: string;
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
      /** The text the operation declares for the code, when it declares one. */
      readonly text?: string;
    }
  | {
      readonly status: "uncertain";
      readonly reason: string;
      readonly retryRefusal: JsonValue | null;
    };

/** Everything one operation sends except its items. */
export type OperationRoute = Omit<WireRequest, "items">;

/** Cancellation for one transport call. */
export interface InvokeOptions {
  readonly signal?: AbortSignal;
}

/**
 * The transport an application supplies.
 *
 * It owns the URL, the credential, the request envelope and the classification
 * of the response into one `Outcome`. The bindings only state what to send.
 */
export interface Transport {
  invoke(request: WireRequest, options?: InvokeOptions): Promise<Outcome<JsonValue>>;
  /** Whether the application origin answers its health probe within three seconds. */
  probe?(signal?: AbortSignal): Promise<boolean>;
  /**
   * Sends one write that carries many items, each with its own request
   * identity, and returns one outcome for each item, in order. The release
   * runs each item on its own. A transport without it sends one item a call.
   */
  invokeEach?(request: WireRequest, options?: InvokeOptions): Promise<readonly Outcome<JsonValue>[]>;
  /**
   * Calls `listener` with the write's `writes` after each write settles,
   * whatever its outcome, and returns the function that stops it. A page
   * reads its shown reads again this way. A transport without it tells
   * nobody about a write.
   */
  onWrite?(listener: (writes: readonly string[] | null) => void): () => void;
}

/** Give the transport outcome the result type declared by the operation. */
export function reviveOutcome<T>(outcome: Outcome<JsonValue>): Outcome<T> {
  return outcome as Outcome<T>;
}

/** The route a client binding uses to call one operation. */
export interface OperationBinding {
  readonly route: OperationRoute;
}

/** Calls one operation through its binding, as its generated function does. */
export async function callOperation<T>(
  transport: Transport,
  binding: OperationBinding,
  items: readonly JsonValue[],
  options?: InvokeOptions,
): Promise<Outcome<T>> {
  return reviveOutcome<T>(await transport.invoke({ ...binding.route, items }, options));
}

/**
 * Calls one operation with many outer inputs in one request, which the
 * release runs one by one, and returns one outcome for each input in order.
 */
export async function callEach<T>(
  transport: Transport,
  binding: OperationBinding,
  items: readonly JsonValue[],
  options?: InvokeOptions,
): Promise<readonly Outcome<T>[]> {
  if (transport.invokeEach === undefined) {
    throw new Error("this transport sends no call with many outer inputs");
  }
  const outcomes = await transport.invokeEach({ ...binding.route, items }, options);
  return outcomes.map(reviveOutcome<T>);
}
