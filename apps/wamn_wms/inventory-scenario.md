# WMS inventory scenario

WMS moves stock and produces a label through a composed operation.
It owns its [manifest](wamn.json), [migrations](migrations/), command SQL, guest, and application assertions.
WMS owns its `wms.location` and `wms.product` tables.
It does not share Receiving's physical tables.

## Stock and commands

The data model contains `product`, `location`, `pallet`, `pallet_quantity`, and `inventory_movement`.
Stock status is `available` or `held`.
The pallet row provides the common lock and `row_version` for competing commands.
Quantities belong to their product and status rows.
`inventory_movement` records the committed movement history.

The four commands have distinct authority and effects:

| Operation | Purpose |
|---|---|
| `inventory.move` | Move a pallet to another location. |
| `inventory.adjust` | Change quantity with a reason. |
| `inventory.merge` | Move stock between two pallets and retire the source. |
| `inventory.split` | Transfer quantity into a newly identified pallet. |

The command owns one explicit transaction for each input item.
It claims the caller's idempotency key, locks the relevant pallet rows, and compares the expected revision.
It validates quantity and status before committing the movement, quantity changes, and new revision.
A refusal reports the declared application error.
`concurrency_conflict` carries both `expected_row_version` and `observed_row_version`.

The move claim stores the original `movement_id` and result.
Repeating the same command returns that result without another movement.
Changing its body under the same key refuses.
Two competing moves on the same pallet must produce one success and one `concurrency_conflict`.

## Reads and labels

`pallet.get` and `pallet.query` provide the declared reads.
The query filters on `status`, `location_id`, and `pallet_code`.
It sorts on `pallet_code`, `location_id`, `updated_at`, or `created_at`.
Its default order uses `created_at` with an `id` tie-breaker and an opaque cursor.
Sorting across the quantity join is outside its declared query.

`inventory.aggregate` returns a bounded projection grouped by status, product, and location.
It is a current SQL read rather than an event-maintained rollup.
The [projection implementation](data/src/inventory_aggregate.rs) owns its result.

The [published wiring](publication/wirings/inventory_move_and_label.json) composes these operations:

```text
inventory.move → label-render → blob-put
```

The move commits stock changes before label rendering and storage.
The label template comes from the wiring's declared `template_id` parameter.
The object key derives from the original movement identity.
Repeating safe storage under that key overwrites the same object rather than creating another label.

A label failure does not undo an already committed movement.
The client displays partial completion only when the served response contains the committed result and failed outcome.
Without that evidence, the outcome remains unknown.
The whole composed route does not inherit the inner command's safe-replay guarantee.

## Application observations

The [cluster cases](tests/cluster.rs) exercise released routes, contention, replay, and label output.
Separate cases exercise committed work after label failure, terminal output, browser output, and host restart.
The [terminal example](examples/wms_move.rs) uses generated screens and the common client submission layer.
These owners replace the original proposal's earlier restriction against a WMS operator interface.

The simulator's `scan_event` and `seed_inventory` profiles supply deterministic traffic shapes.
They use codes such as `PAL-000000`, `LOC-0000`, and `SKU-00000`.
The route driver exercises actual operation permissions and commands.
Traffic generation does not grant direct database mutation authority.

Application assertions require the original movement identity on replay and one label object for that movement.
They also require exactly one conflict under two competing moves.
The [operator methods](../../docs/testing/application-tests.md#operator-outcomes) distinguish partial completion from an unknown outcome.

Cross-package reference tables, joined quantity sorting, and custom label-template authoring remain outside this scenario.
Cycle counts, putaway strategies, and wave picking need separate application requirements.
The app introduces no new platform capability or shared user-interface abstraction by itself.
