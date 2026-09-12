# Summary

`packages/dock` is a new wamn package that books dock appointments. It ships one
component, `components/application/dock`, and five public operations:
`carrier.create`, `dock.create`, `appointment.book`, `appointment.check_in` and
`appointment.query`.

`wamn dev --config "$WAMN_DEV_CONFIG" --overlay-root packages/dock` completes all
twelve stages through Activate. Every operation the scenario names ran against
the activated release, and every invariant from DOCK-0 to DOCK-6 held.

The package declares no base dependency. It owns four relations in the
`receiving` schema: `carrier`, `dock`, `appointment`, and the CDC-excluded claim
relation `book_appointment_command`.

Two appointments on one dock never overlap because a booking locks the dock row
before it probes the neighbours, inside one transaction per input. A PostgreSQL
exclusion constraint states the same rule in the database, but it needs the
`btree_gist` extension, and the migration validator admits no
`CREATE EXTENSION`. The `# Decisions` section records that trade.

# Changes

All new or edited files sit inside the `allowed_paths` of `task.json`.

```
packages/dock/wamn.json
packages/dock/migrations/0001_initial.sql
packages/dock/command/carrier_create/insert_carrier.sql
packages/dock/command/dock_create/insert_dock.sql
packages/dock/command/appointment_book/claim_command.sql
packages/dock/command/appointment_book/find_overlap.sql
packages/dock/command/appointment_book/find_replay.sql
packages/dock/command/appointment_book/insert_appointment.sql
packages/dock/command/appointment_book/load_appointment.sql
packages/dock/command/appointment_book/load_carrier.sql
packages/dock/command/appointment_book/lock_dock.sql
packages/dock/command/appointment_check_in/lock_appointment.sql
packages/dock/command/appointment_check_in/record_arrival.sql
packages/dock/query/appointment_by_slot_start.sql
packages/dock/publication/attachments.json
packages/dock/publication/components/dock.json.in
packages/dock/publication/wirings/{carrier_create,dock_create,appointment_book,appointment_check_in,appointment_query}.json
packages/dock/generated/**                     (written by the Generate stage)
components/application/dock/Cargo.toml
components/application/dock/src/{lib,error,generated,wire,operation,guest}.rs
components/application/dock/wit/deps/{wamn-dock-carrier,wamn-dock-dock,wamn-dock-appointment}/package.wit
components/Cargo.toml                          (one workspace member line)
components/Cargo.lock                          (one [[package]] stanza)
```

One local commit holds all of it. Nothing was pushed.

```
$ git log --oneline -1
701a39ed feat(dock): dock appointments as a wamn package

$ git show --stat --oneline HEAD | tail -1
 91 files changed, 3682 insertions(+)

$ git status --porcelain
```

## The shape of the package

The four relations are:

- `carrier` and `dock`, each an identity and a name.
- `appointment`, which joins one carrier to one dock for one slot, and carries
  `status`, `arrived_at` and `row_version`.
- `book_appointment_command`, the claim relation. It is declared under
  `internal_relations` with `cdc: excluded`, because it is command mechanism
  state and not a domain fact.

`appointment.book` runs one transaction per input item, in this order:

1. Read the claim row for the idempotency key. If it exists, compare the stored
   canonical command with the presented one. Equal means replay, so load and
   return the claimed appointment. Different means `idempotency_conflict`.
2. Insert the claim with `ON CONFLICT DO NOTHING`. A lost insert means a
   concurrent caller holds the key, so read the claim again and take the same
   replay branch.
3. Load the carrier. A missing carrier refuses with `carrier_not_found`.
4. Lock the dock row with `SELECT ... FOR UPDATE`. A missing dock refuses with
   `dock_not_found`.
5. Probe for an appointment on that dock whose slot overlaps. A hit refuses with
   `slot_unavailable`.
6. Insert the appointment under the identity the claim minted.

The claim mints the appointment identity with a `DEFAULT gen_random_uuid()`
column, so a replay returns the identity the first call created. The work never
mints one.

`appointment.check_in` locks the appointment row first. It refuses `not_found`
when the row is absent, and `appointment_not_scheduled` when the status already
moved on. Otherwise it writes `status = 'arrived'`, the caller's `arrived_at`,
and `row_version + 1`.

