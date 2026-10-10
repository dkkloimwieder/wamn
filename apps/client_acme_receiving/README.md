Acme owns package `client_acme_receiving`, its Receiving overlay, SQL, guest, generated client, and application tests.

[Overlay scenario](overlay-scenario.md): Application behavior, limits, and source owners.
[Manifest](wamn.k): Package identity and declared operations. `wamn build` compiles it to `wamn.json` in the build output.
[Migrations](migrations/): Authored application schema.
[Data access](data/): SQL-backed operation implementations.
Generated output: Derived contracts, SQL, and client code. `wamn build` writes it to `apps/target/wamn/client_acme_receiving/`, outside Git.
[Component](component/): Application guest.
[Participant](participant/): Record-receipt participant guest. The base Receiving component calls it before commit.
[Tests](tests/): Application assertions and SQLx metadata.
[Running tests](../../docs/operations/running-tests.md): Commands and required inputs.
[Development loop](../../docs/operations/development-loop.md): Generation and SQLx preparation.
