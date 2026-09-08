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
- **Agents.** Claude Code, three runs per task. Ruled by the owner on
  2026-09-06: the baseline arm is one agent, not two. Codex stays an arm the
  design supports and this wave does not run, so no cross-agent comparison is
  claimed from it. Fresh worktree and environment per run.
- **Task ladder.** T0 calibration: one run per agent extending an existing
  package, to separate "cannot drive the loop" from "cannot author"; not scored
  against H1. T1 greenfield: the scored task (§4), three runs. T2 continuation: a
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

### 4.2 T1 — dock appointments (ratified 2026-09-06)

Off-portfolio by intent: no template in the tree, no planned application anchored
on an experiment artifact. That is why it was chosen, not a side effect: a task
with a template in the tree measures copying, not authoring.

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

Considered and not taken: a portfolio app core (Quality is nearest, and it is
planned app 4 and needs per-user RLS, which is a platform item), and a different
off-portfolio domain of the same size. The first makes the experiment a
dependency of the product plan. The second buys nothing this one does not
already exercise.

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
| exclusion constraint for the overlap invariant (`EXCLUDE USING gist`, needs `btree_gist`) | whether the migration validator and the guest role admit it | **probed 2026-09-06, answer below**: admitted inside `CREATE TABLE`, refused through `ALTER TABLE ADD CONSTRAINT` |
| redelivery | none | gap (B4 `redeliver`) |

The two gaps are expected stall sources in category `generator` (statements) and
`verb-missing` (behavior). They are findings the baseline arm is meant to price,
not surprises.

#### The `EXCLUDE` probe, answered

Run at the pinned commit against a live pilot environment. The answer is not a
yes or a no. It depends on which statement writes the constraint, and the two
statements do not get the same answer.

The validator half, settled in
`crates/schema/introspection/src/migration_policy.rs`:

- Inside `CREATE TABLE`, a named `EXCLUDE USING gist` constraint is **admitted**.
  `validate_constraint_names` polices four kinds for an unquoted name: primary
  key, unique, check and foreign key. `EXCLUDE` is not one of them, so the
  constraint passes with only the 63-byte name check applied.
- Through `ALTER TABLE … ADD CONSTRAINT`, it is **refused**.
  `validate_add_constraint` requires the token after the constraint name to be
  `check` followed by an open parenthesis, and refuses everything else with
  "ADD CONSTRAINT admits only the demanded named CHECK form".
- `CREATE EXTENSION btree_gist` is **REFUSED to a package, and the PLATFORM
  installs it.** [corrected twice on 2026-09-07. First correction: this line
  said the extension passed the allowlist. It does not. It read the
  `is_ruled_operation` list as an allowlist when that list names the REFUSED
  object classes, and `refuses_every_documented_ruled_object_class` pins
  `CREATE EXTENSION pgcrypto` as refused. Second correction, on the owner
  ruling for `wamn-yk9l`: `btree_gist` is now installed by the platform in
  every project-environment database, from the closed list
  `wamn_control_provision::sql::PLATFORM_EXTENSIONS`, by `apply_package` before
  any package DDL. So a package still cannot install an extension, and no
  longer needs to.]

The database half, measured on the environment's own target database as the
role the loop uses: `btree_gist` 1.8 is available and the role creates it. A
table created with `constraint … exclude using gist (dock_id with =, during
with &&)` accepts the first booking and refuses an overlapping second with
`conflicting key value violates exclusion constraint`. Both probes ran inside a
transaction and rolled back, and the environment holds no residue from them.

What this means for a run. [rewritten twice on 2026-09-07, and this is the
state series 020 measures.] The strongest rung IS reachable. The platform
installs `btree_gist`, and the constraint form already passes the name
validator inside `CREATE TABLE`, so an agent that writes
`EXCLUDE USING gist (<scalar> WITH =, <range> WITH &&)` in the table it is
already creating gets the database enforcing the invariant. H-3 scores that at
the top.

