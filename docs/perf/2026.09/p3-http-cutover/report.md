# P3 HTTP cutover

Issue: `wamn-0h0g.2.7.17`.
Status: implementation in progress, with no deployed acceptance claim.

## Scope

The owner permits this cutover ahead of blocked native dispatch B.
The HTTP shell exports `wasi:http/handler@0.3.0` through the existing unmodified 2.9 host.
The root runtime remains pinned to `68ebece9c537f8bb4b5c9999f274ec68d60f35a9` and Wasmtime 47.0.4.
WAMN routing, authentication, and delivery imports remain synchronous.
Application node dispatch remains unchanged.

The shell reads P3 body streams only after authentication.
It retains the selected route's body limit and existing response contracts.
It awaits the body result after stream closure to distinguish transport failure from clean completion.
The response writer runs after the handler returns its response.
Both platform manifest paths declare the exact P3 handler interface.
The three existing in-process drivers use public P3 APIs and fresh stores.

The change removes the shell's P2 incoming handler, blocking stream calls, response outparam, and unused P2 WIT dependency tail.
It removes the three P2 proof drivers when their subject changes to P3.
The general host retains P2 support for its other consumers.
The tenant capability registry, virtualization pin, and ledger row 4 remain unchanged.

## Executed evidence

The isolated debug guest builds and passes `wasm-tools validate`.
Its SHA-256 is `83d0801085446e97267e98adef74d8730cdc2cb641ea9f26d1df886e18076d8e`.
The actual import report retains the WAMN imports and the Rust standard library's existing WASI imports.
It exports the exact P3 HTTP handler.

All 21 adapter tests pass, with no ignored cases.
The authentication-order mutant fails `an_authentication_refusal_never_reaches_delivery` because authentication waits for the stalled body.
The restored suite passes all 21 tests after mutant build outputs are removed.
The final fixture signature also passes all 21 tests in `adapter-tests-final-002`.
Its test binary SHA-256 is `618e3ace88eb5fbbccbffd982ed1d631bc2cf5e872fd62d3a5fd5fe50bd8abbf`.
The existing workload manifest proof passes all 32 properties.
All seven build-selection tests pass after the owner approves HTTP shell isolation.
The proof retains exact package closure, the artifact-plan round trip, and distinct watch roots.
The first integration compilation succeeds and runs no tests.
Its concurrent helper edits are recorded, so it is not a final source-identity proof.
The second integration compilation succeeds with no source changes during the command.
It compiles the new eight-case protocol proof but does not run that proof.

The capability registry guard initially fails because the HTTP WIT no longer supplies tenant adapter provenance.
The replacement guard inspects the actual pinned WASI-Virt adapter through public APIs.
It confirms the existing `0.2.12` registry versions without changing production policy.
All three registry tests pass in `capability-registry-002`.
The production adapter SHA-256 is `bc0f8c4e223b67f594794d0935a240aaf813637f1b64daa222274f75856faeb8`.
The explicit random-refusal adapter SHA-256 is `9a53d6542e37fc887529759c42053f87c7e6ccbc7968251834d4820f0677bec7`.

The guest and adapter Clippy commands return zero.
Their remaining diagnostics concern existing code and generated bindings.
The integration Clippy command returns 101 for an unchanged Receiving history test.
At `tests/integration/tests/receiving_command_histories_live.rs:1044`, `drop(evidence_cell)` triggers `clippy::drop_non_drop`.
This source file matches the lane baseline and is outside the P3 change.
The separate finding is `wamn-10yt.80` and remains open.

Each evidence directory retains its exact command, source patch, source hashes, output, and artifact identities.
The first guest build fails because the removed P2 WIT package leaves an empty directory.
The second build passes after that empty directory is removed.
The trimmed P3 dependency WIT produces the same guest bytes.

## Workspace sweep

`workspace-sweep-001` runs `cargo test --workspace --no-fail-fast --locked --offline`.
The source is `df0ce242014b48b3454fce2a742d58056d750e15` plus the captured patch.
The patch SHA-256 is `eaae51ea1f9564c74f5bbb8b86f63420b16aac42a764c56edaa6b2393ff537c6`.
The log SHA-256 is `cc265a0f03250890e9269b930bc508b977ef0e810580c11421d6a35107266baf`.
The command returns 101 after 445.15 seconds.

Final summaries report 2,185 passed tests, one failed test, and 83 ignored tests.
Doctests report four passed cases and two ignored cases.
These totals exclude one nested runtime subprocess summary.
Successful-test output is captured, so the log does not establish the number of tests that skip themselves.
Reported passes are not an executed live-proof count.

The sole failure is `wamn-proof-system --test deploy_platform_inventory` test `every_mounted_secret_is_declared_here_or_named_a_prerequisite`.
Its undeclared `wamn-object-store-credentials-acme--wms--dev` mount diagnostic exactly matches the cutover baseline.
See `../wasmcloud-2-9-cutover/validation-001/workspace-results.json` for the earlier failure.
The earlier chart connection failure is ignored in this command, not fixed.
Only `docs/operations/build-and-test.md` changes during this sweep.

This sweep runs in the lane before integration.
It does not replace the integrated-tree sweep required by the build recipe.

## Remaining acceptance

The owner approves a separate HTTP shell invocation with the same target directory.
The build tools and Docker component stage retain that boundary.
Application digest pins remain unchanged.
The in-process and deployed acceptance must use the same P3 artifact.
The new eight-case protocol proof and the deployed Receiving correctness journey have not run.
The integrated-tree sweep, final source identities, and publication remain open.
The existing integration lint failure remains visible.
The default workspace sweep does not replace the armed Receiving correctness journey.
No benchmark runs and no performance improvement is claimed.
