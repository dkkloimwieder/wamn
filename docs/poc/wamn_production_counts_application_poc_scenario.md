# WAMN Production Counts: POC Scenario (app 2 of the portfolio)

Status: **DRAFT 2026-09-06, for owner review before code.** Bead
`wamn-7tva.1`, epic `wamn-7tva`. No code lands under this bead.

**AMENDED 2026-09-07 by owner ruling.** Three questions this draft left open
are answered, and the answers moved the ingress design. The MQTT seam is
reading B, and less than a bridge, because NATS captures MQTT natively. The
`wamn:jetstream` registry row is refused, because a tenant guest imports
nothing to receive events. `count.record` is an `event_handler` the platform
delivers to, not a pull consumer the guest drives. Sections 0, 5, 6, 12 and 13
carry the change. The rulings are recorded on `wamn-7tva.1` and `wamn-7tva.2`.

Structure follows the application-brief skeleton in
`docs/experiments/agent-authoring/protocol.md` section 4.5 (`:162-195`). Scope
is fixed by `docs/poc/poc-application-portfolio.md` row 2 (`:13`) and its
build-order entry 2 (`:27-28`). This document does not widen that scope. Shape
follows `docs/poc/wamn_wms_application_poc_scenario.md`.

**Every claim carries a `file:line` or a bead id. A claim with neither is a
design choice, and it says so.**

## 0. What the owner already banked

These are decided. This document does not re-decide them.

1. Production counts is machine data as production values against dispatched
   work orders (`wamn-7tva`, `docs/poc/poc-application-portfolio.md:13`).
2. The ingress is a JetStream pull consumer, with ack after process, and a
   dead-letter queue (`wamn-7tva`, `docs/poc/poc-application-portfolio.md:13`).
   [amended 2026-09-07: the PLATFORM owns that consumer. The application guest
   exports an `event_handler` and is delivered to. See section 5.]
3. The new seam is MQTT ingress. It is the second execution of
   `docs/poc/seam-template.md` (`docs/poc/poc-application-portfolio.md:13`).
   [amended 2026-09-07: withdrawn. NATS captures MQTT natively, so the seam
   template runs no steps here and keeps blobstore as its only proven
   execution. The trigger for a second execution is named in section 5.]
4. The exit gate is the MQTT simulator at 500 messages per second with 5
   percent duplicates, yielding byte-exact rollups, with the dead-letter queue
   receiving only poison (`docs/poc/poc-application-portfolio.md:27-28`).

**Dead-letter queue (DLQ).** A durable place for a message that a consumer
refused permanently. The rest of this document says DLQ.

## 1. Purpose and actors

Machines on production lines report counts. Each count is a production value
against a work order. That work order was dispatched to that line. This
application accepts each count exactly once. It then rolls counts up by hour
and by shift.

It is the portfolio's only streaming-ingress application
(`docs/poc/poc-application-portfolio.md:13`). Everything before it arrives over
a call.

| actor | what it does |
|---|---|
| line gateway | the identity the MQTT bridge runs as. It publishes machine counts. It never reads. |
| production supervisor | dispatches a work order to a line. Reads counts and rollups. |
| platform operator | binds connections and reads the DLQ. Not an application actor. |

Nothing here is scoped per user. Per-user row scope arrives in app 4
(`docs/poc/poc-application-portfolio.md:15`).

## 2. Domain nouns

App 2 owns its own tables and its own migrations
(`docs/poc/poc-application-portfolio.md:52-53`). It owns its `product` and
`production_line` reference tables. Cross-package table sharing is app 3's
deliberate introduction (`docs/poc/wamn_wms_application_poc_scenario.md:82-87`).

| noun | identified by | what it is |
|---|---|---|
| `production_line` | `line_code` | a line that runs work |
| `work_order` | `work_order_code` | a unit of planned production. Status is `planned` or `dispatched`. |
| `production_count` | `id` | one accepted machine reading. Append-only. |
| `count_command` | `idempotency_key` | the idempotency claim row. It holds a pre-generated `count_id` as a UNIQUE column. |

`count_command` copies the WMS shape exactly. WMS copied it from Receiving.
Idempotent replay returns the immutable original result, and the schema is what
keeps that true (`docs/poc/wamn_wms_application_poc_scenario.md:196-207`).

**Code shapes.** The landed profiles emit `PAL-000000`, `LOC-0000` and
`SKU-00000` (`test-support/simulator/src/profiles.rs:37-46`). App 2 needs two
new shapes. Proposal: `WO-000000` and `LINE-00`. **This is a design choice. No
ruling exists.** Shapes are shared across apps. Tables are not
(`docs/poc/poc-application-portfolio.md:45-49`).

