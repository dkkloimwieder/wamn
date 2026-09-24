# Workflow crate

Sep 23, 2026. Epic 2 in [routes, workflows, and the router](routes-router.md), section 7. Beads epic: `wamn-xs9a`.

This page is a scope for owner review. No code starts before the owner accepts it. Section 8 lists the open questions.

## 1. Goal

One new crate, `wamn-workflow`, holds everything that walks or delivers a wiring. A wiring is a graph of operation nodes with edges. The crate holds the walk (`wamn-router`, merged in), the driver glue, wiring delivery and response, the queue and the enqueue path, Cron, and registrations. The workflow crate calls `invoke_operation` for every node and owns no invocation path of its own.

A route never enters the workflow crate. A wiring with edges, a registration, a Cron attachment, and a queued delivery enter only through it.

The wiring facts leave the top level of the serving manifest and go into one workflow section. The manifest format moves to 3, and publish refuses format 2.

This epic moves code. It does not build the workflow contract of routes-router section 4 (start, park, release, list). That is Epic 5.

## 2. Layers

A layer depends only on the layers below it. The order below is measured from the `Cargo.toml` files on main at c9bfaf504. Run-state sits below the engine, by Epic 3 owner ruling 2.

1. `wamn-run-state`: pure decisions and the storage traits.
2. `wamn-engine`: the host, admission, artifacts, and `invoke_operation`.
3. `wamn-runtime`: the plugin set.
4. `wamn-execution-host`: the route path, `OperationHost`, and the delivery bridge.
5. `wamn-workflow`: the walk, the driver, wiring delivery, the queue, enqueue.
6. `services/host`, `services/ctl`, and `wamn-control`: they link what they call.

The brief put the workflow crate below the host. Section 4 explains why this scope puts it above `wamn-execution-host` and below `services/host`. Section 8 asks the owner.

### The dependency test

`crates/execution/workflow/tests/dependency_boundary.rs` runs `cargo tree -p <crate> -e normal --prefix none --locked --offline`, the same method as the engine test. It asserts four facts:

- `wamn-engine` links no `wamn-workflow`.
- `wamn-runtime` links no `wamn-workflow` and no `wamn-router`.
- `wamn-execution-host` links no `wamn-workflow` and no `wamn-router`. This makes "a route never enters the workflow crate" a checked fact.
- `wamn-host` (services/host) links `wamn-workflow`.

The test lives in the workflow crate, because the engine is outside this epic's files. The engine test already refuses `wamn-router`. After the merge, that name no longer exists, and the engine test stays true with no edit.

## 3. Measured facts

These facts changed the brief. Each one is measured on main at c9bfaf504.

`flow_http_routing.rs` has no wiring dispatch arm. It lists HTTP attachments for the ingress guest and decides nothing about the target. The dispatch is in `RouterDeliveryBridge` in `crates/execution/host/src/router_delivery.rs`. The http-route guest and the materializer guest both call the WIT function `wamn:router-delivery/delivery.deliver`. The bridge then takes the route arm (lines 221-264) or the wiring arm (267-318).

The route path still reaches wiring code in five places. `OperationHost` is built only inside `RouterDriver::new`. The bridge needs an `Arc<RouterDriver>`. `refuse()` downcasts a wiring response type. Route warm-up and readiness go through `prepare_synchronous_release` and `RouterReadinessProbe`. One WIT world serves both targets.

`invoke_operation` is in `wamn-engine`. The driver also uses ten host items from `wamn-execution-host/src/operation.rs`: `OperationHost`, `NativeFacts`, `NodeAcquisition`, `InvocationSite`, the trace and span helpers, `bounded_node_deadline_ms`, `authorize_registered_operation`, `validate_component_in_release`, `load_application`, and `prepare_released`. `route.rs` uses most of the same items.

`wamn-runtime` uses the router in four files, all on the wiring side. `wiring_lowering.rs` lowers a catalog `WiringDocument` into `wamn_router::Wiring`. `wiring_resolution.rs` holds the lowered `Wiring` in `ResolvedActiveWiring`. `production_claim.rs` maps a `wamn_router::Outcome` to a queue action (`ProductionRouterAction`, lines 66-269). The test `port_constant_agreement.rs` compares the router port constants with the contract constants. No base-path item needs to move below the router: the port constants already live in `wamn-execution-contract`.

