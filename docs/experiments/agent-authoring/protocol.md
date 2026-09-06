# Agent-authoring experiment — protocol

Status: living protocol. Runs are snapshots under
`docs/experiments/agent-authoring/<nnn>-<agent>-<task>/`. Pinned per run: `main`
commit, model ids, skill inventory.

Tooling: `docs/poc/agent-authoring-tooling-spec.md`. This document owns the question, the
measurements, the rubric, and the task fixtures. Applications appear here only as
fixtures that satisfy the tooling's task interface (work spec A6).

## 1. Question

Can a coding agent author a new wamn package from a scenario and prove it works,
with no human relay? Where does it stall, and which stalls are the platform's fault?

The unit of result is a stall list. A pass with no stalls and a fail with a clean
stall list are both results. A fail attributed to the environment is a wasted run.

## 2. Hypotheses

- H1. The agent reads design documents where it needs task instructions and
  misapplies at least one model rule: the claim law, continuation-as-data, the
  capability surface, no environment data in package content.
- H2. The agent spends at least one loop iteration locating a `wamn dev` failure
  in text output.
- H3. Told only to exercise the operations, the agent verifies the happy path:
  replay, changed-body, contention and not-found are absent from "How I
  verified" unless a skill names them. (The four cases are S9's content and
  never appear in the baseline brief.)
- H4 (exploratory). Across three runs of one agent on one task, variance comes
  from tooling stalls, not task difficulty. Three runs per cell cannot support
  this as a variance claim; it is recorded, not tested. The decision rule's
  two-run floor (§8) is the real protection.

## 3. Design

- **Arms.** Baseline: repository as-is (`CLAUDE.md`, `.agents/skills/beads`, and
  whatever global skills the machine has — inventoried and frozen). Later arms
  change exactly one tooling item (skills; receipts; invoke; flow tests) and rerun
  the same task, so every item is justified by a measured delta.
- **Agents.** Claude Code; Codex. Three runs each per task. Fresh worktree and
  environment per run.
- **Task ladder.** T0 calibration: one run per agent extending an existing
  package, to separate "cannot drive the loop" from "cannot author"; not scored
  against H1. T1 greenfield: the scored task (§4), six runs. T2 continuation: a
  second wiring consuming an event the first emits; opens after T1's stall table.
  T3 intake: from a two-line ask to a ratified application brief (§4.5) using
  S0; measured on question quality and false capability assumptions, never on
  code; kept out of T0–T2 so the authoring variable stays clean.
- **Budget.** Step 20 min, idle 5 min, cap 90 min. Token spend recorded.
- **Isolation.** Disposable environment; detached worktree; no push; `bd` off
  `PATH`; permission bypass acceptable only because both are disposable.
- **Non-intervention.** Nobody answers the agent. The brief tells it to decide,
  note the decision, and continue.

## 4. Task fixtures

Each task is a directory satisfying the tooling interface: `task.json`, `BRIEF.md`,
`SCENARIO.md`, `steps.json`. `BRIEF.md` is §4.1 copied unchanged into every task
directory; a task that alters it is a new arm. The scenario and steps change.

### 4.1 The brief (every task)

