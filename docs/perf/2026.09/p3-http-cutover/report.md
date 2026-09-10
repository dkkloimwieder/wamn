# P3 HTTP cutover

Issue: `wamn-0h0g.2.7.17`.
Status: implementation and deployed correctness acceptance pass.
The integrated workspace retains the classified baseline failures below.

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
It compiles the new eight-case protocol proof.
The later deployed journey runs that proof with the published HTTP artifact.

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

## Deployed correctness proof

The [launch receipt](live-001-launch.json) records the command below at clean source `a2ea0ef32dde01408521041c0657e944fccf649d`.
The command returns zero after 1,892.13 seconds.
The source stays unchanged throughout the run.

```bash
tools/receiving-cluster-journey-run --apply --receiving-correctness \
  --evidence-dir /home/kaalin/dev/wamn/docs/perf/2026.09/p3-http-cutover/live-001
```

The [route log](live-001/production-route.log) reports one passed test, zero failed tests, zero ignored tests, and 74 filtered tests.
That test passes all 13 authenticated routes and all eight P3 protocol cases in 31.46 seconds.
The cases cover origin-form Host handling, two stalled-body refusals, the exact body limit, excess bytes, body failure, cancellation cleanup, and recovery.
The body-failure case refuses valid JSON followed by a transport error.
The cancellation case observes charged guest memory before cancellation and zero retained memory after the store drops.

The [Receiving log](live-001/receiving-correctness.log) reports one passed test, zero failed tests, zero ignored tests, and five filtered tests.
The [history evidence](live-001/receiving-correctness.jsonl) records three explicit histories, 16 generated histories, 129 completed steps, and seven boundary cases.
All 26 history and boundary fixtures pass.
The proof uses PostgreSQL 18.6, seed 7701, and a maximum of 64 shrink iterations.
The boundary cases cover refusal, mixed outcomes, contention, replay, rollback, a withheld response after commit, and denied-then-authorized access.
The [cleanup receipt](live-001/cleanup.receipt) records removal of the owned cluster, containers, and images.

## Artifact identities

The owner approves a separate HTTP shell Cargo invocation with the same target directory.
`tools/build-components` and the Docker component stage keep the async HTTP feature separate from application builds.
The deployed journey builds the standard host and gates images through that Dockerfile.
Application digest pins remain unchanged by this landing.

The [artifact receipt](post-build-001/receipt.json) records all four actual WebAssembly validation commands, their zero exit codes, and their interface inventories.
The release HTTP bytes have SHA-256 `33e08d96ece969573bb2dd153b8617d1d0755700fef168372a0d8b88a90b7da8`.
The [component hashes](live-001/component-bytes.sha256) record those same bytes before the in-process proof and OCI publication.
The shell exports `wasi:http/handler@0.3.0` and retains WAMN `0.1.0`, random `0.2.12`, and actual Rust standard-library `0.2.9` imports.

The [OCI publication receipt](live-001/flow-http-push.json) records manifest digest `sha256:35890f2307bf532bb44efd0022220c36eedf18af3237ead481f60acf8d688efd`.
The [deployment](live-001/flow-http-deployment.json) and [workload](live-001/flow-http-workload.json) select that same digest and the exact P3 handler.
The deployment reports one ready replica, and all five workload conditions report true.
The OCI manifest digest differs from the raw WebAssembly hash because it identifies the publication manifest.

The normalized application hashes are:

| Component | SHA-256 |
| --- | --- |
| Receiving | `cd494c3413f9987a645ea9320f64f3d9dc5b269296feb6a3289edb84ef065d2d` |
| Acme Receiving | `075c21ccefc571f504022a47800b527405180f8976f8ef1830ee92136c13add4` |
| WMS | `a8cb208f652a078689da1d44b506b6601f8dacca2bc63d47b263ff3730371610` |

Receiving matches the existing dependency pin retained by `wamn-10yt.72`.
The normalized applications retain their exact WAMN interfaces and `wasi:io` and `wasi:clocks` imports at `0.2.12`.
The raw materializer hash remains `e91ef70e236046a139072e2c202becc209c1be2d279f886dfa3e251890596e8d`.

