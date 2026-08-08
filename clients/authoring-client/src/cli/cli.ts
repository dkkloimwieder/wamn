// The headless authoring CLI (`wamn`), wamn-ftfc.14.
//
// WHAT THIS IS. A checkout client. It reads working-tree flow definitions and
// drives the public versioned authoring commands over HTTP through the
// wamn-jvzx.2 generated client: `validate` (save the working tree, then validate
// the exact revision it saved), `draft-run`, `suite-run`, `promote` (the public
// `publish` command), and `runs` (the public `suite-projection` command). It
// composes nothing the contract does not define and invents no route.
//
// WHAT THIS IS NOT. It has no in-process handler, no database URL, no operator
// recovery path, and no frontend. Its whole capability surface is the injected
// `CliIo` port below: one POST-only fetch, read/write of caller-named files, and
// a `git` reader for its own checkout's provenance. There is no environment
// read, no process spawn beyond that reader, and no second transport — so the
// absence of those shortcuts is structural rather than a promise.
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
  type AuthoringRequest,
  type AuthoringScope,
  type CommandRefusal,
  type CommitProvenance,
  type DraftSuiteProjection,
  type SuiteProjectionState,
} from "../generated/authoring.js";

/// The two routes the wamn-ctc8.8 management surface reserves. The contract
/// defines no route, so a client supplies both; nothing else is ever reached.
const LOGIN_PATH = "/login";
const AUTHORING_PATH = "/authoring";
const LOGIN_LABEL = "wamn-cli";

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
  /// Last-modified time of one caller-named file, in epoch milliseconds. This
  /// is the "edit" instant of the edit-to-run measurement.
  readonly modifiedAt: (path: string) => Promise<number>;
  /// Read one caller-named JSON file, or `undefined` when it does not exist.
  readonly readJson: (path: string) => Promise<unknown>;
  /// Write one caller-named JSON file, creating parent directories.
  readonly writeJson: (path: string, value: unknown) => Promise<void>;
  /// Run one read-only `git` query in the client's own checkout. Typing the
  /// port as `git` and nothing else is what makes "no other program runs"
  /// structural: there is no call shape that could name another executable.
  readonly git: (args: ReadonlyArray<string>, cwd: string) => string | undefined;
  /// The single machine-readable document.
  readonly out: (line: string) => void;
  /// The human transcript.
  readonly err: (line: string) => void;
}

export type StepStatus = "completed" | "refused" | "unmounted" | "fault";

export type FaultKind = "network" | "http" | "protocol";

export interface StepRecord {
  readonly command: AuthoringCommandKind;
  readonly "command-id": string;
  readonly status: StepStatus;
  readonly "elapsed-ms": number;
  readonly result?: unknown;
  readonly refusal?: CommandRefusal;
  readonly "http-status"?: number;
  readonly fault?: { readonly kind: FaultKind; readonly detail: string };
}

export interface CliDocument {
  readonly client: "wamn";
  readonly "schema-version": typeof AUTHORING_SCHEMA_VERSION;
  readonly verb: string;
  readonly status: StepStatus;
  readonly steps: ReadonlyArray<StepRecord>;
  readonly "edit-to-run-ms": number | null;
  readonly "server-edit-to-run-ms"?: number | null;
  readonly "elapsed-ms": number;
}

