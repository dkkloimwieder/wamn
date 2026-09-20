# Testing stabilization and repository qualification

**Status:** draft 0.3 for epic/issue decomposition  
**Basis:** current repository behavior and test ownership on `main`, especially `docs/testing/*`, `docs/operations/running-tests.md`, existing application test owners, and current delivery/test tooling.

## 1. Goal and scope

Establish a clean, trustworthy repository-wide test baseline before further feature work.

The repository already has the major testing/developer-loop mechanisms this plan previously proposed:

- typed/generated application boundaries are established;
- ordinary Receiving and WMS business tests can run through real components, production authorization, and disposable PostgreSQL without a cluster;
- `wamn-test-postgres` owns isolated test databases;
- `SQLX_OFFLINE=true` is the normal compilation mode;
- SQL/schema changes refresh/check SQLx metadata through the owning application/delivery path;
- `tools/test-changes` selects affected packages and dependents;
- ordinary UI workflow assertions have moved substantially in-process;
- cluster tests retain deployment/runtime boundaries.

This plan therefore does **not** introduce another testing framework or developer-loop architecture. It stabilizes what exists, removes superseded or duplicated coverage, and establishes the baseline that future feature work must preserve.

Applications own business fixtures, expected results, workflows, and application assertions. Platform libraries own invocation mechanics, capability setup, disposable infrastructure, and common failure reporting.

Preserve public contracts, authorization, transaction ownership, replay semantics, exact-value behavior, and deployment guarantees. Do not weaken an assertion merely to obtain a green baseline.

## 2. Testing rule

**Use the smallest existing test boundary that actually proves the behavior.**

| Behavior | Preferred proving boundary |
| --- | --- |
| Pure decisions, validation, reducers, parsing | Unit/property test |
| SQL types, constraints, locking, committed state | Real disposable PostgreSQL |
| Guest + host capability + authorization + transaction behavior | Local application runtime with real components and disposable PostgreSQL |
| Terminal/process lifecycle | Process/PTY test |
| Packaging, native runtime lifecycle, broker/CDC/materializer, deployment recovery | Cluster/deployed test |

Similar assertions at different boundaries are not automatically duplicates. Retain a higher-cost test when it proves a fact the lower-cost test cannot observe.

Mocks may test response handling. They do not establish database correctness, authorization, transaction behavior, process cleanup, or deployment recovery.

Required tests must execute their named cases. Ignored, skipped, filtered-to-zero, or missing-prerequisite runs do not count as passing evidence.

## 3. Increment A — establish the current qualification baseline

Run one deliberate local qualification against the stabilization revision. Do not repeat a broad campaign after every cleanup edit.

Include the existing owners for:

- root/apps/no-std compilation and test targets selected by repository tooling;
- repository formatting/lint/Clippy checks;
- schema/generator tests;
- SQLx metadata verification against fresh disposable schemas;
- PostgreSQL authority, RLS, transaction, rollback, replay, and relevant contention tests;
- Receiving and WMS local application business tests;
- Acme/Receiving transactional participation through production `NativePolicy`;
- operator reducer/workflow tests;
- combined host HTTP/queue lifecycle and active-work recovery tests that already run locally or in-process.

List the retained deployed tests and their prerequisites without running them.
Carry existing deployed-test results forward only as prior evidence for planning.
Keep known failures and limitations named, including `wamn-h6lm`.
Do not claim a new deployed result until Increment D.

Classify every non-green result as exactly one of:

1. **regression** — current production behavior or contract is incorrect;
2. **stale test** — assertion or setup protects a retired behavior/shape;
3. **missing prerequisite** — the required external dependency is unavailable;
4. **known external/runtime limitation** — a named unresolved behavior outside the changed implementation.

A known or classified failure remains a failure. Record it explicitly; do not silently exclude it from the baseline.

### Acceptance

- Every required local qualification owner executes at least one expected case and reports its real result.
- Regressions and stale tests are filed/fixed rather than normalized into the baseline.
- External limitations are named with their existing issue where one exists.
- The resulting baseline is reproducible from `docs/operations/running-tests.md` and existing test documentation.

## 4. Increment B — remove obsolete and duplicate test paths