Four runtime readers use wiring facts as data, with no router type. The JetStream plugin checks `manifest.registrations` for the materializer (`require_registration`). `connection_http.rs` and the blobstore plugin authorize a node inside a wiring. `local_application.rs` validates local wiring facts. These readers stay in the runtime, and they read the new section.

`crates/schema/generator` imports `AttachmentTarget::Wiring`, `ServingAttachment`, and the `wiring.rs` types from `wamn-catalog`. It reads `publication/attachments.json`, which holds the WMS attachment that targets a wiring. This epic does not edit the generator. So the authored attachment type keeps its wiring target, and only the manifest types change.

The generator writes no serving manifest. Publish mints it. So `apps/*/generated/` stays byte-identical, including the format version, which appears only in minted manifests.

The node ABI WIT is at `crates/execution/router/wit/package.wit`. The engine, the generator test, `push_component.rs`, the conformance tests, and 14 component crates name that path. This epic does not move it (section 4).

Only cluster tests run the two remaining wirings end to end. `apps/wamn_wms/tests/cluster.rs` runs `inventory_move_and_label`, and `apps/wamn_receiving/tests/postcommit.rs` runs the `quality_create_inspection` registration. The WMS local test `local_business.rs` loads no wiring. Section 6 lists the tests that this epic can run without a cluster. Section 8 asks the owner.

The engine test `crates/platform/engine/src/release_manifest.rs:234-324` builds a manifest with a wiring and asserts `manifest().wirings`. The format 3 change breaks that test. Section 8 asks the owner.

## 4. Decisions

### Merge `wamn-router` into `wamn-workflow`

Options:

1. Merge. The router source becomes the `walk` module of `wamn-workflow`. Its 35 public items become crate-private, except the few that the host service and the tests name.
2. Keep `wamn-router` as its own crate and depend on it. Its 35 public items stay public, and the workflow crate adds its own.

Pick: merge. The owner's rule is the option with fewer public types. After this epic, only the workflow crate uses the router types. The walk tests (`tests/walk.rs`, `resolution.rs`, `route.rs`, `terminal.rs`) move into the crate as unit tests, so the walk API does not need to stay public for them. `docs/testing/deterministic.md` links move with them.

The node ABI WIT stays at `crates/execution/router/wit`. After the merge, that directory holds only the WIT. Moving it needs edits in the generator and the engine, which are outside this epic. The follow-up `wamn-2bgm` moves it to its owner.

### Where the workflow crate sits

Options:

1. Below `wamn-execution-host`, as the brief says. The driver needs the ten host items from section 3. So a seam trait in the workflow crate carries them, and `OperationHost` implements it. The trace, span, and deadline helpers either go on the trait or move to the engine, and the engine is outside this epic.
2. Above `wamn-execution-host` and below `services/host`. The ten items become `pub` in the host crate, and the driver moves with only import changes. The bridge in the host crate takes a small trait, `WiringDelivery`, for the wiring arm. The workflow crate implements it, and `services/host` connects the two.

Pick: option 2. It moves code without a new seam over ten items. It also makes the route boundary testable: `cargo tree -p wamn-execution-host` shows no workflow crate. The owner decides (section 8, question 1).

### Manifest shape

Options:

1. A workflow section in `ServingManifest`. The field is `workflow`, omitted when a release has no wiring. It holds `wirings`, the attachments that target a wiring (HTTP, Internal, and Cron), and `registrations`. The section types live in `wamn-catalog` with the rest of the manifest.
2. The same section, but its types live in `wamn-workflow`, and the base manifest carries it as opaque JSON.
3. A second manifest document that the workflow crate loads, with its own digest.

Pick: option 1.

The hash consequence of option 1: the release keeps one digest, the RFC 8785 sha256 of the whole manifest. The section is inside it, so a wiring change still changes the release digest. A release with no wiring omits the key. Its canonical bytes differ from format 2 by the format version and by the removal of the empty `wirings` and `registrations` keys. Every package digest moves once. The table `catalog.release_manifest_v3_snapshots` does not change. The pinned vector `crates/catalog/model/tests/fixtures/release_manifest_mint_vector.rs` gets a format 3 preimage and a new digest.

