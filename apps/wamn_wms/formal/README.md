# WMS inventory model

This independent Rust model studies inventory transitions, immutable transactions, and stored command results.
Beads task `wamn-s43x.9` owns this revision.
The owner defines the target business rules below.
Production differences remain explicit in the [assessment](assessment.md).

## Business state

An inventory identity identifies stock with one product and disposition.
`Inventory` has an identity, product, packaging reference, quantity, disposition, and lifecycle.
Disposition describes stock availability: `available` or `held`.
Lifecycle describes whether an identity remains usable: `open` or `closed`.
Merge requires equal dispositions and closes only the source inventory identity.
It does not close the source packaging.
Separate available and held inventory identities can share packaging without merging.

`Packaging` describes physical handling metadata, not stock.
It has an identity, `type`, code, location, and lifecycle.
`Pallet` and `Tote` represent two packaging types without different command rules.
All modeled inventory references packaging.
Inventory location comes only from that packaging, so no separate inventory location can disagree with it.

Closed packaging has no open inventory references and refuses incoming inventory.
`ClosePackaging` refuses while any open inventory references the packaging.
Closed inventory has zero current quantity and can retain its historical packaging reference.
Such references do not prevent packaging closure.
Closure changes packaging metadata without changing inventory, so it creates no inventory transaction rows.
It still stores a command result for replay.

## Rules and sources

The [scenario](../inventory-scenario.md#stock-and-commands), [schema](../migrations/0001_initial.sql), command implementations, and existing tests supply the current business baseline.
The owner's Inventory/Packaging revision and subsequent answers define the target where that baseline differs.
These are target properties, not claims that production already implements them.

| Property | Category | Modeled rule and source |
| --- | --- | --- |
| 1. Move conservation | Postcondition | Move changes one inventory packaging reference and preserves quantity. The owner defines the new command unit. |
| 2. Split conservation | Postcondition | Split transfers a positive quantity into a new inventory identity. Current [split](../data/src/inventory_split.rs) requires a positive remainder. |
| 3. Merge conservation | Postcondition | Merge transfers all source quantity into the target and closes the source. The owner separates inventory from packaging. |
| 4. Adjustment exception | Postcondition | Only adjustment changes total quantity. Current [adjustment](../data/src/inventory_adjust.rs) requires a positive resulting quantity and a reason. |
| 5. Split lineage | History | The new inventory row records its source identity. The retained source row records its own identity. |
| 6. Merge lineage | History | Both rows record the source and target inventory identities. The separate `inventory_id` identifies each affected inventory. |
| 7. Complete transactions | History | Each accepted inventory command records every affected identity and all modeled attributes. |
| 8. Immutable history | Invariant | New commands preserve every existing operation and transaction row. |
| 9. Exact replay | History | Exact replay returns the stored original result and changes no state. |
| 10. Later changes | History | Later inventory or packaging changes preserve earlier history and replay results. |
| 11. Changed intent | Refusal | A changed command under an existing claim refuses without mutation. Invalid input also refuses. |
| 12. Closed inventory | Refusal | Ordinary inventory commands refuse closed source or target inventory. The owner defines this rule for all commands. |
| 13. Packaging/location history | History | Rows record exact packaging and derived location values for each transition. |
| Equal disposition | Precondition | Merge requires matching inventory dispositions. The owner explicitly resolves this rule. |
| Packaging location | Invariant | Inventory derives its location from packaging. There is no independent inventory location field. |
| Packaging lifecycle | Invariant and refusal | Closed packaging has no open inventory. It cannot receive inventory, and occupied packaging cannot close. |

Disposition changes, packaging relocation, and packaging reopening are outside this experiment.
Move can keep its current packaging, as current production move has no same-destination refusal.
The experiment does not infer an additional no-op refusal.
Moving between different packaging at the same location is also permitted.

## Transactions and replay

Each `InventoryTransaction` describes one affected inventory identity.
Rows include `id`, `operation_id`, `type`, `inventory_id`, source lineage, timestamp, and reason.
Paired `from_*` and `to_*` fields record product, packaging, derived location, quantity, disposition, and lifecycle.
Move and adjustment create one row. Split and merge create two rows under one `operation_id`.

For split A to B, the source row records A to A, and the new inventory row records A to B.
The new inventory has zero `from_quantity` and absent other `from_*` attributes because it did not exist.
Absence is not another disposition or lifecycle value.
For merge A to B, both rows record A to B.
Their `inventory_id` values distinguish A's transition from B's transition.

The proof reads each row through its explicit `inventory_id` and rejects duplicate or missing identities.
It reconstructs the resulting inventory from the starting inventory and row values.
It also compares source values, lineage, grouping, type, reason, timestamp, and derived locations.
This observer does not call the transaction constructor.

The stored result includes complete inventory and packaging snapshots within the finite model.
That represents the modeled business result, not the production response schema.
Replay reads this result directly, without consulting current inventory or packaging.
A later packaging closure therefore cannot change the packaging lifecycle in an earlier replay result.

## Bounds and open scope

The model contains two inventory identities, two packaging identities, two locations, two claims, and two committed operations.
Both inventories use the same product identity.
Quantities are whole units, with at most six units in total.
Open inventory has positive quantity, and closed inventory has zero quantity.
These limits define the experiment, not universal warehouse restrictions.

Split requests range from zero through seven to include invalid, exhausting, and excessive requests.
A fresh split needs the unused identity. Exact replay remains possible after that identity exists.
Adjustment requests remain within the six-unit capacity.
Adjustment to zero refuses, consistent with the current positive-count requirement.

Two opaque reason values represent distinct nonempty reasons, and `None` represents absence.
Adjustment requires a reason, but no reason catalog or authorization rule is invented.
Two timestamp values support changed-intent comparisons without a clock-order rule.
Claims represent resolved identities without production namespace or encoding details.

The initial state represents existing stock and starts with empty history.
The model does not reconstruct stock creation before that point.
Packaging already exists, and its identity, code, type, and location remain fixed during modeled commands.
Only its lifecycle can change through closure.

Unpackaged inventory, lot/serial tracking, multiple products, decimal quantities, and concurrent commands remain outside scope.
The owner rules resolve the three blocking questions for this revision.
Policies for those excluded domains remain unspecified and need decisions before any later expansion.
No database, HTTP, Wasm, WIT, deployment state, shared framework, or new DSL belongs to this model.

## Proof structure

Eight Kani harnesses establish initialization, local transitions, transaction completeness, command rules, replay, and concrete two-operation histories.
Arbitrary-state proofs require valid business state and a well-formed claim prefix.
They do not assume that old transaction rows already explain earlier mutations.
They prove that every new operation is complete and every existing operation remains unchanged.

A separate inductive argument composes empty initialization, complete appends, and unchanged prefixes into complete immutable history within the modeled domain.
No single Kani harness proves arbitrary-length history.
The experiment retains its two-operation capacity.

The [deliberate defect](missing-transaction.patch) omits the new inventory transaction during split while preserving quantity.
The [assessment](assessment.md) records results, counterexamples, production discrepancies, and test mappings.
The [run instructions](../../../docs/operations/running-tests.md#wms-formal-model) reproduce the native examples and Kani proofs.
