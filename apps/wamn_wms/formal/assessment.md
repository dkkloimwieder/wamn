# WMS assessment

The prototype remains useful within its finite domain.
The revised model separates inventory disposition, inventory lifecycle, and packaging lifecycle.
It follows the owner's target business rules, including equal-disposition merge and explicit inventory location with a co-location invariant.
The [contract](README.md) records all thirteen requested properties and the finite domain.
Task `wamn-s43x.15` extends this model with Phase 1 packaging relocation.
The earlier explicit-location correction belongs to `wamn-s43x.11`, and the initial production alignment belongs to `wamn-s43x.10`.

## Phase 1 packaging relocation

The owner permits empty open packaging relocation and requires fresh same-location requests to refuse.
Exact replay precedes current-location validation.
Relocation changes packaging location and every assigned open inventory location explicitly.
Closed inventory remains historical and does not move.
The existing two-identity bounds remain unchanged.

The independent history observer requires one row per assigned open inventory identity.
It reconstructs each inventory location from transaction values, without reading current packaging state.
The command preserves quantities and keeps separate held and available identities.
Empty relocation stores a replay result but creates no inventory transaction rows.

The model stages business state, transaction history, and the stored result before publishing the final state.
Failure injection after each stage establishes full-state preservation, including existing claims and history.
This is a business atomicity obligation, not a proof of PostgreSQL rollback or lock behavior.
Production tests must exercise actual failures and concurrent membership changes separately.

`relocation_atomicity_and_refusals` covers arbitrary valid starting states, failures, empty relocation, two affected identities, and closed historical references.
`relocation_history_is_complete` exercises two identities with different dispositions and independently reconstructs both transitions.
`later_relocation_preserves_history_and_replay` relocates out and back, then replays the original result.
The general transition, history, conservation, and changed-intent proofs include relocation as another command.

The local `local_business::packaging_relocation` case passes through the real guest, HTTP route, authorization, and PostgreSQL transaction.
It checks two open identities with different dispositions, one closed historical identity, empty relocation, no-op refusal, and complete transaction values.
A forced failure on the second transaction row rolls back packaging, both inventory updates, the first transaction, and the claim.
Concurrent relocation admits one revision and refuses the other.
A controlled concurrent split changes membership while relocation waits for a lock.
Relocation returns `retry` without partial changes, and its next attempt moves all three open identities.
Later relocation and adjustment preserve prior history and original replay results.
Empty relocation also replays its original result after packaging closure.

Production returns the packaging snapshot and operation identity.
The formal result contains the complete bounded state to test preservation independently of later mutations.
Neither result is reconstructed from current inventory.
The local case passed in 49.11 seconds, or 64.70 seconds including compilation.
The initial run exposed timezone-dependent test rendering, which the fixture now fixes to UTC.
No Phase 1 cluster run, production kernel extraction, conformance generator, or composition work is included.

The following mapping connects the Phase 1 obligations to their test boundaries.
The PostgreSQL assertions live in `tests/wms_runtime_live/relocation.rs` and run through `local_business::packaging_relocation`.

| Property | Kani obligation | PostgreSQL/local assertion |
| --- | --- | --- |
| Explicit co-location and per-identity conservation | `quantities_and_closed_inventory` | Exact locations, revisions, quantities, dispositions, and closed records |
| Complete immutable history | `complete_transactions`, `relocation_history_is_complete` | Exact `from_*` and `to_*` fields for both open identities |
| Refusal preservation and atomic failure | `relocation_atomicity_and_refusals` | Whole-state snapshots for no-op, lifecycle, revision, missing identity, and second history-write failure |
| Prior history and replay preservation | `state_and_history_preservation`, `later_relocation_preserves_history_and_replay` | Replay after relocation and adjustment, unchanged earlier rows, empty replay after closure |
| Changed intent refuses | `original_result_and_changed_intent` | Same key with a different destination refuses without mutation |
| Concurrent membership is complete | Outside the infrastructure-free model | A blocked split adds an identity, relocation refuses with `retry`, then relocates all three identities |

## Phase 1 measured results

All eleven Kani harnesses pass, and all twenty-six cover properties are satisfied.
The run uses Kani 0.68.0, CBMC 6.11.0, loop bound four, and the existing two-identity domain.
Ten native examples, standalone Clippy, and formatting also pass.
The following table retains one successful result per harness.

