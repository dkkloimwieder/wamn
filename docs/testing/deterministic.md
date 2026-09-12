# Deterministic tests

A deterministic test controls the inputs, time, outcomes, and event order that its test driver owns.
Property tests generate cases, while deterministic execution controls how a case runs.
Use them together when that control serves the assertion.
Live database races still require the observations in [database tests](database-tests.md).

## Existing boundaries

The [walk tests](../../crates/execution/router/tests/walk.rs) drive real walk decisions with generated scenarios, supplied outcomes, and a fake clock.
They evaluate the [walk invariants](../../crates/execution/router/src/invariants.rs) after each applied outcome.
This tests walk decisions, not guest execution or process recovery.

The [queue invariants](../../crates/execution/run-state/src/invariants.rs) inspect pure queue decisions for leases, claimability, and attempt limits.
They do not establish database admission history, terminal-row immutability, or producer-key uniqueness.
## Event traffic

The [simulator](../../test-support/simulator/src/lib.rs) generates canonical, repeatable event bytes from the complete seeded profile and count.
It generates identifiers and timestamps within the stream, while the caller owns pacing and delivery.
The [HTTP target](../../test-support/simulator/src/emit.rs) sends authenticated array envelopes to real routes.
Simulators drive routes or consumers and never write application tables directly.
No JetStream sink exists here, and generated traffic must not fabricate internal CDC event envelopes.
This controls input bytes, not database or network scheduling.

## Replay limits

Controlled run-state SQL scheduling and general guest record/replay adapters remain separate proposed work in [the delivery plan](../plan/delivery.md).
Existing narrow controls do not establish that complete facility.
Expand a generated domain when a named defect exposes a gap.
Add guest replay support only as far as a named guest test requires.

Recording uses synthetic data in the test process, never production data or credentials.
Replay must not reach a database or network.
Keep adapters in test support without a production capture feature.
Match request identity, parameters, multiplicity, and required ordering.
Unmatched requests and unused required fixture entries must fail.
Fix guest clocks and randomness when they affect assertions.
Fixture changes require explicit recording and review.

Retain [reproduction inputs](evidence.md) for the controlled execution.
Simulation can replace a redundant sequential scenario only when it covers that scenario's actual guarantee.
It does not automatically retire process-kill, connection-loss, or contention tests.

Kani, Bolero, coverage-guided fuzzing, and production invariant tripwires remain deferred.
There is no automatic numeric-bug trigger to reopen them.
Do not add their dependencies or toolchain work without an owner decision.
