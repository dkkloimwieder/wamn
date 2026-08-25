import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

assert.ok(process.env.WAMN_AUTHORING_CLIENT_TEST_MODULE, "compiled client module is required");
const {
  AUTHORING_SCHEMA_VERSION,
  AuthoringClient,
  AuthoringHttpError,
  AuthoringNetworkError,
  AuthoringPayloadError,
  AuthoringProtocolError,
  assertSupportedAuthoringSchema,
  authoringSchema,
  createFetchTransport,
  parseAuthoringQueryRequest,
  parseAuthoringRequest,
} = await import(process.env.WAMN_AUTHORING_CLIENT_TEST_MODULE);

test("generated output covers every public schema definition", async () => {
  // The schema ships inside the generated module; `generate.mjs --check` is what
  // proves that module still matches the live Rust source it came from.
  const schema = authoringSchema;
  const generated = await readFile(
    new URL("../src/generated/authoring.ts", import.meta.url),
    "utf8",
  );

  assert.equal(AUTHORING_SCHEMA_VERSION, "0.1");
  for (const name of Object.keys(schema.definitions)) {
    assert.match(generated, new RegExp(`export type ${name} =`), name);
  }
  assert.match(generated, /export type AuthoringDocument =/);
});

test("runtime schema support fails closed on unknown keywords and formats", () => {
  assert.doesNotThrow(() =>
    assertSupportedAuthoringSchema({ format: "uint64", minimum: 0, type: "integer" }),
  );
  assert.doesNotThrow(() =>
    assertSupportedAuthoringSchema({ allOf: [{ $ref: "#/definitions/DraftRunCapture" }], default: "full" }),
  );
  assert.doesNotThrow(() =>
    assertSupportedAuthoringSchema({ minLength: 1, type: "string", "x-max-utf8-bytes": 64 }),
  );
  assert.throws(
    () => assertSupportedAuthoringSchema({ allOf: [{ type: "string" }] }),
    /allOf must contain exactly one \$ref/,
  );
  assert.throws(
    () => assertSupportedAuthoringSchema({ allOf: [{ $ref: "#/definitions/A" }, { $ref: "#/definitions/B" }] }),
    /allOf must contain exactly one \$ref/,
  );
  assert.throws(
    () => assertSupportedAuthoringSchema({ pattern: "^[a-z]+$", type: "string" }),
    /unsupported schema keyword pattern/,
  );
  assert.throws(
    () => assertSupportedAuthoringSchema({ format: "uuid", type: "string" }),
    /unsupported schema format uuid/,
  );
});

const request = {
  "command-id": "command-1",
  "schema-version": AUTHORING_SCHEMA_VERSION,
  command: {
    kind: "save-draft",
    input: {
      definition: '{"flow":"draft"}',
      "draft-id": "draft-1",
      "expected-revision": 0,
      "wiring-id": "flow-1",
      scope: { environment: "dev", "project-id": "project-1" },
    },
  },
};

const queryRequest = {
  "query-id": "query-1",
  "schema-version": AUTHORING_SCHEMA_VERSION,
  query: {
    kind: "read-draft",
    input: {
      draft: { "draft-id": "draft-1", revision: 1 },
      scope: { environment: "dev", "project-id": "project-1" },
    },
  },
};

const draftDocument = {
  definition: "{}",
  draft: { "draft-id": "draft-1", "wiring-id": "flow-1", revision: 1 },
};

function validateRequest() {
  return {
    "command-id": "validate-1",
    "schema-version": AUTHORING_SCHEMA_VERSION,
    command: {
      kind: "validate",
      input: {
        draft: { "draft-id": "draft-1", revision: Number.MAX_SAFE_INTEGER },
        scope: { environment: "dev", "project-id": "project-1" },
      },
    },
  };
}

function draftRunRequest(capture) {
  return {
    "command-id": "draft-run-1",
    "schema-version": AUTHORING_SCHEMA_VERSION,
    command: {
      kind: "draft-run",
      input: {
        ...(capture === undefined ? {} : { capture }),
        input: { receipt: "r-1" },
        scope: { environment: "dev", "project-id": "project-1" },
        "validated-draft": { "validated-draft-id": "validated-1" },
      },
    },
  };
}

test("draft-run capture accepts omission, full, and off only", () => {
  for (const capture of [undefined, "full", "off"]) {
    assert.doesNotThrow(() => parseAuthoringRequest(draftRunRequest(capture)));
  }
  for (const retired of ["scrubbed", "preview"]) {
    assert.throws(() => parseAuthoringRequest(draftRunRequest(retired)), AuthoringPayloadError);
  }
});

function response(outcome) {
  return {
    document: "response",
    body: {
      "command-id": request["command-id"],
      "schema-version": AUTHORING_SCHEMA_VERSION,
      outcome,
    },
  };
}

function queryResponse(outcome, queryId = queryRequest["query-id"]) {
  return {
    document: "response",
    body: {
      outcome,
      "query-id": queryId,
      "schema-version": AUTHORING_SCHEMA_VERSION,
    },
  };
}

