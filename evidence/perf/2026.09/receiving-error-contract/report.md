# Receiving error contract agreement

Issue: `wamn-10yt.67`. Base: `0cdb9af37e065af305c089e9035526384ae24844`.

The Receiving test now derives reachable constraint errors from all four generated update slices. It compares each slice with the exact constraint names in the generated update contract. It requires every reachable literal to appear in a contract that this crate implements. The test contains no `UNDECLARED` exception list.

The issue described a future undeclared error when a manifest permits a named constraint. Current generator code already prevents that mismatch. `validate_constraint_error_details` requires the exact constraint kinds. Contract emission and `mutation_constraint_names` select the same constraints through `operation_constraints` and `operation_exclusions`. These functions are in `crates/schema/generator/src/generate.rs` at this base.

Only tests and their explanatory comment change in `components/data/receiving-data/src/error.rs`. Runtime behavior, public error literals, generated artifacts, and guest digest pins remain unchanged.

The scoped library command passes all 38 tests in `library-tests-002`. Each constraint kind passes with a matching generated name and contract entry. Each kind then fails with exit code 101 when the contract entry is absent. All four failures name the expected slice and contract disagreement. The controls cover unique, foreign-key, check, and exclusion constraints.

`constraint-controls.json` records the controls and exact input restoration by SHA-256. `restored-001` passes the focused test after restoration. The generated input hashes match their original values.

`clippy-003` passes scoped Clippy for the library and its tests under the declared package lint policy. It reports 43 existing `result_large_err` warnings in unchanged generated accessors. No diagnostic refers to the changed `error.rs` file. `format-001` passes the format check for that file.

The stricter `clippy-001` attempt stops on existing dependency diagnostics. The package-only `clippy-002` attempt stops on the 43 generated-accessor warnings because it adds `-D warnings`. Neither failure changes the implementation scope. This evidence does not claim a warning-free Clippy result.

`library-tests-001` records an initial package-selection error because the command selected the root workspace. `library-tests-002` uses `components/Cargo.toml`. `controls-preparation-001` records an initial fixture anchor mismatch before any mutation. The corrected control tool uses the generated `pub(crate) const` spelling.

Every command capture records its source revision, changed source hashes, arguments, output, and exit code. The tools preserve the four deliberate test failures as evidence. This proof does not claim a workspace gate, a live database gate, or a guest rebuild.
