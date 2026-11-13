# 10 `wamn-runtime`, part 1: the open points

Path `crates/platform/runtime`. 65 source files, 38,374 lines with inline tests; 33 test files, 19,134 lines. This is the largest crate so far. It is taken in parts.

This part answers the three questions that earlier tabs sent here. It is not the review of the crate. What was read for it: the caller and candidate code of `plugins/wamn_postgres/production_claim.rs`; the raw-SQL path in `resources.rs` and `claims/transactions.rs`; and, outside the crate, `execution/workflow/src/queue.rs` and `router_action.rs`, which are the other end of the first question. Nothing was built or run.

## Size by part (fact, lines with inline tests)

| Part | Lines |
| --- | --- |
| `plugins/wamn_postgres` | 16,629 non-test files; about 6,800 more in its test files |
| `plugins/wamn_blobstore` | 4,768 |
| `plugins/connection_http` | 4,145 |
| `plugins/wamn_jetstream` | 1,872 |
| `plugins/route_authentication`, `wamn_logging`, `effect_span` | 2,401 |
| 21 files at the crate root (connections, OAuth, sessions, release manifest, artifacts) | about 8,500 |

## Question 1: does any queued run have a waiting caller

From tab 09. Answer: no. (fact, both ends read)

| Step | What the code shows |
| --- | --- |
| What is written | The only statements that insert into `runs` in production are the builders in `run-state` `queue/workflow.rs`. They write the trigger sources `event`, `manual` and `schedule`. (`automation` has a builder with no caller.) |
| How "a caller is waiting" is decided | `select_production_claim_sql` computes `durable_caller_attached` as `trigger_source IN ('http','internal','studio')` and `router_caller_attached` as that or a `scenario-draft` or `test-case` run with no flow. None of those five values is written. Both flags are always false. |
| The one consumer | `execution/workflow/src/queue.rs` is the only caller of `RunStore::claim_next`. It passes the two flags on, and discards `candidate` and `service_principal_id` with `_`. |
| What builds a caller outcome | `router_action.rs::production_router_action` builds a `ProductionCallerOutcome` only in the mode `DurableCaller`, which it picks only when the flag is true. |
| Who reads a stored caller outcome | No production code reads `caller_outcome_json`, `caller_released_at` or the other caller columns back. |

So four branches cannot run for a queued run in production:

| Branch | Where it lives |
| --- | --- |
| Caller release | `run-state` `transitions.rs::release_caller_sql`, `CallerReleaseResult`, `StoredCallerOutcome`, `ProductionCallerOutcome`, the `caller-unreleased` refusal in `terminalize_sql`, the caller arms of `terminalize_exhausted_sql`; here `production_claim.rs::complete` and `decode_caller_release`; in `workflow` `RouterResultMode::DurableCaller`; seven `caller_*` columns and four CHECK constraints on `runs` |
| Candidate runs | `ProductionCandidate`, `CandidateBindingWorld`, the candidate arm of the claim decode, `binding_world_json` and the second arm of `runs_execution_grain_check` |
| Service principal | `service_principal_id` in the claim result, the `automation` arm of the constraint |
| Flow rows | Already ruled out in tab 09 |

A caller that waits in process is a different thing and is in use: `engine/src/router_delivery.rs` and `workflow/src/router_driver.rs` set `caller_attached: true` for a synchronous delivery. That path stays.

Not read: the rest of `production_claim.rs` (about 1,100 of its 1,602 non-test lines); `CandidateConnectionBinding` in `claims.rs`, which carries a candidate's connection facts and may be part of the same dead branch.

## Question 2: can raw SQL from an application be held to read-only

From tab 07. Answer: the mechanism exists; the raw path does not use it. (fact)

- `claims/transactions.rs` has two ways to open a transaction: `BEGIN ISOLATION LEVEL READ COMMITTED`, and `BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY`. The second is used for the guest transaction of a read invocation. `begin_with_claims` takes it as the argument `read`.
- The raw interface, `client.query` and `client.execute` in `resources.rs`, goes through `one_shot_for_project`, which passes `read` as `false`. So raw SQL is read and write today.
- The raw path already applies the project's statement timeout and row limit (`run_query` with `pp.row_limit`), and it connects as the role of the calling component. For an application that role is `wamn_app`, so the database grants already bound what it can touch.

## Question 3: the stream host code

From tab 04. `plugins/wamn_postgres/run_stream.rs`, 373 lines, serves `statements.run-stream`. Tab 04 ruled the guest wrapper deleted because no guest calls it. `docs/architecture/data-access.md` says no base read calls it. The raw interface has its own way to read a large result: `transaction.open-cursor` and `cursor.fetch`. (fact)

## Options

| # | For | Option | What goes | What is lost |
| --- | --- | --- | --- | --- |
| 1 | question 1 | Delete the durable caller branch | The items in the first row of the branch table, in three crates, with seven columns and four constraints | Nothing that runs. A queued run that must answer a waiting HTTP caller would need it again. |
| 2 | question 1 | Delete the candidate branch and the service principal branch | The second and third rows of the branch table | Nothing that runs. To check first: `CandidateConnectionBinding` and its users in `claims.rs`. |
| 3 | question 2 | Raw SQL from an application opens the read-only transaction | The hard-coded `false` in `one_shot_for_project` for an application caller; the same for `client.begin` | Nothing. Platform components keep read and write. Not read: how `client.begin` opens its transaction in `resources.rs::begin_transaction`. |
| 4 | question 3 | Delete `run-stream` on the host side | `run_stream.rs`, the function in the WIT, its entry in the effect-span conformance test | Row-by-row streaming for admitted statements. `run` keeps the row limit; raw SQL keeps cursors. |

### Rulings (2026-11-11)

Owner ruling: accept, proceed. Keep the focus on single responsibility, single truth, and removal of dead code.

| # | Ruling |
| --- | --- |
| 1 | The durable caller branch is deleted, with its seven columns and four constraints. |
| 2 | The candidate branch and the service principal branch are deleted. Check first: `CandidateConnectionBinding` in `claims.rs`. |
| 3 | Raw SQL from an application opens the read-only transaction. |
| 4 | `run-stream` is deleted on the host side and in the WIT. |

## Plan for the rest of this crate

One tab per part, each read in full before any option: the Postgres plugin (claims and pools; operations and the write log; statements and numbering; workflow and event admission), then the blob store, the HTTP connection plugin, JetStream, and the root files.

## Tests deleted, changed or moved (added 2026-11-12)

Checked at `epic/dep7` (b94833fd) by grep. Test names are fact; which ruling each follows is from this tab.

Deleted:

- `runtime/src/plugins/wamn_postgres/claims/tests.rs::live_stream_reads_once_and_scopes_claims` and `an_unchecked_declared_statement_is_refused_before_a_stream_opens` (ruling 4).
- `claims/tests.rs::candidate_binding_world_requires_complete_canonical_unique_rows`, `production_claim.rs::candidate_janitor_failure_body_names_the_frozen_wiring`, `candidate_projection_is_exactly_what_the_decoder_indexes` (ruling 2, after the `CandidateConnectionBinding` check).

Changed:

- Durable-caller cases: `run-state/tests/run_state_live.rs`, `admission_live.rs`, `run-state/src/transitions.rs`, `run-state/src/queue/sql.rs`, the `production_claim.rs` tests and `execution/workflow/src/queue.rs` name the durable caller branch (ruling 1). Not read case by case.

Added: one live test that application raw SQL opens a read-only transaction (ruling 3). It replaces the raw-SQL coverage of `sqlx_transaction_live.rs` (tab 04).
