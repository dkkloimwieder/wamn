# WAMN platform and application testing

Status: accepted 0.2 · 2026-09-09 · Increment 1 owned by `wamn-10yt.77`. Increment 2 tracked in `wamn-10yt.78`  
**Scope:** executable tests for platform guarantees and application business rules; Rust-only initial delivery.  
Relationship: this accepted delivery contract refines the deterministic-testing proposal and application-testing procedure. It does not report implementation status. Existing product contracts remain authoritative. [1–4]

## 1. Decision

Use ordinary tests, **Proptest**, deterministic simulation testing (**DST**), and real integration tests together. An **invariant** is a rule at a stated observation boundary. Property tests generate inputs and histories; simulation controls time, outcomes, and event order where the harness owns them. Neither replaces database and deployment tests. Passing sampled histories is not proof over all possible histories.

Kani remains outside this delivery. The owner accepted this deferral on 2026-09-09. The earlier numeric-bug trigger does not reopen Kani work. Reopening requires an explicit owner decision. Do not add Kani dependencies, harnesses, or toolchain work. Coverage-guided fuzzing, Bolero, and production invariant tripwires remain deferred. [1]

Application testing advances alongside platform testing. It does **not** wait for a complete deterministic simulation framework. Add no production effect capture, replay ledger, database extension, policy bypass, or general test-definition language.

## 2. Ownership and invariant contract

**Platform-owned:** invocation, disposable environments, controlled dependencies, generators, failure reporting, and gate integration. **Application-owned:** fixtures, commands, expected results/refusals, business invariants, and workflows. Extend existing test support, not a parallel runtime. [1, 3]

Each critical invariant records an ID, rule, affected state, observation boundary, enforcing code/constraint, and named tests. Use the application brief and ordinary test code, not another registry. [4]

Distinguish three kinds of assertion:

- **State:** quantities stay within limits on a coherent committed snapshot.
- **Transition/history:** only permitted changes occur; claim-backed replay preserves the original result.
- **Eventual outcome:** post-commit work reaches a result within stated time bounds and recovery assumptions—not immediately after the source transaction.

A confirmed pre-commit refusal leaves the **named business state** unchanged; list permitted claim/audit changes separately. Unknown completion is not a refusal.

## 3. Test layers and their limits

| Layer | Execute | Establishes / does not establish |
|---|---|---|
| **Build verification** | The complete generated and authored application SQL corpus through the shared SQLx verification stage; compile components and contracts. | SQL/type and build-contract checks. Not data-dependent business behavior, rollback, or concurrency. [2] |
| **Pure decisions** | Real application/platform functions with examples and Proptest inputs. | Calculations, transitions, validation, and boundary refusals. Do not copy production logic into a test-only implementation. |
| **Command histories** | Generated sequences against real command implementations; component-backed database tests through production capability and transaction paths. | Results and business state agree with an independently expressed, small reference model. Native-only execution does not test the guest boundary. |
| **Controlled guest scenarios** | Real guests with test-controlled effects, clocks, and randomness where needed. | Repeatable behavior for the modeled scenario. Recorded answers do not establish database or external-service correctness. |
| **Live integration and contention** | Real PostgreSQL, authenticated routes, and relevant broker/process boundaries. | Transactions, locking, constraints, privileges/RLS, cleanup, delivery, and served behavior under the exercised failures. A seed does not control PostgreSQL’s internal scheduling. |

Selected public commands require component-backed and served-route coverage; private operations use their actual caller/ingress. Reuse generated claim-law tests, but also execute the full command. [2, 3]

## 4. Application test requirements

### Inputs and histories

Use the existing Proptest strategies/`Arbitrary` pattern. Generate valid and invalid commands: numeric boundaries, absent/null/value, missing records, disallowed statuses, stale revisions, repeated keys, and changed bodies under a key. Reach the invariant’s actual boundaries; retain explicit examples alongside generated cases.

Start from a validated fixture. Execute a bounded history, compare results with a small independent model, and inspect committed state after settled operations. Do not use the production validator as the expected-answer function or translate SQL into a replacement Rust implementation merely to test it.

Shrink failures without losing fixture dependencies or changing the failure being tested. Retain minimized regressions; use stable test identities rather than incidental generated IDs.

### Database and authority

Use exact migrations/effective schema and production-equivalent application identity, grants, and RLS. Separate fixture/read-only observer authority from command execution. Setup writes are explicit; tested histories and portfolio traffic mutate through real operations. [2–4]

Check state independently of the response, using coherent snapshots for multi-table rules. Keep one transaction per command item, never across wiring edges; test mixed-success outer envelopes where applicable. [2, 3]

Run competing transactions on separate real connections. Establish overlap with bounded barriers or observed database waits, not sleeps. Verify contention and final state; require no particular winner unless specified.

