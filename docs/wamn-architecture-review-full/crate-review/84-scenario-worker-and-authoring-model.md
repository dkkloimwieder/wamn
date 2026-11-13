# 84 wamn-scenario-worker and wamn-authoring-model

Scope at `epic/dep7` (b94833fd): `services/scenario-worker` (`management.rs` 1,880, `store/admission.rs` 592, `authoring.rs` 537, `main.rs` 161; `tests/management_live.rs` 2,202) and `crates/authoring/model` (`lib.rs` 674, `gate.rs` 166, tests 599). Nothing built or run.

## Earlier work checked first

- Tab 30: workflows belong to the environment; park, release and run list become control routes. Tab 71 2 A: workflow author, enable, disable and list become control routes.
- Tab 76: `wamn-catalog::workflow_gate` checks an environment workflow against the serving release.
- Tab 01: `wamn-execution-contract` stops existing; its parts move to `wamn-scalar` and `wamn-digest`.
- Tab 62: `catalog.tenant_environments` is one of the copies of environment configuration.

## Files read

- `scenario-worker`: `main.rs`, `Cargo.toml`, headers of `management.rs`, `authoring.rs`, `store/admission.rs`; `management.rs` function list and route table.
- `authoring/model`: `lib.rs` header and item list, `gate.rs` header and functions.
- `deploy/sql/control-portable-store.sql` table list.

## Callers read

- Repository grep for `scenario-worker`, `wamn_authoring_model::`, `wamn_scenario_author`, `wamn_management_admitter`, `wamn_control_author`, `TestSetCase`, `ExpectedOutcome`; `crates/control/dev/src/dev/environment.rs::spawn_journey_management_gate`.

## One purpose per file

- `management.rs`: an HTTP service (`POST /authoring`) that authenticates a PAT, checks the project role, and runs Gate or Publish commands with an audit row. `authoring.rs`: the command adapter and the control-author credential probe. `store/admission.rs`: reads environment state through the management-admitter login.
- `wamn-authoring-model`: the authoring command and query messages, and `gate.rs`, which judges a wiring against admitted component facts.

## Findings

**1. The scenario worker has no deployment and its one remaining command duplicates the workflow route.**

Fact:

- No chart, Job or workload runs `wamn-scenario-worker` (grep of `deploy/`). The `Dockerfile` installs it in the `ctl` image; `dev/environment.rs::spawn_journey_management_gate` starts it as a "test fixture"; `tests/management_live.rs` drives it.
- `store/admission.rs` says "Publication through this legacy command is refused. Workflows are authored in the environment." What remains is Gate: judge a wiring against admitted facts (`gate.rs::judge_gate_document`), as `workflow_gate.rs` does for environment workflows.
- It brings its own roles and stores: `wamn_control_author` (`control_author.rs`, 440), `wamn_management_admitter` (`management_admitter.rs`, 476), `wamn_scenario_author` (in `postgres-init.sql`, `run-state.sql`, `catalog-schema-prefix.sql`), their Secrets and names, and `catalog.authoring_command_audit`, `wamn_run.gate_reports`, `wamn_authority.author_login_tenants` in `control-portable-store.sql`.

Options:

- A: delete `services/scenario-worker`, `wamn-authoring-model`, the three roles with their Secrets, name builders and grants, and the three tables. Workflow authoring is the control route (tab 71 2 A), checked by `workflow_gate`.

Recommend A.

**2. Wiring test cases are carried and never run.**

Fact: `WiringDocument.cases` (`Vec<TestSetCase>`) is bounded by `validate_cases` when a wiring is parsed. `gate.rs` says it judges "without executing a case". No code executes a `TestSetCase` (grep: only `wiring.rs`, `wamn-authoring-model` and `apps/platform/execution/contract/{expect.rs, test_set.rs}` name it).

Options:

- A: delete the `cases` field, `TestSetCase`, `Expect`, `expect.rs` and `test_set.rs` (390 lines).

Recommend A.

## Follows from earlier rulings

- Tab 70 1 A: `spawn_journey_management_gate` goes with the native dev path.
- Tab 62: `catalog.tenant_environments` goes with the environment copies.
- Tab 72 3 A: header citations and "MVP outcome: publish gate".

## Fine as is

- Nothing kept from these two crates under finding 1 A.

## Tests deleted, changed or moved

- Finding 1 A: `services/scenario-worker/tests/management_live.rs` (2,202), the `authoring/model` tests (599), credential and secret tests in `control_author.rs` and `management_admitter.rs`, and the grant cases for the three roles.
- Finding 2 A: case tests in `wiring.rs` and the contract crate.

## Not read

- Bodies of `management.rs`, `authoring.rs`, `store/admission.rs`; `control_author.rs` and `management_admitter.rs` bodies.

## Rulings

- 2026-11-12: both findings A (scenario worker, authoring model, three roles and three tables go; wiring cases and their types go).
