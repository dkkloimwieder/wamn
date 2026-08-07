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
  createFetchTransport,
  parseAuthoringRequest,
} = await import(process.env.WAMN_AUTHORING_CLIENT_TEST_MODULE);

test("generated output covers every public schema definition", async () => {
  const schema = JSON.parse(
    await readFile(new URL("../../../docs/contracts/authoring-surface.schema.json", import.meta.url)),
  );
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
    kind: "save-flow-draft",
    input: {
      definition: '{"flow":"draft"}',
      "draft-id": "draft-1",
      "expected-revision": 0,
      "flow-id": "flow-1",
      scope: { environment: "dev", "project-id": "project-1" },
    },
  },
};

function validateRequest(flowVersion) {
  return {
    "command-id": "validate-1",
    "schema-version": AUTHORING_SCHEMA_VERSION,
    command: {
      kind: "validate",
      input: {
        draft: { "draft-id": "draft-1", revision: Number.MAX_SAFE_INTEGER },
        scope: { environment: "dev", "project-id": "project-1" },
        suite: { "flow-version": flowVersion, "suite-id": "suite-1" },
      },
    },
  };
}

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

test("execute returns a schema-typed completion through a mock transport", async () => {
  let observed;
  const transport = {
    async execute(document) {
      observed = document;
      return response({
        status: "completed",
        value: {
          command: "save-flow-draft",
          result: { "draft-id": "draft-1", "flow-id": "flow-1", revision: 1 },
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
    async execute() {
      return response({
        status: "refused",
        value: {
          command: "save-flow-draft",
          reason: { kind: "authorization-denied" },
        },
      });
    },
  });

  const outcome = await client.execute(request);
  assert.deepEqual(outcome, {
    status: "refused",
    value: { command: "save-flow-draft", reason: { kind: "authorization-denied" } },
  });
});

test("unknown and unversioned requests fail before transport", async () => {
  let calls = 0;
  const client = new AuthoringClient({
    async execute() {
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
        command: "save-flow-draft",
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
            command: "save-flow-draft",
            reason: { kind: "authorization-denied" },
          },
        },
      },
    },
  ]) {
    const client = new AuthoringClient({ async execute() { return payload; } });
    await assert.rejects(client.execute(request), AuthoringProtocolError);
  }
});

async function assertCommandMismatch(outcome) {
  const client = new AuthoringClient({ async execute() { return response(outcome); } });
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
    async execute() {
      calls += 1;
      return response({ status: "refused", value: {} });
    },
  });
  await assert.rejects(client.execute(unsafeRequest), AuthoringProtocolError);
  assert.equal(calls, 0);

  const unsafeResponse = response({
    status: "completed",
    value: {
      command: "save-flow-draft",
      result: {
        "draft-id": "draft-1",
        "flow-id": "flow-1",
        revision: Number.MAX_SAFE_INTEGER + 1,
      },
    },
  });
  await assert.rejects(
    new AuthoringClient({ async execute() { return unsafeResponse; } }).execute(request),
    AuthoringProtocolError,
  );
});

test("uint32 request format accepts its boundary and rejects canonical flow-version overflow", () => {
  assert.doesNotThrow(() => parseAuthoringRequest(validateRequest(4_294_967_295)));
  assert.throws(() => parseAuthoringRequest(validateRequest(4_294_967_296)), AuthoringPayloadError);
});

test("uint32 and uint64 response formats enforce exact inclusive boundaries", async () => {
  const validationRequest = validateRequest(4_294_967_295);
  const identity = {
    "artifact-hash": "artifact-1",
    catalog: { "catalog-id": "catalog-1", version: 4_294_967_295 },
    draft: {
      "draft-id": "draft-1",
      "flow-id": "flow-1",
      revision: Number.MAX_SAFE_INTEGER,
    },
    environment: "dev",
    "execution-bundle-hash": "bundle-1",
    "runtime-flow-version": 4_294_967_295,
    "validated-draft-id": "validated-1",
  };
  const boundaryClient = new AuthoringClient({
    async execute() {
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
    async execute() {
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
          command: "save-flow-draft",
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