### Failure and replay behavior

Distinguish **before commit**, **commit with lost response**, and **downstream failure after commit**. Show which boundary the control reached. A timeout alone does not establish this.

Apply the declared replay contract: claim-backed calls recover the original result; state/revision calls need not. Inner-command idempotency does not make a composed route safe to replay. Add no automatic mutation retries. [3, 5]

UI tests use the same refusal/partial-completion/uncertainty evidence. A refused retry cannot erase earlier uncertainty—the existing review clarification, not a new outcome taxonomy.

## 5. Deterministic testing boundary

Preserve the existing D1 walk simulator and D8a invariant checks; expand their generated domains when a defect exposes a gap. Continue D2b/D2 under their existing owners: controlled time and event order over real run-state SQL. Application tests do not depend on their completion. [1]

Implement D3–D5 only far enough to serve a named guest test. Recording uses synthetic data in the test process, never production data or credentials; replay never reaches a database or network. Adapters remain in test support, with no production capture feature. Validate request identity, parameters, multiplicity, and required ordering; unmatched requests or unused required fixture entries fail. Fix guest clocks and randomness when they affect assertions. Fixture changes require explicit re-recording and review. [1]

Record inputs/history, fixture, generator configuration, source/artifact identity, and schema/toolchain versions—not only the seed. Identical replay applies to controlled execution; live races record observed scheduling without promising identical repetition.

Accepted clarification to [1]: D2 can replace redundant sequential crash scenarios. A simulated crash does not automatically retire real process-kill, connection-loss, or contention tests. Retire a test only when another test covers its actual guarantee.

## 6. Application slices: Receiving and its overlay

### Increment 1 — Receiving command

Bead `wamn-10yt.77` owns this increment. Completion requires executed evidence for every required case below.

Use the existing `receiving.record_receipt` contract, not a new sample application. Its declared transaction validates status and remaining quantity, writes receipt state, and applies idempotency. The following are required tests, not claims of existing coverage. [3]

| Case | Required observation |
|---|---|
| **Generated receipt histories** | Accepted receipts accumulate correctly; quantities never exceed the order; status, revisions, and receipt history agree with committed state. |
| **Boundary and refusal** | Exact remaining quantity succeeds; excess and invalid status refuse without partial business writes. |
| **Repeated intent** | Same key/body returns the original receipt without posting again; changed body under that key refuses. Also exercise overlapping same-key calls. |
| **Competing receipts** | Two individually valid quantities whose sum exceeds remaining quantity cannot both commit; verify the resulting refusal and final totals. |
| **Intermediate failure** | Fail after a confirmed intermediate write but before commit; all receipt business writes roll back. |
| **Lost response** | Commit is independently confirmed, response delivery is suppressed, and permitted captured retry returns the original result without a second receipt. |

Include the existing revision-bearing `purchase_order.update` in the history suite for stale-revision behavior. Pair an authorized call with a denied call and verify no unauthorized business mutation. Test application state through the actual guest and capability boundary. [3]

Detect and reproduce a deliberate business defect, including a contention-protection mutant, under §7’s existing mutation rules. If another constraint still protects the invariant, record that surviving mutant rather than demand unsafe behavior.

### Increment 2 — unchanged overlay and post-commit work

**Compatibility is fresh-install-only.** For each comparison, use two independent disposable installations: base A + overlay O, and candidate base B + the **same overlay artifacts**. Exercise additive and breaking candidates separately. This tests schema-contract compatibility, not an in-place upgrade or preservation of existing installation data. Do not apply a migration suffix to an installed predecessor, activate an upgrade candidate, or reopen the lifecycle freeze. [3, 7]

| Case | Required observation |
|---|---|
| **Additive base** | Both fresh installations satisfy the unchanged overlay’s required schema contract; its named operations work against fresh fixtures and its artifact digests remain unchanged. |
| **Breaking base** | The second fresh-install combination refuses with the named unsatisfied schema requirement or ownership conflict; an unrelated build failure is not the compatibility result. |
| **Duplicate committed event** | Commit a receipt through the real command, observe its event, and redeliver the same source identity through the registered materializer path after the first handler completion. Make sure that the handler receives it again. Make sure that `quality.create_inspection` leaves one inspection for that receipt, without resetting its business state. A distinct valid receipt event creates its own inspection. [3, 8] |
| **Poison event and progress** | Deliver a routable poison event to the same registration, followed by a valid independent receipt event. Under the existing bounded retry/DLQ policy, observe the poison’s correlated dead-letter record and the valid event’s inspection within the declared test bound. No indefinite blockage and no fabricated DLQ row. [6, 9] |

