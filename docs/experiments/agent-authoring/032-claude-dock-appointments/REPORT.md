# Summary

`packages/dock` is a wamn package that books dock appointments. It holds three
models (`carrier`, `dock`, `appointment`), four claim relations, and five public
operations: `carrier.create`, `dock.create`, `appointment.book`,
`appointment.check_in` and `appointment.query`. Its guest component is
`components/application/dock`.

`wamn dev --config "$WAMN_DEV_CONFIG" --overlay-root packages/dock` completes all
twelve stages through Activate. Every operation the scenario names ran against
the activated release, and every invariant from DOCK-0 to DOCK-6 holds.

Every command mints its identity from a claim keyed by the idempotency key, so a
replay returns the original result and writes nothing new. `appointment.book`
locks the dock row before it probes for an overlapping slot. That lock is what
makes two concurrent bookings on one dock serialize.

# Changes

All files are inside the `allowed_paths` of `task.json`.

New, under `packages/dock`:

- `wamn.json`, the package manifest for `wamn_dock@1.0.0`.
- `migrations/0001_initial.sql`, seven tables in schema `receiving`.
- `command/{carrier_create,dock_create,appointment_book,appointment_check_in}/*.sql`,
  nineteen authored statements.
- `query/appointment_query.sql`, the dispatch read.
- `publication/components/dock.json.in`, `publication/wirings/*.json`,
  `publication/attachments.json`.
- `generated/**`, written by the Generate stage and committed unedited.

New, under `components/application/dock`:

- `Cargo.toml`, the `dock` cdylib plus rlib crate.
- `src/{lib,guest,operation,error,scalar,generated}.rs` and one module per
  operation.
- `wit/deps/wamn-dock-{carrier,dock,appointment}/package.wit`.

Changed:

- `components/Cargo.toml`, one new workspace member, `application/dock`.
- `components/Cargo.lock`, the entry that member needs.

# How I verified

## The component's own tests

```
$ cargo test --manifest-path components/Cargo.toml -p dock --locked --offline
     Running unittests src/lib.rs (components/target/debug/deps/dock-6abc68eb9313df7d)

running 21 tests
test appointment_book::tests::a_different_slot_is_a_different_command ... ok
test appointment_check_in::tests::an_unspellable_scalar_is_refused_before_any_statement ... ok
test appointment_book::tests::a_slot_must_end_after_it_starts ... ok
test appointment_book::tests::an_unspellable_scalar_is_refused_before_any_statement ... ok
test appointment_check_in::tests::a_different_arrival_time_is_a_different_command ... ok
test carrier_create::tests::the_canonical_command_excludes_the_key_and_separates_two_carriers ... ok
test carrier_create::tests::an_empty_name_is_refused_before_any_statement ... ok
test appointment_check_in::tests::the_canonical_command_is_spelling_independent_and_excludes_the_key ... ok
test dock_create::tests::an_empty_name_is_refused_before_any_statement ... ok
test appointment_book::tests::the_canonical_command_is_spelling_independent_and_excludes_the_key ... ok
test dock_create::tests::the_canonical_command_excludes_the_key_and_separates_two_docks ... ok
test error::tests::a_missing_row_refusal_carries_the_field_and_the_id ... ok
test operation::tests::a_non_array_envelope_refuses_the_invocation ... ok
test operation::tests::an_empty_envelope_refuses ... ok
test operation::tests::a_refusal_serializes_as_code_and_detail_beside_its_request_id ... ok
test operation::tests::the_correlation_id_is_the_envelopes_and_the_body_is_the_items ... ok
test scalar::tests::a_status_filter_is_one_of_the_three_the_enum_holds ... ok
test scalar::tests::a_day_is_the_half_open_utc_interval_it_names ... ok
test scalar::tests::uuids_and_timestamps_are_respelled_canonically ... ok
test error::tests::every_operation_refuses_only_what_its_contract_declares ... ok
test operation::tests::an_oversized_envelope_refuses_before_any_item_runs ... ok

test result: ok. 21 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests dock

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

```

`cargo clippy --manifest-path components/Cargo.toml -p dock --offline
--all-targets` reports nothing in `application/dock/src`, and `cargo fmt
--manifest-path components/Cargo.toml -p dock -- --check` is clean.

## The loop

