# Receiving overlay and post-commit proof

Issue: `wamn-10yt.78`.

The application proof is complete.
The baseline and fresh additive installations each pass all five proof arms at `72142cf353da2e39afdb17bbfc153dd9d8ee1790`.
Both deliberate controls detect their intended defects.
Exact source and component restoration passes, and the final restored baseline passes at `b5aaa243a57c21e0d543c3d3f47d08cd876a34a5`.
The workspace sweep remains failed with exit 101, and every failure is classified.

Dependency `wamn-10yt.81` is now published at `ccd3ac401f7be09ddca39d43b727dfa092297db5`.
Its [scoped local proof](../receiving-update-projection/report.md) passes, and live attempt 005 exercises the repaired generated queries in both installations.

## Source and scope

The accepted contract is [Increment 2](../../../poc/wamn_testing_spec.md).
The application defines `OVL-SCHEMA`, `REC-EVENT-REPLAY`, and `REC-POSTCOMMIT-PROGRESS` in its [scenario](../../../poc/wamn_receiving_layered_application_poc_scenario.md).
The proof uses independent fresh installations for the baseline and additive base, with the same overlay files and component bytes.
The breaking candidate uses another empty database and the existing package ownership refusal.
This proof does not exercise an in-place upgrade.

The implementation lives in the Receiving integration suite and its existing cluster runner.
Commit `3876c119dd5a9b70ad960cdaa77654b9b53999c3` adds the existing schema introspection crate as a test dependency.
Commit `a0775e679cd9a2a2230907aa6da6af2c5f47338c` repairs observation of the installed schema.
Production introspection rules and installed permissions stay unchanged.

The [tested integration boundary](main-landing-001/result.json) is `b7275405c8adca5aeef0373cfb8276d5b1678717`.
The [rebase record](rebase-003/result.json) identifies only D proof documentation changes since live attempt 005.

## Executed local evidence

Four focused tests pass: the required-schema comparison, generated journey schema equality, strict parser agreement, and the example document.
Each retained test command executes exactly one test.
The seven runner selection cases produce their expected results in [runner-selection-001](runner-selection-001/result.json).

The [PostgreSQL 18.6 observer test](observer-pg18-002/observer.json) observes three tables, fourteen required fields, and seven constraints.
It retains the installed schema permissions and the authoring introspector's `unsupported-acl` refusal.
It rejects four controlled changes: consumed nullability, a changed CHECK expression, an unvalidated CHECK, and a deferrable foreign key.
Each control rolls back, and the final projection matches its prior value.
The disposable PostgreSQL configuration and listener are absent after [cleanup](observer-pg18-002/cleanup.json).
This narrow observer test does not replace the independent fresh-install comparison.

[Scoped Clippy](clippy-004/result.json) exits zero for the integration library and tests, including the final elapsed-time assertion.
Existing warnings remain, with no diagnostics in the two new proof modules.
The first Clippy attempt finds the existing evidence-borrow defect, which `wamn-10yt.80` fixes at `1d38b6da38a460753d89f877ec0a0c68345a7d60`.

The [local HTTP fixture](http-transport-loopback-001/result.json) exercises the repaired runner commands with exact payloads, Host headers, trace headers, and a synthetic bearer.
The positive case sends one GET and two POSTs.
A 503 response stops the runner after one update POST, without sending a receipt POST.
The bearer configuration uses mode 600, and retained artifacts exclude the token.
The [legacy assertion replay](http-transport-legacy-replay-002/result.json) accepts prior successful receipts and rejects the actual failed probe Pod.
These results establish the runner transport behavior, not application or materializer behavior.

## Successful paired proof

[Live attempt 005](live-005/result.json) exits zero after 2,154.196 seconds at source `72142cf353da2e39afdb17bbfc153dd9d8ee1790`.
The [pair result](live-005/pair/pair.json) records two independent fresh installations and a passing result.
The elapsed time records this functional run only.

