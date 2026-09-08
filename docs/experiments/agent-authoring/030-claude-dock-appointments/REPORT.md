# Summary

`packages/dock` is a new wamn package that books dock appointments. It owns
four tables in the `receiving` schema and one guest component,
`components/application/dock`, that exports the five operations the scenario
pins: `carrier.create`, `dock.create`, `appointment.book`,
`appointment.check_in` and `appointment.query`.

`wamn dev --config "$WAMN_DEV_CONFIG" --overlay-root packages/dock` completed
all twelve stages through Activate at commit `0f43d0a3`. I exercised all five
operations and all seven invariants against that running release. The full
request and response transcript is below.

The loop does not complete for the delivered tree today. After the release
above was activated I ran rustfmt on the component. That moved the compiled
bytes, and the platform refused them under the already published `dock@1.0.0`.
My recovery was a version bump to `dock@1.1.0`. That bump applied, failed one
stage later, and left two package coordinates in `catalog.packages`.

The Acl stage demands one package root per applied coordinate. It also forbids
two roots for one package id. No version of `dock` can pass Acl in this
environment again. "Where I got stuck" carries the exact errors and the source
lines that produce them.

The delivered tree is `dock@1.0.0`. It differs from the tree that produced the
activated release by rustfmt whitespace and two `#[expect]` lint attributes,
and by nothing else. `git diff 0f43d0a3 HEAD` shows that whole difference.

# Changes

Every path below is inside `allowed_paths`.

- `packages/dock/wamn.json` holds the package manifest. It declares three models
  (`appointment`, `carrier`, `dock`), one internal relation
  (`book_appointment_command`), five custom operations, one component.
- `packages/dock/migrations/0001_initial.sql` creates the four tables.
- `packages/dock/command/**` and `packages/dock/query/**` hold eight authored SQL
  statements.
- `packages/dock/publication/**` holds five wirings, five HTTP attachments, and
  the component declaration template.
- `packages/dock/generated/**` is written by the Generate stage. It is committed
  because Publish through Activate require a clean worktree.
- `components/application/dock/**` is the wasm32-wasip2 guest. It holds six
  source files and three WIT interface packages.
- `components/Cargo.toml` gains one workspace member line.
- `components/Cargo.lock` gains the `dock` package entry.

The component vendors no WIT copy of `wamn:node` or `wamn:postgres`. It points
`wit-bindgen` at the copies under `components/data/receiving-data/wit/deps`,
the way `components/application/client-acme-receiving` does, so the drift guard
in `crates/platform/runtime/tests/node_wit_coherence.rs` keeps its exact
seven-copy inventory.

# How I verified

## The component's own tests

```
$ cargo test -p dock --offline
   Compiling dock v0.1.0 (/home/kaalin/.cache/wamn-pilot/runs/030-claude-dock-appointments/worktree/components/application/dock)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.38s
     Running unittests src/lib.rs (target/debug/deps/dock-1eabce185a9df019)

running 12 tests
test canonical::tests::every_spelling_of_one_instant_becomes_one_timestamp ... ok
test canonical::tests::only_the_canonical_uuid_spelling_is_accepted ... ok
test canonical::tests::a_day_becomes_half_open_utc_bounds ... ok
test error::tests::only_contractual_database_classes_cross_the_boundary ... ok
test operation::tests::the_status_filter_is_a_closed_vocabulary ... ok
test operation::tests::a_booking_refuses_noncanonical_and_empty_slots ... ok
test operation::tests::a_different_slot_is_a_different_command ... ok
test operation::tests::one_instant_spelled_two_ways_is_one_command ... ok
test operation::tests::the_command_identity_ignores_the_idempotency_key ... ok
test wire::tests::each_refusal_carries_the_detail_its_code_declares ... ok
test wire::tests::a_response_pairs_every_item_with_its_request_id ... ok
test wire::tests::the_envelope_admits_only_identified_bounded_items ... ok

test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

```

The tests cover the pure decisions:

- the canonical UUID spelling and the UTC timestamp re-spelling,
- the half-open day bounds of one calendar day,
- the command identity that `appointment.book` hashes,
- the closed status vocabulary and the request envelope,
- the statement-error translation,
- the refusal detail that each error code declares.

Format and lint:

```
$ cargo fmt -p dock --check
(no output: the crate is rustfmt clean)
$ cargo clippy -p dock --all-targets --offline 2>&1 | grep "application/dock/src"
(no output: no lint names a hand-written source line)
```

Clippy still reports `result_large_err` on `packages/dock/generated/wamn/*.rs`.
That is generator output. `wamn-postgres-statements` already emits the same
class of warning in this tree, so I left it alone.

## The loop

This run is commit `0f43d0a3`. It is the release every request below reached.

```
$ wamn dev --config "$WAMN_DEV_CONFIG" --overlay-root packages/dock --hold
run completed: migrate,introspect,generate,build,virtualize,apply,acl,admit,gate,publish,release,activate
run served: http://127.0.0.1:46159 host=receiving.localhost
```

The host log for the same run records the release it welded to and the
component it compiled:

```
INFO release component pull completed component_digest=sha256:e8c691e22503680b541a061b673995a7e978cc84a398bab6fd4812f675ef5602 component_bytes=516750 elapsed_ms=46
INFO release component compilation completed component_digest=sha256:e8c691e22503680b541a061b673995a7e978cc84a398bab6fd4812f675ef5602 compile_ms=26 compile_wall_ms=26
INFO synchronous release preload completed synchronous_wirings=5 component_digests=1 elapsed_ms=163
INFO wamn-host welded to its release effective_release_id=1 manifest_digest=sha256:189252903d1f1a30f66561a84066aad53f1a6368d4819c3c94cf355b37bb5276
```

