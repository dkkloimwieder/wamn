// The headless authoring CLI (`wamn`), wamn-ftfc.14.
//
// WHAT THIS IS. A checkout client. It drives the public versioned authoring
// commands over HTTP through the wamn-jvzx.2 generated client: `test-set-run`
// (the `gate` verb) and `promote` (the public `publish` command), plus the
// `get-report` read. It composes nothing the contract does not define and
// invents no route.
//
// It SAVES NOTHING (wamn-0h0g.8.5.5). A draft is a client-side file — this
// checkout's own working tree — and the wiring document's content hash is its
// identity, so there is no save, validate, read-back or ad-hoc-run verb here.
//
// WHAT THIS IS NOT. It has no in-process handler, no database URL, no operator
// recovery path, and no frontend. Its whole capability surface is the injected
// `CliIo` port below: one POST-only fetch, read/write of caller-named files, and
// read/write of caller-named files. There is no environment read, NO PROCESS
// SPAWN AT ALL, and no second transport — so the absence of those shortcuts is
// structural rather than a promise. The `git` reader left with `save-draft`
// (wamn-0h0g.8.5.5): provenance had exactly one wire carrier, and with that
// command gone nothing can consume a commit claim, so the capability is
// withdrawn rather than left dangling.
//
// Every invocation writes exactly one JSON document to stdout: typed identities,
// typed product refusals, typed transport absence (`501`), or a fault. The human
// transcript goes to stderr, and neither ever carries credential material.

import {
  AuthoringClient,
  AuthoringHttpError,
  AuthoringNetworkError,
  AuthoringProtocolError,
  createFetchTransport,
  type FetchLike,
} from "../client.js";
import {
  AUTHORING_SCHEMA_VERSION,
  type AuthoringCommand,
  type AuthoringCommandKind,
  type AuthoringQuery,
  type AuthoringQueryKind,
  type AuthoringQueryRequest,
  type AuthoringRequest,
  type AuthoringScope,
  type CommandRefusal,
  type QueryRefusal,
} from "../generated/authoring.js";

/// The route the management surface reserves. The contract defines no route,
/// so a client supplies it; nothing else is ever reached.
const AUTHORING_PATH = "/authoring";

/// The frozen refusal every authentication and authorization failure returns.
/// A pre-dispatch refusal carries no response envelope, and the generated
/// transport keeps no non-2xx body, so the CLI reports this exact contract kind
/// against the command that was refused.
const AUTHORIZATION_DENIED = "authorization-denied" as const;

/// Exit codes. A typed product refusal, an unmounted command, and an
/// infrastructure fault are three different answers and never share a code.
export const EXIT_COMPLETED = 0;
export const EXIT_USAGE = 2;
export const EXIT_REFUSED = 3;
export const EXIT_UNMOUNTED = 4;
export const EXIT_FAULT = 5;

const DEFAULT_STATE_PATH = ".wamn/state.json";

/// Everything this CLI can do to the world. A capability it is not handed here
/// is a capability it does not have: there is no environment access, no generic
/// process spawn, and no second network client.
export interface CliIo {
  /// Milliseconds since the epoch, for latency and command identity.
  readonly now: () => number;
  /// POST-only HTTP, the same shape the generated fetch transport consumes.
  readonly fetch: FetchLike;
  /// Read one caller-named file as UTF-8 text.
  readonly readText: (path: string) => Promise<string>;
  /// Read one caller-named JSON file, or `undefined` when it does not exist.
  readonly readJson: (path: string) => Promise<unknown>;
  /// Write one caller-named JSON file, creating parent directories.
  readonly writeJson: (path: string, value: unknown) => Promise<void>;
  /// The single machine-readable document.
  readonly out: (line: string) => void;
  /// The human transcript.
  readonly err: (line: string) => void;
}

export type StepStatus = "completed" | "refused" | "unmounted" | "fault";

export type FaultKind = "network" | "http" | "protocol";

export interface CommandStepRecord {
  readonly command: AuthoringCommandKind;
  readonly "command-id": string;
  readonly status: StepStatus;
  readonly "elapsed-ms": number;
  readonly result?: unknown;
  readonly refusal?: CommandRefusal;
  readonly "http-status"?: number;
  readonly fault?: { readonly kind: FaultKind; readonly detail: string };
}

export interface QueryStepRecord {
  readonly query: AuthoringQueryKind;
  readonly "query-id": string;
  readonly status: StepStatus;
  readonly "elapsed-ms": number;
  readonly result?: unknown;
  readonly refusal?: QueryRefusal;
  readonly "http-status"?: number;
  readonly fault?: { readonly kind: FaultKind; readonly detail: string };
}