| Harness | Covers satisfied | Solver time |
| --- | --- | --- |
| `later_relocation_preserves_history_and_replay` | 0 | 207.30 seconds |
| `relocation_history_is_complete` | 0 | 32.58 seconds |
| `relocation_atomicity_and_refusals` | 8 | 331.07 seconds |
| `packaging_closure_preserves_history_and_replay` | 0 | 99.57 seconds |
| `later_merge_cannot_change_split_history_or_replay` | 0 | 126.77 seconds |
| `split_history_is_complete` | 0 | 60.61 seconds |
| `complete_transactions` | 5 | 336.10 seconds |
| `initialization` | 1 | 6.62 seconds |
| `original_result_and_changed_intent` | 1 | 335.24 seconds |
| `quantities_and_closed_inventory` | 8 | 529.00 seconds |
| `state_and_history_preservation` | 3 | 372.66 seconds |

The retained solver times total 2,437.52 seconds. This sum is not elapsed wall time because some proofs run in parallel.
The interrupted session left its original suite running, and recovery started overlapping work before that process was identified.
Three repeated harnesses also passed. The remaining duplicate process was stopped, and completed results were retained.
The timing logs retain those repeated and interrupted costs separately.
Configuration, edition-compatibility, and unreachable-construct warnings remain, as in the earlier experiment.
No business assertion fails in the correct model.

The measured source hashes are:

- `model.rs`: `921628fa99fad81d55446c81e6c36fb99fa5db6e3bd39a8d73c46658411239d2`
- `proofs.rs`: `8d0aa8a992c18ead5516f943fe87b3be5f990c40b7bd6eb1af8c8d23159c62a9`
- `tests.rs`: `529602b60fabc4d6ad993482e50043ae31dd3008576f39e416446db5cb86960f`

The production guest build, guest Clippy, publication tests, offline SQL compilation, test Clippy, and browser TypeScript compilation pass.
Generation and SQL metadata preparation use fresh disposable PostgreSQL databases.
The local relocation case exercises the real component and PostgreSQL through the local runtime. It does not establish cluster behavior.
Raw logs and measured command durations remain in `/tmp/wamn-packaging-formal/` and `/tmp/wamn-phase1/` on this host.
Bead `wamn-s43x.15` retains the command timings and closure reference.

The [relocation defect](relocation-missing-transaction.patch) removes the transaction for the second relocated identity.
Its fixed example holds three available units and two held units in one packaging identity.
Both inventory locations change explicitly, and the total remains five units.
Kani fails only at `relocated inventory lacks its transaction` and prints a concrete playback test.
The defective command exits with status one in 86.31 seconds, including 84.14 seconds of solver time.
After restoration, the same proof passes in 45.61 seconds, including 43.40 seconds of solver time.
The restored temporary source matches the measured hashes above.

## Earlier explicit-location evidence

The initial production review used revision `1931d925f15e3e33ef2fc4899a8a46e96f0b4125`.
The scenario, schema, implementations, SQL, and tests at that revision define the former production baseline.
The owner's latest work order and answers define the new Inventory/Packaging target.
The explicit-location model revision changed no production code.
Task `wamn-s43x.10` subsequently replaces production with the target model.
That production alignment did not change the formal source measured in this section.

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
The [released-route cluster gate](../tests/cluster/application.rs) also calls these inventory assertions through the deployed HTTP endpoint.
Its release includes WMS without label components or wiring. A real HTTPS issuer supplies the session configuration required by the published routes.
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

The six earlier formal native examples covered the initial command set.
They include a packaging-only location edit that leaves inventory location unchanged and violates co-location.
Phase 1 now adds explicit packaging relocation.

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

The earlier production alignment changed no formal Rust source, so it did not repeat the formal runs above.
Phase 1 changes that source and requires a new measured run.
The deployed cluster cases did not run during this production correction.
The subsequent `cluster::released_wms_routes` run at `df64aed84` failed during host startup, before route assertions.
Its fixture omitted the session issuer required by the published routes. Bead `wamn-s43x.14` records the fixture correction.
The command took 1534.52 seconds, including builds, and removed its disposable resources.
After the fixture correction at `d8cb113b6`, the exact direct-route gate passed in 479.31 seconds, or 516.61 seconds including test compilation.
The deployed assertions covered inventory mutations, original-result replay, forced history-insertion rollback, and business refusals.
Cleanup removed the owned cluster, containers, and private files. This result supplies direct-route deployment evidence, not label-composition evidence.
Bead `wamn-g4kj` retains the existing unattached label-workflow finding.
Raw production logs remain in `/tmp/wamn-production-alignment/` on this host.
The successful cluster run is retained in `/tmp/wamn-wms-direct-gate/`.