```
You are working in a git worktree of the wamn repository, detached from main, on
a disposable development environment. Nothing you do here reaches main or any
shared service. Read AGENTS.md first.

TASK: SCENARIO.md in "$WAMN_PILOT_TASK_DIR" describes what to build, in domain
terms. Build it as a wamn package.

DONE MEANS ALL OF:
1. `wamn dev --config "$WAMN_DEV_CONFIG" --overlay-root <overlay_root>` completes
   every stage through Activate, where <overlay_root> is the path named in
   "$WAMN_PILOT_TASK_DIR/task.json". Your package lives there; create it if it
   does not exist.
2. You have exercised every operation the scenario names against the running
   release and recorded the exact requests and responses in your report.
3. REPORT.md exists at "$WAMN_PILOT_RUN_DIR/REPORT.md" in the format below.

WHAT IS TRUE OF THIS LOOP:
- Publish through Activate refuse a worktree with uncommitted or untracked
  changes. Commit locally first. Never push.
- `wamn dev … --hold` runs once and keeps the activated release reachable until
  you stop it; it prints `run served: <base_url> host=<route_host>`. Send requests
  to <base_url> with `Host: $WAMN_ROUTE_HOST` and `Authorization: Bearer <token>`,
  where <token> is `.stringData.token` in "$WAMN_ROUTE_CALLER_PAT_FILE".
- There is no reference data. Create it through your own operations.

CONSTRAINTS:
- Create or edit files only under the paths named in
  "$WAMN_PILOT_TASK_DIR/task.json" `allowed_paths`. Nothing else in the
  repository is yours to change. If the task cannot be completed inside those
  paths, say so in the report and stop.
- Do not push, do not create remote branches, do not create or edit beads.
- Do not weaken or bypass any permission, policy, RLS, or gate to make a stage pass.
- Follow the repository's naming and versioning rules. No version suffixes.
- After three attempts at the same failure, stop and write the blocker in the
  report. Do not work around the platform.
- Nobody will answer questions. Decide, record the decision, continue.

TOOLS: `wamn` and `wamn-ctl` are on PATH. `curl` reaches the route host.
Postgres is reachable at the URLs in "$WAMN_DEV_CONFIG" for inspection only.

REPORT.md (these headings, this order):
# Summary · # Changes · # How I verified · # Decisions · # Where I got stuck
# Rules I relied on · # Open questions

"How I verified" must show, with commands and outputs verbatim: your component's
own tests; the loop; every operation the scenario names exercised against the
running release; and what you did not verify, and why.
```

Mechanisms the brief names, and why: the commit rule and `--hold` are facts of the
loop documented nowhere the agent will look (work spec F4, F5); without them the
task is impossible from a script, which would measure the platform, not the agent.
Everything else the agent must find.

### 4.2 T1 — proposed: dock appointments (ruling pending)

Off-portfolio by intent: no template in the tree, no planned application anchored
on an experiment artifact.

`SCENARIO.md`:

```
Carriers book dock appointments. A dock has slots; two appointments on one dock
cannot overlap. Booking is a command with an idempotency key; a replay returns the
same appointment id. An appointment moves scheduled → arrived → departed; check-in
records the actual arrival time. Dispatch needs a list of one dock's appointments
for a day, sortable by slot, filterable by status.
```

