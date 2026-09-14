# Receiving scenario

Receiving records delivered quantities against a purchase order.
The package owns its [manifest](wamn.json), [migration SQL](migrations/0001_initial.sql), command implementation, generated artifacts, and application assertions.
The [Acme overlay](../client_acme_receiving/overlay-scenario.md) adds separate client-owned behavior.
Shared naming and execution rules belong in [architecture](../../docs/architecture/README.md).

## Operator workflow

The operator selects an open purchase order, enters receipt lines, selects locations, and submits one receipt.
The client supplies `request_id`, an idempotency key, `receipt_reference`, and `occurred_at`.
The server owns the authoritative receipt transaction and its resulting identifiers.
CSV import remains outside the declared Receiving operations.

The base owns `item`, `location`, `purchase_order`, `purchase_order_line`, `receipt`, and `receipt_line`.
Its internal `record_receipt_command` table stores replay information and is excluded from CDC.
The public operations are declared in the manifest:

| Operation | Purpose |
|---|---|
| `purchase_order.get` and `purchase_order.query` | Read an order or a page of orders. |
| `purchase_order.update` | Change `supplier_id` with an expected `row_version`. |
| `receipt.get` and `receipt.query` | Read receipt history. |
| `receiving.record_receipt` | Commit one atomic receipt for each input item. |
| `receiving.load_receipt_screen` | Read the order and line information needed for entry. |
| `location.list` | Read a bounded list of available locations. |
| `receiving.load_purchase_order_history` | Read one page of the change log of a purchase order. |

Each public operation has its own operation grant, and the `route-caller` role holds every grant.
The history read has the grant `wamn-receiving:receiving/load-purchase-order-history@1.0.0`.

The query filters on `supplier_id` and `status`.
Its declared sort fields are `purchase_order_number`, `status`, and `created_at`.
The update operation grants no create or delete authority over purchase orders.

## Record history

`purchase_order` keeps its log with retention `unlimited`, and `purchase_order_line` keeps its log for 30 days (`P30D`).
`item`, `location`, `receipt`, and `receipt_line` keep no log.
The history read returns the entries of one purchase order after `after_position`, with at most `limit` entries from 1 through 100.
A purchase order with no retained entries returns an empty page.
Every image in the result holds only the nine `purchase_order` columns that the read declares, so the Acme columns never appear.

## Receipt transaction

The [command](data/src/record_receipt.rs) authorizes the caller and opens one explicit transaction per input item.
It claims the idempotency key, locks the purchase order and lines, and compares the current status and remaining quantities.
It inserts the receipt and lines, updates received quantities and order status, and stores the original result before commit.
A refusal or failure before commit rolls back that item's business changes.

The command accepts 1 through 100 outer items.
Each item contains 1 through 100 receipt lines with distinct `purchase_order_line_id` values.
The ingress body limit is 1 MiB.
The timestamp and numeric carriers follow the declared [input contract](wamn.json).

The same key and canonical body return the original result without another receipt.
A different body under that key returns `idempotency_conflict`.
The replay comparison excludes `request_id` and the idempotency key itself.
Canonicalization orders lines by `purchase_order_line_id` and preserves PostgreSQL numeric scale.

The transaction must not receive more than the remaining ordered quantity.
Competing valid receipts must serialize through the locked order and lines.
Receipt-line sums must agree with stored received quantities and the order's completion status.
A stale `purchase_order.update` returns `concurrency_conflict` without changing business state.

## Client behavior

The [operator application](ui/) composes generated screens with ordinary Rust.
It uses generated request types and the shared submission state machine.
The terminal owns no transaction or authorization decision.
The `h` key on the order list opens the history panel of the selected order.
The panel reads every page up to the head position, and it reads again from the start when the head position changes.
It folds the rows with [`wamn-record-history`](../platform/data/record-history/src/lib.rs) and shows the order at the selected entry as present, absent, or unavailable.
If the fold refuses the rows, the panel shows the refusal.

A confirmed refusal keeps the draft available for correction.
Success consumes that submission.
An unknown outcome must remain unknown until evidence resolves it.
A safe retry uses the captured command bytes, while credentials are obtained for the new attempt.
A replacement target invalidates records, revisions, cursors, drafts, and pending submissions.

## Application assertions

[Command histories](tests/receiving_command_histories_live.rs) compare outcomes with an independent [model](tests/receiving_history/model.rs).
The [database observer](tests/receiving_history/database.rs) reads a coherent business snapshot independently of the response.
It includes orders, lines, command claims, receipts, and receipt lines.
Its assertions compare receipt-line sums separately from the stored received quantity.

The retained cases cover replay, changed-body refusal, exact remainder, excess quantity, invalid status, contention, rollback, lost response, stale revisions, and authority removal.
The lost-response case withholds a received application response after observing commit.
It does not establish a TCP-loss or process-kill result.

[Route tests](tests/route_authentication_live.rs) exercise the deployed application and its exact release.
The PAT journey folds the served history of a purchase order that the Acme update changed, and it finds no Acme column.
A route caller that holds every other grant but not the history grant gets `403` `permission-denied`.
The [data access tests](tests/receiving_data_access.rs) show that an idempotent replay appends no history entry.
They also show that an Acme update logs a changed-column diff, and that the fold rebuilds the effective row.
[Postcommit tests](tests/postcommit.rs) exercise the private Acme consumer through the native event broker.
The materializer case shows that CDC publishes no history entry.
It also shows that the low 32 bits of each entry transaction id of the receipt equal the event `txid`.
[Terminal tests](tests/operator_pty.py) cover the Receiving workflow at the terminal boundary.
The [test methods](../../docs/testing/application-tests.md#receiving-commands) distinguish these observations from compilation or an unexecuted case.

The application does not support atomic client extensions across separate component invocations.
ERP integration and CSV import need an actual application requirement.
Schema changes with retained installed data remain in the deferred [upgrade design](../../docs/plan/upgrades.md).
