# Record history and audit log

This plan has two increments.
Level 1 records who changed a row and when.
Level 2 keeps a log of what changed.
Level 2 has its own acceptance and does not ride on the level-1 tests.
Beads epic `wamn-emtx` records decisions and implementation status.

## 1. Problem

An application that tracks stock, orders, or receipts must show who changed a row and when.
It often must also show what the row looked like before the change.
Today neither answer exists without per-application discipline.
Timestamp columns exist only because an author wrote them into a migration.
No application data records an actor, and generated CRUD cannot stamp one.

## 2. Declaration

Every relation-owning model declares the `audit_log` key.
An overlay model inherits the declaration of the relation owner and must not repeat it.
Generation infers nothing from absence and refuses a missing key or a missing entry.
The declaration is the only record-history input that an author writes.
No model scaffold exists today.
A future scaffold writes all four columns and `"retention": "none"` for a new model.
An author then turns columns off where a row is never edited or has no actor.

```json
"audit_log": {
  "columns": ["created_at", "created_by", "updated_at", "updated_by"],
  "retention": "P90D"
}
```

- `columns` is always an array of the four fixed names. `[]` turns stamping off.
- `retention` is always a value: an ISO 8601 duration, `"unlimited"`, or `"none"`. The value `"none"` means columns only, with no log.

Off, for reference data, derived rows, and claim tables:

```json
"audit_log": { "columns": [], "retention": "none" }
```

Captured data that is written once and never edited, such as a movement, a scan, or a count:

```json
"audit_log": { "columns": ["created_at", "created_by"], "retention": "none" }
```

Columns only, with no log:

```json
"audit_log": {
  "columns": ["created_at", "created_by", "updated_at", "updated_by"],
  "retention": "none"
}
```

Timestamps only, with a 90-day log:

```json
"audit_log": {
  "columns": ["created_at", "updated_at"],
  "retention": "P90D"
}
```

An editable record with every column and a 90-day log:

```json
"audit_log": {
  "columns": ["created_at", "created_by", "updated_at", "updated_by"],
  "retention": "P90D"
}
```

Every column, with the log kept indefinitely:

```json
"audit_log": {
  "columns": ["created_at", "created_by", "updated_at", "updated_by"],
  "retention": "unlimited"
}
```

Application migrations own the physical columns and create no triggers.
Generation makes sure that every selected column exists and is `NOT NULL`.
Time columns must be `timestamptz`, and actor columns must be `uuid`, the type of `app_system.users.id`.
Generation refuses a nullable column, because a generated contract then exposes an optional value that is never null.
Generation creates no column.
Selected columns become server-owned automatically.

The four names are reserved.
A model column with a reserved name must be selected.
A column with another meaning takes another name.
An overlay cannot add a column with a reserved name that the owning declaration does not select.

Generation refuses these declarations:

- A missing key on a relation-owning model, or a key on an overlay model.
- A name outside the four, or a repeated name.
- A selected column that is absent, wrongly typed, nullable, or declared writable.
- An actor without its time. `created_by` requires `created_at`, and `updated_by` requires `updated_at`.
- `columns: []` with a retention other than `"none"`.
- A reserved-name column that the declaration does not select.

Until level 2 exists, `retention` must be `"none"`.
Generation refuses a declaration that enables the log instead of accepting and ignoring it.

## 3. Level 1: the columns

The naming rules reserve four fixed names: `created_at`, `created_by`, `updated_at`, and `updated_by`.
The names are uniform, so generated operator screens can show "changed by X at Y" on any model, and two applications read alike.

### Time

The time is `transaction_timestamp()`, the start time of the current database transaction.
PostgreSQL keeps it in UTC with microsecond precision.
Every stamp in one transaction carries the same instant.
An `explicit_per_input` command stamps one instant per input item, because each item runs in its own transaction.
An implicit statement runs in its own transaction and stamps its own instant.
A nested call runs in its own transaction and stamps its own instant.
The time is not the commit time, and it does not order concurrent changes.

### Actors

The actor is the `app_system.users` id of the executing principal.
The stamped value names the user, never the credential.
The same person with a personal access token or a session token stamps the same id.