Columns, types and indexes stay open to the author
(`docs/experiments/agent-authoring/protocol.md:174`).

## 3. Commands, and the invariants a grader names

Two commands. Every invariant carries an id. Grader steps name it by that id
(`docs/experiments/agent-authoring/protocol.md:184-186`).

### `work_order.dispatch`

Moves a planned work order onto a line. Kind: `command`.

- **WO-1.** Dispatch is permitted only from status `planned`. From any other
  status the command refuses `work_order_already_dispatched`.
- **WO-2.** A dispatched work order names exactly one line. The named line must
  exist. Otherwise the command refuses `line_not_found`.
- **WO-3.** The command takes a caller-supplied `idempotency_key`. A replay
  returns the immutable original result.
- **WO-4.** The command takes `expected_row_version` for the work order row. A
  mismatch refuses `concurrency_conflict`, carrying both revisions
  (`docs/poc/wamn_wms_application_poc_scenario.md:139-141`).

### `count.record`

Records one machine reading. Kind: `command`.

- **PC-1.** The named work order must exist and must be in status `dispatched`.
  Otherwise the command refuses `work_order_not_dispatched`.
- **PC-2.** The named line must equal the line the work order was dispatched
  to. Otherwise the command refuses `line_mismatch`.
- **PC-3.** Quantity must be an integer greater than zero. Otherwise the
  command refuses `invalid_quantity`.
- **PC-4.** The command takes a caller-supplied `idempotency_key`. A replay
  returns the immutable original result, including the original `count_id`.
- **PC-5.** An accepted `production_count` row is never updated and never
  deleted. No command in this package writes one twice.
- **PC-6.** `observed_at` is machine data. The platform stores it. The platform
  never treats it as commit order. Rollup windows key on `observed_at`.
  Ordering keys on the row's own `created_at`.
- **PC-7.** The envelope must carry a known `schema_version`. An unknown value
  refuses `unsupported_schema_version`.

### Why two commands and not one

Dispatch changes authority. Recording a count does not. A caller permitted to
report production is not thereby permitted to dispatch work. That is the same
reasoning WMS used to keep four inventory commands apart
(`docs/poc/wamn_wms_application_poc_scenario.md:116-120`).

### The refusal split the DLQ depends on

PC-1, PC-2, PC-3 and PC-7 are permanent refusals. A retry of the same bytes
gives the same answer. Section 5 sends them to the DLQ.

PC-4 replay is a success, not a refusal. Section 5 acks it.

There is no invariant capping accumulated quantity against a work order target.
Over-production is real. A refusal there is an invented business rule.

SQL, locking and error mapping stay open to the author
(`docs/experiments/agent-authoring/protocol.md:175`).

## 4. Queries

- `work_order.get` and `work_order.query` are generated CRUD.
- `production_count.query` is paged. Filters: `work_order_code`, `line_code`,
  and an `observed_at` range. Sorts: `observed_at` and `created_at`. Keyset
  pagination on `created_at` ascending, with an `id` tie-breaker. That cursor
  is mandatory: a model `query` must be `ResultClass::Page`, and `Page` demands
  it (`docs/poc/wamn_wms_application_poc_scenario.md:166-169`).
- `count.rollup` is a `projection`. It returns accepted quantity totals grouped
  by work order, line and window start. The window is `hour` or `shift`, a
  required parameter carried as a typed enum. It is a `bounded_list` with
  `get`-style refusals.

**`count.rollup` reads `production_count` directly. There is no stored rollup
table.** That keeps byte-exactness a property of the accepted row set alone.
WMS chose a projection for its aggregates for the same reason, and named
rollups as app 2's problem
(`docs/poc/wamn_wms_application_poc_scenario.md:213-220`, `:287`). **Keeping it
a projection here is a design choice. No ruling exists.**

The portfolio names time-series projection SQL with window functions as app 2's
corpus entry (`docs/poc/poc-application-portfolio.md:13`). `count.rollup` is
that entry.

Keyset shape and projections stay open to the author
(`docs/experiments/agent-authoring/protocol.md:176`).

## 5. Ingress

Two paths.

### `work_order.dispatch`: an HTTP route

Attachment kind `Http` (`crates/catalog/model/src/lib.rs:616-621`). The
simulator drives it with the landed HTTP target
(`test-support/simulator/src/emit.rs:52-66`).

