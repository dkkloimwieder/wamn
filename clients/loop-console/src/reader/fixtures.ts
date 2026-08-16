import { byteLength, parseDraft } from "./draft-text";
import {
  EFFECT_UNCERTAIN_MEANING,
  type ContextLane,
  type Draft,
  type DraftRef,
  type ExecutionFact,
  type JsonValue,
  type Report,
  type ReportCase,
  type Run,
} from "./types";

/**
 * Canned reads, one per screen state the console has to render.
 *
 * Ids reproduce the ones the spec's wireframes print, expanded into plausible
 * full ULIDs / trace ids / hashes so steps 4 and 5 can reproduce those
 * wireframes literally rather than approximately.
 *
 * The data is kept small enough to read. Two places where honesty and the
 * wireframe disagree, and what was chosen:
 *
 * - §2.2's header says `14 facts, all` above a five-row body that ends in a
 *   terminal failure. The five rows won: `failingRun` has exactly the facts the
 *   wireframe shows, so its header reads `5 facts, all`.
 * - §2.4's `4,812 bytes` is not the length of the source below. Byte length is
 *   computed from the exact bytes rather than asserted, so the fixture reports
 *   its own true size.
 */

/**
 * Frameless runs pin `current_plan_hash` to the run's execution bundle hash, so
 * the hash is per draft: runs of one draft share one, runs of another cannot.
 */
const PLAN_HASH = "sha256:4f2b8c1de703a95684bf2c07d1e93a5680c47b2fd9138ae06c25b7f4390d8e1c";
const CHARGES_PLAN_HASH = "sha256:e17d4a08b3c95f26d0a7b418c53e9d02f6417ba85c0d9e3721b64af08d5c3721";
const NOTIFICATIONS_PLAN_HASH =
  "sha256:0b96e2af41d78c530e6a1745b9c28df3061a5e94c7b2038df51ea6c93b7042d8";

const ordersDraft: DraftRef = { draftId: "orders", flowId: "orders", revision: 17 };
/** `charge-card` is not a node of the orders draft, so the charge runs execute their own. */
const chargesDraft: DraftRef = { draftId: "charges", flowId: "charges", revision: 3 };
/** Likewise `notify-subscriber` — and only a flow with a cycle can visit one node 200 times. */
const notificationsDraft: DraftRef = {
  draftId: "notifications",
  flowId: "notifications",
  revision: 2,
};

/**
 * A fact, with the fields that are the same everywhere defaulted so each fixture
 * shows only what distinguishes it: the root frame, the orders plan hash, first
 * visit, first attempt, the `main` port, and no context — context being absent
 * is the truthful default, since no per-node context exists durably.
 *
 * Merged field by field rather than spread over the defaults: a spread lets an
 * explicit `undefined` through and silently produces a fact that violates
 * `ExecutionFact`, which is the exact guarantee these fixtures exist to give.
 * `outputPort` and `attempt` test for `undefined` rather than using `??`,
 * because null is a state on both and must survive.
 */
const fact = (
  distinguishing: Partial<ExecutionFact> &
    Pick<ExecutionFact, "index" | "node" | "nodeType" | "status" | "input" | "output">,
): ExecutionFact => ({
  index: distinguishing.index,
  node: distinguishing.node,
  nodeType: distinguishing.nodeType,
  status: distinguishing.status,
  input: distinguishing.input,
  output: distinguishing.output,
  outputPort: distinguishing.outputPort === undefined ? "main" : distinguishing.outputPort,
  attempt: distinguishing.attempt === undefined ? 0 : distinguishing.attempt,
  failure: distinguishing.failure ?? null,
  occurrence: distinguishing.occurrence ?? 0,
  frameId: distinguishing.frameId ?? 0,
  parentFrameId: distinguishing.parentFrameId ?? null,
  planHash: distinguishing.planHash ?? PLAN_HASH,
  durationMs: distinguishing.durationMs ?? null,
  context: distinguishing.context ?? { mode: "absent" },
});

