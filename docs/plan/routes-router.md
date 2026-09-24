# Routes, workflows, and the router

Sep 23, 2026

## 1. Problem and decision

Today every operation call goes through the router. The router walks a graph. But every Receiving wiring is one node with no edges (11 of 11). So the router takes one step and returns. The base platform pays for a graph walk it never uses, and the edge cannot reuse the path because the driver is tied to the cloud plugins.

Decision:

- A base application is one compiled component. A route calls one export. No wiring, no router.
- Workflows are a feature layered on top. They use the router. They never sit on a request path.
- One invocation function serves both. The route calls it once. The router calls it per node.

## 2. Rules

1. A graph with no edges is a route, not a wiring. The generator never emits it. Publish refuses it.
2. Workflows never sit on the request path. They start from an event, a schedule, or a manual trigger.
3. A workflow calls application operations. It never touches a component, a table, or SQL directly. Same grant, intent, and never-replay rules as any client.
4. One invocation path. `invoke_operation(component, operation, input) -> Outcome`. Routes, workflow nodes, and the edge all call it.
5. Composition (`wac`) makes one component out of several at build time. A composed component is still one component behind one route. Composition and the router are unrelated.
6. Expression nodes (JSONata, templates, and so on) are platform node components compiled to wasm. The expression is config. It runs in the guest sandbox, never in the host.
7. The base platform and the edge do not link the router.

Every layer exposes an interface and depends only on layers below it: base platform, then run-state store, then workflow crate. Nothing depends on a UI.

## 3. Base application path

```
request -> route (grant, intent) -> invoke_operation -> component export -> outcome -> response
```

- The route record holds `(package, component, operation, terminal)`. The contract already states all four.
- `invoke_operation` does what `invoke_node` does today for one node: pick the admitted component, build a `Store` with the allowed plugins, set the deadline, call the export, lower the outcome. Run-state (write-ahead intent, never replay) attaches here, through a storage trait, for create, update, delete, and command; reads bypass it.
- `InstancePre` is cached by digest. One fresh `Store` per request. Unchanged.
- A composed component (`wac`) enters the same way.
- Nothing in the contract, SQL, generated clients, or the web UI changes.

## 4. Workflow layer

For operator-configured logic: approvals, data shaping for reports, notifications, scheduled jobs. Low-code. Not part of any base application.

```
event | schedule | manual -> router walks wiring -> nodes call invoke_operation -> respond | emit | discard
```

- A wiring is data with edges. Versioned. Swapped by version with no rebuild. `wamn-router` walks it: frontier, ports, error edges, retry, hop budget, terminal verdict. Unchanged.
- Node kinds:
  - **Operation nodes.** Application operations from the contract.
  - **Platform nodes.** A fixed set of small compiled components, configured by the operator: approval (park and resume), map, filter, branch, notify, schedule, expression engines (rule 6).
- Approval waiting uses the run-state park/release surface.
- The workflow feature is a separate crate the cloud host links. It exposes one interface: a workflow contract (operations to start, park, release, list). Any client uses it: \`ctl\`, a generated UI, a test. The feature depends on nothing above it.

## 5. What changes in the code

| Where | Today | After |
| --- | --- | --- |
| `router_driver.rs` (1.8k code) | one driver: graph walk + per-node host work + delivery shapes | epic 1: `invoke_operation` extracted (\~1k lines) into the execution host; epic 2: the walk glue (\~500) moves to the workflow crate |
| `wamn-router` (2k) | linked by the route path | linked by the workflow crate only |
| Route layer (`flow_http_routing.rs`) | route → wiring id → router | route → `invoke_operation` |
| `publish_release.rs` | emits one wiring per operation | emits no wiring for a route; refuses a one-node wiring |
| Delivery preload | loads every wiring | loads routes; wirings only when the workflow feature is on |
| `wamn-runtime` | non-optional cloud plugins (Postgres, NATS, OCI, OTLP) | split: engine + plugin trait crate, cloud plugins crate |
| Receiving publication | 11 wiring files | 0 |

Not changed: contract, SQL, generator, client IR, TS bindings, components, run-state semantics.

Tests: the invocation path gets its tests once, on the platform fixture. Wiring tests move to the workflow crate.

## 6. Edge

The edge links: the engine crate, `invoke_operation`, the SQLite intent log, and the edge plugins (sockets allowlist, serial, blob, MQTT, Modbus, OPC UA). It does not link the router, Postgres, NATS, OCI, or OTLP gRPC.

The edge "app" is a configured subset: a few operations behind local routes and a device loop. Same `invoke_operation`, same never-replay rule, on SQLite. Workflows on the edge are a later question and would link the workflow crate.

## 7. Decisions taken and sequence

Decided during review:

- [x] Does the route record replace `ServingWiring` in the manifest, or sit beside it until the workflow crate exists.

  Decided: replace. `ServingRoute` takes the place of `ServingWiring` in the base manifest in epic 1. Publish emits no one-node wiring. No flag, no old-versus-new comparison. The check is the platform's own tests at the interface: a request to a route returns the same outcome, refusal, and intent record as before. `ServingWiring` moves to the workflow crate in epic 2.
- [x] Queued (async) delivery: does it stay on the base path (a route with `terminal: emit`) or move to the workflow layer.

  Decided: queued delivery moves to the workflow layer. No application uses it today (every attachment is `http`; `Cron` exists as a kind with no user). The base path is synchronous: request in, outcome out. Events an application emits are the trigger surface for workflows, later.
- [x] Where run-state lives after the runtime split: with `invoke_operation`, or its own crate with Postgres and SQLite backends.

  Decided: run-state is its own crate: pure decisions plus a storage trait, with a Postgres adapter (cloud) and a SQLite adapter (edge). `invoke_operation` takes the store as a parameter. A `None` store is allowed and means bypass: no intent record, no never-replay guarantee. Bypass is decided by operation kind: get, query, and list bypass; create, update, delete, and command log. A read has no effect by definition, and publish refuses a read statement that writes. No contract field, no host-wide switch. On the edge the same rule means a constrained box logs only commands.

Sequence, each its own epic, one at a time:

1. Extract `invoke_operation`. Route layer calls it. Run-state attaches by operation kind. Publish emits no one-node wiring. Receiving runs with zero wirings.
   Built by Beads `wamn-7icx`. Two wirings remain for epic 2: the WMS graph `inventory_move_and_label` and the Acme registration entry `quality_create_inspection`.
2. Move the router and walk glue into a workflow crate. Cloud host links it. `Cron` and queued delivery leave the base manifest with it.
   Built by Beads `wamn-xs9a`. `wamn-workflow` holds the driver, wiring delivery, the queue, and the lowering, and `wamn-router` moved under it. Serving manifest format 3 keeps the wiring facts in its `workflow` section. By owner ruling on `wamn-nq1b`, `/inventory/move` is a route, and the unattached WMS label wiring is `wamn-g4kj`.
3. Split `wamn-runtime` into engine and cloud plugins. Run-state becomes its own crate with a storage trait.
   Built by Beads `wamn-3lw7`. `wamn-engine` holds the host and `invoke_operation`, `wamn-runtime` keeps the plugin set, and `wamn-run-state` holds `RunStore` and `IntentStore`. Route intent logging is `wamn-an24`.
4. Edge: engine + `invoke_operation` + SQLite run-state + edge plugins. First target: read a serial scale, store, forward.
5. Workflow feature: the workflow contract (start, park, release, list), triggers (event, schedule, manual call), platform nodes, approvals. Only when a real workflow is needed.