The [release receipt](live-001/print-release-env.receipt) records manifest `sha256:b15e7990cafed37d531901bb428c8936ed1b3c4fb5ed7a4acfc63de3d2ca4e40`.
The [host image](live-001/host-image.json) configuration digest is `sha256:bf7f3d8dbf06be47392c497110dd07da9e703cfef79a45d35146511b09128c6a`.
Its node runtime digest is `sha256:7723deda52812d5689fdbf08a469fd70d9666acda877611241993f68d7855584`.
All three ready host pods select that runtime digest.
The [gates image](live-001/gates-image.json) configuration digest is `sha256:2f3cb10b34b5b7e38b21ebbf151b1990b68441e609b199eec2a4c018628afd85`.
Its node runtime digest is `sha256:1e23bcd157791478848b0839d9d57ebee123d62430e020aa8339a53dedf50b64`.
The node receipts record both image loads on all three nodes.

## Comparison with the 2.9 correctness baseline

The earlier [Receiving restoration proof](../receiving-correctness/restored-001/result.json) passes at `0673f3f54f2db1250c6b023a11d7dc5877a944da` on unmodified upstream 2.9.
The [comparison receipt](baseline-comparison-001/receipt.json) compares that evidence with this P3 run.
Both runs use the same corpus, seed, schemas, generator provenance, compiler, PostgreSQL version, and eight invariants.
Both execute the same 19 histories and 129 steps, and both pass the same seven boundary cases.
The application hashes differ between those source revisions and are recorded without a claim of identical application bytes.
The current Receiving hash matches the retained `.72` pin.
The owner prioritizes correctness and directs no further benchmarks.
This comparison establishes preserved correctness coverage and makes no latency, throughput, or end-to-end async claim.

## Integrated workspace and source history

The integrated source is `481d281ba8138c639345daa56b0bd2da0ff1952d`.
It retains the exact deployed source commit `a2ea0ef32dde01408521041c0657e944fccf649d` as a reachable ancestor.
The intervening `.79` and `.68` landings change four schema-generator files outside P3 ownership.
The P3 source files retain the exact bytes used by the deployed proof.
The [integration receipt](main-integration-001/receipt.json) records preservation of 146 existing dirty files and the shared index.

The final command runs on that integrated source with no source changes:

```bash
cargo test --workspace --no-fail-fast --locked --offline -- \
  --include-ignored --nocapture --test-threads=1 \
  --skip regenerate_checked_in_journey_schema \
  --skip regenerate_checked_in_dev_config_schema
```

The [command receipt](integrated-workspace-001/result.json) records exit 101 after 219.43 seconds.
Final target summaries report 2,195 passed tests, 75 failed tests, zero ignored tests, and two filtered regeneration cases.
All six doctests pass.
These totals exclude one nested subprocess summary.
The log records at least 85 successful tests that skip their own live work.
Reported passes therefore do not represent 2,195 executed live proofs.

The [classification](integrated-workspace-001/workspace-results.json) retains each test result.
The [comparison](integrated-workspace-001/baseline-comparison.json) matches exact package, Cargo target, test name, and failure cause with the retained baselines.
All 74 failures from the latest generated-TUI workspace baseline remain with unchanged failure signatures.
The sole addition is `wamn-host --test native_lifecycle_live`, test `rebuilt_host_probes_signals_and_scheduler_recovery`.
It refuses the unarmed run with `set WAMN_HOST_LIVE_NATS_SERVER_BIN to the real NATS server binary`.
The existing armed host lifecycle proof remains separate evidence.
There are no unresolved target, count, or compilation classifications.
The workspace baseline and the separate `wamn-10yt.80` lint finding remain visible.

## Remaining deviations

WAMN routing, authentication, and delivery imports remain synchronous.
Native dispatch B remains blocked under its existing public-API stop condition.
This landing does not change warm reuse, ledger row 4, or tenant admission.
The `wamn-10yt.74`, `wamn-10yt.75`, and `wamn-10yt.76` follow-ons remain open.
The ledger and `[P3-HTTP]` build recipe record this landing and its executed proof boundaries.