const captureOff = { state: "capture-off" } as const;
const orderPayload = { order: { sku: "SKU-118", quantity: 2 } };
const parsedOrder = { sku: "SKU-118", quantity: 2 };

// ── runs ───────────────────────────────────────────────────────────────────

export const PASSING_RUN_ID = "01J9W8ND3RM5QT2XZC7B4V6H0P";
/** §2.2's `run 01J9…3F2K`. */
export const FAILING_RUN_ID = "01J9X4T7QW3B8ZC5NDR6MK3F2K";
/** §2.0's `◌ 01J9…KV07`. */
export const EFFECT_UNCERTAIN_RUN_ID = "01J9Z6M2QB4X9TC7NDR5HPKV07";
export const TERMINALIZED_RUN_ID = "01J9T3MB8QZ5ND7XC2RW4V0G6E";
export const TRUNCATED_RUN_ID = "01J9Y2QB7ND4XM5ZC3RT8W9K1S";
export const CAPTURE_OFF_RUN_ID = "01J9V5RT9ZB2XM8ND4QC6W3J7A";

/** The degraded single-cell CONTEXT mode: one final document, no before/write/after. */
const finalOnlyContext: ContextLane = {
  mode: "final-only",
  final: { order: "ord-4471", reserved: true },
};

/**
 * One fact's slice of a run-wide fold: what context the node saw, and what it
 * replaced it with. `write: null` is a node that left context unchanged, so
 * `after` is `before` — which is step 4's last-wins fold, one step at a time.
 * The mode is per run: a run in this mode carries it on every fact.
 */
const ctx = (before: JsonValue, write: JsonValue | null): ContextLane => ({
  mode: "before-write-after",
  before,
  write,
  after: write === null ? before : write,
});

export const passingRun: Run = {
  runId: PASSING_RUN_ID,
  status: "succeeded",
  failure: null,
  draft: ordersDraft,
  traceId: "9c3a7e18d54b2f60ae81c73b9d02f645",
  capture: "full",
  factCount: { returned: 4, total: 4, truncated: false },
  uncertainty: null,
  facts: [
    fact({
      index: 1,
      node: "ingress",
      nodeType: "request",
      status: "success",
      durationMs: 2,
      input: { state: "present", value: { method: "POST", path: "/orders" } },
      output: { state: "present", value: orderPayload },
      context: finalOnlyContext,
    }),
    fact({
      index: 2,
      node: "parse-order",
      nodeType: "transform",
      status: "success",
      durationMs: 1,
      input: { state: "present", value: orderPayload },
      output: { state: "present", value: parsedOrder },
      context: finalOnlyContext,
    }),
    fact({
      index: 3,
      node: "reserve",
      nodeType: "postgres",
      status: "success",
      durationMs: 41,
      input: { state: "present", value: parsedOrder },
      // Over the write-side output ceiling: the row keeps a size and a hash, no value.
      output: { state: "too-large", size: 98_304, hash: "9f31c0a74be2d586" },
      context: finalOnlyContext,
    }),
    fact({
      index: 4,
      node: "respond",
      nodeType: "respond",
      status: "success",
      durationMs: 1,
      input: { state: "present", value: { status: 201 } },
      output: { state: "present", value: { status: 201, body: { reserved: true } } },
      context: finalOnlyContext,
    }),
  ],
};

/**
 * §2.2, literally: a taken branch (fact 3), a context write (fact 2), and a
 * retried effectful node that appears as two facts (facts 4 and 5).
 *
 * This is the run in the `before-write-after` context mode, on every fact, so
 * step 4's last-wins fold has a whole run to fold across: one node writes and
 * the rest carry `write: null`, the state that means context was left alone.
 *
 * Those two facts are two *visits*, not two retries. A retry never produces a
 * second row — occurrence is stable across retries of one visit and the primary
 * key collapses them — so the wireframe's `occurrence 1–4` is unproducible. The
 * four attempts live on fact 4's `attempt`, which is where §2.2's `retryable ×4`
 * comes from.
 */
