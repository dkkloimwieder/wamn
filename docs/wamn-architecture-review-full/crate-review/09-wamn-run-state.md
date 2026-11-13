# 09 `wamn-run-state`

Path `crates/execution/run-state`. 24 source files, 4,264 lines; four test files, 2,477 lines. One workspace dependency, `wamn-execution-contract`. Nine crates depend on it. Every non-test module was read in full, except `schema_drift.rs` and `intent_cases.rs`, which are compiled only for tests and were read by outline. Every public symbol's callers were searched. Nothing was built or run.

Note on the measured size (fact): the 3,641 non-test lines in `crates.json` include those two test-only modules, 639 lines.

## One purpose, by its own header

The durable life of a queued workflow run: admit it, claim it under a lease, renew, complete or fail it, and reap it when its retries are spent. The crate holds the SQL text and the result types; the runtime's Postgres plugin runs the SQL. (fact)

That purpose is about 1,300 of its lines. The rest is five other things.

## What it holds

Callers are crates outside this one, test code excluded.

| Part | Files | Non-test lines | Callers | Finding |
| --- | --- | --- | --- | --- |
| Run queue SQL and result types | `queue/sql.rs`, `queue/workflow.rs`, `transitions.rs`, `sql.rs`, `run_store.rs`, `status.rs` (`RunStatus`, `FailType`) | about 1,300 | `platform/runtime` (runs it), `execution/workflow`, `control/environment` (prune) | The stated purpose. |
| A Rust model of the queue rules | `queue/model.rs`, `queue/claim.rs`, `queue/lease.rs`, `queue/janitor.rs`, `queue/evt.rs`, `invariants.rs` | about 560 | None, except `classify_production_claim` (a `bool` to a two-value enum) and `MAX_RETRY_BACKOFF_MS` | `QueueEntry`, `claim_state`, `plan_claim`, `janitor_verdict`, `lease_live`, `should_renew`, `invariants::check`, `mint_evt_run_id` are called only by this crate's own tests. |
| Trusted invocation context | `invocation_context.rs` | 377 | None | No file outside the crate names `TrustedInvocationContext`, `AdmittedPrincipal` or `HttpEffectPrincipal`. |
| Redaction | `redaction.rs` | 86 | None | Its comment says the one intended user is a future piece of work. |
| Per-call intent record | `intent_store.rs`, `operator_action.rs`, `intent_cases.rs` (tests) | about 200, plus 346 test-only | `platform/runtime` `write_log.rs`, `platform/engine`, `run-state-sqlite`, `services/edge` | A second store trait, for operation calls. It has no lease and no queue. |
| Credentials and roles | `authority_class.rs`, `credential_generation.rs`, `tenant_scope.rs` | about 190 | `CredentialGeneration`: 84 files in eight crates. `AuthorityClass`: 32 files. `app_scope_hash`: `control/provision`, `platform/runtime` | Not run state. The comment in `authority_class.rs` says it is here because this is a crate both the runtime and the provisioner already depend on. |
| Node statuses | `status.rs` (`NodeRunStatus`, `NodeErrorType`, the `From<&NodeError>`) | about 70 | None | The only use of the dependency on `wamn-execution-contract`. |
| `DurabilityClass` | `durability.rs` | 43 | None. Callers use the type of the same name in `wamn-control-registry`. | A second definition of a one-value enum. |
| Stand-in schema check | `schema_drift.rs` | 293, test-only | `tests/conformance` | Compares test copies of the table definitions with `deploy/sql`. |

## Findings

| # | Finding | Mark |
| --- | --- | --- |
| 1 | About 1,130 lines have no production caller: the queue model (560), the invocation context (377), redaction (86), the node statuses (70), `DurabilityClass` (43). Add `insert_automation_run_sql` and `select_automation_run_sql`, which have no caller at all. | fact, by search |
| 2 | The queue rules are stated twice: once in SQL, which runs, and once in Rust, which only tests run. The crate header says of itself that the Rust tests cannot check the statement, and names a real fault that passed every one of them. | fact |
| 3 | The crate has two unrelated store traits. `RunStore` is the workflow run queue. `IntentStore` is the record of one operation call. `intent_store.rs` says the cloud keeps no intent record; `runtime/src/plugins/wamn_postgres/write_log.rs` implements `IntentStore` twice. | fact |
| 4 | The credential items are a third purpose. `AuthorityClass::acl_role` repeats five role names that `control/provision` `WorkloadRoleFamily` also holds; its comment says so and names a test that keeps them equal. | fact |
| 5 | The rule "a caller is waiting" is the text `trigger_source IN ('http','internal','studio')`, written four times in SQL strings (`queue/sql.rs` three, `transitions.rs` one). No Rust item defines the trigger sources. | fact |
| 6 | `sql.rs` says status values are taken from `RunStatus` so the SQL cannot drift. `transitions.rs::FENCED_PREFIX` and `queue/sql.rs::release_production_claim_sql` write `'completed'`, `'failed'`, `'dispatched'`, `'running'` by hand. | fact |
| 7 | Four builders that admit a run (`insert_automation_run_sql`, `insert_event_run_sql`, `insert_manual_run_sql`, `insert_workflow_schedule_run_sql`) are the same statement with a different trigger and column subset, and four matching `select_*` builders. | fact |
| 8 | The same 12-line header paragraph on what pure tests cannot cover is pasted in `lib.rs`, in `queue/mod.rs`, and in `wamn-pg-core`. | fact |
| 9 | `Cargo.toml` and the header call the default build pure and safe for guests. No guest component depends on this crate. | fact |
| 10 | The claim SQL still reads `flow_id` and `flow_version`. Tab 01 found the flow language retired. | fact for the columns. Inference: they are leftovers. Not checked against the table definition or the runtime. |

