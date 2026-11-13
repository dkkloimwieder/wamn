# 31 http-route guest

Scope: `apps/platform/ingress/http-route` (the WASI HTTP guest composed with each application). Nothing built or run.

## Files read

- `src/lib.rs`, `src/guest.rs`, `src/entry.rs`, `src/entry_response.rs` (non-test parts, in full)
- `wit/world.wit`

## Callers read

- `crates/platform/engine/src/flow_http_routing.rs::route_definition` (the route projection, `sealed_operation_id`, `mappings`).
- `crates/platform/engine/src/router_delivery.rs::{settle_route, RouteDelivery}`; `wit/wamn-router-delivery-0.4/package.wit`.
- `crates/execution/host/src/router_delivery.rs::RouterDeliveryBridge` (`deliver_inner`, `run_route`), `host_route.rs::HostRouteDelivery::deliver`, `read_cache.rs` (names only).
- `services/host/src/host.rs` (how the two deliveries are chained).
- `services/edge/src/delivery.rs` (head), `serve.rs` (plugins).
- Route path producers: `wamn_catalog::route_path`, `host_route.rs`, `schema/generator/src/generate/publication.rs`.
- `tests/adversarial.rs` (test names).

## One purpose per file

- `lib.rs`: parse the HTTP request, select the route, map the input, build the HTTP response.
- `guest.rs`: WASI HTTP shell; converts WIT types to `lib.rs` types and back.
- `entry.rs`: the entry path: authority, idempotent writes and reads against the composed application.
- `entry_response.rs`: error bodies of the entry path.

## Findings

**1. After tab 25 B1 the host path carries host routes only.** Fact: the guest calls `wamn:router-delivery/deliver` when a route has no `sealed_operation_id`: host routes (answered by `HostRouteDelivery`) and signed-file routes (passed on to `RouterDeliveryBridge`). Tab 25 B1 moves signed-file routes to the entry path. `HostRouteDelivery` answers only `respond` or a `delivery-error`, with no deadline adjustments and no ETag; it checks `delivery_id` only for being non-empty and ignores `trace` and `if-none-match`. No product code builds `emit`, `discard` or `partially-completed`; `failed` is built only with `terminal`, `retry-exhausted`, `invalid-input`. Host routes live under their own prefix (`host_route_path_prefix()` = `/wamn_control/`). Tab 26 finding 1 already moves the file routes to wash ingress service routes, served natively by the host, as the OAuth callback is. Inference: after B1 no guest request reaches `RouterDeliveryBridge` (the edge has its own delivery; confirm in the edge tab).