`app_system.users` has a `type` column with the closed set `person`, `service`, and `platform`.
Every principal that can write in a tenant has a row.
People have `person` rows.
Stations and integrations have `service` rows, so "which station scanned this" has an answer.
Platform components have `platform` rows.
A row type does not change how a principal authenticates.

Origin and executor are distinct facts, and attribution records the executor.
Every transaction that writes binds `app.user_id` to its executing principal.
The host binds it in each claims transaction that it opens:

- An authenticated route caller binds its principal id.
- A nested call binds the executing principal of its parent: the retained caller, or the platform principal of a callerless parent.
- A post-commit registration delivery binds `wamn:materializer`.
- An executor queue delivery and a management candidate case bind `wamn:executor`.

Writers outside the host set `app.user_id` with `set_config` in their own transactions:

- Provisioning sets `wamn:provisioning`.
- apply-package sets `wamn:apply-package`, including for operation grants.

The implementation lists every production writer to a stamped relation and every path that opens a claims transaction, and each one names its component.
A read that writes nothing and locks nothing keeps the autocommit path and binds no actor.

Actor columns are `NOT NULL`.
A write that reaches a record-history trigger with no bound actor shows that the platform broke its own contract, whatever the column selection is.
The trigger raises SQLSTATE `55000` with the message `actor-required`, and the operation returns `internal_error`.
The operation vocabulary gains no literal for this case, because a caller cannot remedy it.

An attachment with auth policy `none` has no principal, so it cannot write.
Release mint refuses an anonymous attachment whose wiring closure can write.
The host also refuses a transactional statement that has no executing principal before the statement reaches PostgreSQL.

### Platform rows

A platform row is named `wamn:<component>`, with a kebab-case component and no action or version.
The level-1 components are `provisioning`, `apply-package`, `materializer`, and `executor`.
Level 2 adds `audit-retention`.
The row names the component, never the invocation.
Causation on the event records what caused a platform write.
The operator who ran apply-package is causation, and the apply record keeps it.

The id of a platform row is the UUIDv5 of its full name under one fixed WAMN namespace UUID.
`crates/identity/project-state`, the model of the `app_system` schema, defines the namespace, the name grammar, the component list, and the derivation.
The id is the same in every tenant and every deployment.
Provisioning computes the id to write the row, and the host computes it to bind the actor.
No configuration carries the id, and no lookup happens.

`display_name` carries the `wamn:<component>` name.
`email` stays `NOT NULL`.
A platform row's email is `<component>@<platform-domain>`, from a required platform-domain setting in deployment configuration.
Provisioning code writes that email and makes sure that its domain matches, because static deploy SQL cannot read deployment configuration.
A CHECK constraint allows a `wamn:` display name only on a `platform` row and pins the literal name and id pairs.
PostgreSQL has no built-in UUIDv5, so a Rust test compares those pairs with the derivation.

A tenant or application cannot create a `platform` row or a `wamn:` name.
`wamn_app` has only SELECT on `app_system.users`.
The naming rules reserve the `wamn` package id, because that id produces `wamn:` operation tokens.
The catalog and the generator both refuse it.

### Provisioning

A missing users row is a provisioning defect, not a runtime case.
Provisioning refuses to issue a credential for a principal without its users row.
No write looks up the row, and no stamp column has a foreign key to it.

Platform rows exist per tenant and carry that tenant's `tenant_id`.
When provisioning sets up a tenant, it first creates that tenant's `wamn:provisioning` row, and that row stamps itself.
Provisioning then creates the other platform rows for the tenant.

No production code applies `deploy/sql/app-schema.sql` or sets up tenant users today.
Level 1 creates platform rows wherever tenant setup runs today: the development environment, the verification world, `tools/identity-jwks-journey-run`, and test setup code.
`wamn-0h0g.9` owns production application of `app-schema.sql`, provisioning of person and service rows, and the credential refusal.
Every write has an actor wherever that setup runs.

### Administrative SQL and fixtures

Administrative SQL sets `app.user_id` to the operator's person row.
Test fixtures set `app.user_id` to a provisioned test principal.
Nothing bypasses the trigger.
Fixture assertions compare timestamp order, not exact times.

### Scope

