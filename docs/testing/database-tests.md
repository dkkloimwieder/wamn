# Database tests

Follow [test database isolation](../operations/running-tests.md#test-database-isolation) before setup, execution, or cleanup.
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

## Runtime claim coverage

The [standard claim test](../../crates/platform/runtime/tests/production_claim_live.rs) exercises claims with a declared executor credential.
The [durable claim test](../../crates/platform/runtime/tests/production_claim_durable_live.rs) stops at `executor-platform-authority-required` before its first effect attempt.
Its assertions about effect order, final caller responses, retries, and fixed release records remain unexecuted.
The fixture keeps writer and executor credentials separate.
Beads `wamn-0h0g.10.15` owns the unresolved effect-writer consumer and permission decision.

## Record history

Record-history tests bind `app.user_id` to a test principal before each write.
Log tests also bind `app.operation`.
They compare stamp times by order and by equality inside one transaction, not by exact value.
Stamp writes run as the production `wamn_app` guest role where the role is the subject.
The [stamp rules](../architecture/data-access.md#record-history) define the expected results.

- The [deploy SQL test](../../crates/control/provision/tests/deploy_sql_authority.rs) writes through the stamp function as the guest role, including the `actor-required` refusal. It also reads the stable `wamn_audit_retention` role that `postgres-init.sql` and `record-history.sql` create, and it makes sure that an applier without CREATEROLE creates no role.
- The [apply-package test](../../services/ctl/tests/apply_package_live.rs) installs and removes triggers from declarations, and it makes sure that operation grants stamp `wamn:apply-package`. It also creates history tables, moves log triggers with their retention arguments, keeps a history table when its retention becomes `"none"`, and reads the CDC exclusion rows. It reads the audit retention grants after each retention change, and a replay removes a wider grant.
- The [record history retention gate](../../tests/integration/src/record_history_retention.rs) runs `prune-record-history` as the audit retention generation against a package with 30-day, 7-day, and unlimited relations. It makes sure that the verb removes only the expired prefix of each row and writes no marker. It also makes sure that the verb refuses another login and another tenant, and that the role reads no image column.
- The [family denial matrix](../../crates/control/provision/tests/family_denial_matrix.rs) pins the audit retention reach to the history table of a P<n>D relation. The role holds nothing on an unlimited history table and no `wamn_platform` edge.
- The [introspection test](../../crates/schema/introspection/tests/postgres_live.rs) admits only the stamp trigger, log trigger, and history table shapes, and it makes sure that the schema description bytes do not change.
- The [package data-access test](../../services/ctl/tests/package_data_access_live.rs) writes a logged relation with no stamp columns through the production App role, and it makes sure that the reconciler keeps the history insert grant.
- The [app_system schema test](../../crates/identity/project-state/tests/schema.rs) tests the platform rows, their pinned ids, and the provisioning stamps.
- The [generation tests](../../crates/schema/generator/tests/generation.rs) refuse each invalid declaration, retention, and history name, and keep the revision on a true no-op. They also make sure that a logged relation keeps the schema state id.
- The [claims conformance test](../../tests/conformance/tests/session_claims.rs) refuses every reader of `app.user_id` and `app.operation` except the named trigger functions.
- The [claims tests](../../crates/platform/runtime/src/plugins/wamn_postgres/claims/tests.rs) test the principal and operation bindings and the refusal before PostgreSQL. A pooled connection keeps no earlier operation.
- The [identity live test](../../crates/identity/platform/tests/identity_live.rs) reads the system database stamp columns and triggers from the server. It also tests the seeded `wamn:provisioning` row, the platform CHECK, and the `actor-required` refusal.
- The [PAT test](../../crates/identity/platform/tests/pat_live.rs) makes sure that issuance and revocation stamp `wamn:provisioning` and that a token refuses a platform principal.

A fixture that installs triggers with its own SQL does not test installation by apply-package.
These tests run only where test setup applies `app-schema.sql`, so they do not test production provisioning.

## Contention

Use separate real connections for competing transactions.
Establish overlap with a bounded barrier or an observed database wait, not a sleep.
Inspect the refusal and final state without requiring a particular winner unless the contract specifies one.
The [Receiving history database helper](../../apps/wamn_receiving/tests/receiving_history/database.rs) owns its fixture, snapshot, and wait observations.
