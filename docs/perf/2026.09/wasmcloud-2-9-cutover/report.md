# wasmCloud 2.9 cutover evidence

Stage 1 records the pre-change source under `wamn-0h0g.2.7.1`.
The source is WAMN `dfa1c3187fe8cd671688a442b23106046e502cb6`.
This increment contains source observations and retained test results.
No build, test, journey, workload, or performance measurement ran for this capture.
The cutover does not yet carry a release-readiness claim.

The opening sections retain the baseline capture. [Prepared integration](#prepared-integration) records the later source and validation evidence.

## Source and dependency observations

The [input index](baseline-001/input-index.json) identifies the captured bytes with SHA-256, a content hash.
All 14 committed inputs matched the worktree before the parent began dependency edits.
The snapshots retain the root, both standalone probes, both guest workspaces, the Rust toolchain, and Cargo configuration.
The [source record](baseline-001/source-observations.json) lists commits since the charter reviewed WAMN at `2a4cd288`.

The charter is absent from commit `dfa1c318` and exists as an untracked worktree file.
Its captured 25,578 bytes carry SHA-256 `d6898fb7740eb2d02c28c69e6ee0d2b0b11d633d65d80c21db091c79f76637dc`.
The evidence preserves those bytes without assigning them to the source commit.

The [dependency inventory](baseline-001/dependency-inventory.json) parses 71 committed manifests with Python `tomllib`.
Root and probe lockfiles resolve `wash-runtime` 2.8.0 from fork commit `735b57982545358409a7d965a22549b08487ca09`.
Each resolves `wasmtime`, `wasmtime-wasi`, and `wasmtime-wasi-http` 47.0.4 from crates.io.
Root requirements still name 47.0.3, while the probe requirements pin `=47.0.4`.
Neither guest lockfile contains these runtime packages.

The root disables `wash-runtime` defaults and selects `oci`.
Host adds `washlet`, `wasi-config`, `wasi-otel`, and `wasm_component_model_implements`.
Executor adds `washlet` and `wasm_component_model_implements`.
CLI and integration proofs add `washlet`.
The root Wasmtime declaration disables defaults and preserves `cache` and `parallel-compilation`.
The inventory includes normal, development, target-specific, and workspace declarations without inferring a resolved feature tree.

Cargo did not run in this evidence lane.
Effective feature trees for normal service builds and test selections remain unexecuted.
Parent-owned stage 1 must obtain those trees for the root, both guest workspaces, and both standalone probes.
Manifest declarations alone do not establish the compiled capability set.

The upstream checkout has no tracked changes or untracked files at capture.
Its origin is `https://github.com/wasmCloud/wasmCloud`.
Both HEAD and peeled `v2.9.0` equal `68ebece9c537f8bb4b5c9999f274ec68d60f35a9`.
The evidence retains hashes and snapshots of the upstream root manifest, runtime manifest, and operator chart metadata.
These observations do not prove that WAMN compiles against upstream.

## Toolchain, profiles, and deployed identity

The [tool record](baseline-001/tool-observations.json) records Rust 1.98.0, commit `88d9e12ae178fab0fb5cc050a94da85685d449ea`, on `x86_64-unknown-linux-gnu`.
The toolchain selects `wasm32-wasip2`, Clippy, and rustfmt.
Cargo configuration selects four jobs, Clang, and mold.
LTO combines optimization across compiled modules.
Root development builds use line-table debug information and unpacked debug files with LTO disabled.
Root release builds use thin LTO, while both guest release profiles select size optimization and stripping.

The selected `wash` is `/usr/local/bin/wash`, version 0.40.0.
Its version output also names wasmCloud 1.6.1, NATS 2.10.20, and wadm 0.20.2.
This identifies the command in this session, not every installed binary or publication caller.
The [command record](baseline-001/commands.json) retains the source search for `wash` callers and each version command.
The parent owns the complete tool and publication migration.

The [declared image inputs](baseline-001/declared-image-inputs.json) retain source hashes and selected image lines.
Source recipes name operator chart 2.8.0 and the default host image `wamn-host:dev`.
They do not identify the images deployed on a cluster.

The [deployment read record](baseline-001/deployed-identities.json) contains four read-only commands and their exit codes.
`kubectl config current-context` exits 1 with `error: current-context is not set`.
Helm and selected workload reads exit 1 because `localhost:8080` refuses the connection.
The earlier sandbox attempts also exit 1 with a socket permission error.
No command changes a cluster resource, and no deployed image digest or installed chart identity is claimed.

## Retained test evidence

The [prior result inventory](baseline-001/prior-results.json) preserves exact failure names, commands, source references, log hashes, and exit codes.
These tests ran in earlier lanes.
This capture parses retained evidence and does not rerun it.
Counts describe each recorded source, not the complete `dfa1c318` tree or the future 2.9 tree.

The [identity boundary report](../ctc8-15-4-fresh-only/report.md) describes the combined source at `22384f96`.
Its final command exits 0 with 543 passes, zero failures, and 28 ignored tests across 33 summaries.
The command covers catalog, generator, CLI, and execution-host packages.
It is neither a full workspace sweep nor a deployed gate.

```sh
cargo test -p wamn-catalog -p wamn-schema-generator -p wamn-ctl -p wamn-execution-host --lib --tests --locked --offline --no-fail-fast
```

The first combined command exits 101 with two named failures.
`client_ir::tests::fresh_only_contract_policy_is_preserved_in_the_client_ir` lacked the required `kind` field, which commit `22384f96` supplies.
`dev::operator::tests::launch_supplies_exact_bound_facts_without_command_arguments` failed with `Text file busy`.
The unchanged launch test passes in the second run, but `wamn-10yt.62.9` still owns its unresolved cause.

The [generated TUI report](../generated-tui-integration/report.md) records a workspace sweep at `a0f90cda`.
It exits 101 with 2,074 reported passes, 68 failures, and six doctest passes.
The comparison classifies 65 failures as missing live or artifact inputs and one as unavailable Kubernetes discovery.
At least 85 reported passes explicitly skip their live work.
The exact command includes ignored tests and excludes only the two documented regeneration commands.

```sh
cargo test --workspace --locked --offline --no-fail-fast -- --include-ignored --nocapture --test-threads=1 --skip regenerate_checked_in_journey_schema --skip regenerate_checked_in_dev_config_schema
```

`every_mounted_secret_is_declared_here_or_named_a_prerequisite` is the known code failure under `wamn-362o.58`.
`receiving_pat_overlay_renders_a_complete_scoped_host` is the unavailable Kubernetes discovery failure.
`materialize::tests::wrapper_and_split_paths_are_identical_without_reintrospection` is the comparison fixture failure that `b57c8945` repairs.
The repaired fixture passes its focused test.
The report separately records successful disposable operator launch and restart at `b57c8945`, without an in-cluster gate.

The [build guide](../../../operations/build-and-test.md) retains the separate guest digest defect under `wamn-10yt.61`.
Its named test is `one_commit_built_under_two_profiles_yields_identical_guest_digests`.
The guide describes different digests under `m1` and `proof` with separately stated measurements.
The TUI sweep cannot establish that behavior because it lacks this test's required artifact inputs.
This capture does not treat the unrelated defect as a 2.9 regression or rerun its builds.

## Proof limits

The [unexecuted proof record](baseline-001/unexecuted-proofs.json) maps required evidence to the owning recipes and Beads.
The identity boundary leaves live fresh-PAT versus session refusal proofs open under `wamn-ctc8.15.4`.
Those proofs also cover authority removal and preservation of earlier committed work after a later refusal.
Production routes remain PAT-only, and the unfinished proof branch supplies no cutover acceptance evidence.

Receiving composition parity remains under `wamn-ctc8.15.5`, and WMS response proof remains under `wamn-b2m6.10`.
The generated operator recipe remains under `wamn-10yt.62.8`.
The cutover parent owns new memory, lifecycle, routing, deployment, application, authority, telemetry, and performance evidence.
No 2.9 application result, performance comparison, or adjusted gate limit exists in this increment.
The greenfield deployment requires no maintenance window because no running clients need a transition.

## Prepared integration

The root and both standalone HTTP probes now resolve direct upstream wasmCloud 2.9.0 at `68ebece9c537f8bb4b5c9999f274ec68d60f35a9`.
The Wasmtime requirements move from 47.0.3 to 47.0.4, while resolved Wasmtime packages remain at 47.0.4.
No WAMN source patch, override, or virtualizer change supplies this runtime.

The initial [workspace compilation](build-001/cargo-check.log) exits 101 at two integration sites.
Local activation lacks the new `reclaim_window_seconds` and `reclaim_min_instances` fields.
Host startup lacks the new `NatsConnectionOptions.connect_retry` field.
Zero reclaim values preserve fresh stores, and `connect_retry: None` initially preserves single-attempt connection behavior.
The [repeated compilation](build-001/cargo-check-after-api-fix.log) exits 0 after those API edits.
It runs `cargo check --workspace --all-targets --locked` and finishes in 15.34 seconds.

That early compilation precedes the memory, lifecycle, and expected-host adapter edits.
The later [combined check](build-003/exit-code.txt) exits 0 with the same workspace and all-targets command.
It includes those stage 2 and stage 3 changes.
The native chart startup flag selects its bounded NATS retry window explicitly.

The [feature commands](source-001/commands.json) record normal host/executor, conformance/integration, probe, and guest selections.
WAMN keeps runtime defaults disabled and preserves its selected provider features.
The native Wasmtime dependency enables additional features, including `gc-copying`, `gc-drc`, backtraces, and parallel compilation.
The direct WAMN declaration still selects only its reviewed `cache` and `parallel-compilation` features.

Two initial probe observations fail because offline Cargo lacks the newly selected `crossbeam-deque` 0.8.8 package.
The probe now uses 0.8.7, which the root and other probe already lock.
Both [repeated observations](source-001/aligned-probe-commands.json) exit 0.
No unrelated package update supplies that correction.

The [host build](validation-001/host-build-exit-code.txt) and [executor build](validation-001/executor-build-exit-code.txt) both exit 0.
The [native HTTP probe check](validation-001/native-http-probe-exit-code.txt) and [WASI HTTP probe check](validation-001/wasi-http-probe-exit-code.txt) also exit 0.
All three [chart argument checks](validation-001/host-arguments.json) pass for default, Receiving, and WMS profiles.
Each check deliberately exits 1 after parsing, before observability or service startup, and loads no secret environment.
These checks establish argument compatibility, not successful service startup.

The [workspace sweep](validation-001/workspace-results.json) exits 101 with 2,114 reported test passes, six doctest passes, and 67 failures across 32 targets.
It includes ignored tests, reports zero ignored tests, and filters only the two documented regeneration commands.
All 67 failure names match retained baseline failures: 65 missing inputs, unavailable Kubernetes discovery, and the known undeclared-Secret code failure.
At least 85 reported passes explicitly skip their live or artifact proof.
After those exclusions, 2,035 reported passes remain including doctests, but this is not an exact executed-proof count.

Its [log](validation-001/workspace-sweep.log) records passing Count accounting, memory cleanup, expected-host routing, lifecycle, ingress saturation, and local development shutdown tests.
Those named results do not establish a passing workspace sweep.
The [Clippy command](validation-002/command.txt) covers the workspace and all targets, and [exits 0](validation-002/exit-code.txt) with warnings.
After two lifecycle lint cleanups, all four [focused lifecycle tests](validation-003/lifecycle-tests.log) pass.
The [scoped runtime Clippy](validation-003/runtime-clippy-exit-code.txt) also exits 0 with warnings.
The final lifecycle module emits neither of its two corrected warnings.

The [CLI record](source-001/wash-cli.json) identifies the downloaded upstream wash 2.9.0 Linux x86_64 binary.
Its SHA-256 is `590130b23d897e80ba948c15cfd317e946e1b22b73b6d534af57dfec1e4f4bf6`.
The installer checks the bytes before installation or reuse.
The recorded [push help](source-001/wash-push-help.txt) confirms the native `oci push` command.
A successful publication must return both `.success` and `.data.success`, with the digest under `.data.digest`.

Publishers select their existing private credential files through `DOCKER_CONFIG`.
Upstream `oci.rs` uses `docker_credential` 1.4.0, which reads `config.json` and accepts the existing username/password fields.
This replaces the unsupported legacy credential environment variables without passing passwords in command arguments.
The [first deployed run](live-receiving-001/journey/flow-http-push.json) later records successful authenticated publication through this path.

The first [upstream gate](upstream-gate-001/release-check.log) exits 101 because generated Wasm fixtures are absent.
Its formatter passes, and all four isolated Git-template tests pass.
That attempt cannot compile its fixture-dependent runtime targets and supplies no runtime-suite pass.
The gate now invokes upstream `cargo xtask build-fixtures` through its locked direct Cargo equivalent before the runtime tests.
The second attempt retains its separate log under `upstream-gate-002`.

The third [upstream gate](upstream-gate-003/results.json) exits 0 across all six legs, including generated fixtures, runtime tests, and isolated Git fixtures.
It selects upstream default features separately from WAMN production features and includes ignored tests.
Runtime harnesses report 1,050 passes, including one explicit self-skip, leaving 1,049 executed pass results.
`test_nats_blobstore_large_payload` skips its 2 GiB round trip because `NATS_LARGE_PAYLOAD_TESTS=1` is not armed.
The four Git template fixture passes are separate from those runtime results.
Inactive feature targets supply no execution proof, and this gate supplies no deployed WAMN proof.

## Deployment and local supervision

The [distributed chart render](deployment-001/observations.json) covers chart 2.9.0 for default, Receiving, and WMS profiles.
The [CRD capture](deployment-crds-001/crd-inventory.json) records all five distributed CustomResourceDefinitions, which define Kubernetes resource schemas.
Their files match upstream source at `68ebece9c537f8bb4b5c9999f274ec68d60f35a9` byte-for-byte.
The [chart identity](deployment-crds-001/distributed-chart-identities.json) matches the earlier render's OCI digest, `sha256:d70b240cfc3c745f306fc6ebecebff4370e6c8c0568b55c1d02b1eda1716fd17`.

The [schema comparison](deployment-crds-001/schema-changes.json) leaves Host and Artifact unchanged from distributed 2.8.
Workload, WorkloadDeployment, and WorkloadReplicaSet each add `reclaimMinInstances` and `reclaimWindowSeconds`.
The [cutover procedure](../../../../deploy/README.md#wasmcloud-29-cutover) applies pinned CRDs before the operator upgrade because Helm skips existing CRDs.
It captures command exits and installed schemas, waits for Established, and stops on ownership conflicts without `--force-conflicts`.
No CRD apply or installed-schema proof ran in this capture, and the gateway remains disabled.

The host deployment allows 70 seconds for a process bound of 61 seconds.
The local development supervisor now allows 70 seconds after SIGTERM and a separate five seconds after forced kill.
Its child clears ambient WASH overrides, so the default 56-second local host envelope fits that grace.
The preceding workload-stop request retains its separate 30-second bound.
The [workspace test log](validation-001/workspace-sweep.log) records the simulated 56-second exit and short reap-deadline tests passing.

## Expected host inventory and routing limits

This inventory traces the publication writers and retained Receiving evidence.
It does not execute publication, load a current release, or run a live guest.
No retained canonical ServingManifest bytes or WMS deployment snapshot were found in the searched repository evidence and local lane artifact paths.
The WMS entries below are source-derived facts, not observations from a deployed release.

| Journey | Explicit release host | HTTP attachment scope | Native operator Service aliases |
| --- | --- | --- | --- |
| Receiving | `receiving.localhost` | Eight `wamn_receiving` attachments and five `client_acme_receiving` attachments | `flow-http.wamn-receiving-journey`, `flow-http.wamn-receiving-journey.svc` |
| WMS | `wms.localhost` | Seven `wamn_wms` attachments | `flow-http.wamn-wms-journey`, `flow-http.wamn-wms-journey.svc` |

The [Receiving runner](../../../../tools/receiving-cluster-journey-run) sets `route_host=receiving.localhost` at line 171.
Its [production publisher](../../../../tests/integration/src/route_authentication_live.rs) passes that value to `PublishReleaseArgs` at line 2761, pulls the verified artifact and loads `ReleaseManifestWeld` at lines 2824–2832, then checks all 13 attachment hosts at lines 2924–2952.
The [WMS runner](../../../../tools/wms-cluster-journey-run) sets `route_host=wms.localhost` at line 183 and passes `--route-host` to `publish-release` at line 1114.
The [Receiving](../../../../packages/receiving/publication/attachments.json), [Acme overlay](../../../../packages/client_acme_receiving/publication/attachments.json), and [WMS](../../../../packages/wms/publication/attachments.json) attachment documents omit hostnames by design.
`resolve_route_host_overlay` in [the publication writer](../../../../services/ctl/src/publish_release.rs), lines 1296–1372, requires a deployment host for HTTP or Studio attachments, lowercases it, inserts `route.host`, and recalculates each definition hash.
It rejects package-authored `route.host`; neither current journey selects the permitted `*` value.

The [retained Receiving WorkloadDeployment](../ctc8-12-fresh-auth/after-001/journey/flow-http-deployment.json), line 41, and [Workload](../ctc8-12-fresh-auth/after-001/journey/flow-http-workload.json), line 49, both name `receiving.localhost`.
The [retained session fixture](../ctc8-15-3-host-sessions/deployed-007/session-host-fixture-public.json), lines 32–33, names that host and `/purchase_order/get`.
These records support the Receiving host identity without supplying the full canonical release bytes for a new verification.

Both runners pass the same `route_host` to [the workload renderer](../../../../tools/journey-workload.sh), which inserts `hostInterfaces[].config.host` at lines 106–110.
Upstream 2.9's [operator alias writer](https://github.com/wasmCloud/wasmCloud/blob/68ebece9c537f8bb4b5c9999f274ec68d60f35a9/runtime-operator/internal/controller/runtime/workload_controller.go#L244-L264) separately derives only `service.namespace` and `service.namespace.svc`.
Its placement path injects those aliases into the runtime request at lines 345–346.
It does not derive a bare Service name or a `.svc.cluster.local` alias there.
The expected-host adapter does not add any of these aliases to the verified release projection or expand `*`.

Before the first binding, or after the last binding is removed, requests with the exact release Host receive 503 when the native router returns `NoWorkloadForHost`.
The [native router](https://github.com/wasmCloud/wasmCloud/blob/68ebece9c537f8bb4b5c9999f274ec68d60f35a9/crates/wash-runtime/src/host/http.rs#L287-L305) removes the request port before returning that hostname, so `receiving.localhost:8080` and `wms.localhost:8080` are also covered.
Current journey requests already set the explicit Host while connecting through Service DNS or a NodePort: Receiving runner lines 2564, 2635, and 2706; WMS runner lines 2053 and 2148.
Unbound Service aliases, other unknown hosts, case variants that fail native exact lookup, and wildcard-only release routes retain native 404.
A missing Host retains native 400, and a selected workload with no native ingress handle retains native 404.
Once a workload binds, the adapter preserves its application responses, including application route-not-found 404.

The adapter's tests pass in the [workspace log](validation-001/workspace-sweep.log), including bind transitions, application responses, refusals, and the missing-handle residual.
The combined source compiles, while the full workspace sweep still fails.
Current release publication, compiled-guest responses, and measured live restart behavior remain separate proof requirements.
The [final source record](validation-004/source-inputs.json) identifies an uncommitted tree based on `dfa1c318`.
Stage 3 has not landed, and live proof requires a clean, fixed commit.
No current result establishes cutover or release readiness.


## First deployed 2.9 proof

The prepared integration commits as `15f2d4da26df0bdb46022cfd5276c9550d415d86` on the isolated cutover branch.
Commit `7798190c7d045a48231a8db41448cc6e29cf3bd8` records the reviewed journey template hashes.
Main remains at `dfa1c318`, and the stage 3 fence remains held.

The [full Receiving command](live-receiving-001/command.txt) runs from clean source `7798190c` and [exits 0](live-receiving-001/exit-code.txt).
It starts at 14:30:21 UTC and finishes at 14:49:25 UTC on September 9, 2026.
The [verdict](live-receiving-001/journey/verdict.json) records production publication, three ready rebuilt hosts, native scheduling, Receiving causation, and exact materializer acknowledgment.
The [production route log](live-receiving-001/journey/production-route.log) records the real 13-route test passing without a skip.
The selected materializer test also passes without a skip.
Both native publication receipts report success with immutable component digests.
The release manifest is `sha256:e633957452b28418875cdc69a5574d078da95daf94aa9fda76c5ad84cc3dee94`.

The [Warning Event](live-receiving-001/journey/cross-environment-event.json) uses `events.k8s.io/v1` and reason `CrossEnvironmentSchedulingDenied`.
The [permission receipt](live-receiving-001/journey/operator-events-rbac.receipt) grants only `create` and `patch` for modern Events.
The distributed chart still needs the scoped WAMN overlay.
The [cleanup receipt](live-receiving-001/journey/cleanup.receipt) passes, and only the three frozen `kind-wamn` containers remain afterward.

This run cites historical telemetry evidence in its verdict.
It does not establish current invocation traces, effect traces, or metric delivery.
Operator recovery, startup bursts, idle executor cases, and comparable performance measurements remain open.
The prepared Receiving helpers add current telemetry, startup, and operator checks to the full journey.
Their [recipe and scope](../../../operations/build-and-test.md#receiving-cluster-journey--released-flow-http-scheduling-and-reachability) require a new clean-source run. None records a live pass yet.
The existing `--measure-startup` arm remains a separate, unexecuted 2.9 measurement.

## Rebuilt host process proof

The [host process test](host-lifecycle-live-001/test.log) passes once at clean source [`9d54245c`](host-lifecycle-live-001/source.json), with no ignored or filtered cases.
It runs the real rebuilt host and an owned NATS server without a guest workload, PostgreSQL, registry, or operator.
The [SIGTERM receipt](host-lifecycle-live-001/host/host-term.receipt) records successful exit in 5,012 ms. [SIGINT](host-lifecycle-live-001/host/host-int.receipt) takes 5,004 ms, both within 70 seconds.
Both cases prove `/livez` stays healthy during real ingress saturation while `/readyz` reports saturation, then recovers after the held connection closes.
They also observe draining readiness before successful exit.
The SIGTERM case keeps the same host alive through a 50-second NATS outage, then receives its native heartbeat RPC after reconnection.

The [blocked exporter receipt](host-lifecycle-live-001/host/host-blocked-flush.receipt) records exit code 1 in 7,008 ms with the bounded flush error.
The test observes the actual trace exporter connection and its [HTTP/2 preface](host-lifecycle-live-001/host/trace-peer-preface.bin), then leaves that connection unanswered.
This proves an exercised exporter failure, not successful telemetry delivery.
Neither normal signal case observes the initial `starting` probe response.
The run does not prove failure before the first native beat, unexpected native loop/listener termination in a WAMN process, active guest drain, or operator recovery.
The [NATS extraction record](host-lifecycle-preflight-001/extraction.json) retains the pinned executable's provenance.

## Completed 2.8 comparison prebuilds

The [first prebuild](baseline-build-001/run.sh) and [second prebuild](baseline-build-002/source.txt) use isolated source `dfa1c318`, with empty status records before and after.
All recorded build commands exit 0: `tools/build-components m1`, the release host, debug CLI/CDC reader/scenario worker/gates, the integration library test binary with `--no-run`, and `wamn-throughput`.
The [first exit](baseline-build-001/exit-code.txt), [second exit](baseline-build-002/exit-code.txt), and [host feature capture](baseline-build-002/features-exit-code.txt) all report 0.
These receipts complete compilation preparation. No baseline journey or performance measurement ran in them.
The [comparison plan](performance-plan-001/comparison.json) still requires source-matched live measurements before a 2.8-to-2.9 performance conclusion.


## WMS receipt correction

The WMS runner copied five Receiving causation and acknowledgment claims into its final receipt.
Its actual materializer proof establishes ready placement and absence of captured refusal or failure logs.
The [correction](wms-evidence-labels-001/wms-evidence-labels.patch) removes those unsupported claims under `wamn-b2m6.11`.
It names the actual composed move, released operations, winning label object, and idle materializer scope.
The [acceptance map](wms-evidence-labels-001/acceptance-map.json) cites the existing assertions for each replacement.
Shell syntax passes, and no new WMS live result is claimed.


## Executor project binding

The [second full Receiving run](live-receiving-002/exit-code.txt) exits 1 at clean source `c28f651b`.
Its [production route test](live-receiving-002/journey/production-route.log) passes once, with no ignored tests and 65 other cases filtered.
Both native OCI publications succeed, and the [owned cleanup receipt](live-receiving-002/journey/cleanup.receipt) passes.
The [artifact comparison](live-receiving-002/analysis.json) records four unchanged guest artifacts despite the WIT comment edits.

The real executor reports `release_closure` throughout the existing 45-second startup allowance.
Its [log](live-receiving-002/journey/executor-SIGTERM.log) repeatedly reports missing `ExecutorPlatform` credentials for project `receiving`.
The executor registers its credentials under `default`, then requests them under its declared project.
The same composition exists at baseline `dfa1c318`.
Bead `wamn-0h0g.2.7.6` owns this defect.

The fix uses the existing `from_env_for_project` method that the host already uses.
It preserves each authority class and does not add a default-project alias.
All 12 [executor unit tests](executor-project-fix-001/executor-tests.log) pass.
The [existing project tests](executor-project-fix-001/declared-project-tests.log) and [duplicate-source refusal](executor-project-fix-001/duplicate-source-test.log) also pass.
Scoped [Clippy](executor-project-fix-001/clippy-exit-code.txt) exits 0 with warnings.
The real executor signal cases still require a new live run.
The failed run does not reach current telemetry, startup bursts, installed CRDs, or operator recovery.


## Current telemetry and startup protocol

The [third full Receiving run](live-receiving-003/exit-code.txt) exits 1 at clean source `a40d1c18`.
The [executor receipt](live-receiving-003/journey/executor-lifecycle.json) passes both real signal cases and closes `wamn-0h0g.2.7.6`.
Both probes return exact `200` with `ok\n`.
SIGTERM and SIGINT exits succeed in 15.4 ms and 31.5 ms after readiness, without forced termination.
These are idle executor cases, with no queued-delivery drain claim.
The production route and exact materializer acknowledgment tests also pass.

The [current telemetry receipt](live-receiving-003/journey/telemetry/receipt.json) passes for two real PAT requests.
Both traces contain completed WAMN invocations and descendant PostgreSQL effects with the expected identity.
The collector exposes native HTTP duration counts plus WAMN PostgreSQL and JetStream histogram counts.
These metrics do not measure guest CPU or identify individual requests.
HTTP egress injection and retired private runtime phases remain outside this proof.

The [startup test](live-receiving-003/journey/startup-burst/test.log) passes once, with no ignored or filtered cases.
Its [protocol receipt](live-receiving-003/journey/startup-burst/protocol.json) records eight cold starts and eight warm starts with native control, probe, and application observations.
Every owned workload stops, the final workload count is zero, and the host exits successfully.
The helper then fails before it can establish native start overlap from all server traces.
The [Tempo search response](live-receiving-003/journey/startup-burst/trace-search-008.json) contains two 31-digit trace IDs among its 16 results.
The helper incorrectly requires 32 digits, which `wamn-0h0g.2.7.7` owns.

The fix accepts nonzero hexadecimal search IDs up to 128 bits and restores leading zeros for lookup and comparison.
It also checks that each returned span belongs to the requested trace.
Current [Tempo documentation](https://grafana.com/docs/tempo/latest/configuration/) describes optional leading-zero padding in search responses.
The [offline replay](startup-trace-fix-001/replay.json) accepts all 16 retained IDs and checks 42 retained span identities.
It rejects ten malformed or invalid inputs.
A new live run must still establish complete startup trace exposure and operator recovery.
The [owned cleanup receipt](live-receiving-003/journey/cleanup.receipt) passes.


## Completed startup exposure and operator reader failure

The [fourth full Receiving run](live-receiving-004/exit-code.txt) exits 1 at clean source `802aed06`.
The production route, exact materializer acknowledgment, real idle executor signal cases, and current telemetry proofs pass again.
The [complete startup receipt](live-receiving-004/journey/startup-burst/result.json) passes with all 16 exact native start traces.
Each herd contains eight overlapping handlers against the configured start limit of four.
Four cold observations and two warm observations finish inside continuous native-start intervals.
The observations cover native control, live and ready probes, and the expected application response.
Before the cold route binds, the application returns an empty `503`.
During warm starts, the existing workloads keep the read-only application request successful.

The cold herd reaches all-running state in 232.3 ms, and the warm herd takes 114.9 ms.
The host becomes ready 656.3 ms after process start.
The [protocol receipt](live-receiving-004/journey/startup-burst/protocol.json) records zero cleanup errors and successful host exit in 196.7 ms without forced termination.
These local timings do not use the Kubernetes six-CPU quota or the production five-second shutdown delay.
The replicas share one native HTTP digest and its compile deduplication.
Native start spans begin before permit acquisition, so overlap measures queued demand rather than active permits or CPU use.
This live evidence closes `wamn-0h0g.2.7.7` on its fix commit, `802aed06`.

The operator helper then fails before its installed-schema assertions or any deliberate disruption.
Its [captured kubectl output](live-receiving-004/journey/operator-recovery/0003-distributed-crds-client-decode.stdout) contains five consecutive JSON objects.
The helper incorrectly expects one JSON List.
The correction under `wamn-0h0g.2.7.8` decodes every object and preserves the exact five-name inventory and all schema hashes.
The [offline replay](operator-json-fix-001/offline-replay.json) reproduces the original error, accepts all five real objects, and rejects five incomplete or malformed controls.
Installed CRD identity and operator recovery still require a new live run.
The [owned cleanup receipt](live-receiving-004/journey/cleanup.receipt) passes.


## Armed PostgreSQL authority proofs

The [authority summary](authority-live-001/summary.json) records 40 passes and three failures across 43 tests at clean source `802aed06`.
All 16 cases use separate fresh PostgreSQL 18 containers with explicit database inputs.
Every owned container cleanup succeeds, and the source stays clean.
All seven runtime claims tests, SQLx transaction isolation, tenant-key tests, credential generation tests, connection binding, and string-mode refusal pass.

The tenant-floor group reports 34 governed relations against its expected 36.
The denial matrix reports one extra guest UPDATE privilege and six absent management-admitter SELECT privileges.
The [source comparison](authority-preparation-001/authority-failure-source-comparison.json) finds identical bytes in all 29 relevant baseline and cutover files.
Bead `wamn-0h0g.2.7.9` owns diagnosis of these three assertions.
A live baseline comparison and governing contract review remain pending.
No permission changes or passing authority-gate claim follow from this failed run.
Public logs retain Rust test results, while private diagnostics remain outside this branch.


## Authority contract correction

The [live baseline comparison](authority-baseline-001/comparison.json) reproduces all three authority failures at clean `dfa1c318`.
Both groups report 21 passes and three failures, with identical actual and expected values.
The [contract review](authority-contract-fix-001/diagnosis.json) traces each difference to its governing source and commit.

The current schema contains 34 governed relations after one relation addition and three fixture removals.
The tenant-floor test now pins 34 and retains its policy, index, and tenant-refusal assertions.
Commit `46514ab9` removed six management-admitter reads under `wamn-10yt.10.14`.
The denial matrix now excludes those positive expectations while keeping all six relations in its refusal universe.

The guest UPDATE privilege served a callable-admission lock that commit `b1d42599` removed under `wamn-0h0g.22.7`.
Current registration writers use an owner connection, and the materializer only reads registrations.
The correction removes the obsolete column grant and comment while preserving the guest denial assertion.
This deletion controls fresh catalogs and does not revoke an existing database ACL.
The greenfield cutover adds no migration for existing database permissions.

The [static results](authority-contract-fix-001/static-validation.json) pass formatting, source hashes, patch application, and unchanged denial-surface assertions.
The two live authority groups still require a new run on the corrected source.
The protected-write inventory also requires the measured grant removal before `wamn-0h0g.2.7.9` closes.


## Virtualized artifact proof and route fixture

The [virtualization run](virtualization-live-001/summary.json) uses clean source `0388e3bc`.
Its first test passes the exact probe and Receiving import sets and eight Receiving exports.
Its second test fails before invocation because the trusted route fixture omits required component operations from its serving manifest.
The run proves neither sentinel isolation nor panic refusal.
Owned Compose cleanup and the production `m1` rebuild both pass.
The rebuild does not close the separate cross-profile digest finding, `wamn-10yt.61`.

The correction projects the admitted operation facts through the existing `ServingComponentOperation` type, as production publication does.
It retains registered operation identity, fresh-credential requirements, dependencies, and exact SQL statements.
The [scoped Clippy run](fixture-operations-fix-001/exit-code.txt) exits 0 with warnings.
The updated recipe selects the release artifacts that the current build command produces.
Bead `wamn-0h0g.2.7.11` remains open until the actual isolation and refusal proof passes.


## Installed identities and interrupted operator recovery

The [fifth Receiving run](live-receiving-005/exit-code.txt) exits 1 at clean source `0388e3bc`.
Current telemetry, startup exposure, production routes, materializer acknowledgment, and idle executor signal cases pass again.
The [installed CRD receipt](live-receiving-005/journey/operator-recovery/installed-crd-identity.json) passes all five complete schema identities and storage-state assertions.
The [operator image receipt](live-receiving-005/journey/operator-recovery/operator-image-identity.json) matches the pinned distributed image.
This live evidence closes the CRD reader finding, `wamn-0h0g.2.7.8`, on commit `0388e3bc`.

The scheduler NATS outage lasts 150.85 seconds.
All three Host records enter the native loss-of-contact state and retain their object and process identities.
The [request samples](live-receiving-005/journey/operator-recovery/route-samples.json) record 28 exact successes and 24 transport failures.
About 13 seconds after NATS restoration, the operator container restarts within the same Pod and reports unready.
Its prior container exits 0 with reason `Completed`, and its restart count rises from one to two.
The helper stops on its unchanged-process assertion before it measures full recovery.
The [owned cleanup](live-receiving-005/journey/cleanup.receipt) passes.

The [native source review](operator-supervision-fix-001/README.md) distinguishes healthy reconnect attempts from terminal NATS closure that triggers Kubernetes supervision.
The corrected proof accepts only a contiguous graceful restart with previous-container closure logs and a matching Kubernetes liveness event.
It preserves Host process identity, operator Pod and image identity, exact response checks, and the original 120-second recovery ceiling.
It requires fresh Host status after a supervised transition and records original and replacement operator diagnostics.
The [offline controls](operator-supervision-fix-001/offline-validation.json) preserve refusal of the unexplained fifth-run restart.
Bead `wamn-0h0g.2.7.10` remains open for the next live run and the actual restart cause.


## Measured authority correction

The [fresh authority rerun](authority-inventory-live-001/summary.json) passes all 24 tests at clean source `379faef6`.
The tenant-floor group passes four tests, and the denial matrix passes 20 tests.
Both PostgreSQL 18 fixtures receive explicit inputs, execute without skips, and pass cleanup.

The [read-only capture](authority-inventory-live-001/event-registration-protected-write-capture.json) confirms the event-registration permissions before fixture cleanup.
Guests retain SELECT, but no non-owner holds a table or column write grant.
Row security remains enabled and forced.
The protected-write inventory now records the measured grant removal and marks this relation as unavailable for author writes.
Only these two fields change in this one inventory row.
This capture does not regenerate the full inventory or measure an existing deployment.
Source correction `55481ea1` and this measured inventory change resolve `wamn-0h0g.2.7.9`.
The [inventory conformance run](authority-inventory-preparation-001/conformance.log) passes both tests with zero ignored cases.


## Active release query correction

The [second virtualization run](virtualization-live-002/summary.json) uses clean source `ab0467f4`.
Artifact inspection passes, but active route resolution fails before guest invocation.
The active query returns six columns to a decoder that requires eight.
Commit `1efc3b2c` introduced this mismatch before the cutover.
The failed run retains successful fixture cleanup and the production guest rebuild.

The correction supplies the current release snapshot and its full component set through the existing active query.
It preserves activation, package membership, tombstone, environment, component identity, and completeness checks.
The existing guest proof now also refuses an absent snapshot and a release that differs from the mounted release.
The [scoped validation](scoped-validation-002/summary.json) records passing Clippy for both changed Rust packages and their test targets, with warnings.
Its source hashes include the separate memory test and WMS probe correction that await their own commits.
Bead `wamn-0h0g.2.7.13` owns this query correction and remains open until the live proof passes.
The operation projection from `wamn-0h0g.2.7.11` passes manifest decoding in this run, but the complete guest proof remains open.