- A: narrow `wamn:router-delivery` to host routes: `deliver(attachment, payload, caller) -> result<string, delivery-error>`. Host routes keep going through the guest; file routes do not. Two models for platform HTTP.
- B: host routes become ingress service routes under `/wamn_control/`, served natively by the host, as file routes and the OAuth callback are. One model: the host serves every platform endpoint; the guest serves only application routes on the entry path.
  - Guest: drop the `wamn:router-delivery` import, the `validate-input` call (only the host path uses it), the `wasi:random` import, the whole host-path branch and its types, `partial_response`, `failure_kind_code`, the deadline-adjustments header. Finding 5 goes away.
  - Host: `HostRouteDelivery` becomes a native handler that does its own authentication (shared with the file routes), CSRF check, body limit, in-flight permit and input check. `RouterDeliveryBridge`, `run_route` and `read_cache.rs` go.
  - WIT: `wamn:router-delivery` stays only if the edge needs it (edge tab); `validate-input` leaves `wamn:flow-http-routing`.
  - Open checks, the same as tab 26 finding 1: the route host (the workload's `config.host`), the path prefix, the ingress body-size limit.
- Recommend B (owner asked for one model, 2026-11-11).
- Correction: tab 25 finding 10 put the shared tag code in `wamn-execution-contract`, which tab 01 deletes. With A or B the host copy goes, so the guest's tag code is the only one and nothing moves.

**2. Input mappings and path templates have no producer.** Fact: every product route path is fixed (`route_path` = `{prefix}/{model}/{action}`, host routes the same). No attachment sets `mappings`. So `map_input`, `mapping_values`, `named_values`, `set_pointer`, `Mapping`, `MappingSource`, `Cardinality`, `{name}` and `{*name}` templates and the match scoring do nothing in the product.

- A: delete them. Route selection becomes an exact `(method, path)` lookup. Delete WIT `mapping`, `mapping-source`, `cardinality`, `route-definition.mappings`, and the engine's `input_mapping`.

**3. The route table lives in the host; it belongs in the guest.** (Rewritten 2026-11-11 after the owner's question. Replaces "the host selects".)

Fact, wasmCloud (upstream `examples/oci-registry`): wash ingress selects the workload by host. The component parses its own routes (`src/route.rs`) and checks auth itself (`src/auth.rs`). It asks the host only for what it cannot hold, through a generic capability (`wasmcloud:secrets`). The host has no route table and no auth policy.

Fact, wamn: `FlowHttpRouting` (tab 21) holds the route table and every per-route rule: auth policy, input schema, in-flight counters, cache control, ETag inputs, request-hash rules, read limits, the `signs` flag. The guest asks it at each step by attachment id: `routes`, `authenticate` (the host derives the policy again from the attachment), `validate-input`, `try-acquire`. Routing is release data split across the boundary.

Fact, trust: the platform route layer is one pinned guest (`http_route.wasm.sha256`). The composer generates the dispatch component per application (`component-composer/src/dispatch.rs`, which already derives `is-read` from the exports), and refuses an application member that imports `wamn:application-dispatch` or `wamn:operation` (`validate_application_member`). The caller is a host resource the guest cannot forge, and every entry-path route calls `authorize` or `begin-operation`, where the host checks grants.

Ruled design (A):

- The guest owns the application's route table: path, method, auth modes, signature header and credential name, CSRF need, cache control, ETag inputs, request-hash rule, read limits, `signs`. The composer generates it per release, next to dispatch, from the same manifest.
- The host keeps capabilities only:
  - identity: `authenticate(headers, body, accepted)` checks the credential the route accepts (session, PAT, signature) and returns the caller handle. Credential selection, the CSRF check, PAT grant preload and the session key check stay in the host (tab 17's one authenticator).
  - operation authority: `authorize`, `begin-operation`, `commit`, `rollback`, `finish-read`, `sign-result`.
- `wamn:flow-http-routing` loses `routes`, `validate-input`, `route-definition` and its records. The host loses the projection (`route_definitions`, `route_definition`, `read_cache_control`, `input_mapping`), `InputSchemaValidators` (used only by the host path, gone by finding 1 B) and the route-host check (`bind_route_host`; ingress already selects by host).
- Host endpoints (control routes, file routes, OAuth callback) are served natively by the host and keep their own policy lookup. Each route has one owner: application routes the guest, platform endpoints the host.

Checks before implementation (each comes back with options if it fails):

1. In-flight limit. Today a per-route counter in the host sheds with 429 `route-capacity-exhausted`; the edge uses one box-wide counter. wash-runtime bounds a component by `pool_size` × `max_concurrency` per instance (inference: it waits rather than sheds). Decide: drop per-route shedding for the pool bound, keep a generic host limiter keyed by the guest's route name, or count in the guest (per instance only).
2. Environment inactive. Today `authenticate` checks it first, for anonymous routes too. Anonymous routes would skip identity, so the check moves to `authorize` / `begin-operation`, which every entry-path route calls (inference, from `entry.rs`).
3. Release digest in ETags. ETag inputs carry the manifest digest; the composed entry is named `entries@sha256:<manifest digest>`. If composition cannot embed the digest, the guest gets it through `wasi:config` at bind (a native wash-runtime plugin).
4. Operation deadline. `operation-deadline-ms` comes from the host ceiling (`bounded_node_deadline_ms`, tab 22 finding 4 A). It becomes a composition constant or a `wasi:config` value.
5. Edge. `services/edge` composes the same guest and has its own authenticator (`edge/src/authenticator.rs`); confirm in the edge tab.
6. Composer. How the table is emitted next to dispatch; confirm in the composer tab.

**4. A second set of types mirrors the WIT types.** Fact: `lib.rs` defines `RouteDefinition`, `Header`, `AuthRejection`, `DeliveryRequest`, `DeliveryOutcome`, `DeliveryError` and more; `guest.rs` converts the WIT types one to one. `entry.rs` uses the WIT types directly, and `GuestBackend` keeps a WIT copy of each route beside the converted one. The mirror lets `tests/adversarial.rs` run natively.

- A: `lib.rs` uses the WIT-generated types; delete the mirror and the conversions. `Backend` stays as the test seam. Inference: wit-bindgen types compile on the native target.

**5. Entry or host path is decided four times.** Fact: `authenticate` sets `base_route` as a side effect; `validate_input` and `new_delivery_id` read it; `deliver` checks `sealed_operation_id` again.

- A: decide once from the selected route.

**6. Two query parsers.** Fact: `split_target` parses the query into pairs (used only by mappings); `read_query_request_element` parses it again with `decode_read_query` (tab 01: becomes a plain parser here). Path segments are decoded, re-encoded, then decoded again in `match_path`. The percent code is hand-written.

- A: one parse of the target with the `percent-encoding` crate. With 2 A the pairs parse goes; the read query parser is the only one.

**7. Read limits checked twice.** Fact: on the entry path the guest skips host schema validation and checks `limit` against the schema bounds (`validate_read_limits`); `execute` then checks `limit` again against `CEILING` (50,000) with a different answer shape.

- A: the projection sets `page_maximum` to the smaller of the schema maximum and the ceiling. Delete `limit_refusal` and `CEILING`.

**8. Routes reuse the workflow node context.** Fact: `entry.rs::context` fills `wamn:node` `node-context` with an empty wiring id, version 0, empty node id, occurrence 0, no input port and config `"null"`. Only delivery id, trace and deadline mean anything for a route.

- A: `wamn:application-dispatch` takes a route context record (delivery id, trace, deadline).

**9. Six builders of the error envelope.** Fact: `error_response`, `detailed_error_response`, `operation_refusal_response`, `body_too_large_response` (plain text), `entry_response::refusal`, and inline `json!` bodies in `entry.rs` (`timeout`, `internal_error`, `commit_failure`) build `{"error": {...}}` with different members.

- A: one builder `error(code, message, detail)`; the 413 answer uses it too.

## Already ruled, guest side

- Tab 21 finding 3 A: delete `RouteDefinition.host`, `mapped_limit`, `AdapterLimits.mapped_bytes` and the `mapped-payload-too-large` check.
- Tab 01: read query strictness dropped; the plain query parser lives here. Request hashing follows tab 01 option C.
- Tab 01: `canonical_numeric`, `canonical_timestamptz` come from `wamn-scalar`; `canonical_json_sha256` from `wamn-digest`.

## Tests deleted, changed or moved

- Delete (finding 1): `tests/adversarial.rs::every_router_outcome_is_mapped_exhaustively`, `partial_completion_preserves_only_committed_result_and_existing_failure`, `partial_completion_keeps_nested_authorization_and_cancellation_truth`, `deadline_adjustments_reach_success_and_error_responses`, `identical_requests_receive_distinct_delivery_ids`; host `read_cache.rs` tests and the bridge tests in `router_delivery.rs`.
- Delete (finding 2): `route_precedence_is_static_then_parameter_then_catch_all`, `encoded_slash_stays_in_one_path_segment_and_maps_decoded`; the mapping parts of `partial_body_selected_attachment_mapping_and_delivery` and `malformed_oversize_mapping_schema_and_auth_refusals_never_deliver`.
- Change (finding 4): every test in `tests/adversarial.rs` builds WIT types.
- Change (findings 7, 9): tests that read the limit or error bodies.

## Not read

- `tests/adversarial.rs` bodies (test lists on hold for detail).
- `services/edge` beyond `delivery.rs` head and `serve.rs` plugins (edge tab).
- `wit/wamn-flow-http-routing/package.wit` in full.

## Rulings

- Finding 1: B, pending the open checks (route host, path prefix, ingress body-size limit). Host routes become ingress service routes under `/wamn_control/`, like file routes and the OAuth callback.
- Finding 3 (revised 2026-11-11): A. The application route table lives in the guest, generated by the composer per release; the host keeps capabilities only (identity, operation authority). Six checks listed under the finding.
- Finding 4 changes with 3: route records no longer cross the WIT; the rule "use WIT types, no mirror" applies to the records that remain (caller, rejection).
- Finding 6 (revised 2026-11-11): B. The path is compared raw; the read query is parsed with `form_urlencoded`. The hand-written percent code goes.
- Finding 7 changes with 3: read limits come from the guest table, computed at composition (smaller of schema maximum and ceiling).
- Findings 2, 8, 9: A. Finding 5 goes away with 1 B and 3 A.

Cross-reference (tab 38, 2026-11-11): finding 3 A stands, with one change from tab 38 finding 2 C. The route table is its own component, not composed with the operations. It hands JSON to the host through `wamn:operation`, and the host converts it and calls the operation's typed `run` in the application component. This avoids re-entering one instance. Alternatives are recorded in tab 38.

## Check results (2026-11-11)

Source: wash-runtime at the pinned tag (v2.11.0), and wamn code.

- **Finding 1 (route host, path prefix, body limit):** resolved in tab 26. The same answers apply to `/wamn_control/`.
- **Finding 3, check 1 (in-flight limit).** Fact: the instance pool does not bound or queue calls. When every pooled instance is busy, `instance_pool.rs::dispatch_on_new` declines and the call runs in a one-shot store. The ingress bounds connections (`max_connections`) and closes new ones at the ceiling. Inference: a counter in the guest counts per instance only, and overflow calls run in other stores, so it cannot bound a route. Options:
  - A: drop per-route shedding. The ingress connection ceiling, the operation deadline and the database pool bound the load. Delete `RouteLimiter` and `route-capacity-exhausted`.
  - B: one limiter in the host's operation invoke (tab 38 C puts every call there), keyed by operation, with the limit from the route table; it answers 429.
  - Recommend A (native mechanisms, deletion). Needs a ruling.
- **Check 2 (environment inactive).** Fact (`entry.rs::execute`): every read calls `authorize`, every write element calls `begin_operation` before it runs. Under tab 38 C every call also passes the host's invoke. Resolved: the check sits once in the host invoke path.
- **Checks 3 and 4 (release digest, deadline), with check 6 (composer).** Resolved: the route component stays one pinned binary (`http_route.wasm.sha256`). The route table, the release digest and the operation deadline reach it as `wasi:config` values (native wash-runtime plugin). The composer writes data, not code, and no per-release build is needed. Alternative: embed them at compose time (a per-release build of the route component).
- **Check 5 (edge):** waits for the edge tab.
- **Check 6 (composer):** the data shape waits for the composer tab.

Ruling (2026-11-11), finding 3 check 1: A. Per-route shedding is dropped. The ingress connection ceiling, the operation deadline and the database pool bound the load. `RouteLimiter` and `route-capacity-exhausted` are deleted.

Cross-reference (tab 39 finding 1 A, 2026-11-11): `InputSchemaValidators` is kept, not deleted. It moves from the host route path to the host's operation invoke path and checks every JSON input against the published input schema.

Cross-reference withdrawn (2026-11-11): tab 39 finding 1 changed to B. `InputSchemaValidators` is deleted as finding 3 ruled; input checks run in the generated operation function in the guest.
