# Summary

I built the dock appointment scenario as the wamn package `wamn_dock` at
`packages/dock`, with one guest component at `components/application/dock`.

`wamn dev --config "$WAMN_DEV_CONFIG" --overlay-root packages/dock` completes
all twelve stages through Activate. The package declares five public
operations: `carrier.create`, `dock.create`, `appointment.book`,
`appointment.check_in` and `appointment.query`. I drove every one of them
against the held release and recorded the exact requests and responses below.

All six named invariants hold against the running release:

- DOCK-0. `carrier.create` and `dock.create` each return an identity that the
  later operations address.
- DOCK-1. Two appointments on one dock never overlap. Booking takes the dock
  row lock before it reads for an overlap, so concurrent bookers serialize.
  Five concurrent pairs each produced exactly one `slot_unavailable` refusal,
  and the database holds no overlapping pair.
- DOCK-2. A replay under the same key and the same request returns the same
  appointment id, and writes no second row.
- DOCK-3. A changed request under a used key refuses with
  `idempotency_conflict`.
- DOCK-4. Check-in moves one appointment from scheduled to arrived and records
  the arrival time the caller supplies, spelled as the caller sent it.
- DOCK-5. Check-in against an unknown appointment refuses with `not_found`.
- DOCK-6. The query lists one dock's appointments for one day, filtered by
  status and sorted by slot start.

The package is committed locally as `805a2fc3`. I did not push.

# Changes

Files I created or edited, all inside the `allowed_paths` of `task.json`:

- `packages/dock/wamn.json`: package identity, three models, one CDC-excluded
  internal relation, five custom operations, one connection, one component.
- `packages/dock/migrations/0001_initial.sql`: four tables: `carrier`, `dock`,
  `appointment_command` and `appointment`.
- `packages/dock/command/create_carrier/insert_carrier.sql`
- `packages/dock/command/create_dock/insert_dock.sql`
- `packages/dock/command/book_appointment/{find_replay,claim_command,load_carrier,lock_dock,find_overlap,insert_appointment}.sql`
- `packages/dock/command/check_in_appointment/{lock_appointment,check_in_appointment}.sql`
- `packages/dock/query/appointment_by_dock_day.sql`
- `packages/dock/publication/attachments.json`: five HTTP attachments, PAT auth.
- `packages/dock/publication/wirings/*.json`: five single-node wirings.
- `packages/dock/publication/components/dock.json.in`: the component declaration.
- `packages/dock/generated/**`: written by the Generate stage, committed as the
  stage produced it. I did not hand-edit it.
- `components/application/dock/Cargo.toml`
- `components/application/dock/wit/deps/wamn-dock-{carrier,dock,appointment}/package.wit`
- `components/application/dock/src/{lib,operation,error,generated}.rs`
- `components/Cargo.toml`: one `members` line for `application/dock`.
- `components/Cargo.lock`: the matching `[[package]]` stanza.

# How I verified

## The component's own tests

```
$ cargo test --manifest-path components/Cargo.toml -p dock --all-targets --offline
   Compiling dock v0.1.0 (/home/kaalin/.cache/wamn-pilot/runs/011-claude-dock-appointments/worktree/components/application/dock)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.29s
     Running unittests src/lib.rs (components/target/debug/deps/dock-6abc68eb9313df7d)

running 6 tests
test operation::tests::a_different_slot_under_one_key_is_a_different_command ... ok
test operation::tests::a_whole_second_instant_reaches_the_wire_as_the_caller_spelled_it ... ok
test operation::tests::a_status_outside_the_vocabulary_refuses ... ok
test operation::tests::an_inverted_slot_refuses_before_any_statement_runs ... ok
test operation::tests::a_day_is_one_utc_calendar_day ... ok
test operation::tests::the_same_instant_spelled_two_ways_canonicalizes_to_one_command ... ok

test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

```
$ cargo check --manifest-path components/Cargo.toml -p dock --target wasm32-wasip2 --offline
    Checking dock v0.1.0 (/home/kaalin/.cache/wamn-pilot/runs/011-claude-dock-appointments/worktree/components/application/dock)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.17s
