# Native dispatch production integration

Owner: `wamn-0ct2.2`. The production substitution and its owning correctness proofs are complete.
One implementation per imported operation interface replaces selection of a provider for each dependency.
Native execution replaces the manual implementation for released, nested, and candidate calls.
The final workspace sweep retains 83 baseline failures and one unarmed fixture failure, which passes in its separate armed proof.
Each result below names its actual source and artifacts. Git and Beads record publication status.

The [implementation checkpoint](report.md) records the earlier helper proofs.
The [B plan](../../../architecture/wamn_native_alignment_plan.md#b-replace-manual-guest-execution-including-its-duplicate-caches) retains the deletion contract and public-API boundary.
The [recipe](../../../operations/build-and-test.md#native-b-native-application-loading-and-dispatch) names the owning commands and required assertions.

## Source and artifacts

The integration receipts record base commit `a4545728256724deca98598d14bbaf4029a02dae` with additional worktree changes.
Their `source.json` files record source hashes, and their `source.patch` files retain the tracked changes.
Their `command.json` and `result.json` files record exact commands, exit codes, duration, and detected changes during execution.
The base commit alone does not identify the executed integration source.

The runtime remains unmodified wasmCloud 2.9.0 at `68ebece9c537f8bb4b5c9999f274ec68d60f35a9`, with Wasmtime 47.0.4.
The [authenticated log](production-authenticated-003/output.log) identifies the corrected six-scenario test binary as SHA-256 `212e742ee6b03b3457baa90243ad80693dea9eb072fb4c9b68d018f095c1521e`.
The [direct HTTP log](production-live-direct-003/output.log) identifies its integration binary, actual HTTP guest, fixture images, and runner.
Its integration binary is SHA-256 `7e94502d699c73c8593866950c5920d577845995f9c150c603c7a9225f115304`.
The unchanged HTTP guest is SHA-256 `dea43de0fbd76ea61df0fa153b81a2f72c4cc84e78e0382b97d7b073bd76b8ce`.
Commit `6feb01aca9c8fcced0c4ec9d3f5958416d2cf371` introduces the production substitution.
Commit `6f02d70d6b0a03b90160652df2d9c16c065010e3` corrects the WASI plugin declaration and supplies the deployed proof source.
Commit `07c7a858c4452579b12e0ae25a4e61af2107c4eb` corrects only the direct-consumer test inventory.
The final main sweep uses `07c7a858`, with unchanged production source from `6f02d70d`.
The final documentation and evidence commit adds no production changes.

## Production mechanism

The driver uses one native application for an immutable release and one for a complete candidate traversal.
Each application retains complete admitted component facts separately from the digest used by native compilation.
The loader checks every supplied byte buffer before native cache access.
Repeated export-only handlers remain valid, while imported operation interfaces require unique providers.

Native loading and dispatch replace the manual compiled cache, `PreparedCache`, linker preparation, `NodeInstance`, and deadline-to-epoch conversion.
The runtime inventory retires four guards for the removed manual store and epoch implementation.
It retains the upstream native store inventory, feature exclusions, and cross-component export-index fencing.
Every node call still receives a fresh store, and positive `poolSize` remains refused pending B2.

One absolute deadline covers acquisition, initialization, execution, and nested calls.
Host and executor select Tokio's public `event_interval(1)` for timer polling.
The actual guest call retains its application owner until native cancellation releases the work.
The existing authority guard revokes request scopes, and the application guard clears policy after failed or cancelled resolution.

Wasmtime schedules host callbacks outside the guest-call future's tracing scope.
The shared `invocation_trace` carrier stores the host span and subscriber under the existing invocation scope.
Native policy binds it after initialization and revokes it through the existing cleanup guard.
It exposes no guest interface, configuration, or authority.

Nested dispatch and HTTP, PostgreSQL, and blob callbacks restore that context before creating their existing spans.
The observer proof uses the same public helper, with two invocation spans and two host observations required.
Existing lifecycle proofs also require trace revocation, including cancellation and abandoned resolution.
The corrected authenticated proof passes its exact parentage assertions and all six scenarios.
The separate direct and nested HTTP proofs also pass after the callback restoration.
The deployed Receiving proof passes. The [final workspace result](#final-workspace-result) records the remaining baseline and fixture failures.

## Recorded integration attempts

These rows overlap and must not be added into one test total.
Counts exclude subprocess summaries when those summaries repeat the owning test.
Build-only commands provide no executed-test count.

| Evidence | Recorded result and scope |
| --- | --- |
| [Execution-host check](production-check-002/result.json) | Exit 0 after the initial integration compile fixes. |
| [Initial host tests](production-host-tests-001/output.log) | 52 pass, one failure, zero ignored, and one filtered armed test. The standalone trace fixture fails admission. |
| [Host tests after fixture correction](production-host-tests-002/output.log) | 52 pass, zero failures, zero ignored, and one filtered armed test. This precedes the shared tracing carrier. |
| [Earlier authenticated proof](production-authenticated-001/output.log) | One test passes with six authenticated scenarios and zero ignored. This precedes the trace assertion. |
| [Authenticated trace attempt](production-authenticated-002/output.log) | One test fails. Permission and fresh-only cases pass, but the success case exports one invocation span instead of two. |
| [Integration build 001](production-integration-build-001/result.json), [002](production-integration-build-002/result.json), [003](production-integration-build-003/result.json) | Each exits 0 with `--no-run`. These builds precede the shared tracing carrier. |
| [Tracing build 004](production-integration-build-004/output.log) | Exit 101. Callback capture and blob accessor type errors prevent execution. |
| [Corrected tracing build 005](production-integration-build-005/result.json) | Exit 0 with `--no-run` after the callback capture and accessor corrections. |
| [Corrected host tests](production-host-tests-003/output.log) | 52 pass, zero failures, zero ignored, and one filtered authenticated test. Existing lifecycle proofs require trace cleanup. |
| [Corrected authenticated proof](production-authenticated-003/output.log) | One pass, zero failures, and zero ignored. All six scenarios pass, including exactly two invocation spans and two host observations. |
| [HTTP guest build](production-http-guest-001/result.json) | Exit 0. The live logs identify the resulting guest bytes. |
| [Initial direct HTTP proof](production-live-direct-001/output.log) | One failure because native resolution does not account for the structural node-types import. |
| [Corrected direct HTTP proof](production-live-direct-002/output.log) | One pass, zero failures, and zero ignored after the native policy accounts for that structural import. |
| [Nested HTTP proof](production-live-nested-001/output.log) | One pass, zero failures, and zero ignored for child authorization and original-caller preservation. |
| [Direct HTTP after trace restoration](production-live-direct-003/output.log) | One pass, zero failures, and zero ignored in 8.61 seconds. |
| [Nested HTTP after trace restoration](production-live-nested-002/output.log) | One pass, zero failures, and zero ignored in 7.86 seconds. |
| [Runtime inventory](production-runtime-inventory-001/output.log) | 29 pass, zero failures, zero ignored, and 38 filtered. |
| [Focused Clippy attempt](production-clippy-001/output.log) | Exit 101. The executor binary test exceeds the compiler's query-depth limit while calculating its async future layout. |
| [Corrected focused Clippy](production-clippy-002/result.json) | Exit 0 after the compiler-limit correction and new tracing lint cleanup. Baseline warnings remain. |
| [Journey trace consumer](journey-trace-001/output.log) | The synthetic trace-consumer proof exits 0. This does not establish a deployed trace. |
| [Focused runtime tests](production-runtime-tests-001/output.log) | Stable exit 0. Library: 335 reported passes, zero failures, four ignored. Three explicit self-skips leave 332 executed passes. Native binding: two passes. Native deadline: five passes. Both native targets have zero failures and zero ignored. |
| [Scoped source checks](production-source-check-001/output.log) | 25 explicit Rust paths pass formatting with `skip_children=true`. Journey shell syntax and scoped `git diff --check` pass. |

## Failed attempts and corrections

The [first integration check](production-check-001/output.log) retains the obsolete `BoundNestedInvocation` reference and `Box<str>` conversion failures.
The second check records their correction.
These compile failures remain failed attempts.

The standalone trace fixture used an unregistered child dependency that the production manifest refused before dispatch.
The replacement runs its strict assertions inside the existing authenticated permitted-child case.
The [reconstructed fixture](production-host-tests-001/invalid-trace-fixture.reconstructed.rs.txt) is explicitly a reconstruction.
Its [provenance](production-host-tests-001/invalid-trace-fixture.reconstructed.provenance.json) records that the original hash was not captured and byte equality is unproved.

The authenticated trace attempt then exposed a real callback-context loss.
Instrumenting only `GuestCall` did not cover Wasmtime's separately scheduled host callbacks.
The shared carrier restores that context without adding authority or changing guest inputs.
The [source captured before this correction](production-authenticated-002/trace-source-before-fix/manifest.json) states its separate capture provenance.

Build 004 rejects borrowed `ActiveCtx` across the async callback boundary and a blob accessor passed as `SharedCtx`.
The correction moves the callback captures into their futures and uses the accessor's existing `ActiveCtx`.
Build 005 and the subsequent focused tests record the corrected result.

The authenticated trace retains incoming trace `41414141414141414141414141414141` and parent `1717171717171717`.
Its root invocation is `9f0f4f34fc73c26d`, and its child invocation is `50e45d4f1eb7f70d` with that root as parent.
The root host observation is `5edc42a3b1d24470` with the root invocation as parent.
The child host observation is `e2eb80ce9e6e69ee` with the child invocation as parent.
The log records `authenticated-native-trace result=pass invocations=2 host_observations=2`.

Focused Clippy then fails on executor compiler query depth 130 against the default limit of 128.
The correction adds `#![recursion_limit="256"]` to the executor crate for compiler layout calculation.
It changes no runtime deadline, recursion policy, or execution budget.
The tracing carrier also replaces its empty-set `Default` expressions with `HashSet::new()` to resolve its new lint warnings.
The [second Clippy run](production-clippy-002/result.json) exits 0 with unchanged source during execution.
The native helpers and `invocation_trace.rs` emit no warnings in that run. Baseline warnings remain.
Earlier successful source and artifact identities remain unchanged in their receipts.

## Final focused runtime result

The [runtime command](production-runtime-tests-001/command.json) selects the library and two native targets with `wasm_component_model_implements`.
Its [result](production-runtime-tests-001/result.json) records exit 0 and stable source.
The library reports 335 passes, zero failures, and four ignored tests.
Three reported passes explicitly skip because their live inputs are unset, leaving 332 executed passes.

The JetStream self-skips are `live_derived_publish_replay_converges_through_jetstream_dedup` and `live_publish_dedupe_bind_fetch_ack`, both without `WAMN_EVT_NATS_URL`.
The third is `live_scs_off_server_fails_checkout_closed`, without `WAMN_SCS_OFF_PG_URL`.
The one-pass subprocess summary repeats an owning library test and adds no pass.
The native binding and deadline targets pass two and five tests respectively, with zero failures and zero ignored.
The [artifact receipt](production-runtime-tests-001/artifact-identities.json) identifies all three binaries after execution and before another build.

The [source check](production-source-check-001/output.log) covers 25 explicit owned Rust paths with `skip_children=true`, shell syntax, and scoped whitespace errors.
An earlier recursive formatting attempt reached unchanged baseline formatting in `crates/platform/runtime/src/plugins/wamn_postgres/pool.rs:779`.
That file remains untouched. This report claims no whole-repository formatting result.
The deployed Receiving and final workspace results follow below.

## Initial Receiving correctness attempt

The [initial Receiving route log](production-receiving-001/production-route.log) records a failure at source `6feb01aca9c8fcced0c4ec9d3f5958416d2cf371`.
The exact test is `route_authentication_live::production_two_package_release_serves_all_thirteen_pat_routes`.
It reports zero passes, one failure, zero ignored, and 80 filtered tests in 25.21 seconds.
The first cold `acme-record-receipt` request returns HTTP 503 with `{"error":{"code":"execution-failed"}}`.

The [command receipt](production-receiving-001/commands.log) records both image builds before this prerequisite route test.
The [host image receipt](production-receiving-001/host-image.json) identifies `sha256:f7127cb04a65f272813a7b974571bf26a52d5a008b620c776d8b4c79a6b5c214`.
The [gates image receipt](production-receiving-001/gates-image.json) identifies `sha256:a7ad5332abf5f53ee79201328277f6faba9d339d54913df37527cfc7bec5bd09`.
Both image labels identify that same source commit and the `release` build profile.
The command-history suite and deployed host proof never ran because the prerequisite failed.

The generic response does not identify the underlying execution error.
The 25.21-second duration covers the complete test, including setup, and does not establish a request timeout.
The source-backed diagnosis found that the route harness captures tracing events without printing the complete execution error chain.
The [diagnostic replay and correction below](#receiving-diagnosis-and-corrected-local-replay) establish the cause.
The initial attempt remains failed.
This attempt provides no command-history or deployed-host correctness result.

The [cleanup receipt](production-receiving-001/cleanup.receipt) records `verdict=pass` for cluster `wamn-receiving-correctness` and its owned resources.
The command receipt records removal of containers `wamn-receiving-pg18-6feb01aca9c8-3473967`, `wamn-receiving-registry-6feb01aca9c8-3473967`, and `wamn-receiving-nats-6feb01aca9c8-3473967`, including their volumes.
It also records removal of image tags `wamn-host:receiving-6feb01aca9c8-3473967-release` and `wamn-gates:receiving-6feb01aca9c8-3473967-release`, plus the owned temporary directory.
This cleanup result applies to the failed attempt only.

## Receiving diagnosis and corrected local replay

The [diagnostic log](production-receiving-route-diagnostic-001/output.log) identifies an incorrect native plugin request during workload resolution.
`RouterDriver::load_application` copied every admitted component import into `host_interfaces`.
This requested plugin providers for engine-supplied `wasi:io/poll@0.2.12`, `wasi:clocks/monotonic-clock@0.2.12`, and `wasi:clocks/wall-clock@0.2.12`.
Native resolution refused those requests before executing the first operation.
The captured failure names plugin resolution, not a deadline.

The pinned upstream `wash-runtime/src/engine/mod.rs:743–775` adds WASI interfaces during native component initialization.
Its `engine/workload.rs:2512–2573` interprets `host_interfaces` as requests for host plugins.
The [production correction](production-integration-build-007/source.patch) removes the `facts.iter().flat_map(... fact.imports ...)` chain from `load_application`.
It retains the `NativePolicy` world, the unchanged admission checks, and native linker ownership.
The complete admitted import facts remain authority inputs. They are not a list of required plugin providers.

[Build 006](production-integration-build-006/result.json) adds only trace-error context to the route proof and exits 0 without running tests.
The [diagnostic source patch](production-receiving-route-diagnostic-001/source.patch) preserves that proof change against source `6feb01aca9c8fcced0c4ec9d3f5958416d2cf371`.
The diagnostic reports zero passes, one failure, zero ignored, and 80 filtered tests in 22.42 seconds.
Its [source and artifact receipt](production-receiving-route-diagnostic-001/source-before.json) identifies test binary SHA-256 `4d0d04bf2ddae63c98941ec427b8d739a756015a1a816c66106ed5f8ba9800b4`.
Its [stability receipt](production-receiving-route-diagnostic-001/source-stability.json) records unchanged source and artifact hashes during execution.
The [diagnostic recipe](production-receiving-route-diagnostic-001/recipe.sh) and [capture tool](production-receiving-route-diagnostic-001/capture.py) remain with the evidence.

[Build 007](production-integration-build-007/result.json) exits 0 with stable source after the production correction.
The [corrected local route log](production-receiving-route-002/output.log) reports one pass, zero failures, zero ignored, and 80 filtered tests in 22.52 seconds.
The same exact test serves all 13 PAT routes, including the nested operation, and passes all eight P3 protocol cases.
Those cases cover origin-form requests, early refusal of stalled bodies, body limits, transport errors, cancellation, and recovery.
This local proof does not replace the command-history or deployed-host gates.

The corrected [source-before receipt](production-receiving-route-002/source-before.json) and [source-after receipt](production-receiving-route-002/source-after.json) retain exact source and artifact hashes.
The [stability receipt](production-receiving-route-002/source-stability.json) records unchanged HEAD, source hashes, artifact hashes, and worktree status during execution.
The source remains commit `6feb01aca9c8fcced0c4ec9d3f5958416d2cf371` plus the [recorded patch](production-receiving-route-002/source.patch).
The executed driver source is SHA-256 `676cdde28e6114618f53d53a99e21c053437f4bd9b64da25b201d61eb397bbbb`.
The corrected test binary is SHA-256 `ab96ac12aeacf0cc1a42c4fe806ae841de821314c4b4bb5922a22cb45823118c`.
A later unused-import removal changes source identity without changing runtime behavior.
[Final-source Clippy](production-clippy-003/result.json) exits 0 in 69.01 seconds with unchanged source during execution.

Both cleanup receipts record `compose_down_exit_code: 0` and `scratch_removed: true`.
The [diagnostic cleanup](production-receiving-route-diagnostic-001/cleanup.json) names project `wamn-receiving-route-jwk0q3we` and scratch directory `/tmp/wamn-receiving-route.jwk0q3we`.
The [corrected cleanup](production-receiving-route-002/cleanup.json) names project `wamn-receiving-route-8azli5co` and scratch directory `/tmp/wamn-receiving-route.8azli5co`.
These receipts establish cleanup for the two local attempts only.

## Deployed Receiving correctness result

The [deployed journey receipt](production-receiving-002/receiving-correctness-journey.receipt) records a pass at source `6f02d70d6b0a03b90160652df2d9c16c065010e3`.
The [wrapper result](production-receiving-002-console/result.json) records exit 0 in 661.93 seconds with no source changes during execution.
Its [source receipt](production-receiving-002-console/source.json) and [command receipt](production-receiving-002-console/command.json) identify the executed source and exact gate invocation.
The [final workspace result](#final-workspace-result) records the completed sweep and its limits.

The prerequisite [route proof](production-receiving-002/production-route.log) passes one test with zero failures, zero ignored, and 80 filtered tests in 25.82 seconds.
It covers all 13 PAT routes, the nested operation, and eight P3 protocol cases.
The [deployed command-history proof](production-receiving-002/receiving-correctness.log) passes one test with zero failures, zero ignored, and five filtered tests in 1.54 seconds.
The [summary](production-receiving-002/receiving-correctness-summary.json) records 16 generated histories, three explicit histories, and seven boundary cases.
These are scenarios within the owning tests, not additional Rust test totals.

The [JSONL receipts](production-receiving-002/receiving-correctness.jsonl) contain 19 passing histories and 129 completed steps.
The steps comprise 72 `Receive`, 33 `Replay`, and 24 `Update` operations.
Each history's `completed_steps` equals its recorded `history.steps` length.
The histories and seven boundaries use 26 distinct fixture identifiers.
The corpus uses seed `7701`, PostgreSQL version `180006`, and SHA-256 `d826dbbb091eef13c1935ce6e4a1fcf0fecddb71fdb844302a6bed0abd0a0287`.

The seven passing boundaries are `invalid-status`, `mixed-envelope`, `competing-receipts`, `overlapping-replay`, `rollback-after-receipt-write`, `commit-withheld-application-response`, and `denied-then-authorized`.
The withheld-response receipt identifies its boundary as `after HTTP bytes, before application result delivery`.
The mixed envelope records two committed items and one refused item.
These receipts preserve the exact scope of the deployed correctness result.

The [component receipt](production-receiving-002/component-bytes.sha256) and image receipts identify the executed artifacts:

| Artifact | SHA-256 |
| --- | --- |
| HTTP guest bytes | `33e08d96ece969573bb2dd153b8617d1d0755700fef168372a0d8b88a90b7da8` |
| Receiving guest bytes | `a092149c1c8df8b4f74babf64122f9747d15d7b1a78b82b35cb6e496f98fd9a1` |
| Overlay guest bytes | `d69ad067860bbc9cccf0c843475b75211ac7e9ce6e7e05a9d3a50918753a66ec` |
| [Host image](production-receiving-002/host-image.json) | `16e587c47d91bcdde4a582daa4ac91f7492468737879aa2b590526ebc3e99f08` |
| [Gates image](production-receiving-002/gates-image.json) | `d570a32c46d039658c7a8bea69c99fe41bd97f464f9b03c0e4d04ce13c623f85` |

Both images carry the executed source label and the `release` build profile.
The [HTTP push receipt](production-receiving-002/flow-http-push.json) identifies OCI digest `sha256:d7b2520eba9b123228fccebe35e74e5de5a5c780d5d31ebe8b0a22a9615d82bb`.
The [deployment](production-receiving-002/flow-http-deployment.json) and [workload](production-receiving-002/flow-http-workload.json) both pin that digest.
The workload is `flow-http-7754874c55-5785457ff`, UID `61e39e1e-018a-48b9-a3d9-72d6103e2145`.
The [host deployment](production-receiving-002/host-deployment.json) pins release manifest `sha256:62de32341cfb39725f8e03adacb54d478e9421ae7f361384f6a86c9d8f1e829f`.

The [cleanup receipt](production-receiving-002/cleanup.receipt) records `verdict=pass` for cluster `wamn-receiving-correctness` and its exact owned resources.
The [commands](production-receiving-002/commands.log) record removal of containers `wamn-receiving-pg18-6f02d70d6b0a-3548194`, `wamn-receiving-registry-6f02d70d6b0a-3548194`, and `wamn-receiving-nats-6f02d70d6b0a-3548194`, including their volumes.
They also record removal of image tags `wamn-host:receiving-6f02d70d6b0a-3548194-release` and `wamn-gates:receiving-6f02d70d6b0a-3548194-release`, plus `/tmp/tmp.Wb1A3rWHQB`.
The failed Receiving 001 attempt remains failed and retains its separate image identities and cleanup receipt.

## Final workspace result

The [final main sweep](integrated-workspace-002/run.json) runs at `07c7a858c4452579b12e0ae25a4e61af2107c4eb` and exits 101 in 114.18 seconds.
Its [source receipt](integrated-workspace-002/source-stability.json) records no changes during execution.
The [results](integrated-workspace-002/workspace-results.json) report 2,245 test passes, six doctest passes, 84 failures, zero ignored tests, and two filtered schema regenerators.
The 85 explicit self-skips remain unchanged from the baseline.
Subtracting those skips leaves 2,166 reported passes, including doctests, but does not establish an exact executed count.

The [comparison](integrated-workspace-002/workspace-comparison.json) uses the latest nested-authority baseline at `dd28c68cadf825fe9eec44f7ae2e24ab28b89442`.
The [classification](workspace-comparison-002/classification.json) retains the exact causes and command provenance.
Eighty-two failure identities and causes match exactly.
One existing WIT-parser failure differs only in eight checkout path prefixes.
The only added failure requires `WAMN_NATIVE_B_AUTH_PG_URL`, which the unarmed sweep deliberately omits.
No test target or explicit self-skip identity changes.

The [first sweep](integrated-workspace-001/workspace-results.json) reports two additional inventory failures.
Its [classification](workspace-comparison-001/classification.json) retains those introduced failures as failures.
Commit `07c7a858` removes the obsolete direct `wasmtime-wasi` consumer row without weakening the source-universe assertions.
The [focused inventory proof](production-source-identity-001/output.log) passes all three tests.
The final sweep also passes all three inventory tests.

The [separate armed proof](production-authenticated-main-001/output.log) uses the main binary built from production source `6f02d70d6b0a03b90160652df2d9c16c065010e3`.
Its SHA-256 is `3d72e55a7f6f756e21d2d701e164e69e2d7c8355e318098178a471d68e0d52bb`.
It passes one test, with zero failures, zero ignored tests, and 52 filtered tests in 2.41 seconds.
All six cases pass: permission refusal, fresh-only refusal, success, initialization deadline, execution deadline, and cancellation.
The trace contains exactly two invocation spans and two host observations with the required parent relationships.
Its [command](production-authenticated-main-001/command.json) and [result](production-authenticated-main-001/result.json) retain exact invocation and unchanged source.
The log records removal of its exact owned PostgreSQL container.
This separately armed result does not turn the unarmed workspace sweep into a passing run.

## Removed code and remaining deviations

The [final code inventory](production-code-inventory-002/inventory.json) compares published base `79879412` with tested source `07c7a858`.
Its counts include implementation, tests, and documentation. They do not measure production-only code size.
Native loading and dispatch replace the manual compiled cache, prepared cache, component linker, node instances, and store lifecycle.
The same landing removes the obsolete manual epoch and store guards, direct WASI dependency, and replaced trace expectations.

WAMN retains invocation authority, exact component provenance, statement authorization, wiring, truthful outcomes, and the enclosing deadline.
Request authority ends with each invocation. Every call still uses a fresh store.
Positive `poolSize` remains refused until B2 proves reuse, isolation, overflow, and retirement with `maxConcurrency = 1`.
C remains a separate unfinished substitution. D retains resolved-IP enforcement, and F retains the exclusive PostgreSQL session implementation.
Issues `.74` through `.76` remain open under their existing scope.

Both compared sources use unmodified wasmCloud 2.9 at `68ebece9c537f8bb4b5c9999f274ec68d60f35a9` and Wasmtime 47.0.4.
The final comparison records correctness against that identified baseline.
No benchmark runs for this landing, and this report claims no performance improvement.
The owner excludes a benchmark prerequisite. No additional performance gate blocks B.

## Cleanup and remaining evidence

The [earlier authenticated log](production-authenticated-001/output.log) records removal of its exact owned PostgreSQL container.
The [failed authenticated log](production-authenticated-002/output.log) also records its owned container removal.
The [corrected authenticated log](production-authenticated-003/output.log) records removal of container `e3e062b2920c2d483f3516201478912e6e2cc5913bc5b114d9df525e78e8e706`.
The subsequent [direct](production-live-direct-003/output.log) and [nested](production-live-nested-002/output.log) logs record removal of their owned PostgreSQL and registry containers plus anonymous volumes.
These receipts establish cleanup for those attempts only.

The deployed Receiving proof passes. The [final workspace result](#final-workspace-result) records the remaining baseline and fixture failures.
The full workspace sweep runs on main after source integration, as the owning recipe requires.
The linked receipts retain commands, counts, source and artifact identities, cleanup, remaining deviations, and the identified 2.9 correctness comparison.
No owned B build or live fixture remains active.
Worktree cleanup follows publication of both Git and Dolt records. Publication status lives in Beads and Git.

> **Stop condition:** a required context, candidate or loading boundary cannot be represented through public APIs. Record the exact obstacle before expanding the adapter. Do not copy internals or preserve duplicate machinery merely to report native adoption.
