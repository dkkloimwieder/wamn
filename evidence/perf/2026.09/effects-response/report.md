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
Shared finding `wamn-2npt` closes after both runner fixes and their committed cleanup receipts pass.

The deployed response proof is complete.
The first combined candidate is `2452eb0c`.
It contains effects `dc58f2d9`, Receiving `0673f3f5`, and identity `e13fd2f3`, with both proof modes and all three test dependencies preserved.
The [manifest inspection](combined-integration-preparation-001/metadata.json) passes without a build.
The [merged runner check](combined-runner-check-001/result.json) accepts both existing successful logs and rejects the wrong test prefix.
Both proof modes produce their expected plans, and their combined invocation refuses.

The [combined workspace run](combined-workspace-001/run.json) tests clean candidate `fafdce0e` and exits 101 after 420.204 seconds.
It includes ignored tests and excludes only the two named schema regeneration helpers.
The [source receipt](combined-workspace-001/source-stability.json) records the same clean commit before and after the run.

The [classification](combined-workspace-001/workspace-results.json) records 2,151 test passes, six doctest passes, and 74 failures in 35 targets.
At least 85 reported passes skip their live or artifact proof.
The 2,072 reported passes after that subtraction are not an exact executed proof count.
The classifier reports no incomplete targets or unresolved parse cases.

The [final baseline comparison](combined-workspace-001/final-baseline-comparison.json) finds all 68 previous failures with unchanged classified causes.
Five added failures lack their private live fixture inputs: three identity tests, one WMS test, and one Receiving history test.
Those workspace failures do not execute the application behavior.
Their separate deployed and PostgreSQL proofs remain in the linked lane evidence.

The sixth added failure belongs to the TUI revision fixture.
The effects response contract accepts numeric or string int64 values, subject to the complete declared schema.
The old negative fixture treats the valid numeric value `7` as malformed.
The correction uses fractional `7.5` and adds an assertion that numeric revisions keep their carrier through record binding.
The [focused rerun](screen-tests-001/result.json) passes all 13 screen tests after recompilation.
Only that test file changes after the full sweep, so the retained sweep remains a failed run at its original source.

The armed native host lifecycle test passes in the combined sweep.
Its [termination receipt](combined-workspace-001/host-lifecycle/host-term.receipt) records successful shutdown after the NATS outage and recovery.
Its [blocked export receipt](combined-workspace-001/host-lifecycle/host-blocked-flush.receipt) records the expected failed flush and nonzero exit.
No second workspace sweep or cluster proof follows the test-only correction.

The effects implementation and integration validation are complete.
The [main integration](combined-main-landing-001/main-integration-result.json) lands at `2433bb47f053234fa745214a4e3446e1388fe4da`.
The [remote receipt](combined-main-landing-001/remote-main.json) records the matching pushed commit.
The integration preserves the original permissions of 1,472 evidence files and the unrelated files named in its receipt.
Receiving retains the original testing draft in its approved specification evidence before the main integration.

`wamn-b2m6.10` closes locally with the implementation and proof commits.
The shared Dolt publication remains blocked by an automatic approval rejection in the identity lane.
The integrator does not retry that rejected publication through another lane.
Receiving parity `wamn-10yt.62.6` still waits for the landed `wamn-ctc8.15.5` credential provider handoff.
The remaining WMS TUI proof and recipe keep their approved execution order.
