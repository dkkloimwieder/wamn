# 30 wamn-workflow, part 2

Scope: the queue, schedules and the park/release/list contract. Nothing built or run.

## Files read

- `crates/execution/workflow/src/queue.rs` (non-test part, in full)
- `src/schedule.rs` (non-test part, in full)
- `src/contract.rs`, `src/contract/postgres.rs` (in full)

## Callers read

- `services/host/src/host.rs` (schedule task, `RouterDriver::new`).
- `services/ctl/src/workflow_verbs.rs` (park, release, list).
- `crates/platform/runtime/src/plugins/wamn_postgres/event_admission.rs` (`admit_manual_run`, `admit_schedule_run`, `due_text`), `event_batch.rs` (event run insert), `production_claim.rs` (claim result).
- `crates/execution/run-state/src/queue/workflow.rs` (the run insert statements).
- `crates/catalog/model/src/serving_manifest.rs::parse_workflow_schedule`, `wiring.rs` (`WiringTerminal`, `response`).
- Tab 10 rulings 1 and 2 (durable caller, candidate and service principal branches deleted).

## One purpose per file

- `queue.rs`: claim a run, drive it, record the result.
- `schedule.rs`: once a minute, admit runs for due schedule workflows.
- `contract.rs`, `contract/postgres.rs`: park, release and list runs for `wamn-ctl`.

## Findings

**1. The claim flags and the durable caller mapping are dead.**

Fact: tab 10 found the claim flags `router_caller_attached` and `durable_caller_attached` always false, and ruled the durable caller branch deleted (a waiting caller that read a queued run's result from the run row). So no run has a caller today, and a `Respond` node fails with `respond-without-caller`.

What each part is:

- Router `Respond`, `Verdict::Respond`, `Delivery.caller_attached`: how a walk gives a synchronous answer to a caller in the same process. Kept by owner ruling.
- Claim flags, `RouterResultMode::DurableCaller`, the `Respond` arm of `production_router_action`: the deleted run-row design.
- A: delete the claim flags, `RouterResultMode` and the queue's `Respond` arm (replaces tab 29 finding 10). Keep router `Respond` and `caller_attached`.

Fact, router: a walk with no caller that reaches a `Respond` node fails with `respond-without-caller`.

**2. Release packages in the claim scope.** Fact: `QueueService::bind` puts every release package id plus every application name into `package_ids`. All three product run inserts (manual, schedule, event) write the application name. Runs with a release package were the candidate and automation runs that tab 10 ruling 2 deletes.

- A: `package_ids` holds application names only.

**3. The park/release/list contract.** Fact: `Workflows` is a trait with one implementation. `WorkflowErrorType::Refused` and `Conflict` are never built. The only product user is `wamn-ctl`, which connects straight to the project database with the admin URL.

- A: move `PostgresWorkflows` into `wamn-ctl`. Delete the `Workflows` trait and the two unused error types.
- B: park, release and list become host control routes, as manual start already is. `wamn-ctl` calls the routes; its direct database path goes.
- Recommend A (smaller). B removes direct database access from the client tool.

**4. The schedule task has its own connection.** Fact: `host.rs` opens a separate `tokio_postgres` connection for the schedule task, with its own reconnect loop and `connected` flag. `PostgresSchedules` uses it for one SELECT a minute. Admission already goes through `WamnPostgres`, which the task also holds.

- A: read the enabled schedules through `WamnPostgres` (executor-platform login). Delete the separate client, the reconnect loop and the flag. Part of the held item "DB connections outside pools".

**5. A test seam in the schedule task.** Fact: `ScheduleRows` has one product implementation. `tests/schedule_decisions.rs` implements a fake; `serve_with_clock` is public for tests.

- A: one pure function decides which schedules are due at a tick; tests call it. Delete the trait and `serve_with_clock`.

**6. Schedules are UTC only.** Fact: `parse_workflow_schedule` requires "five-field cron in UTC"; `is_due` matches the UTC tick. Owner ruling (2026-11-11): time zone settings are needed; a time zone decides calendar-day behaviour. Inference: a daily 06:00 schedule for a plant outside UTC moves by an hour at each daylight-saving change.

- A: match the cron against the tick in the environment's time zone setting. The `due` time stays UTC.
- B: keep UTC; authors write UTC cron.
- Recommend A.

**7. `serve_queue` writes the turn handling twice.** Fact: the serve loop and the drain loop repeat the same turn, beat and retry code.

- A: one loop; `stopping` switches it to drain mode.

**8. SQL in the workflow crate.** Fact: `ENABLED_SCHEDULES_SQL` in `schedule.rs`.

- Added to the held SQL scatter item (with 4 A it moves next to `admit_schedule_run`).

## Tests deleted, changed or moved

- Delete (finding 1 A): the `Respond` and durable-caller cases in `router_action.rs` tests. Router `Respond` tests and `WiringTerminal::Respond` fixtures stay.
- Change (repo rule: test code must not redo the product): `src/queue/automation_live/shutdown.rs` inserts runs with raw SQL; use `admit_manual_run`.
- Move (finding 3 A): the contract tests in `src/queue/automation_live.rs` and `automation_live/jsonata.rs` move with `PostgresWorkflows`.
- Change (finding 5): `tests/schedule_decisions.rs` calls the pure function; `control/environment/tests/workflow_schedule_live.rs` calls `serve`.

## Not read

- `src/queue/automation_live.rs`, `automation_live/jsonata.rs`, `automation_live/shutdown.rs` beyond the lines above (test code).
- `production_claim.rs` beyond the claim result (tab 10 read its candidate and caller parts).

## Rulings

- Finding 1: A. Router `Respond` and `caller_attached` stay; claim flags, `RouterResultMode` and the queue's `Respond` arm go.
- Finding 3: B. Park, release and list become host control routes; `wamn-ctl` calls them and loses its direct database path. `Workflows`, `PostgresWorkflows` and the unused error types leave the workflow crate.
- Finding 6: A. A schedule's cron matches the tick in the environment's time zone setting; the due time stays UTC.
- Findings 2, 4, 5, 7: A. Finding 8 added to SQL scatter.
- Owner rule (2026-11-11): the review looks at the architecture with fresh eyes. Earlier epics, issues and plan documents are not a reason to keep or shape anything, and are not cited.
