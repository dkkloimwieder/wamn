# Deterministic testing — replay in tests and simulation

Status: proposal for ruling. Beads become the record once ruled.
Evidence: wamn `69f4281`; golem `d667fee`. A claim with no file:line is a design
choice and says so.

Written in plain English. Short sentences. Each technical term is defined
where it first appears.

---

## 0. Adoption plan — read this first

The content below is correct and too big to adopt at once. It names four
engines and eight work items. This section is the order they enter, and the
rule that governs entry:

**Every engine enters on a measured need. The measurement is a finding the
previous phase produced.** No phase opens on a schedule.

### Phase 0 — now

One new dependency, `proptest`. Everything runs in the normal test sweep on
the stable pin.

- D1: the walk simulator, WALK-1..6, `proptest` shrinking.
- D7: generated contract tests for create-shaped commands. `wamn-10yt.19`
  needs them anyway.
- The first half of D8: invariant functions as plain `fn check(state)` for the
  walk and for run-state. No feature flag.
- A `proptest` twin for `sql_lex.rs` only (C1 target 1, the policy parser).

Exit: one injected bug in `apply` is found and shrunk to under five nodes;
every shipped command carries both claim tests.

### Phase 1 — trigger: the first guest test that needs containers to run

- D3: the in-process host harness and replay for `wamn:postgres` only.
- D5 rides D3: the harness owns the WASI context, so fixed clocks and a seeded
  random source are one builder call there.
- `wamn:connection` replay waits for a guest that calls it.

### Phase 2 — trigger: the `$now` bead lands

- D2: the run-state simulator, RUN-1..7. Not before. Not on the shadow schema;
  the shadow is refused (see Rulings).

### Phase 3 — triggers, not a schedule

Each item below is one bead with its trigger written in the body:

- libFuzzer and the nightly job: when a `proptest` finding shows that
  coverage-guided search would have found it sooner.
- Kani: when a pure numeric function (backoff, lease arithmetic) has had a real
  bug.
- `bolero`: only if both of the above adopt.
- The `invariant-tripwires` feature (second half of D8): when a pilot run would
  have been diagnosed faster with it.

The sections that follow are the design each phase implements. They do not set
the order; this section does.

---

## 1. What this spec is for

wamn gives at-least-once delivery. It promises: a run is never lost, a lease has
one owner, a redelivery is bounded, and a terminal row does not change. Today
these promises are tested by hand and by live gates. No test can run the same
sequence twice and get the same result.

This spec adds three things:

1. **Replay in tests.** Record what the host returned to a guest once. Play the
   same answers back in a test. The guest runs without a database.
2. **Deterministic simulation.** Drive the walk and the run-state decisions with
   a seed. The same seed gives the same sequence of events. A failure can be
   replayed from its seed.
3. **Fuzzing.** Feed random input to the pure parsers until one breaks.

None of this changes the runtime. The runtime keeps no log of host calls. That
is a ruling, not an accident (`docs/exe-model.md:20-22`).

## 2. Terms

- **Deterministic**: the same input always gives the same output.
- **Seed**: one number that fixes every random choice in a test run.
- **Seam**: a place in the code where a test can replace the real thing with a
  fake thing.
- **Replay**: run code again with the same answers from outside that it got the
  first time.
- **Simulation**: run code against a fake world that a test controls.
- **Invariant**: a rule that must hold after every step.
- **Mutant**: a copy of the code with one deliberate bug. A test is good if it
  fails on the mutant.
- **Harness**: the test code that drives a seam.
- **Effect**: a guest call that touches something outside the guest: a database
  call, an outbound HTTP request, a blob read or write, a stream publish or ack.

## 3. What golem does, and what we take

golem records every effect in a log called the oplog. Each effect writes a
`Start` entry before it runs and an `End` entry with the answer after it runs
(`golem-common/src/base_model/oplog/mod.rs:76`). When an agent restarts, golem
reads the log and feeds the recorded answers back. The guest does not know it is
replaying. Clocks and random numbers are recorded too, so the guest sees the same
values (`golem-worker-executor/src/durable_host/`).

golem gets two test tools from this: `simulate-crash` at any point, and a check
that new code still follows the old log (`replay_state/`).

