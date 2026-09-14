# Data access

Migration SQL defines an application's schema.
`wamn-schema-introspection` reads PostgreSQL into a normalized schema description.
`wamn-schema-generator` combines that description with exact manifest and SQL bytes to produce package artifacts.
The schema description is derived output, not an editable schema language.
The generator's core transformation performs no filesystem, database, clock, or environment access.

## Schema and definition ownership

The effective schema comes from the selected base migrations followed by the selected overlay migrations.
Each package keeps its own ordered migration stream.
A root version names no predecessor. Each later version names the current leaf as its exact predecessor.
The predecessor's paths and checksums form a byte-identical prefix of the candidate stream.
Forks, skipped predecessors, reordering, and changed inherited bytes refuse.
Before first release membership, a candidate package can correct its migration stream.
First membership seals the package coordinate and its cumulative stream in one transaction.
Later changes require a new package version, and retained records are not pruned.
Application migration declarations select qualified schemas.
Guest SQL names relations without schema qualification and uses the host-selected `search_path`.
Guest input cannot choose a database, schema, or database role.

The migration policy admits the supported additive table, field, and named-constraint forms.
The catalog reader supports ordinary tables, supported columns and defaults, identity properties, constraints, and supported indexes.
It normalizes identity sequences and indexes that belong to constraints instead of counting them as independent definitions.
It excludes PostgreSQL object identifiers and storage details from semantic identity.
Its exact supported forms live in [introspection](../../crates/schema/introspection/src/postgres.rs) and [migration policy](../../crates/schema/introspection/src/migration_policy.rs).

Package migrations cannot create roles, grants, extensions, routines, triggers, rules, or row policies.
Managed schemas refuse foreign tables, authored views, materialized views, unsupported types, and unsupported generated properties.
Nontransactional operations and mutations outside the selected schemas refuse.
The platform alone installs its declared extension list, currently `btree_gist`, before application migration SQL.

