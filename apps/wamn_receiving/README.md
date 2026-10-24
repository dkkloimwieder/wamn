Receiving owns package `wamn_receiving`, its SQL, guest, generated code, and application tests.

[Receiving scenario](receiving-scenario.md): Application behavior, limits, and source owners.
[Manifest](wamn.k): Package identity and declared operations. `wamn build` compiles it to `wamn.json` in the build output.
[Migrations](migrations/): Authored application schema.
[Command SQL](command/): The statements of `record_receipt`.
[Query SQL](query/): The read statements.
[Data access](data/): SQL-backed operation implementations.
Generated output: Derived contracts, SQL, and client code. `wamn build` writes it to `apps/target/wamn/wamn_receiving/`, outside Git.
[Component](component/): Application guest.
[Publication](publication/): The attachments and the component definition.
[Package digest](package.digest): The tag and digest that the built package packs to, checked by the `package_digest` test.
[Web application](web/README.md): The browser page, with its route table.
[Tests](tests/): Application assertions and SQLx metadata.
[Change walkthrough](tests/field-change-walkthrough.md): A Rust-only change through the development loop and its executed test.
[Running tests](../../docs/operations/running-tests.md): Commands and required inputs.
[Development loop](../../docs/operations/development-loop.md): Generation and SQLx preparation.
