# WMS inventory model

Updated through: 2026-09-27, `main` at `19ff1e370`.

## 1. Goal

One WMS table, `wms.inventory_transaction`, records every change of stock quantity. Each row names the stock it takes and the stock it gives: a from side and a to side. The table has no `kind` column. A merge, a split, an adjust, a consumption and a production differ only in which sides are set and whether the two pallets differ.

`wms.inventory_movement` goes. It is a log with one kind per command, and it loses facts: it does not name the other pallet of a merge or a split, and it does not record the sign of an adjust. `pallet_quantity` stays as the balance, and one query states that the balance equals the sum of the transactions.

A pallet that changes location is a fact about the pallet, not a change of stock. The label workflow triggers on that pallet change and writes one label for each move.

## 2. Fixed rules

- One table `wms.inventory_transaction` with `product_id`, `quantity`, `from_pallet_id`, `from_status`, `to_pallet_id`, `to_status`, `occurred_at`, `reason_code` and the stamp columns.
- `quantity > 0`. A null from side is stock that appears. A null to side is stock that leaves. At least one side is set.
- No `kind` column. The shape of a row is its kind:

  | Shape | From side | To side | Pallets |
  | --- | --- | --- | --- |
  | Production, or an adjust up | null | set | not applicable |
  | Consumption, or an adjust down | set | null | not applicable |
  | Merge line, split | set | set | differ |
  | Status change on one pallet | set | set | same, statuses differ |

- `pallet_quantity` stays as the balance. The command writes the balance and its transaction rows in the same transaction.
- One query states that every balance equals the sum of its transactions. A test runs it.
- A location change is a fact on `pallet` and its record history. It is not a transaction row.
- The label workflow triggers on the pallet location change. It writes one label for each move.
- `inventory_movement` goes. The quantity commands `inventory.adjust`, `inventory.merge` and `inventory.split` write transaction rows and return `transaction_ids[]`.
- A move is a pallet command, not a quantity command. `inventory.move` writes the location change on `wms.pallet` and its record history, and no transaction row. It returns the pallet: its id, location and revision. It refuses when the pallet is consumed or already at that location.
- A balance row means that stock is present. A balance that reaches zero is deleted, and `quantity > 0` stays.
- A quantity command that would write no transaction row refuses. `transaction_ids[]` is never empty.
- An adjust to the current count refuses by that rule.
- `delete_mode` says how rows leave a model. The `delete` operation is a route and stays optional. The generator accepts `delete_mode: hard` with no `delete` operation. `pallet_quantity` has no public `delete`.
- The pallet record history keeps its rows with `retention: unlimited`. A retention period is a later decision of the retention verb.
- The `wamn.json` workflow declaration gains an optional `condition`, the expression that the platform registration condition already takes.
- Receiving stays in its own schema. No application writes the tables of another application.
- The write log rules of [write-log.md](write-log.md) stay: the codec owns the transaction, and a retry answers the stored result and runs no post-commit work.
- A command that locks more than one pallet locks them in id order (`lock_both_pallets.sql`).

## 3. Current state

Measured on `main` at `19ff1e370` on 2026-09-27.

