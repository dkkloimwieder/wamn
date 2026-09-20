# Testing stabilization and repository qualification

**Status:** draft 0.4 for epic/issue decomposition

**Basis:** current repository behavior and test ownership on `main`, especially `docs/testing/*`, `docs/operations/running-tests.md`, existing application test owners, generator/control-plane tests, and current delivery/test tooling.

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

This plan therefore does **not** introduce another testing framework or developer-loop architecture. It stabilizes what exists, removes application-specific coupling from platform tests, removes superseded or duplicated coverage, and establishes the baseline that future feature work must preserve.

Applications own business fixtures, expected results, workflows, and application assertions. Platform libraries own invocation mechanics, capability setup, disposable infrastructure, generator/control-plane fixtures, and common failure reporting.

Preserve public contracts, authorization, transaction ownership, replay semantics, exact-value behavior, SQL verification, and deployment guarantees. Do not weaken an assertion merely to obtain a green baseline.

## 2. Testing and fixture rules

### Use the smallest proving boundary

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

### Platform fixtures do not come from shipped applications

A platform test must not depend on a shipped application merely to obtain representative:

- schema/catalog data;
- `wamn.json`;
- authored SQL;
- generated contracts or generated Rust;
- WIT dependency trees;
- package/release structure.

Use small platform-owned fixtures under the owning platform crate.

Shipped applications prove application behavior and intentional release qualification. They are not the generic fixture library for the generator, control plane, or runtime.

This rule does **not** remove intentional real-application acceptance from release qualification. A qualification case that exists specifically to prove the shipped Receiving/Acme or WMS release remains an application test.

## 3. Increment A — establish the current local qualification baseline

Run one deliberate **local** qualification against the stabilization revision. Do not repeat a broad campaign after every cleanup edit.

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
Do not claim a new deployed result until Increment E.

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
- Existing deployed failures are recorded as historical/prior evidence only, not current execution.
- The resulting local baseline is reproducible from `docs/operations/running-tests.md` and existing test documentation.

## 4. Increment B — decouple platform tests from application fixtures and remove obsolete coverage

Complete the work below **in order**. Later steps assume earlier fixture ownership has settled.

### B1. Build one platform-owned generator fixture

Create one small fixture under `crates/schema/generator/tests/support/`.

Requirements:

- no path under `apps/`;
- deliberately not Receiving-shaped;
- small catalog + manifest + SQL;
- enough supported shapes to cover generator tests that genuinely need them.

Include only capabilities actually exercised by generator tests, such as:

- CRUD/get/query/update shape;
- revision/concurrency field;
- required/omittable and nullable/non-null fields;
- custom operation;
- business error details plus derived platform errors;
- authored SQL;
- relation access/locking metadata;
- client/TUI descriptors where their tests need them.

Do not build a second sample application. This is a test fixture owned by the generator.

### B2. Move generator tests out of Receiving

Audit `apps/wamn_receiving/tests/generation.rs`.

For every test:

1. check whether an equivalent assertion already exists in `crates/schema/generator/tests`;
2. if equivalent, delete the application-level duplicate;
3. if distinct and platform/generator-owned, port it to the B1 fixture;
4. if genuinely Receiving business behavior, move it to the appropriate Receiving test owner rather than the generator fixture.

Delete the generated-output drift test if it only proves that committed generated files equal the current generator output and no separately consumed contract depends on that assertion.

End state: delete `apps/wamn_receiving/tests/generation.rs`.

### B3. Remove shipped-application inputs from generator tests

Replace use of real application packages in generator tests with B1 fixtures.

Audit at least:

- `crates/schema/generator/tests/client_emitter.rs`;
- `crates/schema/generator/tests/tui_emitter.rs`;
- `crates/schema/generator/tests/client_projection.rs`;
- generator tests that read WMS WIT dependencies;
- unit tests in generator WIT emission that use `receiving.record_receipt` only as sample vocabulary.

