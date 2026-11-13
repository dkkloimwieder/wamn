# 14 wamn-runtime, part 5: run queue, run admission, resolution

Nothing was built or run. Every statement is from reading the code unless marked *inference*.

## What was read

| File (under `crates/platform/runtime/src/plugins/wamn_postgres/`) | Lines without tests |
| --- | --- |
| `production_claim.rs` | 1,601 |
| `event_admission.rs` | 392 |
| `event_batch.rs` | 290 |
| `workflow_resolution.rs` | 430 |
| `wiring_resolution.rs` | 398 |
| `file_collection.rs` | 58 |

Also read for callers: `crates/execution/workflow/src/queue.rs` (the claim loop), `wiring_lowering.rs` (the lines that read the resolved wiring), `crates/execution/run-state/src/queue/sql.rs::select_production_claim_sql`.

## One purpose each

| File | Purpose | Callers |
| --- | --- | --- |
| `production_claim.rs` | Claim, renew, release, complete and reap queued runs | `execution/workflow/src/queue.rs` only |
| `event_admission.rs` | Admit manual and schedule runs. The name says event; it holds no event code. | `execution/host/src/application_route.rs`, `execution/workflow/src/schedule.rs` |
| `event_batch.rs` | Admit the runs of one change event | `wamn_jetstream` plugin |
| `workflow_resolution.rs` | Read a workflow version and resolve its nodes in a release | `execution/workflow`, `execution/host` |
| `wiring_resolution.rs` | Four things: release components, connection requirement reads, the connection map of a release, readiness | four different callers |
| `file_collection.rs` | One lock and one read for the file collector | `execution/host/src/file_collector.rs` |

None of this code serves a component. It is host work that uses the plugin to get a connection.

## Rulings that land here

