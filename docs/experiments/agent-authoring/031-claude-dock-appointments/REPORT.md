# Summary

The dock-appointments scenario is built as the wamn package `wamn_dock@1.0.0`
under `packages/dock`, served by the guest component `dock` under
`components/application/dock`. `wamn dev` completes all twelve stages through
Activate. All five operations the scenario names run against the running
release, and every invariant from DOCK-0 to DOCK-6 is shown below with its
exact request and answer.

Two appointments on one dock never overlap because a booking locks the dock row
before it reads for an overlap. Two concurrent bookings of one dock therefore
serialize, and the read the second one runs already sees the first one's row.
Nothing else in the package writes an appointment slot.

Every command is idempotent by claim. The claim row pre-generates the identity
the command returns, so a replay hands back the identity the first call minted
and writes nothing new.

One thing is not clean. The guest sources are not `rustfmt` formatted, and I
cannot format them. The reason is under "Where I got stuck".

# Changes

All changes sit inside the `allowed_paths` of `task.json`.

- `packages/dock/**` (new). The package: `wamn.json`, one migration, the
  authored SQL for four commands and one projection, the publication documents
  (component declaration, five wirings, five HTTP attachments), and the
  `generated/` tree the loop's Generate stage writes.
- `components/application/dock/**` (new). The guest crate `dock`: the WIT
  packages for its five exported interfaces, and the Rust that orders the
  generated statements and names each refusal.
- `components/Cargo.toml`. One line adds `application/dock` to the members.
- `components/Cargo.lock`. Fourteen lines record the new member.

The package declares three models (`carrier`, `dock`, `appointment`) and four
CDC-excluded claim relations, one per command. The five operations are custom
operations, so the package owns every statement they run.

| wire name | kind | route |
|---|---|---|
| `carrier.create` | command | `POST /carrier/create` |
| `dock.create` | command | `POST /dock/create` |
| `appointment.book` | command | `POST /appointment/book` |
| `appointment.check_in` | command | `POST /appointment/check-in` |
| `appointment.query` | projection | `POST /appointment/query` |

# How I verified

## The component's own tests

```
$ cargo test --manifest-path components/Cargo.toml -p dock --all-targets --locked --offline
```

```
running 18 tests
test book::tests::a_slot_that_does_not_run_forwards_is_refused ... ok
test book::tests::an_unspellable_scalar_is_refused_before_any_statement ... ok
test book::tests::a_different_slot_is_a_different_command ... ok
test check_in::tests::a_different_arrival_is_a_different_command ... ok
test book::tests::the_canonical_command_is_spelling_independent ... ok
test check_in::tests::an_unspellable_scalar_is_refused_before_any_statement ... ok
test check_in::tests::the_canonical_command_is_spelling_independent ... ok
test create::tests::a_replay_admits_only_the_body_the_key_already_carries ... ok
test create::tests::an_empty_name_is_refused_before_any_statement ... ok
test create::tests::the_canonical_command_excludes_the_key_and_separates_the_names ... ok
test operation::tests::a_non_array_envelope_refuses_the_invocation ... ok
test operation::tests::a_refusal_serializes_as_code_and_detail_beside_its_request_id ... ok
test operation::tests::the_correlation_id_is_the_envelopes_and_the_body_is_the_items ... ok
test scalar::tests::a_day_bounds_one_half_open_utc_interval ... ok
test scalar::tests::a_status_is_one_of_the_three_the_model_admits ... ok
test scalar::tests::uuids_and_timestamps_are_respelled_canonically ... ok
test operation::tests::every_operation_refuses_only_what_its_contract_declares ... ok
test operation::tests::an_oversized_envelope_refuses_before_any_item_runs ... ok

test result: ok. 18 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

```

The eighteen tests cover the envelope, the canonical command bytes, the scalar
re-spelling, the day bounds, and the replay decision. One test reads every
generated `*.errors.json` contract and holds each module's refusal list to it,
so a module cannot refuse with a literal its own contract never declared.

## The loop

```
$ wamn dev --config "$WAMN_DEV_CONFIG" --overlay-root packages/dock --hold
```