`appointment.query` is a read-only projection. It takes one `dock_id`, one `day`
as `YYYY-MM-DD`, and one `status`, and returns `{"appointments": [...]}` ordered
by `slot_start` ascending, with `id` as the tie-breaker.

# How I verified

## The component's own tests

Seventeen native unit tests cover the wire spellings, the command identity, the
envelope, and every refusal shape the manifest declares. They run on the host,
because the crate builds as `["cdylib", "rlib"]` and the WIT export shell is
behind `#[cfg(target_arch = "wasm32")]`.

```
$ cargo test --manifest-path components/Cargo.toml -p dock --all-targets

running 17 tests
test error::tests::an_internal_refusal_carries_an_empty_detail_object ... ok
test error::tests::a_refusal_carries_the_detail_its_contract_declares ... ok
test operation::tests::a_refused_item_carries_its_request_id_and_typed_code ... ok
test operation::tests::a_listed_day_is_one_array_under_one_name ... ok
test operation::tests::an_arrival_reports_the_instant_the_caller_supplied ... ok
test operation::tests::a_different_slot_under_one_key_is_a_different_identity ... ok
test operation::tests::an_unknown_member_refuses_rather_than_being_ignored ... ok
test operation::tests::an_empty_or_backwards_slot_refuses_before_any_statement_runs ... ok
test operation::tests::every_refusal_shape_the_manifest_declares_serializes_as_declared ... ok
test wire::tests::a_caller_reads_back_the_second_precision_spelling_it_sent ... ok
test wire::tests::a_nonzero_fraction_survives_the_wire_spelling ... ok
test operation::tests::an_envelope_is_one_to_one_hundred_identified_items ... ok
test wire::tests::a_day_is_a_half_open_utc_interval ... ok
test wire::tests::an_offset_timestamp_binds_as_the_instant_it_names ... ok
test operation::tests::the_command_identity_is_sorted_compact_canonical_json ... ok
test operation::tests::one_booking_has_one_identity_however_its_caller_spells_it ... ok
test wire::tests::only_a_canonical_uuid_is_admitted ... ok

test result: ok. 17 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

The build emits four dead-code warnings. All four name a `pub id` field in a
generated row struct that the operation does not read, in files under
`packages/dock/generated/wamn/`. The generator writes those structs from the declared statement rows. The row a
lock statement returns is proof of presence, not data the caller needs.

## The loop

```
$ wamn dev --config "$WAMN_DEV_CONFIG" --overlay-root packages/dock --hold
run completed: migrate,introspect,generate,build,virtualize,apply,acl,admit,gate,publish,release,activate
run served: http://127.0.0.1:42403 host=receiving.localhost
run holding
```

The Migrate and Admit stages both passed, so the loop's own fence reports read
`Migrate passed` and `Admit passed`.

After the interrupt, no verification database remained.

```
$ psql "$system_database_url" -At -c \
  "SELECT datname FROM pg_database WHERE datname = 'wamn_dev_verification_2220241';"
(empty)
```

## Every operation, against the running release

Each call sent `Host: receiving.localhost` and the route-caller PAT as a bearer
token. The transcript below is verbatim. The two concurrent pairs write their
lines at the same time, so their request and response lines interleave in the
log. Each pair still carries both requests and both responses.

```
### create-carrier (DOCK-0)
POST /carrier/create
request:  [{"request_id":"r-create-carrier","name":"Northbound Freight"}]
status:   200
response: [{"request_id":"r-create-carrier","value":{"carrier_id":"59cc8f60-e91c-4b61-b928-ea9d9fd7ba0d","name":"Northbound Freight"}}]

### create-dock (DOCK-0)
POST /dock/create
request:  [{"request_id":"r-create-dock","name":"Door 7"}]
status:   200
response: [{"request_id":"r-create-dock","value":{"dock_id":"734b250c-e3bf-4c00-ab97-282940384a95","name":"Door 7"}}]

