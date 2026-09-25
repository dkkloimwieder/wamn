# WMS inventory model

This independent Rust model studies inventory quantities, complete transactions, and immutable command results.
Beads task `wamn-s43x.4` owns the history experiment.
Review correction `wamn-s43x.8` separates pallet status from quantity status.
It has no Receiving dependency or shared formal framework.

## Rules and evidence

The [inventory scenario](../inventory-scenario.md#stock-and-commands) defines the four commands and their atomic effects.
The [schema](../migrations/0001_initial.sql) and [manifest](../wamn.json) define the current stored state and public operations.
The owner's WMS work order supplies the intended transaction history and replay contracts.
Those contracts override implementation discrepancies for this experiment.

| Rule | Category | Evidence and modeled meaning |
| --- | --- | --- |
| WMS-INV-01 | Invariant | Move, split, and merge preserve quantity separately for available and held stock. Adjustment alone can change the selected quantity. |
| WMS-CMD-01 | Postcondition | [Move](../data/src/inventory_move.rs) changes the selected location and preserves quantity. |
| WMS-CMD-02 | Precondition and postcondition | [Split](../data/src/inventory_split.rs) transfers a positive quantity of the selected status and leaves that source row positive. The new pallet inherits the source pallet status. |
| WMS-CMD-03 | Postcondition | [Merge](../data/src/inventory_merge.rs) accepts different active pallet statuses, preserves each quantity status, and keeps the target pallet status. The source closes with zero current quantities. |
| WMS-CMD-04 | Precondition and postcondition | [Adjust](../data/src/inventory_adjust.rs) sets an existing quantity row to a positive count and requires a reason. The other quantity status and pallet attributes remain unchanged. |
| WMS-CMD-05 | Precondition and refusal | Missing inventory or a selected quantity row refuses. Split refuses exhaustion of the selected row. Merge refuses identical identities. Refusal preserves all modeled state. |
| WMS-LIFE-01 | Lifecycle | Closed inventory cannot enter a new inventory-changing operation. Adjust, split, and merge implement this refusal. Move lacks it. |
| WMS-LIFE-02 | Lifecycle | Each committed operation appends complete, immutable transaction rows for every affected inventory identity. This is the owner's intended contract. |
| WMS-LIFE-03 | Lifecycle | Exact replay returns the complete original modeled result without a new effect. Later mutations cannot change that result or earlier history. |
| WMS-CMD-06 | Refusal | Changed intent under an existing claim refuses without any state change. Invalid commands also refuse. |

The model uses `Inventory`, `InventoryTransaction`, `type`, and `closed` consistently.
Production calls the final status `consumed` and retains old quantity rows.
Its [aggregate query](../query/inventory_aggregate.sql) excludes those rows from live stock.
This storage representation does not itself imply double counting.
The intended model puts historical quantities in transactions and sets current closed inventory quantity to zero.

## Two status dimensions

`PalletStatus` describes the pallet lifecycle: available, held, or closed.
`QuantityStatus` distinguishes available stock from held stock.
Each modeled inventory identity represents one pallet with one product and both possible quantity statuses.
`Quantities` stores the amount for each status, independently of the pallet status.
A zero amount means that the corresponding active quantity row is absent.

Merge accepts different active pallet statuses.
Its [lock test](../data/src/inventory_merge.rs) explicitly accepts a held source and an available target.
The [target update](../command/inventory_merge/touch_target.sql) leaves the target pallet status unchanged.
The [quantity update](../command/inventory_merge/add_to_target.sql) and [quantity insertion](../command/inventory_merge/place_on_target.sql) preserve the source quantity status.
The target can therefore contain both available and held stock, regardless of its active pallet status.

Split selects one quantity status and transfers only that stock.
Its [quantity insertion](../command/inventory_split/place_quantity.sql) retains the selected status.
The [new pallet](../command/inventory_split/create_pallet.sql) separately inherits the source pallet status.
Adjustment selects an existing quantity row through [set_quantity.sql](../command/inventory_adjust/set_quantity.sql).
These are current business rules, not unresolved owner choices.

## Transaction meaning

Each row carries its identity, operation identity, type, timestamp, inventory lineage, and exact `from_*` and `to_*` attributes.
Product, location, both quantity amounts, and pallet status form those attributes.
`from_quantities` and `to_quantities` each contain separate available and held amounts.
`from_pallet_status` and `to_pallet_status` record the independent pallet lifecycle.
An operation contains one row for move or adjustment and two rows for split or merge.
All rows in an operation share `operation_id`.

For split, `to_inventory_id` identifies the inventory whose attributes the row describes.
The new row uses the source identity as `from_inventory_id` to preserve lineage.
Its `from_quantities` are both zero.
Its product, location, and pallet status have absent `from_*` values because the new inventory did not exist.
Absence is not a fourth status.
For merge, `from_inventory_id` identifies the affected inventory, while `to_inventory_id` records its destination.

The proofs reconstruct the resulting inventory from transaction values and the operation's starting inventory.
They never read mutable current inventory to recover a historical result.
The model stores a complete inventory snapshot as its command result.
This snapshot represents the modeled business result, not the production response schema.
Wire fields, revisions, and formatting remain outside the experiment.

## Finite domain

The state has two inventory identities, two locations, two claim keys, and two committed operations.
Keys represent resolved claim identities. Production claim namespaces remain outside the model.

Each inventory has one product identity, one location, two quantity amounts, and a separate pallet status.
The current schema supplies no lot or serial identity for this scope.
Both inventories use the same product.
All combinations of active pallet statuses and available/held quantities participate in the proofs.

Quantities use whole units from zero through six, with total current quantity bounded by six.
Active inventory has a positive total quantity. Closed inventory has zero in both quantity amounts.
Empty pallets without quantity rows remain outside this inventory model.

Split requests include zero through seven to cover exhaustion and excess.
Adjustment requests stay within the finite quantity capacity.
An adjustment to zero refuses, as the current implementation requires a positive quantity.
No broader adjustment authorization rule is assumed beyond a supplied reason.

The model records reason presence, not distinct reason strings.
Changed-intent proofs apply to the modeled fields, not every production command field.

A new split needs the unused inventory identity.
An exact split replay remains in scope after that identity exists.
Capacity bounds restrict proof inputs and do not add production refusals.
The model permits at most two distinct committed operations and arbitrarily many modeled replays or refusals on those claims.
It makes no claim about arbitrary decimals, production concurrency, or longer fresh-operation histories.

Commands assume valid locations, current revisions, and unique new inventory codes where applicable.
The model omits their infrastructure and encoding.
`occurred_at` represents two distinct timestamp values for intent comparison, without a clock or ordering rule.
The initial inventory represents existing stock with an empty history for this experiment.
No proof reconstructs stock creation preceding that starting point.

## Proof structure

Initialization establishes valid inventory and an empty operation log.
Each accepted transition preserves inventory validity and every existing operation.
A separate proof requires each new operation to explain every affected inventory transition.
An inductive argument composes these verified obligations into a claim of complete immutable history within the finite domain.
That argument is separate from the individual Kani harnesses.
No harness directly checks arbitrary-length history, and the experiment retains its two-operation capacity.
The arbitrary-state proofs do not assume that old transaction rows are already correct.
The induction starts with empty history, establishes each appended operation, and preserves each existing operation.

The replay proof uses stored results even when current inventory differs.
A reachable split, merge, and replay sequence demonstrates that closing inventory cannot alter an earlier result.
The deliberate defect omits the new inventory's split transaction while still creating its stock.
This tests history completeness independently of quantity conservation.

## Scope limits and files

Review correction `wamn-s43x.8` removes the unsupported restriction on mixed active statuses.
Source evidence resolves question `wamn-s43x.7`: the current merge semantics preserve both status dimensions.
Multiple products, lot rules, serial rules, and label effects remain outside this prototype.
No broader adjustment authorization rule or alternative merge policy is inferred.

The [model](model.rs), [proofs](proofs.rs), and [native examples](tests.rs) are standalone files.
The [assessment](assessment.md) records measured results, source discrepancies, and existing test mappings.
The [run instructions](../../../docs/operations/running-tests.md#wms-formal-model) reproduce the experiment.
