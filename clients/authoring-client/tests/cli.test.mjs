// Network-free gates for the headless authoring CLI (wamn-ftfc.14).
//
// Three things are proven here without a surface in the loop.
//
// 1. REQUEST-SHAPE DRIFT. Every document the CLI can send is compared, key for
//    key, with the matching section of the checked-in request collection
//    (docs/archive/contracts/authoring-surface.v0.1.http) and is decoded by the
//    generated closed validator. The CLI owns the VALUES; the collection and
//    the generated schema own the SHAPE. A hand-rolled, renamed, missing, or
//    extra field fails before anything is sent.
// 2. TYPED ANSWERS. A completed command, a product refusal, an unmounted
//    command (`501`), and an infrastructure fault are four distinct outputs with
//    four distinct exit codes, and none of them can be read as another.
// 3. ABSENCE OF SHORTCUTS. Unversioned and unauthorized requests, privileged
//    database access, direct handler invocation, and frontend dependencies are
//    each refused by a structural check on the client itself.

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { closeSync, mkdtempSync, openSync, readFileSync, rmSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

assert.ok(process.env.WAMN_AUTHORING_CLI_TEST_MODULE, "compiled CLI module is required");
const cli = await import(process.env.WAMN_AUTHORING_CLI_TEST_MODULE);
const {
  AUTHORING_SCHEMA_VERSION,
  authoringSchema,
  parseAuthoringQueryRequest,
  parseAuthoringRequest,
} = await import(process.env.WAMN_AUTHORING_CLIENT_TEST_MODULE);

const COLLECTION_URL = new URL("../../../docs/archive/contracts/authoring-surface.v0.1.http", import.meta.url);
const CLI_SOURCE_URL = new URL("../src/cli/cli.ts", import.meta.url);
const ADAPTER_URL = new URL("../scripts/wamn.mjs", import.meta.url);
const LAUNCHER = fileURLToPath(ADAPTER_URL);

// SHA-256 over the SHAPE of every request in the checked-in collection: field
// names, nesting, and leaf types, with the values erased. Re-pin it only in a
// commit that reviewed a collection shape change. A value edit in the collection
// deliberately does not move this digest; a renamed or dropped field does.
const COLLECTION_SHAPE_DIGEST = "df2e4bcb8bb85511305b17653d5936b26e9af519a730fdac0813158dd2a56daf";

// Retained for the collection guard below, whose fixture file left the tree
// with `docs/archive` (ff04842e) and which therefore cannot pass today.
// deliberately not comparable — the client sends whatever the author wrote.
const OPAQUE_PATHS = new Set(["$.body.command.input.input"]);

const scope = { environment: "dev", "project-id": "receiving" };

/** Replace every leaf with its JSON type, keeping field names and nesting. */
function shapeOf(value, path = "$") {
  if (OPAQUE_PATHS.has(path)) return "opaque";
  if (value === null) return "null";
  if (Array.isArray(value)) return [...new Set(value.map((item) => JSON.stringify(shapeOf(item, `${path}[]`))))].sort();
  if (typeof value === "object") {
    const shape = {};
    for (const key of Object.keys(value).sort()) shape[key] = shapeOf(value[key], `${path}.${key}`);
    return shape;
  }
  return typeof value;
}

function canonical(value) {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (value !== null && typeof value === "object") {
    return `{${Object.keys(value)
      .sort()
      .map((key) => `${JSON.stringify(key)}:${canonical(value[key])}`)
      .join(",")}}`;
  }
  return JSON.stringify(value);
}

/** The collection's request documents, keyed by section name. */
async function collectionRequests() {
  const text = await readFile(COLLECTION_URL, "utf8");
  const requests = new Map();
  for (const section of text.split("\n### ").slice(1)) {
    const name = section.split("\n", 1)[0];
    const body = section.slice(section.indexOf("\n\n") + 2);
    requests.set(name, JSON.parse(body));
  }
  return requests;
}

/** One built document per command kind, exactly as the CLI would send it. */
function builtRequests() {
  return new Map([
    [
      "test-set-run",
      cli.testSetRunRequest({
        commandId: "test-set-1",
        scope,
        validatedDraftId: "sha256:validated-draft-v4",
      }),
    ],
    [
      "publish",
      cli.promoteRequest({
        commandId: "publish-1",
        reportId: "report-receiving-3",
        scope,
        validatedDraftId: "sha256:validated-draft-v4",
      }),
    ],
    [
      "get-report",
      cli.getReportRequest("get-report-1", scope, "report-receiving-3"),
    ],
  ]);
}

test("every CLI request has the shape of its checked-in collection section", async () => {
  const collection = await collectionRequests();
  const built = builtRequests();
  assert.deepEqual([...built.keys()].sort(), [...collection.keys()].sort());

  const shapes = {};
  for (const [name, document] of collection) {
    shapes[name] = shapeOf(document);
    assert.deepEqual(
      shapeOf({ document: "request", body: built.get(name) }),
      shapes[name],
      `${name} diverges in shape from the checked-in collection`,
    );
  }
  assert.equal(
    createHash("sha256").update(canonical(shapes)).digest("hex"),
    COLLECTION_SHAPE_DIGEST,
    "the collection's request shape changed",
  );
});

test("every CLI request decodes through the generated closed validator", () => {
  for (const [name, document] of builtRequests()) {
    const query = "query" in document;
    assert.doesNotThrow(
      () => (query ? parseAuthoringQueryRequest(document) : parseAuthoringRequest(document)),
      name,
    );
    assert.equal(document["schema-version"], AUTHORING_SCHEMA_VERSION, name);
    assert.equal(query ? document.query.kind : document.command.kind, name);
  }
});

// wamn-0h0g.8.5.5: the CLI saves nothing. A draft is this checkout's own working
// tree, so there is no builder for a server-side draft and no verb that names
// one. These are the request builders the collapse removed; a builder that came
// back would fail here rather than quietly re-introduce server-side draft state.
test("the CLI has no builder for any collapsed draft operation", () => {
  for (const removed of [
    "saveDraftRequest",
    "validateRequest",
    "draftRunRequest",
    "readDraftRequest",
    "checkoutProvenance",
  ]) {
    assert.equal(cli[removed], undefined, `the CLI re-grew ${removed}`);
  }
  assert.deepEqual([...cli.VERBS], ["test-set-run", "promote", "get-report"]);
});

test("the CLI sends exactly the schema's public operation inventory", async () => {
  const schema = authoringSchema;
  const operationKinds = (definition) =>
    definition.oneOf.map((variant) => variant.properties.kind.enum[0]);
  assert.deepEqual(
    [...builtRequests().keys()].sort(),
    [
      ...operationKinds(schema.definitions.AuthoringCommand),
      ...operationKinds(schema.definitions.AuthoringQuery),
    ].sort(),
  );
});

// ---------------------------------------------------------------------------
// A fake outside world
// ---------------------------------------------------------------------------

function response(commandId, outcome) {
  return {
    document: "response",
    body: { "command-id": commandId, "schema-version": AUTHORING_SCHEMA_VERSION, outcome },
  };
}

const VALIDATED_DRAFT_ID = "sha256:validated-draft-v4";
const gateReceipt = {
  "report-id": "report-receiving-3",
  "validated-draft": { "validated-draft-id": VALIDATED_DRAFT_ID },
};

function fakeIo({ files = {}, reply, now = () => 1_700_000_000_000 } = {}) {
  const state = { files: { ...files }, out: [], err: [], calls: [] };
  const io = {
    now,
    fetch: async (endpoint, init) => {
      state.calls.push({ endpoint, init });
      const answer = reply(endpoint, init, state.calls.length);
      if (answer instanceof Error) throw answer;
      return {
        ok: answer.status >= 200 && answer.status < 300,
        status: answer.status,
        json: async () => answer.body,
      };
    },
    readText: async (path) => {
      const contents = state.files[path];
      if (contents === undefined) throw new Error(`ENOENT ${path}`);
      return contents;
    },
    modifiedAt: async () => 1_699_999_999_000,
    readJson: async (path) => (state.files[path] === undefined ? undefined : JSON.parse(state.files[path])),
    writeJson: async (path, value) => {
      state.files[path] = `${JSON.stringify(value, null, 2)}\n`;
    },
    // A checkout with no repository: provenance is omitted rather than invented.
    git: () => undefined,
    out: (line) => state.out.push(line),
    err: (line) => state.err.push(line),
  };
  return { io, state };
}

const TOKEN_FILE = "token.env";
const TOKEN = "wamn_pat_0123456789abcdef_".padEnd(26 + 64, "a");
const DEFINITION = "flows/receive-material.flow.json";
const STATE = "cycle-state.json";

const baseFiles = {
  [TOKEN_FILE]: `${TOKEN}\n`,
  [DEFINITION]: '{"schema-version":"0.1","flow-id":"receive-material","version":1}',
};

const gateArguments = [
  "test-set-run",
  "--base-url",
  "http://surface.invalid",
  "--token-file",
  TOKEN_FILE,
  "--project",
  "receiving",
  "--environment",
  "dev",
  "--state",
  STATE,
  "--validated-draft",
  VALIDATED_DRAFT_ID,
];

function commandIdOf(init) {
  return JSON.parse(init.body).body["command-id"];
}

function document(state) {
  assert.equal(state.out.length, 1, "stdout must carry exactly one document");
  return JSON.parse(state.out[0]);
}

test("a completed gate emits its typed receipt and remembers the report", async () => {
  const { io, state } = fakeIo({
    files: baseFiles,
    reply: (endpoint, init) => {
      const body = JSON.parse(init.body).body;
      // The whole input, so a hard-coded or dropped value cannot pass.
      assert.deepEqual(body.command.input, {
        scope: { environment: "dev", "project-id": "receiving" },
        "validated-draft": { "validated-draft-id": VALIDATED_DRAFT_ID },
      });
      assert.equal(init.headers.authorization, `Bearer ${TOKEN}`);
      return {
        status: 200,
        body: response(body["command-id"], {
          status: "completed",
          value: { command: "test-set-run", result: gateReceipt },
        }),
      };
    },
  });

  const code = await cli.runCli(gateArguments, io);
  assert.equal(code, cli.EXIT_COMPLETED);
  const emitted = document(state);
  assert.equal(emitted.status, "completed");
  assert.deepEqual(
    emitted.steps.map((step) => [step.command, step.status]),
    [["test-set-run", "completed"]],
  );
  assert.deepEqual(emitted.steps[0].result, gateReceipt);

  const remembered = JSON.parse(state.files[STATE]);
  assert.equal(remembered["report-id"], "report-receiving-3");
  // The client-local cache holds public identities and nothing else, and it no
  // longer remembers any server-side draft coordinate (wamn-0h0g.8.5.5).
  assert.equal(remembered["draft-id"], undefined);
  assert.equal(remembered.revision, undefined);
  assert.doesNotMatch(state.files[STATE], /wamn_pat_|secret|correct horse/);
});

test("an unmounted command is its own answer and never a success", async () => {
  const { io, state } = fakeIo({
    files: baseFiles,
    // The surface answers 501 with no document for a command kind whose
    // handler has not landed.
    reply: () => ({ status: 501, body: "" }),
  });

  const code = await cli.runCli(gateArguments, io);
  assert.equal(code, cli.EXIT_UNMOUNTED);
  const emitted = document(state);
  assert.equal(emitted.status, "unmounted");
  assert.equal(emitted.steps[0].status, "unmounted");
  assert.equal(emitted.steps[0]["http-status"], 501);
  assert.equal(emitted.steps[0].result, undefined);
  assert.equal(emitted.steps[0].refusal, undefined);
});

test("a product refusal is typed, exits 3, and is not a fault", async () => {
  // The constitutional clause's refusal: a gate is a judgment about a document,
  // so a candidate reaching an effectful component is refused, never executed.
  const refusal = {
    command: "test-set-run",
    reason: { components: ["acme:ledger"], kind: "effectful-component-reached" },
  };
  const { io, state } = fakeIo({
    files: baseFiles,
    reply: (_endpoint, init) => ({
      status: 200,
      body: response(commandIdOf(init), { status: "refused", value: refusal }),
    }),
  });

  assert.equal(await cli.runCli(gateArguments, io), cli.EXIT_REFUSED);
  const emitted = document(state);
  assert.equal(emitted.status, "refused");
  assert.deepEqual(emitted.steps[0].refusal, refusal);
  assert.equal(emitted.steps.length, 1);
});

test("an unauthorized presenter is refused with the frozen contract kind", async () => {
  // The surface refuses before dispatch with a bare `authorization-denied`
  // document under HTTP 403, so there is no response envelope to read.
  const { io, state } = fakeIo({
    files: baseFiles,
    reply: () => ({ status: 403, body: { kind: "authorization-denied" } }),
  });
  assert.equal(await cli.runCli(gateArguments, io), cli.EXIT_REFUSED);
  const emitted = document(state);
  assert.deepEqual(emitted.steps[0].refusal, {
    command: "test-set-run",
    reason: { kind: "authorization-denied" },
  });
  assert.equal(emitted.steps[0]["http-status"], 403);
});

test("a missing token file is a usage error that reaches no network", async () => {
  const withoutToken = gateArguments.filter(
    (argument, index, all) => argument !== "--token-file" && all[index - 1] !== "--token-file",
  );
  const { io, state } = fakeIo({ files: baseFiles, reply: () => ({ status: 200, body: {} }) });
  assert.equal(await cli.runCli(withoutToken, io), cli.EXIT_USAGE);
  assert.equal(state.calls.length, 0);
  assert.equal(state.out.length, 0);
});

test("network, HTTP, and protocol failures are faults, not refusals", async () => {
  const cases = [
    [new Error("offline"), "network"],
    [{ status: 500, body: "" }, "http"],
    [{ status: 200, body: { document: "response", body: { outcome: {} } } }, "protocol"],
  ];
  for (const [authoringAnswer, kind] of cases) {
    const { io, state } = fakeIo({
      files: baseFiles,
      reply: () => authoringAnswer,
    });
    assert.equal(await cli.runCli(gateArguments, io), cli.EXIT_FAULT, kind);
    const emitted = document(state);
    assert.equal(emitted.status, "fault", kind);
    assert.equal(emitted.steps[0].fault.kind, kind);
    assert.equal(emitted.steps[0].result, undefined);
    assert.equal(emitted.steps[0].refusal, undefined);
  }
});

test("no token material ever reaches stdout or the transcript", async () => {
  const { io, state } = fakeIo({
    files: baseFiles,
    reply: (_endpoint, init) => ({
      status: 200,
      body: response(commandIdOf(init), {
        status: "completed",
        value: { command: "test-set-run", result: gateReceipt },
      }),
    }),
  });
  await cli.runCli(gateArguments, io);
  const transcript = [...state.out, ...state.err].join("\n");
  assert.doesNotMatch(transcript, /wamn_pat_/);
});

// ---------------------------------------------------------------------------
// Absence of shortcuts, checked on the client itself
// ---------------------------------------------------------------------------

test("no option, file, or default can send an unversioned or reversioned request", async () => {
  const source = await readFile(CLI_SOURCE_URL, "utf8");
  // Every request is built by the one `request` factory, which stamps the
  // version from the generated constant. No literal version string exists in the
  // client at all, so there is nothing for a flag or a file to select.
  const factory = source.slice(source.indexOf("function request("));
  assert.equal(factory.slice(0, factory.indexOf("\n}")).split("AUTHORING_SCHEMA_VERSION").length - 1, 1);
  assert.equal(source.split("return request(").length - 1, 2);
  assert.equal(source.split("return queryRequest(").length - 1, 1);
  assert.doesNotMatch(source, /"schema-version":\s*"/);
  for (const rejected of ["--schema-version", "--contract-version", "--endpoint"]) {
    assert.throws(() => cli.parseArguments([rejected, "0.2"]), /unrecognized option/);
  }
  // And the generated client refuses an unversioned document before transport,
  // so there is no path that reaches the wire without a version.
  const built = cli.testSetRunRequest({
    commandId: "test-set-1",
    scope,
    validatedDraftId: "sha256:validated-draft-v4",
  });
  const unversioned = { ...built };
  delete unversioned["schema-version"];
  assert.throws(() => parseAuthoringRequest(unversioned));
});

test("the client holds no privileged database or operator capability", async () => {
  const source = await readFile(CLI_SOURCE_URL, "utf8");
  const adapter = await readFile(ADAPTER_URL, "utf8");
  for (const forbidden of [
    "postgres",
    "psql",
    "pg_",
    "DATABASE_URL",
    "dolt",
    "kubectl",
    "wamn-ctl",
    "cargo",
    "tenant",
    "app.tenant",
  ]) {
    assert.doesNotMatch(source, new RegExp(forbidden, "i"), `${forbidden} in the CLI`);
    assert.doesNotMatch(adapter, new RegExp(forbidden, "i"), `${forbidden} in the node adapter`);
  }
  // The compiled CLI imports the generated client and nothing else: no node
  // builtin, so it cannot open a socket, a file, or a process on its own.
  const compiled = await readFile(new URL(process.env.WAMN_AUTHORING_CLI_TEST_MODULE), "utf8");
  const imports = [...compiled.matchAll(/from ["']([^"']+)["']/g)].map((match) => match[1]);
  assert.deepEqual([...new Set(imports)].sort(), ["../client.js", "../generated/authoring.js"]);
  // wamn-0h0g.8.5.5: the node adapter spawns NOTHING. The `git` provenance
  // reader left with `save-draft`, the one command that could carry a commit
  // claim, so the capability is withdrawn rather than left dangling.
  assert.doesNotMatch(adapter, /spawnSync|child_process/);
});

test("the client depends on no frontend and on no application handler", async () => {
  const source = await readFile(CLI_SOURCE_URL, "utf8");
  const adapter = await readFile(ADAPTER_URL, "utf8");
  const manifest = JSON.parse(await readFile(new URL("../package.json", import.meta.url), "utf8"));
  assert.equal(manifest.dependencies, undefined, "the client declares no runtime dependency");
  for (const forbidden of [
    "solid-js",
    "vite",
    "loop-console",
    "\\bwindow\\b",
    "localStorage",
    "querySelector",
    "createElement",
    "addEventListener",
    "services/",
    "crates/",
    "wamn_scenario_worker",
    "wamn-scenario-worker",
  ]) {
    assert.doesNotMatch(source, new RegExp(forbidden), `${forbidden} in the CLI`);
    assert.doesNotMatch(adapter, new RegExp(forbidden), `${forbidden} in the node adapter`);
  }
});

test("the launched CLI reads no endpoint, credential, or database URL from the environment", () => {
  // Every name a shortcut would use, including the collection's own two
  // variables, is present and poisoned. The CLI must still refuse for want of
  // `--base-url`, and must not echo a poisoned value anywhere.
  const sentinel = "SENTINEL-a1b2c3";
  const poisoned = {
    ...process.env,
    DATABASE_URL: `postgres://${sentinel}@db.invalid/wamn`,
    PGPASSWORD: sentinel,
    WAMN_AUTHORING_BEARER_TOKEN: `wamn_pat_${sentinel}`,
    WAMN_AUTHORING_ENDPOINT: `http://${sentinel}.invalid/authoring`,
    WAMN_AUTHORING_PG_URL: `postgres://${sentinel}@db.invalid/wamn`,
    WAMN_SYSTEM_URL: `postgres://${sentinel}@db.invalid/wamn`,
  };
  const captureDirectory = mkdtempSync(join(tmpdir(), "wamn-authoring-cli-test-"));
  const stdoutPath = join(captureDirectory, "stdout");
  const stderrPath = join(captureDirectory, "stderr");
  const stdoutFd = openSync(stdoutPath, "w");
  const stderrFd = openSync(stderrPath, "w");
  try {
    const launched = spawnSync(
      process.execPath,
      [
        LAUNCHER,
        "promote",
        "--project",
        "receiving",
        "--environment",
        "dev",
        "--validated-draft",
        "validated-1",
        "--report-id",
        "r",
      ],
      { env: poisoned, stdio: ["ignore", stdoutFd, stderrFd] },
    );
    closeSync(stdoutFd);
    closeSync(stderrFd);

    const stdout = readFileSync(stdoutPath, "utf8");
    const stderr = readFileSync(stderrPath, "utf8");
    assert.equal(launched.status, cli.EXIT_USAGE);
    assert.match(stderr, /--base-url is required/);
    assert.equal(stdout, "");
    assert.doesNotMatch(stderr, new RegExp(sentinel));
  } finally {
    rmSync(captureDirectory, { force: true, recursive: true });
  }
});