**This is how the dispatch fixture is built.** The simulator drives a real
route. It never seeds the database
(`docs/poc/poc-application-portfolio.md:57`,
`test-support/simulator/src/lib.rs:5-6`). WMS built its fixture the same way,
through `inventory.adjust`
(`docs/poc/wamn_wms_application_poc_scenario.md:246-248`).

### `count.record`: an event handler the platform delivers to

The banked ingress is a JetStream pull consumer. The PLATFORM owns it. The
application guest owns no step below except the last one.

The ordered procedure, one step per sentence.

1. The `wamn:jetstream` host plugin binds a durable consumer over the ingress
   stream through `bind-registration`, which ties every fetched message to a
   release identity
   (`crates/platform/runtime/src/plugins/wamn_jetstream.rs:1568`,
   `crates/platform/runtime/wit/deps/wamn-jetstream/package.wit:150`).
2. It fetches a bounded batch
   (`crates/platform/runtime/wit/deps/wamn-jetstream/package.wit:112`). The
   `max-messages` argument is the backpressure control. The server-side ack
   floor holds the rest.
3. It reads each message's body, subject, headers and delivery metadata
   (`crates/platform/runtime/wit/deps/wamn-jetstream/package.wit:118-127`).
4. Delivery is gated on the serving release's registration projection, so an
   event whose registration identity is absent from the release never reaches a
   component (`services/host/src/host.rs:719-722`).
5. The run id is minted from the flow and the stream position, so a redelivery
   re-mints the SAME id and the write-ahead conflict clause absorbs the
   duplicate (`crates/execution/run-state/src/queue/evt.rs:1-16`).
6. `RouterDeliveryBridge` delivers the event into the wiring the registration
   names (`crates/execution/host/src/router_delivery.rs:385-462`).
7. The plugin settles the message: ack after the command commits and after an
   idempotent replay, nack with a delay on a transient platform failure, and
   dead-letter on a permanent refusal
   (`crates/platform/runtime/wit/deps/wamn-jetstream/package.wit:129-140`).
   PC-1, PC-2, PC-3, PC-7 and unparseable bytes are the permanent refusals.
8. The guest exports `count.record` as an `event_handler` operation
   (`crates/schema/generator/src/manifest.rs:75`) and is delivered to. It
   imports nothing to receive the event.

Consumer configuration carries `stream-name`, `durable`, `filter-subject`,
`ack-wait-ms` and `max-deliver`
(`crates/platform/runtime/wit/deps/wamn-jetstream/package.wit:86-102`).
Redelivery count arrives as `delivered` in the message metadata (`:52-56`).

`dead-letter` is reachable only on a message fetched through
`bind-registration`. The host derives the destination. Nothing names a DLQ
subject
(`crates/platform/runtime/wit/deps/wamn-jetstream/package.wit:136-140`). The
DLQ stream is `WAMN_DLQ` (`components/events/wire/src/lib.rs:256`).

**Why the guest imports nothing** (owner ruling 2026-09-07). A capability
registry row governs what a TENANT guest imports, and admission refuses an
unregistered import by absence
(`crates/platform/component-policy/src/lib.rs:99-100`). App 2's guest is a
tenant guest that only exports, so it needs no row. `wamn:jetstream` stays a
platform-side plugin, and the capability set stays at eight rows.

### What does not exist yet

The path above is not reachable today. Two gaps, down from four.

**I2. There is no external ingress stream.** Streams are provisioned out of
band (`crates/platform/runtime/src/plugins/wamn_jetstream.rs:19-21`). The only
provisioned family is `EVT_<org>_<env>`
(`crates/control/provision/src/name.rs:455-456`), created by the CDC reader
from a write-ahead-log publication (`services/cdc-reader/src/lib.rs:3`). An
MQTT ingress stream is neither.

**I3. There is no registration sourced from an external subject.** A
registration today is a CDC event reader by construction. `EventReader`
requires a `publication`, a replication `slot` and a replication secret, and it
carries `deny_unknown_fields`
(`crates/control/registry/src/types.rs:467-486`). An external MQTT source has
none of the three. The delivery-side registration is package-identified too:
the serving manifest keys each registration by package and source package
(`crates/execution/host/src/router_delivery.rs:462`), and the generator
requires the source package to be the package itself or a declared dependency
(`crates/schema/generator/src/manifest.rs:752-783`). Without an
external-source registration kind, host DLQ derivation has nothing to derive
from.