## Practicality and limits

The model keeps two inventory identities, two claims, two operations, and a six-unit total.
Two packaging identities make packaging references and lifecycle explicit without modeling infrastructure.
It remains smaller than the four production command files alone, excluding SQL and generated code.
Before Phase 1, the business model contained 473 lines, with 496 lines of proofs and 312 lines of native examples.
Phase 1 increases the model to 525 lines, with 675 lines of proofs and 488 lines of native examples.
The retained solver total rises from 1,185.49 to 2,437.52 seconds. Host load and parallel execution prevent a controlled performance comparison.
The added operation remains small, but a full proof run carries a material development cost.
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

## Phase 2 and 3: production relocation kernel

Bead `wamn-s43x.16` extracts relocation decisions into `data/src/packaging_relocate/decision.rs`.
Production calls this pure module after loading locked business state.
The module returns typed refusals or explicit packaging updates, inventory updates, and paired inventory snapshots for history.
It accepts borrowed strings for identities, quantities, dispositions, and lifecycle values.
Relocation preserves quantities without parsing them, including decimal scale.

The adapter retains locks, membership retries, revisions, claims, timestamps, SQL, result serialization, and commit ownership.
It writes the returned `from_*` and `to_*` values directly into history.
It stores the complete result in the same PostgreSQL transaction.
The existing application tests remain unchanged.
Co-location remains a rule of admitted locked commands, not a universal database constraint.

The harness in `production-relocation/` imports the actual production module by path.
Separate assertions count each affected identity and inspect every update and history attribute.
They require exact quantity preservation, explicit location changes, complete lineage, and no updates for closed or unrelated inventory.
They also assert empty relocation, refusal precedence, and unchanged input state.
Bounds remain two inventory identities with representative string values.
This proof does not establish behavior for unbounded collections or all possible strings.

The extraction exposes representation costs.
Borrowed strings preserve production values but depend on the loading boundary for identity and quantity validity.
Allocated vectors and string comparisons increase the work Kani analyzes.
No replacement decision implementation or proof-only production branch is necessary.

Refusal order crosses the business and concurrency boundary.
The adapter calls the kernel's lifecycle guard before checking the expected revision and stable membership.
The final decision calls that same guard again, which preserves the existing refusal order without duplicating its rule.
The adapter loads destination existence before the final decision, which retains the business refusal priority.
This changes query timing, but leaves business outcomes unchanged when database reads succeed.

These proofs cover the production decision function, not SQL persistence, replay storage, or concurrency.
The independent Phase 1 model retains its history and replay obligations.
The unchanged local application test supplies rollback, stored replay, and concurrent membership evidence for the adapter.
Phase 4 conformance generation and composition remain outside this increment.

The unchanged `local_business::packaging_relocation` case passes in 72.45 seconds, or 73.63 seconds including command startup.
It runs the rebuilt WMS component against disposable PostgreSQL.
The final guest build passes in 24.06 seconds, guest Clippy in 10.62 seconds, and offline SQL compilation in 5.11 seconds.
Generation takes 7.32 seconds, and SQL metadata preparation takes 6.16 seconds.
Logs and exact timestamps remain in `/tmp/wamn-phase23/` and the bead.

The deliberate defect retains both location updates but emits only the first inventory history row.
Kani rejects it at `each affected identity has exactly one history row`.
The concrete case moves held inventory with quantity `4.50` and available inventory with quantity `2`.
The second identity lacks history, while both quantities and locations remain correct.
With the final observer, the defective copy fails one of 670 obligations in 16.18 seconds.
The restored copy passes all 670 obligations in 16.48 seconds.
Both native examples pass with Rust 2024 and warnings denied in 0.26 seconds, including compilation.
These logs remain in `/tmp/wamn-kernel-proof-membership-mutant/` and `/tmp/wamn-kernel-proof-membership-remaining/`.