### book-first (DOCK-2)
POST /appointment/book
request:  [{"request_id":"r-book-1","idempotency_key":"report-book-1","carrier_id":"59cc8f60-e91c-4b61-b928-ea9d9fd7ba0d","dock_id":"734b250c-e3bf-4c00-ab97-282940384a95","slot_start":"2026-10-01T09:00:00Z","slot_end":"2026-10-01T10:00:00Z"}]
status:   200
response: [{"request_id":"r-book-1","value":{"appointment_id":"d6ab78ab-904a-496c-85a2-07a71dab9aa7","arrived_at":null,"carrier_id":"59cc8f60-e91c-4b61-b928-ea9d9fd7ba0d","dock_id":"734b250c-e3bf-4c00-ab97-282940384a95","row_version":"1","slot_end":"2026-10-01T10:00:00Z","slot_start":"2026-10-01T09:00:00Z","status":"scheduled"}}]

### book-replay (DOCK-2)
POST /appointment/book
request:  [{"request_id":"r-book-replay","idempotency_key":"report-book-1","carrier_id":"59cc8f60-e91c-4b61-b928-ea9d9fd7ba0d","dock_id":"734b250c-e3bf-4c00-ab97-282940384a95","slot_start":"2026-10-01T09:00:00Z","slot_end":"2026-10-01T10:00:00Z"}]
status:   200
response: [{"request_id":"r-book-replay","value":{"appointment_id":"d6ab78ab-904a-496c-85a2-07a71dab9aa7","arrived_at":null,"carrier_id":"59cc8f60-e91c-4b61-b928-ea9d9fd7ba0d","dock_id":"734b250c-e3bf-4c00-ab97-282940384a95","row_version":"1","slot_end":"2026-10-01T10:00:00Z","slot_start":"2026-10-01T09:00:00Z","status":"scheduled"}}]

### book-changed-body (DOCK-3)
POST /appointment/book
request:  [{"request_id":"r-book-changed","idempotency_key":"report-book-1","carrier_id":"59cc8f60-e91c-4b61-b928-ea9d9fd7ba0d","dock_id":"734b250c-e3bf-4c00-ab97-282940384a95","slot_start":"2026-10-01T14:00:00Z","slot_end":"2026-10-01T15:00:00Z"}]
status:   200
response: [{"error":{"code":"idempotency_conflict","detail":{"field":"idempotency_key"}},"request_id":"r-book-changed"}]

### overlap-a (DOCK-1)
POST /appointment/book
request:  [{"request_id":"r-overlap-a","idempotency_key":"report-overlap-a","carrier_id":"59cc8f60-e91c-4b61-b928-ea9d9fd7ba0d","dock_id":"734b250c-e3bf-4c00-ab97-282940384a95","slot_start":"2026-10-01T11:00:00Z","slot_end":"2026-10-01T12:00:00Z"}]
status:   200
response: [{"request_id":"r-overlap-a","value":{"appointment_id":"a6f7e9aa-b6b2-4915-b469-83ad54ddfdc3","arrived_at":null,"carrier_id":"59cc8f60-e91c-4b61-b928-ea9d9fd7ba0d","dock_id":"734b250c-e3bf-4c00-ab97-282940384a95","row_version":"1","slot_end":"2026-10-01T12:00:00Z","slot_start":"2026-10-01T11:00:00Z","status":"scheduled"}}]

### overlap-b (DOCK-1)
POST /appointment/book
request:  [{"request_id":"r-overlap-b","idempotency_key":"report-overlap-b","carrier_id":"59cc8f60-e91c-4b61-b928-ea9d9fd7ba0d","dock_id":"734b250c-e3bf-4c00-ab97-282940384a95","slot_start":"2026-10-01T11:30:00Z","slot_end":"2026-10-01T12:30:00Z"}]
status:   200
response: [{"error":{"code":"slot_unavailable","detail":{"field":"slot_start"}},"request_id":"r-overlap-b"}]