After this step, generator tests should not require `apps/wamn_receiving`, `apps/client_acme_receiving`, or `apps/wamn_wms` merely to construct representative input.

### B4. Restore generator invariants lost during manifest simplification

Add focused generator tests on the B1 fixture for:

1. the platform-derived error set/detail shape appropriate to each supported action;
2. refusal of the obsolete authored manifest format that attempted to redeclare fixed platform protocol/error details.

These tests verify the new derived behavior directly. They must not restore the deleted authored boilerplate.

### B5. Move SQLx verifier ownership into platform/generator tooling

Do **not** simply delete `receiving_sqlx_verifier.rs` or `client_acme_sqlx_verifier.rs`.

They are currently executable verifier targets used by SQLx preparation/check and release qualification.

First establish a platform-owned verifier mechanism that preserves:

- verification of the exact generated + authored SQL corpus;
- actual PostgreSQL bind/result types and nullability;
- package-specific effective schema/search path;
- `.sqlx` preparation/check behavior;
- delivery qualification's requirement that the candidate SQL has been freshly checked.

Update `crates/control/lib/src/delivery/sqlx.rs`, qualification, and any local generation path to use the replacement.

Only after the replacement is executing the same required verification may the application-specific verifier targets and their Cargo entries be deleted.

### B6. Remove application tests that inspect generated files only

Delete tests whose only purpose is to compare application code against generated files or generated vocabulary after the generator/platform layer owns that contract.

Audit specifically:

- generated-contract/vocabulary tests in `apps/*/data/src/error.rs`;
- `apps/wamn_wms/tests/wms_wiring_shape.rs`;
- other tests reading files under `apps/*/generated/` as test assertions.

Delete assertion-by-assertion, not file-by-file.

Keep application tests that independently verify runtime error classification, application transformation, wiring behavior, or business semantics even if they live in the same source file.

For `wms_wiring_shape.rs`, move distinct platform wiring validation to the owning platform test if it has no equivalent; delete only bookkeeping/generated-file assertions.

### B7. Move platform behavior out of Receiving route tests

Audit `apps/wamn_receiving/tests/route_authentication_live.rs` and its modules against existing owners in `tests/integration/`, especially the existing `route_authentication_live` platform module.

Classify each case as:

- Receiving application/business behavior → keep under Receiving;
- generic session/authentication/runtime/startup/delivery behavior → existing platform owner if equivalent, otherwise move to `tests/integration/`;
- deployed application acceptance → keep with the deployed application owner;
- duplicate → delete.

Check for existing equivalents before moving code.

The goal is not to move thousands of lines mechanically. The goal is to stop Receiving being the generic platform authentication/runtime test application.

### B8. Stop control-plane tests copying Receiving as a generic package fixture

Replace real Receiving/Acme package copies in control-plane tests with small purpose-built platform fixtures where the test is about generic control-plane behavior.

Audit at least:

- `services/ctl/src/ui.rs`;
- `crates/control/lib/tests/apply_package_live.rs`;
- `crates/control/lib/src/push_component.rs`;
- `crates/control/lib/src/publish_release.rs`;
- `crates/control/lib/src/dev/coordinator.rs`.

Create fixture packages beside the owning test/support code as needed. Reuse B1 only when the input model is genuinely the same; do not force every subsystem through one giant fixture.

Retain intentional real-application release acceptance. Do not replace Receiving/Acme/WMS where the purpose of the test is to qualify those shipped artifacts themselves.

### B9. Establish one WIT source/materialization path

The repository currently has multiple copied WIT dependency trees guarded by coherence tests.

Do not delete the coherence tests first.

First:

1. enumerate every WIT authority and vendored copy;
2. identify why each copy exists as a build input;
3. choose one canonical source for each platform WIT package;
4. define one materialization/dependency path for consumers that require local WIT directories;
5. update guest/platform build consumers to use that path.

