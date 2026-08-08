// S0 authenticated smoke gate for the checked-in authoring request collection.
//
// WHAT THIS IS. A pure HTTP client. It exchanges one principal's own subject and
// secret for a personal access token on the reserved login route, presents that
// token as a Bearer credential, and POSTs the `save-flow-draft` request DERIVED
// FROM docs/contracts/authoring-surface.v0.1.http. It holds no database URL, no
// platform-admin impersonation, and no test-only trusted context: the only
// authority it ever presents is a token issued for a credential it was handed.
//
// WHAT THIS IS NOT. It never reads the audit ledger — that read needs storage
// authority this client must not have. Attribution evidence is verified by the
// GATE RUNNER around this script; see the [jvzx.4] section of
// docs/build-and-test.md. The script's contribution to that step is the
// AUDIT-MANIFEST line it prints, naming the command-ids that must appear with
// which principal subject and the command-ids that must NOT appear at all.
//
// THE COLLECTION IS THE ARTIFACT UNDER TEST. Every executable field of the
// outgoing request comes from the checked-in section; only the three declared
// per-run substitutions below may differ. Any other divergence — introduced in
// this script or in the collection — fails a drift check before a byte is sent.
//
//   node scripts/smoke.mjs --check
//   node scripts/smoke.mjs --base-url http://HOST:PORT \
//     --principal /path/to/first.env --principal /path/to/second.env
//
// A credential file is `key=value` lines and must carry `subject` and `secret`.
// It is read as a file so no secret byte ever reaches a command line, an
// environment block, or this script's output.

import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";

const COLLECTION_URL = new URL(
  "../../../docs/contracts/authoring-surface.v0.1.http",
  import.meta.url,
);
const SECTION = "save-flow-draft";
const ENDPOINT_LINE = "POST {{$processEnv WAMN_AUTHORING_ENDPOINT}}";
const AUTHORIZATION_LINE = "Authorization: Bearer {{$processEnv WAMN_AUTHORING_BEARER_TOKEN}}";
const REQUIRED_HEADERS = ["Accept: application/json", "Content-Type: application/json"];

// The two routes the management surface reserves. The collection deliberately
// defines no route and no login request, so supplying both is the caller's job —
// which is precisely the job this script does.
const AUTHORING_PATH = "/authoring";
const LOGIN_PATH = "/login";
const LOGIN_LABEL = "authoring-collection-smoke";

// The one frozen document every authentication and authorization failure
// returns, compared byte for byte.
const REFUSAL_BODY = '{"kind":"authorization-denied"}';
const PAT_PATTERN = /^(wamn_pat_[0-9a-f]{16}_)([0-9a-f]{64})$/;

// SHA-256 of the canonicalized `save-flow-draft` request document in the
// checked-in collection. Re-pin it only in a commit that reviewed the
// collection change; a silent edit on either side must fail this gate.
const TEMPLATE_DIGEST = "7d72007db14e4418f4e6bf230fd18c41594fc8fd5055fa0370f0985194fe437a";

// The complete set of fields a run may substitute, and nowhere else to write.
// `command-id` identifies one attempt, `draft-id` keeps a run from colliding
// with the last one, and `expected-revision` is the collection's own optimistic
// concurrency field, which must track the revision the draft actually has.
const SUBSTITUTIONS = {
  "command-id": ["body", "command-id"],
  "draft-id": ["body", "command", "input", "draft-id"],
  "expected-revision": ["body", "command", "input", "expected-revision"],
};

// One request per leg, all four against the SAME draft in the collection's own
// optimistic-concurrency semantics: principal A creates revision 1, the two
// refused attempts and principal B all present `expected-revision: 1`. Principal
// B's success at revision 2 is therefore only reachable if neither refused
// attempt executed — the refusal happened before the command, not after it.
const LEGS = [
  {
    name: "authorized-a",
    principal: 0,
    credential: "issued",
    expect: "authorized",
    expectedRevision: 0,
    savedRevision: 1,
  },
  { name: "tokenless", principal: 0, credential: "absent", expect: "refused", expectedRevision: 1 },
  { name: "forged-token", principal: 0, credential: "forged", expect: "refused", expectedRevision: 1 },
  {
    name: "authorized-b",
    principal: 1,
    credential: "issued",
    expect: "authorized",
    expectedRevision: 1,
    savedRevision: 2,
  },
];

