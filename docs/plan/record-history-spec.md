# Record history log

This plan describes the unbuilt parts of record history.
Level 1 records who changed a row and when, and it is current behavior in [data access](../architecture/data-access.md#record-history).
Level 2 keeps a log of what changed.
Level 2 has its own acceptance.
Beads epic `wamn-emtx` records decisions and implementation status.

## 1. Problem

An application that tracks stock, orders, or receipts must show who changed a row and when.
It often must also show what the row looked like before the change.
Level 1 stamps who changed a row and when.
No platform record shows what a row looked like before a change.

## 2. Retention

[Data access](../architecture/data-access.md#declaration) owns the `retention` value of the `audit_log` declaration and its refusals.

No model scaffold exists today.
A future scaffold writes all four columns and `"retention": "none"` for a new model.

A declared retention is a minimum keep window.
The retention task removes an expired entry on its next daily run.
The declaration promises nothing about removal time, backups, holds, or erasure.
Beads `wamn-0h0g.13` owns those promises.

The retention task writes as the platform component `audit-retention`, named `wamn:audit-retention` under the [naming rules](../architecture/naming.md#reserved-names).
Level 2 adds that component to the closed component list.

## 3. System database

The system database stamps its identity authority relations, as [data access](../architecture/data-access.md#system-database) describes.
After level 2, Beads `wamn-emtx.24` adds a history table for each of the four identity relations, with no tenant column.
Its static `record_history_log` triggers carry the retention `unlimited`, and no retention task runs against `wamn_system`.

## 4. Level 2: the audit log

### 4.1 Guarantee

The guarantee is bounded historical state.
If a complete chain and a starting state remain, a reader can reconstruct a historical state at a retained per-row position.
A live row reconstructs backward from its current contents, so a row whose insert image expired still reconstructs across its retained diffs.
A deleted row reconstructs only from its retained `before` image.
After that image expires, the contents of the deleted row are unavailable.
The log reports older or incomplete history as unavailable.
No wall-clock "as of" query exists, because transaction timestamps do not order concurrent changes.
An intermediate state inside one transaction was never visible to another caller.

### 4.2 Log trigger

`wamn_history.log_row_change()` is an `AFTER INSERT OR UPDATE OR DELETE` row trigger function in [`deploy/sql/record-history.sql`](../../deploy/sql/record-history.sql).
The function is `SECURITY INVOKER`, so it writes with the authority of the caller and keeps the `current_user` tenant floor.
The function writes the entry in the same transaction as the change.

Each logged relation has its own history table, as section 4.4 describes.
An entry has these columns:

- `position`: the per-row position, a `bigint` identity value that the function allocates under the row lock. Positions rise for each row, and gaps are allowed.
- `tenant_id`: the tenant of the row, only in a history table under a tenant floor.
- `row_key`: a JSONB object of the primary key columns.
- `kind`: `insert`, `update`, or `delete`.
- `operation`: the source of the write, from `app.operation`.
- `changed_by`: the actor, from `app.user_id`.
- `changed_at`: the one time of the entry, from `transaction_timestamp()`. Retention expires entries by this time.
- `transaction_id`: the 64-bit value of `pg_current_xact_id()`, stored as `bigint`.
- `before` and `after`: the JSONB images, rendered by `wamn_history.row_image`.

Every column is `NOT NULL`, and the primary key is `(row_key, position)`.

The event envelope `txid` holds the low 32 bits of the entry transaction id.
The join from an event to its entries is therefore exact within one transaction id epoch.
Causation stays on the event, and the entry does not record it.

Entry contents:

- An insert records the complete resulting row, including defaults, in `after`, and its `before` is `{}`. That row is the starting state for reconstruction.
- An update records the prior and resulting values of the changed columns, including changes to the stamps and the revision column.
- A delete records the full row in `before`, and its `after` is `{}`.
- An update that changes a primary key column writes a delete entry under the old key and an insert entry under the new key. The shared transaction id links the two entries.
- On a shared relation, the trigger reads `OLD` and `NEW`, which hold the effective base and overlay row. An overlay change therefore captures the complete row without widening the operation's public input or result.
- Every entry records an actor, because every write has one. The `columns` selection controls row metadata only, not log contents.
- JSONB fits because PostgreSQL compresses large values without custom encoding, a diff keeps ordinary entries small, and the normal JSON operators query it.
- Each row change writes one entry. A true no-op writes no stamps and no entry.
- The triggers compare JSONB text, so a change of numeric scale alone is a change. It moves the stamps and writes an entry, and the generated `update` bumps the revision.
- `wamn_history.row_image(record)` is the one rendering of a row image. It runs with `TimeZone` set to UTC inside the function and spells each `timestamptz` as the platform canonicalizer spells it.
- The per-row position orders entries. The transaction timestamp does not order concurrent changes and repeats within one transaction.
- A rolled-back change leaves no entry. A failure while writing the entry rolls back the change. An idempotent replay that returns the original result appends nothing.

### 4.3 Operation

Every writer binds `app.operation` to name the source of the write.

- The host binds the executing operation token.
- A platform writer binds `wamn:<component>`: `wamn:provisioning`, `wamn:apply-package`, or `wamn:audit-retention`.
- Administrative SQL binds `admin:<kebab-purpose>`.

`app.operation` follows the bind-and-clear rule of `app.user_id`.
Until Beads `wamn-0h0g.22` replaces caller-settable authority, modified application SQL can forge it, as it can forge `app.user_id`.
The claims fence admits `wamn_history.log_row_change` as a reader of `app.operation`.
An unbound `app.operation` raises SQLSTATE `55000` with the message `operation-required`.
An unbound `app.user_id` raises `actor-required`, as the stamp function does.
A CHECK on the operation column accepts three shapes only: an operation token, `wamn:<component>`, and `admin:<kebab-purpose>`.
The CHECK matches each shape by its grammar pattern.
The platform binding owns the closed component list, so a new component needs no change to a history table.

### 4.4 History tables and trigger installation

[Data access](../architecture/data-access.md#history-tables-and-the-log-trigger) describes the history tables of package relations, the log trigger installation, and the generator description.
`app-schema.sql` also calls `wamn_history.create_history_table`, with the tenant flag, as section 4.7 describes.
`system-schema.sql` calls it without the tenant flag, as section 3 describes.
The history read of section 4.6 uses the generator description of the history table.
An overlay change to a base relation writes to the history table of that relation without declaring anything.
No generated `update` or `delete` exists over a history table, so no application operation can alter an entry.

### 4.5 Retention task

The retention task is the only writer that removes entries.

- A 14th tenant-scoped workload role family runs the task. It has a stable role, an exact grant verifier, and a denial matrix row. The family is not a `wamn_platform` member.
- The role holds schema `USAGE`, `DELETE`, and `SELECT (row_key, position, changed_at)`. It holds them only on history tables whose `record_history_log` argument is `P<n>D`. The task does not widen `wamn_run_retention`.
- apply-package grants and revokes these privileges in the same transaction that installs, changes, or removes the log trigger. The deploy SQL applier creates the stable role under the bootstrap lock, like `wamn_run_retention`.
- The grant verifier reads `pg_trigger`, the same source that the verb reads, so the grants and the verb cannot disagree.
- The verb and the apply-package trigger reconciliation take one shared per-database advisory lock. A retention change therefore cannot land between the read of the verb and its delete.
- A wamn-ctl-ops verb runs the task, and the verb refuses any other login.
- The verb binds `wamn:audit-retention` as the actor and as the operation.
- The verb reads the retention of each relation from the `record_history_log` trigger argument in `pg_trigger`.
- The verb removes entries older than n whole days by entry time.
- Retention removes a prefix of a row's history, never an interior entry. The per-row sequence decides the oldest removable prefix.

An example CronJob and Secret go beside [`run-retention.example.yaml`](../../deploy/platform/run-retention.example.yaml) and [`run-retention-db.example.yaml`](../../deploy/platform/run-retention-db.example.yaml).
The schedule sets the retention precision: an expired entry goes on the next daily run.

Truncation has no stored marker.
If the oldest retained entry of a row is its insert, the history of that row is complete.
Otherwise, the history of that row is incomplete.
The fold reports every position before the oldest retained entry as unavailable.

### 4.6 History read

The history read is an ordinary public projection with its own token, and a grant of it works like any other grant.
The authored fields of the projection decide which prior data it shows.
The log carries copied business values, so a grant of the read is a new read surface.

The read is one flat `bounded_list` projection, driven by the typed key inputs, with the inputs `after_position` and `limit`.
It returns the entries of one row in ascending position order.
Each result row carries one entry, the current row image, and the head position, so one snapshot serves the fold.
No side of the result is nullable.
The current image of a deleted row is `'{}'`, the same as the `after` of its last entry.
The fold reads the `kind` of that entry to know that the row is gone.
A row with no retained entries returns an empty page, and the current row is one ordinary read away.

Images and the current row travel as text fields that hold JSONB text.
The current row comes from `wamn_history.row_image`, so it has the same spelling as the images.

One pure Rust fold in a shared platform crate returns the state of a row at a per-row position, or unavailable.
The fold keeps each column value as raw JSON text and replaces whole values, so numeric scale and every other spelling survive.
Guests, the client, and tests use that fold.
Beads `wamn-emtx.14` tests the read against a generator fixture, and Beads `wamn-emtx.15` authors the first application history read in Receiving.

Level 2 adds no application delete path.
Beads `wamn-cy2q` owns application row deletes.

### 4.7 The app_system log

[`deploy/sql/app-schema.sql`](../../deploy/sql/app-schema.sql) creates the history table of each `app_system` relation with the tenant flag, so each history table sits under the tenant floor.
These relations are `users`, `roles`, `user_roles`, `permissions`, `configurations`, and `api_keys`.
The file installs a static `record_history_log` trigger with the argument `unlimited` on each relation.
The static triggers have the same shape that apply-package installs, and the project-state Postgres test pins them.
The `wamn_history` schema keeps functions only.

The log copies full rows, including `api_keys.key_hash`.
A column that is secret at rest does not belong in a stamped relation.

### 4.8 Trust boundary

The trust boundary of level 1 also covers the log.
Complete logging and the absence of log-editing operations hold for writes through the supported platform paths.
The [record history limits](../architecture/data-access.md#limits) apply to the log as well.

Beads epic `wamn-emtx` records the owner rulings for level 2 and the system database.

## 5. What this is not

- Level 1 is not an audit log. It is the metadata of the current row. Level 2 is the log.
- It is not authorization. A stamped actor shows nothing about permission.
- It is a trigger. The database owns integrity with no decision in it, and Rust owns every decision.

## 6. Tests: level 2

1. Reconstruct a row's state at three retained positions, including after a delete. Then run retention, and make sure that the log reports the range that it can no longer reconstruct as unavailable.
1a. A long-lived row: insert it beyond the retention window, update it today, and run retention. The live row still reconstructs backward across its retained diffs. A deleted row whose image expired reports unavailable.
2. Repeated updates to one row write one entry per change, in per-row position order, and none for a true no-op.
3. Two concurrent changes produce `before` and `after` values and sequence positions that agree with the serialized row changes. No position repeats, and no committed change is missing. Aborted work can leave gaps.
4. A failure inside the log trigger rolls back the business write.
5. An idempotent replay appends no entry.
6. Retention removes expired entries and nothing else, with two relations of different retention values. It writes no marker, and the fold reports each position before the oldest retained entry as unavailable.
7. No generated operation can update or delete an entry.
8. An overlay change on a shared relation reconstructs to the effective base and overlay row. The update entry itself is a changed-column diff, and the operation's public input and result do not change.
9. The history read refuses a caller without its grant.
10. A write with no bound `app.operation` raises `operation-required`. The operation CHECK accepts an operation token, `wamn:<component>`, and `admin:<kebab-purpose>`, and it refuses every other value.
11. The low 32 bits of the entry transaction id equal the `txid` of the event envelope that the same transaction writes.
12. A relation with `"columns": []` and a non-none retention writes history entries and no stamps.
13. Generation derives the CDC exclusion of each history table, and CDC publishes no entry.
14. A change of retention to `"none"` removes the log trigger. The history table and its entries stay.
15. The retention verb refuses a login that is not in the audit retention role family.
16. An update that changes a primary key column writes a delete entry under the old key and an insert entry under the new key. Both entries carry one transaction id.
17. Generation refuses an authored relation whose name ends with `_history`, and a relation whose derived history object name has 64 bytes or more.
18. A numeric scale-only update bumps the revision, moves the stamps, and writes an entry.
19. The output of `wamn_history.row_image` spells each `timestamptz` exactly as the platform canonicalizer spells it.
20. The retention role holds no grant on a history table whose retention is `unlimited`, and the audit retention family is not a `wamn_platform` member.

## 7. Work

Level 2, a separate increment:

- Bind app.operation for every writer (`wamn-emtx.23`).
- Add the platform log trigger function (`wamn-emtx.11`).
- Declare retention values, history tables, and the log trigger (`wamn-emtx.12`).
- Run log retention as a scheduled platform task (`wamn-emtx.13`).
- Declare the history read operation and reconstruction (`wamn-emtx.14`).
- Adopt level 2 in Receiving (`wamn-emtx.15`).
- Adopt level 2 in app_system relations (`wamn-emtx.21`).
- Move landed level-2 behavior into docs and archive the plan (`wamn-emtx.16`).

The system database:

- Stamp system database relations (`wamn-emtx.20`).
- Keep a level-2 log of system database identity relations (`wamn-emtx.24`).