`steps.json` gate (all routes resolved from the agent's `attachments.json`):
create a carrier and a dock; book; book again with the same key → equal `value`;
book with the same key and a changed body → typed refusal; two concurrent
bookings on one slot → exactly one typed refusal; check in → `arrived` with the
recorded time; check in a nonexistent appointment → `not_found`; list one dock for
the day filtered by status, sorted by slot → the expected order.

Exercises: the claim law, a multi-row invariant under contention, a status
transition, a bounded query with sort/filter, generated CRUD plus custom commands,
one component, wirings, attachments, permissions. Nothing to seed.

Alternatives for ruling: a portfolio app core (Quality is nearest; it is planned
app 4 and needs per-user RLS, a platform item), or a different off-portfolio
domain of the same size.

### 4.3 T0 — calibration

One run per agent, extending an existing package with one command, one wiring, one
route (`task.json` sets `baseline.overlay_root` and a narrow allow-list). Graded on
the loop, paths, checks and fence reports only; H1 not scored. Its purpose is to price the loop
itself so T1 stalls are net of it.

### 4.4 T2 — continuation (after T1)

A second wiring consuming an event the first emits (check-in → a downstream
consumer), authored as data plus a second ingress. This is where
continuation-as-data and joins-as-data bite.

### 4.5 Application brief — the format a scenario arrives in

A markdown skeleton with fixed sections and open prose inside them. Each section
maps to one package surface and one skill; a task's `SCENARIO.md` is the domain
sections, its `steps.json` is the exit gate. Minimum for anything to be
authored: nouns, commands with invariants, queries, ingress, external services,
permissions, exit gate. UI is required only when the brief names screens; a
headless package is a valid application.

| section | pinned by the human | open to the agent | surface | skill |
|---|---|---|---|---|
| purpose and actors | who uses it | — | — | — |
| domain nouns | the nouns, ownership, what identifies each | columns, types, indexes | models, migrations | S2 |
| commands | verb, invariant, idempotency expectation, refusals the user must see | SQL, locking, error mapping | `custom_operations`, component | S3, S4 |
| queries | what each screen or caller lists, sorts, filters | keyset, projections | `query/*.sql` | S10 |
| ingress | how each command is triggered: call, event, schedule | routes, schemas | attachments (Http · Internal · Studio · Cron) | S5, S13 |
| external services | from the admitted interfaces only (`wamn:postgres`, `wamn:connection`, `wasmcloud:blobstore`, JetStream); anything else is a platform ask | binding names | `connections`, `bind-connection` | S12 |
| permissions | actors × commands; what is per-user | tokens | permission tokens | S5 |
| UI | screens and the operations each drives; today a Rust TUI over generated bindings (`docs/poc/tui-first-frontend-spec.md:8-30`) | layout | client crate | — |
| non-goals | explicit | — | — | — |
| exit gate | observable, measurable behavior | — | `steps.json` | S9 |

What must not be open: the invariants (each carries an id; they become grader
steps by that id), the
external-service list (an unadmitted one makes the agent invent `wasi:http`,
which Admit refuses), and the exit gate. The brief carries no host, URL,
environment, or secret; those are bindings at deploy (`.20`).

Intake is a stage of its own (T3, skill S0): from a short ask, the agent
interviews to fill the skeleton and emits the brief, a draft `steps.json`, and a
list of asks needing an unadmitted capability, flagged as platform work rather
than dropped. A human ratifies; the ratified brief is what an authoring run
receives. T1's brief is human-written so the authoring measurement has one
variable.

### 4.6 Author testing procedure — what the platform offers today

What an author can run at the pinned commit, and what has no verifier. The
baseline brief asks only for tests, the loop, the scenario's operations, and an
honest omission list. The ordered procedure below, including the four named
cases (replay, changed body, contention, not-found), is skill S9 and appears
only in the skills arm; whether an agent reaches those cases unprompted is what
H3 measures.

| layer | procedure | state |
|---|---|---|
| component unit tests | `cargo test --manifest-path components/Cargo.toml -p <crate> --all-targets` (`build-and-test.md:239-240`) | exists |
| wasip2 compile | `cargo check … --target wasm32-wasip2`; the loop's Build stage | exists |
| manifest ↔ schema | Introspect → Generate → Admit on saved bytes | exists |
| wiring shape and semantics | Gate stage | exists |
| authored SQL statements | per-package hand-written native verifier + `cargo sqlx prepare --check`; no generic verifier in the loop (work spec F21) | gap: proven at runtime |
| behavior | flow tests / test-set | model only, no runner (`.8.5.4`) |
| replay, changed-body, contention, not-found | by hand under `--hold` | gap until B4 case types |
| exclusion constraint for the overlap invariant (`EXCLUDE USING gist`, needs `btree_gist`) | whether the migration validator and the guest role admit it | **probe at the pinned commit before run 1**; record the answer here. If refused, an agent choosing it hits a `generator` stall that is the platform's fault and must be pre-priced |
| redelivery | none | gap (B4 `redeliver`) |

The two gaps are expected stall sources in category `generator` (statements) and
`verb-missing` (behavior). They are findings the baseline arm is meant to price,
not surprises.

## 5. Grading

Machine checks come from the tooling grader (work spec A7): loop through
Activate, allow-list, the scenario's `steps.json`, and the checks the loop has
no fence for (claim replay via the steps, `row_version` spelling, naming).
Capability surface, additive migrations and environment data are the loop's
own fences; the grader reports their verdicts, it does not re-decide them.

The test set itself is graded before the agent is: a task's `steps.json` is
accepted only if it passes the tooling spec's V3 checks — every `must` step
kills at least one application mutant, names a brief invariant, asserts a
predicate rather than a status, and the set runs twice with equal results. A
task whose set fails V3 is not run. Any miss on loop, allow-list, or a step marked
`must: true` in `steps.json` is a run FAIL; other steps are recorded.

Human items, scored from the worktree and the transcript:

- H-1 Model fit: state is rows and operations; no state invented outside the
  database; no wait/poll.
- H-2 Idempotency: the claim law is implemented by construction (claim row keyed
  by `idempotency_key`, identities pre-generated), not by primary-key accident.
- H-3 Invariant: the overlap rule is enforced by the database — an exclusion
  constraint, or a lock and check inside one transaction — not by application
  code that reads then writes. Either database form scores the same. If the
  §4.6 probe shows the migration validator or the guest role refuses `EXCLUDE`,
  lock-in-transaction is the top rung for that run and an agent that tried the
  constraint and was refused is not marked down.
- H-4 Generated artifacts are regenerated, never hand-edited.
- H-5 Report reproduces: every claim in "How I verified" replays.
- H-6 Procedure followed: "How I verified" shows tests, the loop, the
  operations, and an omission list; a skipped part is named, not silent. The
  four S9 cases are counted under Q11, not required here in the baseline arm.
- S-1 Naming law; S-2 node-error taxonomy, no string matching; S-3 stall entries
  point at a stage and a message.

## 6. Operator directions

1. `tools/agent-pilot-run all --run <nnn> --agent {claude|codex} --task <dir>`.
   Record the `main` commit, model id, machine load, skill inventory.
2. Do not intervene. Environment failure → mark `INVALID-ENV`, fix the runner,
   rerun as `<nnn>b`.
3. On driver exit: grade (the runner does the machine half); fill the human items;
   walk the transcript once and tag every failure or pause with one stall category
   (§7.3) and a pointer; classify every file read before the first edit as
   `design-doc | code | skill | generated | other`.
4. `tools/agent-pilot-report --run <nnn>`. Numbers regenerate from the raw
   directory.
5. After six valid T1 runs: the stall table (§8). Do not change the brief and the
   task in the same day.

## 7. Measurements

### 7.1 Primary outcome per run

`PASS` · `FAIL (items: …)` · `INVALID-ENV`.

### 7.2 Quantitative, per run

Q1 minutes to first green through Activate; total minutes. Q2 `wamn dev` runs;
failed-stage histogram. Q3 files read before first edit, by class. Q4 verification
method: `none | curl | invented-script | loop-only`. Q5 lines outside allowed
paths (must be 0). Q6 tokens, cost, model id. Q7 stalls by category with pointers.
Q8 law violations. Q9 over-claims (report statements the grader could not
reproduce). Q10 skills present (repo and global) and skills activated.
Q11 verification coverage: how many of the scenario's operations were driven,
and which of the four S9 cases (replay, changed body, contention, not-found)
appear unprompted.

### 7.3 Stall categories (closed set)

`env` · `verb-missing` (no command for the need, e.g. delivering a payload) ·
`output-parsing` (a failure existed; the agent could not locate it) ·
`rule-unknown` (a model rule not found, or found in the wrong document) ·
`generator` (Introspect/Generate surprised the agent) · `wiring-shape` (Gate
refused) · `component-build` · `permissions` · `provisioning` (identity, package
registration, workspace membership) · `thrash` (same failing action ≥3 times) ·
`other`.

### 7.4 Success and failure of the experiment

SUCCEEDS when six valid T1 runs exist and every stall carries a category and a
pointer. INCONCLUSIVE if fewer than four runs are valid or `env` exceeds half of
all stalls in more than two runs; fix the runner, rerun, rank nothing from it. A
6/6 PASS with an empty stall table means the baseline needs no tooling for this
task and T2 opens.

## 8. Decision rule

Rank stall categories by minutes lost across valid runs.

| category | opens |
|---|---|
| `output-parsing` | B2 receipts |
| `verb-missing`, `invented-script` | B3 invoke, then B4 flow tests |
| `rule-unknown` | B1 skills and the rule paragraphs |
| `provisioning` | findings on `wamn-10yt.10` (`.10.32` identity, base-only loop) |
| `env` | runner only |
| `generator`, `wiring-shape`, `component-build`, `permissions` | product beads, one per distinct message |
| `generator` where the message is a statement first failing at Activate or under `--hold` | the statement-verifier finding (work spec F21) on `wamn-10yt.10` / `wamn-0h0g.22` |
| `thrash` | attributed to what it repeated |

Nothing opens on fewer than two runs showing the category.

## 9. Qualitative rubric

1–4 per dimension, anchors given; the machine checks decide PASS/FAIL, the rubric
explains.

- E1 Model fit (as H-1): 4 rows and operations only · 2 correct after a detour ·
  1 state outside the database or a wait/poll.
- E2 Idempotency reasoning: 4 claim row by construction, explained · 2 PK
  accident · 1 time/random or none.
- E3 Boundary respect: 4 stayed inside the allow-list, refused rather than worked
  around · 1 patched or weakened something to go green.
- E4 Verification honesty: 4 every claim reproduces, unverified items named · 2
  one over-claim · 1 declared done on red.
- E5 Diagnosis: 4 reads the failing stage, changes one thing, reruns · 2 some
  thrash · 1 three or more repeats.
- E6 Craft: 4 naming law, rust-guidelines, canonical spellings, additive
  migration, node-error taxonomy · 2 one miss · 1 suffixes, string-matched errors,
  hand-edited generated code.
- E7 Report usefulness: 4 an owner can open beads from "Where I got stuck" without
  the transcript · 1 prose without pointers.

A Claude instance may pre-tag the transcript against §7.3; the human grader owns
the scores; the machine checks own PASS/FAIL.

## 10. Run report template

```
# <nnn>-<agent>-<task>
main commit · model id · load at launch · driver args · run cap hit? · skills present
Outcome: PASS | FAIL (items) | INVALID-ENV
Q1 first green: <min> · total: <min>
Q2 wamn dev runs: <n> · failed stages: {…}
Q3 reads before first edit: design-doc n · code n · skill n · generated n · other n
Q4 verification: … · Q5 outside allowed paths: <n> · Q6 tokens/cost
Q7 stalls: [{category, minutes, pointer, one line}] · Q8 law violations · Q9 over-claims
Q10 skills activated: [...]
Q11 verification coverage: operations driven <n>/<m> · S9 cases unprompted <n>/4
E1..E7 with one line each
Raw: <nnn>-<agent>-<task>/
```

## 11. Threats to validity

- One task per ladder rung; T1 exercises the common path, not continuation.
- Model drift: record ids; never compare across model versions.
- Machine load: record it; compare categories, not minutes, on a loaded machine.
- Grader bias: machine checks decide; rubric explains; grade one run blind if a
  second grader exists.
- Prompt leakage: the brief names `--hold` and the commit rule and nothing else,
  and never the four verification cases (those are S9);
  a brief that names more is a new arm.
- Test-set quality: a trivial set passes trivially; V3 is applied to the task's
  set before run 1, and the kill matrix is re-run when the set changes.
- Known gaps: the statement verifier (work spec F21) and the missing flow-test
  runner will produce stalls in every run; they are priced, not discovered, and
  do not count against the agent in H-6.
- Global skills: `rust-guidelines` is present via `CLAUDE.md:107` only if
  installed on the machine; the inventory is frozen across runs so arms compare
  like with like.