class CheckFailure extends Error {
  constructor(check, detail) {
    super(`${check}: ${detail}`);
    this.check = check;
    this.detail = detail;
  }
}

const secrets = new Set();
let leaked = false;

/** Emit one transcript line, refusing to print registered secret material. */
function emit(line) {
  let text = String(line);
  for (const secret of secrets) {
    if (secret.length > 0 && text.includes(secret)) {
      leaked = true;
      text = "<<REDACTED — token material reached the transcript>>";
      break;
    }
  }
  process.stdout.write(`${text}\n`);
}

function require_(check, condition, detail) {
  if (!condition) throw new CheckFailure(check, detail);
  emit(`  ok    ${check}`);
}

/** Deterministic JSON with sorted object keys, for digests and comparison. */
function canonical(value) {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (value !== null && typeof value === "object") {
    const fields = Object.keys(value)
      .sort()
      .map((key) => `${JSON.stringify(key)}:${canonical(value[key])}`);
    return `{${fields.join(",")}}`;
  }
  return JSON.stringify(value);
}

function readPath(document, path) {
  return path.reduce((node, key) => (node === undefined ? undefined : node[key]), document);
}

function writePath(document, path, value) {
  const parent = readPath(document, path.slice(0, -1));
  if (parent === undefined || parent === null) {
    throw new CheckFailure("collection-derivation", `no ${path.join(".")} to substitute`);
  }
  parent[path.at(-1)] = value;
}

/**
 * Read the collection's `save-flow-draft` section.
 *
 * The header contract is the same one the static collection gate enforces: the
 * caller supplies the complete endpoint, exactly one caller-supplied bearer
 * token, and JSON on both sides. A checked-in token or a second authorization
 * header fails here too.
 */
function collectionTemplate(collection) {
  const sections = collection.split("\n### ").slice(1);
  const matched = sections.filter((section) => section.split("\n", 1)[0] === SECTION);
  if (matched.length !== 1) {
    throw new CheckFailure(
      "collection-header-contract",
      `collection must define exactly one ${SECTION} request, found ${matched.length}`,
    );
  }
  const request = matched[0].slice(SECTION.length + 1);
  const separator = request.indexOf("\n\n");
  if (separator < 0) throw new CheckFailure("collection-header-contract", "section has no body");
  const headers = request.slice(0, separator).split("\n").filter((line) => line.length > 0);
  const body = request.slice(separator + 2);

  if (headers[0] !== ENDPOINT_LINE) {
    throw new CheckFailure(
      "collection-header-contract",
      "the request must POST to the caller-supplied endpoint",
    );
  }
  const authorization = headers.filter((line) => /^authorization\s*:/i.test(line));
  if (authorization.length !== 1 || authorization[0] !== AUTHORIZATION_LINE) {
    throw new CheckFailure(
      "collection-header-contract",
      "the request must carry exactly one caller-supplied bearer token",
    );
  }
  for (const required of REQUIRED_HEADERS) {
    if (!headers.includes(required)) {
      throw new CheckFailure("collection-header-contract", `missing header ${required}`);
    }
  }

  let template;
  try {
    template = JSON.parse(body);
  } catch (error) {
    throw new CheckFailure("collection-header-contract", `body is not JSON: ${error.message}`);
  }
  emit("  ok    collection-header-contract");
  return template;
}

function derive(template, values) {
  const document = structuredClone(template);
  for (const [field, path] of Object.entries(SUBSTITUTIONS)) {
    writePath(document, path, values[field]);
  }
  return document;
}

/**
 * Fail unless an outgoing request is the collection's request with only the
 * declared substitutions applied. Restoring each declared field from the
 * template must reproduce the checked-in document exactly.
 */
function assertDerivedFromCollection(outgoing, template) {
  const reverted = structuredClone(outgoing);
  for (const path of Object.values(SUBSTITUTIONS)) {
    writePath(reverted, path, readPath(template, path));
  }
  if (canonical(reverted) !== canonical(template)) {
    throw new CheckFailure(
      "collection-derivation",
      `outgoing request diverges from ${SECTION} in the checked-in collection outside the declared substitutions`,
    );
  }
}