### overlap-refuses-exactly-one/a (DOCK-1)
POST /appointment/book
### overlap-refuses-exactly-one/b (DOCK-1)
request:  [{"request_id":"r-overlap-a","idempotency_key":"report-overlap-a","carrier_id":"59cc8f60-e91c-4b61-b928-ea9d9fd7ba0d","dock_id":"734b250c-e3bf-4c00-ab97-282940384a95","slot_start":"2026-10-01T11:00:00Z","slot_end":"2026-10-01T12:00:00Z"}]
POST /appointment/book
status:   200
request:  [{"request_id":"r-overlap-b","idempotency_key":"report-overlap-b","carrier_id":"59cc8f60-e91c-4b61-b928-ea9d9fd7ba0d","dock_id":"734b250c-e3bf-4c00-ab97-282940384a95","slot_start":"2026-10-01T11:30:00Z","slot_end":"2026-10-01T12:30:00Z"}]
status:   200
response: [{"request_id":"r-overlap-a","value":{"appointment_id":"a6f7e9aa-b6b2-4915-b469-83ad54ddfdc3","arrived_at":null,"carrier_id":"59cc8f60-e91c-4b61-b928-ea9d9fd7ba0d","dock_id":"734b250c-e3bf-4c00-ab97-282940384a95","row_version":"1","slot_end":"2026-10-01T12:00:00Z","slot_start":"2026-10-01T11:00:00Z","status":"scheduled"}}]
response: [{"error":{"code":"slot_unavailable","detail":{"field":"slot_start"}},"request_id":"r-overlap-b"}]


### race/a (DOCK-1)
POST /appointment/book
### race/b (DOCK-1)
POST /appointment/book
request:  [{"request_id":"r-race-a","idempotency_key":"report-race-a","carrier_id":"59cc8f60-e91c-4b61-b928-ea9d9fd7ba0d","dock_id":"734b250c-e3bf-4c00-ab97-282940384a95","slot_start":"2026-10-01T20:00:00Z","slot_end":"2026-10-01T21:00:00Z"}]
status:   200
request:  [{"request_id":"r-race-b","idempotency_key":"report-race-b","carrier_id":"59cc8f60-e91c-4b61-b928-ea9d9fd7ba0d","dock_id":"734b250c-e3bf-4c00-ab97-282940384a95","slot_start":"2026-10-01T20:30:00Z","slot_end":"2026-10-01T21:30:00Z"}]
status:   200
response: [{"error":{"code":"slot_unavailable","detail":{"field":"slot_start"}},"request_id":"r-race-a"}]

response: [{"request_id":"r-race-b","value":{"appointment_id":"cf279581-7539-4d26-aad7-c172a9391c13","arrived_at":null,"carrier_id":"59cc8f60-e91c-4b61-b928-ea9d9fd7ba0d","dock_id":"734b250c-e3bf-4c00-ab97-282940384a95","row_version":"1","slot_end":"2026-10-01T21:30:00Z","slot_start":"2026-10-01T20:30:00Z","status":"scheduled"}}]

### check-in (DOCK-4)
POST /appointment/check_in
request:  [{"request_id":"r-check-in","appointment_id":"d6ab78ab-904a-496c-85a2-07a71dab9aa7","arrived_at":"2026-10-01T09:07:00Z"}]
status:   200
response: [{"request_id":"r-check-in","value":{"appointment_id":"d6ab78ab-904a-496c-85a2-07a71dab9aa7","arrived_at":"2026-10-01T09:07:00Z","carrier_id":"59cc8f60-e91c-4b61-b928-ea9d9fd7ba0d","dock_id":"734b250c-e3bf-4c00-ab97-282940384a95","row_version":"2","slot_end":"2026-10-01T10:00:00Z","slot_start":"2026-10-01T09:00:00Z","status":"arrived"}}]

### check-in-unknown (DOCK-5)
POST /appointment/check_in
request:  [{"request_id":"r-check-in-unknown","appointment_id":"00000000-0000-0000-0000-000000000000","arrived_at":"2026-10-01T09:07:00Z"}]
status:   200
response: [{"error":{"code":"not_found","detail":{"field":"appointment_id","id":"00000000-0000-0000-0000-000000000000"}},"request_id":"r-check-in-unknown"}]