The trigger stamps inserts and updates from every statement: generated operations, authored command SQL, and administrative SQL.
The trigger replaces any value that a statement supplies for a selected column, so authored SQL has no reason to write stamp columns.
An insert writes the created pair and the updated pair.
An update writes the updated pair only.
A delete is not stamped, because a removed row has nowhere to keep the values, and this plan adds no soft delete.

### No-op updates

A true no-op changes nothing.
An update whose business values all equal their current values writes no stamps and produces no log entry.
Generated `update` increases the revision only for a supplied field that differs from its current value.

A no-op still runs the normal authorization, row-existence, and expected-revision refusals of the operation.
Matching business values never turn a stale or unauthorized request into a success.
A successful no-op returns outcome `updated` with the current row and its unchanged revision.

### Platform tables

Every `app_system` relation carries all four columns as `NOT NULL`, through the same stamp function.
The relations are `users`, `roles`, `user_roles`, `permissions`, `configurations`, and `api_keys`.
`deploy/sql/app-schema.sql` is platform-owned, so it creates their triggers directly.
Every applier of `app-schema.sql` installs `record-history.sql` first.
`user_roles.granted_at` becomes `created_at`, because it has the same meaning.
`app_system.audit_log` is deleted, because it has no reader or writer and the level-2 log replaces it.

### System database

The system database (`wamn_system`) gets the same treatment as a separate item.
That item records who issued and who revoked each `identity.pats` token.
Its relation list, actor mapping, and connection bindings are open design questions on that item.

## 4. Level 1: the stamp trigger

`deploy/sql/record-history.sql` creates schema `wamn_history` and the function `wamn_history.stamp_row()`.
The file joins the `CATALOG_SCHEMA_SQL` composition, so every applier of the catalog schema installs it.
The schema is outside every configured application schema, because introspection refuses routines there.
The function is `LANGUAGE plpgsql` with `SET search_path = pg_catalog`.
`wamn_db_owner` has `USAGE` on the schema and `EXECUTE` on the function, so apply-package can create the trigger.

The declaration is the trigger.
After the migrations apply, apply-package reads the manifest.
For each owned relation whose declaration selects at least one column, apply-package installs one trigger:

```sql
CREATE TRIGGER record_history_stamp
    BEFORE INSERT OR UPDATE ON receiving.purchase_order
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');
```

apply-package removes a `record_history_stamp` trigger on an owned relation whose declaration no longer selects a column.
Development package reconciliation runs the same step.
The trigger is derived state, like the grants that reconciliation installs from `select_fields` and `insert_fields`.
Migration policy still refuses a trigger in a migration.

Introspection reads each trigger's function, timing, events, and arguments.
It admits exactly the `record_history_stamp` shape and records it in the catalog IR.
Every other trigger is still refused.
apply-package reads the installed triggers through introspection and makes sure that they match the declarations.

The function does the following:

- It reads the actor as `NULLIF(current_setting('app.user_id', true), '')::uuid` and the time as `transaction_timestamp()`.
- On insert, it sets every selected column.
- On update, it keeps the created pair from `OLD`.
- On update, it compares the row with `OLD`, without the selected stamp columns. If they differ, it sets the updated pair. Otherwise it keeps the `OLD` stamps.
- It raises SQLSTATE `55000` with the message `actor-required` for any write with no bound actor, whatever the column selection is.
- It reads no users row.

Selected columns are server-owned, so an operation refuses a supplied stamp field with `invalid_input`.
A route whose input schema closes its objects refuses the field earlier with `schema-invalid`.
On every route, a `schema-invalid` response carries the RFC 6901 pointer of the offending value as one body field, with no WIT change:

```json
{"error":{"code":"schema-invalid","data":{"pointer":"/0/change/created_by"}}}
```

For an unexpected property, the pointer names that property.
An unparseable payload carries the root pointer `""`.
A platform-side cause, such as a missing or uncompiled schema, carries no pointer.
The client reports the pointer.

The claims fence still refuses every production reader of `app.role`.
For `app.user_id`, it refuses authorization and RLS readers and admits the platform trigger functions by name.

The trust boundary covers both levels.
Correct stamping, complete logging, and the absence of log-editing operations hold for writes through the supported platform paths.
On those paths the triggers are installed, and every writer binds its executing principal.
This feature provides no tamper resistance against modified application code or administrative SQL.
Until `wamn-0h0g.22` replaces caller-settable authority, modified application SQL can forge actor attribution.

