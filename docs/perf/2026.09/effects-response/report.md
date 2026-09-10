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

The source boundary is `c86488041b09b02d3f82676c60cdb3ed1d1fca04`.
Identity and Receiving incorporate this candidate into their isolated proof branches.

The [cross-profile comparison](cross-profile-001/details/comparison.json) reproduces `wamn-10yt.61` at that source.
Both component builds pass, but the existing named assertion fails with status 101.
The same four packages differ between `m1` and `proof`: blob-put, client-acme-receiving, receiving, and wms.
This run does not investigate or repair the dependency cause.

The [first deployed WMS run](live-001/result.json) exits 1.
The deployed contention, replay, and remaining operation assertions pass, along with the single winning label object assertion.
Their results are retained in [wms-runtime.receipt](live-001/journey/wms-runtime.receipt).
The new partial arm stops before its test because the journey document already contains its runtime phase.
The helper refuses a second amendment, as its contract requires.

The runner now creates a separate journey document for the return move before it removes the labels bucket.
The [offline setup proof](partial-document-001/result.json) executes that setup against the existing journey example.
It proves that the original document stays unchanged and the new document selects the return location.
This result does not prove the deployed partial response.

The failed run's [bucket restoration log](live-001/journey/partial-bucket-restored.log) records successful restoration.
Its [cleanup receipt](live-001/journey/cleanup.receipt) still reports failure.
A [later read](live-001/cleanup-followup.json) finds none of its owned containers, host image, cluster, or scratch directory.
The old cleanup output does not identify the failed step.
The runner now retains named failed cleanup steps, exit statuses, and command errors without changing cleanup decisions.

The [second deployed run](live-002/result.json) passes all 13 journey arms at source `6f198834`.
The [HTTP evidence](live-002/journey/wms-partial-http.json) records HTTP 500 with exactly the committed movement and the later store failure.
The failed outcome carries `write_failed`, `responded`, and `Error::NoSuchObject`.
The named test sends the command once and proves the committed movement through a later read.
The [database evidence](live-002/journey/wms-partial-database.json) proves one command, one movement, one quantity row, and the matching pallet state.
The [partial receipt](live-002/journey/wms-partial.receipt) also records successful bucket restoration.

The second run still exits 1 because cleanup fails.
Its [diagnostics](live-002/journey/cleanup-cluster-delete.stderr) show that kind deletes the nodes but cannot lock `/dev/null.lock`.
The cleanup command inherits `KUBECONFIG=/dev/null` from the capture tool.
The runner now passes its own Kubernetes configuration file to both cluster creation and deletion.

The [third deployed run](live-003/result.json) passes in 324.406 seconds with exit status 0 at source `f14be496`.
Its [verdict](live-003/journey/verdict.json) records all 13 passing journey arms.
The [partial receipt](live-003/journey/wms-partial.receipt) records one command, one movement, the observed failure, and successful bucket restoration.
The [cleanup receipt](live-003/journey/cleanup.receipt) passes with the owned Kubernetes configuration file.
The [source receipt](live-003/source-stability.json) records a clean worktree at the same commit before and after the run.
Shared finding `wamn-2npt` remains open for the corresponding Receiving cleanup repair.

The deployed response proof is complete.
The final combined integration sweep remains unproved while the identity and Receiving proof branches finish their current runs.
The effects issue remains in progress until that integration boundary.