```
applied wamn_dock@1.0.0: 1 migration(s)
applied wamn_dock@1.0.0: 0 migration(s) (already converged)
projected sha256:0c22de03e1e7e858b792fbca19938f6fc92979d17911ad846f1ddb107e99e970 (source-project: already converged; control: already converged)
sha256:0c22de03e1e7e858b792fbca19938f6fc92979d17911ad846f1ddb107e99e970
sha256:b4f3fb90df4a88e495b31b815fae94034b9edc0ef5d0ec0db9aa6cb56e5abece
sha256:b4f3fb90df4a88e495b31b815fae94034b9edc0ef5d0ec0db9aa6cb56e5abece
2026-09-07T22:31:08.794988Z  INFO connected to NATS
2026-09-07T22:31:08.974222Z  INFO wiring activation doorbell listening; dropped every cached pointer channel="wamn_wiring_activation" dropped=0
2026-09-07T22:31:09.118734Z  INFO release component pull completed component_digest=sha256:0c22de03e1e7e858b792fbca19938f6fc92979d17911ad846f1ddb107e99e970 component_bytes=537012 elapsed_ms=63
2026-09-07T22:31:09.153003Z  INFO release component compilation completed component_digest=sha256:0c22de03e1e7e858b792fbca19938f6fc92979d17911ad846f1ddb107e99e970 compile_ms=33 compile_wall_ms=34
2026-09-07T22:31:09.157259Z  WARN wamn.component.linker_setup:wamn.linker.register: component imports wamn:postgres but sets no wamn.tenant; calls will be refused component="dock#0"
2026-09-07T22:31:09.161198Z  INFO release component instantiation completed component_digest=sha256:0c22de03e1e7e858b792fbca19938f6fc92979d17911ad846f1ddb107e99e970 elapsed_ms=8 total_elapsed_ms=105
2026-09-07T22:31:09.161420Z  INFO synchronous release preload completed synchronous_wirings=5 component_digests=1 elapsed_ms=268
2026-09-07T22:31:09.161622Z  INFO release preload completed; component cache warm
2026-09-07T22:31:09.201472Z  INFO wamn-host starting (base plugins: wasi:config, wamn:logging, wasi:otel, wamn:postgres, wamn:jetstream, wamn:flow-http-routing) router_delivery=true
2026-09-07T22:31:09.201554Z  INFO wamn-host welded to its release effective_release_id=1 manifest_digest=sha256:b4f3fb90df4a88e495b31b815fae94034b9edc0ef5d0ec0db9aa6cb56e5abece
2026-09-07T22:31:09.201644Z  INFO HTTP server listening addr=127.0.0.1:34571 protocol="HTTP"
2026-09-07T22:31:09.201735Z  INFO Starting WASI OTel plugin endpoint=http://127.0.0.1:4319 protocol="grpc"
2026-09-07T22:31:09.202963Z  INFO WASI OTel plugin started
2026-09-07T22:31:09.203096Z  INFO Host started host_id="36e091f8-d265-42ac-8da6-c96aab9cd6e5" friendly_name="fuzzy-payment-0978" host_name="wamn-dev-receiving-487178" labels={"hostgroup": "wamn-dev-receiving"} version="2.8.0"
2026-09-07T22:31:09.203607Z  INFO Host provides interfaces count=10 interfaces=["wasi:cli/terminal-stdout,stderr,stdin,exit,environment,stdout,terminal-output,terminal-input,terminal-stdin,terminal-stderr@0.2.0", "wasi:clocks/wall-time,monotonic-clock@0.2.0", "wasi:clocks/monotonic-clock,wall-clock@0.2.0", "wasi:filesystem/types,preopens@0.2.0", "wasi:http/incoming-handler,types,outgoing-handler@0.2.0", "wasi:http/types,handler@0.3.0", "wasi:io/error,streams,poll@0.2.0", "wasi:random/random,insecure-seed,insecure@0.2.0", "wasi:random/random@0.2.0", "wasi:sockets/tcp-create-socket,udp,instance-network,udp-create-socket,tcp,network,ip-name-lookup@0.2.0"]
2026-09-07T22:31:09.203757Z  INFO wamn-host runtime startup completed elapsed_ms=595
2026-09-07T22:31:10.219631Z  INFO workload_start{workload_id=wamn-dev-flow-http workload.name="flow-http" workload.namespace="dev"}: Starting workload workload_id="wamn-dev-flow-http" namespace="dev" name="flow-http"
run completed: migrate,introspect,generate,build,virtualize,apply,acl,admit,gate,publish,release,activate
run served: http://127.0.0.1:34571 host=receiving.localhost
run holding
2026-09-07T22:31:22.526531Z  WARN handle_http_request{http.method=POST http.uri=/carrier/create http.host=receiving.localhost http.request.method=POST url.path=/carrier/create server.address="receiving.localhost"}:invoke_component_handler{workload.name="flow-http" workload.namespace="dev" workload.id="wamn-dev-flow-http"}:http.handle:wamn.component.invoke{wamn.tenant=receiving-route-auth wamn.project=receiving wamn.environment=dev wamn.wiring_id=carrier_create wamn.wiring_version=1 wamn.component_digest=sha256:0c22de03e1e7e858b792fbca19938f6fc92979d17911ad846f1ddb107e99e970 wamn.node_id=operation wamn.operation=wamn-dock:carrier/create@1.0.0 wamn.caller_principal_id="12e40841-f510-4744-a462-d49055c5be4f"}:wamn.component.linker_setup:wamn.linker.register: component imports wamn:postgres but sets no wamn.tenant; calls will be refused component="dock#1"
2026-09-07T22:31:22.675526Z  WARN handle_http_request{http.method=POST http.uri=/carrier/create http.host=receiving.localhost http.request.method=POST url.path=/carrier/create server.address="receiving.localhost"}:invoke_component_handler{workload.name="flow-http" workload.namespace="dev" workload.id="wamn-dev-flow-http"}:http.handle:wamn.component.invoke{wamn.tenant=receiving-route-auth wamn.project=receiving wamn.environment=dev wamn.wiring_id=carrier_create wamn.wiring_version=1 wamn.component_digest=sha256:0c22de03e1e7e858b792fbca19938f6fc92979d17911ad846f1ddb107e99e970 wamn.node_id=operation wamn.operation=wamn-dock:carrier/create@1.0.0 wamn.caller_principal_id="12e40841-f510-4744-a462-d49055c5be4f"}:wamn.component.linker_setup:wamn.linker.register: component imports wamn:postgres but sets no wamn.tenant; calls will be refused component="dock#2"
2026-09-07T22:31:22.716990Z  WARN handle_http_request{http.method=POST http.uri=/dock/create http.host=receiving.localhost http.request.method=POST url.path=/dock/create server.address="receiving.localhost"}:invoke_component_handler{workload.name="flow-http" workload.namespace="dev" workload.id="wamn-dev-flow-http"}:http.handle:wamn.component.invoke{wamn.tenant=receiving-route-auth wamn.project=receiving wamn.environment=dev wamn.wiring_id=dock_create wamn.wiring_version=1 wamn.component_digest=sha256:0c22de03e1e7e858b792fbca19938f6fc92979d17911ad846f1ddb107e99e970 wamn.node_id=operation wamn.operation=wamn-dock:dock/create@1.0.0 wamn.caller_principal_id="12e40841-f510-4744-a462-d49055c5be4f"}:wamn.component.linker_setup:wamn.linker.register: component imports wamn:postgres but sets no wamn.tenant; calls will be refused component="dock#3"
2026-09-07T22:31:22.762947Z  WARN handle_http_request{http.method=POST http.uri=/dock/create http.host=receiving.localhost http.request.method=POST url.path=/dock/create server.address="receiving.localhost"}:invoke_component_handler{workload.name="flow-http" workload.namespace="dev" workload.id="wamn-dev-flow-http"}:http.handle:wamn.component.invoke{wamn.tenant=receiving-route-auth wamn.project=receiving wamn.environment=dev wamn.wiring_id=dock_create wamn.wiring_version=1 wamn.component_digest=sha256:0c22de03e1e7e858b792fbca19938f6fc92979d17911ad846f1ddb107e99e970 wamn.node_id=operation wamn.operation=wamn-dock:dock/create@1.0.0 wamn.caller_principal_id="12e40841-f510-4744-a462-d49055c5be4f"}:wamn.component.linker_setup:wamn.linker.register: component imports wamn:postgres but sets no wamn.tenant; calls will be refused component="dock#4"
2026-09-07T22:31:22.809318Z  WARN handle_http_request{http.method=POST http.uri=/appointment/book http.host=receiving.localhost http.request.method=POST url.path=/appointment/book server.address="receiving.localhost"}:invoke_component_handler{workload.name="flow-http" workload.namespace="dev" workload.id="wamn-dev-flow-http"}:http.handle:wamn.component.invoke{wamn.tenant=receiving-route-auth wamn.project=receiving wamn.environment=dev wamn.wiring_id=appointment_book wamn.wiring_version=1 wamn.component_digest=sha256:0c22de03e1e7e858b792fbca19938f6fc92979d17911ad846f1ddb107e99e970 wamn.node_id=operation wamn.operation=wamn-dock:appointment/book@1.0.0 wamn.caller_principal_id="12e40841-f510-4744-a462-d49055c5be4f"}:wamn.component.linker_setup:wamn.linker.register: component imports wamn:postgres but sets no wamn.tenant; calls will be refused component="dock#5"
2026-09-07T22:31:22.882681Z  WARN handle_http_request{http.method=POST http.uri=/appointment/book http.host=receiving.localhost http.request.method=POST url.path=/appointment/book server.address="receiving.localhost"}:invoke_component_handler{workload.name="flow-http" workload.namespace="dev" workload.id="wamn-dev-flow-http"}:http.handle:wamn.component.invoke{wamn.tenant=receiving-route-auth wamn.project=receiving wamn.environment=dev wamn.wiring_id=appointment_book wamn.wiring_version=1 wamn.component_digest=sha256:0c22de03e1e7e858b792fbca19938f6fc92979d17911ad846f1ddb107e99e970 wamn.node_id=operation wamn.operation=wamn-dock:appointment/book@1.0.0 wamn.caller_principal_id="12e40841-f510-4744-a462-d49055c5be4f"}:wamn.component.linker_setup:wamn.linker.register: component imports wamn:postgres but sets no wamn.tenant; calls will be refused component="dock#6"
2026-09-07T22:31:23.017558Z  WARN handle_http_request{http.method=POST http.uri=/appointment/book http.host=receiving.localhost http.request.method=POST url.path=/appointment/book server.address="receiving.localhost"}:invoke_component_handler{workload.name="flow-http" workload.namespace="dev" workload.id="wamn-dev-flow-http"}:http.handle:wamn.component.invoke{wamn.tenant=receiving-route-auth wamn.project=receiving wamn.environment=dev wamn.wiring_id=appointment_book wamn.wiring_version=1 wamn.component_digest=sha256:0c22de03e1e7e858b792fbca19938f6fc92979d17911ad846f1ddb107e99e970 wamn.node_id=operation wamn.operation=wamn-dock:appointment/book@1.0.0 wamn.caller_principal_id="12e40841-f510-4744-a462-d49055c5be4f"}:wamn.component.linker_setup:wamn.linker.register: component imports wamn:postgres but sets no wamn.tenant; calls will be refused component="dock#7"
2026-09-07T22:31:23.165414Z  WARN handle_http_request{http.method=POST http.uri=/appointment/book http.host=receiving.localhost http.request.method=POST url.path=/appointment/book server.address="receiving.localhost"}:invoke_component_handler{workload.name="flow-http" workload.namespace="dev" workload.id="wamn-dev-flow-http"}:http.handle:wamn.component.invoke{wamn.tenant=receiving-route-auth wamn.project=receiving wamn.environment=dev wamn.wiring_id=appointment_book wamn.wiring_version=1 wamn.component_digest=sha256:0c22de03e1e7e858b792fbca19938f6fc92979d17911ad846f1ddb107e99e970 wamn.node_id=operation wamn.operation=wamn-dock:appointment/book@1.0.0 wamn.caller_principal_id="12e40841-f510-4744-a462-d49055c5be4f"}:wamn.component.linker_setup:wamn.linker.register: component imports wamn:postgres but sets no wamn.tenant; calls will be refused component="dock#8"
2026-09-07T22:31:23.211454Z  WARN handle_http_request{http.method=POST http.uri=/appointment/book http.host=receiving.localhost http.request.method=POST url.path=/appointment/book server.address="receiving.localhost"}:invoke_component_handler{workload.name="flow-http" workload.namespace="dev" workload.id="wamn-dev-flow-http"}:http.handle:wamn.component.invoke{wamn.tenant=receiving-route-auth wamn.project=receiving wamn.environment=dev wamn.wiring_id=appointment_book wamn.wiring_version=1 wamn.component_digest=sha256:0c22de03e1e7e858b792fbca19938f6fc92979d17911ad846f1ddb107e99e970 wamn.node_id=operation wamn.operation=wamn-dock:appointment/book@1.0.0 wamn.caller_principal_id="12e40841-f510-4744-a462-d49055c5be4f"}:wamn.component.linker_setup:wamn.linker.register: component imports wamn:postgres but sets no wamn.tenant; calls will be refused component="dock#9"
2026-09-07T22:31:23.255643Z  WARN handle_http_request{http.method=POST http.uri=/appointment/book http.host=receiving.localhost http.request.method=POST url.path=/appointment/book server.address="receiving.localhost"}:invoke_component_handler{workload.name="flow-http" workload.namespace="dev" workload.id="wamn-dev-flow-http"}:http.handle:wamn.component.invoke{wamn.tenant=receiving-route-auth wamn.project=receiving wamn.environment=dev wamn.wiring_id=appointment_book wamn.wiring_version=1 wamn.component_digest=sha256:0c22de03e1e7e858b792fbca19938f6fc92979d17911ad846f1ddb107e99e970 wamn.node_id=operation wamn.operation=wamn-dock:appointment/book@1.0.0 wamn.caller_principal_id="12e40841-f510-4744-a462-d49055c5be4f"}:wamn.component.linker_setup:wamn.linker.register: component imports wamn:postgres but sets no wamn.tenant; calls will be refused component="dock#10"
2026-09-07T22:31:23.304570Z  WARN handle_http_request{http.method=POST http.uri=/appointment/book http.host=receiving.localhost http.request.method=POST url.path=/appointment/book server.address="receiving.localhost"}:invoke_component_handler{workload.name="flow-http" workload.namespace="dev" workload.id="wamn-dev-flow-http"}:http.handle:wamn.component.invoke{wamn.tenant=receiving-route-auth wamn.project=receiving wamn.environment=dev wamn.wiring_id=appointment_book wamn.wiring_version=1 wamn.component_digest=sha256:0c22de03e1e7e858b792fbca19938f6fc92979d17911ad846f1ddb107e99e970 wamn.node_id=operation wamn.operation=wamn-dock:appointment/book@1.0.0 wamn.caller_principal_id="12e40841-f510-4744-a462-d49055c5be4f"}:wamn.component.linker_setup:wamn.linker.register: component imports wamn:postgres but sets no wamn.tenant; calls will be refused component="dock#11"
2026-09-07T22:31:23.350350Z  WARN handle_http_request{http.method=POST http.uri=/appointment/check-in http.host=receiving.localhost http.request.method=POST url.path=/appointment/check-in server.address="receiving.localhost"}:invoke_component_handler{workload.name="flow-http" workload.namespace="dev" workload.id="wamn-dev-flow-http"}:http.handle:wamn.component.invoke{wamn.tenant=receiving-route-auth wamn.project=receiving wamn.environment=dev wamn.wiring_id=appointment_check_in wamn.wiring_version=1 wamn.component_digest=sha256:0c22de03e1e7e858b792fbca19938f6fc92979d17911ad846f1ddb107e99e970 wamn.node_id=operation wamn.operation=wamn-dock:appointment/check-in@1.0.0 wamn.caller_principal_id="12e40841-f510-4744-a462-d49055c5be4f"}:wamn.component.linker_setup:wamn.linker.register: component imports wamn:postgres but sets no wamn.tenant; calls will be refused component="dock#12"
2026-09-07T22:31:23.468915Z  WARN handle_http_request{http.method=POST http.uri=/appointment/check-in http.host=receiving.localhost http.request.method=POST url.path=/appointment/check-in server.address="receiving.localhost"}:invoke_component_handler{workload.name="flow-http" workload.namespace="dev" workload.id="wamn-dev-flow-http"}:http.handle:wamn.component.invoke{wamn.tenant=receiving-route-auth wamn.project=receiving wamn.environment=dev wamn.wiring_id=appointment_check_in wamn.wiring_version=1 wamn.component_digest=sha256:0c22de03e1e7e858b792fbca19938f6fc92979d17911ad846f1ddb107e99e970 wamn.node_id=operation wamn.operation=wamn-dock:appointment/check-in@1.0.0 wamn.caller_principal_id="12e40841-f510-4744-a462-d49055c5be4f"}:wamn.component.linker_setup:wamn.linker.register: component imports wamn:postgres but sets no wamn.tenant; calls will be refused component="dock#13"
2026-09-07T22:31:23.510387Z  WARN handle_http_request{http.method=POST http.uri=/appointment/query http.host=receiving.localhost http.request.method=POST url.path=/appointment/query server.address="receiving.localhost"}:invoke_component_handler{workload.name="flow-http" workload.namespace="dev" workload.id="wamn-dev-flow-http"}:http.handle:wamn.component.invoke{wamn.tenant=receiving-route-auth wamn.project=receiving wamn.environment=dev wamn.wiring_id=appointment_query wamn.wiring_version=1 wamn.component_digest=sha256:0c22de03e1e7e858b792fbca19938f6fc92979d17911ad846f1ddb107e99e970 wamn.node_id=operation wamn.operation=wamn-dock:appointment/query@1.0.0 wamn.caller_principal_id="12e40841-f510-4744-a462-d49055c5be4f"}:wamn.component.linker_setup:wamn.linker.register: component imports wamn:postgres but sets no wamn.tenant; calls will be refused component="dock#14"
2026-09-07T22:31:23.560022Z  WARN handle_http_request{http.method=POST http.uri=/appointment/query http.host=receiving.localhost http.request.method=POST url.path=/appointment/query server.address="receiving.localhost"}:invoke_component_handler{workload.name="flow-http" workload.namespace="dev" workload.id="wamn-dev-flow-http"}:http.handle:wamn.component.invoke{wamn.tenant=receiving-route-auth wamn.project=receiving wamn.environment=dev wamn.wiring_id=appointment_query wamn.wiring_version=1 wamn.component_digest=sha256:0c22de03e1e7e858b792fbca19938f6fc92979d17911ad846f1ddb107e99e970 wamn.node_id=operation wamn.operation=wamn-dock:appointment/query@1.0.0 wamn.caller_principal_id="12e40841-f510-4744-a462-d49055c5be4f"}:wamn.component.linker_setup:wamn.linker.register: component imports wamn:postgres but sets no wamn.tenant; calls will be refused component="dock#15"
2026-09-07T22:31:23.595939Z  WARN handle_http_request{http.method=POST http.uri=/appointment/query http.host=receiving.localhost http.request.method=POST url.path=/appointment/query server.address="receiving.localhost"}:invoke_component_handler{workload.name="flow-http" workload.namespace="dev" workload.id="wamn-dev-flow-http"}:http.handle:wamn.component.invoke{wamn.tenant=receiving-route-auth wamn.project=receiving wamn.environment=dev wamn.wiring_id=appointment_query wamn.wiring_version=1 wamn.component_digest=sha256:0c22de03e1e7e858b792fbca19938f6fc92979d17911ad846f1ddb107e99e970 wamn.node_id=operation wamn.operation=wamn-dock:appointment/query@1.0.0 wamn.caller_principal_id="12e40841-f510-4744-a462-d49055c5be4f"}:wamn.component.linker_setup:wamn.linker.register: component imports wamn:postgres but sets no wamn.tenant; calls will be refused component="dock#16"
2026-09-07T22:31:40.248159Z  WARN handle_http_request{http.method=POST http.uri=/appointment/check-in http.host=receiving.localhost http.request.method=POST url.path=/appointment/check-in server.address="receiving.localhost"}:invoke_component_handler{workload.name="flow-http" workload.namespace="dev" workload.id="wamn-dev-flow-http"}:http.handle:wamn.component.invoke{wamn.tenant=receiving-route-auth wamn.project=receiving wamn.environment=dev wamn.wiring_id=appointment_check_in wamn.wiring_version=1 wamn.component_digest=sha256:0c22de03e1e7e858b792fbca19938f6fc92979d17911ad846f1ddb107e99e970 wamn.node_id=operation wamn.operation=wamn-dock:appointment/check-in@1.0.0 wamn.caller_principal_id="12e40841-f510-4744-a462-d49055c5be4f"}:wamn.component.linker_setup:wamn.linker.register: component imports wamn:postgres but sets no wamn.tenant; calls will be refused component="dock#17"
2026-09-07T22:31:40.296177Z  WARN handle_http_request{http.method=POST http.uri=/appointment/check-in http.host=receiving.localhost http.request.method=POST url.path=/appointment/check-in server.address="receiving.localhost"}:invoke_component_handler{workload.name="flow-http" workload.namespace="dev" workload.id="wamn-dev-flow-http"}:http.handle:wamn.component.invoke{wamn.tenant=receiving-route-auth wamn.project=receiving wamn.environment=dev wamn.wiring_id=appointment_check_in wamn.wiring_version=1 wamn.component_digest=sha256:0c22de03e1e7e858b792fbca19938f6fc92979d17911ad846f1ddb107e99e970 wamn.node_id=operation wamn.operation=wamn-dock:appointment/check-in@1.0.0 wamn.caller_principal_id="12e40841-f510-4744-a462-d49055c5be4f"}:wamn.component.linker_setup:wamn.linker.register: component imports wamn:postgres but sets no wamn.tenant; calls will be refused component="dock#18"
2026-09-07T22:31:40.340718Z  WARN handle_http_request{http.method=POST http.uri=/dock/create http.host=receiving.localhost http.request.method=POST url.path=/dock/create server.address="receiving.localhost"}:invoke_component_handler{workload.name="flow-http" workload.namespace="dev" workload.id="wamn-dev-flow-http"}:http.handle:wamn.component.invoke{wamn.tenant=receiving-route-auth wamn.project=receiving wamn.environment=dev wamn.wiring_id=dock_create wamn.wiring_version=1 wamn.component_digest=sha256:0c22de03e1e7e858b792fbca19938f6fc92979d17911ad846f1ddb107e99e970 wamn.node_id=operation wamn.operation=wamn-dock:dock/create@1.0.0 wamn.caller_principal_id="12e40841-f510-4744-a462-d49055c5be4f"}:wamn.component.linker_setup:wamn.linker.register: component imports wamn:postgres but sets no wamn.tenant; calls will be refused component="dock#19"
2026-09-07T22:31:40.390208Z  WARN handle_http_request{http.method=POST http.uri=/appointment/book http.host=receiving.localhost http.request.method=POST url.path=/appointment/book server.address="receiving.localhost"}:invoke_component_handler{workload.name="flow-http" workload.namespace="dev" workload.id="wamn-dev-flow-http"}:http.handle:wamn.component.invoke{wamn.tenant=receiving-route-auth wamn.project=receiving wamn.environment=dev wamn.wiring_id=appointment_book wamn.wiring_version=1 wamn.component_digest=sha256:0c22de03e1e7e858b792fbca19938f6fc92979d17911ad846f1ddb107e99e970 wamn.node_id=operation wamn.operation=wamn-dock:appointment/book@1.0.0 wamn.caller_principal_id="12e40841-f510-4744-a462-d49055c5be4f"}:wamn.component.linker_setup:wamn.linker.register: component imports wamn:postgres but sets no wamn.tenant; calls will be refused component="dock#20"
2026-09-07T22:31:40.483583Z  WARN handle_http_request{http.method=POST http.uri=/appointment/book http.host=receiving.localhost http.request.method=POST url.path=/appointment/book server.address="receiving.localhost"}:invoke_component_handler{workload.name="flow-http" workload.namespace="dev" workload.id="wamn-dev-flow-http"}:http.handle:wamn.component.invoke{wamn.tenant=receiving-route-auth wamn.project=receiving wamn.environment=dev wamn.wiring_id=appointment_book wamn.wiring_version=1 wamn.component_digest=sha256:0c22de03e1e7e858b792fbca19938f6fc92979d17911ad846f1ddb107e99e970 wamn.node_id=operation wamn.operation=wamn-dock:appointment/book@1.0.0 wamn.caller_principal_id="12e40841-f510-4744-a462-d49055c5be4f"}:wamn.component.linker_setup:wamn.linker.register: component imports wamn:postgres but sets no wamn.tenant; calls will be refused component="dock#21"
```