The platform installs the only admitted triggers and the history tables, as [record history](#record-history) describes.

Each managed relation, field, and constraint records its owning package.
An overlay can add a field only where the base permits that extension.
The added field remains overlay-owned even inside a base relation.
Schema ownership does not grant runtime mutation authority.
An overlay reads exposed base fields and mutates its own fields through declared operations.
Base row creation, deletion, and business-field mutation require an explicit base operation or grant.

The platform owns grants and row policies separately from application DDL.
Applications cannot replace or weaken them.
PostgreSQL uses the authenticated database role for application row isolation.
Control-store projections retain their separate tenant-scoped policies.
`wamn-run-state` supplies the executor's actual `CURRENT_USER` membership query.
The native runtime refuses absent membership at the existing guest error boundary.

## Generated SQL and contracts

Each package combines generated CRUD, lock, and mutation SQL with authored static query and projection SQL.
Package-owned typed accessors and registered operations expose that fixed set.
There is no universal runtime controller with unrestricted operation-ID authority over the database.
Command source uses generated accessors and named queries instead of adding unreviewed SQL strings.

Native SQLx tests compile the exact files that guest execution names.
Guests send statement identities and arguments through `wamn:postgres`.
The host resolves those identities to admitted SQL bytes and the operation's allowed statements.
Generation refuses PostgreSQL values that the production `wamn:postgres` type contract cannot represent.
SQLx metadata is compilation data, not the SQL or package contract.

Generation plans every authored and generated statement as `wamn_app` under exactly the grants that the package declaration derives.
PostgreSQL checks column privileges at plan time, so a statement that reads a column outside those grants fails at generate time.
If the grants do not cover every column, the check refuses a whole-row reference such as `to_jsonb(item)`, `RETURNING item`, or `(item).sku`.
The refusal lists the path, SQLSTATE, and PostgreSQL message of each refused statement, for example `42501 permission denied for table item`.
The check runs in one transaction that rolls back, so the generation database keeps its roles and privileges.
The [`whole_row` generator fixture](../../crates/schema/generator/tests/fixtures/whole_row/wamn.json) holds one statement for each refused form.

Generated metadata binds schema requirements, platform-policy requirements, SQL identity, and generator provenance.
Publication and deployment record source commit attribution outside tracked generated bytes.
Operation dependencies name exact base implementations and consumed contracts.
An unchanged overlay can remain compatible with an additive base when those requirements still hold.
A matching schema alone does not establish operation or business compatibility.

## Inputs, queries, and transactions

Create input omits server-owned identity, generated, revision, and audit fields.
Omission lets the declared database default apply, while explicit null requires a nullable writable field.
Update input preserves three states: absent means unchanged, null means SQL NULL, and a supplied value replaces the field.
A revision-controlled update or delete requires the expected revision.
An absent row returns `not_found`, and a changed revision returns `concurrency_conflict`.
A generated update increases the revision only when a supplied field differs from its current value.
A successful update returns the current revision, so a true no-op returns outcome `updated` with its unchanged revision.

Each outer item carries its `request_id`.
For `per_input` commands, each item owns one transaction on one PostgreSQL connection.
The transaction resource never crosses a wiring edge, and separate outer items have no shared atomicity.
A composed write followed by a projection is not one transaction.
Atomic extension of a base command requires a separate future contract.

The host owns begin, commit, rollback, and connection cleanup.
An error, trap, cancellation, or deadline destroys unfinished transaction state before another request can acquire the connection.
Serialization and deadlock errors reach the caller without an automatic transaction retry.
Commands keep network effects outside transactions that hold database locks.
Post-commit work uses the [event path](capabilities.md#events).

Generated queries return bounded pages.
Named projections declare `one`, `optional_one`, `page`, or `bounded_list` results with row and byte limits.
Unbounded JSON materialization is not a streaming or export interface.
Arbitrary tenant SQL remains subject to runtime authority and execution limits.
Compile-time checking for arbitrary tenant components remains demand-gated.

## Record history

Record history stamps who created a row, who last changed it, and when.
A relation that keeps a log also has a history table, and a log trigger writes an entry to it for each row change.
[Naming](naming.md#reserved-names) reserves the four stamp column names, the platform principal names, and the `_history` suffix.

### Declaration

Every relation-owning model declares `audit_log`:

```json
"audit_log": {
  "columns": ["created_at", "created_by", "updated_at", "updated_by"],
  "retention": "none"
}
```

`columns` selects some of the four reserved names, and `[]` turns stamping off.
`retention` is `"none"`, `"unlimited"`, or `"P<n>D"`.
`"P<n>D"` keeps entries for n whole days, and n is a positive integer with no leading zero.
`"none"` keeps no log.
`columns` and `retention` are independent, so a relation with `"columns": []` can keep a log.
An overlay model inherits the declaration of the relation owner and does not declare its own.

A 30-day log with no stamp columns:

```json
"audit_log": {
  "columns": [],
  "retention": "P30D"
}
```

The [manifest validation](../../crates/schema/generator/src/manifest.rs) refuses a missing key, a key on an overlay model, and a repeated name.
It also refuses an actor without its time: `created_by` requires `created_at`, and `updated_by` requires `updated_at`.
It refuses every other retention, including `P0D`, weeks, months, years, time parts, fractions, and signs.
The [retention task](#retention-task) removes expired entries.

[Generation validation](../../crates/schema/generator/src/generate/validation.rs) compares the declaration with the schema.
It refuses a selected column that is absent or nullable, and a logged relation with no primary key.
A selected time column must be `timestamptz`, and a selected actor column must be `uuid`, the type of `app_system.users.id`.
Generation also refuses a column with a reserved name that the declaration does not select.
Every stamp column is server-owned, so generation refuses a writable declaration of it and omits it from generated input.
A stamp column needs no default, because the trigger sets every selected value.

### Stamp trigger

[`deploy/sql/record-history.sql`](../../deploy/sql/record-history.sql) creates schema `wamn_history` and the trigger function `wamn_history.stamp_row`.
The `CATALOG_SCHEMA_SQL` composition carries the file, and the file applies again without error.
The function reads the actor from `app.user_id` and the time from `transaction_timestamp()`.
Every stamp in one transaction carries the same instant, and that instant is not the commit time.

- An insert sets every selected column.
- An update keeps the created pair.
- If a column other than the selected stamps changes, an update sets the updated pair. A true no-op keeps every stamp.
- The function replaces any value that a statement supplies for a selected column.
- A delete gets no stamp.
- A write with no bound actor raises SQLSTATE `55000` with the message `actor-required`, for every column selection.

The function reads no users row, and no stamp column has a foreign key.
Only the platform trigger functions read `app.user_id`, and no authorization or row policy reads it.

After the migrations apply, apply-package installs one `record_history_stamp` trigger on each owned relation whose declaration selects at least one column.
The trigger runs `BEFORE INSERT OR UPDATE` for each row and executes `wamn_history.stamp_row` with the selected columns.
apply-package installs no trigger for `"columns": []`, and it removes a stamp trigger that the declaration no longer selects.
It then reads the installed triggers through introspection and refuses a result that differs from the declarations.
Development package reconciliation runs the same step.
The catalog reader admits only that trigger shape and the log trigger shape below, and it leaves both out of the schema description.
Every other trigger refuses.

### History tables and the log trigger

Each relation whose retention is not `"none"` keeps a log in its own history table.
The history table is `<relation>_history` in the schema of the relation.
`wamn_history.create_history_table` in `record-history.sql` is the one definition of the table shape.
A package relation gets no `tenant_id` column.
`wamn_history.log_row_change` is the `AFTER INSERT OR UPDATE OR DELETE` row trigger function that writes one entry for each row change.
The [record history plan](../plan/record-history-spec.md#42-log-trigger) describes the entry columns and contents.

`wamn_history.row_image(record)` renders both images of an entry and the current row of a [history read](#history-read).
It sets `TimeZone` to UTC inside the function.
It spells each `timestamptz` column as the platform canonicalizer spells it: UTC RFC 3339 with exactly six fractional digits and a `Z`.
Every other value keeps the `to_jsonb` spelling.
The log function and the stamp function compare row values as JSONB text, so a change of numeric scale alone is a change.
Such a change moves the stamps and writes an entry. A true no-op moves no stamp and writes no entry.

The log function calls `row_image` with the authority of the writer.
`record-history.sql` therefore grants `EXECUTE` on `row_image` to `wamn_db_owner` and `wamn_app`, and `USAGE` on `wamn_history` to both.
An applier with no `wamn_app` role, such as the system database, gets no `wamn_app` grant.

Generation refuses these declarations:

- A model table, an internal relation table or key, or a custom operation relation whose name ends with `_history`. A custom operation can read a history table, but it cannot declare an insert, an update, or a row lock on one.
- A logged relation whose longest derived history object name has 64 bytes or more. The longest name is `<relation>_history_transaction_id_not_null`, so the name of a logged relation has 31 bytes or fewer. The refusal names the relation.
- A logged relation with no primary key. Each entry keys the row by its primary key columns.

After the migrations apply, apply-package creates the history table of each owned logged relation as `wamn_db_owner`.
apply-package never drops a history table.
It records the CDC exclusion of each history table in `wamn_cdc_exclusions`, as it records an internal relation.
The exclusion row names the package that owns the relation, and its relation id is the history table name.
A migration that creates a table whose name ends with `_history` refuses with `history-table-name-reserved`.

apply-package installs one `record_history_log` trigger on each owned logged relation.
The trigger runs `AFTER INSERT OR UPDATE OR DELETE` for each row and executes `wamn_history.log_row_change` with one argument, the retention.
The function derives the history table name from the relation and ignores the argument.
A retention of `"none"` removes the trigger, and the history table and its entries stay.
apply-package reads the installed log triggers through introspection and repairs a retention argument that differs from the declaration.
It then refuses a result that still differs.
Development package reconciliation runs the same steps.

The catalog reader leaves every table whose name ends with `_history` out of the schema description.
It admits such a table only in the shape that the function creates for a package relation.
That shape has the fixed columns, the named constraints over their columns, the identity sequence, and no ordinary index, row security, trigger, rule, or policy.
The reader does not compare the CHECK expressions.
It admits a `record_history_log` trigger only on a relation that has its history table.
The schema state id therefore stays the same when a relation starts or stops logging.

When a package logs, the generator adds a fixed description of each history table to the catalog that it validates and generates from.
The description carries the columns and the primary key, and the verified schema state id does not include it.
Authored SQL, the SQL lexer, and grant derivation use the description.
The generated data-access evidence grants `wamn_app` `INSERT` on every entry column of the history table, and no read, update, or row lock.
The log function is `SECURITY INVOKER`, so the writer needs that grant.
The reconciler reads the history table like any other package relation and keeps that grant.
The generation database creates the history tables before `EXPLAIN` and SQLx prepare, as [running tests](../operations/running-tests.md#application-generation-and-sqlx) describes.

### Retention task

A declared `"P<n>D"` retention is a minimum keep window.
It promises nothing about removal time, backups, holds, or erasure, and Beads `wamn-0h0g.13` owns those promises.
The retention task removes an expired entry on its next daily run, so its schedule sets the retention precision.
[`audit-retention.example.yaml`](../../deploy/platform/audit-retention.example.yaml) is an example daily CronJob.

The [`prune-record-history`](../../services/ctl/src/prune_record_history.rs) verb of `wamn-ctl-ops` runs the task.
It connects as a scoped generation of the tenant-scoped audit retention family, whose stable role is `wamn_audit_retention`.
It refuses any login that is not an audit retention generation for `--tenant` in the connected database.
The family is not a `wamn_platform` member, because no reader needs the edge.
`catalog-schema-prefix.sql` creates the stable role under the role bootstrap lock, beside `wamn_platform`.
The system database schema creates no audit retention role.

The verb reads the retention of each relation from the `record_history_log` trigger argument in `pg_trigger`.
It skips an `unlimited` relation.
Each relation runs in its own transaction, which binds `wamn:audit-retention` as the actor and the operation.
The transaction takes the audit retention advisory lock before it reads `pg_trigger`.
apply-package takes the same lock before it reconciles the log triggers, so a retention change waits for a running delete.
The cutoff is `transaction_timestamp()` less n days, in UTC.
The verb deletes an entry older than the cutoff only when no earlier entry of the same row is at or after the cutoff.
It therefore removes a prefix of the history of a row, never an interior entry, and it writes no marker.

apply-package owns the grants of the role.
It grants them in the transaction that reconciles the log triggers.
The role holds schema `USAGE`, `DELETE`, and `SELECT (row_key, position, changed_at)` on each history table whose log trigger carries `"P<n>D"`.
It revokes every other privilege of the role, so a change to `"unlimited"` or `"none"` revokes the grants.
It changes nothing in a database that has no role.
The provisioning grant verifier reads `pg_trigger` in each database with the query that the verb runs.
It refuses any other grant, a grant on an `unlimited` history table, and a grant in a reserved schema or `app_system`.

### Actors

The actor is the `app_system.users` id of the executing principal, never the credential.
The [execution page](execution.md#native-dispatch) lists the principal that the host binds for each kind of delivery.
A writer outside the host binds `app.user_id` with `set_config` in its own transaction.
Provisioning binds `wamn:provisioning`.
apply-package binds `wamn:apply-package` for package writes and operation grants.
The retention task binds `wamn:audit-retention`.
The binding of a platform component also sets `app.operation` to the same `wamn:<component>` name.
Administrative SQL must bind the operator's person row, and test fixtures must bind a provisioned test principal.

`app_system.users.type` is `person`, `service`, or `platform`, and it has no default.
People have `person` rows, stations and integrations have `service` rows, and platform components have `platform` rows.
The type does not change how a principal authenticates.
A `platform` row carries its `wamn:<component>` name in `display_name` and its derived id.
Its email is `<component>@<platform-domain>`.
The `users_platform_principal_check` constraint pins each name and id pair, and it refuses a `wamn:` name on another type.
`wamn_app` has only `SELECT` on `app_system.users`.

The [platform principal owner](../../crates/control/provision/src/platform_principals.rs) renders the platform rows of one tenant.
It refuses a platform domain that is not a valid domain name.
Its SQL binds `wamn:provisioning` first, so the `wamn:provisioning` row stamps itself.
The development environment, the verification world, and `tools/identity-jwks-journey-run` apply that SQL.

Every relation in [`deploy/sql/app-schema.sql`](../../deploy/sql/app-schema.sql) carries the four stamp columns as `NOT NULL` and a `record_history_stamp` trigger.
These relations are `users`, `roles`, `user_roles`, `permissions`, `configurations`, and `api_keys`.
An applier installs `record-history.sql` before `app-schema.sql`.

### System database

The system database (`wamn_system`) stamps its identity authority relations.
These relations are `identity.principals`, `identity.project_roles`, `identity.project_env_memberships`, and `identity.pats`.
Each relation carries the four stamp columns as `NOT NULL` with no default, and a static `record_history_stamp` trigger.
The registry, the sagas, the session keys, the operations tables, and the control store carry no stamps.
In the system database, the actor is an `identity.principals` id.

The `SYSTEM_SCHEMA_SQL` composition in [provisioning](../../crates/control/provision/src/lib.rs) installs `record-history.sql` before [`deploy/sql/system-schema.sql`](../../deploy/sql/system-schema.sql).
`record-history.sql` grants to `wamn_db_owner`, so an applier that runs as `wamn_system` creates that role first.

`identity.principals.kind` is `human`, `service`, or `platform`.
A `platform` row carries its `wamn:<component>` name in `subject` and in `display_name`, and its derived id.
The `principals_platform_principal_check` constraint pins that subject, display name, and id.
It also refuses a `wamn:` display name on another kind, and the subject pattern of another kind refuses a colon.
Only `wamn:provisioning` writes in the system database, so the schema creates only its row.
The schema binds `wamn:provisioning` for the transaction that creates the row, so the row stamps itself.
A platform principal cannot authenticate.
The `identity.pats` foreign key carries the principal kind, and a CHECK refuses a token for a platform principal.
Every path that turns a stored principal into a caller also refuses the platform kind.

The identity issuer and wamn-ctl bind `wamn:provisioning` in each write transaction.
wamn-identity issues each token in its own transaction.
wamn-ctl creates service principals, assigns project roles, grants and revokes memberships, and revokes tokens in the same way.
Test fixtures bind `wamn:provisioning` for platform setup, or a principal row that they insert.
The trigger keeps `created_at`, so an expired-token fixture moves `expires_at` to just after `created_at`.

### History read

A history read is an ordinary public custom projection with its own operation token and grant.
Its authored fields decide which prior data it shows.
The [generator fixture package](../../crates/schema/generator/tests/fixtures/record_history/wamn.json) shows the pattern.

The read is one flat `bounded_list` over the history table of one relation:

- Typed key inputs build `row_key` with `jsonb_build_object`, because the client refuses a JSON input.
- The inputs `after_position` and `limit` select one page in ascending position order.
- An authored `LIMIT` caps the `limit` input, and the host row limit also applies.
- Each result row carries one entry, the current row image, and the head position of the row. No field is nullable.
- `before`, `after`, and the current row are text fields that hold JSONB text, so the host and the client keep their spelling.
- The current row comes from `wamn_history.row_image` with the alias of the relation. A deleted row has the current image `{}`.
- A row with no retained entries returns an empty page.

The SQL lexer reads `wamn_history.row_image(<alias>)` as a read of every column of the relation that the alias names.
The read therefore declares every column of that relation, and the generated grant covers the whole-row reference.
The lexer reads no column from any other whole-row reference.

The [`wamn-record-history`](../../apps/platform/data/record-history/src/lib.rs) crate holds the one fold, and both workspaces register it for guests, the client, and tests.
`state_at` takes every row of every page and returns `Present`, `Absent`, or `Unavailable` at a per-row position.
It folds backward from the current row, or from the `before` image of a final delete.
It keeps each column value as raw JSON text and replaces whole values, so numeric scale and every other spelling survive.
Every position before the oldest retained entry is unavailable.
The fold refuses rows with different head positions, positions that do not rise, and a read that ends before the head position.

### Limits

Stamps are correct for writes through the supported platform paths.
On those paths, the triggers exist and every writer binds its executing principal.
No production code applies `app-schema.sql` or writes person, service, or platform rows today.
Beads `wamn-0h0g.9` owns that production application, the person and service rows, and the refusal of a credential for a principal without a users row.
Until `wamn-0h0g.22` replaces caller-settable authority, modified application SQL can forge actor attribution.
Record history gives no tamper resistance against modified application code or administrative SQL.

The history read has two more limits:

- The fold does not compare the current row with the newest entry. It cannot see a change that the log did not record, such as a change while the retention was `"none"`.
- The fold does not follow a column that a migration adds or drops between entries.

The system database has two more limits:

- The issuer and the revoker of a token read `wamn:provisioning` until Beads `wamn-0h0g.9` gives issuance a person caller.
- A delete gets no stamp. Role and membership removals therefore stay unattributed until Beads `wamn-emtx.24` adds the system database log.

## Canonical values and SQL names

`wamn_execution_contract::canonical_json_bytes` owns durable command, cursor, and package-record JSON bytes.
Input canonicalization precedes idempotency hashing.
It normalizes representation without changing values.
Timestamp spelling, UUID case, and key order normalize, while PostgreSQL numeric scale remains significant.
A changed numeric scale remains a changed command body.
A generated update compares each supplied field with its current value as PostgreSQL text.
A numeric scale-only change, such as `1.0` to `1.00`, is therefore a change that increases the revision.

Keyset pagination uses `id` as the total-order tie-breaker in the primary sort direction.
Descending order reverses the compound order.
Cursor timestamps use UTC RFC 3339 with exactly six fractional digits.
Cursor numerics preserve their PostgreSQL scale.

Constraint and ordinary-index names follow this pattern unless an explicit shorter name is required:

```text
<table>_<column_1>[_<column_n>]_<kind>
```

Columns appear in table-definition order.
Kinds are `pkey`, `key`, `fkey`, `check`, and `idx` for primary, unique, foreign, check, and ordinary-index objects.
Names must contain fewer than 64 bytes.
The author supplies an explicit shorter name when needed. Tools do not silently abbreviate or accept PostgreSQL truncation.

For database preparation and SQLx commands, use the [operations pages](../operations/README.md).
Test database isolation has one definition in [running tests](../operations/running-tests.md#test-database-isolation).
