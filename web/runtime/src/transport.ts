/**
 * The transport that turns one HTTP reply into one outcome.
 *
 * The rule it follows is `classify()` in `crates/client/tui/src/submission.rs`,
 * which the terminal uses. Both read the same case table, at
 * `crates/client/tui/tests/data/classification-cases.json`.
 *
 * It classifies the shape of the reply and trusts the platform for the values
 * inside it. The platform enforces its own contract, so the browser does not
 * repeat that work: it holds no JSON Schema validator and no copy of the wire
 * spelling rules. One difference follows, and it is deliberate: a reply whose
 * value violates its own field contract reads as completed here and as
 * uncertain in the terminal.
 */

import { z } from "zod";

import { encodeReadQuery } from "./readQuery.js";
import { createReadStore, type ReadReply } from "./readCache.js";
import type {
  ErrorCase,
  FieldMap,
  JsonValue,
  Outcome,
  ResponseContract,
  Transport,
  WireRequest,
} from "./wire.js";

/** One HTTP reply, as the transport reads it. */
export interface HttpReply {
  /** The status the release returned. */
  readonly status: number;
  /** The body, as text. */
  readonly body: string;
}

/**
 * What one application supplies to reach its own deployment.
 *
 * A request carries its session one way. A bearer caller holds the token and
 * the transport sends it in the authorization header. A cookie caller holds
 * nothing: the browser sends the session cookie, and the transport copies the
 * CSRF token from its readable cookie into a header. The two options cannot
 * be given together.
 */
export type TransportOptions = BearerOptions | CookieOptions;

/** The options that every carrier shares. */
interface CommonOptions {
  /** Where the release is served. The generated route supplies the path. */
  readonly baseUrl: string;
  /** The fetch to call. The global one is the default. */
  readonly fetch?: typeof globalThis.fetch;
}

/** A caller that holds its token, such as a page with a personal access token. */
export interface BearerOptions extends CommonOptions {
  /** The credential to send, when the application holds one. */
  readonly credential?: string | undefined;
  readonly cookie?: undefined;
}

/** A page that the identity service signed in by cookie. */
export interface CookieOptions extends CommonOptions {
  /** The browser carries the session in its cookie. */
  readonly cookie: true;
  readonly credential?: undefined;
  /** The cookie string to read the CSRF token from. `document.cookie` is the default. */
  readonly cookies?: () => string;
}

/** The readable cookie that holds the CSRF token of the cookie session. */
const CSRF_COOKIE = "__Host-wamn-csrf";

/** The header that carries the CSRF token back to the router. */
const CSRF_HEADER = "x-wamn-csrf";

/** The value of one cookie in a `document.cookie` string, or null. */
function cookieValue(cookies: string, name: string): string | null {
  for (const pair of cookies.split(";")) {
    const at = pair.indexOf("=");
    if (at !== -1 && pair.slice(0, at).trim() === name) {
      return pair.slice(at + 1).trim();
    }
  }
  return null;
}

/** One submitted item, as far as the transport reads it. */
const ITEM = z.looseObject({
  request_id: z.string().optional(),
  value: z.unknown().optional(),
  error: z.looseObject({ code: z.string().optional() }).optional(),
});

/** The complete envelope: exactly one outcome for one submitted item. */
const ONE_OUTCOME = z.array(ITEM).length(1);

/** The partial completion envelope. */
const PARTIAL = z.looseObject({
  committed_result: z.array(ITEM).optional(),
  failed_outcome: z.unknown().optional(),
});

/** The refusal envelope that ingress returns before any item runs. */
const ERROR_ENVELOPE = z.looseObject({
  error: z.looseObject({
    code: z.string(),
    data: z.unknown().optional(),
    operation: z.string().optional(),
    detail: z.unknown().optional(),
  }),
});

