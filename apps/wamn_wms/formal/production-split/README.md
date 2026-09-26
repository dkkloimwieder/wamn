# Production split proofs

The harness imports the production split decision module with `#[path]`.
Production and Kani compile `apps/wamn_wms/data/src/inventory_split/decision.rs`.
The harness does not replace its decimal arithmetic or transition logic.

The observer converts finite decimal quantities to integer hundredths.
It sums the resulting inventory quantities and compares that sum with the source quantity.
It does not call the production subtraction function or reproduce its digit subtraction algorithm.
The created quantity must equal the requested quantity, and both resulting quantities must remain positive.

The assertions identify inventory updates by identity, independently of their array position.
Exactly one update must retain the source identity, and exactly one must use the new identity.
Each update must have exactly one complete history row.
Both rows must name the original source in `from_inventory_id`.
Each row must name its affected identity in `inventory_id` and `to_inventory_id`.
The source row must retain the complete original snapshot.
The new row must have no prior snapshot, which represents zero prior quantity and absent prior attributes.

Product, disposition, and lifecycle must remain unchanged.
The source must retain its packaging and explicit location.
The new inventory must use the requested packaging and explicit location.
History snapshots must match the corresponding inventory updates exactly.
Borrowed fields use exact reference assertions, including pointer and length.
These assertions imply equal text but also reject equivalent text rebuilt in separate storage.
Owned quantities use value comparisons and the independent numeric observer.

Fifteen quantity harnesses partition every pair of three source quantities and five requested quantities.
The source domain is `0.50`, `1.00`, and `2.50`.
The request domain is `0.25`, `0.50`, `1`, `2.5`, and `3.00`.
Each harness fixes both operands and chooses its packaging, product, and disposition independently.
The destination is either the source packaging at its current location or different packaging at another location.
Product and disposition independently select two admitted values.
The operand pairs include six accepted cases and nine refused cases.
Each harness covers its expected outcome, without an unreachable cover for the opposite outcome.
The partitions preserve the complete stated finite domain.
They do not prove arbitrary decimal precision or every possible positive quantity.

A separate refusal harness covers six refusal types and their competing conditions.
The source lifecycle, source existence, source packaging lifecycle, source co-location, destination existence, destination lifecycle, and destination co-location vary independently.
The harness includes competing refusal conditions and asserts their production precedence.
It excludes the all-valid guard combination, which the quantity harnesses cover.
Equal and excess quantity refusals remain in the quantity harnesses.
Every harness asserts that the loaded inventory remains unchanged.
The refusal harness also asserts unchanged packaging inputs.

Two native examples cover fractional conservation, lineage, same-packaging split, equal quantity, and excess quantity.
The imported production module also supplies its native decimal arithmetic examples.
Those examples include greater precision and quantities beyond machine integer bounds.
PostgreSQL special values `NaN` and `Infinity` remain outside the finite Kani quantity domain.
They retain existing production compatibility and native examples.

The loading boundary supplies matched packaging identities and a distinct allocated inventory identity.
It also supplies admitted lifecycle literals and canonical positive request quantities.
The proofs do not establish those caller obligations.
SQL, row locks, revisions, claims, replay, persistence, result serialization, and rollback remain outside the decision module.
The application tests cover those boundaries.
These proofs do not establish transaction atomicity or replay preservation.

Run these commands from the repository root with the installed Kani toolchain.

```bash
proof_run=$(mktemp -d /tmp/wamn-split-proof-XXXXXX)
kani apps/wamn_wms/formal/production-split/harness.rs \
  --target-dir "$proof_run/target" --output-format terse \
  -Z unstable-options --cbmc-args --unwindset memcmp.0:12
rustc --edition=2024 --test apps/wamn_wms/formal/production-split/harness.rs \
  -o "$proof_run/native-tests"
"$proof_run/native-tests"
```

The proof loop bound is six.
The finite quantities require at most four bytes, and each transition contains two inventory updates and two history rows.
The `memcmp.0:12` override covers the longer admitted identity strings.
All unwinding assertions remain enabled.
The run must establish that these bounds cover every admitted loop execution.

For the deliberate defect, copy the harness and production module into a disposable directory with the same relative paths.
Apply `missing-lineage.patch` to that copy.
Run `split_history_retains_source_lineage` with `--harness` and `--exact`.
The mutation changes the created history row to name itself as its source.
Inventory quantities and locations remain correct, but the immutable history loses its source lineage.
The independent lineage assertion must fail.
Reverse the patch and run the same proof again.

Relocation preserves borrowed quantity text without arithmetic.
Split allocates owned decimal results and performs the actual production decimal subtraction.
Fixed arrays bound the two required identities without a general transition framework.
Operand-pair partitions fix decimal string lengths while preserving the stated input domain.
The [assessment](../assessment.md#second-production-kernel-split) records the completed proof results and measured costs.

The initial six harnesses fixed the source quantity but chose the requested text symbolically.
The first variable-request proof grew beyond 12 GB and produced no result before its driver stopped after 555.727 seconds.
A stack sample reached `CaDiCaL::Internal::add_new_original_clause`.
That evidence differs from the relocation run that stalled during pointer expression construction.
The sample alone does not identify the full cause of the split proof cost.
The fifteen partitions remove variable requested lengths from each decimal formula without changing production logic or proof assertions.
The concrete lineage proof passed in 52.742 seconds total, including 39.617 seconds for verification.
The refusal proof passed with 37.592 seconds for verification.
Five native tests passed.

All seventeen final harnesses pass, including all twenty-one coverage assertions.
The deliberate lineage defect fails its expected assertion, and the restored module passes the same proof.