export type StepRecord = CommandStepRecord | QueryStepRecord;

export interface CliDocument {
  readonly client: "wamn";
  readonly "schema-version": typeof AUTHORING_SCHEMA_VERSION;
  readonly verb: string;
  readonly status: StepStatus;
  readonly steps: ReadonlyArray<StepRecord>;
  readonly "elapsed-ms": number;
}

/// Client-local cache of the PUBLIC identities the loop hands back, so the next
/// verb can consume them without a human copying an id. It holds no credential
/// and no privileged datum; every field here also arrives on stdout.
export interface CliState {
  readonly "state-version": 1;
  readonly "validated-draft-id"?: string;
  readonly "report-id"?: string;
}

class UsageError extends Error {}

// ---------------------------------------------------------------------------
// Request construction. These are the whole request surface, exported so the
// drift gate can compare each built document with the checked-in collection.
// ---------------------------------------------------------------------------

function request(commandId: string, command: AuthoringCommand): AuthoringRequest {
  // The contract version is this constant and nothing else: no flag, file, or
  // environment value can select another one, so an unversioned or
  // differently-versioned request has no path through this client.
  return { "command-id": commandId, "schema-version": AUTHORING_SCHEMA_VERSION, command };
}

function queryRequest(queryId: string, query: AuthoringQuery): AuthoringQueryRequest {
  return { "query-id": queryId, "schema-version": AUTHORING_SCHEMA_VERSION, query };
}

export interface TestSetRunOptions {
  readonly commandId: string;
  readonly scope: AuthoringScope;
  readonly validatedDraftId: string;
}

export function testSetRunRequest(options: TestSetRunOptions): AuthoringRequest {
  return request(options.commandId, {
    kind: "test-set-run",
    input: {
      scope: options.scope,
      "validated-draft": { "validated-draft-id": options.validatedDraftId },
    },
  });
}

export interface PromoteOptions {
  readonly commandId: string;
  readonly scope: AuthoringScope;
  readonly validatedDraftId: string;
  readonly reportId: string;
}

/// `promote` is the CLI word for the public `publish` command; the contract has
/// no second spelling and this client sends no other kind.
export function promoteRequest(options: PromoteOptions): AuthoringRequest {
  return request(options.commandId, {
    kind: "publish",
    input: {
      scope: options.scope,
      "successful-report-id": options.reportId,
      "validated-draft": { "validated-draft-id": options.validatedDraftId },
    },
  });
}

export function getReportRequest(
  queryId: string,
  scope: AuthoringScope,
  reportId: string,
): AuthoringQueryRequest {
  return queryRequest(queryId, {
    kind: "get-report",
    input: { "report-id": reportId, scope },
  });
}

// ---------------------------------------------------------------------------
// Argument parsing
// ---------------------------------------------------------------------------

const FLAGS = new Set(["--help", "--no-state"]);

const OPTIONS = new Set([
  "--base-url",
  "--token-file",
  "--project",
  "--environment",
  "--command-id",
  "--query-id",
  "--state",
  "--validated-draft",
  "--report-id",
]);

export const VERBS = ["test-set-run", "promote", "get-report"] as const;

export type Verb = (typeof VERBS)[number];

export interface ParsedArguments {
  readonly verb: Verb | undefined;
  readonly help: boolean;
  readonly values: Readonly<Record<string, string>>;
  readonly flags: ReadonlySet<string>;
}

export function parseArguments(argv: ReadonlyArray<string>): ParsedArguments {
  let verb: Verb | undefined;
  let help = false;
  const values: Record<string, string> = {};
  const flags = new Set<string>();
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index]!;
    if (FLAGS.has(argument)) {
      if (argument === "--help") help = true;
      else flags.add(argument);
      continue;
    }
    if (OPTIONS.has(argument)) {
      const value = argv[index + 1];
      if (value === undefined) throw new UsageError(`${argument} needs a value`);
      values[argument.slice(2)] = value;
      index += 1;
      continue;
    }
    if (argument.startsWith("-")) throw new UsageError(`unrecognized option ${argument}`);
    if (verb !== undefined) throw new UsageError(`unexpected argument ${argument}`);
    const candidate = VERBS.find((known) => known === argument);
    if (candidate === undefined) throw new UsageError(`unknown command ${argument}`);
    verb = candidate;
  }
  return { verb, help, values, flags };
}