export const failingRun: Run = {
  runId: FAILING_RUN_ID,
  status: "failed",
  failure: {
    kind: "retry-exhausted",
    node: "fetch-inventory",
    detail: "upstream 503 after 4 attempts",
    raw: {
      kind: "retry-exhausted",
      node: "fetch-inventory",
      attempts: 4,
      last: { status: 503, upstream: "inventory.internal" },
    },
  },
  draft: ordersDraft,
  traceId: "4bf9c1e7a2d84f6b93c05a17be24d8f3",
  capture: "full",
  factCount: { returned: 5, total: 5, truncated: false },
  uncertainty: null,
  facts: [
    fact({
      index: 1,
      node: "ingress",
      nodeType: "request",
      status: "success",
      durationMs: 2,
      input: { state: "present", value: { method: "POST", path: "/orders" } },
      output: { state: "present", value: orderPayload },
      context: ctx({}, null),
    }),
    fact({
      index: 2,
      node: "parse-order",
      nodeType: "transform",
      status: "success",
      durationMs: 1,
      input: { state: "present", value: orderPayload },
      output: { state: "present", value: parsedOrder },
      // The one context write of the run. Replacement is whole-document, never a merge.
      context: ctx({}, { order: parsedOrder }),
    }),
    fact({
      index: 3,
      node: "check-stock",
      nodeType: "conditional",
      status: "success",
      // The taken branch. §2.2's note reads `→ in-stock`.
      outputPort: "in-stock",
      durationMs: 1,
      input: { state: "present", value: parsedOrder },
      output: { state: "present", value: { sku: "SKU-118", available: 0 } },
      context: ctx({ order: parsedOrder }, null),
    }),
    fact({
      index: 4,
      node: "fetch-inventory",
      nodeType: "http-request",
      status: "error",
      outputPort: "error",
      failure: { kind: "retryable", detail: "upstream 503" },
      // Fourth attempt of the first visit — §2.2's `retryable ×4` is attempt + 1.
      attempt: 3,
      durationMs: 1204,
      input: { state: "present", value: { sku: "SKU-118" } },
      output: { state: "errored" },
      context: ctx({ order: parsedOrder }, null),
    }),
    fact({
      index: 5,
      node: "fetch-inventory",
      nodeType: "http-request",
      status: "error",
      outputPort: "error",
      failure: { kind: "terminal", detail: "inventory service refused the reservation" },
      // The second visit of the same node, not a retry of the first.
      occurrence: 1,
      durationMs: 318,
      input: { state: "present", value: { sku: "SKU-118" } },
      output: { state: "errored" },
      context: ctx({ order: parsedOrder }, null),
    }),
  ],
};

const charge = { amount: 4990, currency: "USD" };

/** The two facts an effect-uncertain run stops on: the effectful row is still `started`. */
const uncertainFacts: readonly ExecutionFact[] = [
  fact({
    index: 1,
    node: "ingress",
    nodeType: "request",
    status: "success",
    planHash: CHARGES_PLAN_HASH,
    durationMs: 2,
    input: { state: "present", value: { method: "POST", path: "/charges" } },
    output: { state: "present", value: charge },
  }),
  fact({
    index: 2,
    node: "charge-card",
    nodeType: "http-request",
    // Outstanding: the attempt may have escaped, so nothing was ever recorded.
    status: "started",
    outputPort: null,
    planHash: CHARGES_PLAN_HASH,
    // Nothing durable was written for this visit, so not even the attempt number
    // survives — the one place the STEP-9 attempt gap is visibly null.
    attempt: null,
    input: { state: "present", value: charge },
    // Not `errored`: the node has not failed, its outcome is precisely unknown.
    // §2.2 exists here to stop a wrong conclusion, so the output slot says so.
    output: { state: "outstanding" },
  }),
];

