WMS owns package `wamn_wms`, its SQL, guest, generated client, and application tests.

[Inventory scenario](inventory-scenario.md): Application behavior, limits, and source owners.
[Manifest](wamn.json): Package identity and declared operations.
[Migrations](migrations/): Authored application schema.
[Data access](data/): SQL-backed operation implementations.
[Generated output](generated/): Derived contracts, SQL, and client code.
[Component](component/): Application guest.
[Operator application](examples/wms_move.rs): Application-specific client composition.
[Tests](tests/): Application assertions.
[Formal business model](formal/README.md): Inventory transitions, immutable history and replay, Kani proofs, and implementation test mappings.
[Running tests](../../docs/operations/running-tests.md): Commands and required inputs.
[Development loop](../../docs/operations/development-loop.md): Generation during a watch session.