## 5. Level 2: the audit log

The guarantee is bounded historical state.
If a complete chain and a starting state remain, a reader can reconstruct a historical state at a retained per-row position.
A live row reconstructs backward from its current contents, so a row whose insert image expired still reconstructs across its retained diffs.
A deleted row reconstructs only from its retained `before` image.
After that image expires, the contents of the deleted row are unavailable.
The log reports older or incomplete history as unavailable.
No wall-clock "as of" query exists, because transaction timestamps do not order concurrent changes.
An intermediate state inside one transaction was never visible to another caller.

`wamn_history.log_row_change()` is an `AFTER INSERT OR UPDATE OR DELETE` trigger function in the same deploy SQL file.
apply-package installs a `record_history_log` trigger for each owned relation whose retention is not `"none"`.
A retention of `"none"` removes that trigger.
Introspection admits exactly the `record_history_log` shape, as it admits the stamp trigger.
The function raises `actor-required` for a write with no bound actor, as the stamp function does.
The function writes the entry in the same transaction as the change.

- An entry holds the relation, the row key, the operation token, the actor, and the time. It also holds `recorded_at`, the kind of change, and `before` and `after` as JSONB. The actor comes from `app.user_id`, and the time comes from `transaction_timestamp()`. `recorded_at` is the time the entry was written, and retention expires entries by it.
- An insert records the complete resulting row, including defaults, in `after` and has no `before`. That row is the starting state for reconstruction.
- An update records the prior and resulting values of the changed columns, including changes to the stamps and the revision column.
- A delete records the full row in `before` and has no `after`.
- On a shared relation, the trigger reads `OLD` and `NEW`, which hold the effective base and overlay row. An overlay change therefore captures the complete row without widening the operation's public input or result.
- Every entry records an actor, because every write has one. The `columns` selection controls row metadata only, not log contents.
- JSONB fits because PostgreSQL compresses large values without custom encoding, a diff keeps ordinary entries small, and the normal JSON operators query it.
- Each row change writes one entry. A true no-op writes no stamps and no entry.
- A per-row sequence column orders entries. The transaction timestamp does not order concurrent changes and repeats within one transaction.
- A rolled-back change leaves no entry. A failure while writing the entry rolls back the change. An idempotent replay that returns the original result appends nothing.
- The log table belongs to the package that owns the relation, and its migration creates it. An overlay change writes there without declaring anything.
- No generated `update` or `delete` exists over the log table, so no application operation can alter an entry. The retention task is the only writer that removes entries.
- Each relation declares its retention, and a scheduled platform task that binds `wamn:audit-retention` runs it. Relations with different retention values can share a table, and each value holds. Expiry is by `recorded_at`. The per-row sequence decides the oldest removable prefix and the reconstruction boundary. Retention removes a prefix of a row's history, never an interior entry. It marks the remaining history as truncated where a chain breaks, so incomplete history never looks complete.
- Reading the log is a declared operation of the application, so permissions apply. The projection carries business values copied into the log, so a grant of it is a new read surface.
- `users`, `roles`, `permissions`, and `user_roles` in `app_system` keep their log with `"unlimited"` retention.

`wamn-emtx.10` owns the open level-2 questions:

- Retention authority and scheduling.
- Log insert authority.
- The reconstruction read and the read grant.
- The ISO 8601 subset and the truncation marker.
- How the trigger reads the operation token: a new host-bound `app.operation` claim, or no token.
- Whether an entry records the event causation of a platform write.
- A delete path for applications.
- Retention for the other `app_system` relations, and where platform-owned relations declare their log.

## 6. What this is not

- Level 1 is not an audit log. It is the metadata of the current row. Level 2 is the log.
- It is not authorization. A stamped actor shows nothing about permission.
- It is a trigger. The database owns integrity with no decision in it, and Rust owns every decision.
- It adds a type to users rows and derived ids for platform components. It adds no principal store. `wamn-0h0g.9` owns production provisioning of person and service rows and the credential refusal.

## 7. Tests: level 1