The run holds at `http://127.0.0.1:34571` with `Host: receiving.localhost`. The
worktree is clean at commit `3d529f85807f3d36f495cb099b6298eaecec8fd2`, so the
Generate stage reproduced the committed `generated/` tree byte for byte.

## Every operation the scenario names

Each call below went to the held run. The header block on every request is
`Host: receiving.localhost`, `Authorization: Bearer <token>` and
`content-type: application/json`, where `<token>` is `.stringData.token` in
`$WAMN_ROUTE_CALLER_PAT_FILE`. The shape of one call:

```
curl -sS -X POST http://127.0.0.1:34571/appointment/book \
  -H "Host: $WAMN_ROUTE_HOST" -H "Authorization: Bearer $TOKEN" \
  -H 'content-type: application/json' -d '<request below>'
```

The `psql` lines read the project-environment database for inspection only.

```

--- DOCK-0 carrier.create
POST /carrier/create
request:  [{"request_id":"r1","value":{"idempotency_key":"f-carrier-1","name":"Northwind Haulage"}}]
response: [{"request_id":"r1","value":{"carrier_id":"d3296298-2ade-45d2-8670-609cf17fbd5b"}}]

--- DOCK-0 dock.create
POST /dock/create
request:  [{"request_id":"r2","value":{"idempotency_key":"f-dock-1","name":"Door 7"}}]
response: [{"request_id":"r2","value":{"dock_id":"3c1b05f5-296b-450c-9255-3cc0bc1308d9"}}]

carrier_id=d3296298-2ade-45d2-8670-609cf17fbd5b
dock_id=3c1b05f5-296b-450c-9255-3cc0bc1308d9

--- appointment.book (09:00-10:00)
POST /appointment/book
request:  [{"request_id":"r3","value":{"idempotency_key":"f-book-1","carrier_id":"d3296298-2ade-45d2-8670-609cf17fbd5b","dock_id":"3c1b05f5-296b-450c-9255-3cc0bc1308d9","slot_start":"2026-10-01T09:00:00.000000Z","slot_end":"2026-10-01T10:00:00.000000Z"}}]
response: [{"request_id":"r3","value":{"appointment_id":"6730c01e-c120-4630-8639-5ef6afd7f82b","status":"scheduled"}}]

appointment_id=6730c01e-c120-4630-8639-5ef6afd7f82b

--- DOCK-2 appointment row count before the replay
query:  SELECT count(*) FROM receiving.appointment WHERE dock_id = '3c1b05f5-296b-450c-9255-3cc0bc1308d9'
answer: 1

--- DOCK-2 appointment.book replayed byte for byte
POST /appointment/book
request:  [{"request_id":"r3","value":{"idempotency_key":"f-book-1","carrier_id":"d3296298-2ade-45d2-8670-609cf17fbd5b","dock_id":"3c1b05f5-296b-450c-9255-3cc0bc1308d9","slot_start":"2026-10-01T09:00:00.000000Z","slot_end":"2026-10-01T10:00:00.000000Z"}}]
response: [{"request_id":"r3","value":{"appointment_id":"6730c01e-c120-4630-8639-5ef6afd7f82b","status":"scheduled"}}]

--- DOCK-2 appointment row count after the replay
query:  SELECT count(*) FROM receiving.appointment WHERE dock_id = '3c1b05f5-296b-450c-9255-3cc0bc1308d9'
answer: 1

--- DOCK-3 appointment.book, same key, different slot
POST /appointment/book
request:  [{"request_id":"r4","value":{"idempotency_key":"f-book-1","carrier_id":"d3296298-2ade-45d2-8670-609cf17fbd5b","dock_id":"3c1b05f5-296b-450c-9255-3cc0bc1308d9","slot_start":"2026-10-01T14:00:00.000000Z","slot_end":"2026-10-01T15:00:00.000000Z"}}]
response: [{"error":{"code":"idempotency_conflict","detail":{"field":"value.idempotency_key"}},"request_id":"r4"}]

--- DOCK-1 appointment.book, overlapping 09:30-10:30
POST /appointment/book
request:  [{"request_id":"r5","value":{"idempotency_key":"f-book-overlap","carrier_id":"d3296298-2ade-45d2-8670-609cf17fbd5b","dock_id":"3c1b05f5-296b-450c-9255-3cc0bc1308d9","slot_start":"2026-10-01T09:30:00.000000Z","slot_end":"2026-10-01T10:30:00.000000Z"}}]
response: [{"error":{"code":"slot_unavailable","detail":{"field":"value.slot_start"}},"request_id":"r5"}]

--- appointment.book, adjacent 10:00-11:00
POST /appointment/book
request:  [{"request_id":"r6","value":{"idempotency_key":"f-book-2","carrier_id":"d3296298-2ade-45d2-8670-609cf17fbd5b","dock_id":"3c1b05f5-296b-450c-9255-3cc0bc1308d9","slot_start":"2026-10-01T10:00:00.000000Z","slot_end":"2026-10-01T11:00:00.000000Z"}}]
response: [{"request_id":"r6","value":{"appointment_id":"55d000c7-9c0d-4589-87ac-e032b70f872d","status":"scheduled"}}]

--- appointment.book, earlier 08:00-09:00
POST /appointment/book
request:  [{"request_id":"r7","value":{"idempotency_key":"f-book-3","carrier_id":"d3296298-2ade-45d2-8670-609cf17fbd5b","dock_id":"3c1b05f5-296b-450c-9255-3cc0bc1308d9","slot_start":"2026-10-01T08:00:00.000000Z","slot_end":"2026-10-01T09:00:00.000000Z"}}]
response: [{"request_id":"r7","value":{"appointment_id":"69257de4-a950-4fa2-91ae-fc6d08da6cbf","status":"scheduled"}}]

--- DOCK-4 appointment.check_in
POST /appointment/check-in
request:  [{"request_id":"r8","value":{"idempotency_key":"f-checkin-1","appointment_id":"6730c01e-c120-4630-8639-5ef6afd7f82b","arrived_at":"2026-10-01T09:04:30.000000Z"}}]
response: [{"request_id":"r8","value":{"appointment_id":"6730c01e-c120-4630-8639-5ef6afd7f82b","arrived_at":"2026-10-01T09:04:30.000000Z","check_in_id":"81856e7f-f970-449e-943c-648dbf46ec58","status":"arrived"}}]

--- DOCK-4 the appointment row
query:  SELECT status, arrived_at FROM receiving.appointment WHERE id = '6730c01e-c120-4630-8639-5ef6afd7f82b'
answer: arrived|2026-10-01 09:04:30+00

--- DOCK-5 appointment.check_in, unknown appointment
POST /appointment/check-in
request:  [{"request_id":"r9","value":{"idempotency_key":"f-checkin-absent","appointment_id":"00000000-0000-4000-8000-000000000000","arrived_at":"2026-10-01T09:04:30.000000Z"}}]
response: [{"error":{"code":"not_found","detail":{"field":"value.appointment_id","id":"00000000-0000-4000-8000-000000000000"}},"request_id":"r9"}]

--- DOCK-6 appointment.query, status scheduled
POST /appointment/query
request:  [{"request_id":"r10","dock_id":"3c1b05f5-296b-450c-9255-3cc0bc1308d9","day":"2026-10-01","status":"scheduled"}]
response: [{"request_id":"r10","value":{"appointments":[{"appointment_id":"69257de4-a950-4fa2-91ae-fc6d08da6cbf","arrived_at":null,"carrier_id":"d3296298-2ade-45d2-8670-609cf17fbd5b","dock_id":"3c1b05f5-296b-450c-9255-3cc0bc1308d9","slot_end":"2026-10-01T09:00:00.000000Z","slot_start":"2026-10-01T08:00:00.000000Z","status":"scheduled"},{"appointment_id":"55d000c7-9c0d-4589-87ac-e032b70f872d","arrived_at":null,"carrier_id":"d3296298-2ade-45d2-8670-609cf17fbd5b","dock_id":"3c1b05f5-296b-450c-9255-3cc0bc1308d9","slot_end":"2026-10-01T11:00:00.000000Z","slot_start":"2026-10-01T10:00:00.000000Z","status":"scheduled"}]}}]

--- DOCK-6 appointment.query, status arrived
POST /appointment/query
request:  [{"request_id":"r11","dock_id":"3c1b05f5-296b-450c-9255-3cc0bc1308d9","day":"2026-10-01","status":"arrived"}]
response: [{"request_id":"r11","value":{"appointments":[{"appointment_id":"6730c01e-c120-4630-8639-5ef6afd7f82b","arrived_at":"2026-10-01T09:04:30.000000Z","carrier_id":"d3296298-2ade-45d2-8670-609cf17fbd5b","dock_id":"3c1b05f5-296b-450c-9255-3cc0bc1308d9","slot_end":"2026-10-01T10:00:00.000000Z","slot_start":"2026-10-01T09:00:00.000000Z","status":"arrived"}]}}]

--- DOCK-6 appointment.query, a day with no appointments
POST /appointment/query
request:  [{"request_id":"r12","dock_id":"3c1b05f5-296b-450c-9255-3cc0bc1308d9","day":"2026-10-02","status":"scheduled"}]
response: [{"request_id":"r12","value":{"appointments":[]}}]

--- DOCK-1 overlapping pairs in the whole table
query:  SELECT count(*) FROM receiving.appointment a JOIN receiving.appointment b ON a.dock_id = b.dock_id AND a.id < b.id WHERE a.slot_start < b.slot_end AND a.slot_end > b.slot_start
answer: 0

--- appointment.check_in replayed byte for byte
POST /appointment/check-in
request:  [{"request_id":"r13","value":{"idempotency_key":"f-checkin-1","appointment_id":"6730c01e-c120-4630-8639-5ef6afd7f82b","arrived_at":"2026-10-01T09:04:30.000000Z"}}]
response: [{"request_id":"r13","value":{"appointment_id":"6730c01e-c120-4630-8639-5ef6afd7f82b","arrived_at":"2026-10-01T09:04:30.000000Z","check_in_id":"81856e7f-f970-449e-943c-648dbf46ec58","status":"arrived"}}]

--- appointment.check_in with a new key, appointment already arrived
POST /appointment/check-in
request:  [{"request_id":"r14","value":{"idempotency_key":"f-checkin-2","appointment_id":"6730c01e-c120-4630-8639-5ef6afd7f82b","arrived_at":"2026-10-01T09:30:00.000000Z"}}]
response: [{"error":{"code":"appointment_not_scheduled","detail":{"field":"value.appointment_id","id":"6730c01e-c120-4630-8639-5ef6afd7f82b"}},"request_id":"r14"}]

--- a dock of its own for the race
POST /dock/create
request:  [{"request_id":"r15","value":{"idempotency_key":"f-dock-race","name":"Door 11"}}]
dock_id=d6f6da53-2a56-4bbe-a083-2065a07f3837

--- DOCK-1 two overlapping bookings sent at the same time, different keys
POST /appointment/book  (both at once)
request a: [{"request_id":"race-a","value":{"idempotency_key":"f-race-a","carrier_id":"d3296298-2ade-45d2-8670-609cf17fbd5b","dock_id":"d6f6da53-2a56-4bbe-a083-2065a07f3837","slot_start":"2026-10-02T09:00:00.000000Z","slot_end":"2026-10-02T10:00:00.000000Z"}}]
request b: [{"request_id":"race-b","value":{"idempotency_key":"f-race-b","carrier_id":"d3296298-2ade-45d2-8670-609cf17fbd5b","dock_id":"d6f6da53-2a56-4bbe-a083-2065a07f3837","slot_start":"2026-10-02T09:30:00.000000Z","slot_end":"2026-10-02T10:30:00.000000Z"}}]
response a: [{"error":{"code":"slot_unavailable","detail":{"field":"value.slot_start"}},"request_id":"race-a"}]
response b: [{"request_id":"race-b","value":{"appointment_id":"37b232b6-34b9-446f-aa7f-32f9185dcb7b","status":"scheduled"}}]

--- DOCK-1 rows on the race dock
query:  SELECT slot_start, slot_end FROM receiving.appointment WHERE dock_id = 'd6f6da53-2a56-4bbe-a083-2065a07f3837' ORDER BY slot_start
answer: 2026-10-02 09:30:00+00|2026-10-02 10:30:00+00

--- DOCK-1 overlapping pairs in the whole table, after everything above
query:  SELECT count(*) FROM receiving.appointment a JOIN receiving.appointment b ON a.dock_id = b.dock_id AND a.id < b.id WHERE a.slot_start < b.slot_end AND a.slot_end > b.slot_start
answer: 0
```

