// The composed edit-to-publish cycle gate for the headless CLI (wamn-ftfc.14).
//
// WHAT THIS IS. The whole authoring loop driven from a real checkout by the
// shipped `wamn` CLI and nothing else: edit a flow file, `validate` (save the
// exact bytes, then validate the exact saved revision), edit it again,
// `draft-run`, `test-set-run`, `promote`. Every leg is a subprocess
// invocation of scripts/wamn.mjs whose stdout document is read as the result, so
// the gate proves the CLI's public behaviour rather than its internals.
//
// WHAT THIS IS NOT. Pure HTTP, like the wamn-jvzx.4 smoke: the caller supplies a
// base URL and ONE pre-issued PAT file and nothing else. No database URL, no
// platform-admin impersonation, no test-only trusted context, and no ledger
// read — that read needs storage authority a client must not hold. The gate
// closes by printing one VERIFY-MANIFEST line naming what a runner-side ledger
// read must find; the `[6A / wamn-ftfc.14]` section of docs/archive/build-and-test.md
// owns that step.
//
// HONEST 501s. The management surface mounts the command kinds whose handlers
// have landed and answers a bare `501` for the rest. A step that answers 501 is
// recorded as `unmounted` — never as a success and never as a refusal — and the
// gate prints exactly which cycle steps are unmounted on the surface it ran
// against. It passes today and keeps passing as kinds mount, because each step
// asserts the CONTRACT shape of whatever answer it gets.
//
//   node scripts/cycle.mjs --check
//   node scripts/cycle.mjs --base-url http://HOST:PORT \
//     --token-file /path/to/pat --project receiving --environment dev
//
// The PAT is read from a mode-600 file so no token byte reaches a command line,
// an environment block, or this gate's output.

import { spawnSync } from "node:child_process";
import { createHash, randomBytes } from "node:crypto";
import {
  closeSync,
  mkdirSync,
  mkdtempSync,
  openSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

import { PACKAGE_ROOT, compiledPackage } from "./compile.mjs";

const LAUNCHER = join(PACKAGE_ROOT, "scripts", "wamn.mjs");

const AUTHORING_PATH = "/authoring";
const REFUSAL_BODY = '{"kind":"authorization-denied"}';
const PAT_PATTERN = /^(wamn_pat_[0-9a-f]{16}_)([0-9a-f]{64})$/;

const FLOW_ID = "receive-material";
const DEFINITION = `{"schema-version":"0.1","flow-id":"${FLOW_ID}","version":1,"nodes":[{"id":"request","type":"request","config":{"input-schema":true}},{"id":"respond","type":"respond","config":{"status":200}}],"edges":[{"from":"request","to":"respond"}]}`;
const AUTHORED_INPUT = '{"receipt-id":"receipt-1042","material":"aluminum"}';

// A validated draft and report the retained commands cannot always own yet.
// When `validate` is unmounted there is no draft identity to carry forward;
// replacement report orchestration is not part of this contract cut. The
// downstream legs use contract-shaped placeholders purely to reach transport.
const PLACEHOLDER_VALIDATED_DRAFT = "sha256:validated-draft-not-yet-issued";
const PLACEHOLDER_REPORT = "report-not-yet-reserved";

/// The required keys of every completed result on the contract. A mounted
/// command that answers with a different identity fails here rather than being
/// accepted because it returned 200.
const RESULT_KEYS = {
  "save-flow-draft": ["draft-id", "flow-id", "revision"],
  validate: [
    "artifact-hash",
    "catalog",
    "draft",
    "environment",
    "runtime-flow-version",
    "validated-draft-id",
  ],
  "draft-run": ["run-id", "validated-draft"],
  "test-set-run": ["report-id", "validated-draft"],
  publish: ["artifact-hash", "flow-id", "version"],
};

class CheckFailure extends Error {
  constructor(check, detail) {
    super(`${check}: ${detail}`);
    this.check = check;
    this.detail = detail;
  }
}

const secrets = new Set();
let leaked = false;

function emit(line) {
  let text = String(line);
  for (const secret of secrets) {
    if (secret.length > 0 && text.includes(secret)) {
      leaked = true;
      text = "<<REDACTED — credential material reached the transcript>>";
      break;
    }
  }
  process.stdout.write(`${text}\n`);
}

function require_(check, condition, detail) {
  if (!condition) throw new CheckFailure(check, detail);
  emit(`  ok    ${check}`);
}

/// This gate's ENTIRE input surface. A storage URL, a platform credential, or a
/// trusted-context switch has no spelling here, which is why `--check` can assert
/// the surface itself rather than scanning for forbidden words.
const NAMED_ARGUMENTS = ["--base-url", "--token-file", "--project", "--environment", "--checkout"];

function parseArguments(argv) {
  const options = { check: false };
  const named = new Set(NAMED_ARGUMENTS);
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--check") options.check = true;
    else if (named.has(argument)) options[argument.slice(2)] = argv[(index += 1)];
    else throw new CheckFailure("usage", `unrecognized argument ${argument}`);
  }
  return options;
}