The initial Kani run uses its default CaDiCaL solver.
The concrete history proof passes in 49.87 verification seconds, and the refusal proof passes in 19.50 verification seconds with five covers.
The symbolic transition proof remains unresolved after more than 30 minutes of active CBMC work.
We stop that process and record the full invocation as interrupted after 2,081.24 seconds, not as a proof failure or success.
The retry selects Kissat for only the unresolved harness, with unchanged decision code, assertions, and bounds.
The retry does not launch its external SAT solver during observation.
A debugger stack sample locates CBMC inside symbolic execution, simplifying pointer relationships through `value_sett::get_value_set` and `goto_symext::do_simplify`.
This evidence points to formula preparation, not a demonstrated SAT solver failure.
Borrowed strings, vectors, and the harness's symbolic collection length are possible contributors, not separately established causes.
The Kissat-configured retry stops after 386.47 seconds without a result.

We partition the symbolic inventory count into fixed counts zero, one, and two.
Their union retains the original count domain, and every inventory attribute choice remains unchanged.
Detailed output still shows observer loops expanding beyond two entries, alongside more than 2,500 string-comparison expansion messages.
This identifies the global loop bound as a source of unnecessary formula construction.
The count-only experiment stops after 148.41 seconds without a result.
The next configuration gives the new success harnesses a bound of three and preserves `memcmp.0:12` for strings of up to eleven bytes.
All unwinding assertions remain enabled, so an insufficient bound fails rather than silently excluding behavior.
With these bounds, two-row symbolic execution completes in 23.37 seconds and intermediate-form conversion takes 70.65 seconds.
The run then consumes about 26 GB during propositional reduction, so we stop it to protect the shared host.
The loop change resolves the observed symbolic-execution bottleneck but does not establish an affordable complete proof.

The final observer requires unchanged borrowed strings to retain their data pointer and length.
Equal immutable references imply equal contents, so this strengthens value preservation without changing production decisions.
Per-identity lookups use that stronger identity requirement too. Counts still reject missing, extra, or incorrect rows.
A future implementation that rebuilds equal text can fail this stronger proof despite preserving business values.
The observer also receives the exact destination reference from the command, without depending on repeated literal allocation.

Reference assertions alone still leave large formulas when vector membership remains symbolic.
We therefore partition packaging assignment and lifecycle as well as inventory count.
One empty case, four one-row cases, and sixteen two-row cases cover the original membership domain exactly.
Every case retains arbitrary product, quantity, disposition, and location choices from the original bounds.
The two-assigned-open case passes all 800 checks and its cover in 137.05 seconds, including setup.
Production source remains unchanged throughout these proof experiments.

All twenty-three final harnesses pass, and all twenty-six cover requirements are reached.
The retained verification total is 1,279.72 seconds, excluding compilation and the interrupted diagnostic runs.
The first membership case takes 137.05 wall-clock seconds, and the remaining twenty-one selected harnesses take 1,176.41 seconds.
The unchanged refusal proof retains its earlier passing result. Its function and input generator remain unchanged.
Final run details remain in `/tmp/wamn-kernel-proof-handoff.json` and bead `wamn-s43x.16`.

This pilot proves the production decision code without a substitute model or production refactor for the verifier.
It also shows substantial proof-authoring and execution costs for strings and dynamically populated vectors.
Complete case partitioning and stronger reference assertions make the bounded proof finish, but this is not yet a cheap general method.

## Phase 4: application conformance

The differential harness imports the independent model directly and sends bounded command histories through the real WMS application over PostgreSQL.
It compares accepted or refused outcomes, inventory, packaging, transaction rows, stored results, and exact replay after each command.
The adapter supplies production revisions and maps identities without duplicating command decisions.
The harness imports existing model types and functions through crate visibility.

Proptest found a disagreement and reduced it to one command with no inventory present.
The original model accepted a fresh close command on already closed packaging, but production refused it.
The minimized case reproduced against a fresh database fixture.
The owner ruling in `wamn-s43x.18` classifies this as a model defect.
Fresh closure requires an open lifecycle and refuses before the empty-packaging test when the packaging is already closed.
Refusal changes no state and stores no new result.

The model now applies that rule, and the harness retains the regression seeds.
Exact replay of a successful closure remains distinct from a fresh closure request.

This result demonstrates value beyond the individual proofs and application tests: executable comparison exposes a difference that both suites previously accepted.
The harness covers the modeled business domain only.
Existing PostgreSQL tests retain responsibility for locking, concurrency, and failure atomicity.
Phase 4 validation and measured durations remain in `wamn-s43x.17`.

The original run passed nine fixed histories and failed two on the closure rule.
The generated regression reproduced that rule difference after shrinking.
After the correction, all eleven fixed histories, sixteen generated histories, and both saved regression seeds pass.
The ten native model examples and generator capacity test also pass.
No further semantic mismatch appears in that bounded run.
This covers all six modeled commands, with full state and history comparison and replay of every earlier accepted request after each command.