We take:

- The idea of recorded answers, but only in tests (Part A).
- The idea of a crash at any point, but as a seed-driven event (Part B).
- Recorded clocks and random values, done as WASI configuration (Part B3).

We do not take:

- A runtime log of effects. The retired capture feature was that
  (`deploy/sql/run-state.sql:410` still carries `capture_mode`).
- Replay in production.

## 4. The four seams in wamn

| seam | what it is | evidence | what a test can replace |
|---|---|---|---|
| S1 walk | The router walk is pure. `next(&mut walk, now_ms) -> Step` decides the next step. `apply(walk, call, outcome, now_ms)` folds a node result in. No I/O. | `crates/execution/router/src/walk.rs:328, 425` | the clock and every node outcome |
| S2 run-state | The crate holds "only decisions and parameterized SQL; Postgres, clocks, and doorbells remain adapter effects". | `crates/execution/run-state/src/lib.rs:5-8`; pure functions `claim_state`, `plan_claim`, `janitor_verdict`, `orphans` (`queue/claim.rs:74-170`, `queue/janitor.rs:29-55`) | the clock and the order of events |
| S3 host plugins | Every effect goes through one host plugin: `wamn:postgres`, `wamn:connection`, `wasmcloud:blobstore`, JetStream. Each effect already opens an effect span with delivery, node, occurrence and requirement. | `crates/platform/runtime/src/plugins/effect_span.rs:1-50`, `wamn_postgres/mod.rs:421` (`impl HostPlugin`) | the answer to each effect |
| S4 guest WASI | Clocks and random numbers reach the guest through `wasi:clocks` and `wasi:random`. wash-runtime builds the WASI context; wamn does not. | `Cargo.toml:93-109`; no `WasiCtxBuilder` under `crates/platform/runtime` or `crates/execution/host` | the clock and the random source |

The router tests already drive S1 with a script of outcomes and a fake clock
(`crates/execution/router/tests/walk.rs:95-160`). The queue tests already drive
S2 with fixed times (`crates/execution/run-state/tests/queue.rs:43-100`). This
spec extends both. It does not start over.

---

## Part A — Replay in tests

### A1. Goal

Run a guest component in a unit test with no database, no network, and no
containers. The test gets the same answers the guest got in one real run.

### A2. Record — in the test process, never in the cluster

Recording is done by a test harness, in-process. The harness hosts the real
plugins under wasmtime inside the test, wraps each one with a recording
`HostPlugin`, and runs the guest against a disposable PostgreSQL with synthetic
data. This is the `virtualized_std_guest` pattern
(`tests/integration/src/virtualized_std_guest.rs`), not the cluster.

Why not the live gate: the live gate runs the production host in a cluster, and
the effect spans there carry no payloads on purpose (the blobstore span omits
keys because keys can carry tenant data). Recording bodies there would put
payload-capture code into `services/host`. That is runtime capture by another
name, and the exe-model ruling retired it (`docs/exe-model.md:20-22`).

So:

- No runtime code changes. The recording wrapper lives in `test-support/`.
- No production payload path. The host binary never sees a fixture.
- Fixtures never contain a real environment's data. The data is synthetic and
  is created by the same test that records.

Each recorded effect is one JSON line:

```json
{"seq": 12, "plugin": "wamn:postgres", "delivery": "…", "node": "…",
 "occurrence": 1, "requirement": "receipt-db",
 "request_digest": "sha256:…", "request": {…}, "response": {…}, "ms": 3}
```

- `request` is the full effect request as the plugin saw it. For a database
  call: statement id, parameters. For HTTP: method, path, headers, body. For a
  blob: key and operation. For a stream: subject and payload.
- `response` is the full effect answer.
- `request_digest` is a hash of `request` with volatile fields removed
  (timestamps the host added, trace ids).

A fixture is one file per delivery: `<component>/<case>.replay.jsonl`. The
recording test writes it. Nothing else writes it.

### A3. Replay

A replay plugin implements the same `HostPlugin` trait as the real plugin
(`wamn_postgres/mod.rs:421`). It holds one fixture as a per-plugin **multiset**
keyed by `(plugin, request_digest)`. A multiset is a set that counts how many
times each key appears. On each effect it:

1. Computes `request_digest` from the incoming request.
2. Looks up `(plugin, request_digest)` in the multiset.
3. If found: returns the recorded `response` and removes one count.
4. If not found: fails the test with the incoming request and the nearest
   recorded request printed as a diff. This is a **divergence**. The guest
   changed what it asks for.
5. At the end of the test: if any count is left, fails the test. The guest asked
   for less than it did before.

Order is not assumed. An async-lifted handler can interleave its host calls.
Where order matters, the fixture says so: a line carries `"after": <seq>` and
the plugin asserts that the named line was consumed first. Nothing else about
order is checked.

A replay plugin never talks to anything outside the process.

### A4. Where it lives

`test-support/replay/`: the recording wrapper, the replay plugins, and the
in-process host harness. Not in `crates/platform/runtime`. Not compiled into
any service binary. The runtime has no feature flag for it.

### A5. Rules

- Only the recording test that owns a fixture may re-record it, and only when
  run with `WAMN_REPLAY_RECORD=1`. A replay test never records.
- A fixture change in a commit comes with the recording test that produced it.
- A guest test that uses replay states which fixture it uses in its name.
- If a fixture is older than the component's interface version, the test fails
  before it runs. The owner re-records.
- A fixture holds synthetic data only. A reviewer who finds a real host, URL,
  tenant, or secret in a fixture rejects the commit.

### A6. What it enables

- Guest unit tests that run in milliseconds.
- Mutation tests over guest code: change the guest, replay, see what fails.
- A guest that calls the database many times is tested at every call boundary.

### A7. Exit gate

One receiving-data command runs under replay with zero containers. Delete one
line of the guest's replay branch (a mutant); the replay test fails.

### A8. Ruling (taken)

