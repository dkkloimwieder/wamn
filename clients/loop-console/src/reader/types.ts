/**
 * The read-surface domain the loop console renders.
 *
 * Shaped from the durable schema (`deploy/sql/run-state.sql`,
 * `deploy/sql/authoring-tests.sql`, `deploy/sql/catalog-schema.sql`) and from
 * the generated authoring read contract, so step 9 reconciles against stable
 * structures rather than invented DTOs. Every vocabulary below is the exact
 * set of persisted or wire literals. Fields the platform does not supply today
 * are marked STEP-9 RISK with what is missing.
 */

export type JsonValue =
  | null
  | boolean
  | number
  | string
  | readonly JsonValue[]
  | { readonly [key: string]: JsonValue };

// ── run ────────────────────────────────────────────────────────────────────

/**
 * Wire `RunStatus`. Deliberately the wire set, not the durable one: this is
 * what the console will receive. STEP-9 RISK — the two diverge. Durable is
 * `dispatched|running|completed|failed|infrastructure-failure|effect-uncertain`;
 * the wire invents `queued`, renames `completed` to `succeeded`, and drops
 * `infrastructure-failure` entirely even though assertions still assert on it.
 */
export type RunStatus =
  | "queued"
  | "dispatched"
  | "running"
  | "succeeded"
  | "failed"
  | "effect-uncertain";

/**
 * Wire `RunFailure` kind. STEP-9 RISK — the durable `FailKind` has 12 values;
 * the wire keeps 4 of them, adds `deadline-exhausted` (which is not durable on
 * a run at all, only on a test case), and gives the seven budget/resolution
 * kinds no wire form.
 */
export type RunFailureKind =
  | "invalid-input"
  | "retry-exhausted"
  | "deadline-exhausted"
  | "effect-uncertain"
  | "terminal";

export type RunFailure = {
  readonly kind: RunFailureKind;
  /** `runs.fail_node`. Not carried by the wire `RunFailure`. STEP-9 RISK. */
  readonly node: string | null;
  /** `runs.fail_reason`. Not carried by the wire `RunFailure`. STEP-9 RISK. */
  readonly detail: string | null;
  /** What the failure panel's `raw` disclosure shows. */
  readonly raw: JsonValue | null;
};

/** Run capture policy, `runs.capture_mode`. Two values — there is no `scrubbed` and no `preview`; `full` is what scrubs. */
export type CaptureMode = "full" | "off";

/**
 * Truthful execution-fact counts for the §2.2 header (`14 facts, all` /
 * `showing 200 of 3,412 · truncated`).
 *
 * STEP-9 RISK, and the first reconciliation item: no bounded-read contract
 * exists anywhere in the platform yet, and `RunProjection` carries no count,
 * total, cursor, or truncation flag. This envelope is deliberately reader-owned
 * and kept outside the platform-shaped projection rather than invented into it.
 * `truncated` is explicit rather than derived from `returned < total` so a
 * future contract that reports truncation without a total still fits.
 */
export type FactCount = {
  readonly returned: number;
  readonly total: number;
  readonly truncated: boolean;
};

/** `node_runs.status`. */
export type NodeRunStatus = "started" | "success" | "error";

/** `node_runs.error_kind`, and identically the assertion side's `NodeFailureKind`. */
export type NodeFailureKind = "retryable" | "rate-limited" | "terminal" | "invalid-input";

export type NodeFailure = {
  readonly kind: NodeFailureKind;
  /** `node_runs.error_detail`, flattened to the one sentence the note column shows. */
  readonly detail: string | null;
};

/**
 * One captured node input or output, in the five states step 4 renders.
 *
 * `present` / `too-large` / absent is the platform's own discriminator: output
 * json present, else output size present, else nothing captured. `errored` and
 * `outstanding` are the console's split of that last case — the node failed so
 * there is no output to show, versus the node is still `started` so no outcome
 * has been recorded yet — and are display distinctions the wire does not draw.
 * They must stay distinct: on an effect-uncertain run, rendering an outstanding
 * effect as errored asserts exactly the wrong conclusion (§2.2).
 * STEP-9 RISK for `errored` and `outstanding` only.
 */
