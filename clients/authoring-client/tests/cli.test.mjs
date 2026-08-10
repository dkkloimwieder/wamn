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
import { readFile } from "node:fs/promises";
import test from "node:test";
import { fileURLToPath } from "node:url";

assert.ok(process.env.WAMN_AUTHORING_CLI_TEST_MODULE, "compiled CLI module is required");
const cli = await import(process.env.WAMN_AUTHORING_CLI_TEST_MODULE);
const {
  AUTHORING_SCHEMA_VERSION,
  parseAuthoringRequest,
} = await import(process.env.WAMN_AUTHORING_CLIENT_TEST_MODULE);

const COLLECTION_URL = new URL("../../../docs/archive/contracts/authoring-surface.v0.1.http", import.meta.url);
const SCHEMA_URL = new URL("../../../docs/archive/contracts/authoring-surface.schema.json", import.meta.url);
const CLI_SOURCE_URL = new URL("../src/cli/cli.ts", import.meta.url);
const ADAPTER_URL = new URL("../scripts/wamn.mjs", import.meta.url);
const LAUNCHER = fileURLToPath(ADAPTER_URL);

// SHA-256 over the SHAPE of every request in the checked-in collection: field
// names, nesting, and leaf types, with the values erased. Re-pin it only in a
// commit that reviewed a collection shape change. A value edit in the collection
// deliberately does not move this digest; a renamed or dropped field does.
const COLLECTION_SHAPE_DIGEST = "9d1af46366c67ed6f9f7e9679d60884472bfd4d779d10d05fdd6fc9ab71813b1";

// `draft-run`'s authored input is `unknown` on the contract, so its shape is
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
      "save-flow-draft",
      cli.saveFlowDraftRequest({
        commandId: "save-1",
        definition: '{"schema-version":"0.1"}',
        draftId: "draft-receiving",
        expectedRevision: 2,
        flowId: "receive-material",
        provenance: { commit: "0".repeat(40), dirty: false, ref: "refs/heads/main" },
        scope,
      }),
    ],
    [
      "validate",
      cli.validateRequest({
        commandId: "validate-1",
        draftId: "draft-receiving",
        flowVersion: 3,
        revision: 3,
        scope,
        suiteId: "receiving-happy-path",
      }),
    ],
    [
      "draft-run",
      cli.draftRunRequest({
        commandId: "run-1",
        input: { material: "aluminum" },
        scope,
        validatedDraftId: "sha256:validated-draft-v4",
      }),
    ],
    [
      "suite-run",
      cli.suiteRunRequest({
        commandId: "suite-1",
        flowVersion: 3,
        scope,
        suiteId: "receiving-happy-path",
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
      "suite-projection",
      cli.runsRequest({ commandId: "read-1", reportId: "report-receiving-3", scope }),
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
    assert.doesNotThrow(() => parseAuthoringRequest(document), name);
    assert.equal(document["schema-version"], AUTHORING_SCHEMA_VERSION, name);
    assert.equal(document.command.kind, name);
  }
  // The optional provenance claim is the only field a request may omit, and
  // omitting it stays a valid document.
  assert.doesNotThrow(() =>
    parseAuthoringRequest(
      cli.saveFlowDraftRequest({
        commandId: "save-2",
        definition: "",
        draftId: "d",
        expectedRevision: 0,
        flowId: "f",
        scope,
      }),
    ),
  );
});