Option 2 fails because the runtime plugins read registrations and wiring attachments. The runtime must not link the workflow crate, so every such reader needs injected facts, and the manifest decode cannot check the section. Option 3 needs a second snapshot row, a second OCI artifact, a second ConfigMap, and a promote change, with no gain.

The brief says the workflow crate owns the section. Under option 1, the workflow crate owns every behavior of the section: lowering, walk, delivery, queue. `wamn-catalog` owns its types and its structural checks, because every manifest decode runs them. Section 8, question 2 asks the owner.

The top-level manifest types change as follows:

- `ServingManifest` loses `wirings` and `registrations`, and gains `workflow: Option<WorkflowSection>`.
- `ServingManifest.attachments` holds route attachments only. A new manifest type with no wiring target replaces `ServingAttachment` there.
- `ServingAttachment` and `AttachmentTarget` stay unchanged as the authored input type, because the generator reads them. Publish splits them into the two manifest places.
- The Cron attachment kind exists only in the section.

### Where the queue's `RunStore` call moves

`RunStore` is a trait in `wamn-run-state`, and `WamnPostgres` in `wamn-runtime` implements it. Both sit below the workflow crate.

Pick: `queue.rs` moves to `wamn-workflow` and keeps calling the trait. The Postgres adapter stays in the runtime with the other Postgres code. The router mapping in `production_claim.rs` (`ProductionRouterAction`, `production_router_action`, `production_router_result_action`, `persisted_router_failure`, `router_failure_code`) moves to the workflow crate, because it names `wamn_router::Outcome`. `wamn-run-state` does not change.

The other option moves the adapter into the workflow crate. That splits `WamnPostgres` across two crates for no gain.

### HTTP attachments that target a wiring

The brief asked whether `flow_http_routing` keeps a wiring dispatch arm or the workflow crate registers its own HTTP handler. Measured, it has no dispatch arm (section 3).

Options:

1. `flow_http_routing` lists the HTTP attachments in the workflow section as data, beside the route attachments. The bridge in `wamn-execution-host` sends a route attachment to `invoke_route`. It sends a section attachment or a registration to `WiringDelivery`.
2. The workflow crate registers its own HTTP handler and a second WIT delivery interface. The http-route guest then needs a change, which changes its digest.

Pick: option 1. No guest changes. The ingress guest calls `deliver` as today.

The WMS graph `inventory_move_and_label` sits behind the HTTP route `/inventory/move`. That is a workflow on a request path, which routes-router rule 2 forbids. Epic 1 kept it by owner ruling, and this epic changes no behavior. A later epic decides its future.

## 5. What goes where

| Today | After |
| --- | --- |
| `crates/execution/router` source and tests | `wamn-workflow`, `walk` module. The WIT stays. |
| host `router_driver.rs` | `wamn-workflow`. `OperationHost` is built outside it. |
| host `router_delivery.rs` wiring arm, `publish_emit`, the `lower_*` functions | `wamn-workflow`, behind `WiringDelivery` |
| host `router_delivery.rs` bridge, route arm, `settle_route`, source checks | stay in `wamn-execution-host` |
| host `router_response.rs`, `queue.rs`, `queue/` | `wamn-workflow` |
| host `readiness.rs` | split: route readiness stays, wiring preload moves |
| runtime `wiring_lowering.rs`, `tests/wiring_lowering.rs`, `tests/port_constant_agreement.rs` | `wamn-workflow` |
| runtime `wiring_resolution.rs` | the SQL and the fetch stay. The lowering into `Wiring` moves. |
| runtime `production_claim.rs` router mapping | `wamn-workflow`. The `RunStore` adapter stays. |
| control `enqueue_run.rs` | `wamn-workflow`. `wamn-ctl` links it. |
| catalog `ServingWiring`, `ServingRegistration`, wiring attachments, Cron | the manifest workflow section |

These stay where they are, because they read wiring data and walk nothing:

- the plugin closure checks in `connection_http.rs`, `wamn_blobstore`, and `wamn_postgres/claims.rs`
- the JetStream tap, emit publish, and registration check
- the authoring code: Gate, scenario-worker candidate admission, `author_wiring.rs`, and the dev coordinator
- `wiring.rs`, `wiring_compatibility.rs`, and `wiring_activation.rs` in `wamn-catalog`, because the generator and control read them
- publish, promote, and catalog tables in `wamn-control`. They write the section.