SQL (carried finding): this crate is close to a single source for SQL on `runs` and `run_queue`. A search finds eight statements on those tables outside it, in four files (`runtime` `production_claim.rs` 4, `provision` `sql.rs` 2, `ctl` `ops_verbs.rs` 1, `environment` `prune_run_history.rs` 1). (fact for the search)

## Earlier rulings that meet this crate

Not decided again here.

- Credentials (Architecture tab, rules row 7): one Secret per role, and the second login generation is dropped. So `CredentialGeneration` goes, in 84 files.
- Tab 01: `node_contract.rs` is deleted, so the `From<&NodeError>` here goes with it, and so does this crate's one dependency.
- Workflow run rulings of 2026-10-28 (retry with backoff, the effect-uncertain path deleted): nothing below changes them.

## Options

| # | For | Option | What goes | What is lost |
| --- | --- | --- | --- | --- |
| 1 | findings 1, 9 | Delete what has no caller | `invocation_context.rs`, `redaction.rs`, `NodeRunStatus`, `NodeErrorType`, `durability.rs`, the two automation statements, `queue/evt.rs` | Nothing in production. Redaction was kept for work not yet started. |
| 2 | findings 1, 2 | Delete the Rust model of the queue | `queue/model.rs`, `queue/lease.rs`, `queue/janitor.rs`, `invariants.rs`, most of `queue/claim.rs`, `tests/queue.rs`. `classify_production_claim` becomes the `bool` it wraps. | Unit tests of rules the production path never runs through this code. The tests that run the real statements stay (`tests/run_state_live.rs`, `tests/admission_live.rs`). |
| 3 | finding 3 | The intent record leaves this crate. It joins its SQLite implementation: `wamn-run-state-sqlite` holds nothing else. | `intent_store.rs`, `operator_action.rs`, `intent_cases.rs` leave | Nothing. The name of the joined crate is the owner's choice; it is about operation calls, not runs. |
| 4 | finding 4 | The credential items leave this crate | `authority_class.rs`, `credential_generation.rs`, `tenant_scope.rs`, the `tenant-key` feature | Nothing here. Where they go is decided with `control/provision`, under the credentials and role rulings. The runtime cannot depend on the provisioner, so they need a home under both. |
| 5 | findings 5, 6 | One definition of the trigger sources and of "a caller is waiting"; every status value in SQL comes from `RunStatus` | Four copies of one rule; the hand-written status values | Nothing |
| 6 | finding 7 | One builder for admitting a run, with the trigger as its argument | Three of four `insert_*` and three of four `select_*` builders | Each trigger's column list is no longer readable on its own |
| 7 | finding 8 | One copy of the header paragraph | Two pasted copies | Nothing |

After options 1 to 4 the crate is the run queue only: about 1,300 lines of SQL text, result types and the `RunStore` trait.

Finding 10 has no option. It needs the table definition and the runtime read.

## Rulings (2026-11-11)

Owner ruling: accept all. The preference is always for removal.

| # | Ruling |
| --- | --- |
| 1 | Deleted: `invocation_context.rs`, `redaction.rs`, `NodeRunStatus`, `NodeErrorType`, `durability.rs`, the two automation statements, `queue/evt.rs`. |
| 2 | Deleted: the Rust model of the queue, `invariants.rs` and `tests/queue.rs`. `classify_production_claim` becomes the `bool` it wraps. |
| 3 | The intent record leaves and joins its SQLite implementation. |
| 4 | The credential items leave this crate. Their home is decided with `control/provision`. |
| 5 | One definition of the trigger sources and of "a caller is waiting". Every status value in SQL comes from `RunStatus`. |
| 6 | One builder for admitting a run. |
| 7 | One copy of the header paragraph. |