All 81 overlay files match exactly between the baseline and additive installations.
Both use component hash `sha256:d69ad067860bbc9cccf0c843475b75211ac7e9ce6e7e05a9d3a50918753a66ec`.
Both installed schemas satisfy three tables, fourteen required fields, and seven constraints.
The separate breaking installation returns `base-definition-mutation-refused` for `receiving.purchase_order.acme_inspection_required`, owned by `wamn_receiving`.
It leaves no partial overlay state and removes its database.

The [baseline record](live-005/pair/baseline/postcommit.json) and [additive record](live-005/pair/additive/postcommit.json) retain the actual replay and event progress evidence.
Both tests wait 122,000 milliseconds before replay so that the broker accepts the duplicate as a new delivery.
Each replay reaches the registered handler after its first completion and preserves one approved inspection at row version 2.

Each installation records three blocked handler attempts and an actual correlated `router-retry-exhausted` dead-letter record.
Independent receipts receive their own pending inspections in 15,698 milliseconds for baseline and 15,913 milliseconds for additive.
Both results satisfy the 90-second progress bound.
These bounds assume that the owned database, broker, host, and materializer remain available.

Both proofs release the database lock and restore the materializer.
The [baseline verdict](live-005/pair/baseline/verdict.json) and [additive verdict](live-005/pair/additive/verdict.json) each record five passing arms.
The [baseline cleanup](live-005/pair/baseline/cleanup.receipt) and [additive cleanup](live-005/pair/additive/cleanup.receipt) name the exact owned resources and report success.
The [pair receipt](live-005/pair/pair.receipt) records passing cleanup for the complete pair.

## Replay-reset control

The [replay-reset control](replay-reset-001/control-verdict.json) exits 101 at source `e581881623a3e8f4378f768309242bfaf9eed224`.
It fails at the intended assertion: `duplicate delivery reset the approved inspection or created another row`.
The [artifact observation](replay-reset-001/artifact-observation.json) shows that the installed overlay matches the prepared mutated component.
Its component hash is `sha256:566decee50b89abd0c336caed59bc268dcf6f7db5fc18775b8b2809a3ce63e2a`.

The inspection is approved at row version 2 before replay.
The actual repeated handler delivery records both `accepted` and `settled`, with outcome `discard`.
The durable acknowledgment advances by one before the state assertion fails.
The test stops before it acquires the poison lock or changes the materializer scale.
The [cleanup receipt](replay-reset-001/live/journey/cleanup.receipt) records success for the exact owned resources.

The [restoration commit](replay-reset-001/restore-commit.json), `36c3b12c4ce6c2d0303b0f1b1e6d60168bcb7472`, restores all seven control paths.
The retained [file state](replay-reset-001/state.json) matches the original bytes, modes, and timestamps.
The restored source tree equals `72142cf353da2e39afdb17bbfc153dd9d8ee1790`.

## Timeout control

The [first timeout attempt](timeout-terminal-001/control-verdict.json) is invalid because it serves the prior replay mutation.
Its [live command](timeout-terminal-001/live/result.json) exits 101 after 3,275.083 seconds at source `8cb7fa0f0208cda9218028d1b1287291d2dbe3b1`.
It fails early with `private handler settled with the wrong outcome` and never reaches the intended timeout assertion.
This failure does not count as detection of the timeout defect.
[Cleanup](timeout-terminal-001/live/journey/cleanup.receipt) passes, and the [invalid component](timeout-terminal-001/invalid-timeout-component.wasm) remains available as evidence.

The restored SELECT uses digest `sha256:0f21db223ec017fa15f04c26dd646b85270965bda939885c2195674960e9242a`.
The [artifact observation](timeout-terminal-001/artifact-digest-observation.json) finds the prior replay digest, `sha256:de7e261650fa3eacef035e830803cfdc61008ab12fb6390c28c6281b64b61285`, in both raw and served components.
The prior `cargo clean` omits `--release`, so the cached data-access library survives the restoration of original timestamps.
The [mutation procedure](tools/mutation.md) now specifies `--release`.

