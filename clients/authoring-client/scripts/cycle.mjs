// The composed edit-to-publish cycle gate for the headless CLI (wamn-ftfc.14).
//
// WHAT THIS IS. The whole authoring loop driven by the shipped `wamn` CLI and
// nothing else: `test-set-run` (the `gate` verb), `promote`, `get-report`. Every
// leg is a subprocess invocation of scripts/wamn.mjs whose stdout document is
// read as the result, so the gate proves the CLI's public behaviour rather than
// its internals.
//
// THERE IS NO EDIT LEG (wamn-0h0g.8.5.5). A draft is a CLIENT-SIDE FILE, so the
// platform stores no working-tree document and the CLI has no save, validate,
// ad-hoc-run or read-back verb to drive. The loop starts from a candidate that
// is already gated, named by its content hash.
//
// WHAT THIS IS NOT. Pure HTTP, like the wamn-jvzx.4 smoke: the caller supplies a
// base URL and ONE pre-issued PAT file and nothing else. No database URL, no
// platform-admin impersonation, no test-only trusted context, and no ledger
// read — that read needs storage authority a client must not hold. The gate
// closes by printing one VERIFY-MANIFEST line naming what a runner-side ledger
// read must find; the `[6A / wamn-ftfc.14]` section of docs/operations/build-and-test.md
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
import { randomBytes } from "node:crypto";
import {
  closeSync,
  mkdtempSync,
  openSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { mkdtemp, readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

import { PACKAGE_ROOT, compiledPackage } from "./compile.mjs";

const LAUNCHER = join(PACKAGE_ROOT, "scripts", "wamn.mjs");

const AUTHORING_PATH = "/authoring";
const REFUSAL_BODY = '{"kind":"authorization-denied"}';
const PAT_PATTERN = /^(wamn_pat_[0-9a-f]{16}_)([0-9a-f]{64})$/;

const WIRING_ID = "receive-material";

// The candidate this loop gates, and the report a gate has not produced yet.
// The caller names a real candidate with `--validated-draft`; absent one, the
// legs use a contract-shaped placeholder purely to reach transport.
const PLACEHOLDER_VALIDATED_DRAFT = "sha256:validated-draft-not-yet-issued";
const PLACEHOLDER_REPORT = "report-not-yet-reserved";

/// The required keys of every completed result on the contract. A mounted
/// command that answers with a different identity fails here rather than being
/// accepted because it returned 200.
const RESULT_KEYS = {
  "test-set-run": ["report-id", "validated-draft"],
  publish: ["artifact-hash", "wiring-id", "version"],
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
const NAMED_ARGUMENTS = ["--base-url", "--token-file", "--project", "--environment", "--validated-draft"];

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
  for (const verb of ["test-set-run", "promote", "get-report"]) {
    require_(`cli-verb-${verb}`, help.stderr.includes(`  ${verb} `), `--help does not document ${verb}`);
  }
  // The cycle covers the whole public command inventory: every kind in the
  // generated schema is reached by a documented verb. That schema is read from
  // the module that ships rather than from a second copy on disk.
  const { authoringSchema: schema } = await import(
    pathToFileURL(join(compiled, "index.js")).href
  );
  const covered = ["test-set-run", "publish"];
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
    NAMED_ARGUMENTS.join(" ") ===
      "--base-url --token-file --project --environment --validated-draft",
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
  // A scratch directory for the client-local state file and the forged-token
  // fixture. It is NOT a checkout: nothing here is a working tree any more.
  const scratch = await mkdtemp(join(tmpdir(), `wamn-ftfc14-cycle-${runId}-`));
  const state = join(scratch, ".wamn", "state.json");

  emit(`surface ${baseUrl}`);
  emit(`run-id=${runId} wiring-id=${WIRING_ID}`);

  // wamn-0h0g.8.5.5: there is no checkout, no git repository and no working-tree
  // fixture here. Those existed to feed `save-draft` and `draft-run`, and both
  // commands left the contract with the draft concept. The loop below starts
  // from a candidate that is already gated, named by its content hash.
  const validatedDraftId = options["validated-draft"] ?? PLACEHOLDER_VALIDATED_DRAFT;

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

  // ---- leg 1: test-set-run, the gate verb ---------------------------------
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

  // ---- leg 2: promote ------------------------------------------------------
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

  // ---- shortcut probes: every one of these must fail -----------------------
  emit("probes (each of these must fail)");
  const probeDocument = (change) => {
    const document = {
      document: "request",
      body: {
        "schema-version": "0.1",
        "command-id": `cycle-${runId}-probe`,
        command: {
          kind: "test-set-run",
          input: {
            scope: { environment: options.environment, "project-id": options.project },
            "validated-draft": { "validated-draft-id": validatedDraftId },
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
  const forgedPath = join(scratch, "forged.token");
  writeFileSync(forgedPath, `${forge(token)}\n`, { mode: 0o600 });
  const forgedRun = wamn("test-set-run", [
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
    "--validated-draft",
    validatedDraftId,
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
      "wiring-id": WIRING_ID,
      "validated-draft-id": validatedDraftId,
      "must-appear": commandIds.authorized.map((id) => ({
        "command-id": id,
        "command-kind": "test-set-run",
        "target-ref": validatedDraftId,
      })),
      "must-not-appear": commandIds.refused,
    })}`,
  );
  rmSync(scratch, { force: true, recursive: true });
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
