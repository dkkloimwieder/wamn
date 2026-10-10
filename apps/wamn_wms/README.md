WMS owns package `wamn_wms`, its SQL, guest, generated client, and application tests.

[Inventory scenario](inventory-scenario.md): Application behavior, limits, and source owners.
[Manifest](wamn.k): Package identity and declared operations. `wamn build` compiles it to `wamn.json` in the build output.
[Migrations](migrations/): Authored application schema.
[Data access](data/): SQL-backed operation implementations.
Generated output: Derived contracts, SQL, and client code. `wamn build` writes it to `apps/target/wamn/wamn_wms/`, outside Git.
[Component](component/): Application guest.
[Web application](web/README.md): The browser page, with its route table.
[Tests](tests/): Application assertions.
[Running tests](../../docs/operations/running-tests.md): Commands and required inputs.
[Development loop](../../docs/operations/development-loop.md): Generation during a watch session.
