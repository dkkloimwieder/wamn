# Native-alignment ledger — wamn vs wasmCloud

WAMN uses native capabilities first.
Each WAMN deviation names its benefit and its condition for removal.
The active runtime source is direct upstream `v2.9.0` at
`68ebece9c537f8bb4b5c9999f274ec68d60f35a9`, with zero carried patches.
The cutover owner is `wamn-0h0g.2.7`.
Source, lifecycle, router, and deployment changes are prepared under that owner.

The source change does not establish deployment or release readiness.
The [cutover charter](../wamn_wasmcloud_2_9_cutover.md) governs those later stages.
The [cutover report](../perf/2026.09/wasmcloud-2-9-cutover/report.md) records evidence and limitations.
The historical ledger below retains the reasons for the former 2.8 patches.

## Part 1 — Platform-model deviations

| # | Deviation (wamn vs native) | What it buys | Disposition / re-convergence trigger |
|---|---|---|---|
| 1 | `wamn:postgres` capability seam — guests never hold sockets or credentials (native: guest sockets + allowed-hosts) | Host-injected credential generations, RLS identity set by trusted code, span per effect. This is the product. | **Keep.** Implementation rides `component-model-async`; never re-converges. |
| 2 | Publish-time import allowlist — **as of §2a, the declared capability registry**: 8 rows of `{package, version, posture}` matched on exact package AND exact version (native: runtime non-satisfaction of ungranted caps) | Auditable tenant contract; refusal before distribution, not at wiring. Posture is declared rather than inferred from a namespace, so an unregistered import is refused by ABSENCE and interfaces carry name, shape and posture with no second classifier. | **Keep** — additive over native model, composes cleanly. The registry lives in `wamn-component-policy` as code, never a catalog row, so posture moves only through code review plus a ledger row. Design: `docs/architecture/2a-capability-registry.md`. |
| 3 | Wirings as gated tenant rows + hot pointer flip (native: lattice links / wadm manifests / CRDs) | Tenancy, minutes-scale user churn, gate + rollback + provenance. | **Keep (ruled).** Trigger to revisit: upstream ships a multi-tenant, per-link-authz link store. |
| 4 | Wasmtime's pooling **allocator is on**, while the router still constructs and drops a fresh store per invocation; wash-runtime's warm `InstancePool` dispatch remains unwired. | Fast allocator-backed fresh instances without guest state surviving a call. Allocator capacity and store reuse are separate controls. | **Keep the allocator; keep reuse off.** Admission rejects `poolSize > 0`, so every workload stays `InstancePolicy::Ephemeral`. The `.17.x` router/native-dispatch wiring remains open only if it preserves that fresh-store rule. Revisit warm reuse with explicit affinity/windowed state; `a5e7d5a` is already present for that future step. |
| 5 | Guest build boundary: native std + `wasm32-wasip2`, normalized before admission by pinned upstream WASI virtualization (the existing `no_std` guests remain until their owning slices migrate them) | The closed capability registry (row 2; this cross-reference said "the 4-element allowlist" until §2a replaced that allowlist with the registry — corrected under standing trigger 8) stays closed with **zero exemption machinery**. Linking std still adds 14 imports per guest, 10 of them `wasi:cli`; deterministic post-link composition removes that ambient CLI surface while preserving `wamn:postgres` and only the already-admitted `wasi:io`/`wasi:clocks` packages. | **Re-convergence proven — amended 2026-08-31.** `wamn-component-virtualizer` pins WASI-Virt `0.2.0` at `448f6df8f688cee5d6995e96b1ffc31f9bf00742` and adapter SHA-256 `28eff8a2255812b440fbad2784a5a87660321e667331c17fb9a95f29caa85632`; its fixed profile supplies an empty environment, denies stdio and exit, passes clocks, filters unused virtualization, and disables `wasm-opt`. Byte-identical repeated output and the SQLx component inventory proved the result. This is build normalization, **not an admission exemption**: the unchanged admission path receives ordinary component bytes whose direct imports satisfy row 2 plus their explicit `wamn:*` grants. **The capability-registry rows move with this pin (§2a).** The virtualizer REWRITES the WASI version admission sees — measured on `receiving`, the authored WIT says `0.2.12`, the raw build imports `0.2.9`, and the virtualized artifact imports `0.2.12` — so row 2's inherited `wasi:io`/`wasi:clocks`/`wasi:random` rows are pinned to the revision and adapter digest above, not to our WIT. Bumping either without moving those rows would silently refuse every std guest at admission; `capability_registry_wasi_rows_match_the_vendored_wit` makes that fail at the gate instead. |
| 6 | Our `tools/build-components` instead of `wash build` | Palette publish integration (digest, admission). | **Watch.** Re-converge when `wash build` + OCI push covers admission hooks; not blocking. |
| 7 | Already aligned (no deviation): OCI distribution + digest pinning, `implements`/maps bindings for connection requirements, runtime-operator CRDs, HPA scaling, wasip2 + P3 + component-model-async, JetStream at-least-once ack-after-process. | — | Cited in exe-model; keep tracking upstream defaults each sync. |
| 8 | Per-environment `events.k8s.io` Role + RoleBinding for the runtime-operator. Native chart 2.9.0 still grants only core-group Events. | Lets the native `CrossEnvironmentSchedulingDenied` Warning Event publish alongside the authoritative `HostSelection=False` condition. | Temporary WAMN configuration. The [2026-09-09 distributed render](../perf/2026.09/wasmcloud-2-9-cutover/deployment-001/operator.json) confirms the gap, so the scoped `create,patch` overlay remains. The first Receiving run at `7798190c` records the native [Warning Event](../perf/2026.09/wasmcloud-2-9-cutover/live-receiving-001/journey/cross-environment-event.json) and exact [RBAC receipt](../perf/2026.09/wasmcloud-2-9-cutover/live-receiving-001/journey/operator-events-rbac.receipt): `create,patch` allowed; all six other checked verbs denied. Remove the overlay only when a pinned chart supplies the required permission in every watched namespace. |
| 9 | `wasmcloud:blobstore@0.1.0` implemented by a WAMN plugin over `object_store`, with binding-scoped credentials and bucket/prefix confinement (native: upstream's `wasi_blobstore` filesystem/in-memory/NATS providers, registered through `multiplexed_plugins()`). | Confinement the contract does not carry: the environment owns endpoint, container and prefix, the author owns only an object key relative to the prefix, and the credential is host-signed so no credential-shaped value exists in anything the guest composes. Plus a bounded body and explicit-commit writes, so a truncated stream cannot overwrite a good object under a deterministic key. | **Keep.** WAMN is the single runtime owner of this contract, and structurally so: the `wasi-blobstore` cargo feature is NOT enabled, so upstream's providers are never compiled and a second registered runtime is impossible rather than merely avoided. **Deviation is confined to three verbs that never succeed, in two categories with different remedies.** *Refused by policy* — `copy-object`, `move-object`: their `object-id` arguments are bare strings carrying no backend or binding discriminator, so confinement cannot hold across them; they return as binding-scoped variants if demand appears, never these signatures. *Unsatisfiable by backend* — `info`: `container-metadata` requires a `created-at` and the object-store surface exposes no container creation time; fabricating `0` would be a timestamp a guest could act on. It returns if `object_store` grows the capability; no policy decision is involved. `create-container`/`delete-container` are NOT deviations — refusing them is the ordinary operation of the environment owning the container. Re-convergence: adopt `wasi:blobstore` when the draft stabilises AND upstream binds it by default. Design: `docs/architecture/2a-capability-registry.md` (posture), bead `wamn-jpxo`. |
| 10 | Closed, PLATFORM-OWNED PostgreSQL extension list installed in every project-environment database (native: whatever a migration asks for). `CREATE EXTENSION` is refused to a package by the migration policy's ruled object classes, so the list is the only way one arrives. It holds exactly `btree_gist` (`wamn_control_provision::sql::PLATFORM_EXTENSIONS`), installed by `apply_package` on the administrator connection before any package DDL. | Extension installation is superuser authority and an extension is database-wide, so leaving it to package text would let one package change the substrate every other package on that database runs on. `btree_gist` is on the list because without it no `EXCLUDE USING gist (<scalar> WITH =, <range> WITH &&)` constraint is expressible, and that constraint is the platform's own documented overlap rule. Three independent authoring agents wrote it, all three were refused with `data type uuid has no default operator class for access method "gist"`, and all three fell back to a row lock (`wamn-yk9l`, pilot series 010). | **Keep.** The list moves only by owner ruling, one row here per entry. Re-convergence trigger: a package-scoped extension mechanism that cannot affect a neighbour, which PostgreSQL does not offer today. Proven in both directions by `the_platform_extensions_make_an_exclusion_constraint_reachable`. |

## Part 2 — Upstream 2.9 source

The root workspace and both standalone HTTP probes use direct upstream source at the same peeled release commit.
The Wasmtime requirements and resolved packages use one crates.io `47.0.4` family.
WAMN keeps upstream default providers disabled.
`tools/wasmcloud-release-check` requires clean released source and retains the upstream runtime suite and isolated Git template fixtures.

The [combined Cargo check](../perf/2026.09/wasmcloud-2-9-cutover/build-003/exit-code.txt) passed with `--workspace --all-targets --locked`.
The host and executor binary builds, both standalone probe checks, and three [chart argument checks](../perf/2026.09/wasmcloud-2-9-cutover/validation-001/host-arguments.json) also pass.
The argument checks deliberately refuse startup after parsing and load no secret environment.

The [upstream gate](../perf/2026.09/wasmcloud-2-9-cutover/upstream-gate-003/results.json) passes all six legs with upstream default features, separately from WAMN production features.
Runtime harnesses report 1,050 passes, including one explicit NATS large-payload self-skip, leaving 1,049 executed pass results.
Four isolated Git template tests also pass.
Inactive feature targets and the skipped 2 GiB payload test supply no execution proof.

The earlier [workspace sweep](../perf/2026.09/wasmcloud-2-9-cutover/validation-001/workspace-results.json) exits 101 with 2,114 reported test passes, six doctest passes, and 67 failures across 32 targets.
All failure names match retained baseline failures: 65 missing inputs, unavailable Kubernetes discovery, and the known undeclared-Secret code failure.
At least 85 reported passes explicitly skip their proof, so these totals are not executed-proof counts.
The named memory, Router, lifecycle, saturation, and local development shutdown tests pass.

Workspace [Clippy](../perf/2026.09/wasmcloud-2-9-cutover/validation-002/exit-code.txt) exits 0 with warnings.
After two lifecycle lint cleanups, all four [focused lifecycle tests](../perf/2026.09/wasmcloud-2-9-cutover/validation-003/lifecycle-tests.log) pass.
The [scoped runtime Clippy](../perf/2026.09/wasmcloud-2-9-cutover/validation-003/runtime-clippy-exit-code.txt) also exits 0 with warnings.
The final lifecycle module emits neither of its two corrected warnings.
The first [full Receiving journey](../perf/2026.09/wasmcloud-2-9-cutover/live-receiving-001/journey/verdict.json) passes at clean `7798190c`, covering the released application, materializer, native scheduling, and cleanup.
Current [telemetry](../perf/2026.09/wasmcloud-2-9-cutover/live-receiving-003/journey/telemetry/receipt.json) passes at `a40d1c18`.
The complete [startup receipt](../perf/2026.09/wasmcloud-2-9-cutover/live-receiving-004/journey/startup-burst/result.json) passes at `802aed06`, with eight overlapping native handlers per cold and warm herd.
The local proof retains full control, probe, and application observations during native-start intervals.
It measures queued demand with one shared HTTP digest, not permit occupancy, CPU use, or a distinct-digest herd under Kubernetes limits.
The fourth journey then fails while decoding five consecutive CRD objects, before installed-schema assertions or deliberate operator disruption.
The corrected reader preserves all five distributed schema hashes under `wamn-0h0g.2.7.8`.
The [fifth run](../perf/2026.09/wasmcloud-2-9-cutover/live-receiving-005/journey/operator-recovery/installed-crd-identity.json) passes all five installed CRD identities and the pinned operator image.
All three Host objects and processes survive the 150.85-second NATS outage and enter the native loss-of-contact state.
The operator then restarts within the same Pod, and the proof stops before complete recovery.
The [sixth-run diagnosis](../perf/2026.09/wasmcloud-2-9-cutover/operator-timeout-diagnosis-001/diagnosis.json) identifies a liveness HTTP timeout that triggers a graceful operator restart before NATS returns.
The underlying HTTP delay remains unknown.
The owner now accepts the documented supervised restart path under `wamn-0h0g.2.7.10`, with the original 120-second recovery ceiling retained.
Complete recovery still needs execution. The failed runs remain failed and the underlying HTTP delay remains unproved.

The first armed [authority run](../perf/2026.09/wasmcloud-2-9-cutover/authority-live-001/summary.json) records 40 passes and three failures across 43 tests.
Its seven runtime claims tests and SQLx transaction isolation pass with explicit fresh database inputs.
The baseline live run reproduces all three failures.
The correction removes an obsolete guest UPDATE grant and repairs two stale test expectations without widening authority.
The [fresh-server rerun](../perf/2026.09/wasmcloud-2-9-cutover/authority-inventory-live-001/summary.json) passes all 24 tenant-floor and denial-matrix tests at clean `379faef6`, with no skips and successful cleanup.
The [protected-write capture](../perf/2026.09/wasmcloud-2-9-cutover/authority-inventory-live-001/event-registration-protected-write-capture.json) confirms guest SELECT, no non-owner table or column writes, and forced row security.
The measured inventory correction changes only this relation's grant-removal and author-write fields under `wamn-0h0g.2.7.9`.

The [full WMS journey](../perf/2026.09/wasmcloud-2-9-cutover/live-wms-001/journey/journey.receipt) passes at clean `ab0467f4`.
It proves the winning movement and stored label, released operations, native scheduling refusal, idle materializer startup, and cleanup.
The separate [startup run](../perf/2026.09/wasmcloud-2-9-cutover/live-wms-startup-002/journey/runtime-startup.receipt) passes at clean `6bfda73b`, with cold/restart startup times of 12,561/623 ms and four unchanged cache files.
One [steady request ratio](../perf/2026.09/wasmcloud-2-9-cutover/live-wms-startup-002/journey/overhead-ratio-steady.receipt) is 7.022 against the unchanged ceiling of 12.
The [restart probe](../perf/2026.09/wasmcloud-2-9-cutover/live-wms-startup-002/journey/first-request-restart-first.receipt) reaches exact 200 after seven HTTP 503 responses and 27 transport failures.
Its 34-second timer starts after readiness waits.
It excludes the earlier interval from SIGTERM to the request job.

The [engine](../../crates/platform/runtime/src/engine.rs) and [manual stores](../../crates/execution/host/src/router_driver.rs) share the native guest-memory budget in `Count` mode.
Production rejects `enforce` and `off` through the native memory-mode flag and environment name.
The heap ceiling, pooling allocator, fresh invocation stores, and native epoch ticker remain in place.
The host explicitly selects `Meters::new(MeterKind::Duration)` without fuel accounting.

The [memory evidence map](../perf/2026.09/wasmcloud-2-9-cutover/memory-cancellation-map-001/evidence-map.json) records executed accounting, limit, cancellation, trap, nested-call, and epoch proofs.
The two manual stores coexist, but their growth calls run in sequence.
The later [deadline proof](../perf/2026.09/wasmcloud-2-9-cutover/memory-cancellation-map-001/deadline-refund-execution.json) observes two charged guest pages, native epoch interruption, zero usage after store removal, and fresh allocation.
Its receipt retains the tested source hashes, which match memory correction `5c70b31a`.
The final workspace sweep below retains its separate failure classification.

The WASI-Virt pin and admitted capability rows remain unchanged.
Both exact [virtualization tests](../perf/2026.09/wasmcloud-2-9-cutover/virtualization-live-003/summary.json) pass at clean `d45a0fe9`, without skips.
They prove artifact imports and exports, active-release refusals, sentinel isolation, a real connection, and typed refusal after a guest panic.
Owned fixture cleanup and the production guest rebuild pass.
This proof does not close the cross-profile digest finding `wamn-10yt.61`.

The host and executor lifecycle uses native `ProbeState` and `Liveness`.
Host readiness requires a real command-loop beat and registers the native ingress connection-limit check.
Executor liveness follows actual queue turns and successful durable lease renewals, while its existing readiness owner retains release and database checks.
Both services mark draining before bounded cleanup, use native `flush_within`, and cap the final Tokio shutdown wait.

Local tests cover cleanup bounds, real-beat liveness, and native probes during ingress saturation.
The final [real host test](../perf/2026.09/wasmcloud-2-9-cutover/workspace-sweep-final-001/workspace.log) passes in 69.63 seconds at clean `c6808b12`.
The rebuilt host passes ingress saturation and recovery, a 50-second NATS outage and reconnection, and successful SIGTERM/SIGINT exits in 5,015/5,005 ms.
An observed blocked exporter produces exit 1 after 7,006 ms, with an explicit flush failure.
The [host Clippy receipt](../perf/2026.09/wasmcloud-2-9-cutover/host-lifecycle-clippy-final-001/summary.json) exits 0 on unchanged clean source, with existing warnings.

A separate [native first-beat case](../perf/2026.09/wasmcloud-2-9-cutover/workspace-sweep-final-001/host-lifecycle/native-missing-first-beat.receipt) exercises ClusterHost and WAMN lifecycle helpers inside the test process.
It observes 300 ms without a beat, then retrieves the actual subscription error with cleanup in 0 ms.
This does not prove unexpected failure exit in a WAMN subprocess.
The local proof leaves the initial starting window, active guest drain, and in-cluster recovery under load unproved.
A later [host test](../perf/2026.09/wasmcloud-2-9-cutover/probe-listener-lifecycle-001/summary.json) proves actual native probe termination through the production error translation and cleanup.
All 15 host unit tests and host Clippy pass on the retained patch above `b8e9881d`, with stable source-file hashes.
The listener case serves HTTP 200 before task cancellation and completes without the configured 60-second traffic delay.
It runs inside the test process and does not establish unexpected failure in a WAMN subprocess.
Local development allows 70 seconds after SIGTERM and a separate five seconds for forced-kill reaping.
Its cleared child environment excludes ambient WASH overrides, so the default 56-second host exit envelope fits that grace.

The [expected-host Router adapter](../../crates/platform/runtime/src/expected_router.rs) wraps native `DynamicRouter` and runs inside native `Ingress`.
It projects explicit HTTP and studio hostnames from the host's already-verified release.
An expected hostname without a native route returns `RouteError::Unavailable` (503), with no `Retry-After` header.
An unbound unknown host retains native 404. Application-generated 404 responses pass through unchanged.
A route selected before its native dispatch handle exists still returns 404.
Operator-created Service aliases absent from the trusted release are outside the projection, as is wildcard expansion.
Their unbound requests retain native 404.

The adapter adds no ingress copy, private dispatch access, or general response rewriting.
It does not change operation-specific retries or unknown-outcome handling.
Its [tests pass](../perf/2026.09/wasmcloud-2-9-cutover/validation-001/workspace-sweep.log) for bind/unbind transitions, refusals, application responses, and the missing-handle residual.
Bead `wamn-0h0g.2.7.4` records the missing-handle 404 as a limitation of the permitted adapter.
The WMS startup probe records a 2.9 restart/rebind window with no observed 404, within the limited request-job clock described above.
The closure of `wamn-0h0g.17.20` records the historical fork proof and does not close this residual.
Neither this adapter nor native probes establish route-aware traffic removal.

Deployment recipes now select chart `2.9.0` and retain the WAMN host binary.
The [distributed-chart render](../perf/2026.09/wasmcloud-2-9-cutover/deployment-001/observations.json) passed for default, Receiving, and WMS profiles.
The profiles explicitly select Count mode and concurrent-start ceilings of four, four, and one, preserving their existing CPU limits.
Host probes use port `8081`, with `70s` termination grace above the configured `61s` process envelope.
Executor probes retain port `8089`, with `15s` grace above its `9s` envelope.
Namespace scopes, the disabled gateway, and row 8's modern Event overlay remain intact.

The render and [public operator image metadata](../perf/2026.09/wasmcloud-2-9-cutover/deployment-001/distributed-image.json) prove configuration and distributed artifact identity only.
The owning Rust render test executes but fails during Kubernetes API discovery because `127.0.0.1:8080` refuses the connection.
Installed CRD identity, local startup exposure, and the separate WMS startup/cache proof now pass.
The [seventh run](../perf/2026.09/wasmcloud-2-9-cutover/live-receiving-007/exit-code.txt) stops before any NATS fault because the sampler Pod is still `ContainerCreating`.
Bead `wamn-0h0g.2.7.16` owns bounded sampler creation/readiness waits. Complete operator recovery still requires the corrected proof rerun.
The [repeated performance comparison](../perf/2026.09/wasmcloud-2-9-cutover/performance-comparison-001/tables.md) is complete. It does not establish a general improvement.
The [direct stop/start runbook](../../deploy/README.md#wasmcloud-29-cutover) keeps one active runtime version per environment without a maintenance window or compatibility period.

The [distributed CRD capture](../perf/2026.09/wasmcloud-2-9-cutover/deployment-crds-001/crd-inventory.json) confirms that all five 2.9 schema files match the pinned source.
Host and Artifact files match distributed 2.8 byte-for-byte.
Workload, WorkloadDeployment, and WorkloadReplicaSet each add `reclaimMinInstances` and `reclaimWindowSeconds`.
The runbook applies these pinned CRDs before operator upgrade, with visible ownership conflicts and no `--force-conflicts`.
This corrects Helm's existing-install behavior and supplies no installed-schema proof.

The isolated integration committed at `15f2d4da`; stage 3 has not landed on main.
The earlier [source capture](../perf/2026.09/wasmcloud-2-9-cutover/validation-004/source-inputs.json) precedes that commit.
The [final workspace sweep](../perf/2026.09/wasmcloud-2-9-cutover/workspace-sweep-final-001/workspace-results.json) completes at clean `c6808b12` in 264.834 seconds and exits 101.
It reports 2,116 test passes, six doctest passes, and 68 failures across 33 targets.
The 67 baseline failures retain their identities and causes: 65 missing inputs, unavailable Kubernetes discovery, and the known undeclared Secret.
The additional failure is the unarmed startup-burst fixture, with no unresolved classifications.
The run reports zero ignored tests, two filtered regeneration tests, and at least 85 explicit self-skips.
These reported passes are not an exact executed-proof count.

The [source receipt](../perf/2026.09/wasmcloud-2-9-cutover/workspace-sweep-final-001/source-stability.json) confirms unchanged clean source before and after the run.
The [2.8 performance baseline](../perf/2026.09/wasmcloud-2-9-cutover/performance-baseline-evidence-001/evidence-map.json) passes 108 steps with a service ratio median of 10.1408 against 12.
The first 2.9 benchmark stops before traffic because the exact probe assertion omits the Kubernetes-defaulted HTTP scheme.
The [correction replay](../perf/2026.09/wasmcloud-2-9-cutover/receiving-probe-scheme-fix-001/receipt.json) passes and preserves every probe refusal.
The [second candidate run](../perf/2026.09/wasmcloud-2-9-cutover/performance-2-9-002/exit-code.txt) passes at clean `e7033f72`, with 108 completed steps and a service ratio median of 8.8608 against 12.
The [completion receipt](../perf/2026.09/wasmcloud-2-9-cutover/performance-completion-002/state.json) confirms both measured source revisions remain clean afterward.
The four guest byte hashes, mounted release digest, and measured host resources match the baseline.
Runtime, chart, feature, and instrumentation changes remain part of the complete cutover comparison.
Cold/restart startup is 747/231 ms, with three unchanged cache files. The 64-second route recovery clock starts after readiness waits.
The [candidate map](../perf/2026.09/wasmcloud-2-9-cutover/performance-candidate-evidence-001/evidence-map.json) retains exact image identities and the missing operator Pod imageID receipt.

The accepted `wamn-0h0g.2.7.10` proof permits contiguous graceful restarts in the same operator Pod and image, with recorded kubelet liveness evidence.
A prior terminal NATS closure or fault-time HTTP timeout must identify the permitted cause for that same container.
Host identities, fresh Host status, exact HTTP 200, and the original 120-second ceiling remain required. Execution is pending.
The accepted `wamn-0h0g.2.7.12` scope excludes missing production automation admission and its dependent queued-execution and active-work drain proofs.
Producer implementation belongs to `wamn-10yt.74`. Dependent active-work shutdown proof belongs to `wamn-10yt.75`.
Both remain unproved, without restoring retired grants or introducing substitute admission.
Existing evidence proves idle executor signals and Receiving stream delivery.
Main integration has not occurred. Further live proof requires clean, fixed source and does not inherit a release-readiness claim.

The charter retires both 2.8 patch deviations by decision.
Upstream 2.9 does not incorporate the former missing-route/missing-handle 503 patches or the nine private P2 phase spans.
The expected-host adapter has the narrower behavior described above.
The old phase measurements remain historical evidence, not current diagnostic coverage.

### Historical 2.8 fork ledger

The former `wamn/2.8.0` branch ended at `735b5798`.
It carried two patches in three commits over upstream `v2.8.0`.
The 503 patch began at `2a183dfb` and completed at `735b5798`.
The following rows record the former behavior and removal conditions.
The 2.9 charter supersedes those conditions and forbids carrying these patches forward.

| Patch | Bead | What it changes | What it buys | Exit condition |
|---|---|---|---|---|
| `2a183dfb` + `735b5798` — an unbound-but-routable host answers **503 `Retry-After: 1`**, not 404 | `wamn-2w3x.2`, completed by `wamn-2w3x.4` | `host/http.rs`: `RouteError::NoWorkloadForHost` status 404 → 503 plus a new `retry_after_seconds()`, and the dispatch path's missing-workload-handle arm, where the router has already resolved the route so only the handle is absent. Both now carry `Retry-After`. `735b5798` completes the same deviation: `tests/integration_secrets_plugin.rs` asserted the old 404 from a stopped workload and now asserts the 503 (one deviation, two commits; no rewrite of the pushed patch, by ruling). | **Correctness, measured.** After a host restart the operator-managed EndpointSlice keeps advertising the host while the workload is rebound, so the restarted host takes live traffic for the whole window and answers 404 for a route present in its own weld. A client treats 404 as an answer and retries a 503, so a transient rebind became a wrong result at the caller — 21 seconds of it, second by second, in `docs/perf/2026.09/2-auth/restart-watch.log`. No in-tree lever reaches it: the status is this line, the listener binds inside `run_cluster_host`, and the endpoint is the operator's. | **Drop when a pinned upstream release answers an unbound-but-routable host with a retryable status of its own.** No upstream filing, per standing owner law. |
| `2eadd937` — spans around each host call on the p2 request path | `wamn-2w3x.3` | `host/http.rs`: nine `info_span`s around the host's own calls on the per-request p2 store — `http.route`, `http.lookup_service`, `http.lookup_workload`, `http.new_store`, `http.incoming_request`, `http.instantiate`, `http.handle`, `http.await_head`, `http.store_drop` — and the store drop made explicit inside the detached task so the allocator's slot return has a name. No behaviour change. | **Measurement, owner-ruled (`wamn-0h0g.17.16`).** A third of a hot request, 4–5 ms, was unspanned, and every unnamed gap sits in this function: the flow guest's instantiate, the store build, the head hand-off and the teardown are fork code, so no in-tree span can reach them. With the host's calls named, the guest's own path shows as the gaps between them. Report: `docs/perf/2026.09/5-residue-spans.md`. | **Drop when a pinned upstream release spans its own p2 dispatch path, or when the p2 per-request store path is retired for the pooled one.** No upstream filing, per standing owner law. |

Dated correction, 2026-09-09: the historical claim "No in-tree lever reaches it" was too broad.
Native v2.8.0 already exposed [`Router` and `RouteError::Unavailable`](https://github.com/wasmCloud/wasmCloud/blob/5c4ec4a3d008b3f401d9e763515f434deebc9936/crates/wash-runtime/src/host/http.rs#L128), plus `Ingress::new(router, addr)`.
That public interface permits an expected-host refusal before dispatch.
It does not control the later missing-handle response or operator EndpointSlice readiness.
The historical rows and measurements retain their original scope.

**What this patch does not fix.** It makes the rebind window *honest*, not
*shorter*. The window is the operator taking ~20 s to rebind the workload while
the route-manager EndpointSlice still advertises the host; with three replicas a
third of traffic meets a 503 for that window instead of a 404. The desired shape
is endpoint readiness computed from bound-workload state, so a restarting host
receives no traffic at all — upstream Go we neither patch nor file against.
Recorded on `wamn-0h0g.17.20` with its trigger.

**Accepted cost.** This layer cannot distinguish "never bound" from "not bound
yet", so a genuinely wrong `Host` header also receives a retryable 503. Ruled
the cheaper of the two errors.

| Former patch | v2.8 disposition | Native or WAMN-owned replacement |
|---|---|---|
| g2br.2 — epoch deadline policy | **Dropped** | Vanilla enables epoch interruption, starts its ticker, and hardens abandoned calls. WAMN starts no ticker; its manual router stores still set invocation deadlines. |
| g2br.3 + g2br.7 — per-store memory limiter and accessors | **Dropped** | Vanilla `HostMemoryBudgets` carries the host total, per-memory heap ceiling, and allocator instance count. WAMN needs one fixed heap ceiling, not a fork-level per-component policy system. |
| g2br.4 — outbound `wasi:http` trace injection | **Dropped** | WAMN's real outbound-effect path, `wamn:connection/http`, injects the active span context itself. |
| g2br.5 + g2br.6 — raw TCP/UDP denial | **Dropped** | Vanilla's centralized `SocketPolicy` is installed with `EgressMode::Enforce`; WAMN admission also rejects tenant `wasi:sockets` imports. |
| g2br.15 — plugin raw-socket opt-in | **Dropped** | `wamn.allow-raw-sockets` and `WAMN_ALLOW_RAW_SOCKETS` are retired. Every guest inherits the same host socket policy. Vanilla defaults deny special/link-local/metadata ranges and allow private ranges; wamn additionally denies private ranges (`wamn-d0w4`), making the address layer a default-deny floor with `allowed_hosts` an opt-in on top rather than the sole confinement. **No fork patch:** `EgressAddressPolicy.allow_private` is a public vanilla field, set from wamn's own `host_socket_policy`. |
| g2br.8 — `wamn.api.requests` counter | **Dropped; series retired** | No current metricbench or dashboard consumes the series. `wamn.router.delivery.attempts` and `.errors` are the live router-owned metrics; they deliberately do not claim the old HTTP `status_class` semantics. |
| g2br.12 — surface terminal P3 service failures | **Dropped** | WAMN has no `workload_status` consumer and admission excludes P3 service WorkloadDeployments. Reconsider only when a real consumer requires terminal service status. |
| g2br.14 — clear HTTP egress state on every stop | **Dropped** | WAMN outbound HTTP uses its invocation-scoped `ConnectionHttp` plugin, not vanilla pooled `wasi:http` egress. Shipped flow HTTP imports WAMN routing/invocation rather than `wasi:http/outgoing-handler`. |
| g2br.16 — reusable-instance kill switch | **Dropped** | Workload generation/admission enforces `poolSize = 0`; no host environment switch duplicates a manifest value WAMN controls. This does not disable Wasmtime's pooling allocator. |
| g2br.13, .17, .18, .19 — rustdoc, formatting, Git fixture isolation | **Dropped** | WAMN does not modify upstream for local maintenance gates. `tools/wasmcloud-release-check` runs the upstream fixture with global/system Git configuration isolated. |

The g2br.8 consumer audit found no metricbench, dashboard, alert, or code
consumer of `wamn.api.requests`. Exact relocation is also not available through
the vanilla v2.8 public API: `Router` runs before dispatch and cannot observe
the final response status, while the status-recording choke point inside
`Ingress` is private. Deriving a similarly named series from sampled INFO spans
would undercount and would not preserve the counter's contract. WAMN therefore
keeps its existing router-owned attempt/error metrics and retires the unused
series instead of carrying a patch or manufacturing an approximate metric. Per
owner directive, no upstream issue or PR is filed.

The v2.8 async-messaging interface is also taken exactly as upstream ships it:
`wasmcloud:messaging@0.3.0`, streaming bodies, in-flight limits, and 0.2
compatibility require no fork integration. WAMN's JetStream capability remains
a separate WAMN-owned interface.

Two address-policy subjects remain deliberately separate:

- `wamn-d0w4` is the Phase-B posture item for a future deliberately admitted
  raw-socket guest. It does not govern `wasi:http` or `ConnectionHttp`, and it
  must land before any such guest is admitted.
- `ConnectionHttp` currently delegates address confinement to its internal
  `ExternallyEnforcedNetworkPolicy`. Whether that plugin needs an in-process
  address-range check is a separate WAMN security concern; vanilla
  `SocketPolicy` does not cover it and this fork sync does not decide it.

## Standing triggers

1. **Each upstream minor:** pin the direct upstream peeled tag and run WAMN behavior proofs. The 2.9 charter requires zero carried patches.
2. **Router/native pool wiring (`.17.x`):** preserve `InstancePolicy::Ephemeral`; allocator adoption is already complete, warm-store reuse is not.
3. **Raw-socket admission:** resolve `wamn-d0w4` before the first guest is allowed to import `wasi:sockets`.
4. **ConnectionHttp address policy:** adjudicate independently of vanilla raw-socket defaults if the external enforcement boundary changes.
5. **CA accessor (`wamn-kdhw`, measured 2026-08-27):** still dropped, but the *"zero call sites"* premise has expired — the consumer appeared and is now named. Vanilla v2.8.0 ships the **setter** `set_extra_ca_certificates` as `pub` (`crates/wash-runtime/src/oci.rs:93`) and `services/executor/src/lib.rs:255` + `services/host/src/host.rs:312` already call it; but the **reader** `extra_ca_certificates()` (:217) is *private*, `wash-runtime` re-exports no `oci_client` symbol (`lib.rs` exports `pub use wasmtime;` alone), and its whole public transfer surface is `oci::{pull_component, push_component}` plus `component_source`, a wrapper over the former. Those two are wasm-component-shaped: they mint and demand `WASM_LAYER_MEDIA_TYPE` + `WasmConfig`, while WAMN's four OCI sites carry platform-owned media types — `application/vnd.wamn.component.v1+wasm` over `…component.config.v1+json`, and `application/vnd.wamn.release-manifest.v3+json` over an empty config — whose config blob **is** the admission proof that `ComponentArtifactSource::pull_verified` and `verify_release_manifest_artifact_layout` re-verify. They further need `ClientProtocol::HttpsExcept` (one insecure registry, not every registry), transport `read_timeout`/`connect_timeout` rather than `OciConfig`'s whole-future `tokio::time::timeout`, and `OciErrorCode::ManifestUnknown` discrimination for the idempotent republish probe. **So no vanilla path carries installed trust to those four sites**, and `ComponentArtifactSourceConfig::with_ca_paths` / `ReleaseManifestSource::with_ca_paths` keep re-reading the same bundles into per-client `extra_root_certificates`. `--oci-ca-path` / `WASH_OCI_CA_PATHS` therefore still feed two dialects. **Exit:** a synced tag makes `extra_ca_certificates()` public or ships a client-construction seam — then delete both `with_ca_paths` bodies for it. Do **not** close this by routing WAMN artifacts through `pull_component`/`push_component`: that discards the platform config blob and the layout check that prove them.
6. **Epoch cadence (`wamn-6evd`):** every tagged-release sync re-runs `the_manual_store_epoch_tick_still_mirrors_the_runtime_ticker` (`tests/conformance/src/runtime_inventory.rs`). It pins the VALUE of wash-runtime's `pub(crate) EPOCH_TICK` against `MANUAL_STORE_EPOCH_TICK` in `crates/execution/host/src/router_driver.rs`; an unmirrored upstream retune rescales **every node deadline** silently, because both halves still compile. This is one of the two source reads `wamn-hopk` R5 exempts as identity pins. **Re-converge:** when upstream makes the constant public, import it, compare it directly, and delete the scan.
7. **New deviation rule:** a WAMN model deviation lands with its ledger row in the same change. The 2.9 charter forbids upstream patches.
8. **Row-5 lineage (why the rule exists).** Row 5 was written from upstream's *posture* — std, per-component builds — without re-deriving our *constraint*. It named what native buys but not what the deviation was holding, and the disposition it reached was mechanically incompatible with row 2 of this same document. A measurement caught it, not a review: wave 69 rebuilt both guests without `no_std` and read the actual import surface. Resolved in row 2's favour — **the allowlist is the contract; the build shape serves it.** Any future row asserting "nothing security-relevant" must cite the measurement that establishes it.
9. **Registry credential reader (`wamn-kdhw`, measured 2026-08-27):** `wamn_runtime::registry_credentials::read_registry_credentials` is deliberately *not* `docker_credential`, and the two are not interchangeable. It takes an **explicit path** from `--registry-auth-file` — a projected Kubernetes pull secret, `/registry/config.json` in the shipped manifests — and demands an exact registry-authority key carrying plaintext `username`/`password`. `docker_credential::get_credential`, which `wash_runtime::oci`'s private `CredentialResolver` uses, resolves only `$DOCKER_CONFIG/config.json` or `$HOME/.docker/config.json`, and additionally admits base64 `auth`, identity tokens, helper binaries, and Docker Hub normalization — the four widenings that reader's doc comment closes by name at the production boundary. `wash_runtime::oci` exposes no explicit-path credential input, so converging would mean mutating process-global `DOCKER_CONFIG` **and** widening that surface. **Keep the WAMN reader** — this is a deviation, recorded, not an oversight. Re-converge only if upstream accepts an explicit-path credential source; a `DOCKER_CONFIG` shim is not a substitute.

Dated clarification for standing trigger 9, 2026-09-09: publication recipes now use the pinned upstream 2.9 CLI with a private `DOCKER_CONFIG` directory.
That environment applies only to the short-lived publisher process.
It does not change a running server's global environment or replace WAMN's explicit-path credential reader.
The server reader's exact-authority and credential-shape contract, and trigger 9's removal condition, remain unchanged.