## Every operation the scenario names

Every request below went to the base URL that the held run printed, with
`Host: receiving.localhost` and the route-caller PAT from
`$WAMN_ROUTE_CALLER_PAT_FILE`. The token is redacted in the command lines and
was sent in full. Responses are the exact bytes the route returned,
pretty-printed.

```
### DOCK-0  carrier.create -- first carrier
$ curl -sS -X POST http://127.0.0.1:46159/dock/carrier/create \
    -H 'Host: receiving.localhost' -H 'Content-Type: application/json' \
    -H 'Authorization: Bearer <route-caller PAT>' \
    -d '[{"request_id":"c-a","carrier_code":"T1-NORTHWIND","carrier_name":"Northwind Freight"}]'
HTTP 200
[
  {
    "request_id": "c-a",
    "value": {
      "carrier_code": "T1-NORTHWIND",
      "carrier_id": "490fde9f-4fe9-4e2f-ba3a-ad1639656dfa",
      "carrier_name": "Northwind Freight"
    }
  }
]

### DOCK-0  carrier.create -- second carrier
$ curl -sS -X POST http://127.0.0.1:46159/dock/carrier/create \
    -H 'Host: receiving.localhost' -H 'Content-Type: application/json' \
    -H 'Authorization: Bearer <route-caller PAT>' \
    -d '[{"request_id":"c-b","carrier_code":"T1-SOUTHBOUND","carrier_name":"Southbound Lines"}]'
HTTP 200
[
  {
    "request_id": "c-b",
    "value": {
      "carrier_code": "T1-SOUTHBOUND",
      "carrier_id": "dcc99305-865e-4493-b545-4132479dae06",
      "carrier_name": "Southbound Lines"
    }
  }
]

### DOCK-0  dock.create -- first dock
$ curl -sS -X POST http://127.0.0.1:46159/dock/dock/create \
    -H 'Host: receiving.localhost' -H 'Content-Type: application/json' \
    -H 'Authorization: Bearer <route-caller PAT>' \
    -d '[{"request_id":"d-1","dock_code":"T1-DOOR-1"}]'
HTTP 200
[
  {
    "request_id": "d-1",
    "value": {
      "dock_code": "T1-DOOR-1",
      "dock_id": "6c580450-7cd9-4238-bbb4-38f73338cef0"
    }
  }
]

### DOCK-0  dock.create -- second dock
$ curl -sS -X POST http://127.0.0.1:46159/dock/dock/create \
    -H 'Host: receiving.localhost' -H 'Content-Type: application/json' \
    -H 'Authorization: Bearer <route-caller PAT>' \
    -d '[{"request_id":"d-2","dock_code":"T1-DOOR-2"}]'
HTTP 200
[
  {
    "request_id": "d-2",
    "value": {
      "dock_code": "T1-DOOR-2",
      "dock_id": "e35e3021-c821-47f8-93f1-7fd101481c98"
    }
  }
]

### DOCK-0  appointment.book -- a dock identity nothing created
$ curl -sS -X POST http://127.0.0.1:46159/dock/appointment/book \
    -H 'Host: receiving.localhost' -H 'Content-Type: application/json' \
    -H 'Authorization: Bearer <route-caller PAT>' \
    -d '[{"request_id":"b-unknown-dock","value":{"idempotency_key":"T1-unknown-dock","dock_id":"00000000-0000-4000-8000-000000000000","carrier_id":"490fde9f-4fe9-4e2f-ba3a-ad1639656dfa","slot_start":"2026-09-10T08:00:00.000000Z","slot_end":"2026-09-10T09:00:00.000000Z"}}]'
HTTP 200
[
  {
    "error": {
      "code": "dock_not_found",
      "detail": {
        "field": "value.dock_id",
        "id": "00000000-0000-4000-8000-000000000000"
      }
    },
    "request_id": "b-unknown-dock"
  }
]

### DOCK-0  appointment.book -- a carrier identity nothing created
$ curl -sS -X POST http://127.0.0.1:46159/dock/appointment/book \
    -H 'Host: receiving.localhost' -H 'Content-Type: application/json' \
    -H 'Authorization: Bearer <route-caller PAT>' \
    -d '[{"request_id":"b-unknown-carrier","value":{"idempotency_key":"T1-unknown-carrier","dock_id":"6c580450-7cd9-4238-bbb4-38f73338cef0","carrier_id":"00000000-0000-4000-8000-000000000001","slot_start":"2026-09-10T08:00:00.000000Z","slot_end":"2026-09-10T09:00:00.000000Z"}}]'
HTTP 200
[
  {
    "error": {
      "code": "carrier_not_found",
      "detail": {
        "field": "value.carrier_id",
        "id": "00000000-0000-4000-8000-000000000001"
      }
    },
    "request_id": "b-unknown-carrier"
  }
]

### DOCK-2  appointment.book -- books 08:00-09:00 on dock one
$ curl -sS -X POST http://127.0.0.1:46159/dock/appointment/book \
    -H 'Host: receiving.localhost' -H 'Content-Type: application/json' \
    -H 'Authorization: Bearer <route-caller PAT>' \
    -d '[{"request_id":"b-1","value":{"idempotency_key":"T1-morning","dock_id":"6c580450-7cd9-4238-bbb4-38f73338cef0","carrier_id":"490fde9f-4fe9-4e2f-ba3a-ad1639656dfa","slot_start":"2026-09-10T08:00:00.000000Z","slot_end":"2026-09-10T09:00:00.000000Z"}}]'
HTTP 200
[
  {
    "request_id": "b-1",
    "value": {
      "appointment_id": "0de8b512-82ad-4ca3-ae0c-0bd2cead3ae9",
      "arrived_at": null,
      "carrier_id": "490fde9f-4fe9-4e2f-ba3a-ad1639656dfa",
      "dock_id": "6c580450-7cd9-4238-bbb4-38f73338cef0",
      "slot_end": "2026-09-10T09:00:00.000000Z",
      "slot_start": "2026-09-10T08:00:00.000000Z",
      "status": "scheduled"
    }
  }
]

### DOCK-2  appointment.book -- replay: same key, same request, new request_id
$ curl -sS -X POST http://127.0.0.1:46159/dock/appointment/book \
    -H 'Host: receiving.localhost' -H 'Content-Type: application/json' \
    -H 'Authorization: Bearer <route-caller PAT>' \
    -d '[{"request_id":"b-1-retry","value":{"idempotency_key":"T1-morning","dock_id":"6c580450-7cd9-4238-bbb4-38f73338cef0","carrier_id":"490fde9f-4fe9-4e2f-ba3a-ad1639656dfa","slot_start":"2026-09-10T08:00:00.000000Z","slot_end":"2026-09-10T09:00:00.000000Z"}}]'
HTTP 200
[
  {
    "request_id": "b-1-retry",
    "value": {
      "appointment_id": "0de8b512-82ad-4ca3-ae0c-0bd2cead3ae9",
      "arrived_at": null,
      "carrier_id": "490fde9f-4fe9-4e2f-ba3a-ad1639656dfa",
      "dock_id": "6c580450-7cd9-4238-bbb4-38f73338cef0",
      "slot_end": "2026-09-10T09:00:00.000000Z",
      "slot_start": "2026-09-10T08:00:00.000000Z",
      "status": "scheduled"
    }
  }
]

### DOCK-2  appointment.book -- replay: the same instants spelled with an offset
$ curl -sS -X POST http://127.0.0.1:46159/dock/appointment/book \
    -H 'Host: receiving.localhost' -H 'Content-Type: application/json' \
    -H 'Authorization: Bearer <route-caller PAT>' \
    -d '[{"request_id":"b-1-offset","value":{"idempotency_key":"T1-morning","dock_id":"6c580450-7cd9-4238-bbb4-38f73338cef0","carrier_id":"490fde9f-4fe9-4e2f-ba3a-ad1639656dfa","slot_start":"2026-09-10T09:00:00+01:00","slot_end":"2026-09-10T10:00:00+01:00"}}]'
HTTP 200
[
  {
    "request_id": "b-1-offset",
    "value": {
      "appointment_id": "0de8b512-82ad-4ca3-ae0c-0bd2cead3ae9",
      "arrived_at": null,
      "carrier_id": "490fde9f-4fe9-4e2f-ba3a-ad1639656dfa",
      "dock_id": "6c580450-7cd9-4238-bbb4-38f73338cef0",
      "slot_end": "2026-09-10T09:00:00.000000Z",
      "slot_start": "2026-09-10T08:00:00.000000Z",
      "status": "scheduled"
    }
  }
]

### DOCK-3  appointment.book -- the same key with a different slot
$ curl -sS -X POST http://127.0.0.1:46159/dock/appointment/book \
    -H 'Host: receiving.localhost' -H 'Content-Type: application/json' \
    -H 'Authorization: Bearer <route-caller PAT>' \
    -d '[{"request_id":"b-1-changed","value":{"idempotency_key":"T1-morning","dock_id":"6c580450-7cd9-4238-bbb4-38f73338cef0","carrier_id":"490fde9f-4fe9-4e2f-ba3a-ad1639656dfa","slot_start":"2026-09-10T10:00:00.000000Z","slot_end":"2026-09-10T11:00:00.000000Z"}}]'
HTTP 200
[
  {
    "error": {
      "code": "idempotency_conflict",
      "detail": {
        "field": "value.idempotency_key"
      }
    },
    "request_id": "b-1-changed"
  }
]

### DOCK-1  appointment.book -- 08:30-09:30 overlaps the booked slot
$ curl -sS -X POST http://127.0.0.1:46159/dock/appointment/book \
    -H 'Host: receiving.localhost' -H 'Content-Type: application/json' \
    -H 'Authorization: Bearer <route-caller PAT>' \
    -d '[{"request_id":"b-2","value":{"idempotency_key":"T1-overlap","dock_id":"6c580450-7cd9-4238-bbb4-38f73338cef0","carrier_id":"dcc99305-865e-4493-b545-4132479dae06","slot_start":"2026-09-10T08:30:00.000000Z","slot_end":"2026-09-10T09:30:00.000000Z"}}]'
HTTP 200
[
  {
    "error": {
      "code": "slot_unavailable",
      "detail": {
        "field": "value.slot_start",
        "id": "0de8b512-82ad-4ca3-ae0c-0bd2cead3ae9"
      }
    },
    "request_id": "b-2"
  }
]

### DOCK-1  appointment.book -- 08:15-08:45 lies inside the booked slot
$ curl -sS -X POST http://127.0.0.1:46159/dock/appointment/book \
    -H 'Host: receiving.localhost' -H 'Content-Type: application/json' \
    -H 'Authorization: Bearer <route-caller PAT>' \
    -d '[{"request_id":"b-3","value":{"idempotency_key":"T1-contained","dock_id":"6c580450-7cd9-4238-bbb4-38f73338cef0","carrier_id":"dcc99305-865e-4493-b545-4132479dae06","slot_start":"2026-09-10T08:15:00.000000Z","slot_end":"2026-09-10T08:45:00.000000Z"}}]'
HTTP 200
[
  {
    "error": {
      "code": "slot_unavailable",
      "detail": {
        "field": "value.slot_start",
        "id": "0de8b512-82ad-4ca3-ae0c-0bd2cead3ae9"
      }
    },
    "request_id": "b-3"
  }
]

### DOCK-1  appointment.book -- 09:00-10:00 starts where the booked slot ends
$ curl -sS -X POST http://127.0.0.1:46159/dock/appointment/book \
    -H 'Host: receiving.localhost' -H 'Content-Type: application/json' \
    -H 'Authorization: Bearer <route-caller PAT>' \
    -d '[{"request_id":"b-4","value":{"idempotency_key":"T1-adjacent","dock_id":"6c580450-7cd9-4238-bbb4-38f73338cef0","carrier_id":"dcc99305-865e-4493-b545-4132479dae06","slot_start":"2026-09-10T09:00:00.000000Z","slot_end":"2026-09-10T10:00:00.000000Z"}}]'
HTTP 200
[
  {
    "request_id": "b-4",
    "value": {
      "appointment_id": "fdf7b00e-c25f-4976-9965-23446bdfff9a",
      "arrived_at": null,
      "carrier_id": "dcc99305-865e-4493-b545-4132479dae06",
      "dock_id": "6c580450-7cd9-4238-bbb4-38f73338cef0",
      "slot_end": "2026-09-10T10:00:00.000000Z",
      "slot_start": "2026-09-10T09:00:00.000000Z",
      "status": "scheduled"
    }
  }
]

### DOCK-1  appointment.book -- 08:00-09:00 on dock two
$ curl -sS -X POST http://127.0.0.1:46159/dock/appointment/book \
    -H 'Host: receiving.localhost' -H 'Content-Type: application/json' \
    -H 'Authorization: Bearer <route-caller PAT>' \
    -d '[{"request_id":"b-5","value":{"idempotency_key":"T1-other-dock","dock_id":"e35e3021-c821-47f8-93f1-7fd101481c98","carrier_id":"dcc99305-865e-4493-b545-4132479dae06","slot_start":"2026-09-10T08:00:00.000000Z","slot_end":"2026-09-10T09:00:00.000000Z"}}]'
HTTP 200
[
  {
    "request_id": "b-5",
    "value": {
      "appointment_id": "c43452ff-37f3-4b5f-859b-00f6c64644fa",
      "arrived_at": null,
      "carrier_id": "dcc99305-865e-4493-b545-4132479dae06",
      "dock_id": "e35e3021-c821-47f8-93f1-7fd101481c98",
      "slot_end": "2026-09-10T09:00:00.000000Z",
      "slot_start": "2026-09-10T08:00:00.000000Z",
      "status": "scheduled"
    }
  }
]

### DOCK-4  appointment.check_in -- records the arrival the caller supplies
$ curl -sS -X POST http://127.0.0.1:46159/dock/appointment/check_in \
    -H 'Host: receiving.localhost' -H 'Content-Type: application/json' \
    -H 'Authorization: Bearer <route-caller PAT>' \
    -d '[{"request_id":"in-1","appointment_id":"0de8b512-82ad-4ca3-ae0c-0bd2cead3ae9","arrived_at":"2026-09-10T08:07:31.000000Z"}]'
HTTP 200
[
  {
    "request_id": "in-1",
    "value": {
      "appointment_id": "0de8b512-82ad-4ca3-ae0c-0bd2cead3ae9",
      "arrived_at": "2026-09-10T08:07:31.000000Z",
      "carrier_id": "490fde9f-4fe9-4e2f-ba3a-ad1639656dfa",
      "dock_id": "6c580450-7cd9-4238-bbb4-38f73338cef0",
      "slot_end": "2026-09-10T09:00:00.000000Z",
      "slot_start": "2026-09-10T08:00:00.000000Z",
      "status": "arrived"
    }
  }
]

### DOCK-4  appointment.check_in -- the same appointment a second time
$ curl -sS -X POST http://127.0.0.1:46159/dock/appointment/check_in \
    -H 'Host: receiving.localhost' -H 'Content-Type: application/json' \
    -H 'Authorization: Bearer <route-caller PAT>' \
    -d '[{"request_id":"in-1-again","appointment_id":"0de8b512-82ad-4ca3-ae0c-0bd2cead3ae9","arrived_at":"2026-09-10T08:20:00.000000Z"}]'
HTTP 200
[
  {
    "error": {
      "code": "appointment_not_scheduled",
      "detail": {
        "field": "appointment_id",
        "id": "0de8b512-82ad-4ca3-ae0c-0bd2cead3ae9"
      }
    },
    "request_id": "in-1-again"
  }
]

### DOCK-5  appointment.check_in -- an appointment that does not exist
$ curl -sS -X POST http://127.0.0.1:46159/dock/appointment/check_in \
    -H 'Host: receiving.localhost' -H 'Content-Type: application/json' \
    -H 'Authorization: Bearer <route-caller PAT>' \
    -d '[{"request_id":"in-missing","appointment_id":"00000000-0000-4000-8000-0000000000ff","arrived_at":"2026-09-10T08:07:31.000000Z"}]'
HTTP 200
[
  {
    "error": {
      "code": "not_found",
      "detail": {
        "field": "appointment_id",
        "id": "00000000-0000-4000-8000-0000000000ff"
      }
    },
    "request_id": "in-missing"
  }
]

### DOCK-6  appointment.query -- dock one on 2026-09-10, every status
$ curl -sS -X POST http://127.0.0.1:46159/dock/appointment/query \
    -H 'Host: receiving.localhost' -H 'Content-Type: application/json' \
    -H 'Authorization: Bearer <route-caller PAT>' \
    -d '[{"request_id":"q-all","dock_id":"6c580450-7cd9-4238-bbb4-38f73338cef0","day":"2026-09-10"}]'
HTTP 200
[
  {
    "request_id": "q-all",
    "value": {
      "rows": [
        {
          "appointment_id": "0de8b512-82ad-4ca3-ae0c-0bd2cead3ae9",
          "arrived_at": "2026-09-10T08:07:31.000000Z",
          "carrier_id": "490fde9f-4fe9-4e2f-ba3a-ad1639656dfa",
          "dock_id": "6c580450-7cd9-4238-bbb4-38f73338cef0",
          "slot_end": "2026-09-10T09:00:00.000000Z",
          "slot_start": "2026-09-10T08:00:00.000000Z",
          "status": "arrived"
        },
        {
          "appointment_id": "fdf7b00e-c25f-4976-9965-23446bdfff9a",
          "arrived_at": null,
          "carrier_id": "dcc99305-865e-4493-b545-4132479dae06",
          "dock_id": "6c580450-7cd9-4238-bbb4-38f73338cef0",
          "slot_end": "2026-09-10T10:00:00.000000Z",
          "slot_start": "2026-09-10T09:00:00.000000Z",
          "status": "scheduled"
        }
      ]
    }
  }
]

### DOCK-6  appointment.query -- the same day filtered to scheduled
$ curl -sS -X POST http://127.0.0.1:46159/dock/appointment/query \
    -H 'Host: receiving.localhost' -H 'Content-Type: application/json' \
    -H 'Authorization: Bearer <route-caller PAT>' \
    -d '[{"request_id":"q-scheduled","dock_id":"6c580450-7cd9-4238-bbb4-38f73338cef0","day":"2026-09-10","status":"scheduled"}]'
HTTP 200
[
  {
    "request_id": "q-scheduled",
    "value": {
      "rows": [
        {
          "appointment_id": "fdf7b00e-c25f-4976-9965-23446bdfff9a",
          "arrived_at": null,
          "carrier_id": "dcc99305-865e-4493-b545-4132479dae06",
          "dock_id": "6c580450-7cd9-4238-bbb4-38f73338cef0",
          "slot_end": "2026-09-10T10:00:00.000000Z",
          "slot_start": "2026-09-10T09:00:00.000000Z",
          "status": "scheduled"
        }
      ]
    }
  }
]

### DOCK-6  appointment.query -- the same day filtered to arrived
$ curl -sS -X POST http://127.0.0.1:46159/dock/appointment/query \
    -H 'Host: receiving.localhost' -H 'Content-Type: application/json' \
    -H 'Authorization: Bearer <route-caller PAT>' \
    -d '[{"request_id":"q-arrived","dock_id":"6c580450-7cd9-4238-bbb4-38f73338cef0","day":"2026-09-10","status":"arrived"}]'
HTTP 200
[
  {
    "request_id": "q-arrived",
    "value": {
      "rows": [
        {
          "appointment_id": "0de8b512-82ad-4ca3-ae0c-0bd2cead3ae9",
          "arrived_at": "2026-09-10T08:07:31.000000Z",
          "carrier_id": "490fde9f-4fe9-4e2f-ba3a-ad1639656dfa",
          "dock_id": "6c580450-7cd9-4238-bbb4-38f73338cef0",
          "slot_end": "2026-09-10T09:00:00.000000Z",
          "slot_start": "2026-09-10T08:00:00.000000Z",
          "status": "arrived"
        }
      ]
    }
  }
]

### DOCK-6  appointment.query -- the day before holds nothing
$ curl -sS -X POST http://127.0.0.1:46159/dock/appointment/query \
    -H 'Host: receiving.localhost' -H 'Content-Type: application/json' \
    -H 'Authorization: Bearer <route-caller PAT>' \
    -d '[{"request_id":"q-day-before","dock_id":"6c580450-7cd9-4238-bbb4-38f73338cef0","day":"2026-09-09"}]'
HTTP 200
[
  {
    "request_id": "q-day-before",
    "value": {
      "rows": []
    }
  }
]

### DOCK-6  appointment.query -- dock two on the same day
$ curl -sS -X POST http://127.0.0.1:46159/dock/appointment/query \
    -H 'Host: receiving.localhost' -H 'Content-Type: application/json' \
    -H 'Authorization: Bearer <route-caller PAT>' \
    -d '[{"request_id":"q-dock-two","dock_id":"e35e3021-c821-47f8-93f1-7fd101481c98","day":"2026-09-10"}]'
HTTP 200
[
  {
    "request_id": "q-dock-two",
    "value": {
      "rows": [
        {
          "appointment_id": "c43452ff-37f3-4b5f-859b-00f6c64644fa",
          "arrived_at": null,
          "carrier_id": "dcc99305-865e-4493-b545-4132479dae06",
          "dock_id": "e35e3021-c821-47f8-93f1-7fd101481c98",
          "slot_end": "2026-09-10T09:00:00.000000Z",
          "slot_start": "2026-09-10T08:00:00.000000Z",
          "status": "scheduled"
        }
      ]
    }
  }
]

```

