# Receiving assessment

The experiment is useful for the selected business rules.
Six Kani harnesses pass, and a deliberate quantity defect produces a two-command counterexample.
These results establish properties of the bounded model, not production correctness.
Beads epic `wamn-ywb0` owns this work.

## Results

The run used Kani 0.68.0, CBMC 6.11.0, and Kani's nightly Rust toolchain dated 2026-08-21.
The source review used repository revision `1931d925f15e3e33ef2fc4899a8a46e96f0b4125`.
The prototype changes were uncommitted during the run.
The [run commands](../../../docs/operations/running-tests.md#receiving-formal-model) reproduce the experiment.

| Harness | Rules | Result | Solver time |
| --- | --- | --- | --- |
| `initialization` | Initial validity for all invariants | Pass | 2.26 seconds |
| `transition_preserves_validity` | REC-INV-01, REC-INV-02, REC-LIFE-02, auxiliary validity | Pass | 78.01 seconds |
| `new_receipt_effect` | REC-CMD-01, REC-LIFE-01 | Pass | 101.61 seconds |
| `refusal_preserves_state` | REC-CMD-02 through REC-CMD-04, REC-IDEM-02 | Pass | 81.31 seconds |
| `replay_returns_original_result` | REC-IDEM-01 | Pass | 56.75 seconds |
| `two_receipts_preserve_quantity` | REC-INV-01 from a concrete initial-state family | Pass | 17.33 seconds |

All fourteen cover properties passed.
They establish reachable success, refusal, completion, and replay cases, including replay of an earlier open result after completion.
The suite exited with status zero.
Three ordinary Rust examples also passed.
Standalone Clippy with warnings denied and Rust formatting also passed.
No application or database test ran for this experiment.

The initial loop bound of four failed inside generated equality comparisons.
The bound of 64 passed with unwinding assertions enabled.
Kani reported unreachable internal branches and warnings about unsupported constructs, but no reachable unsupported construct failed a proof.
The named business assertions passed with Kani's default assertion reachability tests enabled.
The failed preliminary run is not a business counterexample.

The model file has SHA-256 `19f801e2293ec72e368fddd8f9eb19a7b59a383823119fa87d254f5b5dc8353a`.
The proofs file has SHA-256 `677bc01bf42e2af831c7b7a4dd9abfb169532889e0664841a68d7c3bbe2bde44`.
Raw output remained in the owned temporary run directories, outside the repository.

## Deliberate defect

The [patch](overreceipt.patch) compares the requested quantity with the original order instead of the remaining quantity.
The production source was not changed.
Kani found `ordered = 1`, `first = 1`, and `second = 1` for `two_receipts_preserve_quantity`.
Both receipts target the first line under different keys.
The second line remains unreceived, so the order stays open after the first receipt.
The defective model then accepts the second receipt and stores two units against one ordered unit.

The assertion `state.received[0] <= ordered` failed.
The mutant run exited with status one and took 85.93 seconds of solver time.
This was a business assertion failure, not overflow, setup failure, or incomplete loop exploration.
After reversing the patch, the same harness passed and exited with status zero.
No unintended business counterexample appeared in the correct model.

## Existing implementation assertions

The following mappings come from source inspection, not new execution results.
All paths below are relative to the Receiving application.
The local application runner is `route_authentication_live::local_business::command_histories` in `tests/route_authentication_live/local_business.rs`.
It calls the history runner, which observes the real application and database.

| Rules | Existing assertions | Limits |
| --- | --- | --- |
| REC-INV-01, REC-CMD-01 | `tests/receiving_command_histories_live.rs::history`, `competing_receipts`, and `tests/receiving_history/database.rs::assert_state` | Generated receipts select one line. The database assertions compare stored quantities and independently summed receipt lines. |
| REC-INV-02 | `database::assert_state` compares `receipt_totals`, `received`, and claim/receipt counts | The observer depends on real database fixtures. |
| REC-CMD-02 | `data/src/record_receipt.rs::tests::malformed_shapes_refuse_with_invalid_input` | Also covers duplicate lines and numeric forms excluded from the model. |
| REC-CMD-03, REC-CMD-04 | `history`, `invalid_status`, `mixed_items`, and `rollback_after_write` | Rollback and per-item batch behavior need the real application. The model treats one item atomically. |
| REC-IDEM-01 | `history`, `competing_receipts` with the same key, and `lost_response` | These assert unchanged state and original results. Suppressed responses do not establish transport-loss behavior. |
| REC-IDEM-01 | `tests/receiving_history/model.rs::tests::replay_preserves_the_result_before_completion_and_later_updates` | This tests the existing oracle, not production by itself. |
| REC-IDEM-02 | Changed-body cases in `history` | The formal intent bit does not establish canonical encoding. |
| REC-LIFE-01, REC-LIFE-02 | Explicit histories in `tests/receiving_history/model.rs::examples`, consumed by `history` and `database::assert_state` | Completion applies to valid initialized histories, not arbitrary external database writes. |

`receiving_data_access::tests::enum_and_optimistic_update_outcomes_hold_on_postgres_18` calls `assert_record_receipt` in `tests/receiving_data_access.rs`.
That helper covers multi-line receipt success, excess refusal, replay, and current status through SQL.
Canonicalization has separate tests for line order, numeric scale, timestamp spelling, and UUID spelling in `data/src/record_receipt.rs`.
The formal experiment does not replace those assertions.

The deliberate defect matches existing excess-quantity and successive-receipt assertions.
No new production regression test was necessary because the experiment changed no production behavior and exposed no demonstrated coverage gap there.

## Practicality and remaining questions

The model has 177 lines before its native examples, plus 174 lines of Kani harnesses and 84 lines of native examples.
The production command and its authored SQL have 1,196 lines, excluding generated accessors and runtime infrastructure.
The size comparison reflects narrower scope, not equivalent coverage.
The model is small enough to inspect as one state and one transition.
The successful solver runs total about 337 seconds on this host.

The setup needed a pinned verifier installation and one loop-bound adjustment.
Stored-result consistency and canonical intent required explicit treatment that a quantity-only model misses.
Keeping quantities, recorded effects, and status separate allowed their relationships to remain testable.
The [contract](README.md#questions-and-limits) states the excluded semantics.

The result supports exploring WMS with another independent model.
WMS replay status needs an owner ruling in `wamn-s43x.1` before that property is formalized.
No shared model framework, production refactor, or common generator follows from this result.

## Phase 6: production-kernel suitability

A production kernel is a pure function that decides business transitions.
Receiving supports this boundary, but its decisions currently span Rust and SQL.
This phase assesses that boundary without extracting production code.
The independent model proofs do not prove the production implementation.

The proposed kernel takes loaded order state, all order lines, requested quantities, and location existence.
It returns a typed refusal or updated quantities, immutable receipt-line facts, and the resulting order status.
The [command](../data/src/record_receipt.rs) retains input preparation, identity allocation, claims, replay, revisions, participant execution, and persistence.
Receipt-reference uniqueness remains a database constraint within the same transaction.

The [line validator](../command/record_receipt/validate_receipt_line.sql) defines refusal precedence across the entire item.
The command first refuses an absent or non-open order.
Line refusals then rank missing line, wrong order, missing location, and excess quantity.
Within each category, the lowest offending UUID wins.
Returning the first failure in request order changes this behavior.

The [quantity update](../command/record_receipt/update_purchase_order_line.sql) adds each accepted quantity.
The [completion update](../command/record_receipt/finish_purchase_order.sql) compares every order line, including untouched lines.
A future extraction must persist the kernel's quantities and status rather than repeat those decisions in SQL.
Exact decimal arithmetic must preserve scale without a fixed machine-integer limit.
The [schema](../migrations/0001_initial.sql) also admits some special PostgreSQL numeric values.
Compatibility for those values needs measured evidence outside the finite proof domain.

Order serialization, row locks, location locks, and commit remain outside the kernel.
The [application tests](../tests/receiving_command_histories_live.rs) retain responsibility for concurrency and rollback evidence.
Empty orders and external status changes remain outside the stated model boundary.

WMS and Receiving share refusal preservation, exact quantity transitions, immutable facts, and original replay results.
Both separate business decisions from claims, locks, and commits.
Both need explicit model bounds and an independent observer to compare model outcomes with production.
Receiving adds an order-wide completion rule and ranked refusals across multiple requested lines.
Its receipt records delivered quantities rather than transfer lineage between inventory identities.
These differences support a receipt-specific kernel experiment, not a shared framework.


## Phase 6: independent model evidence

Task `wamn-yhb4` validates Receiving independently after the WMS relocation and split pilots.
The model's receipt algorithm remains unchanged.
The application tests import its state and transition functions directly through crate visibility.
The new assertions require one stored receipt effect and one claim for every newly accepted command.
Coverage assertions distinguish both-line success, either line alone, cancelled refusal, completed refusal, and valid-first/excess-second refusal.
Five native examples pass.

All six Kani harnesses pass, and all twenty coverage assertions are reached.
The complete run takes 129.865 seconds, including compilation.
The verification time totals 122.707 seconds.

The deliberate defect still compares requested quantity with the original order instead of the remaining amount.
Kani finds `ordered = 1`, `first = 1`, and `second = 1` on the first line.
The other line remains unreceived, so the order stays open between those commands.
The defective model accepts two units against one ordered unit.
The quantity assertion fails in 32.650 seconds, and Kani prints the concrete input values.
The restored file matches the source byte-for-byte, and the targeted proof passes in 10.304 seconds.
The first mutation invocation lacked Kani's required unstable flag and failed before verification.
That setup error is separate from the deliberate business counterexample.

These proofs cover two lines, two claim keys, ordered quantities of one through three, and requested quantities of zero through four.
They model whole-item acceptance or refusal, not database failure points or concurrent execution.
The conformance tests and retained PostgreSQL tests supply separate evidence at those implementation boundaries.


## Phase 6: application correspondence

The new adapter imports `formal/model.rs` directly alongside the existing Receiving history tests.
Seven fixed histories cover every modeled outcome, multi-line acceptance and refusal, completion, and original replay after later receipts.
Proptest generates histories within the same two-line, two-key quantity domain and shrinks failures against fresh fixtures.
The adapter maps each model line to a fixture UUID and binds each generated receipt UUID once.
The intent bit changes the receipt reference while preserving the claim key.
No receipt decision is copied into the adapter.

After each command, the adapter compares outcomes, order state, line quantities, receipt facts, claims, and stored result fields.
It requires every earlier receipt, receipt line, and claim row to remain unchanged.
It also replays every earlier accepted command and requires its complete original result without additional writes.
Existing direct-route tests retain the database concurrency, authority, rollback, and lost-response assertions.
The selected runtime loads only Receiving and uses its direct route.
Participant and composition assertions do not run in this phase.

The first live run exposes a timestamp assumption in the new adapter.
The observer expects `12:00Z`, but PostgreSQL renders the same instant as `08:00-04:00`.
Bead `wamn-yhb4.1` classifies this as a model defect in the adapter.
The stored business instant is correct.
The correction compares exact parsed instants, including their precision.
It also fixes the observer session to a non-UTC time zone so the regression remains effective on other hosts.
Raw historical rows still require exact preservation.


The second live run exposes another adapter assumption at the HTTP boundary.
The route schema refuses an empty line array before the command executes.
It returns HTTP 400 with `schema-invalid` and the `/0/value/line` pointer.
Bead `wamn-yhb4.2` classifies the wrong expected response envelope as a model defect in the conformance adapter.
The [manifest](../wamn.json) requires at least one line.
The [HTTP shell](../../platform/ingress/http-route/src/lib.rs) maps schema refusal to that response.
The adapter now requires that exact envelope for empty receipts and retains the command-level refusal checks for other invalid inputs.
No invalid command is filtered out, and every refusal must preserve the full snapshot.
Neither adapter correction changes the independent model's receipt decisions or production code.


After both adapter corrections, all seven fixed and sixteen generated formal histories pass.
The retained Receiving history run also passes its sixteen generated cases and seven PostgreSQL boundary cases.
The complete direct application run takes 51.075 seconds.
The separate PostgreSQL data-access test passes in 3.739 seconds.
Twelve native history and adapter tests pass, including the generator's shrinking-domain assertions.
Clippy and formatting pass.
No further business-rule mismatch appears within the stated domain.
The adapter supports shrinking and repeated reproduction, but this run finds no generated business failure that requires either.

The second domain supports the method's use beyond inventory transfers.
The small model states its rules independently, and executable correspondence exercises those rules through production transactions.
The observer requires more code than the model because it maps identifiers, response envelopes, and immutable database facts.
The two adapter findings show that representation and refusal boundaries need explicit treatment even when business decisions agree.
Receiving is suitable for a separate pure kernel experiment with full order state and exact decimal arithmetic.
This phase does not extract that kernel or prove production Rust decisions directly.
Shared formal infrastructure and participant verification remain outside this phase.
