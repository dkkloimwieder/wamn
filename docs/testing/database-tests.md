# Database tests

Follow [test database isolation](../operations/running-tests.md#test-database-isolation) before setup, execution, or cleanup.
This page defines the assertions at the database boundary.

## SQL and runtime behavior

SQLx tests SQL and type compatibility against the selected schema.
It does not establish permissions, business behavior, rollback, or locking.
The complete generated and authored application SQL corpus must match the SQL used during execution.
The [package verifier](../../crates/codegen/src/build_database.rs) describes the exact authored and generated SQL files with SQLx.
A successful description does not mean that the application command executed.
Guest tests must cross the real host service and transaction paths when those paths are the subject.

## Authority and observations

Apply the exact migrations and effective schema.
Use the application's production-equivalent database identity, grants, and row-level security.
Keep setup and read-only observation authority separate from command authority.
Setup writes must be explicit, while tested command histories mutate through real operations.

Inspect committed state independently of the response.
Use coherent snapshots for rules spanning multiple tables.
Each command item owns one transaction, which never crosses a graph edge.
Where supported, exercise outer envelopes containing both successful and refused items.

Establish whether a failure occurred before commit, after commit with a lost response, or during downstream work.
A timeout alone does not establish that boundary.
For rollback, observe an intermediate write before forcing failure and then inspect the final business state.

The [generated CRUD test](../../crates/codegen/tests/statement_admission_live.rs) checks operation outcomes under column grants.

## Record history

Record-history tests bind `app.user_id` to a test principal before each write.
Log tests also bind `app.operation`.
They compare stamp times by order and by equality inside one transaction, not by exact value.
Stamp writes run as the production `wamn_app` guest role where the role is the subject.
The [stamp rules](../architecture/data-access.md#record-history) define the expected results.

- The [fold tests](../../apps/crates/record-history/tests/fold.rs) reconstruct a complete chain, a truncated chain, a deleted row, a key change, and a row with no retained entries. They keep every value spelling, keep only the retained columns, and refuse rows that do not form one read.
- The [history retention test](../../crates/env/tests/history_retention_live.rs) checks that history table creation waits for an active prune.
- The [introspection test](../../crates/codegen/tests/catalog_live.rs) installs a history table and both record history triggers. It makes sure that the schema description bytes do not change.
- The [platform schema test](../../crates/env/tests/project_schema.rs) tests the platform rows, their pinned ids, and the provisioning stamps. It makes sure that each `platform` relation except `number_counter` logs every row change, including a key change, a cascaded delete, and `api_key.key_hash`. It makes sure that `wamn_history.row_image` spells each `timestamptz` as the platform canonicalizer spells it.
- A numeric scale-only update, such as `1.0` to `1.00`, updates the stamps and writes a history entry. A missing actor or operation, an invalid operation, or a missing primary key causes the write to fail without changing business rows or history. The tests check the history table's primary key, required columns and types, and refusal of overlong names. Competing writers on the same row wait for the first writer, and each history entry starts from the last committed row. A rolled-back writer leaves no entry, and writers on different rows can commit out of position order. [Project schema history tests](../../crates/env/tests/project_schema.rs)
- The [platform authority test](../../crates/env/tests/project_authority.rs) makes sure that an App generation reads no `platform` history table and writes none, and that it writes no `platform` table. It writes `number_counter` only through `platform.bump_counter`, `platform.assign_counter` and `platform.set_counter`, which refuse an id that is not a sequence. The numbering tests of the `wamn:postgres` plugin make sure that a caller reaches no sequence of another application, even when the `app.app` identity is rewritten.
- Project databases hold no system-owned relations, and `wamn_app` cannot access credentials. [Platform grant tests](../../crates/platform-install/tests/platform_grants_live.rs)
- The [generation tests](../../crates/codegen/tests/generation.rs) refuse each invalid declaration, retention, and history name, and keep the revision on a true no-op. They also make sure that a logged relation keeps the schema state id. A custom operation reads a history table with only its select grant.
- The [operation grant test](../../crates/env/tests/operation_grants.rs) checks the stored grants and the absence of grant rows for `admin`.
- The [session identity tests](../../crates/plugin/src/session_identity/tests.rs) test the principal and operation bindings and the refusal before PostgreSQL. A pooled connection keeps no earlier operation.
- The [identity live test](../../crates/identity/tests/identity_live.rs) reads the system database stamp columns and triggers from the server. It also tests the seeded `wamn:provisioning` row, the platform CHECK, and the `actor-required` refusal.

A fixture that installs triggers with its own SQL does not test installation by apply-package.
These tests run only where test setup applies `app-schema.sql`, so they do not test production provisioning.

## Contention

Use separate real connections for competing transactions.
Establish overlap with a bounded barrier or an observed database wait, not a sleep.
Inspect the refusal and final state without requiring a particular winner unless the contract specifies one.