A driver script asserted every invariant as it walked the transcript, so a
reader does not have to compare the responses by eye:

```
$ python3 /tmp/drive.py
ok  DOCK-0 four distinct identities were returned
ok  DOCK-0 an unknown dock refuses dock_not_found
ok  DOCK-0 an unknown carrier refuses carrier_not_found
ok  DOCK-2 a new appointment starts scheduled with no arrival
ok  DOCK-2 the replay returns the same appointment id
ok  DOCK-2 one instant spelled two ways is one command
ok  DOCK-3 a rebound key refuses idempotency_conflict
ok  DOCK-1 an overlapping slot refuses slot_unavailable
ok  DOCK-1 the refusal names the appointment it collided with
ok  DOCK-1 a contained slot refuses slot_unavailable
ok  DOCK-1 a touching slot books, so the slot bound is half-open
ok  DOCK-1 overlap is judged per dock, not across docks
ok  DOCK-4 check-in moves scheduled to arrived
ok  DOCK-4 check-in records the supplied arrival time
ok  DOCK-4 status only moves forward
ok  DOCK-5 an unknown appointment refuses not_found
ok  DOCK-5 the refusal names the field and the id
ok  DOCK-6 the list is one dock's day in slot order
ok  DOCK-6 the order is the slot order
ok  DOCK-6 the list holds only the named dock
ok  DOCK-6 a status filter selects only that status
ok  DOCK-6 the arrived filter selects the checked-in appointment
ok  DOCK-6 the day bound excludes the neighbouring day
ok  DOCK-6 each dock lists only its own appointments
every named operation and invariant was exercised
```