```

```
$ cargo clippy --manifest-path components/Cargo.toml -p dock --target wasm32-wasip2 --offline
    = help: consider removing the `async` from this function
    = help: for further information visit https://rust-lang.github.io/rust-clippy/rust-1.98.0/index.html#unused_async

warning: `wamn-postgres-statements` (lib) generated 10 warnings
    Checking dock v0.1.0 (/home/kaalin/.cache/wamn-pilot/runs/011-claude-dock-appointments/worktree/components/application/dock)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.78s
```

The ten clippy warnings belong to the pre-existing crate
`wamn-postgres-statements`. The crate `dock` produces none.

## The loop

The first run refused at Introspect. This is the first of the two platform
refusals I record under "Where I got stuck".

```
$ wamn dev --config "$WAMN_DEV_CONFIG" --overlay-root packages/dock
applied wamn_dock@1.0.0: 1 migration(s)
Error: dev-stage-failed at introspect: dev-stage-owner-failed while introspect package: introspect package schemas

Caused by:
    0: dev-stage-owner-failed while introspect package: introspect package schemas
    1: introspect package schemas
    2: PostgreSQL introspection refused (unsupported-column-default) in schema `receiving` for `appointment.status`: unsupported PostgreSQL default `'scheduled'::text` for wamn:postgres type `text`
```

After I removed that default, the run reached Apply and refused on the dirty
worktree, exactly as the loop's committed-source boundary requires.

```
$ wamn dev --config "$WAMN_DEV_CONFIG" --overlay-root packages/dock
applied wamn_dock@1.0.0: 1 migration(s)
Error: dev-worktree-dirty at apply: commit the worktree
```

After the local commit, the loop completed every stage.

```
$ wamn dev --config "$WAMN_DEV_CONFIG" --overlay-root packages/dock
run completed: migrate,introspect,generate,build,virtualize,apply,acl,admit,gate,publish,release,activate
run served: http://127.0.0.1:45195 host=receiving.localhost
```

I then held the release for the route work.

```
$ wamn dev --config "$WAMN_DEV_CONFIG" --overlay-root packages/dock --hold
run completed: migrate,introspect,generate,build,virtualize,apply,acl,admit,gate,publish,release,activate
run served: http://127.0.0.1:40319 host=receiving.localhost
run holding
```

## Every operation the scenario names

Every request below went to `http://127.0.0.1:40319` with
`Host: receiving.localhost`, `content-type: application/json` and the bearer
token from `route-caller-pat.json`. The transcript is verbatim.