```
$ wamn dev --config "$WAMN_DEV_CONFIG" --overlay-root packages/dock
applied wamn_dock@1.0.0: 1 migration(s)
applied wamn_dock@1.0.0: 1 migration(s)
projected sha256:8b518d593a5a7cf2acb46972783e641ea4785128c41c3d64e241ef520d1750a3 (source-project: already converged; control: changed)
sha256:8b518d593a5a7cf2acb46972783e641ea4785128c41c3d64e241ef520d1750a3
sha256:cb3c8c4175089363835a04984f6ea230c145acb5d979b1af618e1d4fdabb2754
sha256:cb3c8c4175089363835a04984f6ea230c145acb5d979b1af618e1d4fdabb2754
2026-09-08T03:26:30.920988Z  INFO wamn-host welded to its release effective_release_id=1 manifest_digest=sha256:cb3c8c4175089363835a04984f6ea230c145acb5d979b1af618e1d4fdabb2754
run completed: migrate,introspect,generate,build,virtualize,apply,acl,admit,gate,publish,release,activate
run served: http://127.0.0.1:41021 host=receiving.localhost
```

The requests below ran against a held release:

```
$ wamn dev --config "$WAMN_DEV_CONFIG" --overlay-root packages/dock --hold
run completed: migrate,introspect,generate,build,virtualize,apply,acl,admit,gate,publish,release,activate
run served: http://127.0.0.1:32817 host=receiving.localhost
run holding
```

After the third loop run, `git status --short` prints nothing. The Generate
stage therefore reproduces the committed `generated/**` bytes exactly.

Every request below used this shell:

```
BASE="http://127.0.0.1:32817"
TOKEN="$(jq -r .stringData.token "$WAMN_ROUTE_CALLER_PAT_FILE")"
DB="postgresql://postgres@127.0.0.1:54332/wamn-db-acme--receiving--dev--7lutvnku"
```

`$DB` is the inspection URL from `dev.json`. No operation uses it. It reads what
the operations wrote.

## Every operation the scenario names

### DOCK-0 · `carrier.create` and `dock.create` return the identity later operations use

```
$ curl -sS -X POST "$BASE/carrier/create" -H "Host: $WAMN_ROUTE_HOST" \
    -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
    --data '[{"request_id":"req-carrier-1","value":{"idempotency_key":"carrier-blue-line-1","name":"Blue Line Freight"}}]'
[{"request_id":"req-carrier-1","value":{"carrier_id":"e732d0e1-218f-4f0f-a072-9eeb0457987b"}}]
HTTP 200

$ curl -sS -X POST "$BASE/dock/create" -H "Host: $WAMN_ROUTE_HOST" \
    -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
    --data '[{"request_id":"req-dock-1","value":{"idempotency_key":"dock-north-1","name":"North Bay Door 1"}},{"request_id":"req-dock-2","value":{"idempotency_key":"dock-north-2","name":"North Bay Door 2"}}]'
[{"request_id":"req-dock-1","value":{"dock_id":"7f698e6e-2e03-40d4-b240-d35a5b127b29"}},{"request_id":"req-dock-2","value":{"dock_id":"605e1353-16c9-4df3-9d74-0b381958ae88"}}]
HTTP 200

```

The second call carries two items, so it also shows that the envelope is a batch
and that each item answers beside its own `request_id`.

### `appointment.book` · three slots on one dock

```
$ curl -sS -X POST "$BASE/appointment/book" -H "Host: $WAMN_ROUTE_HOST" \
    -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
    --data '[{"request_id":"req-book-a","value":{"idempotency_key":"book-a","carrier_id":"e732d0e1-218f-4f0f-a072-9eeb0457987b","dock_id":"7f698e6e-2e03-40d4-b240-d35a5b127b29","slot_start":"2026-10-01T11:00:00+02:00","slot_end":"2026-10-01T12:00:00+02:00"}}]'
[{"request_id":"req-book-a","value":{"appointment_id":"ced16055-4f1d-44fc-9079-d5e76fd813c4","status":"scheduled"}}]
HTTP 200

$ curl -sS -X POST "$BASE/appointment/book" -H "Host: $WAMN_ROUTE_HOST" \
    -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
    --data '[{"request_id":"req-book-b","value":{"idempotency_key":"book-b","carrier_id":"e732d0e1-218f-4f0f-a072-9eeb0457987b","dock_id":"7f698e6e-2e03-40d4-b240-d35a5b127b29","slot_start":"2026-10-01T10:00:00Z","slot_end":"2026-10-01T11:00:00Z"}},{"request_id":"req-book-c","value":{"idempotency_key":"book-c","carrier_id":"e732d0e1-218f-4f0f-a072-9eeb0457987b","dock_id":"7f698e6e-2e03-40d4-b240-d35a5b127b29","slot_start":"2026-10-01T08:00:00Z","slot_end":"2026-10-01T09:00:00Z"}}]'
[{"request_id":"req-book-b","value":{"appointment_id":"d2ec419a-f3e9-4f1d-9051-6e7c45974187","status":"scheduled"}},{"request_id":"req-book-c","value":{"appointment_id":"63751744-37c5-40dc-a0c0-225b827deb95","status":"scheduled"}}]
HTTP 200

```

