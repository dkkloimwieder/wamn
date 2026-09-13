# Remaining delivery work

Current local development commands belong in [development operations](../operations/development-loop.md).
Current release commands belong in [delivery operations](../operations/delivery.md), and application test methods belong in [testing](../testing/application-tests.md).
Beads records implementation findings and acceptance status.

## CI-provider configuration

The selected CI service will invoke the repository commands that an authorized operator can run directly.
Source hosting, CI execution, artifact storage, and deployment targets remain independent choices.
Provider configuration will supply triggers, credentials, and job order.
It will contain no second implementation of WAMN validation or new CI abstraction layer.

Change checks precede integration.
Qualification and publication use a selected integrated revision, initially from `main`.
A release tag or explicit invocation can also select a candidate.
A successful review build does not qualify different integrated bytes.
Publication credentials must stay out of jobs that execute untrusted changes.

## Conditional extensions

Finer caches, database templates, remote caches, and precise test selection require a measured problem after coarse reuse.
Broader deterministic testing requires a named state or failure guarantee that existing tests cannot cover economically.
The owner retired `wamn-54b0` after its Phase 0 work closed.
These three possibilities require a named need and a new Bead:

- Supply time explicitly to time-dependent run-state SQL, identified as D2b in the earlier design.
- Control event scheduling over real run-state SQL, identified as D2.
- Record synthetic guest effects and fix guest clocks or randomness for a named test, identified as D3–D5.

These labels are design references, not newly created tasks.
The current [deterministic test limits](../testing/deterministic.md#replay-limits) continue to apply.
No production effect capture or full-platform simulator is required.

Formal verification, Bolero, broad fuzzing, and automated mutation campaigns remain deferred.
Advanced attestations, GitOps controllers, canaries, and automatic rollback need an actual deployment or supply-chain requirement.
The proposal requires no coverage percentage, multi-environment rollout program, or repeated performance campaign.