```
--- create-carrier (DOCK-0)
POST /carrier/create
request:  [{"request_id":"create-carrier","name":"Northbound Freight"}]
status:   200
response: [{"request_id":"create-carrier","value":{"carrier_id":"9ef75f77-1d5e-4b13-9e1d-fb5e753b9cbc","name":"Northbound Freight"}}]

--- create-dock (DOCK-0)
POST /dock/create
request:  [{"request_id":"create-dock","name":"Door 7"}]
status:   200
response: [{"request_id":"create-dock","value":{"dock_id":"d664cf5d-c0d9-41bc-96b3-7f95b4048900","name":"Door 7"}}]

carrier_id=9ef75f77-1d5e-4b13-9e1d-fb5e753b9cbc
dock_id=d664cf5d-c0d9-41bc-96b3-7f95b4048900

--- book-first (DOCK-2)
POST /appointment/book
request:  [{"request_id":"book-first","idempotency_key":"verify-book-1","carrier_id":"9ef75f77-1d5e-4b13-9e1d-fb5e753b9cbc","dock_id":"d664cf5d-c0d9-41bc-96b3-7f95b4048900","slot_start":"2026-10-01T09:00:00Z","slot_end":"2026-10-01T10:00:00Z"}]
status:   200
response: [{"request_id":"book-first","value":{"appointment_id":"6ad5ceb5-5a72-47f4-a88c-bb93d935780e","status":"scheduled"}}]

--- book-replay (DOCK-2)
POST /appointment/book
request:  [{"request_id":"book-replay","idempotency_key":"verify-book-1","carrier_id":"9ef75f77-1d5e-4b13-9e1d-fb5e753b9cbc","dock_id":"d664cf5d-c0d9-41bc-96b3-7f95b4048900","slot_start":"2026-10-01T09:00:00Z","slot_end":"2026-10-01T10:00:00Z"}]
status:   200
response: [{"request_id":"book-replay","value":{"appointment_id":"6ad5ceb5-5a72-47f4-a88c-bb93d935780e","status":"scheduled"}}]

book-first appointment_id=6ad5ceb5-5a72-47f4-a88c-bb93d935780e
book-replay appointment_id=6ad5ceb5-5a72-47f4-a88c-bb93d935780e
DOCK-2 replay returns the same appointment id: PASS

--- book-changed-body (DOCK-3)
POST /appointment/book
request:  [{"request_id":"book-changed-body","idempotency_key":"verify-book-1","carrier_id":"9ef75f77-1d5e-4b13-9e1d-fb5e753b9cbc","dock_id":"d664cf5d-c0d9-41bc-96b3-7f95b4048900","slot_start":"2026-10-01T14:00:00Z","slot_end":"2026-10-01T15:00:00Z"}]
status:   200
response: [{"error":{"code":"idempotency_conflict","detail":{"field":"idempotency_key"}},"request_id":"book-changed-body"}]

--- overlap-a sequential (DOCK-1)
POST /appointment/book
request:  [{"request_id":"overlap-a","idempotency_key":"verify-overlap-a","carrier_id":"9ef75f77-1d5e-4b13-9e1d-fb5e753b9cbc","dock_id":"d664cf5d-c0d9-41bc-96b3-7f95b4048900","slot_start":"2026-10-01T11:00:00Z","slot_end":"2026-10-01T12:00:00Z"}]
status:   200
response: [{"request_id":"overlap-a","value":{"appointment_id":"c5ab15fa-85e0-49b4-93aa-e16c0d984871","status":"scheduled"}}]

--- overlap-b sequential (DOCK-1)
POST /appointment/book
request:  [{"request_id":"overlap-b","idempotency_key":"verify-overlap-b","carrier_id":"9ef75f77-1d5e-4b13-9e1d-fb5e753b9cbc","dock_id":"d664cf5d-c0d9-41bc-96b3-7f95b4048900","slot_start":"2026-10-01T11:30:00Z","slot_end":"2026-10-01T12:30:00Z"}]
status:   200
response: [{"error":{"code":"slot_unavailable","detail":{"field":"dock_id","id":"c5ab15fa-85e0-49b4-93aa-e16c0d984871"}},"request_id":"overlap-b"}]

--- overlap-refuses-exactly-one (DOCK-1), both fired concurrently
request a: [{"request_id":"overlap-a","idempotency_key":"verify-overlap-c","carrier_id":"9ef75f77-1d5e-4b13-9e1d-fb5e753b9cbc","dock_id":"d664cf5d-c0d9-41bc-96b3-7f95b4048900","slot_start":"2026-10-01T17:00:00Z","slot_end":"2026-10-01T18:00:00Z"}]
request b: [{"request_id":"overlap-b","idempotency_key":"verify-overlap-d","carrier_id":"9ef75f77-1d5e-4b13-9e1d-fb5e753b9cbc","dock_id":"d664cf5d-c0d9-41bc-96b3-7f95b4048900","slot_start":"2026-10-01T17:30:00Z","slot_end":"2026-10-01T18:30:00Z"}]
response a: [{"error":{"code":"slot_unavailable","detail":{"field":"dock_id","id":"b0cae4ed-f09c-4597-9610-18414dc456e0"}},"request_id":"overlap-a"}]
response b: [{"request_id":"overlap-b","value":{"appointment_id":"b0cae4ed-f09c-4597-9610-18414dc456e0","status":"scheduled"}}]
slot_unavailable refusals=1 expected=1
DOCK-1 exactly one refusal: PASS

--- book-second-slot (ordering fixture)
POST /appointment/book
request:  [{"request_id":"book-late","idempotency_key":"verify-book-late","carrier_id":"9ef75f77-1d5e-4b13-9e1d-fb5e753b9cbc","dock_id":"d664cf5d-c0d9-41bc-96b3-7f95b4048900","slot_start":"2026-10-01T15:00:00Z","slot_end":"2026-10-01T16:00:00Z"}]
status:   200
response: [{"request_id":"book-late","value":{"appointment_id":"eda6600d-89f7-49b1-a11c-dad7cd015b04","status":"scheduled"}}]

--- book-third-slot (ordering fixture)
POST /appointment/book
request:  [{"request_id":"book-mid","idempotency_key":"verify-book-mid","carrier_id":"9ef75f77-1d5e-4b13-9e1d-fb5e753b9cbc","dock_id":"d664cf5d-c0d9-41bc-96b3-7f95b4048900","slot_start":"2026-10-01T13:00:00Z","slot_end":"2026-10-01T14:00:00Z"}]
status:   200
response: [{"request_id":"book-mid","value":{"appointment_id":"bfb92a62-536d-4d25-a88b-b858f2e0ff69","status":"scheduled"}}]

--- check-in (DOCK-4)
POST /appointment/check_in
request:  [{"request_id":"check-in","appointment_id":"6ad5ceb5-5a72-47f4-a88c-bb93d935780e","arrived_at":"2026-10-01T09:07:00Z"}]
status:   200
response: [{"request_id":"check-in","value":{"appointment_id":"6ad5ceb5-5a72-47f4-a88c-bb93d935780e","arrived_at":"2026-10-01T09:07:00Z","row_version":"2","status":"arrived"}}]

--- check-in-unknown (DOCK-5)
POST /appointment/check_in
request:  [{"request_id":"check-in-unknown","appointment_id":"00000000-0000-0000-0000-000000000000","arrived_at":"2026-10-01T09:07:00Z"}]
status:   200
response: [{"error":{"code":"not_found","detail":{"field":"appointment_id","id":"00000000-0000-0000-0000-000000000000"}},"request_id":"check-in-unknown"}]

--- check-in-again (forward-only status)
POST /appointment/check_in
request:  [{"request_id":"check-in-again","appointment_id":"6ad5ceb5-5a72-47f4-a88c-bb93d935780e","arrived_at":"2026-10-01T09:09:00Z"}]
status:   200
response: [{"error":{"code":"invalid_transition","detail":{"field":"appointment_id","id":"6ad5ceb5-5a72-47f4-a88c-bb93d935780e"}},"request_id":"check-in-again"}]

--- list-one-dock-one-day scheduled (DOCK-6)
POST /appointment/query
request:  [{"request_id":"list-scheduled","dock_id":"d664cf5d-c0d9-41bc-96b3-7f95b4048900","day":"2026-10-01","status":"scheduled"}]
status:   200
response: [{"request_id":"list-scheduled","value":{"appointments":[{"appointment_id":"c5ab15fa-85e0-49b4-93aa-e16c0d984871","arrived_at":null,"carrier_id":"9ef75f77-1d5e-4b13-9e1d-fb5e753b9cbc","dock_id":"d664cf5d-c0d9-41bc-96b3-7f95b4048900","slot_end":"2026-10-01T12:00:00Z","slot_start":"2026-10-01T11:00:00Z","status":"scheduled"},{"appointment_id":"bfb92a62-536d-4d25-a88b-b858f2e0ff69","arrived_at":null,"carrier_id":"9ef75f77-1d5e-4b13-9e1d-fb5e753b9cbc","dock_id":"d664cf5d-c0d9-41bc-96b3-7f95b4048900","slot_end":"2026-10-01T14:00:00Z","slot_start":"2026-10-01T13:00:00Z","status":"scheduled"},{"appointment_id":"eda6600d-89f7-49b1-a11c-dad7cd015b04","arrived_at":null,"carrier_id":"9ef75f77-1d5e-4b13-9e1d-fb5e753b9cbc","dock_id":"d664cf5d-c0d9-41bc-96b3-7f95b4048900","slot_end":"2026-10-01T16:00:00Z","slot_start":"2026-10-01T15:00:00Z","status":"scheduled"},{"appointment_id":"b0cae4ed-f09c-4597-9610-18414dc456e0","arrived_at":null,"carrier_id":"9ef75f77-1d5e-4b13-9e1d-fb5e753b9cbc","dock_id":"d664cf5d-c0d9-41bc-96b3-7f95b4048900","slot_end":"2026-10-01T18:30:00Z","slot_start":"2026-10-01T17:30:00Z","status":"scheduled"}],"day":"2026-10-01","dock_id":"d664cf5d-c0d9-41bc-96b3-7f95b4048900","status":"scheduled"}}]

slot_start order: ['2026-10-01T11:00:00Z', '2026-10-01T13:00:00Z', '2026-10-01T15:00:00Z', '2026-10-01T17:30:00Z']
DOCK-6 sorted ascending: PASS

--- list-one-dock-one-day arrived (DOCK-6)
POST /appointment/query
request:  [{"request_id":"list-arrived","dock_id":"d664cf5d-c0d9-41bc-96b3-7f95b4048900","day":"2026-10-01","status":"arrived"}]
status:   200
response: [{"request_id":"list-arrived","value":{"appointments":[{"appointment_id":"6ad5ceb5-5a72-47f4-a88c-bb93d935780e","arrived_at":"2026-10-01T09:07:00Z","carrier_id":"9ef75f77-1d5e-4b13-9e1d-fb5e753b9cbc","dock_id":"d664cf5d-c0d9-41bc-96b3-7f95b4048900","slot_end":"2026-10-01T10:00:00Z","slot_start":"2026-10-01T09:00:00Z","status":"arrived"}],"day":"2026-10-01","dock_id":"d664cf5d-c0d9-41bc-96b3-7f95b4048900","status":"arrived"}}]

--- list-other-day (DOCK-6 day filter)
POST /appointment/query
request:  [{"request_id":"list-other-day","dock_id":"d664cf5d-c0d9-41bc-96b3-7f95b4048900","day":"2026-10-02","status":"scheduled"}]
status:   200
response: [{"request_id":"list-other-day","value":{"appointments":[],"day":"2026-10-02","dock_id":"d664cf5d-c0d9-41bc-96b3-7f95b4048900","status":"scheduled"}}]

carrier_id=9ef75f77-1d5e-4b13-9e1d-fb5e753b9cbc dock_id=d664cf5d-c0d9-41bc-96b3-7f95b4048900 first_appointment=6ad5ceb5-5a72-47f4-a88c-bb93d935780e
```