function launchWamn(args, env = process.env) {
  const captureDirectory = mkdtempSync(join(tmpdir(), "wamn-authoring-cycle-cli-"));
  const stdoutPath = join(captureDirectory, "stdout");
  const stderrPath = join(captureDirectory, "stderr");
  const stdoutFd = openSync(stdoutPath, "w");
  const stderrFd = openSync(stderrPath, "w");
  try {
    const launched = spawnSync(process.execPath, [LAUNCHER, ...args], {
      env,
      stdio: ["ignore", stdoutFd, stderrFd],
    });
    return {
      ...launched,
      stderr: readFileSync(stderrPath, "utf8"),
      stdout: readFileSync(stdoutPath, "utf8"),
    };
  } finally {
    closeSync(stdoutFd);
    closeSync(stderrFd);
    rmSync(captureDirectory, { force: true, recursive: true });
  }
}

/// Run one `wamn` verb and read its single stdout document.
function wamn(verb, args) {
  const launched = launchWamn([verb, ...args]);
  for (const line of launched.stderr.split("\n")) if (line.length > 0) emit(`    | ${line}`);
  let document;
  if (launched.stdout.length > 0) {
    try {
      document = JSON.parse(launched.stdout);
    } catch (error) {
      throw new CheckFailure(`cli-${verb}-document`, `stdout is not one JSON document: ${error.message}`);
    }
  }
  return { code: launched.status, document };
}

async function post(url, { body, token }) {
  const headers = { accept: "application/json", "content-type": "application/json" };
  if (token !== undefined) headers.authorization = `Bearer ${token}`;
  const response = await fetch(url, { method: "POST", headers, body });
  return { status: response.status, text: await response.text() };
}

/// A structurally valid token whose secret half is wrong by one hex digit, so it
/// exercises digest verification rather than parse rejection.
function forge(token) {
  const [, lookup, secret] = PAT_PATTERN.exec(token);
  const forged = `${lookup}${secret[0] === "0" ? "1" : "0"}${secret.slice(1)}`;
  secrets.add(forged);
  return forged;
}

function git(args, cwd) {
  const result = spawnSync("git", args, { cwd, encoding: "utf8" });
  if (result.status !== 0) {
    throw new CheckFailure("checkout", `git ${args.join(" ")} failed: ${result.stderr.trim()}`);
  }
  return result.stdout.trim();
}

