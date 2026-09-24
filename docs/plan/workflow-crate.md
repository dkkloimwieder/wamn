# Workflow crate

Sep 23, 2026. Epic 2 in [routes, workflows, and the router](routes-router.md), section 7. Beads epic: `wamn-xs9a`.

The owner accepted this scope on 2026-09-23. Section 8 records the rulings.

## 1. Goal

One new crate, `wamn-workflow`, holds everything that walks or delivers a wiring. A wiring is a graph of operation nodes with edges. The crate holds the driver glue, wiring delivery and response, the queue and the enqueue path, Cron, and registrations. It depends on the walk, `wamn-router`, which moves under `crates/execution/workflow/router` and stays its own crate. The workflow crate calls `invoke_operation` for every node and owns no invocation path of its own.

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

The brief put the workflow crate below the host. Section 4 explains why it sits above `wamn-execution-host` and below `services/host`, and owner ruling 1 accepts that.

### The dependency test

`crates/execution/workflow/tests/dependency_boundary.rs` runs `cargo tree -p <crate> -e normal --prefix none --locked --offline`, the same method as the engine test. It asserts four facts:

- `wamn-engine` links no `wamn-workflow`.
- `wamn-runtime` links no `wamn-workflow` and no `wamn-router`.
- `wamn-execution-host` links no `wamn-workflow` and no `wamn-router`. This makes "a route never enters the workflow crate" a checked fact.
- `wamn-host` (services/host) links `wamn-workflow`.

The test lives in the workflow crate, because the engine is outside this epic's files. The engine test already refuses `wamn-router`. The router stays its own crate, so that test keeps its subject with no edit.

## 3. Measured facts

These facts changed the brief. Each one is measured on main at c9bfaf504.

`flow_http_routing.rs` has no wiring dispatch arm. It lists HTTP attachments for the ingress guest and decides nothing about the target. The dispatch is in `RouterDeliveryBridge` in `crates/execution/host/src/router_delivery.rs`. The http-route guest and the materializer guest both call the WIT function `wamn:router-delivery/delivery.deliver`. The bridge then takes the route arm (lines 221-264) or the wiring arm (267-318).

The route path still reaches wiring code in five places. `OperationHost` is built only inside `RouterDriver::new`. The bridge needs an `Arc<RouterDriver>`. `refuse()` downcasts a wiring response type. Route warm-up and readiness go through `prepare_synchronous_release` and `RouterReadinessProbe`. One WIT world serves both targets.

`invoke_operation` is in `wamn-engine`. The driver also uses ten host items from `wamn-execution-host/src/operation.rs`: `OperationHost`, `NativeFacts`, `NodeAcquisition`, `InvocationSite`, the trace and span helpers, `bounded_node_deadline_ms`, `authorize_registered_operation`, `validate_component_in_release`, `load_application`, and `prepare_released`. `route.rs` uses most of the same items.

`wamn-runtime` uses the router in four files, all on the wiring side. `wiring_lowering.rs` lowers a catalog `WiringDocument` into `wamn_router::Wiring`. `wiring_resolution.rs` holds the lowered `Wiring` in `ResolvedActiveWiring`. `production_claim.rs` maps a `wamn_router::Outcome` to a queue action (`ProductionRouterAction`, lines 66-269). The test `port_constant_agreement.rs` compares the router port constants with the contract constants. No base-path item needs to move below the router: the port constants already live in `wamn-execution-contract`.

Four runtime readers use wiring facts as data, with no router type. The JetStream plugin checks `manifest.registrations` for the materializer (`require_registration`). `connection_http.rs` and the blobstore plugin authorize a node inside a wiring. `local_application.rs` validates local wiring facts. These readers stay in the runtime, and they read the new section.

`crates/schema/generator` imports `AttachmentTarget::Wiring`, `ServingAttachment`, and the `wiring.rs` types from `wamn-catalog`. It reads `publication/attachments.json`, which holds the WMS attachment that targets a wiring. This epic does not edit the generator. So the authored attachment type keeps its wiring target, and only the manifest types change.

The generator writes no serving manifest. Publish mints it. So `apps/*/generated/` stays byte-identical, including the format version, which appears only in minted manifests.

The node ABI WIT was at `crates/execution/router/wit/package.wit`. The engine, the generator test, `push_component.rs`, the conformance tests, and 14 component crates name that path. Issue 5 moves it with the router crate (section 4).

Only cluster tests run the two remaining wirings end to end. `apps/wamn_wms/tests/cluster.rs` runs `inventory_move_and_label`, and `apps/wamn_receiving/tests/postcommit.rs` runs the `quality_create_inspection` registration. The WMS local test `local_business.rs` loads no wiring. Section 6 lists the tests that this epic can run without a cluster. Owner ruling 4 decides the proof.