The first booking sent `2026-10-01T11:00:00+02:00`. The package re-spelled it to
`2026-10-01T09:00:00.000000Z` before it hashed the command and before it wrote
the row.

### DOCK-2 · a replay returns the same appointment id and writes nothing new

```
$ PGPASSWORD=REDACTED-BY-agent-pilot-scrub psql "$DB" -Atqc "select count(*) from receiving.appointment"
3
$ curl -sS -X POST "$BASE/appointment/book" -H "Host: $WAMN_ROUTE_HOST" \
    -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
    --data '[{"request_id":"req-book-a-replay","value":{"idempotency_key":"book-a","carrier_id":"e732d0e1-218f-4f0f-a072-9eeb0457987b","dock_id":"7f698e6e-2e03-40d4-b240-d35a5b127b29","slot_start":"2026-10-01T09:00:00.000000Z","slot_end":"2026-10-01T10:00:00.000000Z"}}]'
[{"request_id":"req-book-a-replay","value":{"appointment_id":"ced16055-4f1d-44fc-9079-d5e76fd813c4","status":"scheduled"}}]
HTTP 200

$ PGPASSWORD=REDACTED-BY-agent-pilot-scrub psql "$DB" -Atqc "select count(*) from receiving.appointment"
3
$ PGPASSWORD=REDACTED-BY-agent-pilot-scrub psql "$DB" -Atqc "select count(*) from receiving.appointment_book_command"
3
```

The replay spelled the same slot in UTC instead of `+02:00`. It still returned
`ced16055-4f1d-44fc-9079-d5e76fd813c4`, and the row count did not move.

### DOCK-3 · a used key with a different request refuses with `idempotency_conflict`

```
$ curl -sS -X POST "$BASE/appointment/book" -H "Host: $WAMN_ROUTE_HOST" \
    -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
    --data '[{"request_id":"req-book-a-conflict","value":{"idempotency_key":"book-a","carrier_id":"e732d0e1-218f-4f0f-a072-9eeb0457987b","dock_id":"7f698e6e-2e03-40d4-b240-d35a5b127b29","slot_start":"2026-10-01T13:00:00Z","slot_end":"2026-10-01T14:00:00Z"}}]'
[{"error":{"code":"idempotency_conflict","detail":{"field":"value.idempotency_key"}},"request_id":"req-book-a-conflict"}]
HTTP 200

```

### DOCK-1 · `slot_unavailable`, and the same slot on another dock

```
$ curl -sS -X POST "$BASE/appointment/book" -H "Host: $WAMN_ROUTE_HOST" \
    -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
    --data '[{"request_id":"req-book-overlap","value":{"idempotency_key":"book-overlap","carrier_id":"e732d0e1-218f-4f0f-a072-9eeb0457987b","dock_id":"7f698e6e-2e03-40d4-b240-d35a5b127b29","slot_start":"2026-10-01T09:30:00Z","slot_end":"2026-10-01T10:30:00Z"}}]'
[{"error":{"code":"slot_unavailable","detail":{"field":"value.slot_start","id":"ced16055-4f1d-44fc-9079-d5e76fd813c4"}},"request_id":"req-book-overlap"}]
HTTP 200

$ curl -sS -X POST "$BASE/appointment/book" -H "Host: $WAMN_ROUTE_HOST" \
    -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
    --data '[{"request_id":"req-book-dock2","value":{"idempotency_key":"book-dock2","carrier_id":"e732d0e1-218f-4f0f-a072-9eeb0457987b","dock_id":"605e1353-16c9-4df3-9d74-0b381958ae88","slot_start":"2026-10-01T09:30:00Z","slot_end":"2026-10-01T10:30:00Z"}}]'
[{"request_id":"req-book-dock2","value":{"appointment_id":"5fbd5bb5-f37a-4bd0-bbf2-3bfecc35b085","status":"scheduled"}}]
HTTP 200

```

The refusal names the appointment the request collided with, so a dispatcher
sees what already holds the dock.

### DOCK-1 under contention · eight callers, one dock, all at once

The race needs an empty dock, so it starts with one more `dock.create`.

