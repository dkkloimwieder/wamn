# WMS assessment

This experiment is useful within its finite domain.
It tests inventory conservation, complete transaction history, and original-result replay.
The model follows the owner's intended history contract where current production behavior differs.
The [contract](README.md) separates those requirements from current implementation evidence and explicit scope limits.

## Execution record

Production source review used revision `1931d925f15e3e33ef2fc4899a8a46e96f0b4125`.
Review of prototype commit `7d77dd4e8` identified the missing distinction between pallet status and quantity status.
Task `wamn-s43x.8` corrects that model limitation without changing production code or capacity bounds.
The toolchain is Kani 0.68.0, CBMC 6.11.0, and Kani's nightly Rust toolchain dated 2026-08-21.
The [run instructions](../../../docs/operations/running-tests.md#wms-formal-model) reproduce the experiment.
No application, database, or deployed test ran for this prototype.
Existing test mappings below describe inspected assertions, not newly measured application results.

## Measured results

All seven Kani harnesses passed, and all fourteen cover properties were satisfied.
A cover property demonstrates that a specified case is reachable.
The final suite exited with status zero and used loop bound four with unwinding assertions enabled.
These assertions detect execution paths that exceed the loop bound.
Default assertion reachability tests remained enabled.

| Harness | Result | Covers satisfied | Verification time |
| --- | --- | --- | --- |
| `later_merge_cannot_change_split_history_or_replay` | Pass | 0 | 108.91 seconds |
| `split_history_is_complete` | Pass | 0 | 22.20 seconds |
| `original_result_and_changed_intent` | Pass | 1 | 180.31 seconds |
| `quantities_and_closed_inventory` | Pass | 5 | 339.82 seconds |
| `complete_transactions` | Pass | 4 | 207.48 seconds |
| `state_and_history_preservation` | Pass | 3 | 263.47 seconds |
| `initialization` | Pass | 1 | 9.50 seconds |

The successful verification times total 1131.69 seconds, about 18.9 minutes on this host.
The reviewed run at `7d77dd4e8` recorded 409.14 seconds.
These run times are host observations, not a controlled benchmark.
Four native examples passed. Standalone Clippy with warnings denied and Rust formatting also passed.
Kani reported toolchain configuration warnings and unsupported constructs in unreachable paths.
No reachable unsupported construct, unwinding assertion, or business assertion failed in the correct model.
No unintended business counterexample appeared within the declared domain.

In the earlier experiment, a bound of 128 caused excessive runtime, so those exploratory runs were stopped.
Inspection showed short loops over two-element arrays, including generated comparisons.
The final bound of four passed every unwinding assertion without removing any safety property.
Proof assertions use direct Boolean comparisons to avoid unnecessary diagnostic formatting code.

The source hashes identify the measured files:

- `model.rs`: `18633ffdf38ecf24463eaa816f2f201d46e1b67adb2aae9290a4b195416da627`
- `proofs.rs`: `9fd6272fc035f78a165a5c3432b2e2d8d761c69ed01871e5021d8888be42dfcc`
- `tests.rs`: `6beff2dab9c65383ab7016f8099227d9f433be8a1eec92ec724358c5b95f8c49`

Raw final logs remain in `/tmp/wamn-wms-status-run/` and `/tmp/wamn-wms-status-mutant/` on this host.
The assessment preserves the results because temporary directories are not durable project storage.

## Deliberate defect

The [mutation patch](missing-transaction.patch) omits the new inventory transaction during split.
Inventory quantities remain correct, so conservation alone cannot detect this defect.
Kani found an initial inventory with two units and a split request for one unit.
The result contains one unit in each inventory, but the new inventory has no transaction row.
The `split_history_is_complete` harness fails at `created inventory lacks its transaction`.

The defective run exited with status one after 50.10 seconds of solver time.
Kani printed a concrete playback input of `2` for the initial quantity.
The failure is a business-history assertion, not overflow or an insufficient loop bound.
After reversing the patch, the affected proof passed in 26.02 seconds and exited with status zero.
The patch was applied only to an owned temporary copy.
Production code remains unchanged.

## Status correction

The reviewed model combined pallet lifecycle and quantity status in one field.
Its restriction to matching active statuses excluded supported merge behavior.
That restriction was a model defect, not neutral bounding or an unresolved business rule.

The corrected model has separate `PalletStatus` and `QuantityStatus` dimensions.
Each inventory identity has available and held quantities, independent of its pallet status.
Merge adds each quantity to the same status on the target and leaves the target pallet status unchanged.
The source closes with zero current quantities, while transactions retain the original values.
Split and adjustment select a quantity status without changing the independent pallet status.

The two new cover cases require mixed active pallet statuses in both merge directions.
Each case requires stock that produces both available and held quantities on the target.
The native mixed-status example also compares both immutable transaction rows and replays the merge after a later adjustment to held quantity.
The existing two-identity, two-claim, two-operation, and six-unit bounds remain unchanged.

Source evidence resolves the former question `wamn-s43x.7`.
The [merge lock test](../data/src/inventory_merge.rs) accepts a held source and an available target.
The [target update](../command/inventory_merge/touch_target.sql) preserves pallet status.
The [quantity update](../command/inventory_merge/add_to_target.sql) and [quantity insertion](../command/inventory_merge/place_on_target.sql) preserve quantity status.
These sources define the modeled behavior without a new owner policy decision.

## Implementation discrepancies

Beads `wamn-s43x.6` records incomplete inventory history.
The [movement schema](../migrations/0001_initial.sql) lacks complete inventory lineage and paired quantity/status values.
Existing rows group through command type and `idempotency_key`, without an explicit `operation_id`.
That existing grouping does not supply the missing transition snapshots.
The [split insertion](../command/inventory_split/insert_movement.sql) and [merge insertion](../command/inventory_merge/insert_movement.sql) omit complete snapshots for both affected inventories.
The [adjustment insertion](../command/inventory_adjust/insert_movement.sql) records the resulting count without its starting count.
The public movement model exposes reads, but that alone does not prove immutable database storage.

Beads `wamn-s43x.1` records replay that reads mutable status.
The owner resolved the intended rule: exact replay returns the complete original result independently of later inventory changes.
[Split](../data/src/inventory_split.rs), [merge](../data/src/inventory_merge.rs), and [adjustment](../data/src/inventory_adjust.rs) reread current pallet status on replay.
[Move](../data/src/inventory_move.rs) stores its original result status in the claim.
The formal model follows the owner's original-result rule for every command.
The source discrepancy remains open until production code and regression tests fix it.

Beads `wamn-s43x.5` records the missing closed-inventory refusal for move.
The [move lock](../command/inventory_move/lock_pallet.sql) and command body do not reject a closed pallet.
The other three command implementations explicitly reject that state.
This finding comes from source review, not a new runtime reproduction.

Production keeps old quantity rows on a final-state pallet and excludes them from its aggregate.
The intended model closes the inventory with zero current quantity and preserves the old quantity in immutable transactions.
This representation difference is distinct from the incomplete-history finding.
No production refactor or fix forms part of this experiment.

## Existing implementation tests

The [local runner](../tests/local_business.rs), `local_business::operations_and_replay`, calls assertions in [wms_runtime_live.rs](../tests/wms_runtime_live.rs).
The table maps each formal property to the closest existing assertion or an explicit coverage gap.

| Formal properties | Existing assertions | Limits of that evidence |
| --- | --- | --- |
| Initialization and state validity | Fixture inventory and aggregate assertions in `assert_remaining_operations` | These exercise fixture states, not all modeled states. |
| Move quantity and location | `assert_contention_and_replay` and `assert_remaining_operations` | The model covers arbitrary quantities within its bound. |
| Adjustment effect and reason | `assert_remaining_operations` counts inventory to seven. `inventory_adjust::tests::the_reason_and_status_are_refused_before_any_statement` tests preparation. | No complete paired transaction snapshot assertion. |
| Split/merge conservation and closure | `assert_remaining_operations` splits three units, merges stock, observes final source status, and requires seven units on one live pallet. | These test available inventory and one fixture history. |
| Mixed-status merge | `inventory_merge::tests::the_locked_pair_is_found_by_id_and_must_be_live` accepts different active pallet statuses. | This unit test establishes acceptance, not the complete stock transfer. The SQL supplies the transfer rule. |
| Closed inventory refusal | `inventory_merge::tests::the_locked_pair_is_found_by_id_and_must_be_live` | No inspected move assertion for closed inventory. |
| Refused commands preserve state | Excess-split and self-merge cases in `assert_remaining_operations` | The cases do not compare every inventory, claim, and transaction field across refusal. |
| Pallet status inherited by split | Available-source assertions in `assert_remaining_operations` | Held-source coverage remains in `wamn-s43x.3`. |
| Complete immutable transactions | No equivalent complete-history assertion in the inspected owner tests | The current schema cannot express the intended full history. See `wamn-s43x.6`. |
| Original result replay | Move compares full results in `assert_label_delivery_and_replay`. Split compares identity and revision in `assert_remaining_operations`. | Those cases do not establish a complete split/merge/adjust result after later status changes. |
| Changed intent refuses | Command canonicalization unit tests and move claim behavior | Canonicalization evidence does not prove full state preservation for all conflicting command histories. |

The native examples cover held-stock split, mixed-status merge, later adjustment, immutable history, and original replay.
They also cover reason refusal and changed quantity status or timestamp under an existing claim.
The general replay proof covers arbitrary changed modeled intent, including invalid commands that refuse during preparation.
A separate reachable history closes the original split source and then replays that split.
Beads `wamn-s43x.3` retains the existing implementation gaps for held splitting and complete refusal snapshots.

## Practicality and limits

The model contains 435 lines, with 422 lines of proofs and 260 lines of native examples.
It contains no database, transport, generated accessors, deployment state, or shared formal framework.
The four production command files alone exceed 1,200 lines, excluding SQL and generated code.
The model's explicit transaction fields add size, but they expose a history defect that quantity conservation alone cannot detect.

The proofs establish the Rust model's behavior within its finite bounds.
They do not establish equivalence with production code or correctness for unbounded inventory histories.
Kani establishes three local obligations: empty initial history, complete new operations, and preservation of existing operations.
A separate inductive argument composes those obligations for executions within the finite domain.
No single harness checks arbitrary-length history, and the two-operation capacity still limits the experiment.

Mixed active pallet statuses and coexistence of available and held stock now belong to the model.
Multiple-product pallets, decimal encoding, lot/serial rules, and label effects remain outside this experiment.
The measured runtime supports a focused local experiment, but routine CI cost remains unmeasured.
The model is smaller than the production command files and omits their infrastructure.
Its transaction counterexample and source discrepancies justify the added history model.
No result justifies shared formal infrastructure yet.