/** The complete refusals that ingress states with a code and nothing else. */
const COMPLETE_REFUSALS: ReadonlyArray<readonly [number, string]> = [
  [400, "schema-invalid"],
  [400, "malformed-json"],
  [413, "mapped-payload-too-large"],
  [429, "route-capacity-exhausted"],
];

/** The plain-text body that ingress returns when a request is too large. */
const BODY_LIMIT = /^request body exceeds (\d+)-byte limit\n$/;

function uncertain(reason: string, retryRefusal: JsonValue | null = null): Outcome<JsonValue> {
  return { status: "uncertain", reason, retryRefusal };
}

function refused(code: string | null, detail: JsonValue): Outcome<JsonValue> {
  return { status: "refused", code, detail };
}

/**
 * Everything one refusal states besides its code, or null when it states
 * nothing else. The terminal carries the whole error object, and this carries
 * the same members beside the code.
 */
function withoutCode(error: { [key: string]: unknown }): JsonValue {
  const rest: { [key: string]: JsonValue } = {};
  for (const [name, member] of Object.entries(error)) {
    if (name !== "code") {
      rest[name] = member as JsonValue;
    }
  }
  return Object.keys(rest).length === 0 ? null : rest;
}

/**
 * The reason, with the refusal literal the server reported, when it names one.
 *
 * `unknown_with_literal` in `submission.rs` states the same sentence, so an
 * operator reads one diagnostic in both clients.
 */
function reported(reason: string, document: unknown): Outcome<JsonValue> {
  const items = z.array(z.unknown()).length(1).safeParse(document);
  const outcome = items.success ? items.data[0] : document;
  const parsed = ERROR_ENVELOPE.safeParse(outcome);
  if (!parsed.success) {
    return uncertain(reason);
  }
  const literal = parsed.data.error.code;
  let diagnostic = `${reason}; the server reported ${literal}`;
  if (literal === "concurrency_conflict") {
    const revision = (name: string): number | null => {
      const detail = parsed.data.error.detail;
      if (detail === null || typeof detail !== "object") {
        return null;
      }
      const value = (detail as { [key: string]: unknown })[name];
      if (typeof value === "number" && Number.isInteger(value)) {
        return value;
      }
      if (typeof value === "string" && /^-?\d+$/.test(value)) {
        return Number(value);
      }
      return null;
    };
    const expected = revision("expected_row_version");
    const observed = revision("observed_row_version");
    if (expected !== null && observed !== null) {
      diagnostic += ` (expected_row_version=${expected}, observed_row_version=${observed})`;
    }
  }
  return uncertain(diagnostic);
}

/**
 * Turn one reply into one outcome of the submitted intent.
 *
 * `requestId` is the identity the caller wrote into the item it sent. The
 * release echoes it, and a reply that does not carry it back establishes
 * nothing about this submission. It is `null` for a read, which carries no
 * request identity: its one outcome matches its one item by position, and an
 * outcome that carries an identity establishes nothing.
 */
