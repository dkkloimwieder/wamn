# Database tests

Follow [test database isolation](../operations/running-tests.md) before setup, execution, or cleanup.
This page defines the assertions at the database boundary.

## SQL and runtime behavior

SQLx checks SQL and type compatibility against the selected schema.
It does not establish permissions, business behavior, rollback, or locking.
The complete generated and authored application SQL corpus must match the SQL used during execution.
Use the application's native verifier and its committed SQLx metadata through the documented commands.

The [Receiving verifier](../../apps/wamn_receiving/tests/receiving_sqlx_verifier.rs) compiles the exact generated SQL files.
Its successful compilation does not mean that the application command executed.
Guest tests must cross the real capability and transaction paths when those paths are the subject.

## Authority and observations

Apply the exact migrations and effective schema.
Use the application's production-equivalent database identity, grants, and row-level security.
Keep setup and read-only observation authority separate from command authority.
Setup writes must be explicit, while tested command histories mutate through real operations.

Inspect committed state independently of the response.
Use coherent snapshots for rules spanning multiple tables.
Each command item owns one transaction, which never crosses a wiring edge.
Where supported, exercise outer envelopes containing both successful and refused items.

Establish whether a failure occurred before commit, after commit with a lost response, or during downstream work.
A timeout alone does not establish that boundary.
For rollback, observe an intermediate write before forcing failure and then inspect the final business state.

## Contention

Use separate real connections for competing transactions.
Establish overlap with a bounded barrier or an observed database wait, not a sleep.
Inspect the refusal and final state without requiring a particular winner unless the contract specifies one.
The [Receiving history database helper](../../apps/wamn_receiving/tests/receiving_history/database.rs) owns its fixture, snapshot, and wait observations.
