# Receiving correctness evidence

## Current result, 2026-09-10

The baseline passed the real Receiving guest and PostgreSQL proof at source `0673f3f54f2db1250c6b023a11d7dc5877a944da`.
A history is a sequence of commands and expected results for one test fixture.
The run executed three explicit histories, 16 generated histories, and seven boundary cases.
The deliberate business defect failed and reproduced in a fresh fixture.

The SQL lock mutation survived the suite at `9bdd6cd2`.
The final restored run passed from clean baseline `0673f3f5`, and no deliberate defect remains in the implementation.
Bead `wamn-10yt.77` owns the combined workspace sweep and integration status.
These results make no overall release-readiness claim.

The [eight-invariant brief](../../../../docs/poc/wamn_receiving_layered_application_poc_scenario.md#15-receiving-correctness-amendment-2026-09-09) maps the application rules to their state, enforcers, and proof cases.
The [build guide](../../../../docs/operations/build-and-test.md) contains the `RECEIVING-CORRECTNESS` recipe.
The [recorded live command](live-003/command.json) preserves its exact arguments and environment selection.

## Successful baseline

[Live 003](live-003/result.json) exited zero with one executed Rust integration test and no ignored tests.
Its source remained unchanged, and owned cleanup passed.
The generated histories used seed `7701` against PostgreSQL `180006`.
The receipt retains the Receiving component, overlay, schema, toolchain, and corpus identities.

Four boundary cases covered invalid status, a mixed command envelope, competing receipts, and overlapping replay.
Three more covered rollback after a receipt write, withheld application delivery, and authority restoration.
Both concurrent request cases observed two separate guest database connections waiting for a database lock held by the test.
The test then released that lock.
The rollback case observed a separate database lock wait after the receipt and claim existed inside the guest transaction.
The [case log](live-003/journey/receiving-correctness.jsonl) records each history, boundary result, and observed backend identity.

[Local 002](local-002/result.json) passed five model and failure-classification tests at `3bcab34419d71e1b9416d8177d30c5bd162e7945`.
[Local 003](local-003/result.json) repeated those five passes at the baseline source.
Both local runs left the live test ignored and unproved.
Their [local command](local-003/command.json) differs from the armed live recipe.

## Deliberate business defect

The [mutation](mutation-business-001/mutation.patch) disabled the canonical-command comparison in `replay_result` at `c66a9821de10c0c726fbd9d79976a1527bae4641`.
An idempotency key identifies a command so a retry can return its stored result.
The mutation made a changed command return the original success instead of refusing reuse of its idempotency key.
The rebuilt Receiving component changed, while the overlay and corpus retained their baseline hashes.

The [control result](mutation-business-001/result.json) exited `101` on `REC-REFUSAL/response`.
The expected result was `idempotency_conflict`, but `/receiving/record_receipt` returned HTTP `200` with no refusal.
The first explicit history detected the defect, and a second, distinct fixture reproduced it.
No generated histories or boundary cases executed in this control.
Owned cleanup passed.

The [restoration receipt](mutation-business-001/restoration.json) records commit `eb1ec64055ee75ecb3b39afb98d369ae70d6f932` and a source tree equal to baseline `0673f3f5`.
This receipt proves source restoration, not a later live pass.

## SQL lock control

The [SQL mutation](mutation-contention-001/source-mutation.json) replaced `FOR UPDATE` with `FOR KEY SHARE` in the purchase-order statement.
Normal generation and verification passed against PostgreSQL 18, and the rebuilt Receiving component changed.
Its hash became `sha256:efd57e2b16d0bd700cc1c75be6c3f4d04021d63a6b4c9019559f90901a24fb24`.
The generated SQL collection's hash became `sha256:43443f80f2c5d262d91d3c1929652dcb5fc2c932df4af3c292b61322b570ccf6`.
The overlay retained its baseline hash.

The [control result](mutation-contention-001/result.json) classifies the mutation as `survived` because the suite still passed.
One live test executed three explicit histories, 16 generated histories, and seven boundary cases, with no ignored tests.
Both concurrent request cases observed distinct database backends `260` and `262` waiting for the test's lock.
Competing receipts produced one commit and one `quantity_exceeds_remaining` refusal.
Source remained unchanged at `9bdd6cd292a10592bcd35b7d258393dfba41fe36`, and owned cleanup passed.

The original line locks and quantity constraints remained enabled.
This control does not isolate their individual effects or establish that the purchase-order lock is unnecessary.
The [restoration receipt](mutation-contention-001/restoration.json) records all four files restored exactly with fresh modification times at local commit `264b43301f45f73dea55b554738051e95bfd51ad`.
Its source tree equals baseline `0673f3f5`.

## Restored live proof

[Restored 001](restored-001/result.json) passed at unchanged, clean source `0673f3f54f2db1250c6b023a11d7dc5877a944da`.
One exact integration test executed three explicit histories, 16 generated histories, and seven boundary cases, with no ignored tests.
Owned cleanup passed, and every controlled source file matched the baseline bytes.
The full component, SQL collection, schema, compiler, and other recorded identity fields equaled the Live 003 baseline.
The [recorded command](restored-001/command.json) preserves the restored run's exact invocation.

## Retained unsuccessful attempts

[Local 001](local-001/cargo.log) exited `101` because seven evidence expressions required an unavailable `Uuid` serialization implementation.
[Live 001](live-001/result.json) timed out during image preparation with exit `124`, before creating its cluster or executing live histories.
[Live 002](live-002/result.json) exited `7` when curl could not connect to the new correctness NodePort.
Its production route prerequisite passed 13 PAT route cases, but no command-history test executed.
Its exact connection failure remains undetermined because the attempted endpoint and curl error were not retained.
Both live attempts retained passing cleanup receipts.

## Proof limits

The generated model uses bounded integer quantities and two order lines.
It does not exhaust decimal quantities, all possible histories, or scheduler interleavings.
Snapshots cover the fixture's purchase order, lines, claims, receipts, and receipt lines, including independent receipt sums.
They do not audit every database relation or detect transient writes that roll back.

The delivery-loss case withholds the application result after the HTTP adapter receives bytes and an independent observer confirms the commit.
It does not prove a network disconnect or process crash.
The business control proves fresh reproduction of an explicit failing history.
It does not establish live shrinking, although the local tests exercise shrinking classification and preservation of the original failure.