export function classify(
  contract: ResponseContract,
  requestId: string | null,
  reply: HttpReply,
): Outcome<JsonValue> {
  if (reply.status === 401 && contract.direct) {
    return refused("unauthenticated", null);
  }
  if (reply.status === 413 && BODY_LIMIT.test(reply.body)) {
    return refused(null, reply.body);
  }
  let document: unknown;
  try {
    document = JSON.parse(reply.body);
  } catch {
    return uncertain("the response is not valid JSON");
  }

  const partial = PARTIAL.safeParse(document);
  if (
    partial.success &&
    (partial.data.committed_result !== undefined || partial.data.failed_outcome !== undefined)
  ) {
    if (reply.status < 400 || reply.status >= 600) {
      return uncertain("partial completion requires an HTTP error response");
    }
    return classifyPartial(contract, requestId, partial.data);
  }

  const envelope = ERROR_ENVELOPE.safeParse(document);
  if (envelope.success) {
    const { code, operation } = envelope.data.error;
    const bare =
      Object.keys(envelope.data).length === 1 && Object.keys(envelope.data.error).length === 1;
    if (bare && COMPLETE_REFUSALS.some(([status, literal]) => reply.status === status && literal === code)) {
      return refused(code, null);
    }
    if (reply.status === 400 && code === "schema-invalid" && isPointerOnly(envelope.data.error)) {
      return refused(code, withoutCode(envelope.data.error));
    }
    if (reply.status === 403 && contract.direct) {
      if (code === "permission-denied" && operation !== undefined && operation !== "") {
        return refused(code, withoutCode(envelope.data.error));
      }
      return reported("the authorization response is malformed", document);
    }
  } else if (reply.status === 403 && contract.direct) {
    return reported("the authorization response is malformed", document);
  }

  if (reply.status !== 200) {
    return reported("the response does not establish completion", document);
  }
  const outcomes = ONE_OUTCOME.safeParse(document);
  if (!outcomes.success) {
    return reported("the response must contain exactly one outcome", document);
  }
  const item = outcomes.data[0];
  if (item === undefined || !matchesRequest(requestId, item.request_id)) {
    return reported("the response does not match the submitted request", document);
  }
  if (item.value !== undefined && item.error === undefined) {
    const violation = envelopeViolation(contract, item.value);
    if (violation !== null) {
      return reported(violation, document);
    }
    return { status: "completed", value: item.value as JsonValue };
  }
  if (item.value === undefined && item.error !== undefined && confirmed(contract, item.error)) {
    const code = item.error.code ?? null;
    const detail = withoutCode(item.error);
    // A declared refusal carries the text its operation declares for the code.
    const text = contract.errors.find((case_) => case_.literal === code)?.text ?? null;
    return text === null ? refused(code, detail) : { status: "refused", code, detail, text };
  }
  // Neither member, both members, and an undeclared refusal establish no
  // completion fact. A value beside an error is not a partial contract.
  return reported("the response does not establish the submission outcome", document);
}

/**
 * Turn one reply to a call that carries many items into one outcome for each.
 *
 * The release runs each item on its own. A 200 reply with one outcome for each
 * item classifies each item by the rule of one item, so a refusal marks only
 * its own item. Any other reply gives every item the same outcome, because it
 * names no item: an ingress refusal refuses every item, and a partial
 * completion or a failure establishes nothing for any of them.
 * `requestIds` holds the identity each item carries, in the order sent.
 */
export function classifyEach(
  contract: ResponseContract,
  requestIds: readonly (string | null)[],
  reply: HttpReply,
): Outcome<JsonValue>[] {
  const every = (outcome: Outcome<JsonValue>) => requestIds.map(() => outcome);
  let document: unknown;
  try {
    document = JSON.parse(reply.body);
  } catch {
    document = undefined;
  }
  const partial = PARTIAL.safeParse(document);
  if (
    partial.success &&
    (partial.data.committed_result !== undefined || partial.data.failed_outcome !== undefined)
  ) {
    return every(uncertain("a partial completion does not name the item it belongs to"));
  }
  if (reply.status !== 200 || document === undefined) {
    return every(classify(contract, null, reply));
  }
  if (!Array.isArray(document) || document.length !== requestIds.length) {
    return every(uncertain("the response must contain one outcome for each submitted item"));
  }
  return document.map((item, index) =>
    classify(contract, requestIds[index] ?? null, {
      status: 200,
      body: JSON.stringify([item]),
    }),
  );
}

/**
 * Why the completed value does not carry what its result class states, or null.
 *
 * `validate_value` in `submission.rs` states the same shapes. This reads the
 * envelope and trusts the platform for the values inside each row.
 */
