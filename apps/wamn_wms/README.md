WMS owns package `wamn_wms`, its SQL, guest, generated client, and application tests.

[Inventory scenario](inventory-scenario.md): Application behavior, limits, and source owners.
[Manifest](wamn.json): Package identity and declared operations.
[Migrations](migrations/): Authored application schema.
[Data access](data/): SQL-backed operation implementations.
[Generated output](generated/): Derived contracts, SQL, and client code.
[Component](component/): Application guest.
[Operator application](examples/wms_move.rs): Application-specific client composition.
[Tests](tests/): Application assertions and SQLx metadata.
[Running tests](../../docs/operations/running-tests.md): Commands and required inputs.
[Development loop](../../docs/operations/development-loop.md): Generation and SQLx preparation.