function required(parsed: ParsedArguments, name: string): string {
  const value = parsed.values[name];
  if (value === undefined || value.length === 0) throw new UsageError(`--${name} is required`);
  return value;
}

function integer(text: string, name: string): number {
  const value = Number(text);
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new UsageError(`--${name} must be an exactly representable nonnegative integer`);
  }
  return value;
}

export const USAGE = `usage: wamn <command> [options]

commands:
  test-set-run gate a candidate wiring against its own cases
  promote      publish a candidate proven by a successful report
  get-report   read one gate report projection

authentication (always from a file so no token reaches argv):
  --token-file FILE   an already-issued personal access token (required)

common options:
  --base-url URL           management surface base URL (required)
  --project ID             project scope (required)
  --environment NAME       environment scope (required)
  --command-id ID          override the generated command id
  --query-id ID            override the generated query correlation id
  --state FILE             client-local identity cache (default ${DEFAULT_STATE_PATH})
  --no-state               neither read nor write the state file

test-set-run: [--validated-draft ID]
promote:   [--validated-draft ID] [--report-id ID]
get-report: [--report-id ID]

stdout carries exactly one JSON document; exit 0 completed, 3 refused,
4 unmounted (the surface answers 501), 5 fault, 2 usage.`;

// ---------------------------------------------------------------------------
// Transcript with fail-closed redaction
// ---------------------------------------------------------------------------

class Transcript {
  readonly #io: CliIo;
  readonly #secrets = new Set<string>();
  #leaked = false;

  constructor(io: CliIo) {
    this.#io = io;
  }

  /// Register credential material so no line can ever print it.
  guard(secret: string): void {
    if (secret.length > 0) this.#secrets.add(secret);
  }

  get leaked(): boolean {
    return this.#leaked;
  }

  #safe(line: string): string {
    for (const secret of this.#secrets) {
      if (line.includes(secret)) {
        this.#leaked = true;
        return "<<REDACTED — credential material reached the transcript>>";
      }
    }
    return line;
  }

  note(line: string): void {
    this.#io.err(this.#safe(line));
  }

  document(value: unknown): void {
    this.#io.out(this.#safe(JSON.stringify(value, null, 2)));
  }
}

// ---------------------------------------------------------------------------
// One command
// ---------------------------------------------------------------------------

function kindOf(document: AuthoringRequest): AuthoringCommandKind {
  return document.command.kind;
}

async function execute(
  client: AuthoringClient,
  document: AuthoringRequest,
  io: CliIo,
  transcript: Transcript,
): Promise<StepRecord> {
  const command = kindOf(document);
  const started = io.now();
  const base = { command, "command-id": document["command-id"] };
  const elapsed = () => io.now() - started;
  try {
    const outcome = await client.execute(document);
    if (outcome.status === "completed") {
      transcript.note(`  ok    ${command} completed`);
      return { ...base, status: "completed", "elapsed-ms": elapsed(), result: outcome.value.result };
    }
    transcript.note(`  ref   ${command} refused ${outcome.value.reason.kind}`);
    return { ...base, status: "refused", "elapsed-ms": elapsed(), refusal: outcome.value };
  } catch (error) {
    if (error instanceof AuthoringHttpError) {
      if (error.status === 501) {
        // The absence of a route, not a product refusal: the surface mounts the
        // command kinds whose handlers have landed and answers 501 with no
        // document for the rest. Reporting it as its own status is what keeps a
        // missing handler from ever reading as a success or as a refusal.
        transcript.note(`  501   ${command} is not mounted on this surface`);
        return { ...base, status: "unmounted", "elapsed-ms": elapsed(), "http-status": 501 };
      }
      if (error.status === 403) {
        transcript.note(`  ref   ${command} refused ${AUTHORIZATION_DENIED}`);
        return {
          ...base,
          status: "refused",
          "elapsed-ms": elapsed(),
          "http-status": 403,
          refusal: { command, reason: { kind: AUTHORIZATION_DENIED } },
        };
      }
      return {
        ...base,
        status: "fault",
        "elapsed-ms": elapsed(),
        "http-status": error.status,
        fault: { kind: "http", detail: `authoring endpoint returned HTTP ${error.status}` },
      };
    }
    if (error instanceof AuthoringNetworkError) {
      return {
        ...base,
        status: "fault",
        "elapsed-ms": elapsed(),
        fault: { kind: "network", detail: "no HTTP response was received" },
      };
    }
    if (error instanceof AuthoringProtocolError) {
      return {
        ...base,
        status: "fault",
        "elapsed-ms": elapsed(),
        fault: { kind: "protocol", detail: error.message },
      };
    }
    throw error;
  }
}