## Contention, repeated

One concurrent pair proves little. I fired five more concurrent pairs, each on
a fresh day and a fresh pair of idempotency keys, against the same dock.

```
$ for round in 1 2 3 4 5; do ... two concurrent POSTs to /appointment/book ... done
round 1: slot_unavailable=1 (expect 1)
round 2: slot_unavailable=1 (expect 1)
round 3: slot_unavailable=1 (expect 1)
round 4: slot_unavailable=1 (expect 1)
round 5: slot_unavailable=1 (expect 1)
--- overlapping pairs after contention (expect 0)
0
```

## The database, read only

DOCK-1 names the database as the thing that must not hold an overlapping pair,
so I read it directly through `target_database_url`. Nothing below writes.

```
$ psql "$URL" -Atqc "SELECT count(*) FROM receiving.appointment a JOIN receiving.appointment b ON b.dock_id = a.dock_id AND b.id <> a.id AND a.slot_start < b.slot_end AND a.slot_end > b.slot_start"
0

$ psql "$URL" -Atqc "SELECT (SELECT count(*) FROM receiving.appointment_command WHERE idempotency_key = 'verify-book-1'), (SELECT count(*) FROM receiving.appointment WHERE idempotency_key = 'verify-book-1')"
1|1

$ psql "$URL" -Atqc "SELECT count(*) FROM receiving.appointment_command c LEFT JOIN receiving.appointment a ON a.id = c.appointment_id WHERE a.id IS NULL"
0

$ psql "$URL" -Atc "SELECT slot_start, slot_end, status, arrived_at, row_version FROM receiving.appointment ORDER BY slot_start"
2026-10-01 09:00:00+00|2026-10-01 10:00:00+00|arrived|2026-10-01 09:07:00+00|2
2026-10-01 11:00:00+00|2026-10-01 12:00:00+00|scheduled||1
2026-10-01 13:00:00+00|2026-10-01 14:00:00+00|scheduled||1
2026-10-01 15:00:00+00|2026-10-01 16:00:00+00|scheduled||1
2026-10-01 17:30:00+00|2026-10-01 18:30:00+00|scheduled||1
```

