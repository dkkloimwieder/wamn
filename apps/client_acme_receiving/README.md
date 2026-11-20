Acme owns package `client_acme_receiving`, its Receiving overlay, SQL, guest, and generated client.

[Overlay scenario](overlay-scenario.md): Application behavior, limits, and source owners.
[Manifest](wamn.k): Package identity and declared operations. `wamn build` compiles it to `wamn.json` in the build output.
[Migrations](migrations/): Authored application schema.
[Command SQL](command/): The statements of the custom commands.
[Query SQL](query/): The read statement of the quality purchase order detail.
[Data access](component/src/data/) contains the operations that use SQL.
Generated output: Derived contracts, SQL, and client code. `wamn build` writes it to `apps/target/wamn/client_acme_receiving/`, outside Git.
[Component](component/): Application guest.
The manifest declares the component, routes, and the `quality.create_inspection` handler.
The [Receiving data access tests](../wamn_receiving/tests/receiving_data_access.rs) cover the overlay SQL.
[Running tests](../../docs/operations/running-tests.md): Commands and required inputs.
[Development loop](../../docs/operations/development-loop.md): Generation and SQLx preparation.