export const effectUncertainRun: Run = {
  runId: EFFECT_UNCERTAIN_RUN_ID,
  status: "effect-uncertain",
  failure: { kind: "effect-uncertain", node: "charge-card", detail: null, raw: null },
  draft: chargesDraft,
  traceId: "7d18b4e0c92a35f6481be27ac05d9f38",
  capture: "full",
  factCount: { returned: 2, total: 2, truncated: false },
  uncertainty: { meaning: EFFECT_UNCERTAIN_MEANING, resolution: { state: "unresolved" } },
  facts: uncertainFacts,
};

/**
 * The other half of the uncertain panel: the same run after an operator
 * terminalized it. The panel has two resolution states, so clause (b) of the
 * done-check needs a fixture for each — this is the sixth run fixture, beyond
 * step 2's list of five, for that reason alone.
 *
 * Note the resolved triple: the status flips to `failed`, the run failure kind
 * stays `effect-uncertain` (deliberately never rewritten), and a terminal reason
 * appears.
 */
export const terminalizedRun: Run = {
  runId: TERMINALIZED_RUN_ID,
  status: "failed",
  failure: { kind: "effect-uncertain", node: "charge-card", detail: null, raw: null },
  draft: chargesDraft,
  traceId: "e04a7b26c918d35f7a2be40c95d18f37",
  capture: "full",
  factCount: { returned: 2, total: 2, truncated: false },
  uncertainty: {
    meaning: EFFECT_UNCERTAIN_MEANING,
    resolution: {
      state: "operator-terminalized",
      basis: "counterparty-confirmation",
      evidenceRef: "ops/2026-08-14/psp-settlement-report.csv",
      principal: "wamn_project_admin",
      terminalReason: "operator-terminalized-effect-uncertain",
      at: "2026-08-14T11:41:02Z",
    },
  },
  facts: [
    uncertainFacts[0],
    {
      ...uncertainFacts[1],
      status: "error",
      failure: { kind: "terminal", detail: "operator terminalized unresolved effect" },
      // Terminalized, so the row has failed and its outcome is no longer outstanding.
      output: { state: "errored" },
    },
  ],
};

/**
 * 200 returned facts out of 3,412, so §2.2's `showing 200 of 3,412 · truncated`
 * header counts a real list rather than making a claim. They are 200 visits of
 * one node in a loop — occurrences, which is the only way one node id can
 * produce many facts.
 */
export const truncatedRun: Run = {
  runId: TRUNCATED_RUN_ID,
  status: "succeeded",
  failure: null,
  draft: notificationsDraft,
  traceId: "2ae64c908b17d3f5029ce4b86a17d0f9",
  capture: "full",
  factCount: { returned: 200, total: 3412, truncated: true },
  uncertainty: null,
  facts: Array.from({ length: 200 }, (_, index) =>
    fact({
      index: index + 1,
      node: "notify-subscriber",
      nodeType: "http-request",
      status: "success",
      planHash: NOTIFICATIONS_PLAN_HASH,
      occurrence: index,
      durationMs: 12,
      input: { state: "present", value: { subscriber: index } },
      output: { state: "present", value: { delivered: true } },
    }),
  ),
};

/** Capture `off` blanks every capture column, input and output alike. */
export const captureOffRun: Run = {
  runId: CAPTURE_OFF_RUN_ID,
  status: "succeeded",
  failure: null,
  draft: ordersDraft,
  traceId: "b5309cf7e21a48d6903fbc25e7481a0d",
  capture: "off",
  factCount: { returned: 3, total: 3, truncated: false },
  uncertainty: null,
  facts: [
    fact({
      index: 1,
      node: "ingress",
      nodeType: "request",
      status: "success",
      input: captureOff,
      output: captureOff,
    }),
    fact({
      index: 2,
      node: "reserve",
      nodeType: "postgres",
      status: "success",
      input: captureOff,
      output: captureOff,
    }),
    fact({
      index: 3,
      node: "respond",
      nodeType: "respond",
      status: "success",
      input: captureOff,
      output: captureOff,
    }),
  ],
};

