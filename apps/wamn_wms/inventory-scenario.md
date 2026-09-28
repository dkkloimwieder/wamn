# WMS inventory scenario

WMS moves stock between packagings and locations.
It owns its [manifest](wamn.json), [migrations](migrations/), command SQL, guest, and application assertions.
WMS owns its `wms.location` and `wms.product` tables.
It does not share Receiving's physical tables.

## Stock and commands

The data model contains `product`, `location`, `packaging`, `packaging_quantity`, and `inventory_transaction`.
Stock status is `available` or `held`.
The packaging row provides the common lock and `row_version` for competing commands.
Quantities belong to their product and status rows.
`inventory_transaction` records each quantity change as one row with a from side and a to side.
A side names a packaging and a status. A receipt has no from side, and a removal has no to side.
`packaging_quantity` is the balance, and [the balance check](tests/inventory_balance.sql) returns each balance that differs from the sum of its transactions.
A balance row at zero is deleted.
WMS names the unit packaging. A pallet is a `type` value (`pallet`, `tote`, `bin`, `case`, or `loose`), never a table, a column or an operation.
The platform stamp trigger records who created and changed each `packaging` row and when, and who created each `inventory_transaction` row and when.
Command SQL writes no stamp column.

The four commands have distinct authority and effects:

| Operation | Purpose |
|---|---|
| `inventory.move` | Move a packaging to another location at the time it names. It writes no transaction row. |
| `inventory.adjust` | Change quantity with a reason. |
| `inventory.merge` | Move stock between two packagings and retire the source. The two need not share a type. |
| `inventory.split` | Transfer quantity into a new packaging of a given type. |

The generated codec opens one transaction for each input item and claims the caller's idempotency key in the platform write log.
The command locks the relevant packaging rows and compares the expected revision.
It validates quantity and status before committing the transaction rows, quantity changes, and new revision.
A refusal reports the declared application error.
`concurrency_conflict` carries both `expected_row_version` and `observed_row_version`.

The write log stores the result of each command.
`adjust`, `merge`, and `split` return `transaction_ids`, the ids of the transaction rows that the command wrote.
A command that would write no row refuses, so the list is never empty. An adjust to the current count refuses as `invalid_input`.
A move returns the packaging id, its new location, and its new `row_version`. A move to the current location or of a consumed packaging refuses.
A move writes its `occurred_at` to `packaging.located_at` together with the new `location_id`, so a backdated move keeps its time.
A split writes its `occurred_at` to the `located_at` of the new packaging, and a `packaging.create` sets `located_at` to the time of the write.
The record history of `packaging` keeps each pair of `location_id` and `located_at`.
Repeating the same command returns that result without another write.
Changing its body under the same key refuses.
Two competing moves on the same packaging must produce one success and one `concurrency_conflict`.

## Reads and labels

`packaging.get` and `packaging.query` provide the declared reads.
The query filters on `status`, `location_id`, and `packaging_code`.
It sorts on `packaging_code`, `location_id`, `updated_at`, or `created_at`.
Its default order uses `created_at` with an `id` tie-breaker and an opaque cursor.
`updated_at` changes only when a command changes the packaging row.
Sorting across the quantity join is outside its declared query.

Each other model has a generated `get` and a generated `query` that pages by `created_at`.
The `location` and `product` queries filter on their code.
`location` and `product` also have a generated `create` and `update`, and `packaging` has a generated `create`.
Each create claims its key in the write log, so a retry of one key returns the first row.
Each update binds `row_version`. Every WMS revision is an `int4`, and the packaging revision is one too.
`inventory_transaction` has no write, because it is a log that the commands write.
`packaging_quantity` has no public write or delete. The commands delete a balance row at zero.

`inventory.aggregate` returns a bounded projection grouped by status, product, and location.
It is a current SQL read rather than an event-maintained rollup.
The [projection implementation](data/src/inventory_aggregate.rs) owns its result.

`/inventory/move` is a route to `inventory.move`, like every other operation.
The move's row event starts the [label workflow](publication/wirings/inventory_move_and_label.json) off the request path.
`wamn.json` declares it as the workflow `movement_label`, registered on the `packaging` update with the condition `old.location_id != new.location_id`:

```text
packaging update → shape (jsonata) → label-render → blob-put
```

The condition reads the old row, so the `packaging` table needs REPLICA IDENTITY FULL. The `reconcile-replica-identity` verb sets it.
The `shape` node turns the update into one `packaging` label item.
The label key is `{packaging_id}/{row_version}`, and the object path is `wms/{key}`, so a redelivered event overwrites the same object.
One move stores one label. The move response carries the committed move only.

## Application observations

The [cluster cases](tests/cluster.rs) exercise released routes, contention, replay, and the stored label.
Separate cases exercise committed work while the label store is missing, terminal output, browser output, and host restart.
The label cases read the label from the store after the workflow runs.
The [terminal example](examples/wms_move.rs) uses generated screens and the common client submission layer.
These owners replace the original proposal's earlier restriction against a WMS operator interface.

The simulator's `scan_event` and `seed_inventory` profiles supply deterministic traffic shapes.
They use codes such as `PAL-000000`, `LOC-0000`, and `SKU-00000`.
The route driver exercises actual operation permissions and commands.
Traffic generation does not grant direct database mutation authority.

Application assertions require the original result on replay and one label object for each move.
They also require exactly one conflict under two competing moves.
The [operator methods](../../docs/testing/application-tests.md#operator-outcomes) distinguish partial completion from an unknown outcome.

Cross-package reference tables, joined quantity sorting, and custom label-template authoring remain outside this scenario.
Cycle counts, putaway strategies, and wave picking need separate application requirements.
The app introduces no new platform capability or shared user-interface abstraction by itself.