### check-in-twice (status moves forward only)
POST /appointment/check_in
request:  [{"request_id":"r-check-in-twice","appointment_id":"d6ab78ab-904a-496c-85a2-07a71dab9aa7","arrived_at":"2026-10-01T09:09:00Z"}]
status:   200
response: [{"error":{"code":"appointment_not_scheduled","detail":{"field":"appointment_id","id":"d6ab78ab-904a-496c-85a2-07a71dab9aa7"}},"request_id":"r-check-in-twice"}]

### list-one-dock-one-day (DOCK-6)
POST /appointment/query
request:  [{"request_id":"r-query","dock_id":"734b250c-e3bf-4c00-ab97-282940384a95","day":"2026-10-01","status":"scheduled"}]
status:   200
response: [{"request_id":"r-query","value":{"appointments":[{"appointment_id":"a6f7e9aa-b6b2-4915-b469-83ad54ddfdc3","arrived_at":null,"carrier_id":"59cc8f60-e91c-4b61-b928-ea9d9fd7ba0d","dock_id":"734b250c-e3bf-4c00-ab97-282940384a95","row_version":"1","slot_end":"2026-10-01T12:00:00Z","slot_start":"2026-10-01T11:00:00Z","status":"scheduled"},{"appointment_id":"cf279581-7539-4d26-aad7-c172a9391c13","arrived_at":null,"carrier_id":"59cc8f60-e91c-4b61-b928-ea9d9fd7ba0d","dock_id":"734b250c-e3bf-4c00-ab97-282940384a95","row_version":"1","slot_end":"2026-10-01T21:30:00Z","slot_start":"2026-10-01T20:30:00Z","status":"scheduled"}]}}]

### list-arrived (DOCK-6 status filter)
POST /appointment/query
request:  [{"request_id":"r-query-arrived","dock_id":"734b250c-e3bf-4c00-ab97-282940384a95","day":"2026-10-01","status":"arrived"}]
status:   200
response: [{"request_id":"r-query-arrived","value":{"appointments":[{"appointment_id":"d6ab78ab-904a-496c-85a2-07a71dab9aa7","arrived_at":"2026-10-01T09:07:00Z","carrier_id":"59cc8f60-e91c-4b61-b928-ea9d9fd7ba0d","dock_id":"734b250c-e3bf-4c00-ab97-282940384a95","row_version":"2","slot_end":"2026-10-01T10:00:00Z","slot_start":"2026-10-01T09:00:00Z","status":"arrived"}]}}]

### list-other-day (DOCK-6 day filter)
POST /appointment/query
request:  [{"request_id":"r-query-other-day","dock_id":"734b250c-e3bf-4c00-ab97-282940384a95","day":"2026-10-02","status":"scheduled"}]
status:   200
response: [{"request_id":"r-query-other-day","value":{"appointments":[]}}]

### book-unknown-carrier
POST /appointment/book
request:  [{"request_id":"r-unknown-carrier","idempotency_key":"report-unknown-carrier","carrier_id":"00000000-0000-0000-0000-000000000000","dock_id":"734b250c-e3bf-4c00-ab97-282940384a95","slot_start":"2026-10-03T09:00:00Z","slot_end":"2026-10-03T10:00:00Z"}]
status:   200
response: [{"error":{"code":"carrier_not_found","detail":{"field":"carrier_id","id":"00000000-0000-0000-0000-000000000000"}},"request_id":"r-unknown-carrier"}]

### book-unknown-dock
POST /appointment/book
request:  [{"request_id":"r-unknown-dock","idempotency_key":"report-unknown-dock","carrier_id":"59cc8f60-e91c-4b61-b928-ea9d9fd7ba0d","dock_id":"00000000-0000-0000-0000-000000000000","slot_start":"2026-10-03T09:00:00Z","slot_end":"2026-10-03T10:00:00Z"}]
status:   200
response: [{"error":{"code":"dock_not_found","detail":{"field":"dock_id","id":"00000000-0000-0000-0000-000000000000"}},"request_id":"r-unknown-dock"}]

### book-backwards-slot
POST /appointment/book
request:  [{"request_id":"r-backwards","idempotency_key":"report-backwards","carrier_id":"59cc8f60-e91c-4b61-b928-ea9d9fd7ba0d","dock_id":"734b250c-e3bf-4c00-ab97-282940384a95","slot_start":"2026-10-03T10:00:00Z","slot_end":"2026-10-03T09:00:00Z"}]
status:   200
response: [{"error":{"code":"invalid_input","detail":{"field":"slot_end"}},"request_id":"r-backwards"}]

