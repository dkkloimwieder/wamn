Acme owns package `client_acme_receiving`, its Receiving overlay, SQL, guest, generated client, and application tests.

[Overlay scenario](overlay-scenario.md): Application behavior, limits, and source owners.
[Manifest](wamn.json): Package identity and declared operations.
[Migrations](migrations/): Authored application schema.
[Data access](data/): SQL-backed operation implementations.
[Generated output](generated/): Derived contracts, SQL, and client code.
[Component](component/): Application guest.
[Operator](ui/): Acme launcher over the shared Receiving workflow.
[Operator guide](../wamn_receiving/operator-guide.md): Posting, committed results, optional reads, and recovery.
[Tests](tests/): Application assertions and SQLx metadata.
[Running tests](../../docs/operations/running-tests.md): Commands and required inputs.
[Development loop](../../docs/operations/development-loop.md): Generation and SQLx preparation.
