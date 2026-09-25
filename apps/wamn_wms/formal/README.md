# WMS inventory model

This independent Rust model studies inventory quantities, complete transactions, and immutable command results.
Beads task `wamn-s43x.4` owns this extension of the earlier conservation experiment.
It has no Receiving dependency or shared formal framework.

## Rules and evidence

The [inventory scenario](../inventory-scenario.md#stock-and-commands) defines the four commands and their atomic effects.
The [schema](../migrations/0001_initial.sql) and [manifest](../wamn.json) define the current stored state and public operations.
The owner's WMS work order supplies the intended transaction history and replay contracts.
Those contracts override implementation discrepancies for this experiment.

| Rule | Category | Evidence and modeled meaning |
| --- | --- | --- |
| WMS-INV-01 | Invariant | Move, split, and merge preserve total inventory quantity. Adjustment alone can change this total. |
| WMS-CMD-01 | Postcondition | [Move](../data/src/inventory_move.rs) changes the selected location and preserves quantity. |
| WMS-CMD-02 | Precondition and postcondition | [Split](../data/src/inventory_split.rs) transfers a positive quantity, leaves positive source stock, and gives the new identity the source status. |
| WMS-CMD-03 | Postcondition | [Merge](../data/src/inventory_merge.rs) adds source stock to its target. The intended model closes the source with zero current quantity. |
| WMS-CMD-04 | Precondition and postcondition | [Adjust](../data/src/inventory_adjust.rs) sets a positive counted quantity and requires a reason. Other inventory quantities remain unchanged. |
| WMS-CMD-05 | Precondition and refusal | Missing inventory refuses. Split refuses exhaustion. Merge refuses identical identities. Refusal preserves all modeled state. |
| WMS-LIFE-01 | Lifecycle | Closed inventory cannot enter a new inventory-changing operation. Adjust, split, and merge implement this refusal. Move lacks it. |
| WMS-LIFE-02 | Lifecycle | Each committed operation appends complete, immutable transaction rows for every affected inventory identity. This is the owner's intended contract. |
| WMS-LIFE-03 | Lifecycle | Exact replay returns the complete original modeled result without a new effect. Later mutations cannot change that result or earlier history. |
| WMS-CMD-06 | Refusal | Changed intent under an existing claim refuses without any state change. Invalid commands also refuse. |

The model uses `Inventory`, `InventoryTransaction`, `type`, and `closed` consistently.
Production calls the final status `consumed` and retains old quantity rows.
Its [aggregate query](../query/inventory_aggregate.sql) excludes those rows from live stock.
This storage representation does not itself imply double counting.
The intended model puts historical quantities in transactions and sets current closed inventory quantity to zero.

## Transaction meaning

Each row carries its identity, operation identity, type, timestamp, inventory lineage, and exact `from_*` and `to_*` attributes.
Product, location, quantity, and status form those attributes.
An operation contains one row for move or adjustment and two rows for split or merge.
All rows in an operation share `operation_id`.

For split, `to_inventory_id` identifies the inventory whose attributes the row describes.
The new row uses the source identity as `from_inventory_id` to preserve lineage.
Its `from_quantity` is zero, and its other `from_*` attributes are absent because the new inventory did not exist.
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
Each inventory has one product identity, one location, one quantity, and one status.
The current schema supplies no lot or serial identity for this scope.
Both inventories use the same product and, when active, the same status.
Both available and held states participate in the proofs.

Quantities use whole units from zero through six, with total current quantity bounded by six.
Active inventory is positive. Closed inventory has zero quantity.
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
Together, these obligations establish complete immutable history for executions within the finite domain.
The arbitrary-state proofs do not assume that old transaction rows are already correct.
Their correctness follows from initialization, complete append operations, and preservation of the existing log.

The replay proof uses stored results even when current inventory differs.
A reachable split, merge, and replay sequence demonstrates that closing inventory cannot alter an earlier result.
The deliberate defect omits the new inventory's split transaction while still creating its stock.
This tests history completeness independently of quantity conservation.

## Open semantics and files

Beads `wamn-s43x.7` records the remaining owner question: can held inventory merge with available inventory, and which status results?
The model restricts active inventory to one status until that rule is defined.
It does not invent a refusal or status conversion for mixed-status input.
Multiple products, separate pallet and quantity-row statuses, lot rules, serial rules, and label effects remain outside this prototype.

The [model](model.rs), [proofs](proofs.rs), and [native examples](tests.rs) are standalone files.
The [assessment](assessment.md) records measured results, source discrepancies, and existing test mappings.
The [run instructions](../../../docs/operations/running-tests.md#wms-formal-model) reproduce the experiment.