```
$ curl -sS -X POST "$BASE/dock/create" -H "Host: $WAMN_ROUTE_HOST" \
    -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
    --data '[{"request_id":"req-dock-3","value":{"idempotency_key":"dock-north-3","name":"North Bay Door 3"}}]'
[{"request_id":"req-dock-3","value":{"dock_id":"d0e4b423-2e85-4c4e-b523-3ceeabcc392c"}}]
HTTP 200

$ send() {
    curl -sS -X POST "$BASE/appointment/book" -H "Host: $WAMN_ROUTE_HOST" \
      -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
      --data "[{\"request_id\":\"$1\",\"value\":{\"idempotency_key\":\"$1\",\"carrier_id\":\"$CARRIER\",\"dock_id\":\"$DOCK3\",\"slot_start\":\"$2\",\"slot_end\":\"$3\"}}]"
    printf '\n'
  }
$ for i in 1 2 3 4; do
    send "race-$i-a" "2026-10-02T0$i:00:00Z" "2026-10-02T0$i:45:00Z" &
    send "race-$i-b" "2026-10-02T0$i:30:00Z" "2026-10-02T0$((i+1)):15:00Z" &
  done; wait
[{"request_id":"race-2-b","value":{"appointment_id":"54642567-b2f0-41e0-ad2a-7c09d56bd1de","status":"scheduled"}}]
[{"request_id":"race-1-a","value":{"appointment_id":"2f7e66b2-ac75-4f07-bee4-a345aa412e67","status":"scheduled"}}]
[{"error":{"code":"slot_unavailable","detail":{"field":"value.slot_start","id":"54642567-b2f0-41e0-ad2a-7c09d56bd1de"}},"request_id":"race-3-a"}]
[{"error":{"code":"slot_unavailable","detail":{"field":"value.slot_start","id":"54642567-b2f0-41e0-ad2a-7c09d56bd1de"}},"request_id":"race-2-a"}]
[{"request_id":"race-4-a","value":{"appointment_id":"b2acef1e-21c3-4aa7-98e4-359664d9fe06","status":"scheduled"}}]
[{"error":{"code":"slot_unavailable","detail":{"field":"value.slot_start","id":"2f7e66b2-ac75-4f07-bee4-a345aa412e67"}},"request_id":"race-1-b"}]
[{"error":{"code":"slot_unavailable","detail":{"field":"value.slot_start","id":"b2acef1e-21c3-4aa7-98e4-359664d9fe06"}},"request_id":"race-3-b"}]
[{"error":{"code":"slot_unavailable","detail":{"field":"value.slot_start","id":"b2acef1e-21c3-4aa7-98e4-359664d9fe06"}},"request_id":"race-4-b"}]
```

Three bookings won and five refused. The three winners do not overlap each
other: `01:00-01:45`, `02:30-03:15` and `04:00-04:45`.

### DOCK-1 · the database holds no overlapping pair

```
$ PGPASSWORD=REDACTED-BY-agent-pilot-scrub psql "$DB" -c "
SELECT count(*) AS overlapping_pairs
FROM receiving.appointment AS left_side
JOIN receiving.appointment AS right_side
  ON right_side.dock_id = left_side.dock_id
 AND right_side.id <> left_side.id
 AND right_side.slot_start < left_side.slot_end
 AND right_side.slot_end > left_side.slot_start;"
 overlapping_pairs 
-------------------
                 0
(1 row)

       dock       |       slot_start       |        slot_end        |  status   |       arrived_at       
------------------+------------------------+------------------------+-----------+------------------------
 North Bay Door 1 | 2026-10-01 08:00:00+00 | 2026-10-01 09:00:00+00 | scheduled | 
 North Bay Door 1 | 2026-10-01 09:00:00+00 | 2026-10-01 10:00:00+00 | arrived   | 2026-10-01 09:07:31+00
 North Bay Door 1 | 2026-10-01 10:00:00+00 | 2026-10-01 11:00:00+00 | scheduled | 
 North Bay Door 2 | 2026-10-01 09:30:00+00 | 2026-10-01 10:30:00+00 | scheduled | 
 North Bay Door 3 | 2026-10-02 01:00:00+00 | 2026-10-02 01:45:00+00 | scheduled | 
 North Bay Door 3 | 2026-10-02 02:30:00+00 | 2026-10-02 03:15:00+00 | scheduled | 
 North Bay Door 3 | 2026-10-02 04:00:00+00 | 2026-10-02 04:45:00+00 | scheduled | 
(7 rows)

```

### DOCK-4 and DOCK-5 · `appointment.check_in`