## DOCK-1 and DOCK-2 under concurrency

DOCK-1 says the database never holds an overlapping pair, whatever else
happens. A sequential refusal does not prove that. So I sent overlapping
bookings at the same time against the same running release.

```
$ python3 /tmp/race.py
### DOCK-1 concurrency: 8 mutually overlapping bookings on one dock, sent at once
booked   = 1 ['4e8a6d6c-2f55-485b-9523-062e2caba0a1']
refused  = 7 ['slot_unavailable']

### DOCK-2 concurrency: 16 identical requests under one key, sent at once
distinct appointment ids = 1 ['fd136f3e-9ba9-4642-9117-f9814a85d373']
refusals                 = []

both concurrency probes hold
```

Eight mutually overlapping bookings arrived together on one dock. One booked
and seven were refused. Sixteen identical requests arrived together under one
idempotency key. All sixteen returned the same appointment id and none was
refused.

## The database, read only

I read the project-environment database to make sure that the invariant holds
in the data and not only in the answers.

```
$ psql "$target_database_url" -c "<overlapping pair probe>"
 overlapping_pairs | appointments
-------------------+--------------
                 0 |            6
(1 row)

$ psql "$target_database_url" -c "<claim table probe>"
 idempotency_key | has_appointment
-----------------+-----------------
 key-morning     | t
 T1-adjacent     | t
 T1-morning      | t
 T1-other-dock   | t
(4 rows)

$ psql "$target_database_url" -c "<appointment rows>"
 dock_code |       slot_start       |        slot_end        |  status   |       arrived_at
-----------+------------------------+------------------------+-----------+------------------------
 GT-DOOR-1 | 2026-09-10 08:00:00+00 | 2026-09-10 09:00:00+00 | scheduled |
 T1-DOOR-1 | 2026-09-10 08:00:00+00 | 2026-09-10 09:00:00+00 | arrived   | 2026-09-10 08:07:31+00
 T1-DOOR-1 | 2026-09-10 09:00:00+00 | 2026-09-10 10:00:00+00 | scheduled |
 T1-DOOR-2 | 2026-09-10 08:00:00+00 | 2026-09-10 09:00:00+00 | scheduled |
(4 rows)
```