**I1 and I4 are withdrawn by owner ruling 2026-09-07.** I1 asked for a
`wamn:jetstream` registry row, which the ruling refuses: the guest imports
nothing to receive events. I4 asked for an MQTT seam, which the ruling
replaces with native NATS capability, measured below.

Each gap needs a bead. **This lane cannot create beads. The integrator must
file them.**

### The ruling, 2026-09-07

**Reading B, and less than a bridge.** NATS speaks MQTT natively and
JetStream-backed, since 2.2. Machines publish MQTT topics to the pinned NATS
server. JetStream captures them into the ingress stream. The platform consumes
that stream exactly as section 5 describes. There is no WIT, no registry row
and no bridge process. The seam template runs no steps here.

Measured on 2026-09-07, on a disposable container removed by name afterward.

| fact | result |
|---|---|
| the pin supports MQTT | `nats:2.10-alpine` reports server version 2.10.29 (`deploy/infra/nats-jetstream.yaml:145`, `test-support/infrastructure/std-virtualization.compose.yaml:71`) |
| our configuration enables it | no. There is no `mqtt` block in the tree. Enabling it is a ConfigMap block plus a port |
| the server runs it | with `jetstream` plus `mqtt { port: 1883 }` the log reads `Listening for MQTT clients on mqtt://0.0.0.0:1883` |
| capture is end to end | an MQTT publish to topic `counts/line1/tick` landed in stream `COUNTS`, subject filter `counts.>`, on subject `counts.line1.tick`, byte-exact |
| at-least-once is available | a QoS 1 publish returned PUBACK for its packet id and landed at stream sequence 2 |

MQTT topic separators map to NATS subject separators, so the subject an
external source writes is predictable from its topic.

**The seam template keeps ONE proven execution.** Blobstore stays its only
consumer until a CALLING consumer appears. A WIT package a guest imports for
something it RECEIVES rather than CALLS is machinery invented to exercise a
template, and rule R-C refuses it on its own text
(`docs/architecture/poc-architecture-review.md:34`). The named trigger is MQTT
publish FROM a component. `docs/exe-model.md:86` already lists MQTT among the
next node-ABI consumers after blob-put, which is that same publish direction.

## 6. External services

Only from the admitted list
(`docs/experiments/agent-authoring/protocol.md:178`).

| service | used for | status in the tree |
|---|---|---|
| `wamn:postgres` | every command and every query | registry row exists, posture `Effect` (`crates/platform/component-policy/src/lib.rs:137-141`) |
| JetStream | the count ingress and the DLQ | platform-side only. The plugin fetches and settles (`crates/platform/runtime/src/plugins/wamn_jetstream.rs:1568`). The guest imports nothing. **No registry row is needed** (ruling 2026-09-07). |
| MQTT | how machine counts reach the ingress stream | native NATS capability on the `nats:2.10-alpine` pin, measured 2026-09-07. Not enabled in our configuration yet. |
| `wamn:connection` | not used by app 2 | registry row exists (`crates/platform/component-policy/src/lib.rs:142-146`) |
| `wasmcloud:blobstore` | not used by app 2 | registry row exists (`crates/platform/component-policy/src/lib.rs:147-151`) |

Anything outside that list is a platform ask. This document lists three, and
flags each as platform work.

- **PA-1.** MQTT enabled on the pinned NATS server: an `mqtt` block and its
  port, in `deploy/infra/nats-jetstream.yaml` and in the disposable compose
  file. No code.
- **PA-2.** An external ingress stream, plus an external-source registration
  kind that declares a subject and an envelope contract and claims no package
  identity. Gaps I2 and I3, tracked on `wamn-7tva.2`.

The `wamn:jetstream` registry row and the MQTT seam were the third and fourth
asks of this draft. The owner refused both on 2026-09-07.

Binding names stay open to the author
(`docs/experiments/agent-authoring/protocol.md:178`).

## 7. Permissions

| actor | `work_order.dispatch` | `count.record` | `work_order` reads | `production_count.query` | `count.rollup` |
|---|---|---|---|---|---|
| line gateway | no | yes | no | no | no |
| production supervisor | yes | no | yes | yes | yes |

Nothing is per user. Per-user row scope arrives in app 4
(`docs/poc/poc-application-portfolio.md:15`).

Tokens stay open to the author
(`docs/experiments/agent-authoring/protocol.md:179`). A driver mints a personal
access token with `wamn_platform_identity::issue_pat`. A driver never
hand-assembles one (`test-support/simulator/src/emit.rs:70-72`).

