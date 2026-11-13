# 79 materializer

Scope at `epic/dep7` (b94833fd): the guest `apps/platform/execution/materializer` (`main.rs` 785, `wit/world.wit`), the decision crate `apps/platform/events/materializer` (`wamn-materializer`, 1,285) and `apps/platform/events/registration` (`wamn-event-reg`, 641). The host side (`wamn_jetstream`, `event_batch.rs`) was reviewed in tab 18; the wire contract in tab 03. Nothing built or run.

## Earlier work checked first

- Tab 18: the three `wamn:jetstream/registration` handlers each repeat plugins, env\_key, materializer check, claim and span; `wamn-runtime` depends on `wamn-materializer`.
- Owner direction: follow wasmCloud's model; the host provides capabilities. Tab 72 2 B: a wasmCloud host with no guest work becomes a native service.
- Tab 30: workflows belong to the environment; event triggers are workflow rows.

## Files read

- `execution/materializer/src/main.rs` (non-test, in full), `wit/world.wit`, `Cargo.toml`.
- `events/materializer`: `lib.rs`, item lists of every file, `workflow.rs` (head and `EventTrigger::matches`), `Cargo.toml`.
- `wit/wamn-jetstream` (both interfaces).

## Callers read

- Repository grep for `wamn_materializer::` and `wamn_event_reg::`; `runtime/plugins/wamn_postgres/event_batch.rs` (`require_event_materializer`, the call to `EventTrigger::matches`).

## One purpose per file

- Guest `main.rs`: every sweep, read triggers from the host, prepare one durable consumer per source package and entity, fetch a batch, check each message's id, hand it to the host, then ack, nack or terminate.
- `wamn-materializer`: decide whether one event matches one trigger (entity, operations, condition, causation depth) and build the workflow input. Used by the host's admission (`event_batch.rs`) through `workflow::EventTrigger::matches`.
- `wamn-event-reg`: trigger predicates shared by workflow authoring and matching.

## Findings

**1. The materializer guest is a loop around three host calls.**

Fact:

- The guest calls `registration::triggers` (the host reads the trigger rows), `registration::prepare` (the host creates the durable consumer) and `registration::admit` (the host matches the event against triggers and enqueues runs, `event_batch.rs`). Between them it fetches with `wasmcloud:nats/jetstream`, checks the `Nats-Msg-Id` against the event's source coordinate, and acks.
- The host checks every call comes from the environment's materializer (`require_event_materializer`, authority class `EventMaterializer`, credential `WAMN_EVENT_MATERIALIZER_PG_URL`).
- Inference: the guest holds no business rule that the host does not already hold; matching and admission run in the host.

Options:

- A: keep the guest. Trim it per finding 2.
- B: the host consumes and admits events natively in the `wamn_jetstream` plugin, as a host capability. The guest, its world, the `wamn:jetstream/registration` interface, the materializer workload, the `EventMaterializer` authority class and its credential go; the id check moves into the plugin.

Recommend B. Inference: one fewer workload per environment and no host interface that exists only for one platform guest.

**2. The guest carries unused imports and test controls (only if finding 1 is A).**

Fact:

- `world.wit` imports `wamn:postgres/types`, `wamn:postgres/client` and `wamn:router-delivery/delivery`; `main.rs` uses none of them (grep). Its header describes delivery "through released wirings" and "the host-owned router bridge"; the code calls `admit`.
- `WAMN_MAT_MAX_SWEEPS` stops the service after N sweeps and `WAMN_MAT_REPORT_PATH` writes a counters file; both serve tests.

Options:

- A: drop the three imports, rewrite the header, delete the two test controls.

Recommend A, if finding 1 is A.

## Follows from earlier rulings

- Tab 18: the repeated handler preamble goes with the handlers under finding 1 B.
- Tab 72 3 A: header citations ("EVT-MAT, D19 v3 §5 / l5i9.17") go.
- Tab 01 and tab 03: `wamn-event-wire` stays the wire contract.

## Fine as is

- `wamn-materializer` matching and `wamn-event-reg` predicates: one owner, shared by authoring and admission.

## Tests deleted, changed or moved

- Finding 1 B: the guest's tests (fake broker and fake registration modules in `main.rs`) go; the `Nats-Msg-Id` check tests move to the plugin; live tests that deploy the materializer workload drive the host instead.
- Finding 2 A: tests that set `max_sweeps` change.

## Not read

- `wamn-materializer` bodies of `decide.rs`, `condition.rs`, `context.rs`; `wamn-event-reg` bodies.
- How the materializer workload is rendered (`environment/runtime.rs`, `ensure.rs::materializer_host_values`).

## Rulings

- 2026-11-12: finding 1 B (the wamn\_jetstream plugin consumes and admits events natively; the guest, its world, wamn:jetstream/registration, the workload, the EventMaterializer class and its credential go). Finding 2 falls away with 1 B.