test("the CLI sends exactly the schema's public command inventory", async () => {
  const schema = JSON.parse(await readFile(SCHEMA_URL, "utf8"));
  assert.deepEqual(
    [...builtRequests().keys()].sort(),
    [...schema.definitions.AuthoringCommandKind.enum].sort(),
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

const draftIdentity = { "draft-id": "draft-receiving", "flow-id": "receive-material", revision: 1 };
const validatedIdentity = {
  "artifact-hash": "sha256:artifact",
  catalog: { "catalog-id": "receiving", version: 7 },
  draft: draftIdentity,
  environment: "dev",
  "execution-bundle-hash": "sha256:bundle",
  "runtime-flow-version": 4,
  "validated-draft-id": "sha256:validated-draft-v4",
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

const CREDENTIAL = "principal.env";
const DEFINITION = "flows/receive-material.flow.json";
const STATE = "cycle-state.json";

const baseFiles = {
  [CREDENTIAL]: "subject=author@example.com\nsecret=correct horse battery staple\n",
  [DEFINITION]: '{"schema-version":"0.1","flow-id":"receive-material","version":1}',
};

const validateArguments = [
  "validate",
  "--base-url",
  "http://surface.invalid",
  "--credential",
  CREDENTIAL,
  "--project",
  "receiving",
  "--environment",
  "dev",
  "--state",
  STATE,
  "--file",
  DEFINITION,
  "--draft-id",
  "draft-receiving",
  "--flow-id",
  "receive-material",
  "--suite-id",
  "receiving-happy-path",
  "--flow-version",
  "3",
];

const TOKEN = "wamn_pat_0123456789abcdef_".padEnd(26 + 64, "a");

function loginReply() {
  return { status: 200, body: { expires_at: "2026-08-08T00:00:00Z", token: TOKEN } };
}

function commandIdOf(init) {
  return JSON.parse(init.body).body["command-id"];
}

function document(state) {
  assert.equal(state.out.length, 1, "stdout must carry exactly one document");
  return JSON.parse(state.out[0]);
}

test("a completed save and validate emit typed identities and remember them", async () => {
  const { io, state } = fakeIo({
    files: baseFiles,
    reply: (endpoint, init) => {
      if (endpoint.endsWith("/login")) return loginReply();
      const body = JSON.parse(init.body).body;
      if (body.command.kind === "save-flow-draft") {
        // The whole input, so a hard-coded or dropped value cannot pass.
        assert.deepEqual(body.command.input, {
          definition: baseFiles[DEFINITION],
          "draft-id": "draft-receiving",
          "expected-revision": 0,
          "flow-id": "receive-material",
          scope: { environment: "dev", "project-id": "receiving" },
        });
        assert.equal(init.headers.authorization, `Bearer ${TOKEN}`);
        return {
          status: 200,
          body: response(body["command-id"], {
            status: "completed",
            value: { command: "save-flow-draft", result: draftIdentity },
          }),
        };
      }
      assert.equal(body.command.input.draft.revision, 1);
      return {
        status: 200,
        body: response(body["command-id"], {
          status: "completed",
          value: { command: "validate", result: validatedIdentity },
        }),
      };
    },
  });

  const code = await cli.runCli(validateArguments, io);
  assert.equal(code, cli.EXIT_COMPLETED);
  const emitted = document(state);
  assert.equal(emitted.status, "completed");
  assert.deepEqual(
    emitted.steps.map((step) => [step.command, step.status]),
    [
      ["save-flow-draft", "completed"],
      ["validate", "completed"],
    ],
  );
  assert.deepEqual(emitted.steps[0].result, draftIdentity);
  assert.equal(emitted.steps[1].result["validated-draft-id"], "sha256:validated-draft-v4");

  const remembered = JSON.parse(state.files[STATE]);
  assert.equal(remembered.revision, 1);
  assert.equal(remembered["validated-draft-id"], "sha256:validated-draft-v4");
  // The client-local cache holds public identities and nothing else.
  assert.doesNotMatch(state.files[STATE], /wamn_pat_|secret|correct horse/);
});

test("an unmounted command is its own answer and never a success", async () => {
  const { io, state } = fakeIo({
    files: baseFiles,
    reply: (endpoint, init) => {
      if (endpoint.endsWith("/login")) return loginReply();
      const body = JSON.parse(init.body).body;
      if (body.command.kind === "save-flow-draft") {
        return {
          status: 200,
          body: response(body["command-id"], {
            status: "completed",
            value: { command: "save-flow-draft", result: draftIdentity },
          }),
        };
      }
      // The surface answers 501 with no document for a command kind whose
      // handler has not landed.
      return { status: 501, body: "" };
    },
  });

  const code = await cli.runCli(validateArguments, io);
  assert.equal(code, cli.EXIT_UNMOUNTED);
  const emitted = document(state);
  assert.equal(emitted.status, "unmounted");
  assert.equal(emitted.steps[0].status, "completed");
  assert.equal(emitted.steps[1].status, "unmounted");
  assert.equal(emitted.steps[1]["http-status"], 501);
  assert.equal(emitted.steps[1].result, undefined);
  assert.equal(emitted.steps[1].refusal, undefined);
});

test("a product refusal is typed, exits 3, and is not a fault", async () => {
  const refusal = {
    command: "save-flow-draft",
    reason: { "expected-revision": 0, kind: "revision-conflict" },
  };
  const { io, state } = fakeIo({
    files: baseFiles,
    reply: (endpoint, init) =>
      endpoint.endsWith("/login")
        ? loginReply()
        : {
            status: 200,
            body: response(commandIdOf(init), { status: "refused", value: refusal }),
          },
  });

  assert.equal(await cli.runCli(validateArguments, io), cli.EXIT_REFUSED);
  const emitted = document(state);
  assert.equal(emitted.status, "refused");
  assert.deepEqual(emitted.steps[0].refusal, refusal);
  // A refused save never runs the validate step against an invented revision.
  assert.equal(emitted.steps.length, 1);
});

test("an unauthorized presenter is refused with the frozen contract kind", async () => {
  // The surface refuses before dispatch with a bare `authorization-denied`
  // document under HTTP 403, so there is no response envelope to read.
  const { io, state } = fakeIo({
    files: { ...baseFiles, "token.env": `${TOKEN}\n` },
    reply: () => ({ status: 403, body: { kind: "authorization-denied" } }),
  });
  const withToken = validateArguments
    .filter((argument, index, all) => argument !== "--credential" && all[index - 1] !== "--credential")
    .concat(["--token-file", "token.env"]);
  assert.equal(await cli.runCli(withToken, io), cli.EXIT_REFUSED);
  const emitted = document(state);
  assert.deepEqual(emitted.steps[0].refusal, {
    command: "save-flow-draft",
    reason: { kind: "authorization-denied" },
  });
  assert.equal(emitted.steps[0]["http-status"], 403);

  // A refused login never reaches an authoring command at all.
  const denied = fakeIo({ files: baseFiles, reply: () => ({ status: 403, body: { kind: "authorization-denied" } }) });
  assert.equal(await cli.runCli(validateArguments, denied.io), cli.EXIT_REFUSED);
  assert.equal(denied.state.calls.length, 1);
  assert.match(denied.state.calls[0].endpoint, /\/login$/);
  assert.match(document(denied.state).status, /refused/);
});

test("a missing credential is a usage error that reaches no network", async () => {
  const withoutCredential = validateArguments.filter(
    (argument, index, all) => argument !== "--credential" && all[index - 1] !== "--credential",
  );
  const { io, state } = fakeIo({ files: baseFiles, reply: () => ({ status: 200, body: {} }) });
  assert.equal(await cli.runCli(withoutCredential, io), cli.EXIT_USAGE);
  assert.equal(state.calls.length, 0);
  assert.equal(state.out.length, 0);

  // Presenting both is equally refused: the CLI never picks one silently.
  const both = validateArguments.concat(["--token-file", "token.env"]);
  const ambiguous = fakeIo({ files: baseFiles, reply: () => ({ status: 200, body: {} }) });
  assert.equal(await cli.runCli(both, ambiguous.io), cli.EXIT_USAGE);
  assert.equal(ambiguous.state.calls.length, 0);
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
      reply: (endpoint) => (endpoint.endsWith("/login") ? loginReply() : authoringAnswer),
    });
    assert.equal(await cli.runCli(validateArguments, io), cli.EXIT_FAULT, kind);
    const emitted = document(state);
    assert.equal(emitted.status, "fault", kind);
    assert.equal(emitted.steps[0].fault.kind, kind);
    assert.equal(emitted.steps[0].result, undefined);
    assert.equal(emitted.steps[0].refusal, undefined);
  }
});

test("draft-run reports the edit-to-run latency of the working-tree edit", async () => {
  const files = {
    ...baseFiles,
    "input.json": '{"receipt-id":"receipt-1042"}',
    [STATE]: JSON.stringify({
      "draft-id": "draft-receiving",
      "edit-at": 1_699_999_990_000,
      revision: 1,
      "state-version": 1,
      "validated-draft-id": "sha256:validated-draft-v4",
    }),
  };
  const { io, state } = fakeIo({
    files,
    reply: (endpoint, init) =>
      endpoint.endsWith("/login")
        ? loginReply()
        : {
            status: 200,
            body: response(commandIdOf(init), {
              status: "completed",
              value: {
                command: "draft-run",
                result: {
                  "run-id": "run-1",
                  "validated-draft": { "validated-draft-id": "sha256:validated-draft-v4" },
                },
              },
            }),
          },
  });

  const code = await cli.runCli(
    [
      "draft-run",
      "--base-url",
      "http://surface.invalid",
      "--credential",
      CREDENTIAL,
      "--project",
      "receiving",
      "--environment",
      "dev",
      "--state",
      STATE,
      "--input",
      "input.json",
    ],
    io,
  );
  assert.equal(code, cli.EXIT_COMPLETED);
  const emitted = document(state);
  assert.equal(emitted["edit-to-run-ms"], 1_700_000_000_000 - 1_699_999_990_000);
  assert.ok(state.err.some((line) => line.includes("edit-to-run-ms=10000")));
  assert.equal(JSON.parse(state.files[STATE])["run-id"], "run-1");
});

test("runs reports the projection's own edit-to-run latency and case runs", async () => {
  const report = {
    branches: [{ branch: { "from-node-id": "request", "from-port": "out" }, coverage: "covered" }],
    cases: [{ "case-id": "happy", failure: null, outcome: "passed", "run-id": "run-1" }],
    draft: validatedIdentity,
    edges: [
      {
        coverage: "covered",
        edge: {
          "from-node-id": "request",
          "from-port": "out",
          "to-node-id": "respond",
          "to-port": null,
        },
      },
    ],
    "edit-to-run-ms": 4321,
    "execution-id": "execution-1",
    nodes: [
      {
        "failed-case-ids": [],
        "node-id": "request",
        "observed-case-ids": ["happy"],
        outcome: "passed",
      },
    ],
    outcome: { state: "passed" },
    "projection-version": "0.1",
    "report-id": "report-receiving-3",
    suite: { "flow-version": 3, "suite-id": "receiving-happy-path" },
  };
  const { io, state } = fakeIo({
    files: baseFiles,
    reply: (endpoint, init) =>
      endpoint.endsWith("/login")
        ? loginReply()
        : {
            status: 200,
            body: response(commandIdOf(init), {
              status: "completed",
              value: {
                command: "suite-projection",
                result: { report, state: "finalized" },
              },
            }),
          },
  });

  const code = await cli.runCli(
    [
      "runs",
      "--base-url",
      "http://surface.invalid",
      "--credential",
      CREDENTIAL,
      "--project",
      "receiving",
      "--environment",
      "dev",
      "--no-state",
      "--report-id",
      "report-receiving-3",
    ],
    io,
  );
  assert.equal(code, cli.EXIT_COMPLETED);
  assert.equal(document(state)["server-edit-to-run-ms"], 4321);
  assert.ok(state.err.some((line) => line.includes("case-id=happy run-id=run-1 outcome=passed")));
});

test("no credential material ever reaches stdout or the transcript", async () => {
  const { io, state } = fakeIo({
    files: baseFiles,
    reply: (endpoint, init) =>
      endpoint.endsWith("/login")
        ? loginReply()
        : {
            status: 200,
            body: response(commandIdOf(init), {
              status: "completed",
              value: { command: "save-flow-draft", result: draftIdentity },
            }),
          },
  });
  await cli.runCli(validateArguments, io);
  const transcript = [...state.out, ...state.err].join("\n");
  assert.doesNotMatch(transcript, /wamn_pat_/);
  assert.doesNotMatch(transcript, /correct horse battery staple/);
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
  assert.equal(source.split("return request(").length - 1, 6);
  assert.doesNotMatch(source, /"schema-version":\s*"/);
  for (const rejected of ["--schema-version", "--contract-version", "--endpoint"]) {
    assert.throws(() => cli.parseArguments([rejected, "0.2"]), /unrecognized option/);
  }
  // And the generated client refuses an unversioned document before transport,
  // so there is no path that reaches the wire without a version.
  const built = cli.saveFlowDraftRequest({
    commandId: "save-1",
    definition: "",
    draftId: "d",
    expectedRevision: 0,
    flowId: "f",
    scope,
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
  // The node adapter's only child process is a read-only git query.
  const spawns = [...adapter.matchAll(/spawnSync\(\s*("[^"]*"|[A-Za-z]+)/g)].map((match) => match[1]);
  assert.deepEqual(spawns, ['"git"']);
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
  const launched = spawnSync(
    process.execPath,
    [LAUNCHER, "runs", "--project", "receiving", "--environment", "dev", "--report-id", "r"],
    { encoding: "utf8", env: poisoned },
  );
  assert.equal(launched.status, cli.EXIT_USAGE);
  assert.match(launched.stderr, /--base-url is required/);
  assert.equal(launched.stdout, "");
  assert.doesNotMatch(launched.stderr, new RegExp(sentinel));
});
