# Record history and audit log

**Status:** rev 6 · 2026-09-12 · five external review rounds applied; every
open choice is settled here. Two increments: **level 1**, who changed a row and
when; **level 2**, the log that keeps what changed. Level 2 has its own
acceptance and does not ride level 1's tests.

## 1. Problem

An application tracking stock, orders, or receipts must answer "who changed
this row and when," and often "what did it look like before." Neither is
possible today without per-application discipline: timestamp columns exist only
because an author wrote them into a migration, no actor is recorded anywhere in
application data, and generated CRUD cannot stamp one at all.

## 2. Declaration

Every **relation-owning** model declares the key. Overlay writers inherit it
and must not repeat it. Nothing is inferred from absence — a missing key, or a
missing entry inside it, is refused at generation.

```json
"audit_log": {
  "columns": ["created_at", "created_by", "updated_at", "updated_by"],
  "retention": "P90D"
}
```

- `columns` — always an array, drawn from the four fixed names. `[]` is off.
- `retention` — always a value: an ISO 8601 duration, `"unlimited"`, or
  `"none"` (columns only, no log).

The scaffold writes all four columns and `"retention": "none"` for a new
model — enabled logging arrives with level 2; an author
turns columns off where a row is never edited or has no actor. A missing
declaration is refused either way, so "default" means what generation scaffolds,
never what it assumes.

**Off — reference data, derived rows, claim tables:**

```json
"audit_log": { "columns": [], "retention": "none" }
```

**Captured data, written once and never edited — a movement, a scan, a count:**

```json
"audit_log": { "columns": ["created_at", "created_by"], "retention": "none" }
```

**Columns only, no log:**

```json
"audit_log": {
  "columns": ["created_at", "created_by", "updated_at", "updated_by"],
  "retention": "none"
}
```

**Timestamps only, with a 90-day log:**

```json
"audit_log": {
  "columns": ["created_at", "updated_at"],
  "retention": "P90D"
}
```

**An editable record — everything, 90-day log:**

```json
"audit_log": {
  "columns": ["created_at", "created_by", "updated_at", "updated_by"],
  "retention": "P90D"
}
```

**Everything, kept indefinitely:**

```json
"audit_log": {
  "columns": ["created_at", "created_by", "updated_at", "updated_by"],
  "retention": "unlimited"
}
```

**Application migrations own the physical columns.** Generation checks that
every selected column exists, that times are `timestamptz`, and that actor
columns match the application-user identifier type; nullability comes from the
schema and the declaration never redefines it. No column is created
automatically.

Generation refuses: a missing key or entry; a name outside the four; a selected
column absent from the schema or wrongly typed; an actor without its matching
time (`created_by` requires `created_at`, `updated_by` requires `updated_at` —
actor and time always travel as a pair); `columns: []` with a retention other
than `"none"`; a selected column declared writable. Selected columns become
server-owned automatically.

**During level-1 delivery, `retention` must be `"none"`.** A declaration
enabling the log is refused until level 2 exists, rather than accepted and
ignored.

## 3. Level 1 — the columns

Four fixed names, reserved in the naming rules: `created_at`, `created_by`,
`updated_at`, `updated_by`. Uniformity is the point — the generated operator
screens show "changed by X at Y" on any model, and two applications read alike.

**What fills them.**

- Times: the platform's canonical current time (UTC, six fractional digits),
  taken once per invocation — a nested call takes its own, each independently
  processed input item takes its own. It is the invocation time, not the commit
  time, and it orders nothing between concurrent changes.
- Actors: the **org-issued user id** the caller resolves to, the same id
  `app_system.users` keys by. The stamped value is the user, never the
  credential: the same person authenticating with a personal access token or a
  session token stamps the same value.

**Scope: supported inserts and updates.** An insert writes the created pair and
the updated pair; an update writes the updated pair only. A delete is not
stamped — a removed row has nowhere to keep them, and no soft delete is
introduced.

**A true no-op changes nothing.** An update whose business values all equal
their current values writes no stamps and produces no log entry. Stamps are not
refreshed by an assignment that changes nothing.

A no-op still runs every check the operation normally runs — authorization, row
existence, and the expected revision. Matching business values never turn a
stale or unauthorized request into a success; a stale revision refuses as
always. A successful no-op returns the current row and its unchanged revision.

