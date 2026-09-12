# Pilot task and driver inputs

This document defines the independently authored task and driver inputs for the agent pilot.
The [protocol](protocol.md) owns the experiment and human grading rules.
The [Rust runner](../../src/agent_pilot/runner.rs) prepares runs, and the [grader](../../src/agent_pilot/mod.rs) evaluates their recorded outputs.
Operating commands belong in [development operations](../../../../docs/operations/development-loop.md#agent-pilot-authoring-experiment).

## Task input

A task directory contains `task.json`, `BRIEF.md`, `SCENARIO.md`, and the grader's `steps.json`.
The brief supplies the agent prompt.
The scenario states the application problem and its invariants.
The steps exercise those invariants through actual application routes and independent reads.

The [Dock task](tasks/dock-appointments/task.json) is the current concrete example.
Its manifest carries these inputs:

| Field | Meaning |
|---|---|
| `task` | Task name. |
| `identity` | Explicit `org`, `project`, `env`, `schema`, `tenant`, and `route_host`. |
| `package_id` | Declared application package identifier. |
| `package_sources` | Existing package source paths at the pinned commit. |
| `overlay_root` | Application directory that the agent works on. |
| `baseline.overlay_root` | Optional existing application for the pre-agent run. |
| `allowed_paths` | File patterns that include the application directory. |
| `environment` | Explicit `event_stream_replicas` and `event_dup_window_secs`. |
| `grade` | Private step file, named checks, and existing policy-result requests. |

A new application's `overlay_root` need not exist at the pinned commit.
Every declared existing package source must exist there.
The environment fields must declare unsigned integers.
The current Dock fixture declares one event stream replica and a 120-second duplicate window.
Those are fixture inputs, not defaults for another environment.

The runner removes `grade` from the agent's task copy.
It also removes the grader's step file from the agent-visible fixture.
Private grading input remains under `${XDG_STATE_HOME:-$HOME/.local/state}/wamn-pilot-grading/<run>`.
That directory must stay outside the run directory's ancestor path.
The agent receives the brief and scenario without the private answer checks.

## Step input and evaluation

`steps.json` contains an ordered array.
A required step has `must: true` and names its invariant through `invariant`.
At least one required step must exist.
The current runner refuses required route steps that assert only an HTTP status.

A route step names an operation and supplies its command body.
The grader resolves the route from `publication/attachments.json`.
It does not hard-code the agent's chosen attachment path.
`reuse` carries a prior result field into a later input.
The HTTP adapter supplies the bearer credential and declared `Host` value.

The retained step keys include these forms:

| Form | Input and observation |
|---|---|
| Route | `route`, `body`, optional `reuse`, and `expect`. |
| Read | `sql` and the expected row count. |
| Contention | `concurrent` names existing steps and `expect.exactly_one` identifies the required refusal. |

The declared response predicates include item kind, error code, field equality, required fields, and sorted results.
A named check reads only steps that associate themselves with it through `proves`.
It does not infer that association from prose or a step identifier.
The serialized `proves` field remains part of the existing fixture contract.

The grader records application requests and responses in `grade/http.jsonl`.
Its `checklist.json` separates loop, path, step, check, policy, and human results.
Human grading fields follow the protocol and cannot remain null when the report requires them.
Existing platform decisions are reported from their owning command results.
The grader does not replace capability admission, migration rules, or environment confinement with another policy engine.

Replay evaluates a recorded run without starting its environment.
It writes `checklist-replay.json` and preserves the original record.
A run without recorded requests cannot establish replayed route results.
The live run's recorded teardown result remains the only basis for teardown during replay.

## Driver boundary

The runner selects `claude`, `codex`, or `stub` and starts it in the prepared worktree.
The prompt comes from `fixture/BRIEF.md`.
The agent commits locally and does not push.
The runner keeps task-system tooling outside the driver path and directs push configuration at the owned refusal path.

The driver receives these existing environment variables:

```text
WAMN_PILOT_REAL_WAMN
WAMN_DEV_CONFIG
WAMN_ROUTE_HOST
WAMN_ROUTE_CALLER_PAT_FILE
WAMN_PILOT_RUN_DIR
WAMN_PILOT_TASK_DIR
```

`CARGO_TARGET_DIR` is unset in the driver environment.
The run's `bin` directory precedes native tools on `PATH`.
The runner preserves the raw JSON stream in `transcript.jsonl`.
Its `driver.json` records the actual agent, binary, start, end, exit, reason, model, session, and output style.

The native [process owner](../../src/agent_pilot/runner/process.rs) applies three bounds.
The whole run stops after 90 minutes.
A missing stream line for 20 minutes triggers the step timeout.
Five minutes without a stream line or worktree change triggers the idle timeout.
The recorded reasons remain `completed`, `idle-timeout`, `step-timeout`, or `run-cap`.
A stub run exercises the selected driver outcome without implementing the application.

## Invocation records

The wrapper records each started `wamn` call in `verbs-started.jsonl` before execution.
It records completed calls in `verbs.jsonl` with their exit code and duration.
The shared counter allocates distinct log names under a lock.
A held call can have a start record without a completion record.

`wamn_dev_runs` and `wamn_dev_hold_runs` count started calls.
`wamn_dev_failed` counts nonzero completed calls.
`first_green_minutes` uses the first started call whose output records a completed twelve-stage run.
The absence of a completion record does not establish a failed call.

The runner retains final source differences and the exact path comparison against `allowed_paths`.
Cleanup reports only the run's owned resources.
A failed cleanup remains a failure even when the agent or grader succeeded.
Private environment files, raw credentials, and private grading input are not publication material.
