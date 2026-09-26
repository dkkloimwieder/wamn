# WMS assessment

The prototype remains useful within its finite domain.
The revised model separates inventory disposition, inventory lifecycle, and packaging lifecycle.
It follows the owner's target business rules, including equal-disposition merge and explicit inventory location with a co-location invariant.
The [contract](README.md) records all thirteen requested properties and the finite domain.
Task `wamn-s43x.11` owns this correction. Production alignment remains separate in `wamn-s43x.10`.

## Evidence and execution

The initial production review used revision `1931d925f15e3e33ef2fc4899a8a46e96f0b4125`.
The scenario, schema, implementations, SQL, and tests at that revision define the former production baseline.
The owner's latest work order and answers define the new Inventory/Packaging target.
The explicit-location model revision changed no production code.
Task `wamn-s43x.10` subsequently replaces production with the target model.
The formal source and measured proof results remain unchanged by that production work.

The toolchain is Kani 0.68.0, CBMC 6.11.0, and nightly Rust dated 2026-08-21.
The [run instructions](../../../docs/operations/running-tests.md#wms-formal-model) reproduce the experiment.
All eight Kani harnesses passed, and all eighteen cover properties were satisfied.
A cover property demonstrates that a specified case is reachable.
The final suite exited with status zero and kept loop bound four, unwinding assertions, and default assertion reachability enabled.
No business assertion, reachable unsupported construct, or loop-bound assertion failed in the correct model.

| Harness | Covers satisfied | Solver time |
| --- | --- | --- |
| `packaging_closure_preserves_history_and_replay` | 0 | 93.94 seconds |
| `later_merge_cannot_change_split_history_or_replay` | 0 | 81.71 seconds |
| `split_history_is_complete` | 0 | 24.76 seconds |
| `original_result_and_changed_intent` | 1 | 167.76 seconds |
| `quantities_and_closed_inventory` | 8 | 398.70 seconds |
| `complete_transactions` | 5 | 197.28 seconds |
| `state_and_history_preservation` | 3 | 215.81 seconds |
| `initialization` | 1 | 5.54 seconds |

The successful solver times total 1185.49 seconds, about 19.8 minutes on this host.
These are host observations, not a controlled benchmark.
The total covers the final suite and excludes compilation and the separate mutation experiment.
Six native examples passed. Standalone Clippy with warnings denied and Rust formatting also passed.
Kani emitted configuration, edition-compatibility, and unreachable-construct warnings.
No unintended business counterexample appeared within the declared domain.

The source hashes identify the measured model, proofs, and native examples:

- `model.rs`: `3fdd28f4514b12532c93a78ea03c8e0bdba331b99ebf064680253528a8de04b9`
- `proofs.rs`: `fb1c863d633b472915abf69086cdd9ab6580457a4fcd98f0d7858fa70d0af517`
- `tests.rs`: `1e526718e1b414d55a8c92c4beb1a5773ac310b65e4b181d8f7cafffd4ae99cd`

Raw logs remain in `/tmp/wamn-explicit-location-final/` and `/tmp/wamn-explicit-location-mutant/` on this host.
This assessment preserves the results because temporary directories are not durable project storage.

The previous run at `2b7c68c1a` used derived inventory location.
Its recorded results do not establish the corrected explicit-location model.

## Deliberate defect

The [mutation patch](missing-transaction.patch) omits the new inventory transaction during split.
The command still creates stock, and total quantity remains unchanged.
The `split_history_is_complete` harness requires a transaction for the new inventory.
Kani found a source with three units and a request to split one unit.
The result contains two units in the source and one in the new inventory.
The new inventory lacks its transaction row, so conservation holds but history completeness fails.
The assertion `created inventory lacks its transaction` fails, and Kani prints the concrete input `3`.
The defective run exited with status one after 79.81 seconds of solver time.
After reversing the patch, the affected proof passed in 33.19 seconds and exited with status zero.
The patch affected only an owned temporary copy.

## Location correction

The previous model derived inventory location from packaging. The owner explicitly replaces that rule.
Inventory now stores its own current physical location, separate from packaging location.
Open inventory must be co-located with its packaging, but equality is an invariant rather than a derived field.
Move and split accept explicit destination locations and record direct inventory location snapshots.
The transaction observer reconstructs location from transaction rows without reading packaging location.
The bounds and command set remain unchanged. Packaging relocation remains outside this correction.

## Production alignment

Task `wamn-s43x.10` replaces the old POC schema, APIs, commands, generated clients, fixtures, and tests.
The earlier source baseline remains available at Git commit `df515100b`.
That baseline combined pallet state with stock status and reconstructed some replay results from mutable state.
Beads `wamn-s43x.1`, `wamn-s43x.5`, and `wamn-s43x.6` track the replay, closed-inventory, and incomplete-history findings.

The replacement uses independent inventory disposition, inventory lifecycle, and packaging lifecycle.
Move and split explicitly change inventory location and packaging references.
Merge requires equal product and disposition, closes source inventory, and preserves packaging metadata.
Every inventory mutation and its complete transaction rows share one explicit PostgreSQL transaction.
The claim stores the whole returned result before commit.
History failure rolls back state changes, earlier history inserts, and the claim.

The package grants only insertion and reading on `InventoryTransaction`.
No trigger or compatibility path supplies ledger atomicity.
Application row locks protect business validation and packaging closure.
Admitted locked command paths enforce co-location, not a universal database constraint.
Database constraints retain structural identity, reference, quantity, and lifecycle rules.
Administrative fixture creation is a baseline operation outside the public command model.

## Property and test mapping

The [local runner](../tests/local_business.rs) calls the real-route assertions in [wms_runtime_live.rs](../tests/wms_runtime_live.rs).
The tests supplement the bounded formal model. They do not prove production equivalence.

| Required property | Formal obligation | Application assertion |
| --- | --- | --- |
| Move preserves quantity | `quantities_and_closed_inventory` | Contended moves retain stock and explicitly select destination packaging and location. |
| Split and merge conserve quantity | `quantities_and_closed_inventory` | Available and held split/merge cases compare resulting quantities. |
| Adjustment changes quantity | `quantities_and_closed_inventory` | Adjustment preserves a positive decimal count and requires a reason. |
| Split lineage | `split_history_is_complete` | Both split rows share an operation, and the new row names the source. |
| Merge lineage | `complete_transactions` | Both merge rows name the source and target identities. |
| Complete affected history | `complete_transactions` | History assertions inspect both identities. Failed second-row insertion rolls back all state. |
| Immutable history | `state_and_history_preservation` | Later commands preserve prior rows. Database grants refuse application updates and deletion. |
| Original replay without mutation | `original_result_and_changed_intent` | Full split, adjust, and merge results survive later mutations. |
| Later changes preserve replay | Two-operation history harnesses | A held split replays its original result after its child closes. |
| Changed intent refuses | `original_result_and_changed_intent` | Changed split intent refuses under the existing key. Claim-law tests exercise the SQL claims. |
| Closed inventory refuses | `quantities_and_closed_inventory` | All four ordinary commands refuse closed inventory with complete state snapshots. |
| Packaging/location snapshots | `complete_transactions` | Transactions retain explicit location and packaging values. Wrong destination location refuses. |
| Disposition and packaging lifecycle | `state_and_history_preservation` | Unequal dispositions refuse. Empty packaging closes. Closed packaging refuses inventory. |

The six formal native examples retain their original coverage and source hashes.
They include a packaging-only location edit that leaves inventory location unchanged and violates co-location.
Production supplies no packaging-location mutation command in this phase.

## Production test results

The replacement passed the local runtime suite on a fresh PostgreSQL database.
The suite ran real WMS components through HTTP and finished in 51.86 seconds.
It also exercised packaging creation and replay after packaging closure.
The forced history failure rejected the second split row after the earlier writes.
The inventory, history, and claim snapshots remained unchanged after rollback.

The native WMS suite passed twelve tests, including offline SQL compilation, publication contracts, and wiring shape.
Six tests require explicit runtime or cluster inputs and remained ignored in that native command.
The separate local runtime command ran the inventory case described above.
The generated terminal example passed three native tests and both synthetic terminal scenarios: success and refusal.
The synthetic scenarios do not establish deployed cluster behavior.

Generator unit tests passed all forty-three cases, and browser component generation passed twenty-six cases.
The browser TypeScript compilation passed after regeneration of WMS and Receiving clients.
Guest, generator, and all-target WMS test Clippy runs passed with warnings denied.
Claim-law, journey-schema, and label-template tests passed.
The small seed loaded ten products, ten locations, ten packaging records, and nineteen inventory records with platform audit stamps enabled.

The formal Rust source hashes still match the completed Kani and deliberate-defect runs recorded above.
Production alignment changed no formal Rust source, so those runs were not repeated.
The deployed cluster cases did not run during this production correction.
The subsequent `cluster::released_wms_routes` run at `df64aed84` failed during host startup, before route assertions.
Its fixture omitted the session issuer required by the published routes. Bead `wamn-s43x.14` tracks this failed gate.
The command took 1534.52 seconds, including builds, and removed its disposable resources.
Bead `wamn-g4kj` retains the existing unattached label-workflow finding.
Raw production logs remain in `/tmp/wamn-production-alignment/` on this host.


## Practicality and limits

The model keeps two inventory identities, two claims, two operations, and a six-unit total.
Two packaging identities make packaging references and lifecycle explicit without modeling infrastructure.
It remains smaller than the four production command files alone, excluding SQL and generated code.
The business model contains 473 lines, with 496 lines of proofs and 312 lines of native examples.
The earlier production baseline contained 1,296 lines across its four command files, before SQL and generated dependencies.
The transaction observer tests history completeness independently of quantity conservation.
The measured solver cost appears in the execution record above. Routine CI cost remains unmeasured.

The proofs establish behavior of this finite Rust model, not production equivalence or an unbounded warehouse system.
Kani establishes empty initialization, complete new operations, and preservation of existing operations.
A separate inductive argument composes those obligations within the modeled domain.
No individual harness checks arbitrary-length history.

The owner's answers resolve merge disposition, location ownership, and packaging closure for this phase.
Unpackaged stock, lot/serial identity, multiple products, decimal quantities, and concurrency remain excluded rather than assigned invented semantics.
Adjustment to zero retains the current refusal rule, and no broader adjustment authorization policy is inferred.
The prototype does not justify a shared framework or DSL.