**Actor and time move together.** A post-commit handler or materializer runs
with no caller by ruling. Such a write sets both: `NULL` actor where the column
permits it, otherwise the write is refused. A previous human actor is never
carried forward onto an automated change.

**Service principals are users.** A scanner station or integration credential
that writes gets an `app_system.users` row like a person, so "which station
scanned this" is answerable. Having a row does not make it human or change how
it authenticates.

**Caller-to-user mapping.** The host resolves a caller to a principal; a model
selecting an actor column additionally requires that principal's
`app_system.users` row. An identified principal whose row is missing is refused
by name — never a silent null, which is reserved for an actorless invocation.
Whether provisioning guarantees that row is a fact of the identity work, not of
this feature; a model selecting no actor column acquires no such prerequisite.

## 4. Level 1 — where stamps come from, and what that guarantees

Two requirements, distinct and separately true:

**The trust boundary, stated once and applying to both levels:** platform input
validation rejects client-supplied stamp fields. Correct stamping, complete
logging, and the absence of log-editing operations are guarantees of the
supported generated path. **This feature provides no tamper resistance against
modified application code or administrative SQL.**

The generator emits the mutation and its stamps as one statement set, executed
by the generated accessor; the statement cannot omit or override them because
the author never writes it.

| Path | Behavior |
|---|---|
| Generated `create` / `update` | The generator emits the columns into the statement. No author action. |
| Generated `delete` | Not stamped. |
| Command using the generated mutation accessor | The accessor supplies the stamps; the statement cannot omit or override them. |
| A **declared** mutation operation over the relation that does not use the generated accessor | Refused at generation, naming the accessor as the remedy. |

**The check is over declarations, not over SQL text.** Every mutation operation
an application ships is declared, and its target relation is part of that
declaration; generation refuses a declared mutation over a history-enabled
relation that does not use the generated accessor. It does **not** claim to
detect a mutation embedded in arbitrary authored SQL — that is outside the
supported history-writing path, and no parser or second statement inventory is
introduced to claim otherwise. The refusal test demonstrates the declaration
check and nothing wider.

## 5. Level 2 — the audit log

**The guarantee is bounded historical state.** Historical states can be
reconstructed at retained per-row mutation positions where a complete chain and
a starting state remain available. **A live row reconstructs backward from its
current contents**, so a row whose insert image has expired still reconstructs
across its retained diffs. A **deleted** row reconstructs only from its
retained `before` image; once that expires, its contents are unavailable. Older or incomplete history is reported as
unavailable. A deleted row's contents remain available only while its retained
history supports reconstruction. There are no wall-clock "as of" queries —
invocation timestamps do not order concurrent changes, and an intermediate
state within one transaction was never observable by another caller.

- **What a row holds:** relation, row key, the operation token, the actor and
  time **from the invocation context**, `recorded_at` (the time the entry was
  written, which is what retention expires by), the kind of change, and `before` /
  `after` as JSONB. An **insert** records the complete resulting row including
  defaults in `after` and has no `before` — that is the starting state a chain
  reconstructs from. An **update** records the changed columns' prior and
  resulting values, **including actual changes to the stamps and the revision
  column** — otherwise the recorded state would omit metadata the mutation
  changed. A **delete** records the full row in `before` and has no
  `after`. On a shared relation the full row is the **effective base plus
  overlay row**, not one writer's projection. **Capture comes from
  effective-schema generation:** the statement set is emitted against the
  base-plus-overlay schema the loop already builds, so an overlay's mutation
  captures the complete row without widening the operation's public input or
  result. Inserts and deletes capture full rows; updates remain changed-column
  diffs, and it is the reconstructed state that covers the effective row.
- **The log always records the invocation's actor when there is one**,
  whichever columns the row carries: `columns` selects row metadata only and
  does not control log contents. An actorless invocation records a null actor.
- **Why JSONB:** PostgreSQL compresses values past its own threshold with no
  custom encoding, the diff keeps ordinary entries small, and the normal JSON
  operators query it.
- **Where it is written:** the generator emits the log insert into the same
  statement set as the mutation, executed in the same transaction by the same
  accessor. The mutation and its entry are emitted together or not at all — the
  same generated-code guarantee as §4, with the same stated limit.
- **One entry per row mutation**, associated with the affected row key and its
  actual resulting values. A true no-op produces neither stamps nor an entry.