What each block proves:

- DOCK-0. `carrier.create` returns `carrier_id` and `dock.create` returns
  `dock_id`. Every later call addresses them by those two values.
- DOCK-1. The overlapping 09:30 to 10:30 booking refuses with
  `slot_unavailable`. The adjacent 10:00 to 11:00 booking is accepted, because
  touching is not overlapping. Two overlapping bookings sent at the same moment
  under different keys produce one acceptance and one `slot_unavailable`, and
  the dock holds one row. The overlapping-pair count over the whole table is 0.
- DOCK-2. The replayed booking returns the same `appointment_id`, and the row
  count on that dock is 1 before and 1 after.
- DOCK-3. The same key with a different slot refuses with
  `idempotency_conflict`.
- DOCK-4. `appointment.check_in` answers `status: arrived` with the arrival
  time the caller supplied, and the row reads `arrived|2026-10-01 09:04:30+00`.
- DOCK-5. Check-in against an appointment that does not exist refuses with
  `not_found`, naming the field and the id.
- DOCK-6. `appointment.query` for one dock and one day, filtered by status,
  answers in slot order: 08:00 before 10:00 under `scheduled`, and the single
  09:00 appointment under `arrived`. A day with no appointments answers with an
  empty list.

## What I did not verify

- The repository's wider gates. I ran only the `dock` crate's tests and the
  loop. I did not run `cargo test --workspace`, the conformance suites, or the
  system gates, because none of them names this package and the loop is the
  proof this task asks for.
