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
    assertSupportedAuthoringSchema({ allOf: [{ $ref: "#/definitions/SomeEnum" }], default: "full" }),
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

const SCOPE = { environment: "dev", "project-id": "project-1" };
const VALIDATED = { "validated-draft-id": "validated-1" };

const request = {
  "command-id": "command-1",
  "schema-version": AUTHORING_SCHEMA_VERSION,
  command: {
    kind: "test-set-run",
    input: { scope: SCOPE, "validated-draft": VALIDATED },
  },
};

const queryRequest = {
  "query-id": "query-1",
  "schema-version": AUTHORING_SCHEMA_VERSION,
  query: {
    kind: "get-report",
    input: { "report-id": "report-1", scope: SCOPE },
  },
};

const gateReceipt = { "report-id": "report-1", "validated-draft": VALIDATED };

const finalizedReport = {
  passed: true,
  "report-id": "report-1",
  state: "finalized",
  summary: { cases: [] },
  "validated-draft": VALIDATED,
};

/// wamn-0h0g.8.5.5: the four collapsed operations are gone from the generated
/// closed validator, so a document naming one is refused before transport.
test("the collapsed draft operations are refused by the generated validator", () => {
  const collapsed = [
    {
      kind: "save-draft",
      input: {
        definition: "{}",
        "draft-id": "draft-1",
        "expected-revision": 0,
        "wiring-id": "flow-1",
        scope: SCOPE,
      },
    },
    { kind: "validate", input: { draft: { "draft-id": "draft-1", revision: 1 }, scope: SCOPE } },
    {
      kind: "draft-run",
      input: { input: {}, scope: SCOPE, "validated-draft": VALIDATED },
    },
  ];
  for (const command of collapsed) {
    assert.throws(
      () => parseAuthoringRequest({ ...request, command }),
      AuthoringPayloadError,
      command.kind,
    );
  }
  assert.throws(
    () =>
      parseAuthoringQueryRequest({
        ...queryRequest,
        query: { kind: "read-draft", input: { draft: { "draft-id": "d", revision: 1 }, scope: SCOPE } },
      }),
    AuthoringPayloadError,
  );
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
        value: { query: "get-report", result: finalizedReport },
      });
    },
  });
  assert.equal((await client.query(queryRequest)).status, "completed");
  assert.deepEqual(observed, { document: "request", body: queryRequest });

  for (const payload of [
    queryResponse({
      status: "completed",
      value: { query: "get-report", result: finalizedReport },
    }, "wrong-query"),
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
          command: "test-set-run",
          result: gateReceipt,
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
          command: "test-set-run",
          reason: { kind: "authorization-denied" },
        },
      });
    },
  });

  const outcome = await client.execute(request);
  assert.deepEqual(outcome, {
    status: "refused",
    value: { command: "test-set-run", reason: { kind: "authorization-denied" } },
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
        command: "test-set-run",
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
            command: "test-set-run",
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
      command: "publish",
      result: { "artifact-hash": "sha256:artifact", version: 1, "wiring-id": "wiring-1" },
    },
  });
});

test("refused response command must match the request", async () => {
  await assertCommandMismatch({
    status: "refused",
    value: {
      command: "publish",
      reason: { kind: "authorization-denied" },
    },
  });
});

/// wamn-0h0g.8.5.5 deleted every `uint64` wire site with the draft operations
/// (`expected-revision` and the draft `revision`). `PublishedWiringIdentity`'s
/// `version` is the ONE numeric field the contract still carries, so the exact
/// inclusive `uint32` boundary is pinned there.
test("uint32 response format enforces its exact inclusive boundary", async () => {
  const publishRequest = {
    "command-id": "publish-1",
    "schema-version": AUTHORING_SCHEMA_VERSION,
    command: {
      kind: "publish",
      input: {
        scope: SCOPE,
        "successful-report-id": "report-1",
        "validated-draft": VALIDATED,
      },
    },
  };
  const identityFor = (version) => ({
    document: "response",
    body: {
      "command-id": publishRequest["command-id"],
      "schema-version": AUTHORING_SCHEMA_VERSION,
      outcome: {
        status: "completed",
        value: {
          command: "publish",
          result: { "artifact-hash": "sha256:artifact", version, "wiring-id": "wiring-1" },
        },
      },
    },
  });
  const client = (version) =>
    new AuthoringClient({ async executeCommand() { return identityFor(version); } });

  assert.equal(
    (await client(4_294_967_295).execute(publishRequest)).value.result.version,
    4_294_967_295,
  );
  for (const refused of [-1, 4_294_967_296]) {
    await assert.rejects(client(refused).execute(publishRequest), AuthoringProtocolError);
  }
});

/// A request that fails the closed validator never reaches the transport.
test("an invalid request fails before transport", async () => {
  let calls = 0;
  const client = new AuthoringClient({
    async executeCommand() {
      calls += 1;
      return response({ status: "refused", value: {} });
    },
  });
  const forged = structuredClone(request);
  forged.command.input["expected-revision"] = 0;
  assert.throws(() => parseAuthoringRequest(forged), AuthoringPayloadError);
  await assert.rejects(client.execute(forged), AuthoringProtocolError);
  assert.equal(calls, 0);
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
          command: "test-set-run",
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
