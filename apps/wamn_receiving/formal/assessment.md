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