- `cargo fmt --manifest-path components/Cargo.toml -p dock -- --check` FAILS.
  See "Where I got stuck". I ran it, I saw the diff, and the environment refuses
  the formatted bytes.
- `cargo clippy`. I did not run it. The build emits thirteen warnings, all
  dead-code notes on generated row fields the operations do not read, plus the
  `REFUSALS` constants that only the tests use.
- The `departed` status is reachable in the model and its check constraint, but
  no operation moves an appointment to it. The scenario names no such operation,
  so I wrote none and exercised none.
- Behavior under a host restart, under Postgres failure, or under load beyond
  the two-request race above.
- The `retry` refusal path. It needs a first call that dies between its claim
  and its commit, and I had no way to arrange that from the route.

# Decisions

Nobody was available to answer, so each of these is mine and recorded here.

**Schema `receiving`.** `task.json` names it, and the host injects the search
path from the deployment configuration key `schema`. The guest names tables
unqualified and cannot choose its own schema, so `receiving` is the only schema
the package can own here.

**A standalone package, not an overlay.** `packages/dock/wamn.json` declares no
`base_dependencies`. The scenario shares no data with `packages/receiving`, so
a declared base creates a dependency the domain does not have. The loop
then treats `packages/receiving` as an ignored package source.

**All five operations are custom operations.** The three models declare no CRUD
operations. The generated `create` and `query` shapes exist, but the scenario
pins its own input and result field names, and a custom operation lets the
package own every statement and every refusal literal exactly.

