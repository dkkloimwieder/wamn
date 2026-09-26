# Receiving business model

This experiment studies one receipt item as an atomic business transition.
An invariant is a rule that holds in every modeled valid state.
The model does not establish that production code implements its rules.
Beads epic `wamn-ywb0` owns the initial experiment.
Task `wamn-yhb4` owns Phase 6, the independent Receiving validation after the WMS pilots.

## Sources

The [Receiving scenario](../receiving-scenario.md#receipt-transaction) defines quantities, completion, refusal, and replay.
The [manifest](../wamn.json) declares the commands and their inputs.
The [command](../data/src/record_receipt.rs) prepares input before `record_receipt_in` looks for replay.
`replay_result` returns the original result before the command tests the current order status.
The [line validator](../command/record_receipt/validate_receipt_line.sql) refuses excess quantities.
The [quantity update](../command/record_receipt/update_purchase_order_line.sql) adds accepted quantities.
The [completion update](../command/record_receipt/finish_purchase_order.sql) stores status after a new receipt.
The [schema](../migrations/0001_initial.sql) requires positive ordered quantities and received quantities within their bounds.

## Rules

| ID | Category | Rule |
| --- | --- | --- |
| REC-INV-01 | Invariant | Each received quantity is between zero and its ordered quantity. |
| REC-INV-02 | Invariant | Recorded receipt effects sum to the stored received quantities. |
| REC-CMD-01 | Postcondition | A new accepted receipt adds each requested quantity once and preserves each unrelated line. |
| REC-CMD-02 | Precondition/refusal | Empty items and selected zero quantities refuse before replay. |
| REC-CMD-03 | Precondition/refusal | A new receipt requires an open order and quantities within the remaining amounts. |
| REC-CMD-04 | Postcondition | A refused item preserves quantities, status, claims, receipt effects, and results. |
| REC-IDEM-01 | History | Exact replay returns the original result and leaves business state unchanged. |
| REC-IDEM-02 | Precondition/refusal | Different valid intent under a committed key refuses without changing business state. |
| REC-LIFE-01 | Postcondition | After a new receipt, status is complete exactly when all order lines are fully received. |
| REC-LIFE-02 | Invariant | In valid modeled states, complete implies every line is fully received. |

REC-CMD-02 follows `prepare_with_intent` and `canonical_positive_numeric` in the command.
REC-CMD-03 follows `record_receipt_in` and the line validator.
REC-CMD-04 follows the scenario's transaction boundary.
REC-IDEM-01 and REC-IDEM-02 follow the scenario and `replay_result`.
REC-LIFE-01 follows the completion update.
REC-LIFE-02 is an inductive claim from the stated initial states, not a constraint on every possible database row.

For an order of `[2, 1]`, receiving `[1, 1]` leaves `[1, 1]` and open status.
Receiving the remaining first unit completes the order.
Replaying the first receipt returns its original open result and leaves the current order complete.
If either selected line exceeds its remainder, the whole item refuses.

## Model boundary

The model contains one order, two lines, and two available command keys.
Ordered quantities range from one through three units.
Requested quantities range from zero through four units, so excess requests remain representable.
Quantities denote abstract whole units, not a proof over arbitrary PostgreSQL decimals.
The two keys identify two receipt slots and do not wrap or expire.
An exhausted slot does not introduce a new refusal: every later command uses an existing key.

The initial received quantities are zero, with no claims or receipts.
Initial status is open or cancelled.
Cancelled represents an existing fixture, not a cancellation command.
The model stores status separately from quantities so completion assertions can detect an incorrect status update.
Successful receipts increment an abstract revision and store the original result with their claim.
The model does not include supplier updates, so revisions count only new receipts.

A command selects either or both lines.
Each selected line appears once, as required by input validation.
Line and location identities are valid by construction.
Receipt references are distinct between keys.
A separate Boolean distinguishes two valid canonical intents with the same quantities.
It represents a changed business field, such as occurrence time, without modeling its representation.

Canonical means the normalized command body used for replay comparison.
Production keeps numeric scale in that body, so equal quantities do not necessarily mean equal intent.
The model assumes this comparison and leaves its encoding to existing implementation tests.
Malformed identifiers, duplicate lines, reference conflicts, overlays, external writes, and unknown outcomes are outside this experiment.
The model contains no database, transport, runtime, or deployment behavior.

## Questions and limits

The sources define no public cancellation, reversal, or reopening command.
This experiment adds none.
The schema permits status independently of quantities, so an arbitrary database snapshot is not necessarily a valid initial model state.
The sources do not define a receipt lifecycle for empty orders or external status changes.
Those cases remain outside this model, and they establish no inferred business rule.

The model proves preservation only for its finite domain and its modeled commands.
Valid-state assumptions include consistency between claims, receipt effects, and their stored results.
Proofs must establish initialization and preservation of those assumptions.
They must also demonstrate reachable success, refusal, replay, and completion cases.
An unreachable assertion does not establish a useful business result.

## Phase 6 conformance

The application adapter imports this executable model directly.
It compares fixed and generated multi-line command histories with the real Receiving application over disposable PostgreSQL.
Each command compares its outcome, order status, quantities, immutable receipt facts, claims, and stored result.
Later commands must preserve earlier receipt facts and complete replay results.
The adapter translates identities and response fields without replacing the model's receipt decisions.
An empty receipt maps to the route schema's exact HTTP 400 refusal.
Other modeled invalid inputs map to the command's `invalid_input` result.
Both must preserve the complete database state.
Receipt timestamps compare as exact instants.
Historical rows must retain all stored values in the database snapshots.

The [run instructions](../../../docs/operations/running-tests.md#receiving-formal-conformance) select only the direct Receiving route.
Existing PostgreSQL history tests retain responsibility for concurrency, authority, and failure rollback.
The [assessment](assessment.md#phase-6-production-kernel-suitability) identifies the production-kernel boundary and its decimal representation requirements.
This phase assesses kernel suitability without extracting production code or creating shared infrastructure.

## Files

- [Model](model.rs): Independent state, receipt transitions, and ordinary Rust examples.
- [Proofs](proofs.rs): Kani initialization, transition, and replay obligations.
- [Assessment](assessment.md): Results, counterexample, test mappings, and practical limits.
