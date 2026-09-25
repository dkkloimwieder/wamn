Receiving owns package `wamn_receiving`, its SQL, guest, generated code, operator application, and application tests.

[Receiving scenario](receiving-scenario.md): Application behavior, limits, and source owners.
[Operator guide](operator-guide.md): Sign-in, receipt entry, keyboard controls, history, recovery, and logout.
[Manifest](wamn.json): Package identity and declared operations.
[Migrations](migrations/): Authored application schema.
[Data access](data/): SQL-backed operation implementations.
[Generated output](generated/): Derived contracts, SQL, and client code.
[Component](component/): Application guest.
[Operator application](ui/): Application-specific client composition.
[Tests](tests/): Application assertions and SQLx metadata.
[Formal business model](formal/README.md): Bounded Receiving rules, Kani proofs, and their implementation test mappings.
[Change walkthrough](tests/field-change-walkthrough.md): A Rust-only change through the development loop and its executed test.
[Running tests](../../docs/operations/running-tests.md): Commands and required inputs.
[Development loop](../../docs/operations/development-loop.md): Generation and SQLx preparation.