What remains: the run queue. SQL text, result types and the `RunStore` trait, about 1,300 lines, with no dependency on another workspace crate.

Still open, outside this crate: nothing. `CredentialGeneration` goes (rules row 7, Architecture tab), and so do the `flow_id` and `flow_version` columns of finding 10 (ruled 2026-11-11).

### Two further rulings (2026-11-11)

Owner ruling: drop the extra Secret per role; a single one with a host restart is fine for now. Drop the flow columns. The preference is always for removal of old code.

| Ruling | What goes |
| --- | --- |
| One login per role. A changed Secret restarts the host. | `CredentialGeneration` and every use of it, in 84 files in eight crates. This also settles rules row 7 of the Architecture tab. |
| The flow columns go. | `flow_id` and `flow_version` on `wamn_run.runs`; the first arm of `runs_execution_grain_check`; the index `runs_flow`; their two lines in the update guard; the projections in `queue/sql.rs`; in the runtime's `production_claim.rs`, the legacy arm of the candidate decode and `ExhaustedExecutionIdentity::Flow`; the copy of the constraint text in `schema/control/src/run_plane/declarations.rs`. |

What the read for this ruling showed (fact): `deploy/sql/run-state.sql` calls the two columns the legacy execution grain and says new admissions leave both NULL. No production code writes them. The one write found is in `execution/workflow/src/queue/automation_live`, a test-only module.

### A finding to verify with the runtime

Production code writes three trigger sources: `event`, `manual` and `schedule` (the builders in `queue/workflow.rs`; the `automation` pair is deleted under ruling 1). A search of non-test Rust and of `deploy/sql` finds no code that writes `http`, `internal`, `studio`, `scenario-draft` or `test-case`. (fact for the search)

The rule "a caller is waiting" is `trigger_source IN ('http','internal','studio')`. If nothing writes those three values, then nothing ever waits, and the caller-release path has no run to act on: `transitions.rs::release_caller_sql`, `CallerReleaseResult`, `StoredCallerOutcome`, `ProductionCallerOutcome`, the `caller-unreleased` refusal in `terminalize_sql`, and the seven `caller_*` columns. (inference. It needs the runtime's `production_claim.rs` and the host's route code read before any option is written.)

## Not read

- The four test files, beyond test names in `tests/queue.rs`.
- `schema_drift.rs` and `intent_cases.rs`, beyond their outlines.
- `deploy/sql/run-state.sql` and `run-queue.sql` for this tab.
- The runtime's `production_claim.rs`, which runs this SQL, beyond its imports.

## Tests deleted, changed or moved (added 2026-11-12)

Checked at `epic/dep7` (b94833fd) by grep. Test names are fact; which ruling each follows is from this tab and tab 88.

Deleted:

- `src/invocation_context.rs` (5 tests), `src/redaction.rs` (1), `src/durability.rs` (2), `src/queue/evt.rs` (3) (ruling 1).
- `tests/queue.rs` (7) with `src/invariants.rs` (ruling 2).
- `src/credential_generation.rs` (1) (`CredentialGeneration` goes). 56 test files that name `CredentialGeneration` change.
- `src/operator_action.rs::basis_vocabulary_is_exact`, and in `src/intent_cases.rs` `uncertain_lists_open_intents_oldest_first_up_to_the_limit`, `a_resolved_intent_leaves_the_list_and_answers_its_basis`, `a_finished_intent_does_not_resolve` (tab 88 R-06 C).
- `execution/workflow/src/queue/automation_live` (1,358 lines, test-only): it runs the deleted automation statements and is the only writer of the flow columns. Inference: a case that admits a run through the real queue may move to the manual trigger.

Changed:

- `src/intent_cases.rs::a_begun_key_is_uncertain_and_never_new_again`: `in-progress`, then `outcome-unknown` after the timeout (tab 88 R-06 C).
- `tests/store.rs::status_sql_literals_round_trip`, `persisted_fail_kind_vocabulary_is_exact_and_alias_free`: SQL values come from `RunStatus` (ruling 5).
- Flow-column cases in the `runtime/src/plugins/wamn_postgres/production_claim.rs` tests and `runtime/tests/production_claim_live.rs`: the legacy arm and `ExhaustedExecutionIdentity::Flow` go.

Moved:

- `src/intent_cases.rs` (shared intent store cases, used by `write_log/tests.rs` and `run-state-sqlite/tests/intent_store.rs`) leaves with the intent record (ruling 3).
- `src/authority_class.rs` (3) leaves with the credential items (ruling 4).