| Place | Today |
| --- | --- |
| The table | `wms.inventory_movement` has one `pallet_id`, `product_id`, `kind` in (`move`, `adjust`, `merge`, `split`), `from_location_id`, `to_location_id`, `quantity > 0`, `reason_code`, `occurred_at` and stamps (`apps/wamn_wms/migrations/0001_initial.sql`). A check requires both locations and different locations for `move`, and a reason for `adjust`. |
| Move | One movement row for each quantity row of the pallet, with the from and to location (`apps/wamn_wms/data/src/inventory_move.rs`). A pallet with no quantity rows writes no row and the move succeeds. `move_pallet.sql` is the only statement that changes `pallet.location_id`. |
| Adjust | `set_quantity.sql` sets the row to the counted quantity. The movement row records the quantity the row became, not the change (`inventory_adjust.rs:11-12`). The sign of the change and the old quantity are lost. An adjust to zero is refused as `invalid_input` on `value.quantity` (`data/src/scalar.rs:37-50`), because `pallet_quantity` requires `quantity > 0`. An adjust cannot add a product that the pallet does not hold (`QuantityNotFound`). |
| Merge | For each source quantity row, the command adds the quantity to the target and writes one `merge` row against the source pallet (`inventory_merge.rs`). The target pallet is not recorded. The source pallet becomes `consumed`, and its quantity rows stay with their old quantities (`consume_source.sql`). |
| Split | The command takes part of one row from the source, creates a new pallet, places the quantity there, and writes one `split` row against the source pallet (`inventory_split.rs`). The new pallet is not recorded. The source must keep stock. |
| Balance | `pallet_quantity` is unique on (`pallet_id`, `product_id`, `status`), with `quantity > 0` and `status` in (`available`, `held`). The movement rows cannot rebuild it: a merge leaves the source rows in place, and the adjust rows hold totals, not changes. |
| Live stock | `inventory_aggregate.sql` excludes consumed pallets, because their quantity rows stay after a merge. |
| Where stock appears | No command creates stock. Stock appears only through direct SQL in `apps/wamn_wms/tests/fixtures/wms-seed.sql:61-84` and `tests/business_fixture.rs:23-24`. |
| Deletes | Authored command SQL can now `DELETE FROM` a model that declares `delete_mode: hard` (`wamn-cy2q.5`, closed at `696c2afd2`). `delete_mode` requires a declared `delete` operation (`crates/schema/generator/src/manifest.rs:805-807`). No WMS model declares it. The comment in `consume_source.sql` that says the platform refuses DELETE is out of date. |
| Pallet record history | The pallet model declares `audit_log` with `retention: none` (`apps/wamn_wms/wamn.json`). That gives the stamp columns only, with no history table (`crates/control/lib/src/apply_package/record_history.rs:30-45`). |
| Label workflow | The registration `movement_label` fires on `insert` into `inventory_movement` (`wamn.json` `workflows`). The wiring keeps rows with `new.kind = "move"` and keys the label on the movement id (`publication/wirings/inventory_move_and_label.json`). A move of a pallet with three product lines writes three labels. |
| Update events | The node input is `{event, new, old}`. `old` is present only when the table has `REPLICA IDENTITY FULL` (`apps/platform/events/materializer/src/input.rs:8-24`, `services/cdc-reader/tests/event_reader_live.rs:736`). The reconciler sets `FULL` when a registration has a `condition` that reads `old` (`crates/schema/control/src/replica_identity.rs:8-9`). The platform registration has an optional JMESPath `condition` (`apps/platform/events/registration/src/model.rs:82-90`), but the `wamn.json` declaration has only `source_package`, `entity` and `ops` (`manifest.rs:178-183`). |
| Results | The four commands return `movement_ids[]` (`wamn.json`, `publication/components/wms.json.in`). |
| Readers of the movement | The model `inventory_movement` with get and query, its routes (`publication/attachments.json:418-445`), the web routes (`web/src/routes.tsx:28-30,164-178`), the generated TUI and clients, and the tests `wms_runtime_live.rs`, `terminal_preflight.py`, `wms_pty.py`, `wms_publication.rs`, `wms_wiring_shape.rs` and `cluster/application.rs`. |
| Receiving | Receiving writes no `wms` table and calls no WMS command. The only `workflows` block is the WMS one. |

## 4. Design

### 4.1 The table

`wms.inventory_transaction`, in `apps/wamn_wms/migrations/0001_initial.sql`, in place of `inventory_movement`.

| Column | Type | Rule |
| --- | --- | --- |
| `id` | `uuid` | Primary key, `DEFAULT gen_random_uuid()`. |
| `product_id` | `uuid` | Not null, references `product`. |
| `quantity` | `numeric` | Not null, `quantity > 0`. |
| `from_pallet_id` | `uuid` | References `pallet`. Null when stock appears. |
| `from_status` | `text` | Set if and only if `from_pallet_id` is set. In (`available`, `held`). |
| `to_pallet_id` | `uuid` | References `pallet`. Null when stock leaves. |
| `to_status` | `text` | Set if and only if `to_pallet_id` is set. In (`available`, `held`). |
| `occurred_at` | `timestamptz` | Not null. The time the command names. |
| `reason_code` | `text` | Nullable. |
| `created_at`, `created_by` | stamps | As the `audit_log` columns of the model, `retention: none`. |

Checks:

- At least one side is set.
- If both sides are set, the sides differ: a different pallet, or the same pallet with a different status.

The model `inventory_transaction` gets the get and query operations that `inventory_movement` has today, with the same permissions renamed. It has no write operation. Only `adjust`, `merge` and `split` write it.

### 4.2 The balance and its check

For each (`pallet_id`, `product_id`, `status`), the balance is the sum of `quantity` over the rows whose to side names it, minus the sum over the rows whose from side names it. The command that changes a `pallet_quantity` row writes its transaction rows in the same transaction.

One SQL file states the rule. It returns every (pallet, product, status) where `pallet_quantity.quantity` differs from the sum of its transactions, on both sides of a full outer join. An empty result means that the balance and the transactions agree. The local business test runs it after its command sequence and expects no rows. The seed files write a transaction row with a null from side for every quantity row they insert, so the check holds on seeded data.

A balance that reaches zero is deleted in the same transaction. `pallet_quantity` declares `delete_mode: hard` and no `delete` operation, and the command SQL deletes the row.

