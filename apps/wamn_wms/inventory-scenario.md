# WMS inventory

WMS uses `Inventory`, `Packaging`, and `InventoryTransaction` as its business records.
Install this prototype in a fresh disposable database.
The initial schema replaces the earlier POC. It supplies no migration or compatibility API.

## Inventory and packaging

Inventory identifies a quantity of one product with one disposition.
Its disposition is `available` or `held`. Its lifecycle is `open` or `closed`.
Open inventory has positive quantity. Closed inventory has zero quantity.
Ordinary commands refuse closed inventory.

Inventory stores its current `location_id` explicitly.
Packaging stores its own `location_id` separately.
Open inventory references open packaging at the same location.
Commands check that relationship while they hold locks on the affected rows.
Admitted command paths enforce co-location. The database schema does not enforce it for arbitrary direct writes.
Changing packaging metadata never implicitly relocates inventory.

Packaging has an identity, `type`, code, location, lifecycle, and revision.
Types describe physical handling, such as `tote`, `carton`, or `pallet`.
Packaging is not inventory. Multiple inventory identities can share it.
Different dispositions can share packaging without merging their inventory identities.

## Commands

Every command owns one explicit PostgreSQL transaction.
It reads or claims its request key, locks affected rows, checks the rules, applies changes, records history, and stores its result.
It commits only after every step succeeds. Any failure rolls back the complete operation.
Multiple inventory rows and multiple packaging rows are locked in database ID order.

`inventory.move` takes an inventory identity, destination packaging, and explicit destination location.
It requires open destination packaging at that location.
It changes the inventory's packaging and location while preserving quantity and disposition.
A move within the same location or packaging is permitted.

`inventory.adjust` replaces a positive quantity and requires a nonempty reason.
It preserves identity, product, packaging, location, disposition, and lifecycle.
It is the quantity-conservation exception.

`inventory.split` takes a positive quantity strictly below the source quantity.
It creates a new inventory identity in existing open packaging at the explicit destination location.
Both identities retain the source product and disposition. Their total quantity stays unchanged.

`inventory.merge` requires distinct open identities with the same product and disposition.
It checks both expected revisions, adds source quantity to target quantity, and closes the source with zero quantity.
It preserves the target's packaging and location. It does not close either packaging record.

`packaging.create` creates empty open packaging with a type, unique code, and existing location.
`packaging.close` requires that no open inventory references that packaging.
Closed inventory references do not prevent closure.
There is no packaging relocation, reopening, or implicit disposition-change command.

## Transactions and replay

Each successful inventory command inserts one immutable transaction row per affected inventory identity.
Split and merge each insert two rows under one `operation_id`.
The subject `inventory_id` identifies the row whose attributes change.
The `from_inventory_id` and `to_inventory_id` preserve lineage.
Both merge rows name the source and target inventory identities.

Transaction rows store complete `from_*` and `to_*` product, packaging, location, quantity, disposition, and lifecycle values.
They also store the operation type, occurrence time, and adjustment reason.
A new split identity has zero `from_quantity` and null prior attributes.
Application authority permits only `INSERT` and `SELECT` on the transaction table.
No application operation updates or deletes these rows.

The command claim stores the complete returned result as immutable replay data.
An exact replay returns that result without inventory or history changes.
Changed intent under the same key refuses. Later inventory changes never reconstruct an earlier result.
Claims and history share the inventory transaction, so a history insertion failure also removes the claim.

## Scope and tests

Product and location creation, update, and reads remain available.
Inventory, packaging, and transaction reads use generated contracts and bounded pages.
The aggregate reads inventory's explicit location and groups open stock by product, location, and disposition.
Its packaging count counts distinct packaging identities.

Fresh fixtures supply baseline stock. This phase introduces no stock-admission command, unpackaged stock, or lot/serial policy.
Production retains positive decimal quantities. The independent formal model uses small integer bounds.

[The formal assessment](formal/assessment.md) maps properties to the application tests.
[The local runtime test](tests/local_business.rs) exercises real commands, transaction rollback, replay, history, and database permissions.
[The publication test](tests/wms_publication.rs) checks the exported routes and their authorization modes.