async function executeQuery(
  client: AuthoringClient,
  document: AuthoringQueryRequest,
  io: CliIo,
  transcript: Transcript,
): Promise<QueryStepRecord> {
  const query = document.query.kind;
  const started = io.now();
  const base = { query, "query-id": document["query-id"] };
  const elapsed = () => io.now() - started;
  try {
    const outcome = await client.query(document);
    if (outcome.status === "completed") {
      transcript.note(`  ok    ${query} completed`);
      return { ...base, status: "completed", "elapsed-ms": elapsed(), result: outcome.value.result };
    }
    transcript.note(`  ref   ${query} refused ${outcome.value.reason.kind}`);
    return { ...base, status: "refused", "elapsed-ms": elapsed(), refusal: outcome.value };
  } catch (error) {
    if (error instanceof AuthoringHttpError) {
      if (error.status === 501) {
        transcript.note(`  501   ${query} is not mounted on this surface`);
        return { ...base, status: "unmounted", "elapsed-ms": elapsed(), "http-status": 501 };
      }
      if (error.status === 403) {
        transcript.note(`  ref   ${query} refused ${AUTHORIZATION_DENIED}`);
        return {
          ...base,
          status: "refused",
          "elapsed-ms": elapsed(),
          "http-status": 403,
          refusal: { query, reason: { kind: AUTHORIZATION_DENIED } } as QueryRefusal,
        };
      }
      return {
        ...base,
        status: "fault",
        "elapsed-ms": elapsed(),
        "http-status": error.status,
        fault: { kind: "http", detail: `authoring endpoint returned HTTP ${error.status}` },
      };
    }
    if (error instanceof AuthoringNetworkError) {
      return {
        ...base,
        status: "fault",
        "elapsed-ms": elapsed(),
        fault: { kind: "network", detail: "no HTTP response was received" },
      };
    }
    if (error instanceof AuthoringProtocolError) {
      return {
        ...base,
        status: "fault",
        "elapsed-ms": elapsed(),
        fault: { kind: "protocol", detail: error.message },
      };
    }
    throw error;
  }
}

const PRECEDENCE: Readonly<Record<StepStatus, number>> = {
  completed: 0,
  refused: 1,
  unmounted: 2,
  fault: 3,
};

function overall(steps: ReadonlyArray<StepRecord>): StepStatus {
  let worst: StepStatus = "completed";
  for (const step of steps) {
    if (PRECEDENCE[step.status] > PRECEDENCE[worst]) worst = step.status;
  }
  return worst;
}