[corrected 2026-09-08, owner ruling on `wamn-nvbd.15`. The paragraph above is
wrong, because the probe behind it was incomplete. The probe tested two refusal
points, the migration validator and the guest role. Both admit the constraint.
The probe never tested Introspect, and Introspect refuses it.

`crates/schema/introspection/src/postgres.rs` maps `pg_constraint` by `contype`
and has arms for `p`, `u`, `f` and `c` only. `CONSTRAINTS_SQL` filters on
namespace and `relkind = 'r'`, with no `contype` predicate. An exclusion
constraint therefore reaches the catch-all arm and returns
``unsupported pg_constraint contype `x` ``. `map_indexes` rejects the supporting
index on the same grounds.

Migrate admits the constraint, the database creates it, and Introspect refuses
it one stage later. The constraint is unreachable from inside a package.
**Lock-in-transaction is the top rung for series 030.** No arm is marked down
for the rung above it. The platform defect is `wamn-10yt.36`, ruled to be fixed
by modelling the constraint rather than by moving the refusal earlier. The H-3
fallback clause is rewritten to name any stage rather than two, and that rewrite
lands with `wamn-10yt.36`.]

[corrected 2026-09-08, owner ruling on `wamn-10yt.36`, which is now fixed. Two
sentences in the annotation above are superseded. The original text stands as
written.

`map_indexes` never rejected the supporting index. `INDEXES_SQL` already omits
every constraint-backed index, through a `NOT EXISTS` on
`pg_constraint.conindid`. The gist index behind an exclusion constraint is
constraint-backed. It never reached the `row.exclusion` test. This was measured
on a live PostgreSQL 18 while the bead was fixed. THE ONE REAL REFUSAL WAS THE
`contype` CATCH-ALL in `map_constraints`. The fix removed it.

LOCK-IN-TRANSACTION IS NO LONGER THE TOP RUNG. Introspect now MODELS an
exclusion constraint. The migration validator admits a named
`EXCLUDE USING gist` inside `CREATE TABLE`. The database creates it. The IR
carries it as `Table::exclusions`. The strongest rung is reachable end to end.
An arm that already ran cites a commit before the fix and keeps the rung it had.
An arm that cites the fix or later does not.]

A ROW LOCK ON THE PARENT ROW, TAKEN BEFORE THE OVERLAP READ, IS A VALID RUNG 2
and it is not marked down. All three runs of series 010 found it without help
and all three passed the contention invariant with it, on a platform where the
top rung did not yet exist.

An agent that creates the table first and adds the constraint second is still
refused, by a message that names CHECK and never names `EXCLUDE`. That is a
pre-priced stall in category `generator` and it is still the platform's fault.

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
  §4.6 probe shows that any stage refuses `EXCLUDE`, lock-in-transaction is the
  top rung for that run and an agent that tried the constraint and was refused
  is not marked down.
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
5. After three valid T1 runs: the stall table (§8). Do not change the brief and
   the task in the same day.
6. **A platform change restarts the series.** Runs compare only against runs on
   the same platform, so a fix to something a run stalled on opens a new series
   rather than continuing the old one. Series are numbered by decade: 001 to 009
   is the first, 010 to 019 the second. A retired series keeps its runs and its
   findings; it does not contribute to the current stall table.

   [amended 2026-09-07 by owner ruling, recorded on `wamn-nvbd.11` and stated
   again on 2026-09-08. A PLATFORM CHANGE DOES NOT OPEN A SERIES. Only an
   INSTRUMENT change does. Each arm cites its own commit, and that citation is
   what makes the arms comparable. Series 030 is the worked case. Arm 030 hit
   the `wamn-10yt.27` wall. The fix landed at `d2612972`. Arms 031 and 032 then
   ran on the fixed platform, in the same series. The renumber from 020 to 030
   was an instrument change, not a platform change.]

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

Restated for n = 3, the run count the owner ruled on 2026-09-06. The thresholds
keep their original proportions: two thirds of the runs must be valid, and `env`
may dominate at most a third of them.