### 4.3 The commands

- `inventory.move` changes `pallet.location_id` and bumps `row_version`. It writes no transaction row. It returns the pallet id, location and `row_version`, and loses `movement_ids[]` and the pallet status. A consumed pallet refuses as `pallet_not_found`, as today. A pallet already at the destination refuses as `invalid_input` on `value.to_location_id`, because the contract has no literal for "nothing to change".
- `inventory.adjust` reads the row under the pallet lock, then sets it to the counted quantity. If the count is higher, it writes one row with a null from side and the pallet and status on the to side, for the difference. If the count is lower, it writes one row with the pallet and status on the from side and a null to side, for the difference. The reason is on the row. A count equal to the balance refuses as `invalid_input` on `value.quantity`, because the contract has no literal for "nothing to change". The zero refusal in `scalar.rs` goes, and an adjust to zero deletes the row.
- `inventory.merge` writes one row for each source quantity row: from (source, status) to (target, status), for the whole quantity. It deletes the source balance rows. The source pallet becomes `consumed`, as today.
- `inventory.split` writes one row: from (source, status) to (new pallet, status). The source keeps stock, as today.

`adjust`, `merge` and `split` return `transaction_ids[]` in place of `movement_ids[]`, in the order that the command wrote them. The result list shape is the one that Epic 24 built. The codec, the clients and the component pass it through.

### 4.4 The location change and the label

The pallet location change is on `pallet` and in its record history. The pallet model declares `retention: unlimited` in place of `none`, so the history keeps each location change.

The label workflow registers on `pallet` for `update`, with a condition that selects a change of location: `old.location_id != new.location_id`, in the JMESPath of the registration condition. The `wamn.json` workflow declaration gains an optional `condition`, which publish maps to the existing `EventRegistration.condition`. The reconciler then sets `REPLICA IDENTITY FULL` on `wms.pallet`, because the condition reads `old`. One generator test covers the declaration.

The wiring shapes one item for each event. The label key is the pallet id and its new `row_version`, for example `wms/{pallet_id}/{row_version}`. A move bumps `row_version` once, so each move gets one key. A retry of a move answers the stored result and writes no update, so it writes no second label. The updates that adjust, merge and split make to a pallet do not change its location, so the condition does not select them.

### 4.5 Readers

The `inventory_movement` model, its routes, the web table and detail routes, the generated TUI screens and clients, the tests that count movement rows, and the example `examples/wms_move.rs` move to `inventory_transaction`. `inventory-scenario.md`, `docs/architecture/data-access.md:234`, `docs/architecture/execution.md:238` and `docs/plan/workflow-feature.md` describe the new table and trigger. The out-of-date DELETE comments in `consume_source.sql` and `inventory_aggregate.sql` go, because the source balance rows are deleted.

## 5. Issues

One branch, one agent (the routes agent). Each issue lands with its tests. No stop between them.

1. The table and the balance check. The migration replaces `inventory_movement` with `inventory_transaction`. The model, its get and query operations, its routes and the web routes follow. The seed files write their transaction rows. The balance check SQL and the local business test that runs it land here. Regenerate every output of the package, and the clients in the same commit.
2. Delete mode without a route. The generator accepts `delete_mode: hard` with no `delete` operation. One generator test on the platform fixture covers the acceptance.
3. The commands. `adjust`, `merge` and `split` write transaction rows as section 4.3 states and return `transaction_ids[]`. `move` returns the pallet and refuses a consumed pallet or the same location. `pallet_quantity` declares `delete_mode: hard`, and a balance that reaches zero is deleted. The existing command tests change to the new rows. The balance check runs after the command tests.
4. The label trigger. The `wamn.json` workflow declaration gains `condition`, and publish maps it. The pallet model gets its retention. The registration moves to `pallet` `update` with the location condition, and the wiring keys the label on the pallet and its `row_version`. Generator and publish tests cover the condition. The WMS terminal and label cluster cases run as a cluster stage.
5. Closeout. The documents of section 4.5 describe the new model. Workspace test run, log path in the close reason, merge to main.

## 6. Out of scope

- Commands for production and consumption. The table shapes allow them, and no command writes them in this epic.
- A status change command (hold and release). The table shape allows it, and no command writes it today.
- Adding a product to a pallet by adjust. The adjust keeps its `QuantityNotFound` refusal.
- Receiving. It stays in its own schema, and no workflow joins it to WMS in this epic.
- A label for the new pallet of a split. Today a split writes no label, and this epic keeps that.
- An upgrade of a deployed database. The one initial migration changes in place, as the POC rule allows.
- Replay of the transactions into a balance. The check query compares them, and nothing rebuilds a balance from them.

## 7. Owner answers

The owner answered every question of this spec on 2026-09-27. Section 2 records the answers. No question remains.
