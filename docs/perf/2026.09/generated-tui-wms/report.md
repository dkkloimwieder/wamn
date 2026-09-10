# WMS generated form proof

This report records `wamn-10yt.62.7` at source `e0ea0de395165403591994f66a3a4142206f7f2b`.
The source includes native descriptor changes through `7a50997b`.
The platform response implementation is `5b9526f2`, integrated through `c32aeed9` for `wamn-b2m6.10`.

The example composes generated `pallet.get` and `inventory.move` screens through the shared terminal driver.
A matching successful pallet read supplies the protected pallet ID and revision.
The operator enters the destination.
The generated package stays unchanged because its contract declares no record mapping for this command.
The shared reducer owns success, partial completion, uncertainty, and submission reuse.

## Focused measurements

The [focused run](focused-003/result.json) passes 26 shared terminal tests and five example tests.
Scoped Clippy and the native example build also pass.
The first run fails compilation because a test names a nonexistent draft variant.
The second run exposes a test expectation that omits the required six fractional timestamp digits.
Both failed runs remain beside the passing run.

The example sends through `WamnClient` and compares the entire outgoing body with a fixed byte literal.
Its assertions cover protected bindings, incompatible reads, target replacement, label rendering, partial completion, and missing evidence.
The [negative control](negative-control-001/result.json) changes only the bound revision from seven to eight.
The exact wire test fails with exit 101 after compilation.
Restoring the original bytes returns that test to passing.

The [database preflight](database-preflight-001/result.json) runs the actual fixture statements against fresh PostgreSQL 18.
Seed, observation, fixture cleanup, and container cleanup pass.
The [terminal preflight](terminal-preflight-001/result.json) drives the compiled binary through both response modes.
Its HTTP responses and database observations are explicitly synthetic.
Those results prove the driver controls and assertions, without proving platform execution or object storage.

## Live terminal results

The [successful session](live-001/journey/generated-tui-success/result.json) displays the stored label key through the served response descriptors.
The harness reads the corresponding MinIO object and compares its hash with the returned ZPL bytes.
The [partial session](live-001/journey/generated-tui-partial/result.json) runs after removal of the disposable labels bucket.
It displays the committed movement beside `write_failed` and the observed `responded` effect outcome.
The response carries only the committed result and failed outcome.

Each session sends one pallet read and one move through the real route and credential provider.
The recording relay forwards the original request bodies, routing host, and authorization header.
It retains the HTTP bodies and credential-match boolean, without retaining the authorization value.
After F7 and Ctrl-S, the relay still records only those two requests.
Database assertions also retain one matching claim, one movement, the new pallet revision, and the unchanged stock quantity.

Both sessions restore the terminal and remove their owned database fixtures.
The [success transcript](live-001/journey/generated-tui-success/terminal.ansi) and [partial transcript](live-001/journey/generated-tui-partial/terminal.ansi) retain the actual terminal output.
The [HTTP evidence](live-001/journey/generated-tui-partial/http.json) ties the partial display to the live served response.
This proof covers an observed store failure after commitment.
Missing evidence remains uncertain in the shared reducer tests, without a live timeout or response-loss claim.

The [complete journey](live-001/result.json) exits successfully after 274.357 seconds.
Its [cleanup receipt](live-001/journey/cleanup.receipt) records removal of the owned cluster, containers, reader, and host image.
The existing WMS contention, remaining-operation, platform partial-response, scheduling, and idle-materializer assertions also pass.
The source tree remains clean at the measured commit.

## Workspace boundary

The [clean workspace sweep](workspace-001/run.json) exits with 101 after 170.172 seconds.
It reports 2,183 test passes, six doctest passes, and 74 failures across 34 targets.
It includes ignored tests and explicitly filters the two schema-regeneration tests.
At least 85 reported passes skip their live or artifact proof.
Subtracting those skips does not establish an exact executed proof count.

The [baseline comparison](workspace-001/baseline-comparison.json) matches all 74 Receiving-baseline failure identities and classified causes.
There are no added failures, removed failures, or changed causes.
The workspace WMS live test lacks its private journey document in that sweep.
The separately armed live journey above runs and passes that test.
The [contract follow-up](contract-diff-001/result.json) passes all 36 tests.
