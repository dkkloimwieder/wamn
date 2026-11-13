# 18 wamn-runtime, part 9: events, guest logging, effect spans

Nothing was built or run. Every statement is from reading the code. **Fact** = read in the code. **Inference** = my conclusion.

## Files read

| File | Lines | Non-test lines |
| --- | --- | --- |
| `plugins/wamn_jetstream.rs` | 1,618 | 1,058 |
| `plugins/wamn_jetstream/admission.rs` | 254 | 254 |
| `plugins/wamn_logging.rs` | 855 | 722 |
| `plugins/effect_span.rs` | 689 | 473 |
| Total | 3,416 | 2,507 |

Callers read: `services/host/src/host.rs` (plugin setup, event admission task), `crates/execution/workflow/src/queue.rs`, `crates/execution/host/src/router_delivery.rs`, `crates/execution/host/src/operation/native_policy.rs` (bind and release of a pooled instance), `crates/control/environment/src/environment/ensure.rs` (host env), `wamn_postgres/event_batch.rs::require_event_materializer`, `wamn_postgres/resources.rs` (span), every `effect_span!` call.

## One purpose each

| File | Purpose | Verdict |
| --- | --- | --- |
| `wamn_jetstream.rs` | Event plugin: check registrations, publish derived events | Also keeps a per-component claim map it does not need |
| `admission.rs` | Route source events to application consumers; admit them | Also declares a consumer |
| `wamn_logging.rs` | Guest log capture | Carries bench controls and a test window in production code |
| `effect_span.rs` | One span vocabulary for effects | One purpose. One dead field |

## Findings: events

### 1. The derived-publish scope is one value per host, kept in a per-component map

- **Fact.** `bind_derived_scope` has two callers: `queue.rs` and `router_delivery.rs`. Each passes a fixed id and the same three values from the loaded release (env\_key, project, environment).
- **Fact.** `revoke_derived_scope` has no caller. Its comment says the driver revokes at check-in. It does not.
- **Option.** The plugin takes the host scope once, at construction. Delete `bind_derived_scope`, `revoke_derived_scope`, `required_derived_claim`. `DerivedPublishRequest.component_id` stays only as a span label.

### 2. The rest of the claim map only labels spans

- **Fact.** After finding 1, `claims` serves one use: env\_key and project on the spans of the three registration calls of the materializer.
- **Fact.** Each of those calls already passes `require_event_materializer`, which checks the component's env\_key in the Postgres plugin.
- **Fact.** The file has no unbind handler. Entries set at workload bind are never removed. (Answers the question carried from tab 13.)
- **Option.** Delete `claims`, `JetstreamClaim`, `set_claim`, `claim_for` and the `wamn.environment` config read. Spans take the host scope. No unbind handler is needed then.

### 3. The event coordinates repeat values the host already has

- **Fact.** `WamnJetstreamConfig::from_env` reads `WAMN_EVT_ORG`, `WAMN_EVT_PROJECT`, `WAMN_EVT_ENV`. `ensure.rs` sets them from the same environment document that gives the host its org, project and environment.
- **Fact.** `admit_application_events` then checks that the release scope equals these coordinates.
- **Fact.** `WamnJetstream::from_env` has test callers only.
- **Option.** The host passes its own org, project and release environment. Delete the three variables (runtime, `ensure.rs`, `runtime.rs` list, two `deploy/gcp` files), `WamnJetstream::from_env` and the equality check. `WAMN_EVT_STREAM_REPLICAS` and `WAMN_EVT_DUP_WINDOW_SECS` stay: they are declarations, not identity.
- This is one more spelling for the carried "environment spelled five ways" list.

### 4. The same token check is written three times, and runs on every call

- **Fact.** `value.is_empty() || value.trim() != value || subject_token(value) != value` is in `require_event_coordinates`, `bind_derived_scope` and `required_derived_claim`. `require_event_coordinates` runs in four call paths.
- **Option.** One function, called once at construction. With findings 1 and 3 it has one call site.

### 5. The lazy connection is never lazy in production