## 8. UI

**None.** A brief that names screens requires UI. A brief that names none does
not, and a headless package is a valid application
(`docs/experiments/agent-authoring/protocol.md:167-169`). The exit gate names a
rate, a rollup and a DLQ. It names no screen. WMS ruled the same way for the
same reason (`docs/poc/wamn_wms_application_poc_scenario.md:47-49`).

## 9. Non-goals

```text
mqtt_qos2_and_retained_message_semantics
historical_backfill_and_replay_of_machine_data
work_order_scheduling_and_routing
work_order_completion_and_closeout
oee_downtime_and_fault_analytics        (app 5)
cross_package_shared_reference_data     (app 3)
per_user_row_scope                      (app 4)
stored_rollup_tables
operator_screen
```

## 10. Exit gate

Exactly the portfolio's (`docs/poc/poc-application-portfolio.md:27-28`),
restated as measurements. Item 4 comes from the epic's own wording
(`wamn-7tva`).

1. **Load.** A `machine_counts` profile at 500 messages per second, with
   `duplicate_pct` set to 5, seed pinned, driven into the ingress stream.
2. **Byte-exact rollups.** The canonical bytes of `count.rollup` after the run
   equal the canonical bytes of the same projection over the deduplicated
   stream. The comparable form is
   `wamn_execution_contract::canonical_json_bytes`
   (`test-support/simulator/src/lib.rs:20-24`).
3. **The DLQ receives only poison.** Every message in the per-registration DLQ
   is one the fault plan injected. Nothing else is there. `malformed_every` is
   the only poison source (`test-support/simulator/src/lib.rs:53-56`,
   `:132-140`), so the expected DLQ count is exact rather than approximate.
4. **Ack after process.** Kill the consumer mid-run. Restart it. The accepted
   set is unchanged and no count is lost.

### Where the banked gate meets what the tree can do

**E1. Nothing paces today.** `Profile::rate` is carried on the profile and is
never applied. Wall-clock pacing is an emitter concern
(`test-support/simulator/src/lib.rs:26-28`, `:67-69`). The landed HTTP target
does not read it (`test-support/simulator/src/emit.rs:117-140`). The JetStream
sink is the first emitter that must pace. **500 messages per second is new
work, not an existing capability.**

**E2. The duplicate knob already fits.** `duplicate_pct` is a percentage drawn
below 100 (`test-support/simulator/src/lib.rs:70`, `:142-155`), so 5 percent is
exact. A duplicate keeps its original `event_id`
(`test-support/simulator/src/emit.rs:88-91`), so it arrives as the same
`message_id`. No conflict here.

**E3. Gate item 3 cannot be measured until I2 and I3 close.** The
`dead-letter` verb is reachable only through `bind-registration`. No external
registration exists.

**E4. There is no `machine_counts` profile.** Two profiles ship, and both
describe WMS traffic (`test-support/simulator/src/profiles.rs:15-20`). A third
profile is demand-gated, and the app that needs one brings it (`:6-7`). App 2
brings it.

E1, E3 and E4 each need a bead. **The integrator must file them.**

## 11. The external ingress contract

This section is the contract that unblocks the simulator's JetStream sink.
`test-support/simulator/src/emit.rs:5-14` holds that sink at trait-only until
app 2 defines this contract.

### The rule in one line

**The simulator produces external facts. It never produces internal facts.**

An external fact is what a machine said. An internal fact is what the platform
observed. The platform derives an internal envelope's identity from a real
write-ahead-log sequence number, or from a host-side digest. The simulator has
neither.

### What the simulator must never produce

- **Nothing on `evt.*`.** Those subjects carry CDC envelopes.
  `verified_source_event_id` accepts exactly one `Nats-Msg-Id`, and it must
  equal a derivation over project, environment and log sequence number
  (`components/events/materializer/src/decide.rs:24-35`,
  `components/events/wire/src/lib.rs:319`). A fabricated envelope carries a
  fabricated sequence number, which asserts a commit that never happened.
  Otherwise it fails the check outright. Neither outcome is honest.
- **No derived event.** `verified_derived_source_event_id` compares tenant,
  project and environment before it derives identity
  (`components/events/materializer/src/decide.rs:41-52`). A foreign scope is
  refused. The local scope manufactures a platform fact.
- **Nothing on `dlq.*`.** The DLQ destination is host-derived and reachable
  only through `bind-registration`
  (`crates/platform/runtime/wit/deps/wamn-jetstream/package.wit:136-140`). Only
  poison that a consumer actually refused belongs there. That is exactly what
  gate item 3 measures. A seeded DLQ row falsifies the gate.