The engine test `crates/platform/engine/src/release_manifest.rs:234-324` builds a manifest with a wiring and asserts `manifest().wirings`. The format 3 change breaks that test. Owner ruling 3 allows the edit.

## 4. Decisions

### Move `wamn-router` under `wamn-workflow`

Options:

1. Merge. The router source becomes the `walk` module of `wamn-workflow`. Its 35 public items become crate-private, except the few that the host service and the tests name.
2. Keep `wamn-router` as its own crate and depend on it. Its 35 public items stay public, and the workflow crate adds its own.

Pick: option 2, moved, not merged (owner ruling of 2026-09-23). The plan first picked the merge, because the owner's rule is the option with fewer public types. A merge puts the walk in a crate that links `wamn-run-state` and `wamn-runtime`. `crates/execution/run-state/tests/shelving_contract.rs` then loses the link rule it checks: the walk links no run plane. So `wamn-router` moves to `crates/execution/workflow/router` and keeps its 35 public items and its tests. Its `lib.rs` names `wamn-workflow` as its only consumer.

The node ABI WIT moves with the crate to `crates/execution/workflow/router/wit`. The same commit updates every reference to the path, the engine and the generator test included, by owner ruling. No symlink.

### Where the workflow crate sits

Options:

1. Below `wamn-execution-host`, as the brief says. The driver needs the ten host items from section 3. So a seam trait in the workflow crate carries them, and `OperationHost` implements it. The trace, span, and deadline helpers either go on the trait or move to the engine, and the engine is outside this epic.
2. Above `wamn-execution-host` and below `services/host`. The ten items become `pub` in the host crate, and the driver moves with only import changes. The bridge in the host crate takes a small trait, `WiringDelivery`, for the wiring arm. The workflow crate implements it, and `services/host` connects the two.

Pick: option 2. It moves code without a new seam over ten items. It also makes the route boundary testable: `cargo tree -p wamn-execution-host` shows no workflow crate. Owner ruling 1 accepts it.

### Manifest shape

Options:

1. A workflow section in `ServingManifest`. The field is `workflow`, omitted when a release has no wiring. It holds `wirings`, the attachments that target a wiring (HTTP, Internal, and Cron), and `registrations`. The section types live in `wamn-catalog` with the rest of the manifest.
2. The same section, but its types live in `wamn-workflow`, and the base manifest carries it as opaque JSON.
3. A second manifest document that the workflow crate loads, with its own digest.

Pick: option 1.

The hash consequence of option 1: the release keeps one digest, the RFC 8785 sha256 of the whole manifest. The section is inside it, so a wiring change still changes the release digest. A release with no wiring omits the key. Its canonical bytes differ from format 2 by the format version and by the removal of the empty `wirings` and `registrations` keys. Every package digest moves once. The table `catalog.release_manifest_v3_snapshots` does not change. The pinned vector `crates/catalog/model/tests/fixtures/release_manifest_mint_vector.rs` gets a format 3 preimage and a new digest.

Option 2 fails because the runtime plugins read registrations and wiring attachments. The runtime must not link the workflow crate, so every such reader needs injected facts, and the manifest decode cannot check the section. Option 3 needs a second snapshot row, a second OCI artifact, a second ConfigMap, and a promote change, with no gain.

The brief says the workflow crate owns the section. Under option 1, the workflow crate owns every behavior of the section: lowering, walk, delivery, queue. `wamn-catalog` owns its types and its structural checks, because every manifest decode runs them. Owner ruling 2 accepts it.

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
| `crates/execution/router`, the WIT included | `crates/execution/workflow/router`, still the crate `wamn-router` |
| host `router_driver.rs` | `wamn-workflow`. `OperationHost` is built outside it. |
| host `router_delivery.rs` wiring arm, `publish_emit`, the `lower_*` functions | `wamn-workflow`, behind `WiringDelivery` |
| host `router_delivery.rs` bridge, route arm, `settle_route`, source checks | stay in `wamn-execution-host` |
| host `router_response.rs`, `queue.rs`, `queue/` | `wamn-workflow` |
| host `readiness.rs` | split: route readiness stays, wiring preload moves |
| runtime `wiring_lowering.rs`, `tests/wiring_lowering.rs`, `tests/port_constant_agreement.rs` | `wamn-workflow` |
| runtime `wiring_resolution.rs` | the SQL, the fetch, and the catalog checks stay. `ResolvedActiveWiring` carries the authored document and its package version. The digest swap and the lowering into `Wiring` move to `lower_resolved_wiring`. |
| runtime `production_claim.rs` router mapping | `wamn-workflow`. The `RunStore` adapter stays. |
| control `enqueue_run.rs` | stays in `wamn-control`. It reads the release snapshot through `publish_release::read_release_snapshot`, so a move would make the workflow crate depend on all of `wamn-control`. |
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
- The walk on real graphs: `tests/integration/src/trusted_http_route.rs` (multi-node graphs through the driver) and `crates/execution/workflow/src/queue/automation_live.rs` (the queue).

