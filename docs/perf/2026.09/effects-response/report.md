# Declared partial responses

This report records the response contract work for `wamn-b2m6.10`.
The implementation remains in the isolated effects branch until its live proof and integration gates finish.

A registered operation declares which returned values prove commitment.
The wiring selects that operation and declares its normal HTTP response separately from its edge schemas.
After a later failure, the response carries only the selected committed result and the existing failure.
A single observed failed capability adds one of the six existing effect outcomes.
Missing or ambiguous evidence adds no effect outcome.
The client enforces that declared response and keeps composed retries disabled.

Each command directory contains its arguments, source hashes, output, and exit status.
The source records name base commit `41ba0334` and the edited source hashes because these focused runs precede the source commit.

The generator build passes in [generator-build-001](generator-build-001/result.json).
The [materialization run](materialize-001/result.json) regenerates Receiving, its Acme overlay, and WMS against fresh PostgreSQL 18 databases.
Each package passes write, check, and a second check.
The owned database container cleanup passes.

The [first client suite](client-tests-001/result.json) reports 232 passing tests and two failures.
One failure exposes numeric WMS revisions that the result validator accepts only as text.
The other compares two equivalent enum lists in different orders.
The repair keeps the served schema as the wire authority and accepts both valid signed integer carriers in descriptor validation.
The projection test compares enum members without order.

The [isolated client run](request-tests-001/result.json) also exposes a missing `chrono/std` dependency for its timestamp error source.
The manifest now declares that feature directly.
The repaired [request tests](request-tests-002/result.json) pass all 11 cases.
The repaired [submission tests](submission-tests-002/result.json) pass all 22 cases.
The repaired [projection tests](projection-tests-002/result.json) pass all 13 cases.

The first catalog run fails to compile because three new assertions omit the JSON macro qualification.
After that correction, the [catalog gate](catalog-tests-002/result.json) passes all 78 tests.

The [first runtime and host run](runtime-host-tests-001/command.log) fails to compile because Boon's `CompileError` does not implement `Send` or `Sync`.
The repair converts that error into contextual `anyhow` text.
The [rerun](runtime-host-tests-002/command.log) passes 42 host tests and 317 runtime tests, with exit status 0.
The nested subprocess test is already included in the 317 runtime tests.
The [command](runtime-host-tests-002/command.json) explicitly filters four unrelated infrastructure tests for tap-stream permissions, timestamp spelling, typed text carriers, and pool isolation.
This run does not prove those four cases.

The [native component run](component-native-tests-001/command.log) passes all 44 tests, with exit status 0.
That total includes 19 HTTP route tests, 11 materializer tests, and 14 execution contract tests.
These native tests do not prove deployed Wasm behavior.

Each mutation introduces one deliberate fault in production code.
In [mutations-001](mutations-001/result.json), both unchanged tests pass, both faults fail their named assertions, and both restored tests pass.
Each failing run exits with status 101 because of an assertion failure, without a compile failure.
The harness restores source bytes and permissions with a fresh modification time before each restored run.
The evidence records the original timestamps and the restored modification times.

The [unselected-result mutation](mutations-001/unselected-success-overwrites-committed-result/mutated/command.log) fails `selected_committed_result_survives_arbitrary_success_and_label_enrichment` after an unselected success overwrites the selected result.
The [multiple-failure mutation](mutations-001/multiple-failures-keep-latest-outcome/mutated/command.log) fails `one_failure_survives_successes_but_never_another_failure` after a second failed attempt replaces ambiguity with the latest outcome.
These two named regressions do not prove live HTTP, guest execution, database behavior, or the full test suite.

The [Wasm build](component-wasm-build-001/result.json) passes with the established `m1` component profile.
Its [artifact record](component-wasm-build-001/artifacts.json) retains the raw and virtualized digests.
The virtualized Receiving digest changes to `sha256:5326f1a794dfe01736dbf08a41a73593a908f4551c76e116c2e9a79f3b6795b3`.
The overlay now pins that measured artifact.
The [second materialization](materialize-002/result.json) passes for all three packages and updates four generated overlay metadata files.
Both repeated checks and the owned PostgreSQL cleanup pass.

The [format run](format-001/result.json) finds one new indentation error in the terminal fixture, which the patch corrects.
The remaining differences belong to unchanged code in component admission, blobstore release coordinates, and the PostgreSQL pool.
The module check reaches that pool through its existing module declaration.

The [CLI and integration compilation](ctl-integration-compile-001/result.json) passes for all selected test targets.
It compiles the new live WMS test but does not run it.

The WMS live proof and the full integration sweep did not run in this capture.
These local results do not establish deployed behavior or complete the effects issue.