The first probe is the DOCK-1 statement written as SQL, and it counts zero.
The claim table holds one row per booking that committed. The four keys that
were refused (`T1-unknown-dock`, `T1-unknown-carrier`, `T1-overlap`,
`T1-contained`) are absent, which is the evidence that a refusal rolls its
idempotency claim back. The `GT-` and `key-morning` rows come from an earlier
exploratory pass against the same release.

## What the loop does for the delivered tree

```
$ git log --oneline -4
7263361f Revert "feat(dock)!: publish the formatted component as dock@1.1.0"
ed75ba5f feat(dock)!: publish the formatted component as dock@1.1.0
773deb28 style(dock): rustfmt the component and name its two lint overrides
0f43d0a3 feat(dock): dock appointments as a wamn package

$ git status --short
(no output)

$ wamn dev --config "$WAMN_DEV_CONFIG" --overlay-root packages/dock
applied dock@1.0.0: 1 migration(s)
applied dock@1.0.0: 0 migration(s) (already converged)
Error: dev-stage-failed at acl: dev-stage-owner-failed while reconcile generated package data access: package-data-access-installed-set-mismatch: missing-artifacts=[dock@1.1.0]; unexpected-artifacts=[]; remedy=present every applied package root
```

Migrate, Introspect, Generate, Build, Virtualize and Apply all pass. Acl
refuses. "Where I got stuck" explains why, and shows the same refusal from the
other side of the version bump.

