# Integration state

`wamn_integration` is a base package in `apps/platform/wamn_integration`.
An integration application composes its component with an exact version and component digest.
The package uses the existing PostgreSQL capability and generated operation interfaces.
Its build output is `apps/target/wamn/wamn_integration`.

The `wamn_integration` SQL schema owns `setting` and `log`.
`setting.read` reads a JSON value by key and returns no value for an absent key.
`setting.write` inserts or replaces that value in the host transaction.
It inserts the key if absent, then updates the value in the same transaction.

`log.append` retains one row for each `(delivery_id, record_id, outcome)` tuple.
A repeated append returns the record key and leaves the existing row unchanged.
The outcomes are `pushed` and `refused`.
The default timestamp is the transaction start time.

All three operations are private.
The composing application owns its public entry points and permissions.
The package declares no event registration, so change data capture (CDC) does not include its tables.

Build the package before its consumers with `target/debug/wamn build apps/platform/wamn_integration`.
Set `DATABASE_URL` to a PostgreSQL 18 server as [building](../../../docs/operations/building.md#wamn-build) describes.
Run component unit tests in the apps workspace with `cargo test --manifest-path apps/Cargo.toml -p wamn-integration --lib`.
Run the SQL test with `cargo test -p wamn-control --test wamn_integration_live`.
The SQL test uses the [test database fixture](../../../docs/operations/running-tests.md) and the generated application grants.