```

### What each invariant rests on

| id | evidence |
|---|---|
| DOCK-0 | `create-carrier` returns `carrier_id`, `create-dock` returns `dock_id`, and every later call addresses them by those values. |
| DOCK-1 | `overlap-b` refuses `slot_unavailable`. The concurrent pair refuses exactly one. The database holds no overlapping pair. |
| DOCK-2 | `book-replay` returns the `appointment_id` that `book-first` returned, and the claim count equals the appointment count. |
| DOCK-3 | `book-changed-body` refuses `idempotency_conflict`. |
| DOCK-4 | `check-in` returns `"status":"arrived"` and `"arrived_at":"2026-10-01T09:07:00Z"`, the exact instant the caller sent. |
| DOCK-5 | `check-in-unknown` refuses `not_found`. |
| DOCK-6 | `list-one-dock-one-day` returns two scheduled appointments in ascending `slot_start` order. `list-arrived` returns only the arrived one. `list-other-day` returns an empty list. |

### The concurrency result, summarized

The driver fired each pair with two parallel `curl` calls.

```
$ work/verify.sh report
concurrent gate pair    slot_unavailable=1 booked=1
concurrent fresh pair   slot_unavailable=1 booked=1
day list sorted by slot_start ascending: true
```

The "gate pair" repeats the two keys the exit gate uses, so its second call is a
replay. The "fresh pair" uses two keys never seen before, so the replay path
cannot decide it. The dock row lock decides it. An earlier round of five fresh
pairs gave the same answer every time.

```
round 1: booked=1 slot_unavailable=1
round 2: booked=1 slot_unavailable=1
round 3: booked=1 slot_unavailable=1
round 4: booked=1 slot_unavailable=1
round 5: booked=1 slot_unavailable=1
```

### The database, read only

```
$ psql "$target_database_url" -At -c \
  "SELECT count(*) FROM receiving.appointment a
     JOIN receiving.appointment b ON a.dock_id = b.dock_id AND a.id < b.id
    WHERE a.slot_start < b.slot_end AND a.slot_end > b.slot_start;"
0

$ psql "$target_database_url" -At -F' | ' -c \
  "SELECT (SELECT count(*) FROM receiving.book_appointment_command),
          (SELECT count(*) FROM receiving.appointment),
          (SELECT count(*) FROM receiving.book_appointment_command c
             LEFT JOIN receiving.appointment a ON a.id = c.appointment_id
            WHERE a.id IS NULL);"
11 | 11 | 0

$ psql "$target_database_url" -At -c \
  "SELECT idempotency_key FROM receiving.book_appointment_command
    WHERE idempotency_key LIKE '%unknown%' OR idempotency_key LIKE '%backwards%';"
(empty)
```

The first query is DOCK-1 stated over the stored rows. The second shows one
appointment per claim and no orphan claim. The third shows that a refused
booking rolls its claim back, so a caller can retry the same key.

### The static checks the exit gate names

```
$ grep -rlE 'rowversion|row_ver\b|version_row|rowVersion' packages/dock
(empty)