Only after independently editable copies are gone should the corresponding `*_wit_coherence.rs` tests be deleted.

No WIT registry or second interface-description system is introduced.

### B10. Decide what generated application output remains committed

At the reviewed snapshot there are 295 files under `apps/*/generated/`.

Do **not** begin with a deletion target.

After B2–B9:

1. enumerate every remaining reader of committed generated output;
2. classify it as build input, publication/distribution contract, offline evidence, or obsolete;
3. keep generated artifacts that must be available without regeneration for a real build/publish/distribution reason;
4. move reproducible intermediates to build output where no real consumer requires them committed;
5. update clean-checkout build and publication paths before untracking anything.

Preserve `.sqlx` metadata or other offline evidence where the owning tool still requires committed input.

### Acceptance for Increment B

- Generator tests have no dependency on shipped applications merely as fixtures.
- `apps/wamn_receiving/tests/generation.rs` is gone.
- Every moved/deleted generator test has either an equivalent current owner or a deliberate deletion rationale.
- Derived platform error behavior and obsolete-manifest refusal have direct generator-level tests.
- SQLx qualification still checks exact application SQL before application verifier targets are removed.
- Application tests no longer inspect generated files where the platform generator already owns the contract.
- Receiving route tests contain Receiving/application behavior rather than generic platform authentication/runtime coverage.
- Generic control-plane tests use platform-owned fixtures.
- WIT has one authority/materialization path per package and no copy-coherence tests remain solely to police editable duplicates.
- The committed generated-output set has an explicit remaining consumer for every retained artifact class.

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

## 6. Increment D — confirm the normal developer test path

Complete this increment before deployed qualification.
Treat the landed developer-loop mechanisms as requirements to verify, not new architecture to build.

### Change selection

Confirm:

- `tools/test-changes dry-run` selects the expected owning packages and dependents;
- `tools/test-changes run` executes the selected commands;
- a named/required test matching zero cases is rejected;
- non-Cargo inputs read indirectly by packages have explicit selection ownership where Cargo metadata cannot infer it.

The B1/B8 fixture changes must not accidentally cause ordinary application edits to select unrelated platform suites.

### PostgreSQL isolation

Confirm:

- database-backed tests use `wamn-test-postgres` or the documented owning fixture;
- tests do not read user-supplied PostgreSQL URLs for ordinary execution;
- tests that mutate server-wide roles/settings hold the documented lock or own a separate server;
- interactive development databases are never used by tests.

### SQLx

Confirm after B5:

- ordinary Rust-only builds/tests compile from committed `.sqlx` metadata with PostgreSQL unavailable;
- SQL, migration, effective-schema, or verifier-input changes refresh metadata through the owning application/delivery command;
- check-only qualification verifies metadata against a fresh schema and does not rewrite tracked metadata first;
- stale/missing metadata is rejected;
- removal of application-specific verifier test targets did not reduce the SQL corpus being checked.

### Required-test behavior

Confirm:

- missing prerequisites fail by name;
- ignored or skipped tests never count as executed evidence;
- qualification commands require real executed cases.

### Acceptance

- One Rust-only application edit reaches focused feedback without SQLx preparation or cluster setup.
- One SQL edit refreshes/checks metadata through the normal path.
- One application business edit runs its selected local application/database coverage.
- Platform fixture edits select their owning platform tests rather than unrelated application tests.
- The full qualification path remains available separately.

## 7. Increment E — qualify the real external boundaries once

After cleanup and final local qualification settle, run the retained deployment/runtime layer once against the resulting topology.

This increment alone owns new deployed execution and establishes the post-cleanup deployment baseline.

At minimum, retain and execute cases that genuinely require deployment or external infrastructure:

- combined deployed host startup/readiness/shutdown;
- packaged HTTP ingress and queued execution;
- deployed active-work interruption/recovery;
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

## 8. Documentation and ownership cleanup

Testing documentation should describe current behavior only.