```
$ curl -sS -X POST "$BASE/appointment/check_in" -H "Host: $WAMN_ROUTE_HOST" \
    -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
    --data '[{"request_id":"req-checkin-a","value":{"idempotency_key":"checkin-a","appointment_id":"ced16055-4f1d-44fc-9079-d5e76fd813c4","arrived_at":"2026-10-01T11:07:31+02:00"}}]'
[{"request_id":"req-checkin-a","value":{"appointment_id":"ced16055-4f1d-44fc-9079-d5e76fd813c4","arrived_at":"2026-10-01T09:07:31.000000Z","check_in_id":"b6d30bac-fe7a-4932-9ed9-0406249dbf98","status":"arrived"}}]
HTTP 200

$ curl -sS -X POST "$BASE/appointment/check_in" -H "Host: $WAMN_ROUTE_HOST" \
    -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
    --data '[{"request_id":"req-checkin-a-replay","value":{"idempotency_key":"checkin-a","appointment_id":"ced16055-4f1d-44fc-9079-d5e76fd813c4","arrived_at":"2026-10-01T09:07:31.000000Z"}}]'
[{"request_id":"req-checkin-a-replay","value":{"appointment_id":"ced16055-4f1d-44fc-9079-d5e76fd813c4","arrived_at":"2026-10-01T09:07:31.000000Z","check_in_id":"b6d30bac-fe7a-4932-9ed9-0406249dbf98","status":"arrived"}}]
HTTP 200

$ curl -sS -X POST "$BASE/appointment/check_in" -H "Host: $WAMN_ROUTE_HOST" \
    -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
    --data '[{"request_id":"req-checkin-missing","value":{"idempotency_key":"checkin-missing","appointment_id":"00000000-0000-4000-8000-000000000000","arrived_at":"2026-10-01T09:07:31Z"}}]'
[{"error":{"code":"not_found","detail":{"field":"value.appointment_id","id":"00000000-0000-4000-8000-000000000000"}},"request_id":"req-checkin-missing"}]
HTTP 200

```

Check-in moved the appointment to `arrived` and stored the arrival time the
caller supplied, re-spelled to UTC. The replay returned the same `check_in_id`
and the same `arrived_at`. A check-in against an appointment that does not exist
refused with `not_found`.

### DOCK-6 · `appointment.query`

```
$ curl -sS -X POST "$BASE/appointment/query" -H "Host: $WAMN_ROUTE_HOST" \
    -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
    --data '[{"request_id":"req-query-scheduled","dock_id":"7f698e6e-2e03-40d4-b240-d35a5b127b29","day":"2026-10-01","status":"scheduled"}]'
[{"request_id":"req-query-scheduled","value":{"appointments":[{"appointment_id":"63751744-37c5-40dc-a0c0-225b827deb95","carrier_id":"e732d0e1-218f-4f0f-a072-9eeb0457987b","dock_id":"7f698e6e-2e03-40d4-b240-d35a5b127b29","slot_end":"2026-10-01T09:00:00.000000Z","slot_start":"2026-10-01T08:00:00.000000Z","status":"scheduled"},{"appointment_id":"d2ec419a-f3e9-4f1d-9051-6e7c45974187","carrier_id":"e732d0e1-218f-4f0f-a072-9eeb0457987b","dock_id":"7f698e6e-2e03-40d4-b240-d35a5b127b29","slot_end":"2026-10-01T11:00:00.000000Z","slot_start":"2026-10-01T10:00:00.000000Z","status":"scheduled"}]}}]
HTTP 200

$ curl -sS -X POST "$BASE/appointment/query" -H "Host: $WAMN_ROUTE_HOST" \
    -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
    --data '[{"request_id":"req-query-arrived","dock_id":"7f698e6e-2e03-40d4-b240-d35a5b127b29","day":"2026-10-01","status":"arrived"},{"request_id":"req-query-other-dock","dock_id":"605e1353-16c9-4df3-9d74-0b381958ae88","day":"2026-10-01","status":"scheduled"},{"request_id":"req-query-other-day","dock_id":"7f698e6e-2e03-40d4-b240-d35a5b127b29","day":"2026-10-02","status":"scheduled"}]'
[{"request_id":"req-query-arrived","value":{"appointments":[{"appointment_id":"ced16055-4f1d-44fc-9079-d5e76fd813c4","carrier_id":"e732d0e1-218f-4f0f-a072-9eeb0457987b","dock_id":"7f698e6e-2e03-40d4-b240-d35a5b127b29","slot_end":"2026-10-01T10:00:00.000000Z","slot_start":"2026-10-01T09:00:00.000000Z","status":"arrived"}]}},{"request_id":"req-query-other-dock","value":{"appointments":[{"appointment_id":"5fbd5bb5-f37a-4bd0-bbf2-3bfecc35b085","carrier_id":"e732d0e1-218f-4f0f-a072-9eeb0457987b","dock_id":"605e1353-16c9-4df3-9d74-0b381958ae88","slot_end":"2026-10-01T10:30:00.000000Z","slot_start":"2026-10-01T09:30:00.000000Z","status":"scheduled"}]}},{"request_id":"req-query-other-day","value":{"appointments":[]}}]
HTTP 200

```