SUCCEEDS when three valid T1 runs exist and every stall carries a category and a
pointer. INCONCLUSIVE if fewer than two runs are valid, or if `env` exceeds half
of all stalls in more than one run; fix the runner, rerun, rank nothing from it.
A 3/3 PASS with an empty stall table means the baseline needs no tooling for this
task and T2 opens.

Three runs buy a stall inventory, not a rate. Read the resulting table the way §2
already reads H4: it is recorded, not tested.

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

Nothing opens on fewer than two runs showing the category. At n = 3 that floor
does not move, and it is the whole protection: a category seen once is a story,
and two of three runs is the smallest evidence that separates a defect from an
accident. A category that appears in exactly one run is recorded with its pointer
and opens nothing.

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

### 8a. Series

| series | platform | runs | why it closed |
|---|---|---|---|
| 001–009 | `wamn-10yt.10.39` open: a package that ships a component could not be authored inside its own paths | 001 | the wall was fixed, so no later run is comparable to this one |
| 010–019 | the component allowlist closes over the packages, and the claim law is emitted by construction (`522a1941`) | 010, 011, 012 | open. Fixing `wamn-nvbd.9` closes it, because hiding the rubric changes what a run measures |
| 020–029 | never measured | 020, 021 | closed by the instrument, not the platform. Both runs read the grading fixture out of their own worktree, so both are leak instances and neither is comparable |
| 030–039 | the rubric, the pilot tools and the gate document's pilot section leave with the baseline commit (`wamn-nvbd.12`) | 030, 031, 032 | open |

Run 001 stands as the wall arm. Its finding is what the pilot exists to produce
and it is unaffected by the two contaminations §11 records, because a blocker
does not become less real for having been well documented.

Series 010 is three runs, all `PASS`, all on `522a1941`, one at a time on an
otherwise idle machine. It succeeds under §7.4: three valid runs exist, every
stall carries a category and a pointer, and `env` never exceeds half the stalls
in a run. The stall table lives on `wamn-nvbd.6`. Read the three passes with
`wamn-nvbd.9`: every run could read `steps.json` from its own task directory, so
the rubric was visible. The stall table is unaffected by that, because every
stall is the platform refusing something.

Series 030 is three runs on the fenced instrument, one FAIL and two PASS. Its
stall table lives on `wamn-nvbd.11`. It succeeds under §7.4: three valid runs
exist, every stall carries a category and a pointer, and `env` is 0.4 of 24.5
stall minutes. Each arm cites its own commit, because rule 6 is amended and a
platform change does not open a series.

The top category is `provisioning` at 13.7 minutes over two runs. Its largest
single message is `component-fact-conflict` after a reformat, and that message
costs 6.0 minutes in both runs. That is `wamn-10yt.38`, the finding this series
exists to produce. `component-build` and `generator` also clear the floor and open
`wamn-10yt.39` through `wamn-10yt.42`. Four categories appear in one run each and
open nothing.

## 10. Run report template