1. A model that selects all four columns: an insert stamps four, an update stamps the updated pair and keeps the created pair, and one transaction stamps one instant.
2. A caller that supplies `created_by` on a closed route schema is refused, and the response body carries the JSON pointer. The operation refuses the same field with `invalid_input`. A service principal stamps its own id.
3. A timestamps-only selection behaves as test 1 with no actor column.
4. A person's update followed by a platform write stamps the platform principal, and a nested call under that platform write stamps the same principal. A write with no bound actor raises SQLSTATE `55000` with `actor-required` on a timestamps-only relation as well, and the caller sees `internal_error`. The host refuses a transactional statement with no executing principal.
5. A platform row requires type `platform`, a `wamn:<component>` display name, and its pinned id. A person or service row cannot carry a `wamn:` name. `wamn_app` cannot insert or change a users row. The catalog and the generator refuse package id `wamn`.
6. Generation refuses each case in section 2, including a nullable selected column and an unselected reserved-name column.
7. `record_receipt` stamps `purchase_order` and `receipt` with one instant, so the trigger stamps real business commands, not only CRUD. Authored SQL that supplies `created_at`, `created_by`, or `updated_at` ends with the trigger's values, and an update keeps the created pair.
8. A true no-op update through the generated operation changes no business value and no stamp, and the returned state shows it. A no-op with a stale revision still refuses, and an unauthorized one still refuses.
9. A replay returns the original result without refreshing stamps. A rolled-back write leaves the stamps unchanged. An upsert's update branch keeps the created pair.
10. An overlay operation writes a base relation's stamps, and the overlay declares nothing.
11. Every `app_system` relation carries the four columns and its trigger. Provisioning writes stamp `wamn:provisioning`, that row stamps itself, and operation grants stamp `wamn:apply-package`.
12. Administrative SQL without `app.user_id` is refused. With the operator's person row, it stamps that row, and a value that it supplies for a selected column is replaced.
13. apply-package installs one trigger per owned relation that selects a column and installs none for `[]`. It removes a trigger that the declaration no longer needs. Introspection refuses any other trigger. A migration that carries a trigger is still refused.
14. Release mint refuses an anonymous attachment whose wiring closure can write, and it still admits an anonymous closure that only reads.

## 8. Tests: level 2

1. Reconstruct a row's state at three retained positions, including after a delete. Then run retention, and make sure that the log reports the range that it can no longer reconstruct as unavailable.
1a. A long-lived row: insert it beyond the retention window, update it today, and run retention. The live row still reconstructs backward across its retained diffs. A deleted row whose image expired reports unavailable.
2. Repeated updates to one row write one entry per change, in per-row sequence order, and none for a true no-op.
3. Two concurrent changes produce `before` and `after` values and sequence positions that agree with the serialized row changes. No position repeats, and no committed change is missing. Aborted work can leave gaps.
4. A failure inside the log trigger rolls back the business write.
5. An idempotent replay appends no entry.
6. Retention removes expired entries and nothing else, with two relations of different retention values sharing one table. It marks truncation where a chain breaks.
7. No generated operation can update or delete an entry.
8. An overlay change on a shared relation reconstructs to the effective base and overlay row. The update entry itself is a changed-column diff, and the operation's public input and result do not change.
9. The declared read operation refuses a caller without its permission.

## 9. Work

Level 1:

- The platform stamp function in deploy SQL, and the narrowed claims fence.
- The `audit_log` declaration, its generation refusals, and the no-op revision rule.
- Trigger installation by apply-package and trigger admission in introspection.
- Executing-principal binding in the host, and the refusal of a transactional statement without one.
- Platform principals, the typed users table, platform rows, and stamping of `app_system` relations.
- The JSON pointer in route `schema-invalid` responses.
- Release-mint refusal of anonymous attachments that can write.
- Adoption in Receiving, Acme, and WMS. Fresh installs are the only supported installs, so Receiving and WMS correct `0001_initial.sql` in place and keep version `1.0.0`.
- The system database, as a separate item.

Level 2, a separate increment:

- The log function and its per-row sequence column.
- The log table in the owning package's migration, and the retention declaration.
- The retention task, its scheduling, and the truncation marker.
- The declared read operation.
- Logs for `app_system` relations.