The first answer lists the two still-scheduled appointments of dock 1 on
2026-10-01, and the order is the slot order: `08:00` then `10:00`. The second
call shows the same dock and day filtered to `arrived`, another dock, and
another day.

### The refusals the package names for itself

```
$ curl -sS -X POST "$BASE/appointment/book" -H "Host: $WAMN_ROUTE_HOST" \
    -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
    --data '[{"request_id":"req-book-missing-dock","value":{"idempotency_key":"book-missing-dock","carrier_id":"e732d0e1-218f-4f0f-a072-9eeb0457987b","dock_id":"00000000-0000-4000-8000-0000000000d9","slot_start":"2026-10-03T09:00:00Z","slot_end":"2026-10-03T10:00:00Z"}}]'
[{"error":{"code":"dock_not_found","detail":{"field":"value.dock_id","id":"00000000-0000-4000-8000-0000000000d9"}},"request_id":"req-book-missing-dock"}]
HTTP 200

$ curl -sS -X POST "$BASE/appointment/book" -H "Host: $WAMN_ROUTE_HOST" \
    -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
    --data '[{"request_id":"req-book-missing-carrier","value":{"idempotency_key":"book-missing-carrier","carrier_id":"00000000-0000-4000-8000-0000000000c9","dock_id":"7f698e6e-2e03-40d4-b240-d35a5b127b29","slot_start":"2026-10-03T09:00:00Z","slot_end":"2026-10-03T10:00:00Z"}}]'
[{"error":{"code":"carrier_not_found","detail":{"field":"value.carrier_id","id":"00000000-0000-4000-8000-0000000000c9"}},"request_id":"req-book-missing-carrier"}]
HTTP 200

$ curl -sS -X POST "$BASE/appointment/book" -H "Host: $WAMN_ROUTE_HOST" \
    -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
    --data '[{"request_id":"req-book-bad-slot","value":{"idempotency_key":"book-bad-slot","carrier_id":"e732d0e1-218f-4f0f-a072-9eeb0457987b","dock_id":"7f698e6e-2e03-40d4-b240-d35a5b127b29","slot_start":"2026-10-03T10:00:00Z","slot_end":"2026-10-03T09:00:00Z"}}]'
[{"error":{"code":"invalid_input","detail":{"field":"value.slot_end"}},"request_id":"req-book-bad-slot"}]
HTTP 200

$ curl -sS -X POST "$BASE/appointment/check_in" -H "Host: $WAMN_ROUTE_HOST" \
    -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
    --data '[{"request_id":"req-checkin-twice","value":{"idempotency_key":"checkin-a-again","appointment_id":"ced16055-4f1d-44fc-9079-d5e76fd813c4","arrived_at":"2026-10-01T09:30:00Z"}}]'
[{"error":{"code":"appointment_not_scheduled","detail":{"field":"value.appointment_id","id":"ced16055-4f1d-44fc-9079-d5e76fd813c4"}},"request_id":"req-checkin-twice"}]
HTTP 200

```

### The gates I did not weaken

```
$ curl -sS -X POST "$BASE/appointment/query" -H "Host: $WAMN_ROUTE_HOST" \
    -H "Content-Type: application/json" \
    --data '[{"request_id":"req-noauth","dock_id":"7f698e6e-2e03-40d4-b240-d35a5b127b29","day":"2026-10-01","status":"scheduled"}]'
{"error":{"code":"unauthorized"}}
HTTP 401

$ curl -sS -X POST "$BASE/appointment/query" -H "Host: $WAMN_ROUTE_HOST" \
    -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
    --data '[{"request_id":"req-bad-day","dock_id":"7f698e6e-2e03-40d4-b240-d35a5b127b29","day":"2026-10-01T00:00:00Z","status":"scheduled"}]'
{"error":{"code":"schema-invalid"}}
HTTP 400
```

The route refuses an unauthenticated caller, and the input port refuses a `day`
that is not a date. I changed no permission, policy or gate.

## The published data contract

```
$ jq -c '{class, fields: [.fields[] | {(.path): .type}]}' \
    packages/dock/generated/contracts/appointment/query.result.json
{"class":"one","fields":[{"appointments[].appointment_id":"uuid"},{"appointments[].carrier_id":"uuid"},{"appointments[].dock_id":"uuid"},{"appointments[].slot_start":"timestamptz"},{"appointments[].slot_end":"timestamptz"},{"appointments[].status":"text"}]}

$ jq -c '.canonicalization' packages/dock/generated/contracts/appointment/book.input.json
{"excluded_fields":["request_id","value.idempotency_key"],"numeric":"postgresql_lexical_scale_preserved","payload":"canonical_compact_json","timestamptz":"utc_rfc3339_six_fractional_digits","uuid":"lowercase_hyphenated"}
```