/// Client-local cache of the PUBLIC identities the loop hands back, so the next
/// verb can consume them without a human copying an id. It holds no credential
/// and no privileged datum; every field here also arrives on stdout.
export interface CliState {
  readonly "state-version": 1;
  readonly "draft-id"?: string;
  readonly "flow-id"?: string;
  readonly revision?: number;
  readonly "edit-at"?: number;
  readonly "validated-draft-id"?: string;
  readonly "suite-id"?: string;
  readonly "flow-version"?: number;
  readonly "report-id"?: string;
  readonly "execution-id"?: string;
  readonly "run-id"?: string;
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

export interface SaveOptions {
  readonly commandId: string;
  readonly scope: AuthoringScope;
  readonly draftId: string;
  readonly flowId: string;
  readonly expectedRevision: number;
  readonly definition: string;
  readonly provenance?: CommitProvenance;
}

export function saveFlowDraftRequest(options: SaveOptions): AuthoringRequest {
  const input = {
    definition: options.definition,
    "draft-id": options.draftId,
    "expected-revision": options.expectedRevision,
    "flow-id": options.flowId,
    scope: options.scope,
    ...(options.provenance === undefined ? {} : { provenance: options.provenance }),
  };
  return request(options.commandId, { kind: "save-flow-draft", input });
}

export interface ValidateOptions {
  readonly commandId: string;
  readonly scope: AuthoringScope;
  readonly draftId: string;
  readonly revision: number;
  readonly suiteId: string;
  readonly flowVersion: number;
}

export function validateRequest(options: ValidateOptions): AuthoringRequest {
  return request(options.commandId, {
    kind: "validate",
    input: {
      draft: { "draft-id": options.draftId, revision: options.revision },
      scope: options.scope,
      suite: { "flow-version": options.flowVersion, "suite-id": options.suiteId },
    },
  });
}

export interface DraftRunOptions {
  readonly commandId: string;
  readonly scope: AuthoringScope;
  readonly validatedDraftId: string;
  readonly input: unknown;
}

export function draftRunRequest(options: DraftRunOptions): AuthoringRequest {
  return request(options.commandId, {
    kind: "draft-run",
    input: {
      input: options.input,
      scope: options.scope,
      "validated-draft": { "validated-draft-id": options.validatedDraftId },
    },
  });
}

export interface SuiteRunOptions {
  readonly commandId: string;
  readonly scope: AuthoringScope;
  readonly validatedDraftId: string;
  readonly suiteId: string;
  readonly flowVersion: number;
}

export function suiteRunRequest(options: SuiteRunOptions): AuthoringRequest {
  return request(options.commandId, {
    kind: "suite-run",
    input: {
      scope: options.scope,
      suite: { "flow-version": options.flowVersion, "suite-id": options.suiteId },
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

export interface RunsOptions {
  readonly commandId: string;
  readonly scope: AuthoringScope;
  readonly reportId: string;
}

/// `runs` reads the durable report through the public `suite-projection`
/// command. There is no generic runs route on the contract (wamn-jvzx.13 owns
/// one if it ever lands), and this client does not invent one.
export function runsRequest(options: RunsOptions): AuthoringRequest {
  return request(options.commandId, {
    kind: "suite-projection",
    input: { "report-id": options.reportId, scope: options.scope },
  });
}

// ---------------------------------------------------------------------------
// Argument parsing
// ---------------------------------------------------------------------------

const FLAGS = new Set(["--help", "--no-state", "--no-provenance"]);

const OPTIONS = new Set([
  "--base-url",
  "--credential",
  "--token-file",
  "--project",
  "--environment",
  "--command-id",
  "--state",
  "--file",
  "--draft-id",
  "--flow-id",
  "--expected-revision",
  "--suite-id",
  "--flow-version",
  "--validated-draft",
  "--input",
  "--report-id",
]);

export const VERBS = ["validate", "draft-run", "suite-run", "promote", "runs"] as const;

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
  validate     save the working-tree definition, then validate the exact saved revision
  draft-run    run one authored input against a validated draft
  suite-run    run a stored suite against a validated draft
  promote      publish a validated draft proven by a successful report
  runs         read the durable suite projection for a report

authentication (exactly one, always from a file so no secret reaches argv):
  --credential FILE   subject=/secret= file, exchanged for a PAT at POST /login
  --token-file FILE   an already-issued personal access token

common options:
  --base-url URL           management surface base URL (required)
  --project ID             project scope (required)
  --environment NAME       environment scope (required)
  --command-id ID          override the generated command id
  --state FILE             client-local identity cache (default ${DEFAULT_STATE_PATH})
  --no-state               neither read nor write the state file

validate:  --file PATH --draft-id ID --flow-id ID --suite-id ID --flow-version N
           [--expected-revision N] [--no-provenance]
draft-run: --input PATH [--validated-draft ID]
suite-run: [--validated-draft ID] [--suite-id ID] [--flow-version N]
promote:   [--validated-draft ID] [--report-id ID]
runs:      [--report-id ID]

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
// Credentials
// ---------------------------------------------------------------------------

function credentialFields(text: string): { subject: string; secret: string } {
  const fields = new Map<string, string>();
  for (const line of text.split("\n")) {
    const separator = line.indexOf("=");
    if (separator > 0) {
      fields.set(line.slice(0, separator).trim(), line.slice(separator + 1).trim());
    }
  }
  const subject = fields.get("subject");
  const secret = fields.get("secret");
  if (subject === undefined || secret === undefined || subject === "" || secret === "") {
    throw new UsageError("a credential file must define subject and secret");
  }
  return { subject, secret };
}

type Credential = { readonly token: string } | { readonly refused: true } | { readonly fault: string };

/// Exchange one principal's own subject and secret for a personal access token
/// on the reserved login route. This is the whole first-party PAT flow: the CLI
/// never mints, stores, or forwards authority it was not handed.
async function login(
  io: CliIo,
  transcript: Transcript,
  baseUrl: string,
  path: string,
): Promise<Credential> {
  const { subject, secret } = credentialFields(await io.readText(path));
  transcript.guard(secret);
  let response;
  try {
    response = await io.fetch(`${baseUrl}${LOGIN_PATH}`, {
      body: JSON.stringify({ label: LOGIN_LABEL, secret, subject }),
      headers: { accept: "application/json", "content-type": "application/json" },
      method: "POST",
    });
  } catch {
    return { fault: "the login route could not be reached" };
  }
  if (response.status === 403) return { refused: true };
  if (!response.ok) return { fault: `login returned HTTP ${response.status}` };
  let issued: unknown;
  try {
    issued = await response.json();
  } catch {
    return { fault: "login did not return JSON" };
  }
  const token = (issued as { token?: unknown }).token;
  if (typeof token !== "string" || token.length === 0) {
    return { fault: "login returned no token" };
  }
  transcript.guard(token);
  transcript.note(`  ok    logged in subject=${subject} token=<redacted>`);
  return { token };
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
// Provenance
// ---------------------------------------------------------------------------

/// The client's own claim about where it read a definition. The platform runs no
/// Git, so this is the only place a commit can come from — and it is attribution
/// only: it selects no principal, widens no role, and changes no result.
export function checkoutProvenance(io: CliIo, directory: string): CommitProvenance | undefined {
  const commit = io.git(["rev-parse", "HEAD"], directory);
  if (commit === undefined || commit.length === 0) return undefined;
  const reference = io.git(["symbolic-ref", "--quiet", "HEAD"], directory);
  const status = io.git(["status", "--porcelain"], directory);
  return {
    commit,
    dirty: status !== undefined && status.length > 0,
    ref: reference === undefined || reference.length === 0 ? null : reference,
  };
}

function directoryOf(path: string): string {
  const separator = path.lastIndexOf("/");
  return separator <= 0 ? "." : path.slice(0, separator);
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
  // A composed verb sends two commands, and the contract's command id is per
  // command, so an override is suffixed rather than reused.
  return ordinal === 0 ? override : `${override}-${ordinal}`;
}

function stateOrRequired(session: Session, option: string, key: keyof CliState): string {
  const supplied = session.parsed.values[option];
  if (supplied !== undefined && supplied.length > 0) return supplied;
  const remembered = session.state[key];
  if (typeof remembered === "string" && remembered.length > 0) return remembered;
  throw new UsageError(`--${option} is required (no ${String(key)} in the state file)`);
}

function stateNumberOrRequired(session: Session, option: string, key: keyof CliState): number {
  const supplied = session.parsed.values[option];
  if (supplied !== undefined) return integer(supplied, option);
  const remembered = session.state[key];
  if (typeof remembered === "number") return remembered;
  throw new UsageError(`--${option} is required (no ${String(key)} in the state file)`);
}

async function runValidate(session: Session): Promise<StepRecord[]> {
  const file = required(session.parsed, "file");
  const draftId = required(session.parsed, "draft-id");
  const flowId = required(session.parsed, "flow-id");
  const suiteId = required(session.parsed, "suite-id");
  const flowVersion = integer(required(session.parsed, "flow-version"), "flow-version");
  const suppliedRevision = session.parsed.values["expected-revision"];
  const expectedRevision =
    suppliedRevision !== undefined
      ? integer(suppliedRevision, "expected-revision")
      : session.state["draft-id"] === draftId && typeof session.state.revision === "number"
        ? session.state.revision
        : 0;

  const definition = await session.io.readText(file);
  const editedAt = await session.io.modifiedAt(file);
  const provenance = session.parsed.flags.has("--no-provenance")
    ? undefined
    : checkoutProvenance(session.io, directoryOf(file));
  session.transcript.note(
    `save   file=${file} draft-id=${draftId} flow-id=${flowId} bytes=${definition.length} ` +
      `expected-revision=${expectedRevision} provenance=${provenance === undefined ? "none" : provenance.commit}`,
  );

  const saved = await execute(
    session.client,
    saveFlowDraftRequest({
      commandId: commandId(session, "save-flow-draft", 0),
      definition,
      draftId,
      expectedRevision,
      flowId,
      provenance,
      scope: session.scope,
    }),
    session.io,
    session.transcript,
  );
  const steps = [saved];
  if (saved.status !== "completed") return steps;

  const revision = (saved.result as { revision: number }).revision;
  await writeState(session.io, session.statePath, session.state, {
    "draft-id": draftId,
    "edit-at": editedAt,
    "flow-id": flowId,
    "flow-version": flowVersion,
    revision,
    "suite-id": suiteId,
  });

  session.transcript.note(`validate draft-id=${draftId} revision=${revision} suite-id=${suiteId}`);
  const validated = await execute(
    session.client,
    validateRequest({
      commandId: commandId(session, "validate", 1),
      draftId,
      flowVersion,
      revision,
      scope: session.scope,
      suiteId,
    }),
    session.io,
    session.transcript,
  );
  steps.push(validated);
  if (validated.status === "completed") {
    const identity = validated.result as { "validated-draft-id": string };
    await writeState(session.io, session.statePath, { ...session.state, revision }, {
      "draft-id": draftId,
      "edit-at": editedAt,
      "flow-id": flowId,
      "flow-version": flowVersion,
      revision,
      "suite-id": suiteId,
      "validated-draft-id": identity["validated-draft-id"],
    });
  }
  return steps;
}

async function runDraftRun(session: Session): Promise<StepRecord[]> {
  const validatedDraftId = stateOrRequired(session, "validated-draft", "validated-draft-id");
  const inputPath = required(session.parsed, "input");
  let input: unknown;
  try {
    input = JSON.parse(await session.io.readText(inputPath));
  } catch (error) {
    throw new UsageError(`--input ${inputPath} is not JSON: ${(error as Error).message}`);
  }
  session.transcript.note(`draft-run validated-draft=${validatedDraftId} input=${inputPath}`);
  const step = await execute(
    session.client,
    draftRunRequest({
      commandId: commandId(session, "draft-run", 0),
      input,
      scope: session.scope,
      validatedDraftId,
    }),
    session.io,
    session.transcript,
  );
  if (step.status === "completed") {
    const receipt = step.result as { "run-id": string };
    await writeState(session.io, session.statePath, session.state, { "run-id": receipt["run-id"] });
  }
  return [step];
}

async function runSuiteRun(session: Session): Promise<StepRecord[]> {
  const validatedDraftId = stateOrRequired(session, "validated-draft", "validated-draft-id");
  const suiteId = stateOrRequired(session, "suite-id", "suite-id");
  const flowVersion = stateNumberOrRequired(session, "flow-version", "flow-version");
  session.transcript.note(
    `suite-run validated-draft=${validatedDraftId} suite-id=${suiteId} flow-version=${flowVersion}`,
  );
  const step = await execute(
    session.client,
    suiteRunRequest({
      commandId: commandId(session, "suite-run", 0),
      flowVersion,
      scope: session.scope,
      suiteId,
      validatedDraftId,
    }),
    session.io,
    session.transcript,
  );
  if (step.status === "completed") {
    const receipt = step.result as { "execution-id": string; "report-id": string };
    await writeState(session.io, session.statePath, session.state, {
      "execution-id": receipt["execution-id"],
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

async function runRuns(session: Session): Promise<StepRecord[]> {
  const reportId = stateOrRequired(session, "report-id", "report-id");
  session.transcript.note(`runs report-id=${reportId}`);
  return [
    await execute(
      session.client,
      runsRequest({ commandId: commandId(session, "suite-projection", 0), reportId, scope: session.scope }),
      session.io,
      session.transcript,
    ),
  ];
}

/// Summarize a finalized projection for a human, keying every row by the
/// contract's stable identities rather than by prose.
function describeProjection(transcript: Transcript, state: SuiteProjectionState): number | null {
  if (state.state === "not-found") {
    transcript.note("  info  report state=not-found");
    return null;
  }
  if (state.state === "pending") {
    transcript.note(`  info  report state=pending reason=${state.report.reason.kind}`);
    return null;
  }
  const report: DraftSuiteProjection = state.report;
  const failed = report.cases.filter((entry) => entry.outcome === "failed").length;
  transcript.note(
    `  info  report state=finalized outcome=${report.outcome.state} cases=${report.cases.length} ` +
      `failed=${failed} nodes=${report.nodes.length} branches=${report.branches.length} edges=${report.edges.length}`,
  );
  for (const entry of report.cases) {
    transcript.note(
      `  case  case-id=${entry["case-id"]} run-id=${entry["run-id"]} outcome=${entry.outcome}` +
        (entry.failure === undefined || entry.failure === null
          ? ""
          : ` failure=${entry.failure.kind} node=${entry.failure["node-id"] ?? "-"}`),
    );
  }
  return report["edit-to-run-ms"] ?? null;
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
    const credentialPath = parsed.values["credential"];
    const tokenPath = parsed.values["token-file"];
    if ((credentialPath === undefined) === (tokenPath === undefined)) {
      throw new UsageError("exactly one of --credential and --token-file is required");
    }

    let token: string;
    if (credentialPath !== undefined) {
      const credential = await login(io, transcript, baseUrl, credentialPath);
      if ("refused" in credential) {
        // Login refused: no command was attempted, so there is no step to
        // report — only the frozen refusal the surface returns for every
        // authentication and authorization failure alike.
        transcript.note(`  ref   login refused ${AUTHORIZATION_DENIED}`);
        transcript.document({
          client: "wamn",
          "edit-to-run-ms": null,
          "elapsed-ms": io.now() - started,
          refusal: { reason: { kind: AUTHORIZATION_DENIED } },
          "schema-version": AUTHORING_SCHEMA_VERSION,
          status: "refused",
          steps: [],
          verb: parsed.verb,
        });
        return EXIT_REFUSED;
      }
      if ("fault" in credential) {
        transcript.note(`  FAIL  ${credential.fault}`);
        transcript.document({
          client: "wamn",
          "edit-to-run-ms": null,
          "elapsed-ms": io.now() - started,
          fault: { detail: credential.fault, kind: "network" },
          "schema-version": AUTHORING_SCHEMA_VERSION,
          status: "fault",
          steps: [],
          verb: parsed.verb,
        });
        return EXIT_FAULT;
      }
      token = credential.token;
    } else {
      token = (await io.readText(tokenPath!)).trim();
      if (token.length === 0) throw new UsageError("--token-file is empty");
      transcript.guard(token);
    }

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
      case "validate":
        steps = await runValidate(session);
        break;
      case "draft-run":
        steps = await runDraftRun(session);
        break;
      case "suite-run":
        steps = await runSuiteRun(session);
        break;
      case "promote":
        steps = await runPromote(session);
        break;
      case "runs":
        steps = await runRuns(session);
        break;
    }

    // Edit-to-run latency, measured where a checkout client can actually
    // measure it: from the modification time of the definition file it
    // submitted to the moment a run receipt came back. `runs` additionally
    // reports the platform's own `edit-to-run-ms` when a report is finalized.
    const producesRun = parsed.verb === "draft-run" || parsed.verb === "suite-run";
    const completedRun = producesRun && steps[0]?.status === "completed";
    const editedAt = state["edit-at"];
    const editToRun =
      completedRun && typeof editedAt === "number" ? io.now() - editedAt : null;
    let serverEditToRun: number | null | undefined;
    if (parsed.verb === "runs" && steps[0]?.status === "completed") {
      serverEditToRun = describeProjection(transcript, steps[0].result as SuiteProjectionState);
    }

    const status = overall(steps);
    const document: CliDocument = {
      client: "wamn",
      "edit-to-run-ms": editToRun,
      "elapsed-ms": io.now() - started,
      "schema-version": AUTHORING_SCHEMA_VERSION,
      status,
      steps,
      verb: parsed.verb,
      ...(serverEditToRun === undefined ? {} : { "server-edit-to-run-ms": serverEditToRun }),
    };
    if (editToRun !== null) transcript.note(`  time  edit-to-run-ms=${editToRun}`);
    if (serverEditToRun !== undefined && serverEditToRun !== null) {
      transcript.note(`  time  server-edit-to-run-ms=${serverEditToRun}`);
    }
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
