# Journey helper retirement

Date: 2026-09-11. Work: `wamn-47wm.4.1`, `wamn-47wm.4.2`, and `wamn-47wm.4.3`.

Receiving and WMS now call their Rust test owners directly.
The ten shared shell helpers, ten paired shell tests, and the telemetry script have no current executable callers.
The [source record](source.json) lists the exact deleted files and their previous hashes.
The [existing case map](../rendering-port-001/rendering-case-map.json) and [helper case map](../rendering-port-001/helper-case-map.json) retain the earlier comparisons.

Commit `01799b2f` transfers the remaining throughput Job fields, generated script output, credential order, and retry response cases into ordinary app tests.
The WMS retry script bytes stay unchanged.
The old Bash reference-name and argument-parsing cases no longer apply to typed Rust calls.
Rustfmt and whitespace checks pass.
No Cargo test or service ran in the deletion lane.
The first integrated run at `8759198e` passes all three WMS cases and six Receiving cases.
One Receiving case fails because its local pgbench executable matches the generated sample-file glob.
The [original failure log](../retained-rendering-tests-001/receiving.log) remains unchanged.
Commit `859948c6` places that executable in a separate `bin/` directory.
The generated script stays unchanged.
The corrected integrated run remains pending.
Commit `9c7cf281` makes the local pgbench executable stop if its required statement is absent.
The [command comparison](document-command.json) executes both document writers with synthetic inputs and confirms the same eleven fields and values.
Both commands exit zero and remove only their owned temporary directory.
The [caller search](current-callers.json) finds no remaining current source references.

Two historical executable capture tools now depend on retired entrypoints.
`docs/perf/2026.09/effects-response/tools/partial_document.sh` uses `tools/journey-document.sh` and reads an old WMS shell block.
Its current owners are `test-support/harness/src/journey.rs` and `apps/wamn_wms/tests/cluster/application.rs`.
The strict document tests remain in `tests/integration/src/route_authentication_live.rs`.
`docs/perf/2026.09/receiving-postcommit/tools/telemetry_scope.py` imports the retired `tools/journey-telemetry-proof`.
Its current owner is `test-support/infrastructure/traces/telemetry.rs`, called from the Receiving default and postcommit cases.

Those tools and their captured results stay unchanged as historical records.
Their original source revisions remain necessary to replay them.
The Rust owners use the declared private cluster inputs and existing document types.
This deletion does not claim that old source readers execute against the new tree.
