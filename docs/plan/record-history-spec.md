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

[Data access](../architecture/data-access.md#declaration) owns the `audit_log` declaration and its level-1 refusals.
Level 2 uses the `retention` value of that declaration.

- `retention` is always one of three values: `"P<n>D"`, `"unlimited"`, or `"none"`.
- `"P<n>D"` keeps entries for n whole days. The value n is a positive integer with no leading zero.
- Generation refuses `P0D`, weeks, months, years, time parts, fractions, and signs.
- `"unlimited"` keeps every entry, and `"none"` means no log.
- `columns` and `retention` are independent. A relation with `"columns": []` can keep a log, and a relation with stamp columns can have no log.

Until level 2 exists, generation refuses every retention value other than `"none"`.
Level 2 removes that refusal and the refusal of a log with `"columns": []`.
No model scaffold exists today.
A future scaffold writes all four columns and `"retention": "none"` for a new model.

A declared retention is a minimum keep window.
The retention task removes an expired entry on its next daily run.
The declaration promises nothing about removal time, backups, holds, or erasure.
Beads `wamn-0h0g.13` owns those promises.

Timestamps only, with a 90-day log:

```json
"audit_log": {
  "columns": ["created_at", "updated_at"],
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

A 30-day log with no stamp columns:

```json
"audit_log": {
  "columns": [],
  "retention": "P30D"
}
```

The retention task writes as the platform component `audit-retention`, named `wamn:audit-retention` under the [naming rules](../architecture/naming.md#reserved-names).
Level 2 adds that component to the closed component list.

## 3. System database

The system database (`wamn_system`) stamps its identity authority relations.
These relations are `identity.principals`, `identity.project_roles`, `identity.project_env_memberships`, and `identity.pats`.

- Each of these relations gets the four stamp columns and a static `record_history_stamp` trigger.
- The columns `assigned_at` and `granted_at` become `created_at`.
- The registry, the sagas, the session keys, the ops tables, and the control store get no stamps.
- `identity.principals` admits the kind `platform`. A CHECK pins each platform name and id pair.
- A platform row exists only for a component that writes in `wamn_system`. That component is `wamn:provisioning`.
- `PrincipalKind` gains `Platform`. `identity.pats` refuses a platform principal through a composite key.
- The identity issuer and wamn-ctl bind `wamn:provisioning` as the actor in each write transaction.

Beads `wamn-emtx.20` delivers the stamps only.
After level 2, Beads `wamn-emtx.24` adds a log table in `wamn_system` with the `app_system` log shape.
Its static `record_history_log` triggers carry the retention `unlimited`, and no retention task runs against `wamn_system`.

The system database has two limits:

- The issuer and the revoker of a token read `wamn:provisioning` until Beads `wamn-0h0g.9` gives issuance a person caller.
- A delete gets no stamp. Role and membership removals therefore stay unattributed until the system database log lands.

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
Generation derives an `INSERT` grant on the log table for `wamn_app` from the declaration.
Generation refuses a declared operation that inserts into or updates the log table.
The function writes the entry in the same transaction as the change.

An entry has these fields:

- The relation that changed.
- The row key.
- The operation, from `app.operation`. The column is `NOT NULL`.
- The actor, from `app.user_id`.
- One time, from `transaction_timestamp()`. Retention expires entries by this time.
- The transaction id, from `pg_current_xact_id()`. This value has 64 bits.
- The kind of change: insert, update, or delete.
- The `before` and `after` images as JSONB.
- The per-row sequence, from a sequence column that the function allocates under the row lock.

The event envelope `txid` holds the low 32 bits of the entry transaction id.
The join from an event to its entries is therefore exact within one transaction id epoch.
Causation stays on the event, and the entry does not record it.

Entry contents:

- An insert records the complete resulting row, including defaults, in `after` and has no `before`. That row is the starting state for reconstruction.
- An update records the prior and resulting values of the changed columns, including changes to the stamps and the revision column.
- A delete records the full row in `before` and has no `after`.
- On a shared relation, the trigger reads `OLD` and `NEW`, which hold the effective base and overlay row. An overlay change therefore captures the complete row without widening the operation's public input or result.
- Every entry records an actor, because every write has one. The `columns` selection controls row metadata only, not log contents.
- JSONB fits because PostgreSQL compresses large values without custom encoding, a diff keeps ordinary entries small, and the normal JSON operators query it.
- Each row change writes one entry. A true no-op writes no stamps and no entry.
- The per-row sequence orders entries. The transaction timestamp does not order concurrent changes and repeats within one transaction.
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

### 4.4 Log table and trigger installation

apply-package derives one platform-shaped log table in the package schema for each package that logs a relation.
The log table name is reserved, and generation refuses an authored relation with that name.
Introspection admits only the platform shape of the log table.
apply-package never drops a log table automatically.
Generation derives the CDC exclusion of the log table from the declaration through the existing exclusion path.
An overlay change writes to the log table of the relation owner without declaring anything.
No generated `update` or `delete` exists over the log table, so no application operation can alter an entry.

apply-package installs a `record_history_log` trigger on each owned relation whose retention is not `"none"`.
The trigger argument carries the retention value.
A retention of `"none"` removes that trigger, and the log table and its entries stay.
Introspection admits exactly the `record_history_log` shape and compares its argument with the declaration.
The log function ignores the argument.

### 4.5 Retention task

The retention task is the only writer that removes entries.

- A 14th tenant-scoped workload role family runs the task. It has a stable role, an exact grant verifier, and a denial matrix row.
- The role holds only `DELETE` and column `SELECT` on the log tables. The task does not widen `wamn_run_retention`.
- A wamn-ctl-ops verb runs the task, and the verb refuses any other login.
- The verb binds `wamn:audit-retention` as the actor and as the operation.
- The verb reads the retention of each relation from the `record_history_log` trigger argument in `pg_trigger`.
- The verb removes entries older than n whole days by entry time.
- Retention removes a prefix of a row's history, never an interior entry. The per-row sequence decides the oldest removable prefix.
- Relations with different retention values can share one log table, and each value holds.

An example CronJob and Secret go beside [`run-retention.example.yaml`](../../deploy/platform/run-retention.example.yaml) and [`run-retention-db.example.yaml`](../../deploy/platform/run-retention-db.example.yaml).
The schedule sets the retention precision: an expired entry goes on the next daily run.

Truncation has no stored marker.
If the oldest retained entry of a row is its insert, the history of that row is complete.
Otherwise, the history of that row is incomplete.
The fold reports every position before the oldest retained entry as unavailable.

### 4.6 History read

The history read is an ordinary public projection with its own token, and a grant of it works like any other grant.
The read returns bounded pages of the entries of one row in sequence order, plus the current row.
The authored fields of the projection decide which prior data it shows.
The log carries copied business values, so a grant of the read is a new read surface.

One pure Rust fold in a shared platform crate returns the state of a row at a per-row position, or unavailable.
Guests, the client, and tests use that fold.

Level 2 adds no application delete path.
Beads `wamn-cy2q` owns application row deletes.

### 4.7 The app_system log

[`deploy/sql/app-schema.sql`](../../deploy/sql/app-schema.sql) adds the `app_system` log table in the `app_system` schema under its tenant floor.
The `wamn_history` schema keeps functions only.
The file installs a static `record_history_log` trigger with the argument `unlimited` on each `app_system` relation.
These relations are `users`, `roles`, `user_roles`, `permissions`, `configurations`, and `api_keys`.
The static triggers have the same shape that apply-package installs, and the project-state Postgres test pins them.
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
2. Repeated updates to one row write one entry per change, in per-row sequence order, and none for a true no-op.
3. Two concurrent changes produce `before` and `after` values and sequence positions that agree with the serialized row changes. No position repeats, and no committed change is missing. Aborted work can leave gaps.
4. A failure inside the log trigger rolls back the business write.
5. An idempotent replay appends no entry.
6. Retention removes expired entries and nothing else, with two relations of different retention values sharing one table. It writes no marker, and the fold reports each position before the oldest retained entry as unavailable.
7. No generated operation can update or delete an entry.
8. An overlay change on a shared relation reconstructs to the effective base and overlay row. The update entry itself is a changed-column diff, and the operation's public input and result do not change.
9. The history read refuses a caller without its grant.
10. A write with no bound `app.operation` raises `operation-required`. The operation CHECK accepts an operation token, `wamn:<component>`, and `admin:<kebab-purpose>`, and it refuses every other value.
11. The low 32 bits of the entry transaction id equal the `txid` of the event envelope that the same transaction writes.
12. A relation with `"columns": []` and a non-none retention writes log entries and no stamps.
13. Generation derives the CDC exclusion of the log table, and CDC publishes no log entry.
14. A change of retention to `"none"` removes the log trigger. The log table and its entries stay.
15. The retention verb refuses a login that is not in the audit retention role family.

## 7. Work

Level 2, a separate increment:

- Bind app.operation for every writer (`wamn-emtx.23`).
- Add the platform log trigger function (`wamn-emtx.11`).
- Declare the package log relation, retention values, and log trigger (`wamn-emtx.12`).
- Run log retention as a scheduled platform task (`wamn-emtx.13`).
- Declare the history read operation and reconstruction (`wamn-emtx.14`).
- Adopt level 2 in Receiving (`wamn-emtx.15`).
- Adopt level 2 in app_system relations (`wamn-emtx.21`).
- Move landed level-2 behavior into docs and archive the plan (`wamn-emtx.16`).

The system database:

- Stamp system database relations (`wamn-emtx.20`).
- Keep a level-2 log of system database identity relations (`wamn-emtx.24`).