export type CapturedValue =
  | { readonly state: "present"; readonly value: JsonValue }
  | { readonly state: "capture-off" }
  | { readonly state: "too-large"; readonly size: number; readonly hash: string | null }
  | { readonly state: "errored" }
  | { readonly state: "outstanding" };

/**
 * The CONTEXT lane for one fact.
 *
 * STEP-9 RISK, and the spec's own step-10 evidence item: per-node context does
 * not exist durably in any form — not before/write/after, not even a final-only
 * cell. Run context lives in memory and, transiently, inside the opaque
 * `runs.state_json`, which no read surface exposes. `absent` is therefore the
 * truthful mode today; `final-only` is the degraded single-cell mode the spec
 * describes, and `before-write-after` is the shape a real ctx capture would
 * fill. Context replacement is whole-document, never a merge, so `write` is a
 * complete document and `write: null` means the node left context unchanged.
 *
 * The mode says what ctx capture exists, which is a property of the run: every
 * fact of one run carries the same mode, or step 4's last-wins fold has nothing
 * to fold across.
 */
export type ContextLane =
  | { readonly mode: "absent" }
  | { readonly mode: "final-only"; readonly final: JsonValue }
  | {
      readonly mode: "before-write-after";
      readonly before: JsonValue;
      readonly write: JsonValue | null;
      readonly after: JsonValue;
    };

/**
 * One returned node run, in returned order.
 *
 * STEP-9 RISK, and the largest single gap: the wire `RunNodeProjection` is four
 * fields — `node-id`, `status` (a bare string), `failure-kind` (a bare string),
 * `output`. Node type, occurrence, frame, plan hash, timings, output port,
 * input, and attempt are all absent from it, so every one of them below is
 * reader-synthesized until the projection widens.
 */
export type ExecutionFact = {
  /** The `#` column: 1-based position in the returned order. */
  readonly index: number;
  /** `node_runs.local_node_id`. */
  readonly node: string;
  /**
   * The authored node type. An open string, not a closed enum: only `request`
   * and `event` are engine-reserved. Not persisted on `node_runs` at all — it
   * has to be joined from the flow definition by node id. STEP-9 RISK.
   */
  readonly nodeType: string;
  readonly status: NodeRunStatus;
  /**
   * `node_runs.output_port` — the branch taken, and the first ingredient of the
   * §2.2 note column. `main` and `error` are reserved; node types define their
   * own beyond those. Null when the node emitted no port.
   */
  readonly outputPort: string | null;
  readonly failure: NodeFailure | null;
  /**
   * Which visit of this node this fact is (0 = first). Stable across retries of
   * one visit — a retry never produces a second fact, because the primary key
   * collapses it — so two facts for one node id mean two visits, never a retry.
   */
  readonly occurrence: number;
  /** `node_runs.frame_id`; 0 is the root frame. Only root frames are produced today. */
  readonly frameId: number;
  /** Null exactly when `frameId` is the root frame. */
  readonly parentFrameId: number | null;
  /** `node_runs.current_plan_hash`, `sha256:<64 hex>`. */
  readonly planHash: string;
  /**
   * Retry attempt of this visit, 0 on first execution. The §2.2 note's `×4` is
   * `attempt + 1`. STEP-9 RISK — the attempt number has no durable home at all:
   * it lives only in the in-memory dispatch and possibly inside the opaque
   * `runs.state_json`. Null when unknown.
   */
  readonly attempt: number | null;
  /**
   * STEP-9 RISK — not derivable today. `node_runs.started_at` defaults to insert
   * time (after the node has finished) and `ended_at` is only ever written by
   * operator terminalization, so ordinary nodes have no duration fact.
   */
  readonly durationMs: number | null;
  readonly input: CapturedValue;
  readonly output: CapturedValue;
  readonly context: ContextLane;
};