function envelopeViolation(contract: ResponseContract, value: unknown): string | null {
  const object = value !== null && typeof value === "object" ? (value as { [key: string]: unknown }) : null;
  if (contract.resultClass === "one") {
    return object === null || Array.isArray(object) ? "the result violates its field contract" : null;
  }
  if (contract.resultClass !== "bounded_list" && contract.resultClass !== "page") {
    return null;
  }
  const key = contract.resultClass === "page" ? "item" : "rows";
  if (object === null || !Array.isArray(object[key])) {
    return "the result has no declared row collection";
  }
  if (key === "item") {
    const cursor = object["next_cursor"];
    const valid = cursor === null || typeof cursor === "string";
    if (!("next_cursor" in object) || !valid) {
      return "the page has no valid next_cursor";
    }
  }
  return null;
}

/**
 * Whether one refusal is the operation's own, declared outcome.
 *
 * `refusal_is_confirmed` in `submission.rs` states the rule. A refusal that
 * this does not confirm establishes no fact about the submission, so the
 * caller reads an uncertainty instead. The transaction test is the reason:
 * only a transactional operation guarantees that a refusal changed nothing.
 */
function confirmed(contract: ResponseContract, error: { code?: string | undefined }): boolean {
  if (!contract.direct || error.code === undefined) {
    return false;
  }
  const code = error.code;
  const declared: ErrorCase | undefined = contract.errors.find((case_) => case_.literal === code);
  if (declared === undefined) {
    return false;
  }
  const detail = (error as { detail?: unknown }).detail;
  const member = (name: string): unknown =>
    detail !== null && typeof detail === "object"
      ? (detail as { [key: string]: unknown })[name]
      : undefined;
  const present = declared.required.every((name) => {
    const value = member(name);
    if (name === "expected_row_version" || name === "observed_row_version") {
      return (
        (typeof value === "number" && Number.isInteger(value)) ||
        (typeof value === "string" && /^-?\d+$/.test(value))
      );
    }
    return typeof value === "string" && value !== "";
  });
  if (!present) {
    return false;
  }
  if (contract.transaction !== "implicit" && contract.transaction !== "explicit_per_input") {
    return false;
  }
  if (declared.sources.length === 0) {
    return (
      ["get", "query", "create", "update", "delete"].includes(contract.kind) &&
      ["invalid_input", "not_found", "concurrency_conflict", "idempotency_conflict"].includes(code)
    );
  }
  return declared.sources.every((source) => KNOWN_SOURCES.includes(source));
}

/** The declared origins that a client accepts as a confirmed refusal. */
const KNOWN_SOURCES: readonly string[] = [
  "malformed_input",
  "envelope_count",
  "line_count",
  "duplicate_line",
  "nonpositive_quantity",
  "transaction_invariant",
  "same_key_different_canonical_command",
  "unique_violation",
  "foreign_key_violation",
  "check_violation",
  "exclusion_violation",
  "not_null_violation",
  "permission_denied",
];

/** Whether the refusal states a schema pointer and nothing else. */
function isPointerOnly(error: { [key: string]: unknown }): boolean {
  const keys = Object.keys(error);
  if (keys.length !== 2 || !keys.includes("code") || !keys.includes("data")) {
    return false;
  }
  const data = error["data"];
  if (data === null || typeof data !== "object") {
    return false;
  }
  const inner = data as { [key: string]: unknown };
  return Object.keys(inner).length === 1 && typeof inner["pointer"] === "string";
}

function classifyPartial(
  contract: ResponseContract,
  requestId: string | null,
  document: {
    committed_result?: readonly unknown[] | undefined;
    failed_outcome?: unknown;
  },
): Outcome<JsonValue> {
  if (contract.partialSchema === null) {
    return uncertain("the route declares no partial completion contract");
  }
  const committed = document.committed_result;
  if (committed === undefined || committed.length !== 1) {
    return uncertain("the committed result must contain exactly one outcome");
  }
  const item = ITEM.safeParse(committed[0]);
  if (!item.success || !matchesRequest(requestId, item.data.request_id)) {
    return uncertain("the committed result does not match the submitted request");
  }
  if (
    item.data.value === undefined ||
    item.data.error !== undefined ||
    document.failed_outcome === undefined
  ) {
    return uncertain("the response does not establish partial completion");
  }
  return {
    status: "partiallyCompleted",
    committedResult: item.data.value as JsonValue,
    failedOutcome: document.failed_outcome as JsonValue,
  };
}