These eventual-outcome tests use the actual broker/materializer and private handler, not a new public route. Record observed deliveries, source/registration identity, configured retry and time bounds, and settled business/DLQ state. Publishing twice is insufficient if broker deduplication prevents the second handler delivery. State the recovery assumptions; bounded progress does not promise zero delay while poison handling runs. **One inspection per receipt is application idempotency, not a platform-wide exactly-once guarantee.** [3, 6, 8, 9]

## 7. Execution, evidence, and incremental delivery

Use existing Cargo suites, package/dev-loop gates, and named live recipes. Pure tests run in the normal sweep, including the component workspace. Live histories run with declared finite case, step, and timeout limits on disposable state. Required legs that execute zero cases, lack infrastructure, or are ignored are **not passed**. Record them separately; a partial result cannot complete this slice. Do not change repository-wide merge policy implicitly.

Report invariant IDs, source/component/corpus identity, fixture/history, executed counts, failures, and reproduction commands. Critical invariants need positive, refusal/failure, and deliberate-defect evidence. This is a suite-level requirement; the pilot’s separate grading rubric is unchanged. Preserve unrelated failures. [1, 4]

**Mutation and assertion discipline is inherited, not redefined:** `docs/operations/build-and-test.md` → **Traps**, especially **“Score a mutant by the proof’s EXIT CODE”**, **“A mutant scores killed only when the proof ran to completion and failed on the mutated property”**, **“a mutant must prove it LANDED”**, **“Every guard’s proof carries a negative control”** / **“the control has to ISOLATE”**, the **unguessable fixture values** rule, and **“A test must assert the DISTINGUISHING STEP”**. The suite’s recipes reference these laws and retain their required evidence. [6]

| Increment | Delivery and exit |
|---|---|
| **1 — Receiving end to end** | Implement §6’s Increment 1 command cases through existing support. Exit: real-command histories, rollback, replay, authority, and controlled contention execute; a business mutant is detected and reproduced. No full DST prerequisite. |
| **2 — Overlay reuse** | Execute §6’s fresh-install compatibility pairs and materializer duplicate/poison/progress cases. Keep the overlay artifacts unchanged and invariants application-owned; extract only demonstrated shared helpers. No in-place update or lifecycle work. [3, 7] |
| **3 — Targeted simulation** | Extend platform scheduling or guest replay under existing D1–D5 work only where it closes a named coverage gap. Keep the live tests for unmodeled behavior. |

Respect existing platform-wave fences. This specification governs reconciliation with the older proposal and authoring procedure. Beads and executed test results own completion. Do not maintain competing plans.

The owner accepted this specification and authorized Increment 1 on 2026-09-09. Bead `wamn-10yt.77` owns implementation and evidence. Increment 2 remains a future slice with two independent fresh installations and unchanged overlay artifacts. The implementing bead selects exact helper APIs and finite budgets for each suite. This approval makes no completion claim.

## Basis

[1] `docs/poc/deterministic-testing-spec.md`, §0 and Parts A–E: D1–D8, replay, invariants. The accepted decisions above defer Kani without an automatic trigger and govern retirement of crash tests.

[2] `wamn_architecture_proposal.md`, §§1.1–2; base-application POC, §§5, 7–8: Rust-only initial delivery, application SQLx corpus, transactions, authority.

[3] `docs/poc/wamn_receiving_layered_application_poc_scenario.md`, §§2, 6–8, 13: Receiving, layering, BFF limits, and private `quality.create_inspection`. Older upgrade walkthroughs are not authorized by this spec; [7] governs the lifecycle scope.

[4] `docs/operations/agent-pilot.md`, §§4.5–5; `docs/poc/agent-authoring-tooling-spec.md`, V3–V4: invariant ownership, traceability, behavioral and mutation checks.

[5] Generated operator TUI specification, rev 4, P2 and §4, plus its review clarification — evidence-based outcomes, captured intent, and route-specific replay safety.

[6] `docs/operations/build-and-test.md`, **Traps** (named mutation, negative-control, fixture, and distinguishing-step laws) and **[RECEIVING-MATERIALIZER-JOURNEY]**. Checked at `dfa1c3187fe8cd671688a442b23106046e502cb6`; use the maintained law entries when implementing.

[7] `docs/history/poc-architecture-review.md`: **“Schema evolution is fresh-install-only.”** This review scopes compatibility tests to that restriction, not the older upgrade workflow.

[8] `packages/client_acme_receiving/command/create_inspection/insert_inspection.sql`: inspection identity is `receipt_id`; the statement uses `ON CONFLICT ON CONSTRAINT quality_inspection_receipt_id_pkey DO NOTHING`. This implementation fact motivates the duplicate-delivery assertion; it is not execution evidence.

[9] `docs/exe-model.md`, **Ingress and durability** and **Owned tradeoffs**: at-least-once delivery, bounded retries and per-registration DLQ; duplicate effects remain possible outside named idempotency boundaries.