```
# <nnn>-<agent>-<task>
main commit · model id · load at launch · driver args · run cap hit? · skills present
Outcome: PASS | FAIL (items) | INVALID-ENV
Conduct: <one line on what the agent did with the rules the brief gave it>
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
  The baseline runs deliberately share the machine with build lanes, so every
  minute in this arm is a loaded-machine minute, and no run is compared to
  another by duration.
- Grader bias: machine checks decide; rubric explains; grade one run blind if a
  second grader exists.
- Prompt leakage: the brief names `--hold` and the commit rule and nothing else,
  and never the four verification cases (those are S9);
  a brief that names more is a new arm.
- **Leakage through the tree itself.** This document lives in the repository the
  agent works in, and so do the task fixtures. Run 001 quoted §4.6's `EXCLUDE`
  probe back in its report: the answer to a question the run existed to ask was
  sitting in the worktree, next to the scenario and the grader's own steps. The
  runner now removes `docs/experiments/` from the run worktree and commits the
  removal, and that commit is the run's baseline. Run 001 is contaminated on
  this axis and its H1, H3 and craft scores are read with that in mind; its
  platform finding is unaffected, because a blocker does not become less real
  for having been well documented.
- **The rubric is in the agent's own task directory.** `WAMN_PILOT_TASK_DIR`
  holds `BRIEF.md`, `SCENARIO.md`, `task.json` AND `steps.json`. The last is the
  grader's fixture: the route each step drives, the bodies, the reuse chains and
  the exact expected `error_code` literals. Every run read it. Run 010 read it as
  its FOURTH command and cites it 24 times, run 012 twenty times, run 011 seven,
  run 001 twenty-four. Removing `docs/experiments/` from the worktree, the fix
  above, does not touch this copy. So the passes in series 010 are passes against
  a VISIBLE rubric, and no result may be read as authoring from domain language
  alone. `task.json` leaks on its own too: the brief sends the agent there for
  `allowed_paths` and the same file carries the check and fence names. Filed as
  `wamn-nvbd.9`; the stall table is unaffected, because every stall is the
  platform refusing something.
- **The rubric moved, and stayed readable.** The `wamn-nvbd.9` fix put the
  fixture at the run directory root and stripped the `grade` block from the
  agent's `task.json`. The run directory is exported to the agent as
  `WAMN_PILOT_RUN_DIR`, so the rubric was still one `cat` away, and run 020
  read it. Filed as `wamn-nvbd.12`. The fixture now lives in a harness grading
  directory that nothing hands the agent, and `up` refuses the run if the
  fixture or a `grade` block is reachable from any exported path.
- **The instrument was readable too.** Both runs of series 020 read
  `tools/agent-pilot-grade` out of their own worktree, which names every check,
  every fence report and the verdict rules. The baseline commit now removes
  `docs/experiments`, every `tools/agent-pilot-*` file, and the `[AGENT-PILOT]`
  section of `docs/operations/build-and-test.md` by section fence, because the
  agent needs the gate recipes and not the paragraph saying it is measured.
  THE RULE: the instrument lives outside the tree the agent can read, or the
  agent reads it. Agents read what is there; this is not a discipline problem.
- **The first defect the fence produced.** Arm 031 is the first run whose
  transcript touches no leak channel at all. It passed twelve stages, served a
  release, and then every route step returned `schema-invalid`. The grading
  fixture posts `name` to `carrier.create` and the agent declared `carrier_name`,
  because the brief named the operations and never named a field. THE FIXTURE
  PINNED A WIRE CONTRACT THE BRIEF DID NOT STATE. That was invisible in every
  arm that could read `steps.json`, and visible the moment one could not, which
  is the fence working and the strongest evidence that the earlier passes
  measured rubric-reading. The brief now carries a data contract: per operation,
  the input and output field names and types. It carries no expected error code,
  no step and no verification list. The envelope fields `request_id` and
  `idempotency_key` are platform law rather than scenario content, so they stay
  out of the brief and the grader supplies them, and an agent that did not
  implement the claim is graded on that property.
- **Series 020 is excluded, and series 030 is the clean baseline.** Run 020
  read the fixture with one command. Run 021 read it in two, covering the whole
  file, and also graded FAIL on `check-in`. Both stay in the record as the leak
  instances. The instrument changed materially, so section 6.5 applies and the
  series is renumbered; three arms, because nothing opens on fewer than two.
- Test-set quality: a trivial set passes trivially; V3 is applied to the task's
  set before run 1, and the kill matrix is re-run when the set changes.
- Known gaps: the statement verifier (work spec F21) and the missing flow-test
  runner will produce stalls in every run; they are priced, not discovered, and
  do not count against the agent in H-6.
- Global skills: `rust-guidelines` is present via `CLAUDE.md:107` only if
  installed on the machine; the inventory is frozen across runs so arms compare
  like with like.
- Machine settings and the active output style. Run 001 measured this rather
  than assumed it: the driver inherited the machine's output style and used the
  Bash tool 154 times and no other tool, not one `Read`, `Write` or `Edit`. An
  arm that changes how an agent reaches for its tools is not the arm we think we
  are running, so the runner records the settings files by digest beside the
  skills, and every run report carries the output style it ran under. Compare
  only runs whose settings digests match.