## What I did not verify, and why

- I did not exercise the delivered tree against a release built from the
  delivered tree. The loop cannot reach Publish in this environment any more.
  The delivered tree differs from the tree behind the transcript by rustfmt
  whitespace and two lint attributes, shown in full by
  `git diff 0f43d0a3 HEAD`. No production statement, SQL file, manifest field
  or wire shape differs.
- I did not exercise `appointment.query` past the host row limit. The result
  class is `bounded_list` and the host owns that bound, so a page boundary is
  not this package's contract to prove.
- I did not reach a `departed` appointment. The scenario names the status but
  pins no operation that sets it, so the package stores the vocabulary and
  offers no transition into it.
- I did not exercise `retry`, `timeout` or `permission_denied`. Each needs an
  injected database or authority failure, and the loop offers no injection
  seam.
- I did not exercise the multi-item envelope past two items. Every operation
  accepts one to one hundred items and refuses each item on its own. The
  transcript sends one item per call.
- I did not run the repository gates in `docs/operations/build-and-test.md`.
  They need a kind cluster and the two-stage image, which is outside this
  task's paths and its environment.

# Decisions

Nobody was available to answer questions, so each decision below is mine and is
recorded here.

## One dock row lock, not an exclusion constraint

The natural way to write
DOCK-1 is a Postgres `EXCLUDE` constraint over `(dock_id, tstzrange)`. Two
repository rules forbid it. `crates/schema/introspection/src/migration_policy.rs`
admits only ordinary schema-qualified `CREATE TABLE` and a narrow `ALTER TABLE`,
with no `CREATE EXTENSION` for `btree_gist`. `crates/schema/introspection/src/postgres.rs`
refuses any `pg_constraint` type outside `p`, `u`, `f` and `c`, and any index that
is not a plain btree. So `appointment.book` takes `SELECT id FROM dock
WHERE id = $1 FOR UPDATE` as its own statement, and looks for an overlap in the
next statement. The split matters. Under READ COMMITTED a statement takes its snapshot
when it starts. An overlap test inside the locking statement can therefore
still read the state from before the lock was granted. The next statement takes
a fresh snapshot and sees the row the winner committed. The concurrency probe
above is the evidence.