| Ruling | What goes |
| --- | --- |
| Durable caller deleted (part 1) | The caller block in `production_claim.rs::complete_in_transaction` (70 lines), `decode_caller_release` (45), the field `durable_caller_attached` |
| Candidate and service principal deleted (part 1) | `ProductionCandidate`, the fields `candidate` and `service_principal_id`, the candidate arm of `decode_selected_claim`, the line \`release.filter( |
| Flow columns dropped (tab 09) | `ExhaustedExecutionIdentity::Flow`, the flow arms in `reap_in_transaction`, `generic_failure_outcome`, `decode_selected_claim` |
| `wamn-pg-core` deleted (tab 06) | `event_batch.rs::history_actor` uses its quoting three times |

**Two more facts for these rulings**

- `router_caller_attached` is always false. `select_production_claim_sql` sets it true only for the trigger sources `http`, `internal`, `studio`, `scenario-draft`, `test-case`. Production inserts `event`, `manual`, `schedule` only. So this field, its SQL expression and the value `queue.rs` passes on are a constant. They go with the part 1 ruling.
- A claimed run with no workflow columns is failed at once (`queue.rs::drain_one`). So `workflow: Option<Box<ProductionWorkflow>>` can be required, and the decoder refuses a row without it.

Estimate for this block: 300 lines in `production_claim.rs`.

## Findings, each with an option

### Single truth

**1. Fourteen functions open a transaction by hand.** Seven in `production_claim.rs` (`claim_next`, `reap_exhausted`, `renew`, `complete`, `record_deadline_adjustments`, `release_claim`, `pinned_backlog`) repeat the same 45 lines: read env\_key, read runner, read five more claims, check out, begin with twelve arguments, map three errors. Three admissions and three functions in `wiring_resolution.rs` repeat a shorter form. `workflow_resolution.rs::executor_read` is the fourteenth and is already a helper.

- They end in three different ways: commit and rollback written inline, `finish_queue_transaction`, `finish_guarded_queue_transaction`.
- Option: one function, "run this work in an executor transaction", with a read-only flag. `executor_read` becomes that function. Estimate: 450 lines less.

**2. Three run admissions repeat the same steps.** `admit_manual_run`, `admit_schedule_run`, `admit_event_batch` each: read the durability value, build the node digest JSON, insert the run, insert the queue row, and on a repeat read the first run.

- The durability SQL is a constant in `event_admission.rs` and a literal in `event_batch.rs`.
- The node digest JSON is built twice here with the same eight lines.
- Option: one `admit_run` for insert, queue and repeat. One function for the node digests. One durability read. This is where the tab 09 ruling (one run-admission builder) lands.
- Open, for the run table tab: tab 09 deleted the Rust durability type. Whether the column and this read stay is decided there.

**3. The components of a release are obtained in two ways.**

- In Rust: read `canonical_bytes`, parse with `ServingManifest::from_canonical_bytes` (`workflow_resolution.rs::resolve_workflow`, `event_admission.rs::admit_schedule_run`).
- In SQL: `wiring_resolution.rs::RELEASE_COMPONENTS_SQL`, 38 lines that rebuild each component as JSON from the same bytes, then `decode_release_components` parses that JSON and checks a count.
- Option: one way, in Rust. `RELEASE_COMPONENTS_SQL`, `decode_release_components` and the count check go (about 70 lines).

**4. The schedule admission reads the release from the database on every tick.** `admit_schedule_run` reads and parses the manifest by digest. Its doc comment calls it "the release the host serves". `admit_event_batch` takes the same release from host memory (`LoadedRelease`).

- Option: `admit_schedule_run` takes the loaded release like the event path. One read and one parse per tick go.
- *Inference:* `execution/workflow/src/schedule.rs` holds the loaded release. Confirm when that crate is read.

**5. The event trigger query is written twice.** `event_batch.rs::ENABLED_EVENTS_SQL` and the literal in `event_batch.rs::event_triggers` select the same rows; the second leaves out four columns.

- Option: one query.

**6. Error helpers named `storage` and `contract` are defined three times.** `production_claim.rs`, `event_batch.rs` (same names, different signatures), `write_log.rs`.

- Option: one set, as constructors on `ProductionClaimError`.

### Dead code

**7. `RunStore` is a trait with one implementation.** `wamn_run_state::run_store::RunStore` is implemented by `WamnPostgres` only. Its two users import it only to call the methods (`use ... RunStore as _`). It has an associated type that exists for the candidate claim.

- Option: delete the trait. The methods become plain methods.

**8. `wiring_resolution.rs::declared_connection_names` has no caller.**

- Option: delete.

**9. `ResolvedActiveWiring.package_version` is always empty.** The one constructor passes `String::new()`. `execution/workflow/src/wiring_lowering.rs` passes the field to both sides of a comparison, so `PackageVersionMismatch` cannot occur. `DecodedWiring` is a struct with one user.

- Option: delete the field, the comparison, the error value and `DecodedWiring`.

**10. `RELEASE_COMPONENTS_SQL` is public for one test.** `runtime/tests/executor_platform_surface_live.rs` runs it by hand.

- Option: goes with finding 3.

### Cost per call

**11. Every executor transaction starts with a query that asks if the login is an executor.** `production_claim.rs::require_executor_authority` has twelve call sites. The pool was opened with the executor login, and the connection check already confirmed its role (tab 11 finding 14).

- Option A: delete it and its test. The claim loop saves one round trip per turn.
- Option B: keep it. It catches a role revoked while the host runs.
- With A, a revoked role still fails: Postgres refuses the next statement for lack of grants.

**12. Effect projections are checked again on every run resolution.** `wiring_resolution.rs::verify_served_effect_projections` runs in `resolved_wiring` (each claimed run) and in `resolve_release_components`. The comment gives old rows as the reason. The components come from release bytes that are matched to a digest.

- Option A: check once when a release is loaded.
- Option B: keep.
- *Inference:* the check is pure and its cost is small. Low priority.

### Single responsibility

**13. Three reads run without a transaction and without claims.** `wiring_resolution.rs::declared_connection_interfaces`, `declared_connection_names` and `release_fact` check out a connection and query at once. Every other function here sets the env\_key claim first.

- Option: they use the one function from finding 1. By the tab 11 ruling (option A) the connection reads move to their owners, `integrations.rs` and `connection_facts.rs`.
- Not checked: whether row security on these tables needs the claim.

**14. `event_batch.rs::history_actor` knows the layout of generated history tables.** Sixty lines look through every schema for `wamn_entities`, build table names by adding `_history`, and build SQL text. The generator defines those tables.

- Option: carried to the generator tab. The question there: one definition of the history table name and key.

**15. Where the queue code lives.** `production_claim.rs` has one caller, `execution/workflow/src/queue.rs`. `wamn-workflow` already depends on `wamn-runtime`, so the direction allows a move.

- Option A: after finding 1, move `production_claim.rs` to `wamn-workflow`. The plugin keeps the one transaction function.
- Option B: leave it. Rename `event_admission.rs` to what it holds.
- The admissions and resolution cannot move the same way: `execution/host` calls them and does not depend on `wamn-workflow`.

**16. A development branch sits inside three production functions.** `resolve_workflow`, `resolve_release_components` and `release_component_bindings_ready` each begin with `if let Some(local) = &self.local_application`.

- Carried to the part that reads `local_application.rs`. No option yet.

## Tests that go or change

**Delete**

| Test | File | Goes with |
| --- | --- | --- |
| `candidate_janitor_failure_body_names_the_frozen_wiring`, `candidate_projection_is_exactly_what_the_decoder_indexes` | `production_claim.rs` | candidate ruling |
| `executor_authority_uses_current_user_for_each_operation` | `production_claim.rs` | 11, option A |
| `component_facts_come_from_the_release_descriptors` | `wiring_resolution.rs` | 3 |
| The part that runs `RELEASE_COMPONENTS_SQL` | `runtime/tests/executor_platform_surface_live.rs` | 3, 10 |

**Change**

| Test | File | Why |
| --- | --- | --- |
| `janitor_failure_body_uses_host_jcs_not_database_json_text`, `generic_refusal_body_is_exact_and_message_free` | `production_claim.rs` | flow arm gone |
| Sites that match `ProductionClaimResult::Ready` | `runtime/tests/production_claim_live.rs`, `executor_platform_surface_live.rs`, `execution/workflow/src/queue/automation_live/` | fields removed |
| Sites that import `RunStore` | `execution/workflow/src/queue.rs`, `automation_live/shutdown.rs` | 7 |

The other 7 tests in `production_claim.rs`, 3 in `workflow_resolution.rs`, 4 in `wiring_resolution.rs` and 1 each in the two admission files stay.

## Not read

- Test bodies.
- `local_application.rs`.
- `execution/workflow/src/schedule.rs` beyond the call line.

## Rulings (2026-11-11)

- Findings 1 to 10 and 13: accepted.
- Finding 11: **option A.** `require_executor_authority` and its test go.
- Finding 12: **option A.** Effect projections are checked once, when a release is loaded.
- Finding 15: **option A.** `production_claim.rs` moves to `wamn-workflow` after finding 1. The plugin keeps the one transaction function.
- Findings 14 and 16: carried, as written.
- Tab 14 is fully ruled.
