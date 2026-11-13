# 21 wamn-engine, part 1: route plugin

Nothing was built or run. Every statement is from reading the code. **Fact** = read in the code. **Inference** = my conclusion.

`wamn-engine` is 12,607 lines. It is read in four parts. This is part 1.

## Files read

All under `crates/platform/engine/src/`.

| File | Lines | Non-test lines |
| --- | --- | --- |
| `flow_http_routing.rs` | 2,687 | 1,545 |
| `invocation_trace.rs` | 212 | 212 |
| `lib.rs` | 94 | 94 |
| `entry_operation.rs` | 32 | 32 |
| `route_bindings.rs` | 22 | 22 |
| Total | 3,047 | 1,905 |

Callers read: `services/host/src/host.rs` and `services/edge/src/serve.rs` (construction), `services/edge/src/authenticator.rs`, `runtime/src/plugins/route_authentication.rs`, `crates/execution/host/src/file_route.rs` (native permit and authenticate), `native_policy.rs` (trace bind), the guest `apps/platform/ingress/http-route/src` (every use of a `route-definition` field), `wit/wamn-flow-http-routing/package.wit`.

## One purpose each

| File | Purpose | Verdict |
| --- | --- | --- |
| `flow_http_routing.rs` | Route plugin of the http-route guest | Four things in one file |
| `invocation_trace.rs` | Carry the trace context into host callbacks | One purpose. One more per-scope map |
| `route_bindings.rs`, `entry_operation.rs` | Generated bindings | One purpose |

## Findings

### 1. One file holds four things

- **Fact.** `flow_http_routing.rs` holds: the in-flight limiter; input schema compile and check; the projection from manifest to `RouteDefinition`; the caller type, credential selection, CSRF check and refusals. Plus the plugin and WIT handlers.
- **Option.** Split by purpose into four modules under `flow_http_routing/`: `limiter.rs`, `input_schema.rs`, `projection.rs`, `credentials.rs`. The plugin and handlers stay in the parent file. No behaviour change.

### 2. The route projection is rebuilt on every request

- **Fact.** Each `routes` call walks every attachment of the release and builds a complete `RouteDefinition` for each (strings, mapping lists, hash rules), then filters by method.
- **Fact.** The manifest never changes in a process. The input schemas are already compiled once at construction.
- **Fact.** `InputSchemaValidators::new`, `requires_pat_route_authentication` and `requires_session_route_authentication` each call `route_definition` again only to ask "is this attachment serviceable".
- **Fact.** `carries_route` (used by both permit paths) accepts an attachment of the right type even when `route_definition` returns none. `routes` skips the same attachment. Two answers to one question.
- **Option.** Build the definitions once at construction. `routes` clones only the matches. The three "serviceable" askers and `carries_route` read that one projection. Follows the repository rule that memory comes before CPU.

### 3. Three fields cross the WIT boundary with a constant value

- **Fact.** `host` is always the empty string. The comment says the WIT field stays. The guest copies it and never matches on it.
- **Fact.** `mapped-limit` is always `u32::MAX`. The guest takes the smaller of it and its own limit, so its own limit always wins.
- **Fact.** `operation-deadline-ms` is `bounded_node_deadline_ms(None)`, the same host constant for every route.
- **Option A (recommended, greenfield).** Delete `host` and `mapped-limit` from `wamn:flow-http-routing`. The WIT version changes; the guest and its tests change. `operation-deadline-ms` is decided in part 2, with `router_delivery.rs`.
- **Option B.** Keep the fields.

### 4. The attachment definition is read by hand

- **Fact.** `route_definition` takes `route`, `raw-body-bytes`, `mappings` and `input-schema` out of the JSON value key by key. `input_mapping` decodes each mapping by hand.
- **Fact.** `read_limits` takes page limits and band starts from inside the JSON schema (`input-schema.items.properties...`). A value of the wrong type gives "no limit" without a warning.
- **Fact.** The comment says this is on purpose: unknown keys must not take routing offline.
- **Option.** One typed struct for the served HTTP definition in `wamn-catalog`, with optional keys defaulted and unknown keys ignored. That keeps "generous in what is accepted". `input_mapping` goes. `read_limits` stays, next to the struct.
- **To check in the catalog tab.** Whether the publish side already has this struct.

### 5. Three entry points exist only for tests

