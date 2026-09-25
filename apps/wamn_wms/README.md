WMS owns package `wamn_wms`, its SQL, guest, generated client, and application tests.

[Inventory scenario](inventory-scenario.md): Application behavior, limits, and source owners.
[Manifest](wamn.json): Package identity and declared operations.
[Migrations](migrations/): Authored application schema.
[Data access](data/): SQL-backed operation implementations.
[Generated output](generated/): Derived contracts, SQL, and client code.
[Component](component/): Application guest.
[Operator application](examples/wms_move.rs): Application-specific client composition.
[Web application](web/README.md): The browser page, with its route table.
[Tests](tests/): Application assertions.
[Running tests](../../docs/operations/running-tests.md): Commands and required inputs.
[Development loop](../../docs/operations/development-loop.md): Generation during a watch session.