- **Nothing on `tap.*`.** It is host-only
  (`crates/platform/runtime/src/plugins/wamn_jetstream.rs:28-35`).

All three prefixes are reserved. The guest-facing publish verb refuses the
first two by name
(`crates/platform/runtime/wit/deps/wamn-jetstream/package.wit:195-201`).

`tests/integration/src/streambench.rs` looks like a template for a sink and is
not one. It asserts at the JetStream substrate and never passes the
materializer, which is the only reason its synthetic sequence numbers work
(`test-support/simulator/src/emit.rs:12-14`).

### What the simulator can produce

One external ingress message per machine reading. It is published to a subject
inside an ingress namespace. That namespace is none of `evt.*`, `dlq.*` or
`tap.*`. The exact prefix is a binding at deploy, so it is not written here
(`docs/experiments/agent-authoring/protocol.md:187-188`).

Envelope fields. **This is a proposal. There is no external ingress schema in
the tree.**

| field | type | why it is there |
|---|---|---|
| `message_id` | string | the publisher's own id. It is also the `Nats-Msg-Id` header value. |
| `schema_version` | string | PC-7 refuses an unknown value, so a version refusal is a decision and not a guess |
| `line_code` | string | PC-2 |
| `work_order_code` | string | PC-1 |
| `quantity` | integer | PC-3 |
| `observed_at` | RFC 3339 UTC timestamp | PC-6. Machine data, never commit order. |

Headers: exactly one `Nats-Msg-Id`, equal to `message_id`.

### Why the id appears twice

There are two absorption layers, and the envelope needs both.

JetStream deduplicates on `Nats-Msg-Id` inside the stream's duplicate window. A
deduplicated publish is a successful ack carrying `duplicate: true`
(`crates/platform/runtime/wit/deps/wamn-jetstream/package.wit:189-192`).

Outside that window the body's `message_id` becomes the command's
`idempotency_key`, and the claim row absorbs the repeat (PC-4).

A header alone loses the second layer. A body field alone loses the first.

### What the sink must not inherit

`MAX_ENVELOPE_ITEMS` is 100 (`test-support/simulator/src/emit.rs:24-27`). It is
the HTTP array envelope's ceiling, mirroring a guest-side authority for a route
body. It is not a JetStream concern. The sink publishes one message per event.

### What a later lane implements against this contract

- A JetStream target implementing `EmissionTarget`
  (`test-support/simulator/src/emit.rs:46-50`). It publishes one message per
  event to the ingress subject, with the header above.
- Pacing driven from `Profile::rate` (E1).
- A `machine_counts` profile kind emitting the fields above (E4).
- A decision about `ItemOutcome`. The trait returns one outcome per item in
  request order (`test-support/simulator/src/emit.rs:49`). A JetStream publish
  returns a publish ack, not a command result. **What an `ItemOutcome` means
  for an asynchronous ingress is an open design question. No ruling exists.**

## 12. Open questions for the owner

| # | Question | Where |
|---|---|---|
| 1 | ~~Reading A or Reading B for the MQTT seam?~~ | RULED 2026-09-07: reading B, and less than a bridge. Section 5. |
| 2 | ~~What posture does a `wamn:jetstream` registry row carry?~~ | RULED 2026-09-07: no row. Section 5. |
| 3 | Is `count.rollup` a projection, as proposed? | section 4, a design choice with no ruling |
| 4 | Are `WO-000000` and `LINE-00` the code shapes? | section 2, a design choice with no ruling |
| 5 | What does `ItemOutcome` mean for an asynchronous sink? | section 11 |

## 13. Gaps the integrator must file as beads

| id | gap |
|---|---|
| I2 | an external ingress stream, provisioned outside the CDC reader's `EVT_` path (`wamn-7tva.2`) |
| I3 | an external-source registration kind, so `bind-registration` and host DLQ derivation both work (`wamn-7tva.2`) |
| I5 | MQTT enabled on the pinned NATS server, a ConfigMap block and a port |
| E1 | a paced emitter, the first to apply `Profile::rate` |
| E4 | a `machine_counts` simulator profile |
| Q5 | `ItemOutcome` semantics for an asynchronous sink |

One further platform gap is already known and is not app 2's to fix. Authored
SQL has no generic verifier in the authoring loop, so `count.rollup` is proven
at runtime (`docs/experiments/agent-authoring/protocol.md:212`).