Temporarily restoring the old closure defect makes the differential test fail again.
Proptest reduces it to one fresh close command against empty, already closed packaging.
The test reproduces the minimized failure, and the corrected model is restored byte-for-byte afterward.

Four targeted Kani harnesses pass after the correction, with all six cover properties reached.
They cover closure precedence, state and history preservation, exact replay and changed intent, and later closure preserving earlier history and replay.
The precedence harness includes both empty contents and inconsistent open inventory to distinguish the order of refusals.
Clippy passes, and production code remains unchanged.


## Second production kernel: split

Bead `wamn-s43x.19` owns the split extraction, production proofs, and application conformance evidence.
The production adapter loads locked inventory and packaging, then calls `inventory_split/decision.rs`.
The pure decision returns either a typed refusal or two inventory updates with two complete history rows.
The adapter supplies the allocated identity and persists that transition inside the existing transaction.
Claims, revisions, locks, SQL, replay results, and commit remain outside the kernel.

Split differs from relocation because it creates an identity and subtracts decimal quantities.
The kernel preserves PostgreSQL decimal scale without a machine integer bound.
Native examples compare 62,001 quantity pairs with independent integer arithmetic.
Application tests compare ten decimal cases directly with PostgreSQL arithmetic.
That corpus includes mixed scales, decimal borrowing, values beyond `i128`, equal and excess quantities, `NaN`, and `Infinity`.
The stored special values retain existing behavior but remain outside the finite conservation proof.
No new numeric restriction or shared decimal framework enters production.

The [production split harness](production-split/README.md) imports the actual decision module.
An independent observer converts finite quantities to hundredths and checks their sum.
It identifies the source and created inventory by identity, independent of their array positions.
It requires preserved product, disposition, lifecycle, packaging, explicit location, and complete history for both identities.
Both history rows retain the source lineage.
Separate refusal assertions cover competing guards and unchanged inputs.
The adapter remains responsible for matched packaging identities and a fresh allocated identity.

The proof domain contains three source quantities and five requested quantities.
Each pair admits both packaging choices, two products, and two dispositions, for 120 combinations.
The proofs do not establish arbitrary decimal precision.
As in relocation, borrowed text uses stronger reference assertions to control proof cost.
Split uses fixed two-element arrays but owned decimal strings, unlike relocation's unchanged quantity references.
The first variable-request proof exceeded 12 GB without a result and was stopped after 555.727 seconds.
Its refusal harness passed before the stop.
A stack sample reached CaDiCaL clause insertion but did not establish the full cause of the cost.
Fifteen operand-pair proofs retain the same domain and assertions.
Each proof fixes its decimal string lengths.
The first partition passed in 86.443 seconds, including 71.353 seconds for verification.
All seventeen final harnesses pass, and all twenty-one coverage assertions are reached.
The retained verification time totals 474.905 seconds, excluding compilation, interrupted work, and the deliberate-defect runs.
The remaining fourteen operand-pair harnesses pass in 414.433 wall-clock seconds.
The unchanged refusal and concrete-lineage harnesses retain their earlier passing results.

The deliberate defect changes only the created history row's source identity.
For a split of `0.50` from `2.50`, the quantities remain `2.00` and `0.50`.
The created row incorrectly names the new identity as its source.
Kani rejects its lineage assertion in 42.954 seconds total.
Reversing the patch restores the production module byte-for-byte, and the same proof passes in 66.140 seconds.

The rebuilt application passes the complete six-command differential histories against the unchanged independent model.
All eleven fixed histories, sixteen generated histories, and two retained regression seeds pass.
After every command, the harness compares state, immutable history, stored results, and exact replay.
The application tests retain concurrency and forced history-write rollback coverage.
A new decimal split case also changes both resulting identities and then replays the original stored result.
The two local PostgreSQL/application tests pass in 263.470 seconds.
The native test crate passes 26 tests, with eight external tests excluded from that run.
Both the production guest and application test crate pass Clippy with warnings denied.
The split and relocation proofs cover different input domains, so their durations do not measure a like-for-like speed difference.

The owner accepts Phase 5 as the boundary already evidenced by these PostgreSQL tests.
The Kani decision proofs do not claim persistence atomicity, claim concurrency, or runtime correctness.
