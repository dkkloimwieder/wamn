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

- `retention` is always a value: an ISO 8601 duration, `"unlimited"`, or `"none"`. The value `"none"` means columns only, with no log.

Until level 2 exists, generation refuses every retention value other than `"none"`.
No model scaffold exists today.
A future scaffold writes all four columns and `"retention": "none"` for a new model.

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

The retention task writes as the platform component `audit-retention`, named `wamn:audit-retention` under the [naming rules](../architecture/naming.md#reserved-names).
Level 2 adds that component to the closed component list.

## 3. System database

The system database (`wamn_system`) gets the same treatment as a separate item.
That item records who issued and who revoked each `identity.pats` token.
Its relation list, actor mapping, and connection bindings are open design questions on that item.

## 4. Level 2: the audit log

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

The trust boundary of level 1 also covers the log.
Complete logging and the absence of log-editing operations hold for writes through the supported platform paths.
The [record history limits](../architecture/data-access.md#limits) apply to the log as well.

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
6. Retention removes expired entries and nothing else, with two relations of different retention values sharing one table. It marks truncation where a chain breaks.
7. No generated operation can update or delete an entry.
8. An overlay change on a shared relation reconstructs to the effective base and overlay row. The update entry itself is a changed-column diff, and the operation's public input and result do not change.
9. The declared read operation refuses a caller without its permission.

## 7. Work

Level 2, a separate increment:

- The log function and its per-row sequence column.
- The log table in the owning package's migration, and the retention declaration.
- The retention task, its scheduling, and the truncation marker.
- The declared read operation.
- Logs for `app_system` relations.

The system database item is separate work.