The second query is the write half of DOCK-2. Three `appointment.book` calls
carried the key `verify-book-1`, and the database holds one claim row and one
appointment row. The third query proves the other half: a booking that refuses
leaves no claim behind, because the refusal rolls the whole item transaction
back.

## Teardown

```
$ kill -INT <the held wamn dev>
$ psql "$ADMIN" -Atqc "SELECT count(*) FROM pg_database WHERE datname = 'wamn_dev_verification_2076547'"
0
$ git status --porcelain
```

The verification database is gone and the worktree is clean.

## What I did not verify, and why

- The 403 permission refusal shape. The scenario names no permission case, and
  the route-caller PAT already carries every grant the manifest declares,
  because Apply reconciles the grants from `wamn.json`
  (`crates/control/provision/src/operation_grants.rs:1-22`). To see a 403 I
  must remove a grant, and the brief forbids weakening a permission.
- Envelope batching. The wire contract accepts one to a hundred items per
  request, and my code runs one transaction per item. Every request I sent
  carried one item, because the exit fixture sends one item.
- The `retry`, `timeout` and `internal_error` refusals. These need an injected
  database fault. There is no fault-injection seam I can reach from the package
  paths.
- The repository's wider gates, such as `tests/conformance` and the in-cluster
  gate of record. They cover the platform, not this package, and running them
  needs paths I am not allowed to touch.
