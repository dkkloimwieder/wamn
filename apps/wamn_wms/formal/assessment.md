# WMS assessment

The prototype remains useful within its finite domain.
The revised model separates inventory disposition, inventory lifecycle, and packaging lifecycle.
It follows the owner's target business rules, including equal-disposition merge and packaging-derived location.
The [contract](README.md) records all thirteen requested properties and the finite domain.
Task `wamn-s43x.9` owns this revision. Production alignment remains separate in `wamn-s43x.10`.

## Evidence and execution

Production source review used revision `1931d925f15e3e33ef2fc4899a8a46e96f0b4125` and the unchanged command sources on this branch.
The scenario, schema, implementations, SQL, and existing tests define the current production baseline.
The owner's latest work order and answers define the new Inventory/Packaging target.
No production code changed, and no application or deployed test ran for this model revision.
Existing application test mappings below describe inspected assertions, not new runtime results.

The toolchain is Kani 0.68.0, CBMC 6.11.0, and nightly Rust dated 2026-08-21.
The [run instructions](../../../docs/operations/running-tests.md#wms-formal-model) reproduce the experiment.
All eight Kani harnesses passed, and all eighteen cover properties were satisfied.
A cover property demonstrates that a specified case is reachable.
The final suite exited with status zero and kept loop bound four, unwinding assertions, and default assertion reachability enabled.
No business assertion, reachable unsupported construct, or loop-bound assertion failed in the correct model.

| Harness | Covers satisfied | Solver time |
| --- | --- | --- |
| `packaging_closure_preserves_history_and_replay` | 0 | 177.29 seconds |
| `later_merge_cannot_change_split_history_or_replay` | 0 | 157.44 seconds |
| `split_history_is_complete` | 0 | 32.55 seconds |
| `original_result_and_changed_intent` | 1 | 171.10 seconds |
| `quantities_and_closed_inventory` | 8 | 366.00 seconds |
| `complete_transactions` | 5 | 210.13 seconds |
| `state_and_history_preservation` | 3 | 278.53 seconds |
| `initialization` | 1 | 15.26 seconds |

The successful solver times total 1408.30 seconds, about 23.5 minutes on this host.
These are host observations, not a controlled benchmark.
The total covers only the final successful suite, excluding compilation, interrupted exploratory runs, and the separate mutation experiment.
Five native examples passed. Standalone Clippy with warnings denied and Rust formatting also passed.
Kani emitted configuration, edition-compatibility, and unreachable-construct warnings.
No unintended business counterexample appeared within the declared domain.

The source hashes identify the measured model, proofs, and native examples:

- `model.rs`: `a00144dd5bf03b24ec9b35a2e1aa4b2ffbc9ae5f3807a4b4cf0aa358c0adddcf`
- `proofs.rs`: `6202bcd6b61d829bdb2bdcff99001b6ca7b8e607dd8a92328b915aebf279b754`
- `tests.rs`: `9fa70b5e87a169fb6e6fb0ebf29b8fc49581d3361922a0099508b97570037e49`

Raw final logs remain in `/tmp/wamn-wms-inventory-final/` and `/tmp/wamn-wms-inventory-mutant/` on this host.
This assessment preserves the results because temporary directories are not durable project storage.

## Deliberate defect

The [mutation patch](missing-transaction.patch) omits the new inventory transaction during split.
The command still creates stock, and total quantity remains unchanged.
The `split_history_is_complete` harness requires a transaction for the new inventory.
Kani found a source with three units and a request to split one unit.
The result contains two units in the source and one in the new inventory.
The new inventory lacks its transaction row, so conservation passes but history completeness fails.
The failing assertion is `created inventory lacks its transaction`.
The defective run exited with status one after 79.94 seconds of solver time.
Kani printed the concrete input `3`. No overflow or loop-bound failure caused this result.
After reversing the patch, the affected proof passed in 47.55 seconds and exited with status zero.
The patch affected only an owned temporary copy.

## Target versus production

The previous correction at `e017dee28` separated pallet status from available/held quantity rows.
That correction modeled current production, including mixed active pallet statuses during merge.
The new owner rules define a different command unit: a scalar inventory identity with one disposition, separate from packaging.
The revised model therefore refuses unequal inventory dispositions without silently changing either one.
The prior production finding remains valid and does not define the new target policy.

Beads `wamn-s43x.10` tracks this domain alignment separately from the prototype.
Current [merge](../data/src/inventory_merge.rs) transfers all source pallet rows by product and stock status.
Its [lock test](../data/src/inventory_merge.rs) accepts a held source pallet and an available target pallet.
The [target update](../command/inventory_merge/touch_target.sql) preserves target pallet status.
The [quantity update](../command/inventory_merge/add_to_target.sql) and [insertion](../command/inventory_merge/place_on_target.sql) preserve each stock status.
Current production can therefore retain both available and held stock on one target pallet.
This is not a target inventory merge between unequal dispositions.

Current move relocates a whole pallet. The target move changes one inventory's packaging reference.
Current split creates a pallet. The target split creates inventory inside existing open packaging.
Current merge retires the source pallet. The target merge closes source inventory and leaves packaging lifecycle unchanged.
The current schema has no separate generic packaging lifecycle with the owner's empty-closure invariant.
The prototype models that invariant without changing production storage or commands.

Beads `wamn-s43x.6` records incomplete inventory history.
The [movement schema](../migrations/0001_initial.sql) lacks complete lineage and paired transition values.
Rows already group through command type and `idempotency_key`, but lack an explicit `operation_id`.
Existing grouping does not supply the missing transition snapshots.
The [split insertion](../command/inventory_split/insert_movement.sql) and [merge insertion](../command/inventory_merge/insert_movement.sql) omit complete snapshots for both affected identities.
The [adjustment insertion](../command/inventory_adjust/insert_movement.sql) records the resulting count without its starting count.
A read-only public movement model alone does not establish immutable storage.

Beads `wamn-s43x.1` records replay that reads mutable pallet status.
[Split](../data/src/inventory_split.rs), [merge](../data/src/inventory_merge.rs), and [adjustment](../data/src/inventory_adjust.rs) reread current status during replay.
[Move](../data/src/inventory_move.rs) stores its original result status in the claim.
The target requires the complete original result, independent of later changes.
The formal model follows that rule for inventory and packaging snapshots.

Beads `wamn-s43x.5` records the missing final-state refusal in move.
The [move lock](../command/inventory_move/lock_pallet.sql) and command body do not reject the terminal pallet state.
The other three command implementations reject that state.
This remains a source finding, not a new runtime reproduction.

Production retains old quantity rows on a terminal pallet and excludes them from the live aggregate.
The target closes source inventory with zero current quantity and preserves original quantity in transactions.
That representation difference does not itself establish production double counting.
All production findings remain open until implementation changes and tests resolve them.

## Property and test mapping

The [local runner](../tests/local_business.rs), `local_business::operations_and_replay`, calls assertions in [wms_runtime_live.rs](../tests/wms_runtime_live.rs).
The mappings below identify shared business rules and explicit gaps.
They do not claim equivalence between pallet commands and the new inventory commands.

| Required property | Formal obligation | Existing application evidence or gap |
| --- | --- | --- |
| 1. Move preserves quantity | `quantities_and_closed_inventory` | `assert_contention_and_replay` and `assert_remaining_operations` exercise pallet moves. Inventory repackaging has no equivalent test. |
| 2. Split conserves quantity | `quantities_and_closed_inventory` | `assert_remaining_operations` splits three units and examines resulting inventory. |
| 3. Merge conserves quantity | `quantities_and_closed_inventory` | `assert_remaining_operations` merges stock and requires seven units on one live pallet. |
| 4. Adjustment changes quantity | `quantities_and_closed_inventory` | `assert_remaining_operations` counts inventory to seven. Adjustment preparation tests require a reason. |
| 5. Split source lineage | `complete_transactions`, `split_history_is_complete` | No complete paired snapshot assertion. See `wamn-s43x.6`. |
| 6. Merge source lineage | `complete_transactions` | No assertion for explicit source lineage on both affected inventory rows. See `wamn-s43x.6`. |
| 7. Complete affected history | `complete_transactions` | Current history cannot express the full target transition. See `wamn-s43x.6`. |
| 8. Existing history immutable | `state_and_history_preservation` | Inspected tests do not compare complete prior transaction snapshots. |
| 9. Original replay, no mutation | `original_result_and_changed_intent` | Move compares full results in `assert_label_delivery_and_replay`. Split compares identity and revision in `assert_remaining_operations`. |
| 10. Later changes preserve replay | Both two-operation history harnesses | Split/merge/adjustment lack full-result coverage after later status changes. See `wamn-s43x.1`. |
| 11. Changed intent refuses | `original_result_and_changed_intent` | Canonical-command unit tests and move claim behavior cover selected intent differences. |
| 12. Closed inventory refuses | `quantities_and_closed_inventory` | Merge lock tests require live pallets. Inspected move tests lack terminal-state refusal. See `wamn-s43x.5`. |
| 13. Packaging/location snapshots | `complete_transactions` | Current move tests inspect location, but no generic packaging transition exists. See `wamn-s43x.10`. |
| Equal dispositions and packaging lifecycle | `quantities_and_closed_inventory`, `state_and_history_preservation` | New target rules have no direct current-production equivalents. |

The five native examples test held-stock split and merge, mismatched dispositions, packaging/location snapshots, empty closure, and closed-packaging refusal.
They also compare immutable history, original replay results, adjustment reasons, and changed intent.
Beads `wamn-s43x.3` retains the older application gaps for held splitting and complete refusal snapshots.

## Practicality and limits

The model keeps two inventory identities, two claims, two operations, and a six-unit total.
Two packaging identities make packaging references and lifecycle explicit without modeling infrastructure.
It remains smaller than the four production command files alone, excluding SQL and generated code.
The business model contains 458 lines, with 490 lines of proofs and 257 lines of native examples.
The four production command files contain 1,296 lines before their SQL and generated dependencies.
The transaction observer tests history completeness independently of quantity conservation.
The measured solver cost appears in the execution record above. Routine CI cost remains unmeasured.

The proofs establish behavior of this finite Rust model, not production equivalence or an unbounded warehouse system.
Kani establishes empty initialization, complete new operations, and preservation of existing operations.
A separate inductive argument composes those obligations within the modeled domain.
No individual harness checks arbitrary-length history.

The owner's answers resolve merge disposition, location ownership, and packaging closure for this phase.
Unpackaged stock, lot/serial identity, multiple products, decimal quantities, and concurrency remain excluded rather than assigned invented semantics.
Adjustment to zero retains the current refusal rule, and no broader adjustment authorization policy is inferred.
The prototype does not justify a shared framework or DSL.