test("query-id enforces nonempty UTF-8 byte length at 64", () => {
  for (const value of ["a".repeat(64), "é".repeat(32), "🦀".repeat(16)]) {
    assert.doesNotThrow(() => parseAuthoringQueryRequest({ ...queryRequest, "query-id": value }));
  }
  for (const value of ["", "a".repeat(65), `${"a".repeat(63)}é`, `${"a".repeat(61)}🦀`]) {
    assert.throws(
      () => parseAuthoringQueryRequest({ ...queryRequest, "query-id": value }),
      AuthoringPayloadError,
    );
  }
});

test("query dispatch is separate and verifies query-id plus operation echoes", async () => {
  let observed;
  const client = new AuthoringClient({
    async executeQuery(document) {
      observed = document;
      return queryResponse({
        status: "completed",
        value: { query: "read-draft", result: draftDocument },
      });
    },
  });
  assert.equal((await client.query(queryRequest)).status, "completed");
  assert.deepEqual(observed, { document: "request", body: queryRequest });

  for (const payload of [
    queryResponse({
      status: "completed",
      value: { query: "read-draft", result: draftDocument },
    }, "wrong-query"),
    queryResponse({
      status: "refused",
      value: { query: "get-report", reason: { kind: "report-not-found", "report-id": "report-1" } },
    }),
  ]) {
    await assert.rejects(
      new AuthoringClient({ async executeQuery() { return payload; } }).query(queryRequest),
      AuthoringProtocolError,
    );
  }
});

test("execute returns a schema-typed completion through a mock transport", async () => {
  let observed;
  const transport = {
    async executeCommand(document) {
      observed = document;
      return response({
        status: "completed",
        value: {
          command: "save-draft",
          result: { "draft-id": "draft-1", "wiring-id": "flow-1", revision: 1 },
        },
      });
    },
  };

  const outcome = await new AuthoringClient(transport).execute(request);
  assert.equal(outcome.status, "completed");
  assert.deepEqual(observed, { document: "request", body: request });
});

test("typed refusals return normally and are not infrastructure faults", async () => {
  const client = new AuthoringClient({
    async executeCommand() {
      return response({
        status: "refused",
        value: {
          command: "save-draft",
          reason: { kind: "authorization-denied" },
        },
      });
    },
  });

  const outcome = await client.execute(request);
  assert.deepEqual(outcome, {
    status: "refused",
    value: { command: "save-draft", reason: { kind: "authorization-denied" } },
  });
});

test("unknown and unversioned requests fail before transport", async () => {
  let calls = 0;
  const client = new AuthoringClient({
    async executeCommand() {
      calls += 1;
      return response({ status: "refused", value: {} });
    },
  });

  const unknown = { ...request, principal: "forged" };
  await assert.rejects(client.execute(unknown), AuthoringProtocolError);

  const unversioned = { ...request };
  delete unversioned["schema-version"];
  await assert.rejects(
    client.execute(unversioned),
    AuthoringProtocolError,
  );
  assert.equal(calls, 0);
});

test("unknown, unversioned, and mismatched responses are protocol faults", async () => {
  for (const payload of [
    { ...response({ status: "refused", value: {} }), extra: true },
    { document: "response", body: { "command-id": "command-1", outcome: {} } },
    response({
      status: "refused",
      value: {
        command: "save-draft",
        reason: { kind: "authorization-denied", detail: "must stay prose-free" },
      },
    }),
    {
      document: "response",
      body: {
        "command-id": "different",
        "schema-version": AUTHORING_SCHEMA_VERSION,
        outcome: {
          status: "refused",
          value: {
            command: "save-draft",
            reason: { kind: "authorization-denied" },
          },
        },
      },
    },
  ]) {
    const client = new AuthoringClient({ async executeCommand() { return payload; } });
    await assert.rejects(client.execute(request), AuthoringProtocolError);
  }
});

async function assertCommandMismatch(outcome) {
  const client = new AuthoringClient({ async executeCommand() { return response(outcome); } });
  await assert.rejects(
    client.execute(request),
    (error) =>
      error instanceof AuthoringProtocolError &&
      error.message === "authoring response command does not match the request",
  );
}

test("completed response command must match the request", async () => {
  await assertCommandMismatch({
    status: "completed",
    value: {
      command: "draft-run",
      result: { "run-id": "run-1", "validated-draft": { "validated-draft-id": "v1" } },
    },
  });
});

test("refused response command must match the request", async () => {
  await assertCommandMismatch({
    status: "refused",
    value: {
      command: "validate",
      reason: { kind: "authorization-denied" },
    },
  });
});

test("unsafe request and response integers fail instead of returning rounded values", async () => {
  const unsafeRequest = structuredClone(request);
  unsafeRequest.command.input["expected-revision"] = Number.MAX_SAFE_INTEGER + 1;
  assert.throws(() => parseAuthoringRequest(unsafeRequest), AuthoringPayloadError);

  let calls = 0;
  const client = new AuthoringClient({
    async executeCommand() {
      calls += 1;
      return response({ status: "refused", value: {} });
    },
  });
  await assert.rejects(client.execute(unsafeRequest), AuthoringProtocolError);
  assert.equal(calls, 0);

  const unsafeResponse = response({
    status: "completed",
    value: {
      command: "save-draft",
      result: {
        "draft-id": "draft-1",
        "wiring-id": "flow-1",
        revision: Number.MAX_SAFE_INTEGER + 1,
      },
    },
  });
  await assert.rejects(
    new AuthoringClient({ async executeCommand() { return unsafeResponse; } }).execute(request),
    AuthoringProtocolError,
  );
});