export const runFixtures: ReadonlyMap<string, Run> = new Map(
  [passingRun, failingRun, effectUncertainRun, terminalizedRun, truncatedRun, captureOffRun].map(
    (run) => [run.runId, run],
  ),
);

// ── reports ────────────────────────────────────────────────────────────────

/** §2.3's `report 01J9…8Q11`. */
export const FINALIZED_REPORT_ID = "01J9T5C8NB2XQ7ZD4RM9KV8Q11";
/** §2.0's `✓ 01J8…MM04`, the tree's all-pass report. */
export const ALL_PASS_REPORT_ID = "01J8Q4RD7NBXC9ZT5KW3HPMM04";
export const PENDING_REPORT_ID = "01J9R7ZC4NM2QB9XD5TW3H5P2D";

/** §2.3's `test-set a41c…`. */
const TEST_SET_HASH = "sha256:a41c7d2e9b8f0a35c6d14e7b2f809a63d5c8e14700b93a2f6d8c05e71a4b39f2";

/**
 * Passing cases keep a run id so §2.3's `run →` is present on every row, but
 * only the failing case points at a run this reader can serve — that is the §5
 * acceptance journey, report to run.
 */
const caseRunId = (ordinal: number): string =>
  `01J9CB3XQ7ZD4RM9KV2NTW${String(ordinal).padStart(4, "0")}`;

const passingCase = (caseId: string, ordinal: number): ReportCase => ({
  caseId,
  passed: true,
  runId: caseRunId(ordinal),
  failureKind: null,
  failedAssertions: [],
});

/**
 * The failing case carries one failure per assertion family, so every family's
 * rendering has a fixture. The observed side is free text on purpose: the
 * evaluator produces one English sentence built with Rust debug formatting, and
 * there is no structured observed value to render instead.
 */
const failingCase: ReportCase = {
  caseId: "backorders-when-out-of-stock",
  passed: false,
  runId: FAILING_RUN_ID,
  failureKind: "assertion-failed",
  failedAssertions: [
    {
      expected: {
        family: "named-node-terminal",
        flowId: "orders",
        nodeId: "check-stock",
        status: "error",
        failureKind: "terminal",
      },
      observed: 'flow/node pair ("orders", "check-stock") had 1 mismatching occurrence(s) out of 1',
    },
    {
      expected: { family: "run-terminal-outcome", status: "completed" },
      observed: "run terminal status Failed did not match Completed",
    },
    {
      expected: { family: "typed-flow-failure", kind: "retry-exhausted" },
      observed: "no typed flow failure was captured",
    },
    {
      expected: {
        family: "terminal-respond",
        status: 201,
        body: { created: true, backordered: false },
      },
      observed:
        'terminal response TerminalRespond { status: 500, body: Object {"error": String("inventory unavailable")} } did not match TerminalRespond { status: 201, body: Object {"created": Bool(true), "backordered": Bool(false)} }',
    },
  ],
};

export const finalizedReport: Report = {
  state: "finalized",
  reportId: FINALIZED_REPORT_ID,
  testSetHash: TEST_SET_HASH,
  passed: 11,
  total: 12,
  cases: [
    passingCase("creates-order", 1),
    passingCase("rejects-empty-cart", 2),
    failingCase,
    passingCase("reserves-single-item", 3),
    passingCase("splits-multi-warehouse", 4),
    passingCase("honours-idempotency-key", 5),
    passingCase("rejects-unknown-sku", 6),
    passingCase("applies-tax-table", 7),
    passingCase("emits-order-created", 8),
    passingCase("responds-201-on-create", 9),
    passingCase("retries-transient-inventory", 10),
    passingCase("releases-caller-on-respond", 11),
  ],
};