- **Fact.** `host.rs` calls `activate_events` at start and stops on an error. After that the stored context is always present, or the plugin has no NATS client.
- **Fact.** Every later call still locks `Mutex<Option<Context>>` and fetches the stream again by name.
- **Option.** One async constructor checks the streams and stores the context and the source stream. Delete `ensure_ctx`, `activate_events`, the mutex and the per-call `get_stream`.

### 6. The subject grammar is spelled by hand in three places

- **Fact.** `wamn_event_wire::subject` owns `evt.<org>.<project>.<env>.<entity>.<op>`. `require_registration` builds the prefix with `format!`. `application_consumer` builds the filter with `format!`. `subject_source` parses the same shape.
- **Option.** `wamn-event-wire` gets the three functions (prefix, entity filter, parse). The three hand spellings go.

### 7. A consumer is declared in the runtime

- **Fact.** Stream and consumer declarations live in `wamn_control_provision::events`. `admission.rs::application_consumer` declares the application consumer here: durable name format, `max_deliver = -1`, `max_waiting = 64`.
- **Option.** Move `application_consumer` next to the other declarations. `admission.rs` keeps only the calls.

### 8. Repeated lead-in code

- **Fact.** The three WIT handlers each repeat: get plugins, env\_key, materializer check, claim, span. `prepare` carries its own copy of `event_plugins`.
- **Fact.** `route_event` and `admit_application_events` each repeat: read triggers, context, stream name, stream.
- **Fact.** `prepare_consumer` searches the trigger list twice for the same trigger.
- **Option.** One lead-in function for the handlers. `require_registration` returns the trigger it found. With finding 5 the stream lead-in goes.

### 9. Spans and durations are uneven

- **Fact.** Duration is recorded for `publish-derived` and `prepare-registration`, not for `event-triggers` and `route-event`.
- **Fact.** No call here uses `EffectOutcomeGuard`, so `effect.outcome` is empty on every `wamn.jetstream` span.
- **Option.** One wrapper runs a call under its span, sets the outcome and records the duration. All four calls use it.

## Findings: guest logging

### 10. The context fields have no sender

- **Fact.** `ParsedContext` reads `flow`, `run`, `node`, `seq`, `run_label`, `traceparent`, `tracestate` from the guest's context string.
- **Fact.** No guest, SDK or generator in the repository calls `wasi:logging`. Only two test guests do. `run_label` appears in no other file.
- **Fact.** The host already attests wiring, node and run for each pooled instance (`native_policy.rs` binds them to the other plugins).
- **Option A (recommended now).** Delete `run_label` and `seq`.
- **Option B (with the held scope record, tab 15 finding 2).** Take wiring, node, run and trace from the host-bound record. Pass the guest context through as one plain attribute. Delete `ParsedContext` and `parse_w3c_traceparent`.

### 11. Bench controls and a test window in production code

- **Fact.** `drain_rate_per_sec` (`WAMN_LOG_DRAIN_RATE`) slows the drain for a saturation demonstration. No file in `deploy/` or `docs/` sets any `WAMN_LOG_*` variable.
- **Fact.** `force_flush` has no caller. The `linker_entries` and `scope_registrations` counters of this plugin have no reader.
- **Fact.** `Capture`, `CapturedRec`, `new_with_capture`, `add_to_linker`, `claim_snapshot`, `accepted`, `emitted`, `dropped` have test callers only.
- **Option.** Delete the drain rate, `force_flush`, the two counters with their getters, and the five `WAMN_LOG_*` reads (the defaults become the values). Put the capture types, `add_to_linker`, `claim_snapshot` and the three getters behind `test-util`.

### 12. Third copy of the claim map and of the config key names

- **Fact.** `wamn_logging.rs` defines `ENV_KEY_CONFIG_KEY` and `PROJECT_CONFIG_KEY` again. Tab 11 ruled: defined once.
- **Fact.** It keeps its own map component id → (env\_key, project). The pooled path clears it (`native_policy.rs`). The workload path has no unbind handler, so those entries stay. (Answers the question carried from tab 13.)
- **Fact.** A missing claim gives three behaviours: logging writes `unregistered`, events used empty strings, Postgres refuses.
- **Option.** Use the one definition of the key names now. The map joins the held scope record (tab 15 finding 2); I added this file to that held row.

### 13. The log provider is never shut down