test("uint64 wire domain accepts 2^53-1 and refuses 2^53 and u64 max", async () => {
  // wamn-ftfc.21 settled the domain at [0, 2^53-1]. `u64::MAX` is written as a
  // JavaScript literal on purpose: the parse is already lossy, which is exactly
  // what the contract refuses to let reach an identity.
  const exact = structuredClone(request);
  exact.command.input["expected-revision"] = 9007199254740991;
  assert.equal(parseAuthoringRequest(exact).command.input["expected-revision"], 9007199254740991);

  for (const refused of [9007199254740992, 18446744073709551615]) {
    const outOfDomain = structuredClone(request);
    outOfDomain.command.input["expected-revision"] = refused;
    assert.throws(() => parseAuthoringRequest(outOfDomain), AuthoringPayloadError);
  }

  const identityFor = (revision) =>
    response({
      status: "completed",
      value: {
        command: "save-draft",
        result: { "draft-id": "draft-1", "wiring-id": "flow-1", revision },
      },
    });
  const client = (revision) =>
    new AuthoringClient({ async executeCommand() { return identityFor(revision); } });

  assert.equal(
    (await client(9007199254740991).execute(request)).value.result.revision,
    9007199254740991,
  );
  for (const refused of [9007199254740992, 18446744073709551615]) {
    await assert.rejects(client(refused).execute(request), AuthoringProtocolError);
  }
});

test("uint32 and uint64 response formats enforce exact inclusive boundaries", async () => {
  const validationRequest = validateRequest();
  const identity = {
    "artifact-hash": "artifact-1",
    catalog: { "catalog-id": "catalog-1", version: 4_294_967_295 },
    draft: {
      "draft-id": "draft-1",
      "wiring-id": "flow-1",
      revision: Number.MAX_SAFE_INTEGER,
    },
    environment: "dev",
    "runtime-flow-version": 4_294_967_295,
    "validated-draft-id": "validated-1",
  };
  const boundaryClient = new AuthoringClient({
    async executeCommand() {
      return {
        document: "response",
        body: {
          "command-id": validationRequest["command-id"],
          "schema-version": AUTHORING_SCHEMA_VERSION,
          outcome: { status: "completed", value: { command: "validate", result: identity } },
        },
      };
    },
  });
  assert.equal((await boundaryClient.execute(validationRequest)).status, "completed");

  const overflowIdentity = { ...identity, "runtime-flow-version": 4_294_967_296 };
  const overflowClient = new AuthoringClient({
    async executeCommand() {
      return {
        document: "response",
        body: {
          "command-id": validationRequest["command-id"],
          "schema-version": AUTHORING_SCHEMA_VERSION,
          outcome: {
            status: "completed",
            value: { command: "validate", result: overflowIdentity },
          },
        },
      };
    },
  });
  await assert.rejects(overflowClient.execute(validationRequest), AuthoringProtocolError);
});

test("fetch transport posts to only the caller-supplied endpoint", async () => {
  let observedEndpoint = "";
  let observedInit;
  const transport = createFetchTransport({
    endpoint: "https://management.example/authoring",
    headers: { authorization: "Bearer mock" },
    async fetch(endpoint, init) {
      observedEndpoint = endpoint;
      observedInit = init;
      return { ok: true, status: 200, async json() { return response({
        status: "refused",
        value: {
          command: "save-draft",
          reason: { kind: "authorization-denied" },
        },
      }); } };
    },
  });

  await new AuthoringClient(transport).execute(request);
  assert.equal(observedEndpoint, "https://management.example/authoring");
  assert.deepEqual(JSON.parse(observedInit.body), {
    document: "request",
    body: request,
  });
  assert.equal(
    observedInit.headers.authorization,
    "Bearer mock",
  );
});

test("fetch network, HTTP, and JSON failures remain distinct", async () => {
  const network = createFetchTransport({
    endpoint: "/authoring",
    async fetch() { throw new Error("offline"); },
  });
  await assert.rejects(new AuthoringClient(network).execute(request), AuthoringNetworkError);

  const http = createFetchTransport({
    endpoint: "/authoring",
    async fetch() { return { ok: false, status: 503, async json() { return null; } }; },
  });
  await assert.rejects(new AuthoringClient(http).execute(request), AuthoringHttpError);

  const invalidJson = createFetchTransport({
    endpoint: "/authoring",
    async fetch() {
      return { ok: true, status: 200, async json() { throw new SyntaxError("bad JSON"); } };
    },
  });
  await assert.rejects(new AuthoringClient(invalidJson).execute(request), AuthoringProtocolError);
});