async function loadCredential(path) {
  const text = await readFile(path, "utf8");
  const fields = new Map();
  for (const line of text.split("\n")) {
    const separator = line.indexOf("=");
    if (separator > 0) fields.set(line.slice(0, separator).trim(), line.slice(separator + 1).trim());
  }
  const subject = fields.get("subject");
  const secret = fields.get("secret");
  if (!subject || !secret) {
    throw new CheckFailure("credential-file", `${path} must define subject and secret`);
  }
  secrets.add(secret);
  return { path, subject, secret };
}

async function post(url, { body, token }) {
  const headers = { accept: "application/json", "content-type": "application/json" };
  if (token !== undefined) headers.authorization = `Bearer ${token}`;
  const response = await fetch(url, { method: "POST", headers, body });
  return { status: response.status, text: await response.text() };
}

async function login(baseUrl, credential, ordinal) {
  const check = `login-principal-${ordinal}`;
  const body = JSON.stringify({
    subject: credential.subject,
    secret: credential.secret,
    label: LOGIN_LABEL,
  });
  const { status, text } = await post(`${baseUrl}${LOGIN_PATH}`, { body });
  require_(`${check}-status`, status === 200, `expected HTTP 200, got HTTP ${status} ${text}`);
  const issued = JSON.parse(text);
  const match = PAT_PATTERN.exec(String(issued.token ?? ""));
  require_(`${check}-token-shape`, match !== null, "login did not return a well-formed token");
  secrets.add(issued.token);
  secrets.add(match[2]);
  emit(`  info  principal ${ordinal} subject=${credential.subject} token=<redacted> expires_at=${issued.expires_at}`);
  return issued.token;
}

/** A structurally valid token whose secret half is wrong by one hex digit. */
function forge(token) {
  const [, lookup, secret] = PAT_PATTERN.exec(token);
  const flipped = secret[0] === "0" ? "1" : "0";
  const forged = `${lookup}${flipped}${secret.slice(1)}`;
  secrets.add(forged);
  return forged;
}

function assertAuthorized(check, leg, response) {
  require_(
    `${check}-status`,
    response.status === 200,
    `expected HTTP 200, got HTTP ${response.status} ${response.text}`,
  );
  const document = JSON.parse(response.text);
  require_(
    `${check}-response-document`,
    document.document === "response" && document.body["schema-version"] === "0.1",
    "response is not a version 0.1 authoring response document",
  );
  require_(
    `${check}-command-identity`,
    document.body["command-id"] === leg.commandId,
    "response does not answer the command-id that was sent",
  );
  const outcome = document.body.outcome;
  require_(
    `${check}-completed`,
    outcome.status === "completed" && outcome.value.command === SECTION,
    `expected a completed ${SECTION}, got ${canonical(outcome)}`,
  );
  const result = outcome.value.result;
  require_(
    `${check}-saved-revision`,
    result.revision === leg.savedRevision &&
      result["draft-id"] === leg.draftId &&
      result["flow-id"] === leg.flowId,
    `expected revision ${leg.savedRevision} on ${leg.draftId}, got ${canonical(result)}`,
  );
}

function assertRefused(check, response) {
  require_(
    `${check}-status`,
    response.status === 403,
    `expected HTTP 403, got HTTP ${response.status} ${response.text}`,
  );
  require_(
    `${check}-frozen-refusal`,
    response.text === REFUSAL_BODY,
    `expected the byte-exact refusal ${REFUSAL_BODY}, got ${response.text}`,
  );
}

function parseArguments(argv) {
  const options = { check: false, baseUrl: undefined, principals: [] };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--check") options.check = true;
    else if (argument === "--base-url") options.baseUrl = argv[(index += 1)];
    else if (argument === "--principal") options.principals.push(argv[(index += 1)]);
    else throw new CheckFailure("usage", `unrecognized argument ${argument}`);
  }
  return options;
}