- **Fact.** The plugin owns an `SdkLoggerProvider` with a 200 ms batch delay. Nothing calls `shutdown` or `force_flush` on it.
- **Inference.** Records in the last batch can be lost when the host stops.
- **Option.** `Drop` for `WamnLogging` shuts the provider down.

### 14. File headers describe things that no longer exist

- **Fact.** The `wamn_logging.rs` header describes the "S5" bench, `logbench` and a runner. None exists in the repository.
- **Fact.** `effect_span.rs` and `resources.rs` cite `tests/integration/src/metricbench.rs` and `docs/archive/observability/dashboards.md`. Neither file exists.
- **Option.** Replace both headers with what the code does now. Remove the dead citations.

## Findings: effect spans

### 15. `wamn.requirement` is always empty

- **Fact.** `EffectRun` has one user, `wamn_postgres/resources.rs`, and it passes `requirement: ""`. Every other surface passes `None`.
- **Option.** Delete `EffectRun` and the `wamn.requirement` field. The macro takes the run id directly.

## Fine as is

- `effect_span!` as a macro: `tracing` needs the span name as a literal.
- `EffectOutcomeGuard` and `EffectEvidence`: used by HTTP and blob store; small and clear.
- The three histograms live in `effect_span.rs` because a repository lint forbids statics in `connection_http.rs`.
- `publish_derived` waits for the server acknowledgement and checks the stream name.

## Carried

- `wamn-runtime` depends on `wamn-control-provision` (declarations, SQL, role names) and `wamn-materializer`. Platform code depends on control code. To the dependency summary.
- `host.rs` detects OpenTelemetry by "any variable starts with `OTEL_`" here and has its own observability setup. To the host tab.

## Tests

**Delete**

- `wamn_jetstream.rs::derived_publication_refuses_an_unbound_or_partial_scope` (finding 1)
- `wamn_jetstream.rs::config_from_env_has_no_connection` (finding 3)
- With finding 10 option B only: `wamn_logging.rs::parse_w3c_traceparent_valid_absent_garbage`, `traceparent_sets_trace_id_on_record`

**Change**

- `wamn_jetstream.rs::derived_publication_uses_only_the_bound_scope_and_admitted_selector`, `derived_publication_span_carries_the_wiring_and_node_it_published_for`, `two_derived_publications_name_their_own_nodes`: scope comes from construction (findings 1, 2)
- `wamn_jetstream.rs::derived_publication_refuses_missing_or_foreign_event_coordinates_before_connect`: construction refuses (findings 3, 4)
- `wamn_jetstream.rs::activation_requires_declared_stream_limits_separate_from_workload_instances`: the constructor is the subject (finding 5)
- `wamn_jetstream.rs::registration_preparation_requires_the_exact_environment_source`: uses the `wamn-event-wire` functions (findings 6, 8)
- Setup only, `WamnJetstream::from_env()` → `new` (finding 3): `crates/control/environment/tests/workflow_manual_run_live.rs`, `tests/support/schedule_fixture.rs`, `crates/execution/workflow/src/queue/automation_live.rs`, `crates/execution/host/tests/files_live.rs`
- `crates/control/environment/src/event_advisories.rs` tests and `tests/workflow_event_nats.rs`: `activate_events` → constructor (finding 5)
- `wamn_logging.rs::ingest_enriches_from_host_claim_over_guest_context`: drops `seq` and `run_label` (finding 10)
- `wamn_postgres/resources.rs` span literal test: drops `wamn.requirement` (finding 15)

**Move**

- The parts of `registration_preparation_requires_the_exact_environment_source` that test subject parsing move to `wamn-event-wire` (finding 6).

## Not read

- Test bodies of the four files: names and call sites only.
- `wamn_control_provision::events`, `wamn_event_wire`, `wamn_materializer::workflow`: signatures only.
- `host.rs::serve_application_events`: call site only.
- wash-runtime's native `TracingLogger`: not read. So I offer no option to replace this plugin with it.

## Rulings

None yet.

**Ruled 2026-11-11**

- Findings 1 to 9, 11 to 15: accepted.
- Finding 10: **option A.** Delete `run_label` and `seq`. Option B stays with the held scope record (tab 15 finding 2).
- Tab 18 is fully ruled.