/// The step's answer, checked against the contract before it is recorded.
function record(steps, leg, verb, answer, expectedCommands) {
  const document = answer.document;
  if (document === undefined) {
    throw new CheckFailure(`cycle-${leg}`, `the CLI emitted no document (exit ${answer.code})`);
  }
  const commands = document.steps.map((step) => step.command);
  if (JSON.stringify(commands) !== JSON.stringify(expectedCommands)) {
    throw new CheckFailure(
      `cycle-${leg}-commands`,
      `expected ${JSON.stringify(expectedCommands)}, got ${JSON.stringify(commands)}`,
    );
  }
  for (const step of document.steps) {
    if (step.status === "fault") {
      throw new CheckFailure(`cycle-${leg}-fault`, `${step.command}: ${JSON.stringify(step.fault)}`);
    }
    if (step.status === "unmounted") {
      if (step["http-status"] !== 501 || step.result !== undefined || step.refusal !== undefined) {
        throw new CheckFailure(
          `cycle-${leg}-unmounted`,
          `${step.command} claimed to be unmounted without a bare 501`,
        );
      }
    }
    if (step.status === "completed") {
      const missing = RESULT_KEYS[step.command].filter(
        (key) => !Object.hasOwn(step.result ?? {}, key),
      );
      if (missing.length > 0) {
        throw new CheckFailure(
          `cycle-${leg}-identity`,
          `${step.command} completed without ${missing.join(", ")}`,
        );
      }
    }
    if (step.status === "refused" && step.refusal?.reason?.kind === undefined) {
      throw new CheckFailure(`cycle-${leg}-refusal`, `${step.command} refused with no typed reason`);
    }
    steps.push({ leg, verb, ...step });
  }
  return document;
}

function stepOf(document, command) {
  return document.steps.find((step) => step.command === command);
}

async function staticHalf() {
  emit("static checks only (--check); no request is sent");
  const compiled = await compiledPackage();
  const help = launchWamn(["--help"]);
  require_("cli-compiles", help.status === 0, `wamn --help exited ${help.status}`);
  for (const verb of [
    "validate",
    "draft-run",
    "test-set-run",
    "promote",
    "read-draft",
    "get-report",
  ]) {
    require_(`cli-verb-${verb}`, help.stderr.includes(`  ${verb} `), `--help does not document ${verb}`);
  }
  // The cycle covers the whole public command inventory: every kind in the
  // generated schema is reached by a documented verb. That schema is read from
  // the module that ships rather than from a second copy on disk.
  const { authoringSchema: schema } = await import(
    pathToFileURL(join(compiled, "index.js")).href
  );
  const covered = [
    "save-flow-draft",
    "validate",
    "draft-run",
    "test-set-run",
    "publish",
  ];
  const commandKinds = schema.definitions.AuthoringCommand.oneOf.map(
    (variant) => variant.properties.kind.enum[0],
  );
  require_(
    "cycle-covers-the-command-inventory",
    JSON.stringify([...covered].sort()) ===
      JSON.stringify([...commandKinds].sort()),
    "the cycle does not reach every public command kind",
  );
  // This gate's own input surface: a base URL, one token file, a scope, and
  // a checkout. There is no storage URL, platform credential, or trusted-context
  // argument to supply, so a run cannot smuggle one in.
  require_(
    "gate-input-surface",
    NAMED_ARGUMENTS.join(" ") === "--base-url --token-file --project --environment --checkout",
    `the gate grew an argument: ${NAMED_ARGUMENTS.join(" ")}`,
  );
  for (const rejected of ["--database-url", "--system-url", "--pg-url", "--token", "--tenant"]) {
    let refused = false;
    try {
      parseArguments([rejected, "x"]);
    } catch {
      refused = true;
    }
    require_(`gate-rejects-${rejected.slice(2)}`, refused, `${rejected} was accepted`);
  }
}

