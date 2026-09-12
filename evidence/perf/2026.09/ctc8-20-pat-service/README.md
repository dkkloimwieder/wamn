# PAT service correctness proofs

These records cover `wamn-ctc8.20` on 2026-09-10.
They contain no benchmarks.
The first implementation commit is `2011eb2a`.
The separate TUI harness commit is `43d5c347`.
Neither commit is on main at this evidence boundary.

## Positive tests

The initial debug build passed in [native-001](native-001/build.log).
The final unit run passed 409 tests in [native-004](native-004/unit.log).
Six existing ignored tests did not run and do not count as proof.
The [workspace check](native-004/workspace.log) and [scoped Clippy check](native-004/clippy.log) returned zero.
Clippy reported warnings, so this result does not claim a warning-free tree.

Each live suite used its own fresh PostgreSQL 18 container and a database named `wamn_system`.
The runner removed each owned container and its volumes.
The [capture script](native-003/run-live.sh) records the exact arming variables and commands.
Each suite has a separate exit file, database log, and cleanup receipt.

| Suite | Passed tests | Evidence |
| --- | ---: | --- |
| Native certificate-authenticated PAT issuance | 2 | [service.log](native-003/service.log) |
| First-time CLI bootstrap through the separate process | 1 | [bootstrap.log](native-003/bootstrap.log) |
| Scoped issuer database grants | 1 | [grants.log](native-003/grants.log) |
| Compiled issuer CLI and credential generations | 2 | [grants-cli.log](native-003/grants-cli.log) |
| Existing session exchange | 1 | [session.log](native-003/session.log) |
| Existing HTTPS JWKS and health routes | 2 | [jwks.log](native-003/jwks.log) |

The bootstrap proof observes the database login that inserts each PAT.
Both inserts came from the scoped identity process.
The process stopped, its login lost authority, and its private certificate files disappeared after provisioning.
The proof also authenticated and revoked a returned PAT.

## Deliberately broken controls

The [mutation script](native-005/run-mutants.sh) started from clean commit `43d5c347` after the positive tests passed.
Each change made its named test fail with exit 101 for the intended reason.
The script restored each source file through `apply_patch` and compared its SHA-256 hash.
Each database mutation used a fresh, separate container.

| Broken control | Observed failure | Evidence |
| --- | --- | --- |
| Bypass the operator certificate guard | Anonymous issuance returned 201 instead of 403 | [operator.log](native-005/operator.log) |
| Follow two redirects | The client sent three requests instead of one | [redirect.log](native-005/redirect.log) |
| Allow INSERT into `revoked_at` | The server reported an extra column grant | [grants.log](native-005/grants.log) |

The [result](native-005/result) records all three detections and exact source restoration.

The [restored-source run](native-006/run-restored.sh) rebuilt and tested clean commit `43d5c347` after all mutations.
All 409 unit tests, [seven session-token tests](native-006/session-token.log), and [13 public-key-cache tests](native-006/session-cache.log) passed.
The runner also rebuilt the unmodified native identity binary.

## Post-P3 workflow

The PAT branch rebased without conflicts onto main commit `88db1d3a`.
The resulting source commit is `6b91d423`.
Its [409 unit tests](post-p3-001/unit.log) and [workspace check](post-p3-001/workspace.log) passed.
The [dry-run](post-p3-001/dry-run.log) selected only Receiving correctness, with no benchmark mode.

The [full workflow](post-p3-001/run-receiving.sh) built the standard host and proof images from that clean source.
The [console log](post-p3-001/receiving.console.log) records all 13 local routes and eight P3 protocol cases passing after native PAT bootstrap.
The deployed proof passed three explicit histories, 16 generated histories, and seven boundary cases.
The [summary](receiving-001/receiving-correctness-summary.json) records those results.
The [final receipt](receiving-001/receiving-correctness-journey.receipt) binds the result to the source and successful cleanup.
The runner removed only its owned cluster, containers, images, and private scratch files.

This proof exercises the separate native PAT bootstrap process, not an operator-CA mount through the identity Helm chart.

## Optional operator CA deployment

The owner approved the three identity Helm files and full public Beads publication on 2026-09-10.
The chart accepts an existing dedicated CA Secret through `operatorCaSecret`.
Its default remains empty, which disables PAT issuance.
The mount contains only `ca.crt`, read-only with mode `0400`.
The deployment instructions require a service restart after the trusted CA changes.

The first [render test](helm-001/positive.log) exposed a test-parser error with nested Secret names.
Commit `94d5992f` corrected that parser without changing the chart.
All three [identity render tests](helm-001/positive-fixed.log) then passed, and [Helm lint](helm-001/lint.log) passed.
The [negative control](helm-001/broken-path.log) changed the CA environment path to a nonexistent file.
The named operator-CA test detected that exact difference and returned 101.
The [restored run](helm-001/restored.log) passed all three tests with the original template SHA-256 hash.
The [full inventory](helm-001/inventory.log) passed eight tests and retained the existing Secret-inventory failure, `wamn-362o.58`.

These chart proofs render manifests locally. They do not claim an installed Helm deployment.

## Integrated workspace

Main fast-forwarded to `437672fc` with unchanged existing file hashes, modes, and staged changes.
That merge retains the original native proof commits without changing the current source tree.
The executed Receiving source, `6b91d423`, also remains an ancestor.

The [workspace command](main-landing-001/workspace.command) ran on main and returned 101, with no compile error.
It reported 2,209 test passes, six documentation-test passes, and 78 failures.
At least 85 reported passes explicitly skipped their subjects and do not count as live proof.
The command excluded the two existing schema-regeneration tests to preserve shared files.
The source patch stayed empty throughout the run.

The [comparison](main-landing-001/workspace-p3-comparison.json) matches all 75 P3 failures by test identity and observed cause.
The two new PAT live tests refused absent disposable database inputs.
Their separate armed runs passed in [bootstrap.log](native-003/bootstrap.log) and [service.log](native-003/service.log).

The remaining failure comes from `version_identity::wamn_wit_packages_stay_at_mvp_version`.
It treats the opening brace in bundled WIT declarations as part of the version.
All eight named declarations already use `0.1.0`.
The [base objects](main-landing-001/wit-guard-base.objects) and [merged objects](main-landing-001/wit-guard-merged.objects) are identical for that guard and its named evidence files.
The existing guard inventory, `wamn-0h0g.15.137`, records this false positive.
No guard, version, or historical P3 evidence changed.

This workspace result does not claim a fully passing suite or complete release readiness.