Audit retained tests against current production architecture.

Delete or rewrite tests that exist only for retired implementation shapes, including where applicable:

- transaction-view token/UUID contracts superseded by participant-local WIT resources;
- pre-native-async bridges or `block_on` behavior;
- retired JSON component-call adapters where typed native contracts now own the boundary;
- executor/dispatcher/waker process topology removed by service consolidation;
- removed manifest fields or parser compatibility retained only for superseded authored formats;
- old generated-TUI paths replaced by the current generated/application-owned UI composition;
- setup/build helpers whose only consumer was deleted;
- snapshots or bookkeeping checks that protect no independently consumed artifact or contract.

For duplicated business assertions, retain the lowest-cost boundary that proves the behavior and keep higher-cost coverage only for the distinct boundary it owns.

Examples:

- quantity/idempotency/business refusal → local application test;
- PostgreSQL constraint/lock semantics → database/local application test;
- rendering/submission state → reducer/widget test;
- terminal restoration/password masking/signals → process test;
- image/startup/broker/restart/CDC/materializer behavior → cluster test.

Do not rewrite a surviving process test merely to remove Python or standardize implementation language.

### Acceptance

- Every deleted test has either a surviving equivalent assertion at the correct boundary or no longer corresponds to production behavior.
- Cluster tests contain no ordinary business assertions whose only purpose is already established locally.
- No retained test imports or configures a retired runtime/process path.
- No obsolete compatibility layer is kept solely to satisfy an old test.

## 5. Increment C — audit UI/process coverage

Ordinary application UI behavior should run in-process through existing reducers/event handlers and Ratatui test surfaces.

Audit remaining PTY/process cases and classify each as:

- **process behavior** — keep;
- **application state/rendering behavior** — move to or confirm equivalent in-process coverage, then delete the duplicate process assertion.

Retain process-level coverage for behavior such as:

- terminal initialization/restoration;
- signal/shutdown handling;
- password echo suppression;
- actual binary/process startup where the process boundary is the property.

In-process coverage should continue to own:

- navigation and editing;
- request construction;
- absent/null/value behavior;
- double-submit prevention;
- success/refusal/partial-completion/uncertain states;
- safe captured retry rules;
- target replacement/session reset;
- late-response isolation;
- rendered application state.

### Acceptance

- Every retained PTY/process assertion proves a process property.
- Application-state assertions have an in-process owner.
- No frontend behavior is weakened to remove a process test.

## 6. Increment D — qualify the real external boundaries

After cleanup and final local qualification settle, run the retained deployment/runtime layer once against the resulting topology.
This increment alone owns deployed execution and establishes the post-cleanup deployment baseline.

At minimum, retain and execute the cases that genuinely require deployment or external infrastructure:

- combined deployed host startup/readiness/shutdown;
- packaged HTTP ingress and queued execution;
- deployed active work interruption/recovery;
- broker/CDC/materializer delivery;
- packaged Receiving/Acme behavior where packaging/native deployment is the subject;
- WMS label delivery and partial-completion boundary;
- release/startup/restart cases that cannot be established in-process.

Keep setup, application assertion failures, and cleanup failures distinct.

Do not move ordinary business scenarios back into cluster tests merely to create an end-to-end journey.

Known external/runtime failures remain visible. In particular, an unresolved operator/broker-outage assertion must stay failed/limited until its own owner resolves it; do not weaken the assertion as part of testing cleanup.

### Acceptance

- Every retained deployment test identifies the external boundary it proves.
- Required prerequisites either exist and the case executes, or the case fails naming the missing prerequisite.
- Cleanup is observed after success, failure, and handled interruption.
- The deployed layer is materially smaller than the business/local layer.

## 7. Increment E — confirm the normal developer test path

Complete this increment before the deployed qualification in Increment D.
Treat the landed developer-loop mechanisms as requirements to verify, not new architecture to build.

Confirm:

### Change selection

- `tools/test-changes dry-run` selects the expected owning packages and dependents.
- `tools/test-changes run` executes the selected commands.
- A named/required test matching zero cases is rejected.
- Changes outside Cargo ownership that are read indirectly by another package have an explicit owner documented where Cargo metadata cannot infer it.