/**
 * The transport that generated bindings call.
 *
 * It owns the URL, the credential and the envelope. The bindings state what to
 * send, and this states where and how.
 */
export function createTransport(options: TransportOptions): Transport {
  const call = options.fetch ?? globalThis.fetch;
  // The store belongs to this transport and to one session, so one caller's
  // reads never answer another's.
  const reads = createReadStore();
  const writeListeners = new Set<() => void>();
  const send = async (target: string, init: RequestInit): Promise<ReadReply> => {
    const response = await call(`${options.baseUrl}${target}`, init);
    return {
      status: response.status,
      body: await response.text(),
      cacheControl: response.headers.get("cache-control"),
      etag: response.headers.get("etag"),
    };
  };
  /** Put the session or the credential on one request. */
  const credentials = (headers: { [name: string]: string }, init: RequestInit, read: boolean) => {
    if (options.cookie === true) {
      // The CSRF cookie is read on every request, because a renewal replaces
      // it. Without it the header stays off, and the router decides. A read
      // needs no CSRF header, and a request with one is never cached.
      // A new session or a renewal sets a new cookie, and empties the store.
      const csrf = cookieValue((options.cookies ?? (() => document.cookie))(), CSRF_COOKIE);
      reads.belongTo(csrf);
      if (csrf !== null && !read) {
        headers[CSRF_HEADER] = csrf;
      }
      init.credentials = "include";
    } else if (options.credential !== undefined) {
      headers["authorization"] = `Bearer ${options.credential}`;
    }
  };
  /** Send one request, and return its reply or the uncertainty of a failed send. */
  const exchange = async (request: WireRequest): Promise<HttpReply | Outcome<JsonValue>> => {
    const read = request.method === "GET";
    const headers: { [name: string]: string } = {};
    const init: RequestInit = { method: request.method, headers };
    let target = request.template;
    if (read) {
      // A read carries its one item in the query string, and has no body.
      const [item, ...rest] = request.items;
      if (rest.length > 0 || item === null || typeof item !== "object" || Array.isArray(item)) {
        return uncertain("a read sends exactly one request item");
      }
      const query = encodeReadQuery(item as { readonly [name: string]: JsonValue });
      target = query === "" ? target : `${target}?${query}`;
    } else {
      headers["content-type"] = "application/json";
      init.body = JSON.stringify(request.items);
    }
    credentials(headers, init, read);
    let reply: HttpReply;
    try {
      if (read) {
        // A read the store holds sends the store's tag with cache
        // "no-store", so its 304 reaches the store. A read the store does
        // not hold, as after a reload, uses "no-cache": the browser
        // revalidates its own copy, and a 304 saves the body.
        const stored = (outcome: Outcome<JsonValue>) =>
          outcome.status === "completed" || outcome.status === "refused";
        reply = await reads.read(
          `${request.operation} ${target}`,
          (tag) =>
            send(
              target,
              tag === null
                ? { ...init, cache: "no-cache" }
                : { ...init, cache: "no-store", headers: { ...headers, "if-none-match": tag } },
            ),
          (candidate) => stored(classify(request.contract, null, candidate)),
        );
      } else {
        reply = await send(target, init);
      }
    } catch (error) {
      return uncertain(`the request did not complete: ${String(error)}`);
    } finally {
      // Any write can change what a stored read returns, so every stored
      // read revalidates next (wamn-fjdo narrows this to the write's models),
      // and each listener then reads what its page shows again.
      if (!read) {
        reads.invalidate();
        for (const listener of [...writeListeners]) {
          listener();
        }
      }
    }
    return reply;
  };
  return {
    async invoke(request: WireRequest): Promise<Outcome<JsonValue>> {
      const requestId = request.method === "GET" ? null : submittedRequestId(request);
      const reply = await exchange(request);
      return isReply(reply) ? classify(request.contract, requestId, reply) : reply;
    },
    async invokeEach(request: WireRequest): Promise<readonly Outcome<JsonValue>[]> {
      if (request.method === "GET") {
        return request.items.map(() => uncertain("a read sends exactly one request item"));
      }
      const requestIds = request.items.map(itemRequestId);
      const reply = await exchange(request);
      return isReply(reply)
        ? classifyEach(request.contract, requestIds, reply)
        : requestIds.map(() => reply);
    },
    onWrite(listener: () => void): () => void {
      writeListeners.add(listener);
      return () => writeListeners.delete(listener);
    },
    async openStream(
      request: WireRequest,
      signal?: AbortSignal,
    ): Promise<ReadableStream<Uint8Array> | Outcome<JsonValue>> {
      const [item, ...rest] = request.items;
      if (rest.length > 0 || item === null || typeof item !== "object" || Array.isArray(item)) {
        return uncertain("a read sends exactly one request item");
      }
      const query = encodeReadQuery({
        ...(item as { readonly [name: string]: JsonValue }),
        shape: "stream",
      });
      const target = `${request.template}?${query}`;
      const headers: { [name: string]: string } = {};
      // The browser revalidates its own copy with the load's ETag, and a 304
      // saves the body. The read store holds no load: a load holds its rows.
      const init: RequestInit = { method: "GET", headers, cache: "no-cache" };
      if (signal !== undefined) {
        init.signal = signal;
      }
      credentials(headers, init, true);
      let response: Response;
      try {
        response = await call(`${options.baseUrl}${target}`, init);
      } catch (error) {
        return uncertain(`the request did not complete: ${String(error)}`);
      }
      const type = response.headers.get("content-type") ?? "";
      if (response.status === 200 && type.startsWith(STREAM_CONTENT_TYPE) && response.body !== null) {
        return response.body;
      }
      // A read that ended before its first row answers as a page answers.
      let body: string;
      try {
        body = await response.text();
      } catch (error) {
        return uncertain(`the response did not complete: ${String(error)}`);
      }
      return classify(request.contract, null, { status: response.status, body });
    },
  };
}