**A dock row lock, not an exclusion constraint.** The natural guard for DOCK-1
is `EXCLUDE USING gist (dock_id WITH =, tstzrange(slot_start, slot_end) WITH &&)`.
The migration policy admits `CREATE TABLE` and additive `ALTER TABLE` only, so
`CREATE EXTENSION btree_gist` is refused, and a range column has no type in the
frozen column vocabulary. Booking therefore takes `SELECT ... FOR UPDATE` on the
dock row before it reads for an overlap. That is the serialization point, and
the race above shows it holding.

**`appointment.check_in` returns a `check_in_id`.** A command that is idempotent
by claim must pre-generate at least one identity that its result carries. The
scenario's result for check-in names `status` and `arrived_at`, neither of which
is a minted identity. The arrival is a real event with its own identity, so the
claim mints `check_in_id` and the result carries it beside the two pinned
fields. The result is a superset of the pinned contract, never a substitute.

**The day is bounded in Rust, not in SQL.** `appointment.query` takes `day` as
text and the guest turns it into two `timestamptz` bounds, `day` and the next
day, half open. A `date` cast inside the statement reads the session time
zone, and this scenario's slots are UTC.

**No `depart` operation.** The status vocabulary carries `departed` because the
scenario says status moves scheduled to arrived to departed. The pinned wire
names list five operations and none of them departs an appointment, so I wrote
none.