- **Ordering** is a per-row monotonic sequence in the log table, not the
  invocation timestamp, which orders nothing between concurrent changes and can
  repeat within one command.
- **Failure and replay:** a rolled-back mutation leaves no entry; a failure
  writing the entry rolls back the mutation; an idempotent replay returning the
  original result appends nothing.
- **Ownership:** the log table belongs to the package that owns the relation,
  created by its migration. An overlay's generated mutation writes there
  without declaring anything.
- **Append-only:** no generated `update` or `delete` is emitted over the log
  table, so no application operation can alter an entry. The retention task is
  the only writer that removes them.
- **Retention** is declared per relation and executed by a scheduled platform
  task; entries from relations with different settings may share a table and
  each is honored. **Expiry is by `recorded_at`; the per-row sequence decides
  the oldest removable prefix and the reconstruction boundary.** Retention
  removes a prefix of a row's history, never an interior entry, and marks the
  remaining history as truncated where a chain is broken — it never leaves
  history that looks complete and is not.
- **Reading it is a declared operation** of the application, so permissions
  apply. Its projection carries business values copied into the log; treat it
  as a new read surface when granting it.

## 6. What this is not

- **Level 1 is not an audit log** — it is the current row's metadata. Level 2
  is the log.
- Not authorization: stamping an actor proves nothing about permission.
- Not a trigger: values come from the invocation context in generated
  statements, consistent with the database owning integrity and Rust owning
  decisions.
- **No new identity model.** It uses the caller the host already resolves.
  Integration work may remain — the `app_system.users` mapping in §3 — and that
  belongs to the identity work, not here.

## 7. Tests — level 1

1. A model selecting all four: insert stamps four, update stamps two and leaves
   the created pair, a command writing several rows stamps one instant.
2. A caller supplying `created_by` is refused, field named. A service principal
   with a user row stamps its own id.
3. A timestamps-only selection behaves as above with no actor column.
4. Human update followed by an actorless update: the actor becomes null where
   permitted and the write is refused where not — both tested.
5. An identified principal with no `app_system.users` row is refused, named,
   against a model selecting an actor; a model selecting none is unaffected.
6. Generation refuses each case in §2 — including a selected column absent from
   the schema, a wrongly typed one, and an authored mutation that **omits** the
   stamps entirely.
7. A real transactional command through the accessor path: the restriction must
   be practical for business mutations, not only CRUD.
8. A true no-op update through the generated operation changes no business
   value and no stamp, and the returned state shows it — while a no-op carrying
   a stale revision still refuses, and an unauthorized one still refuses.
9. Replay returns the original result without refreshing stamps; a rolled-back
   mutation leaves them unchanged; an upsert's update branch preserves the
   created pair.
10. A base relation's stamps written through an overlay operation, the overlay
    declaring nothing.

## 8. Tests — level 2

1. Reconstruct a row's state at three retained positions, including after a
   delete; then run retention and confirm the now-unreconstructible range is
   reported unavailable rather than answered wrongly.
1a. A long-lived row: insert beyond the retention window, update today, run
   retention. The live row still reconstructs backward across its retained
   diffs; a deleted row whose image expired reports unavailable.
2. Repeated updates to one row: one entry per mutation, ordered by the per-row
   sequence, none for a true no-op.
3. **Two concurrent mutations** produce before/after values and sequence
   positions agreeing with the serialized row changes — no duplicate position,
   no missing committed change. Gaps from aborted work are permitted.
4. Failure after the business write but before the log insert: both roll back.
5. An idempotent replay appends no entry.
6. Retention removes expired entries and nothing else, with two relations of
   different settings sharing one table, and marks truncation where a chain is
   broken.
7. No generated operation can update or delete an entry.
8. An overlay mutation on a shared relation reconstructs to the effective base
   plus overlay row, with the update entry itself a changed-column diff and the
   operation's public input and result unchanged.
9. The declared read operation refuses a caller without its permission.

## 9. Work

**Level 1:** generator (declaration, schema validation, statement and accessor
emission), the invocation context's caller and per-invocation time, the
manifest schema, the no-op rule, and one application adopting it. Depends on
the `app_system.users` mapping for models selecting an actor.

**Level 2, a separate increment:** the log table in the owning package's
migration, the log insert emitted with each mutation, the per-row sequence, the
retention task and its scheduling mechanism, the truncation marker, and a
declared read operation.