### PostgreSQL isolation

- Database-backed tests use `wamn-test-postgres` or the documented owning fixture.
- Tests do not read user-supplied PostgreSQL URLs for ordinary execution.
- Tests that mutate server-wide roles/settings hold the documented lock or own a separate server.
- Interactive development databases are never used by tests.

### SQLx

- Ordinary Rust-only builds/tests compile from committed `.sqlx` metadata with PostgreSQL unavailable.
- SQL, migration, effective-schema, or verifier-input changes refresh metadata through the owning application/delivery command.
- Check-only qualification verifies metadata against a fresh schema and does not rewrite tracked metadata first.
- Stale/missing metadata is rejected.

### Required-test behavior

- Missing prerequisites fail by name.
- Ignored or skipped tests never count as executed evidence.
- Test commands used as qualification require real executed cases.

### Acceptance

- One Rust-only application edit reaches focused feedback without SQLx preparation or cluster setup.
- One SQL edit refreshes/checks metadata through the normal path.
- One application business edit runs its selected local application/database coverage.
- The full qualification path remains available as a separate command set.

## 8. Documentation and ownership cleanup

Testing documentation should describe current behavior only.

- `docs/operations/running-tests.md` owns commands, prerequisites, test selection, database isolation, and execution mechanics.
- `docs/testing/strategy.md` owns the boundary-selection rule and test philosophy.
- `docs/testing/application-tests.md`, `database-tests.md`, and `cluster-tests.md` own assertions at those boundaries.
- Completed implementation plans belong in history; they are not competing instructions.
- Remove recipes for retired binaries, services, adapters, manifest fields, or test harnesses.
- Do not create an evidence registry, test inventory database, or second dependency graph.

Where a behavior has one authoritative recipe, link to it rather than restating the command in several plans.

## 9. Explicit non-goals

This stabilization epic does **not** add:

- new product features;
- a new test framework or DSL;
- a mandatory mutation-testing campaign;
- a coverage percentage gate;
- a broad deterministic-simulation/replay program;
- Kani or a new formal-verification engine;
- another result/evidence registry;
- a crate-count target;
- a new application schema language;
- a broad performance benchmark program.

Focused property tests, deliberate mutants, deterministic controls, or regression cases remain appropriate when they prove a concrete invariant or defect.

## 10. Delivery

Land cleanup in small commits grouped by test owner or retired mechanism.

For each change:

1. identify the current behavior/contract;
2. identify the test(s) that prove it;
3. remove or move only redundant/obsolete coverage;
4. run the smallest affected validation;
5. reuse the baseline result rather than rerunning unrelated suites.

Start with the local baseline, then complete cleanup and review the developer test path.
Complete final local qualification, then run deployed qualification once and request final owner review.
Run only the smallest affected tests during cleanup.
Run the broad local qualification after the cleanup set settles, then run the retained external/deployment layer once.

Report:

- tests removed/moved and their surviving owners;
- regressions fixed;
- stale tests deleted;
- unresolved external limitations;
- commands and executed case counts for the final baseline.

Ordinary logs remain local. No permanent per-run evidence tree is required.

## 11. Completion gate

Testing stabilization is complete when:

1. Current `main` has a reproducible local qualification baseline with every required case actually executed.
2. Regressions are fixed; stale tests are removed; external limitations are explicitly named rather than hidden.
3. Ordinary application business behavior runs without cluster/image/broker setup where those boundaries are irrelevant.
4. Cluster/deployment tests contain only assertions that genuinely require those boundaries.
5. UI application behavior is owned in-process; retained PTY/process tests prove actual process behavior.
6. SQLx offline compilation, fresh-schema verification, disposable PostgreSQL, and change-based test selection are the normal documented paths.
7. Required tests cannot report green by skipping, matching zero cases, or silently lacking prerequisites.
8. No test or helper references a retired service topology, transaction-token path, obsolete adapter, or removed manifest contract.
9. Testing documentation has one current owner for each command and testing rule.
10. The owner reviews the remaining failures/limitations and accepts the repository as the baseline for future feature work.

**No new feature epic opens until this completion gate is reviewed.**
