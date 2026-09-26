# Production relocation proofs

The harness imports the production decision module with `#[path]`. It does not copy or replace the decision code.
Production and Kani compile `apps/wamn_wms/data/src/packaging_relocate/decision.rs`.

The transition assertions independently count updates and history rows for each loaded identity.
They require exactly one of each for every open identity assigned to the packaging.
Closed identities and identities assigned elsewhere require neither.
The assertions inspect every inventory attribute, including the explicit location and unchanged quantity text.
They also inspect both inventory snapshots in every history row and both packaging snapshots.
Field-preservation assertions require the same immutable string reference, including its pointer and length.
The observer also identifies output rows by the original identity reference and requires exactly one row for each affected identity.
That stronger property implies identical bytes, including decimal scale. Business membership and refusal comparisons still compare string contents.
An implementation that rebuilds equal text in different storage can fail these stronger assertions without violating the business rule.
The production decision code does not rebuild these borrowed values.

The refusal proof covers closed packaging, a fresh no-op, misplaced open inventory, and a missing destination.
It asserts their existing precedence and input preservation with two loaded rows.
The success proofs include empty packaging and two affected identities.
There is one empty case, four one-row cases, and sixteen two-row cases.
Each row has four packaging/lifecycle combinations: assigned/open, unrelated/open, assigned/closed, and unrelated/closed.
The harness names abbreviate those combinations as `AO`, `UO`, `AC`, and `UC`.
The two-row cases include every ordered pair of those combinations exactly once.
Their union equals the original `length <= 2` domain. Product, quantity, disposition, and location remain arbitrary within the same bounds.
All cases use the same independent transition assertions.
Two native examples exercise decimal scale, closed inventory, empty packaging, and fresh no-op refusal.

The bounds are zero to two inventory identities, two locations, two product identities, and two packaging identities.
The success proofs relocate `package-a` from `location-a` to `location-b`.
Loaded inventory attributes vary within the stated bounds. The proofs do not quantify over every relocation direction or arbitrary identity strings.
Each inventory identity is unique, as required by the database primary key.
The caller must supply the complete, stable membership set.
The harness includes unrelated and closed rows to exercise their exclusion.
Disposition and lifecycle use the admitted literals. Quantity uses representative opaque strings, including `4.50` and `0`.
The proof also permits zero quantity on open input rows to test preservation without assuming quantity validation.
That input does not claim that the production schema admits open inventory with zero quantity.
The decision function does no quantity arithmetic. Equality assertions require exact preservation, including decimal scale.
These finite string choices do not prove arbitrary string implementations or unbounded inventory counts.

SQL, row locks, retries, revisions, claims, replay, result serialization, and rollback remain outside this kernel.
The existing application tests cover those boundaries. These proofs do not establish atomic persistence or replay preservation.
The independent business model remains separate and retains its broader history obligations.

Run the proofs from the repository root. Set `KANI_HOME` to the installed Kani toolchain directory.

```bash
proof_run=$(mktemp -d /tmp/wamn-kernel-proof-XXXXXX)
kani apps/wamn_wms/formal/production-relocation/harness.rs \
  --target-dir "$proof_run/target" --output-format terse \
  -Z unstable-options --cbmc-args --unwindset memcmp.0:12
rustc --edition=2024 --test apps/wamn_wms/formal/production-relocation/harness.rs \
  -o "$proof_run/native-tests"
"$proof_run/native-tests"
```

For the deliberate defect, copy the harness and production module into a disposable directory with the same relative paths.
Apply `missing-history.patch` to that copy. Run `two_inventory_history_is_complete` with `--harness` and `--exact`.
The mutation retains both location updates but omits the second history row.
The independent row-count assertion must fail. Reverse the patch and run the same proof again.

Borrowed strings avoid UUID and decimal dependencies in Kani without changing the production decision code.
They do not encode validated identity or quantity types. The loading boundary supplies that validation.
The transition allocates vectors, so Kani must analyze the production allocation and string operations too.
No shared framework, production-only substitute, or proof-only decision branch is present.

The success harnesses use an unwind bound of three for collections with at most two rows.
The `memcmp.0:12` override retains comparisons for the admitted strings, whose longest value has eleven bytes.
All unwinding assertions remain enabled. They must establish that these bounds cover every admitted loop execution.
The refusal and concrete history harnesses retain their original unwind bound of twelve.

The initial combined proof expanded symbolic pointer expressions for much longer than the concrete proof.
A retry selected Kissat, but inspection found CBMC inside pointer-value simplification before the external solver started.
Changing the SAT solver did not address that stage.
The fixed-length and membership partitions preserve the input domain. Separate collection and string bounds avoid twelve iterations for two-row collection loops.
Repeated byte comparisons in the observer also expanded the formula. A later run reached about 26 GB of resident memory and stopped.
The observer now asserts exact reference preservation for borrowed fields. This reduces formula construction without weakening the required value-preservation property.

Kani 0.68.0 with CBMC 6.11.0 passed all twenty-three final harnesses and all twenty-six cover goals.
The two native examples also passed with Rust edition 2024 and warnings denied.
The deliberate defect moved both inventory identities but omitted the second history row.
Kani rejected that transition at the independent row-count assertion. The restored production source passed the same proof.
The concrete counterexample keeps quantities `4.50` and `2` unchanged but leaves `inventory-b` without history.