/**
 * The other finalized verdict: every case passed, so §2.3's verdict line reads
 * `12 / 12 PASSED` and the bar renders `ok` — the third of the screen's three
 * verdict renderings, and §2.0's `✓` tree entity.
 *
 * Its last case carries no run id. The case→run link is durable but no wire
 * field exposes it, so null is the state step 9 will actually hit and §2.3's
 * `run →` has to degrade on a passing row rather than break.
 */
export const allPassReport: Report = {
  state: "finalized",
  reportId: ALL_PASS_REPORT_ID,
  testSetHash: TEST_SET_HASH,
  passed: 12,
  total: 12,
  cases: [
    passingCase("creates-order", 12),
    passingCase("rejects-empty-cart", 13),
    passingCase("reserves-single-item", 14),
    passingCase("splits-multi-warehouse", 15),
    passingCase("honours-idempotency-key", 16),
    passingCase("rejects-unknown-sku", 17),
    passingCase("applies-tax-table", 18),
    passingCase("emits-order-created", 19),
    passingCase("responds-201-on-create", 20),
    passingCase("retries-transient-inventory", 21),
    passingCase("releases-caller-on-respond", 22),
    {
      caseId: "backorders-when-out-of-stock",
      passed: true,
      runId: null,
      failureKind: null,
      failedAssertions: [],
    },
  ],
};

/**
 * A reservation that has not finalized. `pendingReason` is null because there is
 * no reason to render: the pending projection is three fields and none of them
 * carries one.
 */
export const pendingReport: Report = {
  state: "pending",
  reportId: PENDING_REPORT_ID,
  testSetHash: "sha256:b7e309a4c81d5f2609ba734ce15d82073fc8d0165ae294b7d0c8351f6b29ae04",
  pendingReason: null,
};

export const reportFixtures: ReadonlyMap<string, Report> = new Map(
  [finalizedReport, allPassReport, pendingReport].map((report) => [report.reportId, report]),
);

// ── drafts ─────────────────────────────────────────────────────────────────

/**
 * The half-finished edit §2.4 describes: `"schema-version"` on line 41 lost its
 * colon. The platform stores it exactly as submitted rather than refusing it,
 * which is the whole point of the source tab.
 */
const draftSourceRevision16 = `{
  "nodes": [
    { "id": "ingress", "type": "request" },
    {
      "id": "parse-order",
      "type": "transform",
      "config": {
        "expr": "body.order",
        "ctx": { "order": "body.order" }
      }
    },
    {
      "id": "check-stock",
      "type": "conditional",
      "config": { "expr": "ctx.order.quantity > 0" }
    },
    {
      "id": "fetch-inventory",
      "type": "http-request",
      "config": {
        "method": "GET",
        "url": "https://inventory.internal/v1/levels",
        "retry": { "max": 4 }
      }
    },
    {
      "id": "reserve",
      "type": "postgres",
      "config": { "statement": "select reserve($1, $2)" }
    },
    { "id": "respond", "type": "respond" }
  ],
  "edges": [
    { "from": "ingress", "to": "parse-order" },
    { "from": "parse-order", "to": "check-stock" },
    { "from": "check-stock", "from-port": "in-stock", "to": "reserve" },
    { "from": "check-stock", "from-port": "backorder", "to": "fetch-inventory" },
    { "from": "fetch-inventory", "to": "reserve" },
    { "from": "reserve", "to": "respond" }
  ],
  "schema-version" "0.1",
  "flow-id": "orders",
  "version": 3,
  "name": "orders"
}
`;

/** Revision 17 is revision 16 with the missing colon restored — the next save. */
const draftSourceRevision17 = draftSourceRevision16.replace(
  '"schema-version" "0.1"',
  '"schema-version": "0.1"',
);