**Carrier and dock names are not unique.** The scenario asks for no uniqueness,
a unique constraint adds a refusal class the contract does not name.

**One refusal of my own: `appointment_not_scheduled`.** Check-in against an
appointment that already arrived is neither `not_found` nor a repeat under one
key. The scenario says any other refusal my package needs is mine to name.

**`internal_relations` for the four claim tables.** Each command keys its claim
under `idempotency_key`, stores the canonical command beside it, and
pre-generates one uuid identity. The claim relations are CDC-excluded, which the
claim law requires.

# Where I got stuck

The guest sources are not `rustfmt` formatted, and the environment will not let
me format them.

The order of events:

1. I committed the package at `3d529f85` and ran the loop. It completed through
   Activate and minted effective release 1.
2. I then ran `cargo fmt`. It rewrote six files, only wrapping long expressions.
   The tests still passed.
3. The formatting moved line numbers, which moved the `#[track_caller]` panic
   locations the compiler embeds, which changed the component bytes. The raw
   `dock.wasm` digest moved from
   `b68195b486f178e869ac4bde4d541d7262ffdeaee63dac8b6e3d8a75a049d67e` to
   `ce201d1d5e26eef9f607ef696648078d690b02c6c6479a0cce98534c4fd1f05e`.
4. The loop then refused at Admit:

```
Error: dev-stage-failed at admit: dev-stage-owner-failed while project admission into the environment: component-projection-refused (component-fact-conflict): wamn_dock@1.0.0 component=dock interface-version=0.1.0 collides with different admitted facts
```

`catalog.component_library` has
`PRIMARY KEY (tenant_id, package_id, package_version, component, interface_version)`,
so one package version admits one component digest per tenant, for ever.

A package version bump to `2.0.0` does not help. `publish_release` refuses a
second closure under one effective release id with
`effective release package membership is already frozen to another exact set`,
and `wamn dev --config` pins `effective_release_id` to 1. I read the code rather
than spending an attempt on it.

I then reverted the formatting so the tree carried the frozen bytes again. The
loop refused at Release instead:

```
Error: dev-stage-failed at release: dev-stage-owner-failed while publish the effective release manifest: deployment-attestation-content-conflict: receiving-route-auth/1 -> acme/receiving/dev: db error: ERROR: deployment-attestation-content-conflict
```

The attestation row explains it:

```
$ psql -c "select tenant_id, effective_release_id, org_id, project_id, environment, deployed_manifest_hash, source_commit from catalog.deployment_attestations"
      tenant_id       | effective_release_id | org_id | project_id | environment |                         deployed_manifest_hash                          |              source_commit               
----------------------+----------------------+--------+------------+-------------+-------------------------------------------------------------------------+------------------------------------------
 receiving-route-auth |                    1 | acme   | receiving  | dev         | sha256:b4f3fb90df4a88e495b31b815fae94034b9edc0ef5d0ec0db9aa6cb56e5abece | 3d529f85807f3d36f495cb099b6298eaecec8fd2
(1 row)
```

Effective release 1 is attested to one source commit, and a revert commit is a
different commit. So I reset the branch to `3d529f85`, the commit the running
release attests. The loop then completed through Activate again, and that is the
run this report records.

The blocker, stated plainly: in this environment effective release 1 is frozen
to the component bytes AND to the source commit of the first successful run. Any
later change to the guest, including a whitespace change, cannot be activated. I
stopped rather than reset the project environment or delete a catalog row, both
of which are working around the platform.

The cost is that `cargo fmt --manifest-path components/Cargo.toml -p dock -- --check`
reports a diff in six files. `cargo fmt --manifest-path components/Cargo.toml --all -- --check`
is a gate of record in `docs/operations/build-and-test.md`, so the tree as it
stands fails that gate for this crate. A follow-up that runs against a fresh
project environment, or a new effective release id, fixes it in one command.

The lesson for the next run of this loop: format the sources BEFORE the first
commit that reaches Publish. The first activated commit is the only one the
environment will accept.

# Rules I relied on

- `services/ctl/src/dev.rs`. `DEV_STAGE_ORDER` names the twelve stages and
  `DevStageBoundary` says Admit onward requires a committed worktree. Generate
  writes `packages/dock/generated/`, so that tree must be committed before Apply.
- `services/ctl/src/dev/config.rs`, `resolve_dev_packages`. A package source that
  the overlay does not name as a base dependency is ignored, which is what makes
  a standalone overlay legal.
- `tools/build-components`. A package's own `wamn.json` names its components, so
  adding `packages/dock` and the crate `dock` adds the component with no edit to
  `architecture/workspace-tiers.json`. The count checks balance because the crate
  and the package arrive together.
- `crates/schema/generator/src/manifest.rs`. The closed manifest vocabulary,
  `validate_operation_vocabulary`, and the rule that a command declares
  `idempotent_by` as exactly one of claim, state or inherited.
- `crates/schema/generator/src/generate.rs`, `require_claim_relation` and
  `require_pre_generated_identity`. A claim keys `idempotency_key` under a
  primary key alone, carries a non-null `canonical_command` of type bytes, and
  pre-generates at least one unique non-null uuid defaulting to
  `gen_random_uuid()`.
- `crates/schema/generator/src/generate.rs`,
  `validate_static_sql_relation_access`. The declared relation privileges must
  equal what the lexer derives from the authored SQL, field for field, including
  the row lock.
- `crates/schema/introspection/src/migration_policy.rs`. Migrations admit only
  schema-qualified `CREATE TABLE` and additive `ALTER TABLE`, every constraint is
  named, and the name must follow `<table>_<columns>_<kind>`.
- `crates/schema/introspection/src/ir.rs`, `ColumnType`. The ten admitted column
  types. There is no `date` and no range type, which decided the `day` input.
- `components/data/wms-data` and `components/data/receiving-data`. The shipped
  shape of an operation module: parse and re-spell, canonicalize, find a replay,
  claim, work, finalize, and translate a statement failure exactly once.
- `crates/platform/runtime/src/plugins/wamn_postgres/mod.rs` and
  `services/ctl/src/dev/activation.rs`. The host injects `wamn.schema` from the
  deployment configuration, so authored SQL names relations unqualified.
- `services/ctl/src/publish_release.rs`,
  `validate_attachment_definition_hashes`. An attachment's `definition-hash` must
  equal the canonical JSON sha256 of its own `definition`, and the definition
  must not author `route.host`.
- `AGENTS.md`. Simplicity first, surgical changes, and the Rust convention that
  a repository-defined error enum is the contract at the wire boundary while
  implementations translate exactly once at the owning boundary.

# Open questions

1. Is the freeze of effective release 1 to one source commit intended for a
   development loop? Every code change after the first activation is
   unactivatable in the same environment. If the intent is that `wamn dev` runs
   against a disposable environment, then the pilot's durable project-environment
   database and its pinned `effective_release_id` make the loop single shot. I
   found no operator verb that mints the next effective release id from the
   development configuration.

2. `appointment.check_in` had to invent an identity to satisfy the claim law,
   because a command idempotent by claim must pre-generate at least one identity
   the result carries. A command that only moves a state forward has none. Is
   `idempotent_by: state` meant to cover that case? Its guard names an input
   field carrying an expected row version, and this scenario's pinned input for
   check-in has no version field.

3. `appointment.query` returns `appointments` as a named member, and its result
   contract declares the entries with `appointments[]` paths under
   `bounded_list`. `receiving.load_receipt_screen` declares flat per-row fields
   instead. I found no rule that decides between the two, so I chose the
   one the scenario's wording names.

4. The generated row structs produce dead-code warnings for fields an operation
   does not read. `packages/wms` has the same warnings. Is a package expected to
   read every column it declares in a statement row, or is the warning accepted?