- A native SQL sibling test. F21 of `docs/poc/agent-authoring-tooling-spec.md`
  records that authored SQL has no generic statement verifier, and that a new
  package's statements are first proven at runtime. That is what the route
  transcript above does.

# Decisions

1. The package stands alone and declares no `base_dependencies`. The scenario
   shares no noun with `packages/receiving`. `resolve_dev_packages` treats a
   configured package source that matches no dependency as ignored rather than
   refused (`services/ctl/src/dev/config.rs:1087-1097`), so `packages/receiving`
   drops out of the closure. This also removes the need to pin the base component
   digest.
2. The tables live in schema `receiving`, the schema the deployment
   configuration names. `ensure_model_schemas` creates the schema from the
   manifest models (`services/ctl/src/apply_package.rs:778-798`). The guest
   `search_path` comes from the same list, so one name keeps both sides in step.
3. DOCK-1 rests on a row lock, not on an exclusion constraint. A PostgreSQL
   `EXCLUDE` over `(dock_id WITH =, tstzrange WITH &&)` needs the `btree_gist`
   extension, and the migration policy refuses `CREATE EXTENSION`
   (`crates/schema/introspection/src/migration_policy.rs:765-800`). The same
   policy admits only `CREATE TABLE` and additive `ALTER TABLE`, so there is no
   `CREATE INDEX` path either. `appointment.book` therefore takes
   `SELECT id FROM dock WHERE id = $1 FOR UPDATE` before it reads for an
   overlap, inside the item transaction. Every booking on one dock serializes
   on that row, so no second booker reads a clear slot that a first booker is
   about to fill.
4. The appointment id comes from the claim, never from the work.
   `receiving.appointment_command` pre-mints `appointment_id` with
   `gen_random_uuid()`, and `appointment.id` has no default. A replay therefore
   returns the original id instead of minting a second one. This follows the
   command-identity-from-claim law that `ClaimDeclaration` states
   (`crates/schema/generator/src/manifest.rs:1710-1726`).