$ jq -r '[ .. | objects | (.name? // empty) ] | map(select(type == "string"))
  | map(select(test("^[a-z][a-z0-9_]*$") | not) // select(test("_v[0-9]+$")))
  | unique | join(", ")' packages/dock/wamn.json
(empty)
```

The grader resolves a route by matching the operation name against a wiring id.
All five resolve.

```
carrier.create           -> /carrier/create
dock.create              -> /dock/create
appointment.book         -> /appointment/book
appointment.check_in     -> /appointment/check_in
appointment.query        -> /appointment/query
```

## What I did not verify, and why

- The native SQL verifier. `packages/dock/generated/native-verifier/*.rs` is
  written by the Generate stage, but nothing compiles it. The one package that
  does own such a test, `tests/conformance/tests/receiving_sqlx_verifier.rs`, is
  hand written per package, and `tests/` is outside my `allowed_paths`. Every
  statement is instead proven at runtime by the calls above.
- Batches of more than one item. Every attachment admits one to one hundred
  items and the code loops over them, but every call above sent one item. The
  per-item loop is covered by a unit test, not by a live batch.
- Appointments that move to `departed`. The status vocabulary carries the
  value and the CHECK constraint admits it. The scenario names no operation
  that writes it, so this package has none.
- Load, timeouts, and the `retry` and `timeout` refusals. Those arms translate a
  host statement error that no call here produced.
- Any second wamn package installed beside this one. `packages/dock` declares no
  base dependency and ran alone.
- The whole-repository gate of record in `docs/operations/build-and-test.md`. I
  ran the component tests and the loop, not the full suite.

# Decisions

Non-overlap is held by a row lock, not by an exclusion constraint. The exact
database statement of DOCK-1 is
`EXCLUDE USING gist (dock_id WITH =, tstzrange(slot_start, slot_end) WITH &&)`.
That needs the `btree_gist` extension for the `uuid` equality operator class.
The extension is available on the server but not installed, and
`crates/schema/introspection/src/migration_policy.rs` refuses `CREATE EXTENSION`
in a migration. Installing it by hand is a change outside my `allowed_paths`
and a bypass of that fence. So every booking takes `SELECT id FROM dock WHERE id = $1 FOR UPDATE` before it
probes for an overlap, in the same transaction as the insert. All bookings on
one dock therefore serialize on that row. The cost is that the rule lives in
the command and not in the schema. A future writer that skips the lock can
break it.

`status` has no column default. The introspection IR admits a closed set of
defaults, and `'scheduled'::text` is not in it. Rather than borrow an admitted
literal with the wrong meaning, `insert_appointment` writes `'scheduled'`
itself. The command that owns the transition also owns the opening value.

`carrier.create` and `dock.create` are plain commands with no idempotency key.
DOCK-0 asks only that each returns an identity, and the exit gate sends no
replay for either. A claim relation on each is machinery the scenario does not
ask for. A redelivered create therefore creates a second carrier. This is the
one place where the package is weaker than `appointment.book`.

All five operations are `custom_operations`, not generated CRUD. The exit gate
posts a flat item, for example
`{"request_id": ..., "day": ..., "status": ...}`. A generated `query` takes a
`filter`, `sort`, `cursor` and `limit` shape instead, and a generated `create`
takes its own envelope. Custom operations let the wire shape match what the
scenario asks for.

A day is a UTC calendar day. `appointment.query` reads `day` as `YYYY-MM-DD`
and turns it into the half-open interval `[dayT00:00:00Z, day+1T00:00:00Z)`. A
dock is a physical door and its operators keep local time. The scenario names
no zone for one, and a server default zone moves every boundary appointment
without saying so.

`status` is required on `appointment.query`. DOCK-6 says the list is "filtered
by status", and the exit gate always sends one. An optional filter needs a
nullable bind and a second SQL branch, for no gain here.

Timestamps have two spellings, and `components/application/dock/src/wire.rs`
carries both. The bind spelling is RFC 3339 UTC with six fractional digits,
exactly as `wamn.json` freezes it, and it is what the command identity hashes.
The wire spelling drops a zero fraction, so a caller that sends
`2026-10-01T09:07:00Z` reads that instant back in the spelling it sent. Ordering
in a listing comes from `ORDER BY slot_start` in PostgreSQL, not from comparing
those strings.

The component and its data access are one crate. `tools/build-components`
derives the package half of its inventory from `packages/*/wamn.json`, but
`architecture/workspace-tiers.json` still pins a `package_count` per workspace,
and that file is outside my `allowed_paths`. A second crate makes the member
count disagree with `package_count` plus the derived package components, and the
build refuses. So `components/application/dock` holds the operations, the
error type, the wire spellings and the WIT export shell together.

The result contract for `appointment.query` uses `appointments[].<field>` paths.
The emitted value is `{"appointments": [...]}`. The neighbouring packages spell a
bounded list as `{"rows": [...]}` with flat contract paths. `appointments` reads
better at this wire and the contract path says where each field sits.

The keys the driver sent are namespaced. Every idempotency key begins with
`report-`, so it never collides with the `dock-gate-*` keys the exit gate uses
against the same durable database.

# Where I got stuck

Two stage failures, both fixed inside the package.

The Introspect stage refused `DEFAULT 'scheduled'` on `appointment.status`:

```
PostgreSQL introspection refused (unsupported-column-default) in schema
`receiving` for `appointment.status`: unsupported PostgreSQL default
`'scheduled'::text` for wamn:postgres type `text`
```

`crates/schema/introspection/src/ir.rs` admits a closed default vocabulary:
`gen_random_uuid()`, `CURRENT_TIMESTAMP`, `'open'`, `'not_required'`,
`'pending'`, `false`, `1` and `0`. I dropped the default and made the booking
command write the opening status.

The Build stage refused an empty `CARGO_TARGET_DIR`:

```
error: the target directory is set to an empty string in the `CARGO_TARGET_DIR`
environment variable
build-components: locked component Cargo metadata failed for components/Cargo.toml
```

The pilot's launch environment sets `CARGO_TARGET_DIR` to an empty string rather
than leaving it unset, and `tools/build-components` passes it through to cargo.
I ran `wamn dev` under `env -u CARGO_TARGET_DIR`. This is a property of the
harness, not of the package.

Nothing else blocked the work. No attempt at the same failure ran three times.

# Rules I relied on

- `services/ctl/src/dev.rs` fixes the stage order and puts the committed-source
  boundary at Apply. So the first loop run writes `packages/dock/generated/`,
  refuses at Apply on the now dirty worktree, and a commit makes the second run
  green.
- `crates/schema/generator/src/generate.rs` compares the `select_fields`,
  `insert_fields`, `update_fields` and `lock` a manifest declares against what
  it lexes out of the authored SQL. The two must be equal, so the manifest
  states the authority the SQL actually uses.
- `crates/schema/introspection/src/migration_policy.rs` admits only
  schema-qualified `CREATE TABLE` and additive `ALTER TABLE`, and requires a name
  on every constraint.
- `services/ctl/src/dev/coordinator.rs` refuses any table in the introspected
  catalog that no installed package declares as a model or an internal relation.
  All four relations are therefore declared.
- `services/ctl/src/publish_release.rs` requires each attachment's
  `definition-hash` to equal `canonical_json_sha256` of its `definition`, and
  refuses an authored `route.host`. A small script writes the attachments and
  their hashes together.
- `tools/build-components` derives the package component set from
  `packages/*/wamn.json`, and compares each workspace's member count with
  `architecture/workspace-tiers.json`. That is why exactly one crate joined the
  workspace.
- `components/data/postgres-statements/src/lib.rs` says a dropped transaction
  rolls back. That is what makes a refused booking leave no claim behind.
- The naming law in `crates/schema/generator/src/manifest.rs`: singular
  snake_case identifiers, one `module.operation` separator, and a permission
  token equal to the operation identity.

# Open questions

- Is DOCK-1 better held by a database constraint than by a command rule? A
  constraint holds it on the day a package can install `btree_gist`. Today no
  package can, because the migration validator admits no `CREATE EXTENSION` and
  no package owns the extension set of its schema. A platform owner has to say
  whether an extension is package-owned state or environment state.
- Does the admitted column-default vocabulary in
  `crates/schema/introspection/src/ir.rs` need to grow with each package? It
  names literals from the Receiving and Acme migrations. A new package with a
  new status word has to move the value into its command. That is arguably the
  better design, but the refusal reads like a gap rather than a rule.
- Is `{"appointments": [...]}` or `{"rows": [...]}` the intended envelope for a
  bounded list? Nothing in the tree fences either, and the two neighbouring
  packages both use `rows`.
- Do `carrier.create` and `dock.create` need idempotency keys? The scenario does
  not ask for them, but the platform's own command-identity law covers every
  create-shaped command. If the answer is yes, the rule belongs in the manifest
  validator, not in each author's judgment.
- Who owns a dock's time zone? A day boundary is a domain fact and this package
  had to pick UTC.