- `docs/operations/running-tests.md` owns commands, prerequisites, test selection, database isolation, and execution mechanics.
- `docs/testing/strategy.md` owns the boundary-selection and platform-fixture rules.
- `docs/testing/application-tests.md`, `database-tests.md`, and `cluster-tests.md` own assertions at those boundaries.
- Completed implementation plans belong in history; they are not competing instructions.
- Remove recipes for retired binaries, services, adapters, manifest fields, WIT copies, generated-file checks, or test harnesses.
- Update SQLx documentation after B5 so it no longer names deleted verifier targets.
- Do not create an evidence registry, test inventory database, fixture registry, or second dependency graph.

Where a behavior has one authoritative recipe, link to it rather than restating the command in several plans.

## 9. Explicit non-goals

This stabilization work does **not** add:

- new product features;
- a new test framework or DSL;
- a mandatory mutation-testing campaign;
- a coverage percentage gate;
- a broad deterministic-simulation/replay program;
- Kani or a new formal-verification engine;
- another result/evidence registry;
- a fixture registry;
- a crate-count target;
- a new application schema language;
- a broad performance benchmark program.

Focused property tests, deliberate mutants, deterministic controls, or regression cases remain appropriate when they prove a concrete invariant or defect.

## 10. Delivery

Land cleanup in small commits grouped by test owner or dependency edge.

For each change:

1. identify the current behavior/contract;
2. identify the current test(s) and fixture(s) that prove it;
3. check for equivalent coverage before moving or recreating it;
4. move platform fixtures out of application ownership where required;
5. remove only redundant/obsolete coverage;
6. run the smallest affected validation;
7. reuse the baseline result rather than rerunning unrelated suites.

Sequence:

```text
local baseline
→ B1 fixture
→ B2–B4 generator decoupling
→ B5 SQLx verifier ownership
→ B6 generated-file test cleanup
→ B7 platform/application test ownership
→ B8 control-plane fixture cleanup
→ B9 WIT ownership
→ B10 generated-output decision
→ UI/process audit
→ developer-path verification
→ final local qualification
→ one deployed qualification
→ owner review
```

Report:

- tests removed/moved and their surviving owners;
- application dependencies removed from platform tests;
- fixtures introduced and their owner;
- generated/WIT copies removed;
- regressions fixed;
- stale tests deleted;
- unresolved external limitations;
- commands and executed case counts for the final baseline.

Ordinary logs remain local. No permanent per-run evidence tree is required.

## 11. Completion gate

Testing stabilization is complete when:

1. Current `main` has a reproducible local qualification baseline with every required case actually executed.
2. Regressions are fixed; stale tests are removed; external limitations are explicitly named rather than hidden.
3. Generator/control-plane/runtime tests do not depend on shipped application files merely as generic fixtures.
4. Ordinary application business behavior runs without cluster/image/broker setup where those boundaries are irrelevant.
5. Cluster/deployment tests contain only assertions that genuinely require those boundaries.
6. UI application behavior is owned in-process; retained PTY/process tests prove actual process behavior.
7. SQLx offline compilation, fresh-schema verification, disposable PostgreSQL, and change-based test selection are the normal documented paths.
8. SQLx verification still covers the exact application SQL corpus after verifier ownership is cleaned up.
9. Required tests cannot report green by skipping, matching zero cases, or silently lacking prerequisites.
10. WIT packages have one canonical source/materialization path rather than editable copies guarded by coherence tests.
11. Every committed generated-output class has a real build, publication, distribution, or offline-evidence consumer.
12. No test or helper references a retired service topology, transaction-token path, obsolete adapter, removed manifest contract, or obsolete generated artifact.
13. Testing documentation has one current owner for each command and testing rule.
14. One post-cleanup deployed qualification has run, with remaining external failures explicitly recorded.
15. The owner reviews the remaining failures/limitations and accepts the repository as the baseline for future feature work.

**No new feature epic opens until this completion gate is reviewed.**