The corrected attempt uses the same clean source commit.
Its [release cleanup](timeout-terminal-002/clean-release.log) removes 15 files totaling 2.0 MiB.
Its [m1 build](timeout-terminal-002/build/result.json) passes in 5.533 seconds and recompiles the data-access library.
The [prepared artifacts](timeout-terminal-002/prepared-scope.json) contain the original digest, with the prior replay digest absent.
Only the overlay component changes, to `sha256:6f79598ca74341fcbe8f42a04373c4c393e92052307669e45e6a92b8bf780e7d`.

The [corrected control](timeout-terminal-002/control-verdict.json) exits 101 after 368.348 seconds at the intended assertion: `expected three real blocked handler attempts, observed 1`.
Replay completes before poison processing and preserves the approved inspection at row version 2.
The proof observes exactly one blocked handler attempt.
The served overlay matches the prepared component.

The helper finds a dead letter that matches the source stream and sequence before the assertion.
The full dead-letter record is not saved before this assertion, so this control does not prove its final contents.
Proof cleanup releases the lock and restores the materializer, and [cleanup of the exact owned resources](timeout-terminal-002/live/journey/cleanup.receipt) passes.

## Restored baseline

Commit `b5aaa243a57c21e0d543c3d3f47d08cd876a34a5` [restores the source](restored-positive-001/source-restoration.json).
The complete source tree equals `72142cf353da2e39afdb17bbfc153dd9d8ee1790`.
The normal [release rebuild](restored-positive-001/build/result.json) passes after [release cleanup](restored-positive-001/clean-release.json).
The [artifact record](restored-positive-001/artifact-restoration.json) shows that all four component hashes match the original positive run.
The original statement digest is present, and the prior replay digest is absent.

The [final baseline result](restored-positive-001/verdict.json) records exit zero after 353.63 seconds.
All five [proof arms](restored-positive-001/live/journey/verdict.json) pass.
The proof observes three blocked handler attempts and a real `router-retry-exhausted` dead letter with broker delivery count 1.
The independent receipt receives one inspection in 15,849 milliseconds, below the 90,000-millisecond bound.

All 81 overlay files match the original positive run.
The served component hash is `sha256:d69ad067860bbc9cccf0c843475b75211ac7e9ce6e7e05a9d3a50918753a66ec`.
The proof releases the lock and restores the materializer, and [cleanup of the exact owned resources](restored-positive-001/live/journey/cleanup.receipt) passes.

This run covers one restored baseline installation after both valid controls.
The earlier [paired run](live-005/pair/pair.json) supplies the separate fresh additive installation proof.

## Integrated workspace sweep

The [completed sweep classification](integrated-workspace-001/interpretation.json) records exit 101 at source `b7275405c8adca5aeef0373cfb8276d5b1678717`.
Cargo reports 2,231 passing tests, six passing doctests, and 84 failures.
The test passes include 85 explicit self-skips, which supply no live proof.
The [source record](integrated-workspace-001/source-stability.json) shows an unchanged commit and a clean tree.

All 81 failures from D return.
Eighty retain their exact causes, and one WIT diagnostic differs only in its checkout prefix.
Two added failures come from deliberately unarmed `.78` tests: the observer lacks PostgreSQL input, and the post-commit test lacks its journey document.
The third added failure comes from the sweep wrapper, which places `TMPDIR` inside the checkout.
Every failure is classified.

The [focused rerun](workspace-tempdir-rerun-001/classification.json) passes `workspace_tier_helper_runs_safely_outside_repository` using the exact test binary from the sweep.
It executes one test and changes only `TMPDIR` to a directory outside the checkout.
The temporary directory is removed, and product source stays unchanged.
The [original wrapper](workspace-tempdir-001/integrated_sweep_before.py) remains intact, with its hash matching the [launch record](integrated-workspace-001/tools.json).
The wrapper now places temporary directories outside the checkout for future runs.
The full sweep remains failed and did not run again.

## Retained failed attempts