The full workspace sweep runs once, in the last issue, with `--no-fail-fast`. Its log is kept outside the checkout, and the close reason names its path (finding `wamn-ijzy`). The sweep compares by test name with the Epic 14 sweep at 46a880bdc: 2469 passed, 0 failed, 52 ignored. Moved tests are mapped by name.

## 7. Issues

Seven issues. They land in the order 1, 2, 4, 3, 5, 6, 7. Issue 4 lands before issue 3, because the lowering can leave the runtime only after its caller, the driver, leaves the host crate. The host crate never links the workflow crate.

1. `wamn-xs9a.1` Create `wamn-workflow`, empty, with the dependency test. `services/host` adds the dependency. The test asserts the four facts in section 2. Until issue 3, `wamn-runtime` links `wamn-router`, and until issue 4, `wamn-execution-host` does. So those two assertions start ignored, and issues 3 and 4 turn them on.
2. `wamn-xs9a.2` Separate the route path from `RouterDriver`, in place in `wamn-execution-host`, before any move. `OperationHost` gets its own constructor. The bridge takes `Arc<OperationHost>` and an optional `WiringDelivery`. Route readiness no longer goes through the wiring preload. `refuse()` names no wiring type. The route and bridge tests pass unchanged.
3. `wamn-xs9a.3` The runtime drops `wamn-router`. `wiring_lowering.rs` and its test, the lowering half of `wiring_resolution.rs`, the router mapping in `production_claim.rs`, and `port_constant_agreement.rs` move to `wamn-workflow`. For now, the workflow crate depends on `wamn-router`.
4. `wamn-xs9a.4` Move the driver, the wiring delivery, the response, and the queue into `wamn-workflow`. The ten host items become `pub`. `wamn-execution-host` drops `wamn-router`. `services/host` builds the driver and hands it to the bridge. `enqueue_run.rs` stays in `wamn-control` (section 5). The host drops its direct `wamn-router` dependency. The host router check turns on in issue 3, because the host still links the router through `wamn-runtime` until then.
5. `wamn-xs9a.5` Move `wamn-router` under `crates/execution/workflow/router`: moved, not merged. A merge puts the walk in a crate that links `wamn-run-state` and `wamn-runtime`, and `shelving_contract.rs` then loses its link rule. One commit is the `git mv` of the whole crate with every path reference, the WIT included. The 35 public items stay.
6. `wamn-xs9a.6` Manifest format 3 and the workflow section. `SERVING_MANIFEST_FORMAT_VERSION` moves to 3, and format 2 refuses. Publish writes the section. Promote, deployment activation, the runtime readers, and the driver read it. The pinned vector gets a new preimage and digest. The frozen manifest literals in about 14 test files change.
7. `wamn-xs9a.7` Regenerate, docs, sweep, closeout. Receiving, WMS, Acme, and the fixture regenerate byte-identical. `docs/architecture/overview.md` and `execution.md` name the workflow crate. The sweep runs with its log kept. The closeout goes on the epic bead, and section 7 of routes-router.md gets one line. This page is then removed.

Root `Cargo.toml` is shared with Epic 13 (`wamn-i0iy.2`). Whichever epic merges second rebases. The conflict is one line.

## 8. Owner rulings

The owner answered the four open questions on 2026-09-23.

1. `wamn-workflow` sits above `wamn-execution-host` and below `services/host`. The route path is in the host crate. The workflow crate calls it, never the reverse. The third dependency fact makes this rule checkable.
2. The section types live in `wamn-catalog`, and the behavior lives in `wamn-workflow`. A plugin that reads registrations as data reads the manifest, not the workflow. The catalog types carry no router type. The lowering to `wamn_router::Wiring` stays in the workflow crate.
3. Issue 6 edits the engine test `release_manifest.rs:234-324`. The test asserts a field that the format removes, so the edit belongs to the format change, not to Epic 13.
4. The tests of section 6 are enough for each issue. One cluster run of the WMS graph and one of the Acme registration are required before the merge, and the closeout records them. If no cluster is up, the epic stops at "ready to merge" and waits.
