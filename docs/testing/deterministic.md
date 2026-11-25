# Deterministic tests

A deterministic test controls the inputs, time, outcomes, and event order that its test driver owns.
Property tests generate cases, while deterministic execution controls how a case runs.
Use them together when that control serves the assertion.
Live database races still require the observations in [database tests](database-tests.md).

## Existing boundaries

The [schedule test](../../crates/workflow-graph/src/schedule.rs) supplies fixed UTC minutes and checks which ones match a workflow schedule.

## Event traffic

Simulators drive routes or consumers and never write application tables directly.
Generated traffic must not fabricate internal CDC event envelopes.
This controls input bytes, not database or network scheduling.

## Replay limits

The repository has no controlled run-state SQL scheduling and no general guest record and replay adapters.
Existing narrow controls do not establish that complete facility.

Recording uses synthetic data in the test process, never production data or credentials.
Replay must not reach a database or network.
Keep adapters in test support without a production capture feature.
Match request identity, parameters, multiplicity, and required ordering.
Unmatched requests and unused required fixture entries must fail.
Fix guest clocks and randomness when they affect assertions.
Fixture changes require explicit recording and review.

Identify the relevant [reproduction inputs](evidence.md) when reporting a failure.
Simulation can replace a redundant sequential scenario only when it covers that scenario's actual guarantee.
It does not automatically retire process-kill, connection-loss, or contention tests.

The repository does not use Kani, Bolero, coverage-guided fuzzing, or production invariant tripwires.