Each of the four commands publishes the same two canonical-form names:
`utc_rfc3339_six_fractional_digits` for a timestamp and `lowercase_hyphenated`
for a uuid.

## What I did not verify

- The whole-repository gates. I ran the tests, clippy and rustfmt for the `dock`
  crate alone. The gate of record in `docs/operations/build-and-test.md` builds
  the two-stage Docker image, and this environment holds no cluster to run it
  against.
- The `departed` status. The scenario names it as a status value, and the table
  admits it, but the scenario names no operation that reaches it. No operation
  writes it, so nothing exercises it.
- Event delivery. The package registers no event handler, so the CDC and
  materializer path never runs.
- The `generated/native-verifier/*.rs` files. They are Generate output, and the
  proof crate that compiles them belongs to `tests/`, which is outside my
  allowed paths.
- Rollback after a crash between the claim and the commit. That path returns
  `retry`, and reaching it needs a killed host process mid-transaction, which I
  did not stage.

# Decisions

**The package schema is `receiving`.** The strict `dev.json` sets `schema` to
`receiving`, and the host passes that one value to the guest as its
`search_path`. A guest names relations unqualified, so tables in any other
schema are unreachable. The package therefore owns `receiving.carrier`,
`receiving.dock` and `receiving.appointment` in the same schema its deployment
names.

**The overlap rule is a dock-row lock, not an `EXCLUDE` constraint.** A
PostgreSQL exclusion constraint on `(dock_id WITH =, tstzrange(...) WITH &&)`
holds DOCK-1 in the table itself. Its `dock_id` equality half needs the
`btree_gist` operator class, and `btree_gist` is available but not installed.
The migration policy in `crates/schema/introspection/src/migration_policy.rs`
admits only schema-qualified `CREATE TABLE` and narrow additive `ALTER TABLE`,
so a package cannot install an extension. Installing it by hand outside the
package is a workaround, and the package cannot reproduce it. `appointment.book`
therefore locks the dock row with `SELECT ... FOR UPDATE` before it probes for an
overlap. That lock is the serialization point, and the race above is the
evidence.

**`dock` is a standalone package, not an overlay of `wamn_receiving`.** The
overlay root is `packages/dock` and `package_sources` names `packages/receiving`.
`resolve_dev_packages` treats a source that no `base_dependencies` entry claims
as ignored, so declaring no base dependency keeps the closure to this package
alone. Dock appointments share no relation and no operation with Receiving, so a
dependency is a claim the package cannot honor.

**Every command is idempotent by claim.** `carrier.create`, `dock.create` and
`appointment.book` each mint an identity. The platform admits exactly one shape
for that: a CDC-excluded claim relation whose uuid column PostgreSQL defaults
once. `appointment.check_in` mints no domain identity, so the other two shapes
were candidates. `state` needs an input field carrying an expected row version,
and the pinned input for `check_in` is `appointment_id` and `arrived_at` alone.
A required version field changes the request the exit gate sends. `inherited`
needs a base dependency, and this package has none. So check-in mints the
identity of the check-in event itself, `check_in_id`, and returns it beside the
pinned `status` and `arrived_at`.

**`appointment.query` publishes `appointments[]`, not a flat row.** The data
contract names `appointments` as a list and `slot_start` as a timestamp. A flat
`bounded_list` result publishes `slot_start` and no `appointments`. A result of
class `one` whose fields are `appointments[].appointment_id` and
`appointments[].slot_start` publishes both names. The wire answer is one object
per envelope item carrying an `appointments` array.

**`day` is declared `text`.** The closed type vocabulary in
`crates/schema/introspection/src/ir.rs` holds ten types and no date. The
alternative was `timestamptz`, which forces a caller to write
`2026-10-01T00:00:00Z` instead of the `2026-10-01` the scenario pins. The input
port pins the shape with the pattern `^[0-9]{4}-[0-9]{2}-[0-9]{2}$`, and
`scalar::day_bounds` refuses anything else.

**A day is a UTC day.** A slot is a `timestamptz`. "One dock's appointments for
a day" therefore needs a zone before it is a question. The package answers in
the one zone every timestamp it emits uses. `day_bounds` widens `2026-10-01` to the
half-open interval `[2026-10-01T00:00:00Z, 2026-10-02T00:00:00Z)`.