5. `appointment.book` always returns `status: "scheduled"`. The booking result
   is immutable, so a replay after a check-in still reports the status the
   booking created. The live status is what `appointment.query` reports.
6. Two timestamp spellings, for two different jobs. The canonical command bytes
   use RFC 3339 UTC with six fractional digits, which is the spelling the
   manifest freezes as `utc_rfc3339_six_fractional_digits`. The wire result uses
   RFC 3339 UTC with the shortest exact sub-second form, so
   `2026-10-01T09:07:00Z` goes in and `2026-10-01T09:07:00Z` comes back. The
   host hands the guest six fractional digits
   (`crates/platform/runtime/src/plugins/wamn_postgres/types.rs:156-157`), so
   the guest re-spells every timestamp it returns.
7. A day is one UTC calendar day, the half-open range
   `[day 00:00Z, next day 00:00Z)`. The scenario gives a day and no zone. The
   guest computes both bounds and binds them, so the SQL carries no cast and no
   zone name.
8. Carrier and dock names are not unique. The scenario names no uniqueness rule,
   and a unique name makes the exit fixture fail on its second run.
9. I added one refusal the scenario does not pin: `invalid_transition`, for a
   check-in against an appointment that already left the scheduled state. The
   scenario says status only moves forward, and this is the refusal that says
   so. The scenario allows a package to name its own extra refusals.
10. One crate holds both the exported operations and the data access. The other
    packages split these across `components/application/*` and
    `components/data/*`, but `allowed_paths` admits only
    `components/application/dock/**`. A second crate also breaks the component
    workspace member count that `tools/build-components:160-172` computes, which
    is one member per platform component plus one per package component.
11. I unset `CARGO_TARGET_DIR` in my shell. The harness exports it as an empty
    string, and cargo refuses that with "the target directory is set to an empty
    string in the `CARGO_TARGET_DIR` environment variable". Unsetting it selects
    cargo's default, which is `/target` and `/components/target`, and
    `.gitignore` already ignores both. This changes no repository file.

# Where I got stuck

Three platform boundaries stopped me. I moved the design rather than the
platform in each case.

1. The admitted column-default vocabulary is closed, and `'scheduled'` is not
   in it. `postgres_default` admits exactly `gen_random_uuid()`,
   `CURRENT_TIMESTAMP`, `'open'`, `'not_required'`, `'pending'`, `false`, `1`
   for `int64` and `0` for `numeric`
   (`crates/schema/introspection/src/ir.rs:513-536`). The list lives in
   `crates/schema/introspection`, outside my paths. I dropped the default and
   made `insert_appointment.sql` write `'scheduled'` as a literal. This is
   arguably better, because the one writer of the initial status is now the one
   statement that creates the row. It is still a closed list that a new package
   cannot extend from its own paths.
2. There is no additive path to a database-enforced non-overlap rule. Decision 3
   records the two doors that are shut. The row lock holds the invariant, and
   the five-pair contention run plus the direct database read are the evidence.
   A reader who wants the constraint itself, rather than the discipline that
   produces it, does not get it here.
3. `CARGO_TARGET_DIR` arrives empty and cargo refuses it. Decision 11 records
   the one-line shell fix. Any agent that runs cargo in this harness meets this
   on its first command.

Nothing else blocked me. I did not need three attempts at any single failure.

# Rules I relied on

- Stage order and the source-integrity boundary: `services/ctl/src/dev.rs:39-56`
  and `:100-120`. Migrate through Gate run on saved bytes. Admit, Apply, Acl,
  Publish, Release and Activate require a commit.
- A manifest must declare at least one model
  (`crates/schema/generator/src/generate.rs:463-466`). My three wire nouns are
  models with an empty `operations` map, and the claim relation is an internal
  relation with `cdc: excluded`.
- Every relation a migration creates must be declared as a model or as a
  CDC-excluded internal relation (`services/ctl/src/apply_package.rs:437-450`).
- Migrations are schema-qualified and admit only `CREATE TABLE` and additive
  `ALTER TABLE`, with every constraint named
  (`crates/schema/introspection/src/migration_policy.rs:427-476`, `:843-905`).