function exitCode(status: StepStatus): number {
  switch (status) {
    case "completed":
      return EXIT_COMPLETED;
    case "refused":
      return EXIT_REFUSED;
    case "unmounted":
      return EXIT_UNMOUNTED;
    case "fault":
      return EXIT_FAULT;
  }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

async function readState(io: CliIo, path: string | undefined): Promise<CliState> {
  if (path === undefined) return { "state-version": 1 };
  const stored = await io.readJson(path);
  if (stored === undefined || stored === null || typeof stored !== "object") {
    return { "state-version": 1 };
  }
  return { ...(stored as CliState), "state-version": 1 };
}

async function writeState(
  io: CliIo,
  path: string | undefined,
  previous: CliState,
  update: Partial<CliState>,
): Promise<void> {
  if (path === undefined) return;
  await io.writeJson(path, { ...previous, ...update, "state-version": 1 });
}

// ---------------------------------------------------------------------------
// Verbs
// ---------------------------------------------------------------------------

interface Session {
  readonly io: CliIo;
  readonly transcript: Transcript;
  readonly client: AuthoringClient;
  readonly scope: AuthoringScope;
  readonly parsed: ParsedArguments;
  readonly statePath: string | undefined;
  readonly state: CliState;
  readonly started: number;
}

function commandId(session: Session, kind: AuthoringCommandKind, ordinal: number): string {
  const override = session.parsed.values["command-id"];
  if (override === undefined) return `${kind}-${session.started.toString(36)}-${ordinal}`;
  return ordinal === 0 ? override : `${override}-${ordinal}`;
}

function queryId(session: Session, kind: AuthoringQueryKind): string {
  return session.parsed.values["query-id"] ?? `${kind}-${session.started.toString(36)}`;
}

function stateOrRequired(session: Session, option: string, key: keyof CliState): string {
  const supplied = session.parsed.values[option];
  if (supplied !== undefined && supplied.length > 0) return supplied;
  const remembered = session.state[key];
  if (typeof remembered === "string" && remembered.length > 0) return remembered;
  throw new UsageError(`--${option} is required (no ${String(key)} in the state file)`);
}

async function runTestSetRun(session: Session): Promise<StepRecord[]> {
  const validatedDraftId = stateOrRequired(session, "validated-draft", "validated-draft-id");
  session.transcript.note(`test-set-run validated-draft=${validatedDraftId}`);
  const step = await execute(
    session.client,
    testSetRunRequest({
      commandId: commandId(session, "test-set-run", 0),
      scope: session.scope,
      validatedDraftId,
    }),
    session.io,
    session.transcript,
  );
  if (step.status === "completed") {
    const receipt = step.result as { "report-id": string };
    await writeState(session.io, session.statePath, session.state, {
      "report-id": receipt["report-id"],
    });
  }
  return [step];
}

async function runPromote(session: Session): Promise<StepRecord[]> {
  const validatedDraftId = stateOrRequired(session, "validated-draft", "validated-draft-id");
  const reportId = stateOrRequired(session, "report-id", "report-id");
  session.transcript.note(`promote validated-draft=${validatedDraftId} report-id=${reportId}`);
  return [
    await execute(
      session.client,
      promoteRequest({
        commandId: commandId(session, "publish", 0),
        reportId,
        scope: session.scope,
        validatedDraftId,
      }),
      session.io,
      session.transcript,
    ),
  ];
}

async function runGetReport(session: Session): Promise<StepRecord[]> {
  const reportId = stateOrRequired(session, "report-id", "report-id");
  return [
    await executeQuery(
      session.client,
      getReportRequest(queryId(session, "get-report"), session.scope, reportId),
      session.io,
      session.transcript,
    ),
  ];
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

export async function runCli(argv: ReadonlyArray<string>, io: CliIo): Promise<number> {
  const transcript = new Transcript(io);
  const started = io.now();
  try {
    const parsed = parseArguments(argv);
    if (parsed.help || parsed.verb === undefined) {
      io.err(USAGE);
      return parsed.help ? EXIT_COMPLETED : EXIT_USAGE;
    }
    const baseUrl = required(parsed, "base-url").replace(/\/+$/, "");
    const scope: AuthoringScope = {
      environment: required(parsed, "environment"),
      "project-id": required(parsed, "project"),
    };
    const tokenPath = required(parsed, "token-file");
    const token = (await io.readText(tokenPath)).trim();
    if (token.length === 0) throw new UsageError("--token-file is empty");
    transcript.guard(token);

    const statePath = parsed.flags.has("--no-state")
      ? undefined
      : (parsed.values["state"] ?? DEFAULT_STATE_PATH);
    const state = await readState(io, statePath);
    const client = new AuthoringClient(
      createFetchTransport({
        endpoint: `${baseUrl}${AUTHORING_PATH}`,
        fetch: io.fetch,
        headers: { authorization: `Bearer ${token}` },
      }),
    );
    const session: Session = {
      client,
      io,
      parsed,
      scope,
      started,
      state,
      statePath,
      transcript,
    };

    let steps: StepRecord[];
    switch (parsed.verb) {
      case "test-set-run":
        steps = await runTestSetRun(session);
        break;
      case "promote":
        steps = await runPromote(session);
        break;
      case "get-report":
        steps = await runGetReport(session);
        break;
    }

    const status = overall(steps);
    const document: CliDocument = {
      client: "wamn",
      "elapsed-ms": io.now() - started,
      "schema-version": AUTHORING_SCHEMA_VERSION,
      status,
      steps,
      verb: parsed.verb,
    };
    transcript.document(document);
    transcript.note(`${status.toUpperCase()} verb=${parsed.verb}`);
    if (transcript.leaked) {
      io.err("FAILED check=no-credential-material-in-output");
      return EXIT_FAULT;
    }
    return exitCode(status);
  } catch (error) {
    if (error instanceof UsageError) {
      io.err(`usage error: ${error.message}`);
      io.err(USAGE);
      return EXIT_USAGE;
    }
    // A fault the client could not classify: report it as a fault, never as a
    // refusal, and never as a completed command.
    const detail = error instanceof Error ? error.message : String(error);
    io.err(`fault: ${detail}`);
    transcript.document({
      client: "wamn",
      "elapsed-ms": io.now() - started,
      fault: { detail, kind: "protocol" },
      "schema-version": AUTHORING_SCHEMA_VERSION,
      status: "fault",
      steps: [],
    });
    return EXIT_FAULT;
  }
}
