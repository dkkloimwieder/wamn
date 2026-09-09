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
The actual authenticated publication proof remains unexecuted.

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