/** `operator_run_actions.basis`. */
export type OperatorBasis = "external-evidence" | "counterparty-confirmation" | "operator-judgment";

/**
 * The operator resolution state of an effect-uncertain run. One-shot, not a
 * state machine: the platform allows exactly one action per run, ever.
 *
 * A terminalized run reads back as `status: "failed"` with the run failure kind
 * still `effect-uncertain` — deliberately not rewritten — plus
 * `terminal_reason = operator-terminalized-effect-uncertain`. That triple is the
 * resolved discriminator.
 *
 * STEP-9 RISK — no public surface reaches the terminalize action (every role is
 * revoked and no service calls it), and `runs.terminal_reason` has no CHECK
 * constraint, so its vocabulary is open with exactly one known member.
 */
export type OperatorResolution =
  | { readonly state: "unresolved" }
  | {
      readonly state: "operator-terminalized";
      readonly basis: OperatorBasis;
      readonly evidenceRef: string | null;
      readonly principal: string;
      readonly terminalReason: string;
      readonly at: string;
    };

/**
 * The one state the console must not let an author misread, so it is a value
 * rather than a flag on the run.
 */
export type EffectUncertainty = {
  /** The platform's meaning, verbatim. Never paraphrase these. */
  readonly meaning: readonly string[];
  readonly resolution: OperatorResolution;
};

/** `crates/execution/run-state/src/status.rs:26-28` and `:176-178`, verbatim. */
export const EFFECT_UNCERTAIN_MEANING: readonly string[] = [
  "An effectful attempt may have escaped without a durable outcome, so the platform cannot safely dispatch it again or claim whether it occurred.",
  "An effect may have escaped before its durable completion record was written, so repeating it would be unsafe.",
];

/** Identity of the draft a run executed, for the verdict bar's `draft orders@17 →`. */
export type DraftRef = {
  readonly draftId: string;
  readonly flowId: string;
  readonly revision: number;
};

export type Run = {
  readonly runId: string;
  readonly status: RunStatus;
  /** Present exactly when the run failed or is effect-uncertain. */
  readonly failure: RunFailure | null;
  readonly draft: DraftRef;
  /** W3C trace id. STEP-9 RISK — `RunProjection` carries no trace field. */
  readonly traceId: string;
  readonly capture: CaptureMode;
  readonly facts: readonly ExecutionFact[];
  readonly factCount: FactCount;
  /** Present exactly when the run is effect-uncertain, resolved or not. */
  readonly uncertainty: EffectUncertainty | null;
};

// ── report ─────────────────────────────────────────────────────────────────

/** The four assertion families. Externally tagged on the wire; one key per family. */
export type AssertionFamily =
  | "run-terminal-outcome"
  | "terminal-respond"
  | "typed-flow-failure"
  | "named-node-terminal";

/** Terminal statuses only — assertions cannot assert `dispatched` or `running`. */
export type RunTerminalStatus = "completed" | "failed" | "infrastructure-failure" | "effect-uncertain";

/** A strict subset of the durable 12 run fail kinds. */
export type FlowFailureKind =
  | "terminal"
  | "retry-exhausted"
  | "invalid-input"
  | "runaway-budget"
  | "effect-uncertain"
  | "depth-budget"
  | "dispatch-budget";

export type NodeTerminalStatus = "success" | "error";

/** The expected side of an assertion, one variant per family. */
export type Assertion =
  | { readonly family: "run-terminal-outcome"; readonly status: RunTerminalStatus }
  | { readonly family: "terminal-respond"; readonly status: number; readonly body: JsonValue }
  | { readonly family: "typed-flow-failure"; readonly kind: FlowFailureKind }
  | {
      readonly family: "named-node-terminal";
      readonly flowId: string;
      readonly nodeId: string;
      readonly status: NodeTerminalStatus;
      /** Forbidden when `status` is `success`, required when it is `error`. */
      readonly failureKind: NodeFailureKind | null;
    };

