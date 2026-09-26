# Workflow feature

This page scopes the workflow feature, item 5 of [routes, workflows, and the router](routes-router.md). The owner named the parts on 2026-09-25. Beads holds the epic, its issues, and their status.

## 1. Goal

A workflow is a wiring that runs off the request path. After this epic, an application event starts a registered workflow. The workflow runs through the durable queue, and an operator or a test can start, park, release, and list workflow runs through one interface.

The first real workflow is the WMS label graph. A pallet move commits through the `/inventory/move` route. The move's row event then starts the graph, which renders the pallet label and stores it in the blob store.

## 2. Fixed rules

The owner set these rules in the brief, and routes-router section 2 sets the rest.

- The workflow crate `wamn-workflow` exposes the workflow contract: start, park, release, and list. Any client uses it, for example `wamn-ctl-ops`, a test, or a later UI.
- An application event starts a wiring. The workflow never sits on a request path.
- One platform node is built: a JSONata expression node compiled to wasm. The expression is configuration, and it runs in the guest sandbox.
- The WMS `inventory_move_and_label` graph runs as a registered workflow, off the request path.
- Approvals and an admin UI are out of scope. Schedule triggers (`Cron`) stay out of scope.
- A workflow calls operations through `invoke_operation`, with the same grant, intent, and never-replay rules as any client.
- The base platform and the edge do not link the workflow crate. The dependency tests keep this rule.

## 3. Current state

Measured on main at `bceff31ad` on 2026-09-25.

| Place | Today |
| --- | --- |
| `wamn-workflow` | Holds the driver (`RouterDriver`), wiring delivery, the queue (`QueueService`), and the lowering. It exposes no workflow contract. `services/host` links it. |
| Queued runs | `wamn_control::enqueue_run::enqueue` writes one `runs` row and one `run_queue` row under a service principal. The `wamn-ctl enqueue-run` verb is its only caller. `QueueService` claims due rows and runs each wiring through the driver. |
| Park | The queue has a park: a row whose `available_at` is in the future is not claimable (`run_state::queue::claim`). Backoff uses it. The run-level status `parked` was retired, and a test makes sure that the server refuses it. No verb parks or releases a run. |
| List | No interface lists workflow runs. |
| Registrations | An operation of kind `event_handler` declares a `registration` in `wamn.json`. Publish derives a `ServingRegistration` only for the one wiring whose entry node is that handler. Apply-package writes the matching row in `catalog.event_registrations`. |
| Event delivery | The CDC reader publishes row events to JetStream. The materializer guest reads `catalog.event_registrations`, checks each condition, and calls the delivery bridge. The driver walks the wiring inline. It writes no `runs` row, so no interface can list or park that work. |
| Platform nodes | `transform`, `label-render`, and `label-template` are `no_std` guests in `apps/platform/no-std`. `blob-put` is a std guest in `apps/platform/execution`. No expression node exists. |
| JSONata | No crate is in the lockfiles. The registry cache holds `jsonata-core` 2.2.7 and `jsonata-rs` 0.3.4. Both are std crates. |
| WMS graph | `inventory_move_and_label` walks `inventory.move`, then `label-render`, then `blob-put`, with a `respond` terminal. No attachment names it (`wamn-g4kj`). The WMS cluster label cases still expect the label on the move response. |

## 4. Design

### 4.1 The workflow contract

`wamn_workflow::contract` defines one trait, `Workflows`, with four methods.

| Method | Effect |
| --- | --- |
| `start` | Admits one run of a released wiring. It writes the `runs` row and the `run_queue` row in one transaction. An idempotency key makes a repeated start return the first run. |
| `park` | Holds a queued run. The run's `available_at` becomes `infinity`, so no claim takes it. A running or finished run is refused. |
| `release` | Returns a parked run to the queue. Its `available_at` becomes `now()`. A run that is not parked is refused. |
| `list` | Returns the runs of one tenant and environment, newest first, with a limit. Each row gives the run id, the wiring, the trigger, the status, and whether the run is parked. |

Park acts on the queue row only. The run status stays `dispatched`, so the retired run-level `parked` status does not come back. A park inside a walk needs a saved walk and is part of approvals, which are out of scope.

The Postgres implementation `PostgresWorkflows` lives in the workflow crate. The SQL text lives in `wamn-run-state` beside the claim SQL. The admission that `enqueue` does today moves into `start`. The verbs `wamn-ctl workflow start`, `park`, `release`, and `list` call the contract, and `workflow start` replaces `enqueue-run`.

### 4.2 The event trigger

An application declares a workflow in `wamn.json`. The declaration names a wiring and the event that starts it:

```json
"workflows": {
  "movement_label": {
    "wiring": "inventory_move_and_label",
    "registration": { "source_package": "wamn_wms", "entity": "inventory_movement", "ops": ["insert"] }
  }
}
```

The wiring can enter at any node. Apply-package writes the registration row, as it does for an event handler. Publish derives a `ServingRegistration` that targets the named wiring, and it marks the registration as a workflow start. An existing release keeps its bytes, because the new field is left out when it is not set.

The materializer and the bridge do not change. When the driver receives a delivery for a workflow registration, it calls `Workflows::start` and does not walk the wiring. The delivery id `<registration>:event:<stream_seq>:<event id>` is the idempotency key, so a redelivered event starts one run only. The run records `trigger_source = 'event'` and the registration id. The queue then runs the wiring like any queued run, as the platform executor.

### 4.3 The JSONata node

The node is a std guest at `apps/platform/execution/jsonata`. It exports `wamn:node/handler@0.1.0`, like `label-render`. Its one parameter, `expression`, holds the JSONata text. The node evaluates the expression on the input and emits the result. An input that is not JSON is an `invalid-input` error. An expression that does not parse, or an evaluation that fails, is a `terminal` error, because a retry gives the same result.

The first issue picks the crate by measurement. The crate must build for `wasm32-wasip2`, its component must pass admission after the release stage virtualizes it, and it must pass the expression cases that the WMS graph uses.

### 4.4 The WMS label workflow

The graph keeps its id `inventory_move_and_label`. The move node leaves the graph, because the move is the route that commits the row that starts the graph.

```text
inventory_movement insert -> shape (jsonata) -> label (label-render) -> store (blob-put)
```

The `shape` node turns the row event into the item array that `label-render` takes, with `movement_id`, `pallet_id`, and `location_id`. A movement that is not a move gives an empty array. The graph has no `respond` terminal, because no caller waits. The WMS cluster cases read the label from the blob store after the run completes. They no longer read it from the move response.

## 5. Issues

Each issue lands with its tests. The order follows the dependencies.

1. The workflow contract and `PostgresWorkflows`, with the `workflow` verbs. A live test on a disposable database starts a run twice, parks it, makes sure that a claim skips it, releases it, runs it, and lists each state.
2. The JSONata node, with the crate choice, the declaration, unit cases, and one engine call of the admitted component.
3. The event trigger: the `workflows` declaration, apply-package, publish, and the driver's start. Unit tests cover the derivation and a repeated delivery.
4. The WMS label workflow: the new graph, its declaration, the shape tests, and the cluster and terminal cases. This closes `wamn-g4kj`.
5. The end-to-end run on a disposable WMS kind cluster, and the documentation of current behavior. A move through the route leaves one stored label, and `workflow list` shows one completed run.

## 6. Out of scope

- Approvals, and a park inside a walk.
- An admin UI for workflows.
- Schedule triggers. `Cron` stays a kind with no scheduler.
- Workflows on the edge.
- The dev loop gate for an application whose wiring names a platform component (`wamn-hw3n`).