## Claim the idempotency key before doing the work

`appointment.book` looks
for a replay, then inserts the key into `book_appointment_command` with
`ON CONFLICT DO NOTHING RETURNING appointment_id`, then locks the dock. The
claim mints the appointment id, so a replay returns an id that was decided
before the work, not one the work minted twice. This is the shape
`packages/receiving/command/record_receipt` already uses. A refusal drops the
transaction and the claim with it, so a key names a booking only once one
exists. The database probe above shows the four refused keys absent.

## A retry that respells one instant is a replay, not a conflict

`appointment.book` accepts any RFC 3339 timestamp with an offset, re-spells it
as UTC with six fractional digits, and hashes what it re-spelled. So
`2026-09-10T09:00:00+01:00` and `2026-09-10T08:00:00.000000Z` are one command.
`packages/receiving` chose the stricter rule and refuses anything but the
canonical spelling. I chose the looser one because the manifest's own
`CommandCanonicalization` doc argues for it. A spelling that validates but is
never re-spelled turns a genuine retry into a conflict.
The transcript proves both directions, and a unit test pins the identity.

## A day is a UTC calendar date

`appointment.query` takes `day` as
`YYYY-MM-DD` and covers `[day 00:00Z, day+1 00:00Z)` by `slot_start`. A slot is
an instant, so "one day" needs a zone, and the scenario names none. UTC is the
only zone this package knows. The caller sends a date and never a pair of
bounds, so the choice is stated in one place.

## The sort is fixed, and the status filter is optional

DOCK-6 pins "sorted
by slot start, and the order is the slot order", so `appointment.query` orders
by `slot_start` then `id` and takes no direction argument. `status` is nullable
and absent means every status. The prose says "sortable by slot", and a
direction argument nobody asked for is the kind of flexibility
`AGENTS.md` tells me not to add.

## No `appointment.depart` operation

The scenario names `departed` in the
status vocabulary and pins no operation that reaches it. The check constraint
admits the value and no operation writes it.

## One crate, not two

Every other package here splits its component into a
guest and a data-access rlib. `allowed_paths` names only
`components/application/dock/**`, and a second crate raises the
`package_count` in `architecture/workspace-tiers.json`, which is not mine to
edit. So `components/application/dock` holds the WIT export layer and the data
access in one crate, in separate modules.

## Errors beyond the three pinned codes

The scenario pins
`slot_unavailable`, `idempotency_conflict` and `not_found`, and leaves the rest
to me. I named four more refusals:

- `dock_not_found` and `carrier_not_found`, so an unknown identity does not
  arrive as `internal_error`,
- `appointment_not_scheduled`, so a second check-in says what it means,
- `carrier_code_conflict` and `dock_code_conflict`, for the two unique codes.

`slot_unavailable` carries the id of the appointment it collided with.

## The delivered version is 1.0.0

See "Where I got stuck". `dock` is a
greenfield package and its first version is 1.0.0. The 1.1.0 existed only
because this worktree reformatted a component that was already published. That
is an accident of my working order and not a property of the package.

# Where I got stuck

Two refusals, in order. Both are real platform rules and I did not work around
either one.

## Refusal one: a published package version is immutable

After the release
was activated I ran `cargo fmt` on the component and added two `#[expect]`
attributes. The next loop run refused:

```
Error: dev-stage-failed at admit: dev-stage-owner-failed while project
admission into the environment: component-projection-refused
(component-fact-conflict): dock@1.0.0 component=dock interface-version=0.1.0
collides with different admitted facts
```

`services/ctl/src/push_component.rs` appends component facts keyed by
`(tenant, package_id, package_version, component, interface_version)` and
refuses a second row with different bytes. The bytes moved because rustfmt
moves lines, and `expect` bakes a `#[track_caller]` location into the artifact.
This rule is correct. My mistake was formatting after publishing, not before.

## Refusal two: a version bump ends the loop for that package

I bumped to
`dock@1.1.0` with `predecessor_version: "1.0.0"`, which is the path
`crates/schema/control/src/package_migrations.rs` documents. Migrate,
Introspect, Generate, Build, Virtualize and Apply all passed. Acl refused:

```
Error: dev-stage-failed at acl: dev-stage-owner-failed while reconcile
generated package data access:
package-data-access-installed-set-mismatch: missing-artifacts=[dock@1.0.0];
unexpected-artifacts=[]; remedy=present every applied package root
```

Two rules in `services/ctl/src/reconcile_package_data_access.rs` cannot both be
met after a bump. `validate_installed_set` reads
`SELECT package_id, package_version, manifest_sha256 FROM catalog.packages
WHERE tenant_id = $1` with no lineage filter, and requires the presented roots
to equal that whole set. Twenty lines earlier, `execute` refuses two roots that
share a package id with
`package-data-access-installed-set-repeats-package`. `catalog.packages` is
append-only and keeps the superseded coordinate.
`services/ctl/src/dev/config.rs` presents one overlay plus its declared base
dependencies, and a base dependency must not name the owning package. So no
`wamn dev` invocation can present both coordinates, and none can present fewer.

Reverting to `dock@1.0.0` gives the mirror image of the same refusal, which is
what the delivered tree does now:

```
Error: dev-stage-failed at acl: dev-stage-owner-failed while reconcile
generated package data access:
package-data-access-installed-set-mismatch: missing-artifacts=[dock@1.1.0];
unexpected-artifacts=[]; remedy=present every applied package root
```

The stage names a remedy that cannot be performed. I stopped after these two
attempts rather than trying a third shape. The refusal follows from the two
rules above and not from anything in my package.

What I did not do, and why:

- I did not delete the `dock@1.1.0` row from `catalog.packages`. The brief
  grants Postgres for inspection only, and removing a gate's input to make the
  gate pass is exactly what the brief forbids.
- I did not run `wamn dev up` again. It resets the control store and rewrites
  `$WAMN_DEV_CONFIG` and the environment secrets, and that directory belongs to
  the harness, not to this task.
- I did not use `wamn-ctl`. The brief says it is on PATH. It is a broken
  symlink here: `/home/kaalin/.cache/wamn-pilot/runs/030-claude-dock-appointments/bin/wamn-ctl`
  points at `target-fc7fb819/debug/wamn-ctl`, which does not exist.

A fresh project environment clears this. The delivered tree applies, admits,
publishes and activates from an empty `catalog.packages`, which is exactly what
commit `0f43d0a3` did.

# Rules I relied on

- Migrations author schema selection and admit only ordinary schema-qualified
  `CREATE TABLE` and a narrow additive `ALTER TABLE`
  (`crates/schema/introspection/src/migration_policy.rs`). SQL corpus files use
  unqualified relation names and inherit the host-injected `search_path`
  (`components/data/postgres-statements/wit/deps/wamn-postgres/package.wit`).
- Catalog introspection admits only `p`, `u`, `f` and `c` constraints and plain
  btree indexes (`crates/schema/introspection/src/postgres.rs`). This is what
  rules out an exclusion constraint.
- Every relation a migration creates must be declared as a model or as a
  CDC-excluded internal relation
  (`services/ctl/src/apply_package.rs`,
  `DEFINITION_OWNER_DECLARATION_MISSING_REFUSAL`).
- A package's declared relation privileges must equal what its SQL actually
  reads, writes and locks, derived from the SQL itself
  (`crates/schema/generator/src/generate.rs` and `sql_lex.rs`). `RETURNING`
  columns count as reads.
- An operation identity is `{package-id kebab}:{module}/{operation}@{version}`,
  so every operation identity moves when the package version moves
  (`crates/schema/generator/src/manifest.rs`,
  `canonical_operation_identity`).
- A package declares its own components in `wamn.json`, and
  `tools/build-components` derives the build and virtualization allowlists from
  those keys. Only the platform half stays in
  `architecture/workspace-tiers.json`, so a new package needs no central edit
  beyond its workspace member line.
- An attachment `definition-hash` is `sha256` over the compact `serde_json`
  encoding of its `definition`
  (`services/ctl/src/publish_release.rs`,
  `components/execution/contract/src/lib.rs`).
- Publish, Release, Activate, Apply, Acl and Admit require a committed
  worktree. Migrate through Virtualize and Gate run from saved bytes
  (`services/ctl/src/dev.rs`, `DevStage::boundary`).
- `wamn:node@0.1.0` copies are inventoried, so a consumer points at an existing
  vendored copy instead of adding one
  (`crates/platform/runtime/tests/node_wit_coherence.rs`).
- Contextual error structs inside the implementation, translated exactly once at
  the owning boundary, with the WIT-shaped literals as the wire contract
  (`AGENTS.md`, "Rust").

# Open questions

- Is `reconcile_package_data_access` meant to filter `catalog.packages` to the
  current coordinate of each lineage? Today a supported version bump ends the
  loop for that package in that environment. I cannot tell whether the
  installed-set rule means "every applied coordinate" on purpose, or whether it
  predates the predecessor lineage that `apply_package` records.
- Is there a supported way to retire a superseded package coordinate? I found
  no `DELETE` against `catalog.packages` anywhere in the production sources and
  no `wamn` verb for it.
- `wamn-ctl` is named in the brief and is a broken symlink in this environment.
  I do not know whether the loop was meant to offer a verb I therefore never
  saw.
- Does `appointment.query` need a set of statuses instead of one? "filtered
  by status" reads either way. One nullable status covers "all" and "one" and
  not "two of three".
- Does anything downstream want `departed`? The status vocabulary admits it and
  no operation reaches it. A future `appointment.depart` is therefore additive.