[Live attempt 001](live-001/classification.json) exits 101 at source `35c11280713a05ca15dec8d41e2085f94543d307`.
The route and P3 phase reaches the new schema observation, where authoring introspection rejects the installed schema permissions.
The post-commit and additive legs do not execute.
Owned-resource cleanup passes.
The failed result and exact source patch remain in that directory.

[Live attempt 002](live-002/classification.json) passes the production route test and observes all fourteen required fields and seven constraints.
The separate breaking installation refuses with `base-definition-mutation-refused`, leaves no partial overlay state, and removes its database.
The reachability probe then exits 127 because the selected standard host image lacks `curl`.
The additive and post-commit legs do not execute, and owned-resource cleanup passes.
The complete source patch and hashes remain in that directory.

[Live attempt 003](live-003/classification.json) passes the repaired HTTP probe, both initial HTTP writes, and the causal CDC/materializer test.
The telemetry helper then refuses the new disposable namespace before it collects evidence.
The additive and post-commit legs do not execute, and owned-resource cleanup passes.
The repaired [namespace guard](telemetry-scope-001/result.json) admits exactly the old and new disposable context/namespace pairs.
It refuses crossed pairs, the frozen cluster, and an unnamed cluster before any external command.
That local guard test does not collect telemetry.

[Live attempt 004](live-004/classification.json) passes all five baseline arms and telemetry collection at source `95dc61c1fe2d1ec624f899aa89a63da5c72abe0d`.
The replay reaches the registered handler after its first completion and preserves the approved inspection.
The poison event produces three blocked handler attempts and an actual correlated `router-retry-exhausted` dead-letter record.
The independent receipt receives its inspection within 15,618 milliseconds, below the 90-second bound.
The proof releases the database lock and restores the materializer.

The separate fresh additive installation fails the production route test with `permission_denied` for `purchase_order.update`.
This failure occurs before the intentional grant-revocation control and before the additive post-commit tests.
Both installations pass their owned-resource cleanup.
The full pair exits 101 and produces no passing pair receipt.
The complete source patch and hashes remain in that directory.

The [PostgreSQL diagnosis](returning-privilege-001/diagnosis.json) uses production package installation and permission reconciliation in two disposable databases.
Both shipped UPDATE statements pass against the baseline schema and fail against the additive schema with SQLSTATE `42501`.
The new column has no SELECT grant, but `RETURNING model.*` requests it.
Replacing only that wildcard with the declared columns makes both statements succeed under the same role and grants.
Every statement transaction rolls back, and disposable database cleanup passes.

This diagnosis covers the candidate that changes initial DDL while retaining generated SQL and grants.
It does not establish failure for every regenerated additive base.
The published `wamn-10yt.81` repair emits the exact declared model columns in generated UPDATE results and regenerates the affected packages.
Grants, operation inputs, and operation outputs stay unchanged.
The regenerated UPDATE contracts change only their statement digests.
The overlay artifacts must remain identical between the two installations in the paired proof.

The [repair report](../receiving-update-projection/report.md) and [final results](../receiving-update-projection/final-checks.json) record the scoped local PostgreSQL 18 proof.
Both regenerated queries pass while the additional column remains ungranted.
Both old-query controls fail with the intended permission error.
Exact restoration and the rebuilt positive test pass.
These local results do not complete the paired cluster proof or its guest defect controls.

The first local observer fixture omits permission reconciliation, so it fails before the observer assertions.
The second fixture uses the production reconciliation function and passes.
The initial schema build also retains its compile failure before the NATS header conversion repair.
None of these failures count as detection of a guest defect.

## Reproduction

From the clean implementation commit, run the paired proof with a new evidence directory under this report directory.

```bash
tools/receiving-postcommit-proof --apply \
  --evidence-dir /home/kaalin/dev/wamn/docs/perf/2026.09/receiving-postcommit/REPLACE_WITH_NEW_RUN
```

The [capture tool](tools/capture.py) records exact arguments, source identity, output, exit status, and elapsed time.
The [guest defect procedure](tools/mutation.md) specifies the two controls, their intended failures, and exact restoration.
Its offline helper results do not count as executed application proofs.