async function main() {
  const options = parseArguments(process.argv.slice(2));
  const collection = await readFile(COLLECTION_URL, "utf8");

  emit(`collection ${COLLECTION_URL.pathname}`);
  const template = collectionTemplate(collection);
  const digest = createHash("sha256").update(canonical(template)).digest("hex");
  require_(
    "collection-template-digest",
    digest === TEMPLATE_DIGEST,
    `the ${SECTION} request changed: expected ${TEMPLATE_DIGEST}, got ${digest}`,
  );

  const flowId = readPath(template, ["body", "command", "input", "flow-id"]);
  const scope = readPath(template, ["body", "command", "input", "scope"]);
  const runId = `${Date.now().toString(36)}`;
  const draftId = `${readPath(template, SUBSTITUTIONS["draft-id"])}-smoke-${runId}`;
  const baseCommandId = readPath(template, SUBSTITUTIONS["command-id"]);
  for (const leg of LEGS) {
    leg.commandId = `${baseCommandId}-smoke-${runId}-${leg.name}`;
    leg.draftId = draftId;
    leg.flowId = flowId;
    leg.document = derive(template, {
      "command-id": leg.commandId,
      "draft-id": draftId,
      "expected-revision": leg.expectedRevision,
    });
    assertDerivedFromCollection(leg.document, template);
  }
  emit(`  ok    collection-derivation`);
  emit(`  info  run-id=${runId} draft-id=${draftId} flow-id=${flowId}`);

  if (options.check) {
    emit("static drift checks only (--check); no request was sent");
    return;
  }
  if (!options.baseUrl || options.principals.length !== 2) {
    throw new CheckFailure(
      "usage",
      "a live run needs --base-url and exactly two --principal credential files",
    );
  }

  const baseUrl = options.baseUrl.replace(/\/+$/, "");
  const credentials = [];
  for (const path of options.principals) credentials.push(await loadCredential(path));
  require_(
    "distinct-principals",
    credentials[0].subject !== credentials[1].subject,
    "the two credential files name the same principal",
  );

  emit(`surface ${baseUrl}`);
  const tokens = [];
  for (const [ordinal, credential] of credentials.entries()) {
    emit(`login principal-${ordinal}`);
    tokens.push(await login(baseUrl, credential, ordinal));
  }

  for (const leg of LEGS) {
    const check = `authoring-leg-${leg.name}`;
    emit(`leg ${leg.name} credential=${leg.credential} expect=${leg.expect} expected-revision=${leg.expectedRevision}`);
    const token =
      leg.credential === "absent"
        ? undefined
        : leg.credential === "forged"
          ? forge(tokens[leg.principal])
          : tokens[leg.principal];
    const response = await post(`${baseUrl}${AUTHORING_PATH}`, {
      body: JSON.stringify(leg.document),
      token,
    });
    emit(`  http  status=${response.status} body=${response.text}`);
    leg.status = response.status;
    if (leg.expect === "authorized") assertAuthorized(check, leg, response);
    else assertRefused(check, response);
  }

  const authorized = LEGS.filter((leg) => leg.expect === "authorized");
  const refused = LEGS.filter((leg) => leg.expect !== "authorized");
  const last = authorized.at(-1);
  require_(
    "refusal-before-execution",
    refused.every(
      (leg) => leg.draftId === last.draftId && leg.expectedRevision === last.expectedRevision,
    ) && last.savedRevision === last.expectedRevision + 1,
    "the refused attempts did not target the revision the last authorized save consumed",
  );
  require_(
    "two-principal-attribution",
    authorized.length === 2 &&
      credentials[authorized[0].principal].subject !== credentials[authorized[1].principal].subject,
    "the authorized saves are not attributable to two distinct principals",
  );

  const manifest = {
    "run-id": runId,
    project: scope["project-id"],
    environment: scope.environment,
    "draft-id": draftId,
    "command-kind": SECTION,
    "must-appear": authorized.map((leg) => ({
      "command-id": leg.commandId,
      "principal-subject": credentials[leg.principal].subject,
      revision: leg.savedRevision,
    })),
    "must-not-appear": refused.map((leg) => ({ "command-id": leg.commandId, leg: leg.name })),
  };
  emit("audit manifest for the runner-side ledger read (this client never reads the ledger)");
  emit(`AUDIT-MANIFEST ${JSON.stringify(manifest)}`);
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
  emit("FAILED check=no-token-material-in-output detail=a transcript line carried token material");
  status = 1;
} else {
  emit("  ok    no-token-material-in-output");
}
emit(status === 0 ? "SMOKE PASS" : "SMOKE FAIL");
process.exitCode = status;