async function main() {
  const options = parseArguments(process.argv.slice(2));
  if (options.check) {
    await staticHalf();
    return;
  }
  if (!options["base-url"] || !options["token-file"] || !options.project || !options.environment) {
    throw new CheckFailure(
      "usage",
      "a live run needs --base-url, --token-file, --project, and --environment",
    );
  }

  const baseUrl = options["base-url"].replace(/\/+$/, "");
  const token = (await readFile(options["token-file"], "utf8")).trim();
  require_("token-file", PAT_PATTERN.test(token), "needs one well-formed personal access token");
  secrets.add(token);
  secrets.add(PAT_PATTERN.exec(token)[2]);
  const runId = `${Date.now().toString(36)}-${randomBytes(2).toString("hex")}`;
  const draftId = `draft-${options.project}-cycle-${runId}`;
  const checkout = options.checkout ?? (await mkdtemp(join(tmpdir(), `wamn-ftfc14-cycle-${runId}-`)));
  const state = join(checkout, ".wamn", "state.json");
  const definitionPath = join(checkout, "flows", `${FLOW_ID}.flow.json`);
  const inputPath = join(checkout, "input.json");

  emit(`surface ${baseUrl}`);
  emit(`checkout ${checkout}`);
  emit(`run-id=${runId} draft-id=${draftId} flow-id=${FLOW_ID}`);

  // ---- a real checkout, so the client's provenance claim is a real claim ----
  mkdirSync(join(checkout, "flows"), { recursive: true });
  await writeFile(definitionPath, DEFINITION);
  await writeFile(inputPath, AUTHORED_INPUT);
  git(["init", "--quiet", "--initial-branch=main"], checkout);
  git(["add", "-A"], checkout);
  git(
    [
      "-c",
      "user.email=cycle@example.invalid",
      "-c",
      "user.name=ftfc14 cycle",
      "-c",
      "commit.gpgsign=false",
      "commit",
      "--quiet",
      "-m",
      "cycle fixture",
    ],
    checkout,
  );
  const commit = git(["rev-parse", "HEAD"], checkout);

  const scope = [
    "--base-url",
    baseUrl,
    "--token-file",
    options["token-file"],
    "--project",
    options.project,
    "--environment",
    options.environment,
    "--state",
    state,
  ];
  const steps = [];
  const commandIds = { authorized: [], refused: [] };

  // ---- leg 1: edit -> save -> validate -------------------------------------
  emit("leg edit-1 (committed working tree)");
  const first = record(
    steps,
    "edit-1",
    "validate",
    wamn("validate", [
      ...scope,
      "--command-id",
      `cycle-${runId}-edit-1`,
      "--file",
      definitionPath,
      "--draft-id",
      draftId,
      "--flow-id",
      FLOW_ID,
    ]),
    ["save-flow-draft", "validate"],
  );
  const firstSave = stepOf(first, "save-flow-draft");
  require_(
    "save-first-revision",
    firstSave.status === "completed" && firstSave.result.revision === 1,
    `the first save did not create revision 1: ${JSON.stringify(firstSave)}`,
  );
  commandIds.authorized.push(firstSave["command-id"]);

  // ---- leg 2: the editor changes one literal and saves again ---------------
  const editedText = DEFINITION.replace('"status":200', '"status":201');
  if (editedText === DEFINITION) throw new CheckFailure("checkout", "the fixture edit changed nothing");
  await writeFile(definitionPath, editedText);
  emit("leg edit-2 (dirty working tree)");
  const second = record(
    steps,
    "edit-2",
    "validate",
    wamn("validate", [
      ...scope,
      "--command-id",
      `cycle-${runId}-edit-2`,
      "--file",
      definitionPath,
      "--draft-id",
      draftId,
      "--flow-id",
      FLOW_ID,
    ]),
    ["save-flow-draft", "validate"],
  );
  const secondSave = stepOf(second, "save-flow-draft");
  require_(
    "save-second-revision",
    secondSave.status === "completed" && secondSave.result.revision === 2,
    `the second save did not advance to revision 2: ${JSON.stringify(secondSave)}`,
  );
  require_(
    "save-optimistic-concurrency-threaded",
    secondSave.result["draft-id"] === draftId && secondSave.result["flow-id"] === FLOW_ID,
    "the second save did not target the same draft identity",
  );
  commandIds.authorized.push(secondSave["command-id"]);

  const validated = stepOf(second, "validate");
  const validatedDraftId =
    validated.status === "completed"
      ? validated.result["validated-draft-id"]
      : PLACEHOLDER_VALIDATED_DRAFT;

  // ---- leg 3: draft-run ----------------------------------------------------
  emit("leg draft-run");
  const draftRun = record(
    steps,
    "draft-run",
    "draft-run",
    wamn("draft-run", [
      ...scope,
      "--command-id",
      `cycle-${runId}-draft-run`,
      "--input",
      inputPath,
      "--validated-draft",
      validatedDraftId,
    ]),
    ["draft-run"],
  );

  // ---- leg 4: test-set-run -------------------------------------------------
  emit("leg test-set-run");
  const testSetRun = record(
    steps,
    "test-set-run",
    "test-set-run",
    wamn("test-set-run", [
      ...scope,
      "--command-id",
      `cycle-${runId}-test-set-run`,
      "--validated-draft",
      validatedDraftId,
    ]),
    ["test-set-run"],
  );
  const testSetStep = stepOf(testSetRun, "test-set-run");
  const reportId =
    testSetStep.status === "completed" ? testSetStep.result["report-id"] : PLACEHOLDER_REPORT;

  // ---- leg 5: promote ------------------------------------------------------
  emit("leg promote");
  const promote = record(
    steps,
    "promote",
    "promote",
    wamn("promote", [
      ...scope,
      "--command-id",
      `cycle-${runId}-promote`,
      "--validated-draft",
      validatedDraftId,
      "--report-id",
      reportId,
    ]),
    ["publish"],
  );

  // ---- edit-to-run latency -------------------------------------------------
  const latency = draftRun["edit-to-run-ms"];
  if (typeof latency === "number") {
    emit(`  time  edit-to-run-ms=${latency} (working-tree edit -> run receipt)`);
  } else {
    emit(
      "  time  edit-to-run-ms=unmeasurable — no run receipt was issued because the " +
        "draft-run is unmounted on this surface",
    );
  }

  // ---- shortcut probes: every one of these must fail -----------------------
  emit("probes (each of these must fail)");
  const probeDocument = (change) => {
    const document = {
      document: "request",
      body: {
        "schema-version": "0.1",
        "command-id": `cycle-${runId}-probe`,
        command: {
          kind: "save-flow-draft",
          input: {
            definition: editedText,
            "draft-id": `${draftId}-probe`,
            "expected-revision": 0,
            "flow-id": FLOW_ID,
            scope: { environment: options.environment, "project-id": options.project },
          },
        },
      },
    };
    change(document);
    return JSON.stringify(document);
  };

  const tokenless = await post(`${baseUrl}${AUTHORING_PATH}`, { body: probeDocument(() => {}) });
  require_(
    "probe-tokenless-refused",
    tokenless.status === 403 && tokenless.text === REFUSAL_BODY,
    `expected the byte-exact 403 refusal, got HTTP ${tokenless.status} ${tokenless.text}`,
  );

  const unversioned = await post(`${baseUrl}${AUTHORING_PATH}`, {
    body: probeDocument((document) => {
      delete document.body["schema-version"];
    }),
    token,
  });
  require_(
    "probe-unversioned-refused",
    unversioned.status === 400,
    `an unversioned request was not refused: HTTP ${unversioned.status} ${unversioned.text}`,
  );
  const unsupported = await post(`${baseUrl}${AUTHORING_PATH}`, {
    body: probeDocument((document) => {
      document.body["schema-version"] = "0.2";
    }),
    token,
  });
  require_(
    "probe-unsupported-version-refused",
    unsupported.status === 400 &&
      JSON.parse(unsupported.text).kind === "unsupported-contract-version",
    `an unsupported version was not typed-refused: HTTP ${unsupported.status} ${unsupported.text}`,
  );

  // A forged token through the CLI proves the typed refusal a caller sees.
  const forgedPath = join(checkout, "forged.token");
  writeFileSync(forgedPath, `${forge(token)}\n`, { mode: 0o600 });
  const forgedRun = wamn("validate", [
    "--base-url",
    baseUrl,
    "--token-file",
    forgedPath,
    "--project",
    options.project,
    "--environment",
    options.environment,
    "--no-state",
    "--command-id",
    `cycle-${runId}-forged`,
    "--file",
    definitionPath,
    "--draft-id",
    `${draftId}-forged`,
    "--flow-id",
    FLOW_ID,
  ]);
  rmSync(forgedPath, { force: true });
  require_(
    "probe-forged-token-refused",
    forgedRun.code === 3 &&
      forgedRun.document?.steps?.[0]?.refusal?.reason?.kind === "authorization-denied",
    `a forged token was not refused as authorization-denied: exit ${forgedRun.code}`,
  );
  commandIds.refused.push(`cycle-${runId}-forged`);

  // The environment can hold every shortcut-shaped value there is; the CLI reads
  // none of them and refuses for want of the flags it actually accepts.
  const sentinel = "SENTINEL-cycle";
  const poisoned = launchWamn(
    [
      "promote",
      "--project",
      options.project,
      "--environment",
      options.environment,
      "--validated-draft",
      PLACEHOLDER_VALIDATED_DRAFT,
      "--report-id",
      PLACEHOLDER_REPORT,
    ],
    {
      ...process.env,
      PGPASSWORD: sentinel,
      WAMN_AUTHORING_BEARER_TOKEN: `wamn_pat_${sentinel}`,
      WAMN_AUTHORING_ENDPOINT: `${baseUrl}${AUTHORING_PATH}`,
      WAMN_AUTHORING_PG_URL: `postgres://${sentinel}@db.invalid/wamn`,
      WAMN_SYSTEM_URL: `postgres://${sentinel}@db.invalid/wamn`,
    },
  );
  require_(
    "probe-environment-shortcut-refused",
    poisoned.status === 2 &&
      poisoned.stdout === "" &&
      /--base-url is required/.test(poisoned.stderr) &&
      !poisoned.stderr.includes(sentinel),
    `the CLI accepted an endpoint or credential from the environment: exit ${poisoned.status}`,
  );

  // ---- the honest cycle table ---------------------------------------------
  emit("cycle");
  for (const step of steps) {
    emit(
      `  step  leg=${step.leg} verb=${step.verb} command=${step.command} status=${step.status}` +
        (step["http-status"] === undefined ? "" : ` http=${step["http-status"]}`) +
        (step.refusal === undefined ? "" : ` refusal=${step.refusal.reason.kind}`) +
        ` elapsed-ms=${step["elapsed-ms"]}`,
    );
  }
  const unmounted = [...new Set(steps.filter((step) => step.status === "unmounted").map((step) => step.command))];
  const completed = [...new Set(steps.filter((step) => step.status === "completed").map((step) => step.command))];
  emit(`CYCLE-COMPLETED ${JSON.stringify(completed)}`);
  emit(`CYCLE-UNMOUNTED-501 ${JSON.stringify(unmounted)}`);

  // ---- what a runner-side ledger read must find ---------------------------
  emit("audit manifest for the runner-side ledger read (this gate never reads the ledger)");
  emit(
    `VERIFY-MANIFEST ${JSON.stringify({
      "run-id": runId,
      project: options.project,
      environment: options.environment,
      "draft-id": draftId,
      "flow-id": FLOW_ID,
      "must-appear": commandIds.authorized.map((id, index) => ({
        "command-id": id,
        "command-kind": "save-flow-draft",
        revision: index + 1,
        "provenance-commit": commit,
        "provenance-ref": "refs/heads/main",
        "provenance-dirty": index === 1,
      })),
      "must-not-appear": commandIds.refused,
      "definition-sha256": createHash("sha256").update(editedText).digest("hex"),
      "definition-bytes": editedText.length,
    })}`,
  );
  if (options.checkout === undefined) rmSync(checkout, { force: true, recursive: true });
}

let status = 0;
try {
  await main();
} catch (error) {
  if (error instanceof CheckFailure) {
    emit(`  FAIL  ${error.check}`);
    emit(`FAILED check=${error.check} detail=${error.detail}`);
  } else {
    emit(`FAILED check=transport detail=${error.message}`);
  }
  status = 1;
}
if (leaked) {
  emit("FAILED check=no-credential-material-in-output detail=a line carried credential material");
  status = 1;
} else {
  emit("  ok    no-credential-material-in-output");
}
emit(status === 0 ? "CYCLE PASS" : "CYCLE FAIL");
process.exitCode = status;