## 6. Tests

Each issue ends with the workspace build, clippy, and fmt clean, and the tests of the crates it touches passing. A move commit is a `git mv` with no content change. Import and path fixes go in the next commit.

These tests run without a cluster and cover the two wirings:

- WMS: `apps/wamn_wms/tests/wms_publication.rs` and `wms_wiring_shape.rs`.
- Acme: `apps/client_acme_receiving/tests/acme_overlay_publication.rs`, including `receipt_insert_registration_selects_one_private_owner_wiring`.
- The walk on real graphs: `tests/integration/src/trusted_http_route.rs` (multi-node graphs through the driver) and `crates/execution/host/src/queue/automation_live.rs` (the queue).

The full workspace sweep runs once, in the last issue, with `--no-fail-fast`. Its log is kept outside the checkout, and the close reason names its path (finding `wamn-ijzy`). The sweep compares by test name with the Epic 14 sweep at 46a880bdc: 2469 passed, 0 failed, 52 ignored. Moved tests are mapped by name.

## 7. Issues

Seven issues, in order. Each depends on the one before it.

1. `wamn-xs9a.1` Create `wamn-workflow`, empty, with the dependency test. `services/host` adds the dependency. The test asserts the four facts in section 2. Before issue 4, `wamn-runtime` and `wamn-execution-host` still link `wamn-router`, so those two assertions start as `#[ignore = "turns on in wamn-xs9a.4"]`, and issue 4 turns them on.
2. `wamn-xs9a.2` Separate the route path from `RouterDriver`, in place in `wamn-execution-host`, before any move. `OperationHost` gets its own constructor. The bridge takes `Arc<OperationHost>` and an optional `WiringDelivery`. Route readiness no longer goes through the wiring preload. `refuse()` names no wiring type. The route and bridge tests pass unchanged.
3. `wamn-xs9a.3` The runtime drops `wamn-router`. `wiring_lowering.rs` and its test, the lowering half of `wiring_resolution.rs`, the router mapping in `production_claim.rs`, and `port_constant_agreement.rs` move to `wamn-workflow`. For now, the workflow crate depends on `wamn-router`.
4. `wamn-xs9a.4` Move the driver, the wiring delivery, the response, and the queue into `wamn-workflow`. The ten host items become `pub`. `wamn-execution-host` drops `wamn-router`. `services/host` builds the driver and hands it to the bridge. `enqueue_run.rs` moves from `wamn-control`. The two ignored dependency assertions turn on.
5. `wamn-xs9a.5` Merge `wamn-router` into `wamn-workflow`. First commit: `git mv` of the source and tests. Second commit: paths, visibility, and doc links. The crate leaves the workspace. The WIT stays.
6. `wamn-xs9a.6` Manifest format 3 and the workflow section. `SERVING_MANIFEST_FORMAT_VERSION` moves to 3, and format 2 refuses. Publish writes the section. Promote, deployment activation, the runtime readers, and the driver read it. The pinned vector gets a new preimage and digest. The frozen manifest literals in about 14 test files change.
7. `wamn-xs9a.7` Regenerate, docs, sweep, closeout. Receiving, WMS, Acme, and the fixture regenerate byte-identical. `docs/architecture/overview.md` and `execution.md` name the workflow crate. The sweep runs with its log kept. The closeout goes on the epic bead, and section 7 of routes-router.md gets one line. This page is then removed.

Root `Cargo.toml` is shared with Epic 13 (`wamn-i0iy.2`). Whichever epic merges second rebases. The conflict is one line.

## 8. Open questions for the owner

1. May `wamn-workflow` sit above `wamn-execution-host` and below `services/host` (section 4)? The brief put it below the host. The pick avoids a seam over ten host items and makes the route boundary a `cargo tree` check.
2. May the section types live in `wamn-catalog`, with every behavior in `wamn-workflow` (section 4)? The runtime plugins read registrations, and the runtime must not link the workflow crate.
3. Issue 6 breaks one engine test, `crates/platform/engine/src/release_manifest.rs:234-324`, which builds a manifest with a wiring. May issue 6 edit that test? It is not a file that Epic 13 changes.
4. Only cluster tests run the two wirings end to end. Is the closeout proof the tests in section 6, or do you want one WMS and one Receiving cluster run?