/** The content type of a streamed read: one JSON value per line. */
const STREAM_CONTENT_TYPE = "application/x-ndjson";

/**
 * Calls `listener` after each write on `transport`, and returns the function
 * that stops it. A transport without `onWrite` never calls it.
 */
export function afterWrites(transport: Transport, listener: () => void): () => void {
  return transport.onWrite?.(listener) ?? (() => undefined);
}

/** Whether an exchange returned a reply, rather than the outcome of a failed send. */
function isReply(result: HttpReply | Outcome<JsonValue>): result is HttpReply {
  return typeof result.status === "number";
}

/** Whether an echoed identity matches the submitted one, or is absent for a read. */
function matchesRequest(requestId: string | null, echoed: string | undefined): boolean {
  return requestId === null ? echoed === undefined : requestId !== "" && echoed === requestId;
}

/**
 * The request identity the caller wrote into the item it is sending.
 *
 * The release echoes it, and the classifier matches it. A submission that
 * carries no request identity matches nothing, which reads as uncertainty.
 */
function submittedRequestId(request: WireRequest): string {
  return itemRequestId(request.items[0]);
}

/** The request identity of one submitted item, or "" when it carries none. */
function itemRequestId(item: JsonValue | undefined): string {
  if (item === undefined || item === null || typeof item !== "object" || Array.isArray(item)) {
    return "";
  }
  const value = (item as { [key: string]: JsonValue })["request_id"];
  return typeof value === "string" ? value : "";
}