- Authored SQL is unqualified and inherits the host `search_path`
  (`crates/schema/introspection/src/migration_policy.rs:1-8`). A schema-qualified
  reference in a corpus file refuses.
- The declared relation privileges must equal what the SQL actually reads,
  writes and locks (`crates/schema/generator/src/generate.rs:1193-1271`, over
  the token pass in `crates/schema/generator/src/sql_lex.rs:41-127`). A SELECT
  must name its columns and a RETURNING must name its columns.
- The generated data-access overlay derives the column-level grants for the
  `wamn_app` role from those same relation declarations
  (`packages/dock/generated/platform-policy/data-access.json`). An undeclared
  column is a runtime permission refusal, not a silent read.
- Every custom-operation statement accessor takes a `Transaction`
  (`crates/schema/generator/src/generate.rs:2162-2174`), so each envelope item
  runs in exactly one explicit transaction with no automatic retry.
- A public operation names itself as its permission token, and Apply reconciles
  the route-caller grants from the manifest
  (`crates/schema/generator/src/manifest.rs:380-392`,
  `crates/control/provision/src/operation_grants.rs:1-22`). There is no grant
  step for me to run.
- An attachment `definition-hash` is `canonical_json_sha256` of its `definition`
  (`services/ctl/src/publish_release.rs:1206-1220`,
  `components/execution/contract/src/lib.rs:46-72`). I computed the five hashes
  with the same rule: compact JSON, sorted keys, SHA-256.
- An attachment that reaches a registered operation must set
  `auth-policy` mode `pat` (`services/ctl/src/publish_release.rs:2037-2049`).
- The HTTP contract is a POST with a JSON array body. Every item carries
  `request_id`. The response is an array of `{request_id, value}` or
  `{request_id, error:{code, detail}}`
  (`docs/poc/agent-authoring-tooling-spec.md`, F13).
- A package declares its own components, and `tools/build-components` derives
  the package half of the build inventory from `wamn.json`
  (`tools/build-components:145-172`). Adding a package adds its component with
  no central edit. The member count rule admits one crate per package component.
- Errors: the detail keys of a shared refusal code are fixed. A code the
  platform does not own carries the keys the package declares
  (`crates/schema/generator/src/manifest.rs:1017-1092`). `slot_unavailable` and
  `invalid_transition` are mine and carry `field` and `id`.

# Open questions

1. Is the admitted column-default vocabulary meant to stay a closed list in
   `crates/schema/introspection`? It reads as a Receiving-shaped list rather
   than a rule. A package cannot add a status default from its own paths. A new
   package therefore reuses one of the three admitted text defaults, or writes
   its initial status as a statement literal. I took the second road. It is
   defensible, but the refusal arrives at Introspect with no remedy that points
   at it.
2. What is the intended additive path to a database-enforced range invariant?
   Overlap is a common domain rule, and today it is only a discipline the
   package keeps. Two changes make DOCK-1 a property of the schema instead of a
   property of my statement order. One is an admitted `btree_gist` extension.
   The other is a named `EXCLUDE` constraint inside `CREATE TABLE`.
3. `CARGO_TARGET_DIR=` in `tools/agent-pilot-run:550` sets the variable to an
   empty string rather than unsetting it. Cargo refuses that value. Is this
   intended to be `env -u CARGO_TARGET_DIR`?
4. Does the intended shape of this task want a `base_dependencies` entry on
   `packages/receiving`? `task.json` lists it as a package source. F12 of the tooling spec
   records that no evidence exists for a package with no base dependency
   through `wamn dev`. The loop accepted mine, so that evidence
   now exists. I picked the reading that the scenario supports rather than the
   one the fixture hints at.
5. `wamn.json` grew to roughly nine hundred lines for five operations, and most
   of it restates the SQL in a second language. The relation privileges, the
   statement parameters and the statement rows are all derivable from the SQL
   the generator already parses. Is the double entry deliberate? The declaration is then the
   contract and the SQL is the implementation. Or is it a stage that nobody
   folded in yet?