export type AssertionFailure = {
  readonly expected: Assertion;
  /**
   * The observed side is free text and only free text: the evaluator emits one
   * English sentence built with Rust debug formatting, and there is no
   * structured observed value anywhere. STEP-9 RISK — §2.3's `observed
   * check-stock → success` cannot be rendered as a typed pair.
   */
  readonly observed: string;
};

/** `authoring_test_case_runs.failure_kind`. */
export type CaseFailureKind =
  | "assertion-failed"
  | "deadline-exhausted"
  | "effect-uncertain"
  | "resolution-map-mismatch";

export type ReportCase = {
  readonly caseId: string;
  readonly passed: boolean;
  /**
   * The case's run, for §2.3's `run →`. STEP-9 RISK — the link is durable
   * (`authoring_test_case_runs.run_id`) but no wire field exposes it, so this
   * may not be wireable at all; null when unavailable.
   */
  readonly runId: string | null;
  /** Null exactly when the case passed. */
  readonly failureKind: CaseFailureKind | null;
  readonly failedAssertions: readonly AssertionFailure[];
};

/**
 * `authoring_test_run_reservations.state` / `authoring_test_reports`.
 *
 * STEP-9 RISK, three items. The pending projection is three fields
 * (`report-id`, `validated-draft`, `test-set`) and none of them is a reason, so
 * `pendingReason` is conservatively nullable and null in practice. The
 * finalized projection has no `cases` field at all — the case list has to come
 * out of the untyped `summary` object, whose shape is unspecified and whose
 * writer does not exist yet, which is the single biggest step-9 risk on this
 * screen. And it carries `passed` as a bare bool, not counts, so §2.3's
 * `N / M PASSED` verdict line — the screen's answer — has no wire source for
 * either number; both are reader-synthesized until the projection widens.
 */
export type Report =
  | {
      readonly state: "pending";
      readonly reportId: string;
      readonly testSetHash: string;
      readonly pendingReason: string | null;
    }
  | {
      readonly state: "finalized";
      readonly reportId: string;
      readonly testSetHash: string;
      readonly passed: number;
      readonly total: number;
      readonly cases: readonly ReportCase[];
    };

// ── draft ──────────────────────────────────────────────────────────────────

/**
 * The client's claim about its own working tree. The platform never runs git
 * and never verifies it; `dirty` means the content is descended from `commit`,
 * not equal to it.
 *
 * STEP-9 RISK — this is a write-side input that lands in
 * `authoring_command_audit`, not on the draft row, so `read-draft` cannot
 * return it. §2.4's provenance block is reader-synthesized.
 */
export type CommitProvenance = {
  readonly commit: string;
  readonly ref: string | null;
  readonly dirty: boolean;
};

export type DraftParse =
  | { readonly ok: true }
  /**
   * `line` is null when the engine reported no position — several V8 shapes and
   * every Firefox/Safari shape do. §2.4 then has no line to tint and its label
   * reads the message alone; it must never tint a guessed line.
   */
  | { readonly ok: false; readonly line: number | null; readonly message: string };

/**
 * STEP-9 RISK — the wire `DraftDocument` is two fields, `definition` and
 * `draft`. `savedAt` and `provenance` have no read-side source at all, and
 * `byteLength` is not stored (drafts have no `byte_length` column, unlike test
 * sets), so it is computed from the exact bytes.
 */
export type Draft = {
  readonly draftId: string;
  readonly flowId: string;
  readonly revision: number;
  readonly savedAt: string | null;
  /** UTF-8 byte count of `definition`, computed — never a code-unit count. */
  readonly byteLength: number;
  /** Exactly the bytes the client submitted, never reparsed by the platform. */
  readonly definition: string;
  readonly parse: DraftParse;
  readonly provenance: CommitProvenance | null;
};