/**
 * The declared path that one refusal names, or null.
 *
 * A refusal states its code, and its detail carries what else the operation
 * declared. A case that requires a `field` member names the path directly, in
 * the contract spelling, such as `value.line[].quantity`. A schema refusal
 * names it through a pointer, such as `/0/value/line/2/quantity`, which carries
 * the index of the item it refused. Both forms come back as one declared path,
 * and `refusalMarks` decides which control that path names.
 *
 * The detail of a typed refusal can nest one level, because the error object
 * carries its own `detail` member beside the code, so the reader descends once.
 */
export function refusedMember(detail: JsonValue): string | null {
  const members = object(detail);
  if (members === null) {
    return null;
  }
  const field = members["field"];
  if (typeof field === "string" && field !== "") {
    return field;
  }
  const pointer = members["pointer"] ?? object(members["data"])?.["pointer"];
  if (typeof pointer === "string" && pointer !== "") {
    return declaredPath(pointer);
  }
  const nested = object(members["detail"]);
  return nested === null ? null : refusedMember(nested);
}

/** One JSON object, or null for every other value. */
function object(value: JsonValue | undefined): { [key: string]: JsonValue } | null {
  return value === undefined || value === null || typeof value !== "object" || Array.isArray(value)
    ? null
    : (value as { [key: string]: JsonValue });
}

/**
 * The declared path that one JSON pointer names.
 *
 * The first segment is the item of the envelope, which one submission always
 * fills with a single item, so it drops. A segment of digits is the index of
 * the element before it, and it stays, so the form marks one line.
 */
function declaredPath(pointer: string): string | null {
  const segments = pointer.split("/").filter((segment) => segment !== "");
  if (segments.length > 0 && /^\d+$/.test(segments[0] ?? "")) {
    segments.shift();
  }
  let path = "";
  for (const segment of segments) {
    if (/^\d+$/.test(segment)) {
      path += `[${segment}]`;
    } else {
      path = path === "" ? segment : `${path}.${segment}`;
    }
  }
  return path === "" ? null : path;
}

/**
 * The member path that a checked input refuses, in the contract's spelling.
 *
 * A schema names its path in TypeScript member spelling, with a number for
 * each element of a repeated group. A control states the path the contract
 * declares, which keeps the wire spelling, so the field map turns one into
 * the other. `refusalMarks` then reads the answer exactly as it reads a
 * refusal that the release sent.
 */
export function checkedMember(
  path: readonly (string | number)[] | undefined,
  fields: FieldMap,
): string | null {
  if (path === undefined || path.length === 0) {
    return null;
  }
  let member = "";
  let level: FieldMap | null = fields;
  for (const segment of path) {
    if (typeof segment === "number") {
      // The field map spells a repeated group's key with `[]`, and the index
      // fills those brackets rather than adding a second pair.
      member = member.endsWith("[]")
        ? `${member.slice(0, -2)}[${segment}]`
        : `${member}[${segment}]`;
      continue;
    }
    const entries: [string, FieldMap[string]][] = Object.entries(level ?? {});
    const found = entries.find(([, value]) =>
      typeof value === "string" ? value === segment : value.member === segment,
    );
    if (found === undefined) {
      return null;
    }
    const wireKey: string = found[0];
    const value: FieldMap[string] = found[1];
    level = typeof value === "string" ? null : value.fields;
    member = member === "" ? wireKey : `${member}.${wireKey}`;
  }
  return member === "" ? null : member;
}

/**
 * Does the path that a refusal names reach this control?
 *
 * A control states the path the contract declares, such as
 * `value.line[].quantity`. A refusal that names an index marks the control of
 * that element alone. A refusal that names none marks the control of every
 * element, because the operation refused the group without saying which line.
 */
export function refusalMarks(member: string | null, declared: string, index?: number): boolean {
  if (member === null) {
    return false;
  }
  const refused = member.replace(/\[\d+\]/g, "[]");
  if (refused !== declared) {
    return false;
  }
  const named = /\[(\d+)\]/.exec(member);
  return named === null || index === undefined || Number(named[1]) === index;
}