- **Fact.** `authenticate_authorization_for_test`, `authenticate_headers_for_test` and `authenticate_request_for_test` wrap `authenticate`. They turn string pairs into `Header` and the refusal into a tuple. 30 call sites, all in `wamn-runtime` tests.
- **Fact.** `authenticate` and `Header` are public.
- **Option.** Delete the three. The tests call `authenticate`.

### 6. Public functions with no outside caller

- **Fact.** `serves_read`, `session_cookie`, `bearer_token` and `NoRelease` are `pub`. Only this file uses them. The runtime and the edge use `route_credential`, `required_bearer_token`, `check_csrf` and the three refusal functions.
- **Option.** Make the four private.

### 7. One more per-scope map, and a table scan on every host call

- **Fact.** `InvocationTraces` is a map scope → trace with `bind`, `get`, `revoke`. `native_policy.rs` binds and revokes it with the other per-scope maps.
- **Fact.** `invocation_trace` looks in that map, then scans the whole resource table of the store for a `RequestTrace`, then takes the current context.
- **Option.** The map joins the held scope record (tab 15 finding 2). I added this file to that held row. The table scan goes with it.

### 8. Every header is copied to read the trace context

- **Fact.** The `authenticate` handler copies all request headers into a new `Vec<(String, String)>` only so `request_trace` can look up the trace headers.
- **Option.** `request_trace` reads the `Header` slice it already has. The copy goes. Follows the repository rule on clones.

## Fine as is

- The limiter: no queue, release on drop, one metric callback pair.
- Input schemas: compiled once per canonical hash.
- Credential selection: one mechanism per request, no fallback to a second credential.
- `release: Option`: a host without a release is a real state and refuses.
- `lib.rs` lists the crates the engine must not depend on. A dependency test holds the list.

## Tests

**Move** (finding 1, with their module)

- Limiter: `route_limit_is_nonzero_and_has_one_chart_default`, `route_slots_are_independent_shed_without_queueing_and_release_on_drop`, `a_box_limit_counts_the_requests_of_every_route_together`
- Input schema: `a_nonmatching_payload_refuses_with_the_pointer_of_the_offending_value`, `canonical_schema_hash_deduplicates_reordered_schemas_and_isolates_distinct_ones`, `platform_causes_refuse_without_a_pointer`
- Credentials: `bearer_parsing_has_one_success_shape_and_one_opaque_refusal_class`, `the_session_cookie_is_found_among_others_once_or_refused`, `csrf_requires_the_claim_and_one_header_that_hashes_to_it`, `csrf_not_required_skips_the_header_but_still_needs_the_claim`, `originating_caller_holds_its_references_or_admin`
- Projection: the other 19 tests of the file

**Change**

- `omitted_projection_fields_defer_to_their_real_authority`, `a_matching_method_and_authority_serves_the_release_attachment`: drop `host` and `mapped-limit` (finding 3 option A)
- `apps/platform/ingress/http-route/tests/adversarial.rs`: three uses of `mapped_limit` test the guest's own limit (finding 3 option A)
- `runtime/src/plugins/route_authentication/tests/signature.rs`: fixture field `mapped_limit` (finding 3 option A)
- 30 call sites of the three test entry points, in `route_authentication.rs`, `tests/connection_live.rs`, `tests/signature.rs`, `claims/tests.rs`, `tests/session_route_authentication.rs` (finding 5)

**Delete**

- None in this part.

## Not read

- Test bodies: names only.
- The guest `http-route`: only the lines that read a `route-definition` field.
- `wamn-catalog` types `ServedAttachment`, `AttachmentAuthPolicy`: signatures only.

## Rulings

None yet.

**Ruled 2026-11-11**

- Findings 1, 2, 4 to 8: accepted.
- Finding 3: **option A.** Delete `host` and `mapped-limit` from `wamn:flow-http-routing`. `operation-deadline-ms` is decided in part 2.
- Tab 21 is fully ruled.

**Correction, 2026-11-11 (tab 31 finding 3 A).** The application route table moves out of the host into the composed guest. Effect on this tab:

- Finding 1 (split into four modules): `projection.rs` and `input_schema.rs` are deleted, not split out; `credentials.rs` stays as the identity capability; `limiter.rs` waits for tab 31 check 1.
- Finding 2 (build the projection once at construction): superseded; the composer builds the table once per release.
- Finding 3 (delete `host`, `mapped-limit`): superseded; `route-definition` leaves the WIT entirely.
- The host keeps a policy lookup only for its own native endpoints (control routes, file routes).