**Three refusals beyond the pinned three.** The scenario pins
`slot_unavailable`, `idempotency_conflict` and `not_found` and leaves the rest to
the package. `appointment.book` adds `dock_not_found` and `carrier_not_found`,
because a caller cannot act on one refusal that covers both. `appointment.check_in`
adds `appointment_not_scheduled`, because status only moves forward and a second
arrival under a new key is a different request from a replay.

**The export surface is `#[cfg(target_arch = "wasm32")]`.** The crate is
`crate-type = ["cdylib", "rlib"]` so its own tests run on the host. The native
cdylib link fails when `export!` is unconditional, because mold rejects the
component export symbol `cabi_post_wamn-dock:appointment/book@1.0.0#run` in a
linker version script. Gating the whole guest module on `wasm32` keeps the
operation logic testable and leaves the wasm artifact unchanged.

**Carrier and dock names are not unique.** The scenario asks for an identity the
later operations address, and it gives no uniqueness rule. A unique index adds
a refusal the contract does not name. Two carriers can therefore share a
name and still differ by id.

# Where I got stuck

Nothing blocked the task. Two things cost a rewrite.

The first was the native link failure described above. `cargo test` builds the
cdylib for the host target, and the component export names are not valid inside
a linker version script. The fix was one `#[cfg]`, not a workaround.

The second was the dead-code warnings from generated rows. An existence probe
such as `find_carrier` reads whether a row came back, never the id it already
sent, so the generated `id` field is unread. `components/data/client-acme-receiving-data`
sets the precedent of an `#[expect(dead_code, reason = ...)]` on the generated
module, and I followed it.

# Rules I relied on

- `docs/architecture/application-naming.md`. Technical identifiers are singular
  `snake_case`. The operation token is
  `<package-id-kebab>:<module-kebab>/<action-kebab>@<package-version>`, which
  gives `wamn-dock:appointment/check-in@1.0.0`. Constraint names are
  `<table>_<column_1>[_<column_n>]_<kind>` and shorter than 64 bytes.
  Canonicalize on ingest, then hash.
- `crates/schema/generator/src/manifest.rs`. A command declares `idempotent_by`
  as exactly one of three shapes. A claim relation is CDC-excluded, keys
  `idempotency_key` under a primary key alone, and carries `canonical_command`.
  Each identity column is a unique non-null uuid defaulting to
  `gen_random_uuid()`.
- `crates/schema/generator/src/generate.rs`. The declared relation access must
  equal the access the generator derives from the authored SQL, field by field
  and lock by lock.
- `crates/schema/introspection/src/migration_policy.rs`. A migration authors
  schema selection and requires qualified DDL. SQL corpora inherit the
  host-selected `search_path` and refuse qualified references.
- `tools/build-components`. The package half of the component inventory comes
  from each `packages/*/wamn.json` `components` key, so a new package needs no
  edit to `architecture/workspace-tiers.json`. The cargo package name is the
  component name with underscores replaced by hyphens.
- `services/ctl/src/publish_release.rs`. An attachment `definition-hash` is
  `sha256:` plus the sha256 of the canonical JSON bytes of its `definition`. I
  checked my `jq -cSj . | sha256sum` method against all seven `packages/wms`
  attachments before I used it.
- `services/ctl/src/dev.rs`. Publish through Activate require a committed
  worktree, so I committed locally after Generate and never pushed.
- `AGENTS.md`. Simplicity first, surgical changes, and the conservative git
  profile: commit locally when the loop demands it, never push.

# Open questions

- **Does the exit gate tolerate extra result fields?** `appointment.check_in`
  returns `check_in_id` and `appointment_id` beside the pinned `status` and
  `arrived_at`. The pinned pairs are all present, but I cannot tell whether the
  gate asserts a superset or an exact set.
- **Does the exit gate read `appointments[].slot_start` as `slot_start`?** The
  contract path carries the list prefix. A checker that reads leaf names finds
  both `appointments` and `slot_start`. A checker that reads whole paths finds
  neither name spelled the way the table writes it.
- **Is `text` an acceptable type for `day`?** The scenario writes `date`, and
  the platform's closed vocabulary has none. The wire value is exactly
  `2026-10-01` either way.
- **What does `appointment.check_in` owe a caller whose appointment already
  arrived?** The scenario says status only moves forward and pins no code for a
  second arrival under a new key. I refuse it with `appointment_not_scheduled`.
  Returning the earlier arrival instead is also defensible.
- **Who moves an appointment to `departed`?** The status vocabulary names it and
  no operation writes it. The scenario names no departure command.