Replay is approved in the in-process shape above. The cluster-gate shape was
not approved: it would have put payload capture into the host binary. The
difference that made the ruling: where the file is written (a test process, not
the executor) and what the data is (synthetic, not an environment's).

---

## Part B — Deterministic simulation

### B1. Walk simulator (seam S1)

**What exists.** `tests/walk.rs` has `run(wiring, script, clock)`: it calls
`next`, performs the scripted outcome, calls `apply`, and repeats
(`crates/execution/router/tests/walk.rs:95-135`).

**What is added.**

- A wiring generator: from a seed, build a random wiring with N nodes, edges,
  fan-out, merges, and permitted cycles. Use the same builders as the tests
  (`node`, `edge`, `edge_on`, `wiring`).
- An outcome generator: from the same seed, choose one of `Success`, `Error`,
  `Cancelled`, `Retryable`, `RateLimited`, `Terminal`, `InvalidInput`
  (`crates/execution/router/src/outcome.rs:24`) for each invocation, with
  weights.
- A fake clock that only moves when the harness moves it.
- An invariant function that runs after every `apply`.

**Invariants (ids for traceability).**

- WALK-1: the walk ends. The number of invocations never passes the hop limit.
- WALK-2: after `Done`, `next` never returns `Invoke`.
- WALK-3: a merged node runs once per arriving token, never more.
- WALK-4: retries per node never pass the retry budget.
- WALK-5: a `Wait` is never earlier than the clock.
- WALK-6: a `Terminal` outcome ends the walk with the matching verdict.

**Shrinking.** When an invariant fails, `proptest` shrinks the case to the
smallest failing wiring and outcome script and prints it with the seed. The
generators are `proptest` strategies; nothing is hand-rolled.

**Size.** Small. Pure. Runs in the normal test sweep.

### B2. Run-state simulator (seam S2)

**What exists.** Pure decision tests with fixed times (`queue.rs`). Live tests
that run each statement once (`run_state_live.rs`, `admission_live.rs`).

**What is added.** A seeded event scheduler. The state of the world is a set of
runs and queue rows in a throwaway PostgreSQL database. Time is a number the
harness owns. From a seed, the harness draws events in order:

| event | what it does |
|---|---|
| `admit(producer_key)` | inserts a run and a queue row through the real admission statement |
| `claim(runner)` | runs the claim statement as one runner |
| `heartbeat(runner)` | renews that runner's lease |
| `complete(runner, outcome)` | writes the terminal outcome |
| `crash(runner)` | the runner stops. No statement runs. Its lease stays until it expires |
| `tick(ms)` | moves time forward |
| `janitor()` | runs the janitor sweep |
| `redeliver_same_key(producer_key)` | admits with a key already seen |

The statements are the real ones. Only the order and the clock are simulated.

**One fact gates this.** The queue statements call the database's own `now()`
inside the SQL (`crates/execution/run-state/src/queue/sql.rs:54-55, 281-283,
331, 376`). The crate's own doc says clocks remain adapter effects
(`lib.rs:5-8`), so these statements contradict their declared design. The fix
is to align code with doc: every time-dependent statement takes `$now` as a
parameter. That is its own bead (D2b). D2 opens when it lands (Phase 2).

A test-only shadow (`wamn_sim.now()` ahead of `pg_catalog` on the
`search_path`) was considered and refused: a test that passes for a reason
unrelated to the code under test, and that breaks the day a statement qualifies
`pg_catalog.now()`.

**Invariants.**

- RUN-1: a run that was admitted is never absent from both `runs` and the
  queue.
- RUN-2: at any time, at most one runner holds a live lease on one run.
- RUN-3: a run is delivered at least once unless its attempts are exhausted.
- RUN-4: attempts never pass `max_attempts`; the janitor orphans exactly the
  exhausted rows.
- RUN-5: a terminal row never changes after it is written.
- RUN-6: the same producer key admits once; the second admission returns the
  first run.
- RUN-7: after `crash`, the run is claimable again only after the lease expires.

**Reproduction.** The failure message prints the seed and the event list. A
second run with the same seed produces the same event list and the same failure.

**What this replaces.** The tooling spec's one-off "kill the executor mid-walk"
gate, which stays in force until D2 lands and closes it. A crash then becomes
one event among many, drawn thousands of times.

**Size.** Medium. Needs a throwaway database per test (the existing gate
pattern). Runs on demand and in the live sweep, not in the pure sweep.

### B3. Guest determinism (seam S4)

A guest that reads the clock or draws a random number gives a different answer
each run. Replay (Part A) fixes the effects. B3 fixes the rest.

Under ruling A8 the in-process harness hosts the guest itself, so it owns the
WASI context. Fixed clocks and a seeded random source are one builder call in
`test-support/replay/`: a fixed wall clock, a monotonic clock that moves only
when the harness moves it, and a random source seeded by the test. Set the
engine flags for NaN canonicalization and deterministic relaxed SIMD in the
same place. Confirm the builder method names against wasmtime 47.0.3 at
implementation.

No wash-runtime change is needed and nothing waits on one. Run the probe anyway
(does wash-runtime 2.8.0 let a host supply the WASI context?) and record the
answer in `docs/architecture/native-alignment-ledger.md` for the day the
production host wants it.

### B4. The deterministic delivery

B1 + Part A + B3 together give a full delivery that runs the same way every time:
a real wiring, real guests, recorded effects, a fixed clock. This is the unit
that flow tests (B4 in the tooling spec) can run without a database.

### B5. Limits

- PostgreSQL is not deterministic across two connections. Two commands that race
  for the same row cannot be simulated exactly. Contention is tested live, as a
  `concurrent` step. B2 simulates the order of events, not the database's
  internal scheduling.
- Replay fixtures drift when the guest changes what it asks. That is the point.
  The divergence message says what changed.

---

## Part C — Fuzzing and proof

Three tools answer three different questions. Only the first is in use now;
the other two enter on the Phase 3 triggers in §0.

| tool | question it answers | runs where | toolchain |
|---|---|---|---|
| `proptest` / `arbitrary` | does this invariant hold on many random structured inputs? | `cargo test`, the normal sweep, seconds | stable; the workspace pin |
| `cargo-fuzz` (libFuzzer) | is there any input that crashes or trips an assertion? Coverage-guided: it finds deep parser paths that random generation misses | a scheduled job; minutes to hours; corpora committed | nightly, in a `fuzz/` crate outside the workspace, run as `cargo +nightly fuzz`. The pin stays stable |
| Kani | within declared bounds, is there **no** input that trips an assertion? A proof, not a search | a scheduled job; minutes per proof | its own; Kani installs the compiler it needs. The pin stays stable |

Sanitizers come free with nightly. They matter little in safe Rust and matter
when a target reaches into wasmtime or FFI.

### C1. Targets

Fuzz (coverage-guided), in order of policy weight:

1. `crates/schema/generator/src/sql_lex.rs` (816 lines, two tests). It decides
   which relations a statement may touch. A wrong answer here is a permission
   bug.
2. The manifest parser (`wamn.json`) and the wiring and attachment parsers.
3. The route envelope parser: the request array, `request_id`, typed items.
4. The node-error mapping in the driver (`router_driver.rs:2725-2745`).

Prove (Kani), where the function is pure, numeric, and bounded:

5. Retry backoff: `next delay` never passes the cap and never overflows
   (`crates/execution/router/src/retry.rs`, `CAP_MS_CEILING`).
6. Lease arithmetic: `lease_deadline`, `lease_live`, `claim_state` agree for
   every `now` (`crates/execution/run-state/src/queue/claim.rs:74-170`).
7. `janitor_verdict_with_attempt`: orphans exactly the exhausted rows
   (`queue/janitor.rs:29-55`).
8. Hop-limit accounting in the walk: the counter cannot pass the limit.

Do not point Kani at a whole parser or at anything that touches PostgreSQL or
async code. Bounded, pure, numeric, or nothing.

### C2. One harness, three engines — refused for now

`bolero` would let one harness run under `cargo test`, libFuzzer, and Kani.
It is refused until both libFuzzer and Kani have adopted on their own triggers
(Phase 3). A unifying layer for engines not yet in use is abstraction ahead of
its second consumer. Until then, `Arbitrary` implementations are still shared
between `proptest` strategies and any later fuzz target, so nothing is written
twice when the trigger fires.

### C3. Rules

- Corpora are committed. A crash is a bead with the input attached.
- A fuzz target that ran one hour with no finding is recorded with its date.
- A Kani proof that needed an unwinding bound records the bound; a proof that
  timed out is not a proof and says so.
- The nightly job and the Kani job never gate `main`. Their findings become
  beads; the beads gate.

---

## Part D — Invariants as code

Where a rule lives decides how strong it is. Use the highest rung that can hold
the rule.

1. **Database constraints**: CHECK, UNIQUE, FK, EXCLUDE, `row_version`, RLS.
   They hold under concurrency and across code versions.
2. **Typed refusals**: business rules that a caller must see.
3. **Generated contract tests**: the generator knows the claim law. For each
   create-shaped command it emits two tests: same key twice returns the same
   result; same key with a changed body returns a typed refusal. This also
   proves `wamn-10yt.19` without forcing a package to declare a create.
4. **Invariant functions**: `fn check(state) -> Result<(), Violation>` for the
   walk (WALK-*) and for run-state (RUN-*). Property tests and simulators call
   them after every step. Phase 0 ships the functions and their use in tests.
   Calling them from the pilot executor as tripwires, behind a cargo feature
   `invariant-tripwires` on `wamn-executor`, is deferred to Phase 3: it is a
   production feature flag, it goes through the feature-allowlist review like
   every other, and it enters only when a pilot run would have been diagnosed
   faster with it.
5. **Guest assertions**: `assert!` only for a state that must never happen. A
   trap becomes a node failure. Redelivery repeats it. The delivery dead-letters.
   That is right for a broken invariant and wrong for a user error.

---

## Part E — Proving a test is not trivial

The four checks from the tooling spec (V3) apply to every test set in this
document:

- **Kill matrix**: each test fails on at least one mutant.
- **Traceability**: each test names an invariant id (WALK-*, RUN-*, or a brief
  invariant).
- **Predicate floor**: a test asserts a value, never only "it did not crash".
- **Executed and deterministic**: zero executed cases is red; two runs with one
  seed agree.

Mutation follows the repository's rule: exit code decides, not line count
(`docs/operations/build-and-test.md:313, 1665`). The `wamn-hopk` rule also
applies: no test reads source as text.

---

## Work items, by phase

| phase | id | item | size | exit gate |
|---|---|---|---|---|
| 0 | D1 | walk simulator: `proptest` strategies, fake clock, WALK-1..6 | small | 10 000 cases pass; one injected bug in `apply` found and shrunk to under five nodes |
| 0 | D7 | generated contract tests for create-shaped commands (`wamn-10yt.19`) | small | every shipped command carries both claim tests; mutants over the emitted SQL fail them |
| 0 | D8a | invariant functions `fn check(state)` for walk and run-state, called from tests | small | D1 and the queue tests call them after every step |
| 0 | D6a | `proptest` twin for `sql_lex.rs` | small | grammar cases committed; a known-bad statement is refused |
| 1 | D3 | in-process host harness (`virtualized_std_guest` pattern), recording wrapper, replay for `wamn:postgres`, multiset matching, divergence diff | medium | Part A exit gate |
| 1 | D4 | recording tests that own each fixture (`WAMN_REPLAY_RECORD=1`) | small | fixture written and read in one commit |
| 1 | D5 | WASI clock and random, engine flags, in the D3 harness; wash-runtime probe recorded for the ledger | small | two runs of one guest with one seed give equal output bytes |
| 2 | D2b | `$now` parameter on every time-dependent run-state statement | small product change; own bead | the queue statements take time from the caller; `run_state_live` unchanged |
| 2 | D2 | run-state simulator: event scheduler, RUN-1..7, seed reproduction | medium | 1 000 seeds pass; a seed that failed once fails the same way twice |
| 3 | D6b | libFuzzer on nightly in `fuzz/` outside the pin, targets 1–4 | small each | trigger in the bead body: a `proptest` finding that coverage-guided search would have found sooner |
| 3 | D6c | Kani proofs on backoff, lease arithmetic, janitor verdict, hop accounting | small each | trigger: a real bug in one of those functions |
| 3 | D8b | `invariant-tripwires` feature on `wamn-executor`, allowlist-reviewed, off by default | small | trigger: a pilot run that the tripwire would have diagnosed faster |
| 3 | — | `bolero` | — | trigger: D6b and D6c both adopted |

Phase 0 opens now. Phase 1 opens on the first guest test that needs
containers. Phase 2 opens when D2b lands. Phase 3 items open one bead at a time,
each on its trigger.

## Fit with planned work

| item | bead | relationship |
|---|---|---|
| D1 | `wamn-0h0g.16` (router) | tests only; no router change |
| D2 | `wamn-0h0g.11` (proof floor), `.19` (ingress) | supersedes the tooling spec's kill gate when it lands; the kill gate stays until then (tooling spec §V1) |
| D3, D4 | `virtualized_std_guest` (`tests/integration/src/virtualized_std_guest.rs`) | same in-process shape; test-support only |
| D2b | `wamn-0h0g.19` (ingress); `run-state/src/lib.rs:5-8` | aligns the statements with the crate's declared design; own bead; gates D2 |
| D5 | `native-alignment-ledger.md` | probe result recorded there; no upstream ask |
| D6 | `wamn-0h0g.22` (data access) | `sql_lex.rs` is that epic's policy parser |
| D7 | `wamn-10yt.19` | proof shape for that bead |
| D8a | tooling spec V4 | invariant functions used by the simulators |
| D8b | tooling spec A1 | tripwire feature used by the pilot; Phase 3 |

Nothing here touches the deployment boundary, the durable tier, or RLS.

## Rulings (taken)

1. Replay: approved in the in-process shape (A2, A8). The cluster-gate shape
   was refused.
2. Guest determinism: no wash-runtime change; the harness owns the context.
   The probe runs for the record only.
3. D2 replaces the one-off crash gate in the tooling spec.
4. D7 is the proof shape for `wamn-10yt.19` and supersedes the earlier
   "emission tests plus mutants" acceptance.
5. D2b: file the `$now` bead. The shadow schema is refused.
6. `bolero`: refused for now; Phase 3 trigger.
7. Tripwires: deferred with D8b; Phase 3 trigger.
8. Adoption follows §0: every engine enters on a measured need, and the
   measurement is a finding the previous phase produced.

## Open

Nothing. Phase 0 can start.