/** Byte length and parse verdict are derived from the bytes, never asserted beside them. */
const draft = (fields: Omit<Draft, "byteLength" | "parse">): Draft => ({
  ...fields,
  byteLength: byteLength(fields.definition),
  parse: parseDraft(fields.definition),
});

/** §2.4's `orders @ 17`, and the `draft orders@17 →` every run's verdict bar links to. */
export const parsingDraft: Draft = draft({
  draftId: "orders",
  flowId: "orders",
  revision: 17,
  savedAt: "2026-08-14T09:12:00Z",
  definition: draftSourceRevision17,
  provenance: {
    commit: "7c1f0a93b8e254d6a03fbc719e2d845067ab31cf",
    ref: "refs/heads/main",
    dirty: false,
  },
});

/** §2.0's `orders@16`: does not parse, and dirty — content descended from the commit. */
export const nonParsingDraft: Draft = draft({
  draftId: "orders",
  flowId: "orders",
  revision: 16,
  savedAt: "2026-08-14T08:57:00Z",
  definition: draftSourceRevision16,
  provenance: {
    commit: "2d84b06fe7a1359c48db20f6ac57193ed80b4a2f",
    ref: null,
    dirty: true,
  },
});

/**
 * The charge flow the two uncertain runs execute, so `charge-card`'s node type
 * has a definition to be joined from. Saved-at and provenance are null because
 * that is what a real read returns: neither has any read-side source, so §2.4's
 * header line and provenance block have to render their absence.
 */
export const unattributedDraft: Draft = draft({
  draftId: "charges",
  flowId: "charges",
  revision: 3,
  savedAt: null,
  definition: `{
  "nodes": [
    { "id": "ingress", "type": "request" },
    {
      "id": "charge-card",
      "type": "http-request",
      "config": { "method": "POST", "url": "https://psp.internal/v1/charges" }
    },
    { "id": "respond", "type": "respond" }
  ],
  "edges": [
    { "from": "ingress", "to": "charge-card" },
    { "from": "charge-card", "to": "respond" }
  ],
  "schema-version": "0.1",
  "flow-id": "charges",
  "version": 3,
  "name": "charges"
}
`,
  provenance: null,
});

/** The fan-out flow the truncated run loops in — a second flow, so §2.0's DRAFTS group. */
export const loopingDraft: Draft = draft({
  draftId: "notifications",
  flowId: "notifications",
  revision: 2,
  savedAt: "2026-08-13T16:04:00Z",
  definition: `{
  "nodes": [
    { "id": "ingress", "type": "event" },
    {
      "id": "notify-subscriber",
      "type": "http-request",
      "config": { "method": "POST", "url": "https://hooks.internal/v1/notify" }
    },
    { "id": "next-subscriber", "type": "conditional", "config": { "expr": "ctx.remaining > 0" } }
  ],
  "edges": [
    { "from": "ingress", "to": "notify-subscriber" },
    { "from": "notify-subscriber", "to": "next-subscriber" },
    { "from": "next-subscriber", "from-port": "more", "to": "notify-subscriber" }
  ],
  "schema-version": "0.1",
  "flow-id": "notifications",
  "version": 2,
  "name": "notifications"
}
`,
  provenance: {
    commit: "b41e07d29a5f836c0d7be145392fac8017d6b035",
    ref: "refs/heads/main",
    dirty: false,
  },
});

export function draftKey(draftId: string, revision: number): string {
  return `${draftId}@${revision}`;
}

/**
 * Keyed by id and revision, because the route addresses both. The durable store
 * keeps only the head revision of a draft, so serving two revisions of `orders`
 * at once is a fixture affordance that lets §2.0's tree render.
 */
export const draftFixtures: ReadonlyMap<string, Draft> = new Map(
  [parsingDraft, nonParsingDraft, unattributedDraft, loopingDraft].map((entry) => [
    draftKey(entry.draftId, entry.revision),
    entry,
  ]),
);
