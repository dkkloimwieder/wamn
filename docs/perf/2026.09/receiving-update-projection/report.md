# Explicit columns in generated UPDATE results

Issue: `wamn-10yt.81`.

The generator returns each declared model column explicitly from its UPDATE statement.
It no longer requests columns that a later schema adds outside the generated grant list.
The outer result projection, operation input, and operation output remain unchanged.
The owner approved this repair before production edits.

## Measured defect

The original [PostgreSQL 18.6 diagnosis](diagnosis-001/diagnosis.json) uses production package installation and permission reconciliation.
Both shipped UPDATE statements pass the baseline schema and fail with SQLSTATE `42501` after an ungranted column is added.
Replacing only the wildcard with declared columns makes both statements succeed under the same role and grants.
The diagnosis retains its exact source, queries, grants, and cleanup results.
It covers a candidate that changes initial DDL while retaining its generated corpus and grants.
It does not establish failure for every regenerated additive base.

## Generation and contracts

The [normal materializer](materialize-001/result.json) runs each package against its own disposable PostgreSQL database.
All three packages pass write and repeated drift checks.
Only eight generated Receiving and Acme files change, and WMS remains byte-identical.
The [generation comparison](generation-scope.json) records those paths.
Both UPDATE operation contracts change only their statement digest.

[SQLx preparation and drift checking](sqlx-001/result.json) pass against fresh PostgreSQL 18.
Exactly two query records replace their old versions, and no unrelated metadata needs restoration.
The [metadata comparison](sqlx-001/metadata-comparison.json) shows identical parameter and result descriptions.
Both disposable database cleanups pass.

## Executed tests

The [generator gate](generator-tests-001/result.json) passes 154 tests across all targets, with ignored tests included.
The [database regression](regression-positive-001/regression-result.json) executes its one named test and passes.
It derives production grants and proves that both regenerated queries work while the additional column remains ungranted.
It also preserves stale and missing-row outcomes and leaves the additional stored value unchanged.
Its internal wildcard control returns SQLSTATE `42501`.
The fixture rolls back its schema, role, grants, and rows, and [cluster cleanup](regression-positive-001/cleanup.json) passes.

Both separate old-query controls fail at the intended permission error with test exit code 101.
The [base control](wildcard-base-001/control-result.json) uses the original base query from `b8c52797`.
The [overlay control](wildcard-overlay-001/control-result.json) uses the original overlay query from the same commit.
Each control changes one generated SQL file and restores its exact bytes, mode, and timestamps afterward.
Both disposable database cleanups pass.
The [scoped Cargo cleanup](restore-clean-001/result.json) removes the integration package outputs before the restored positive test.

The [restored positive test](regression-restored-001/regression-result.json) passes after recompilation, and its disposable database cleanup passes.
The [normal m1 build](m1-001/result.json) compiles and virtualizes the application components successfully.
The [pin refresh](remint-001/result.json) changes one authored Receiving digest and exactly four derived Acme files.
The new Receiving digest is `a092149c1c8df8b4f74babf64122f9747d15d7b1a78b82b35cb6e496f98fd9a1`.
Fresh package checks pass before the pin change, and normal Acme regeneration and its final check pass afterward.
The disposable database cleanup passes.

The [native caller gate](native-callers-001/result.json) reports 52 passing tests and no failures.
Two existing `generated_update_exclusion_from_postgres` cases self-skip because `WAMN_EXCLUSION_DIAGNOSTICS` is absent.
Those two cases supply no executed database proof in this run.
The new additive-column regression uses its own armed database fixture and executes separately.

The [offline SQLx verifier](offline-sqlx-001/result.json) passes both compile-verifier tests.
[Scoped Clippy](clippy-001/result.json) passes for the generator and integration package across all targets.
Existing warnings remain, with no diagnostics on the changed Rust lines.

These are local generator, compiler, component, and disposable database results.
The full unchanged-overlay proof remains owned by `wamn-10yt.78`.
This repair does not claim that the paired cluster proof or its guest controls pass.

## Source and reproduction

The base is `b8c527975028931386cf11849a8e6144e9f14848`.
The [source patch](preparation-001/source.patch) and [file hashes](preparation-001/source-files.json) record all 21 repair paths.
The retained generation tools record the one-time repair from that base.
The [component identities](component-identities.json) record the two changed applications and the unchanged HTTP shell and materializer.

Run the generator and offline verifier from the repaired checkout.

```bash
cargo test -p wamn-schema-generator --all-targets --no-fail-fast --locked --offline -- --include-ignored
SQLX_OFFLINE=true cargo test -p wamn-proof-conformance --test receiving_sqlx_verifier --locked --offline -- --include-ignored
```

Run the new database test with a fresh PostgreSQL 18 URL through the [Receiving recipe](../../../operations/build-and-test.md).
The retained [regression helper](tools/regression_pg18.py) creates its own disposable PostgreSQL instance and records cleanup.
Use a new evidence directory for each execution.
