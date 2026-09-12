# WAMN → upstream wasmCloud 2.9.0
## Agent implementation work order

**Status:** revised next-platform-wave charter for owner review; not implementation evidence.  
**Goal:** run WAMN on the unmodified upstream release, retire both wasmCloud fork deviations, and verify the application and operational paths.  
**Context checked:** WAMN `main` at `2a4cd2886f6b637a62413b4abbab4fbe4a4d964f` (September 8, 2026 EDT). Fetch current `main` before implementation and record any intervening changes. [S1]

## 1. Scope and decisions

**Zero wasmCloud patches is the cutover requirement.** Depend directly on upstream, not a WAMN mirror. Do not cherry-pick the old patches, modify Cargo's checkout, introduce a local/path override, or copy upstream ingress into WAMN. A demonstrated blocker is reported for an owner decision; it does not authorize another fork. Preserve the old fork remotely as historical evidence; deleting it is not part of this task.

WAMN is greenfield. Change its integration, configuration and tests where necessary; delete replaced code rather than preserve compatibility shims. This does **not** authorize unrelated database, authorization, execution-model or application redesign.

Two deliberate differences from the fork are accepted:

- **Unbound HTTP routing:** §4’s adapter is **required if feasible**. Native routing-level 404 is the fallback only after its stop rule fires. The later missing-handle 404 and absent `Retry-After` are accepted; permanent route failure or steady-state regression is not. Record residual exposure on `wamn-0h0g.17.20`.
- **P2 phase spans:** retire the diagnostic patch now, even while P2 remains. Preserve historical evidence, but stop requiring those private span names in current tests or benchmarks.

Keep fresh application stores, the pooling allocator, existing credential hiding, admission rules, release verification, tenant isolation, and operation-level authorization. No new production database state, proxy, readiness controller, automatic mutation retries, or tenant capability is introduced. Other dependency forks, including `pg_walstream`, are outside this work order.

**P3 remains the HTTP direction, but is a separate cutover after the upstream baseline is green.** Neither patch retirement nor this upgrade depends on completing it. §7 records that follow-on without adding it to the initial agent assignment.

## 2. Establish the baseline and replace the source

Read `AGENTS.md`/`CLAUDE.md`, `docs/exe-model.md`, `docs/architecture/native-alignment-ledger.md`, `docs/operations/build-and-test.md`, and `deploy/README.md`. Use the existing Beads workflow and upgrade ownership. Do not create a parallel task program or push/commit outside the active repository authorization. Work in an isolated worktree; preserve unrelated edits. **Acquire the serial-wave ownership and machine fence in §8 before shared-file edits or baseline runs.**

| Identity | Verified value / instruction |
|---|---|
| Current runtime | Fork `735b57982545358409a7d965a22549b08487ca09`, based on upstream 2.8.0. Its Cargo comment incorrectly says it carries no patch. |
| Target repository | `https://github.com/wasmCloud/wasmCloud` |
| Target release | `v2.9.0` |
| Target **peeled commit** | `68ebece9c537f8bb4b5c9999f274ec68d60f35a9` — not the annotated tag object's SHA. |
| Runtime dependency family | Upstream declares Wasmtime `47.0.4`; resolve one consistent Wasmtime family and source in WAMN. |
| Operator chart | Upstream chart version and `appVersion` are `2.9.0`; verify actual distributed images/chart before use. |

These are source observations, not claims that WAMN already compiles against them. [S1–S3]

Before editing, record the WAMN SHA, runtime and tool versions, effective Cargo features, build profiles, deployed image/chart identities, and existing test failures. The latest reviewed commit reports a pre-existing sweep failure tracked as `wamn-362o.58`; reproduce or otherwise identify it separately, not as an upgrade regression. Do not present the baseline as entirely green. [S1]

### Dependency and build changes

Set the shared dependency to:

```toml
wash-runtime = { git = "https://github.com/wasmCloud/wasmCloud", rev = "68ebece9c537f8bb4b5c9999f274ec68d60f35a9", default-features = false, features = ["oci"] }
```

Retain explicitly required features at their existing consumers; the line above is not their complete feature inventory. Inspect normal service builds as well as tests, since feature unification can hide missing declarations. Do not enable upstream defaults to resolve compilation errors: they include additional providers and capabilities WAMN has not selected. Preserve the existing single-owner PostgreSQL/blobstore/logging registration and exclusions. [S3]

Update Wasmtime requirements and all affected lockfiles through Cargo. Keep the current Rust toolchain unless a demonstrated incompatibility requires changing it. Preserve deliberate cache/parallel-compilation features and profile choices. Check `cargo metadata --locked` and feature/dependency trees for **every relevant workspace and standalone probe**; no executed path may still resolve the wasmCloud fork or a second incompatible Wasmtime family.

Inventory active `wash` binaries and publication scripts. Pin upstream `wash` 2.9.0 in the project's controlled tooling location; update callers to its real command and JSON receipt format, not a legacy-command shim. Preserve the distinction between upstream component publication and WAMN's custom OCI artifact layouts and admission proofs. The P3 probe records why those publishing paths are not interchangeable. [S8]

**Capability-registry coupling:** WAMN already uses Wasmtime 47.0.3. Keep the separately pinned WASI-Virt/adapter unless a demonstrated incompatibility requires changing them. Class-2 inherited WASI rows (`wasi:io`, `wasi:clocks`, `wasi:random`) must match the **post-virtualization imports actually admitted**, not a guessed version from the host or authored WIT. If a necessary pin change moves that surface, update the pin, adapter digest, affected exact registry rows, conformance expectations and ledger row 5 **in the same commit**. `capability_registry_wasi_rows_match_the_vendored_wit` exposes an incomplete coupled update; do not waive its refusal or widen the registry. Verify rebuilt artifacts and exclusion tests. Unchanged import versions need no row change. [S10]

Rebuild images and affected components from source. Regenerate affected manifests, corpus/policy hashes and release artifacts through the owning tools. Do not hand-edit generated digests, disable drift checks, or overwrite sealed durable package facts to make the upgrade pass. Record compiler, profile, feature and artifact identities in the evidence.

## 3. Adapt the WAMN embedding, not only the dependency

Audit the production host, executor, local-dev activation and in-process gates. Changes inside upstream `wash host` do not automatically reach WAMN's own binaries or manually created stores.

| Concern | Required implementation |
|---|---|
| **API compatibility** | Adapt actual call sites to 2.9.0, including `UnresolvedWorkload::resolve(..., &Meters)` and changed host-handler signatures. Remove use of the retired global `invocation_meter`; use the public per-host meter APIs. Do not solve errors by disabling a capability or test. |
| **Memory configuration** | Replace duplicated host/executor text parsing with `HostMemoryBudgets::resolve_strs`, preserving WAMN's chosen values and configuration precedence. Invalid values fail startup. |
| **Aggregate guest accounting** | Attach the engine's shared budget to every manually created application `SharedCtx` with `with_guest_memory(engine.guest_memory())`, and call `install_memory_limiter` before instantiation. Native-created stores already use this mechanism. Do not construct a separate counter per invocation. |
| **Memory policy** | Use **Count** for the first deployment, explicitly recorded. Retain the existing enforced per-memory ceiling and allocator limits. Test **Enforce** in disposable proofs; enabling aggregate refusal in deployment is a separately recorded setting, not an accidental new default. Count is not enforcement, and guest growth accounting is not total process RSS. |
| **Deadlines and cleanup** | Preserve bounded instantiation, invocation, cancellation and capability cleanup. Re-run the existing epoch-cadence conformance check; keep one runtime-owned ticker. Do not infer that a direct `Store::new` inherits native dispatch safeguards. |
| **Metrics** | Explicitly select duration metering for the baseline, not fuel. Adopt `guest.invocation.duration` where native paths provide it; retain WAMN invocation/effect telemetry. Do not relabel the removed `guest.execution.time` as an equivalent CPU measure or claim private phase coverage from an aggregate histogram. |
| **Startup concurrency** | Expose/use the native `maxConcurrentStarts` setting rather than another start scheduler. Measure with WAMN's actual parallel-compilation features: simultaneous starts are not a CPU-core budget. |
| **Shutdown and health** | Wire the native probe/liveness facilities and bounded `observability::flush_within` into WAMN's own lifecycle. Handle SIGINT/SIGTERM and unexpected ingress/runtime termination; do not leave a failed runtime hidden behind a process still waiting for a signal. |

The relevant APIs and semantic changes are in the tagged source and upstream changes #5506, #5511, #5522, #5525, #5534 and #5542. [S4–S6]

**Minimum memory proof:** normal allocation succeeds; the fixed per-memory ceiling still refuses; two concurrent application stores charge the same budget; aggregate Enforce refuses excess growth; dropping/cancelling stores returns their charges. Include WAMN's manual nested-call path, not only an upstream example. Use the native helpers rather than implementing another limiter.

## 4. Retire the fork patches; bound the HTTP workaround

The removed deviations are `2a183dfb` + `735b5798` (503 behavior and its test) and `2eadd937` (nine P2 phase spans). Record them as **retired by this decision**, not incorporated upstream. Tagged 2.9.0 still has both unbound-workload 404 sites. [S2, S6]

### Required-if-feasible adapter, with an explicit stop rule

WAMN already supplies `DynamicRouter` to native `Ingress`. **Default to implementing and proving a thin delegating `Router` wrapper.** First identify the existing trusted release/deployment data that supplies the expected hostnames unambiguously, including before initial binding:

```text
native NoWorkloadForHost + expected hostname → RouteError::Unavailable → 503
all other native results                    → unchanged
```

Forward every relevant lifecycle and outgoing-policy method, including P3 methods and service hooks. Derive the immutable expected-hostname set once from existing trusted inputs; introduce no independently maintained routing state, second authored inventory, hostname heuristic, per-request database read, or background reconciliation. Do not reconstruct expected hosts solely from successful bind history: that misses initial startup.

**Historical correction:** `Router` and `RouteError::Unavailable` already existed in 2.8.0. Correct the ledger’s claim that no in-tree lever reaches routing status; this is not a new 2.9 feature. [S6]

This adapter cannot change the later **missing-workload-handle** 404, and `RouteError` does not supply the former `Retry-After` header. Accept those differences. Unknown-host and application-level 404s must remain unchanged. Do not rewrite responses based on status/body text or turn all 404s into retryable failures. [S6]

**Stop rule:** if the wrapper needs copied ingress, private access, new routing state machinery, or unavailable hostname semantics, omit it and use native behavior. Omission requires a cited feasibility finding: record the obstacle, fallback and measured restart/rebind window on `wamn-0h0g.17.20` and in the ledger. Do not mark that correctness defect fixed. Exact parity is not required, but a feasible adapter is not discretionary.

Test known-host unbound/bound behavior, unknown host, malformed request, ordinary application 404, and bind/unbind transitions. Prove a routing refusal does not invoke the guest. Characterize the post-routing missing-handle race separately; do not claim the wrapper closes it. Record any residual 404 window on `wamn-0h0g.17.20`, whether or not the adapter lands.

**No blanket retry changes.** A 503, 404 or lost response alone does not authorize resending a mutation. Preserve existing operation-specific retry/idempotency rules and unknown-outcome handling. Startup polling in a bounded test is not a production mutation-retry policy.

## 5. Cut over deployment and operational controls

Upgrade the operator, chart/CRDs and gateway together with the rebuilt WAMN host images through the existing runbook. Preserve WAMN's host binary; do not replace it with stock `wash host`, which lacks WAMN's registered plugins and release integration. Pin and record the actual artifacts, not `latest` or `canary`.

Render each shipped host-group configuration and exercise its arguments with the rebuilt binary. New chart flags/environment variables must be consumed or explicitly disabled; “accepted by Helm” is not proof the WAMN host implements them.

Use native `ProbeState`, `Liveness` and ingress connection-limit checks. Bind the probe listener before announcing readiness; beat liveness from the actual command loop, mark draining before shutdown, and monitor listener/runtime failure. Account for probe ports and NetworkPolicies. Test that the probes answer during data-plane saturation.

**Do not claim these probes remove the rebind outage.** The tagged route-controller still marks its managed endpoints ready, and upstream documents routing paths that bypass Service readiness. Inspect and test WAMN's actual traffic path. Route-aware traffic removal is deferred, not implemented by adding `/readyz`. [S7]

Set termination grace periods above the shutdown sequence actually wired into each binary, including command drain/abort, plugin cleanup and telemetry flush. Check both SIGINT and SIGTERM. Test NATS loss/recovery and operator restart: the release includes protection against deleting hosts merely because the operator lost its bus connection. [S5, S7]

Preserve OCI trust roots, explicit registry credentials, TLS, socket/address confinement, and connection-bound authority.

**Events RBAC: check the removal trigger now.** The pinned 2.9.0 operator RBAC templates still grant Events in `apiGroups: [""]`, not `events.k8s.io`; deletion is not justified by the source check. Render the exact distributed chart with WAMN’s watched namespaces and service account, **without the overlay masking the result**. Check the required `events.k8s.io/events` verbs in every watched namespace and run the Warning Event proof. If sufficient, retire the overlay, its references and ledger row 8’s temporary deviation **in the same stage-3 change**. Otherwise retain it and record why. Do not broaden RBAC or patch the chart to manufacture convergence. [S11]

## 6. Required evidence and completion criteria

Use the existing recipes in `docs/operations/build-and-test.md`, current gate registry, Receiving/WMS journeys and throughput bench. Extend their owning tests; do not add a parallel harness. Run on disposable infrastructure with exclusive use of the measurement machine; no concurrent pilot, cluster journey, build or load run.

| Proof | Required result |
|---|---|
| **Source and build** | Active manifests, lockfiles, tools and images use upstream 2.9.0 with no overrides or local modifications. Normal production builds and the required test configurations compile with the intended feature set and one Wasmtime family. |
| **Application behavior** | Existing Receiving/base-overlay and WMS routes work; typed refusals, authorization, transaction rollback, idempotency and queued delivery remain correct. No fixture-only substitute for the product serving path. |
| **Isolation and authority** | Existing tenant, nested-operation, credential-generation and candidate-binding proofs execute and retain their refusals. Admission still rejects excluded imports; fresh stores do not retain previous-call state. |
| **Limits and cancellation** | §3's memory tests pass; deadline/trap/cancellation release owned resources. No duplicate ticker or silently rescaled timeout. |
| **Routing and recovery** | The adapter passes §4’s tests, or a cited stop-rule finding justifies omission. Record response codes, residual 404 exposure and recovery time on `wamn-0h0g.17.20`. Never make recovery green by broadly accepting all non-success responses. |
| **Operations** | Cold-start bursts do not strand serving/control-plane work; NATS and operator recovery pass; probes report real states; shutdown/flush complete or report a bounded failure within deployment grace. |
| **Telemetry and performance** | Real invocation/effect traces and selected metrics arrive. Measure latency percentiles, throughput, CPU, memory, the ratio and throughput knee, plus cold/restart behavior. Establish the separate 2.9 baseline below; retain 2.8 as the comparison. |

**Re-baseline, not just rerun.** The first fully passing, comparable 2.9 evidence run establishes a new baseline in the existing performance evidence layout. Use the existing benchmark’s repeated-measurement protocol. Record the ratio, selected ceiling and rationale, throughput knee and gate settings, with SHAs, artifacts, resources, profile/features, meter mode and load. Keep the 2.8 numbers and raw evidence unchanged beside the comparison; name any loss of phase comparability after retiring the spans.

**Re-baselining does not relax gates.** Preserve current limits while evaluating 2.9; a selected ceiling may remain unchanged. Threshold changes need measured justification and explicit owner approval, never adjustment merely to make a run green. Investigate material regressions and retain failing runs.

Remove only assertions whose contracts were deliberately retired: fork commit counts, fork-only phase names, and exact patched HTTP behavior. Replace applicable routing assertions with §4’s narrower contract; preserve unrelated correctness and performance gates.

Replace `tools/fork-sync-check` with `tools/wasmcloud-release-check`, updating its test and recipe. Validate the peeled commit and clean source, not a named fork branch or patch count. Retain the runtime-test and isolated Git-template-fixture legs where supported, recording their feature sets and executed/skipped cases; no upstream formatting or fixture repair may modify the dependency. Update all active callers in the same change. [S2]

**A skipped or unavailable proof is not green.** Separate pre-existing failures, new failures and unexecuted tests with exact commands and exit codes. Report incomplete validation as blocked; do not mark the cutover complete or weaken unrelated gates to conceal it. Existing unrelated failures require explicit disposition before a release-readiness claim.

## 7. Follow-ons, not prerequisites

| Follow-on | Boundary for subsequent work |
|---|---|
| **P3 HTTP cutover** | Reuse `wamn-0h0g.17.26` and `evidence/perf/2026.09/p3-probe.md`. Port the actual shell, authority handling, bounded body streaming, P2-specific harnesses and manifests; prove one P3 artifact through both in-process and cluster paths. Preserve fresh stores. No P2 compatibility track unless a real consumer requires it. |
| **Native `DispatchTarget` / `GuestCall`** | Probe one real node and its WAMN authority/context before replacing manual lifecycle code. No runtime rewrite inside dependency adaptation. |
| **HTTP transport pooling** | Continue the filed authority-preserving probe. P3, warm stores and outbound connection pooling are independent choices. |
| **Plugin bindings, native NATS, warm reuse** | Evaluate under existing owners when needed. Do not enable new tenant imports/providers, replace the run/lease model, or enable persistent guest state during this cutover. |

The P3 report demonstrates feasibility, not a completed migration or an isolated performance improvement. The new dispatch API supports fresh calls; adopting it does not require warm reuse. [S8–S9]

## 8. Delivery and agent handoff

Use this charter for the **next platform wave**, in reviewable stages:

| Stage | Scope |
|---|---|
| **1** | Baseline and direct upstream pin. |
| **2** | Embedding/build compatibility, including any necessary coupled registry update. |
| **3** | Patch retirement, required-if-feasible router adapter, deployment wiring and Events RBAC disposition. |
| **4** | Full evidence, the 2.9 performance baseline and documentation. |

**Serial-wave fence:** coordinate exclusive ownership through the existing integrator/Beads workflow before stage 1. Until stage 3 lands and the integrator releases the fence, no other lane edits `services/host`, `services/executor`, `crates/platform/runtime`, `crates/execution/host/src/router_driver.rs`, `services/ctl/src/dev/`, `deploy/`, or the shared manifests/lockfiles and tests this cutover changes. Sequence the effects, HTTP-pooling and TUI-loop work after that boundary; a separate worktree does not remove the conflict. If ownership is unavailable, report the wave blocked rather than race another lane. Machine-wide execution stays exclusive, and stage-4 measurements run on a fixed tree without competing builds or workloads. Do not launch the §7 follow-ons inside this wave.

Update the native-alignment ledger, current build/deploy recipes, runtime inventory checks and stale dependency comments. Historical references may still name the fork; active dependencies and procedures must not. The ledger records zero carried patches, adapter disposition and `.17.20` evidence, retired diagnostic coverage, explicit meter/memory settings, any virtualizer/registry coupling, the Events RBAC trigger result and remaining follow-ons.

Return one implementation summary with: before/after SHAs and artifacts; changed integration points and deletions; adapter outcome and residual gap; registry/RBAC disposition; test commands/results/skips; the 2.9 baseline versus 2.8 and any approved gate-setting changes; open Beads; and release-readiness verdict. Findings stay in Beads, not a new competing findings document.

A serious regression stops promotion. Recovery uses the existing prior deployment/release procedure, subject to its compatibility checks; do not create runtime fallback to the fork or assume runtime rollback reverses database changes. This work order grants no production rollout or upstream issue/PR authority beyond the existing operator workflow.

## Source anchors

Paths below are relative to the pinned repositories. They identify the basis for this work order; agents must check current WAMN call sites before editing.

- **[S1] WAMN context:** `2a4cd2886f6b637a62413b4abbab4fbe4a4d964f`; root `Cargo.toml`, `CLAUDE.md`, and that commit's recorded sweep result. Repository: `https://github.com/dkkloimwieder/wamn`.
- **[S2] Current integration:** WAMN `docs/architecture/native-alignment-ledger.md`, `tools/fork-sync-check`, `tests/conformance/tests/fork_sync_check.rs`, `services/host/src/host.rs`, `services/executor/src/lib.rs`, `crates/execution/host/src/router_driver.rs`.
- **[S3] Upstream identity/build:** `v2.9.0` peels to `68ebece9c537f8bb4b5c9999f274ec68d60f35a9`; root and `crates/wash-runtime/Cargo.toml`, `charts/runtime-operator/Chart.yaml`. Release: `https://github.com/wasmCloud/wasmCloud/releases/tag/v2.9.0`.
- **[S4] Memory:** upstream `crates/wash-runtime/src/engine/{host_memory,guest_memory,ctx}.rs`; changes #5506 and #5511. Public helper locations checked against the tagged source, not only PR descriptions.
- **[S5] Embedding/lifecycle:** upstream `crates/wash-runtime/src/washlet/mod.rs` and observability modules; changes #5522, #5524, #5525, #5534 and #5542.
- **[S6] HTTP:** upstream `crates/wash-runtime/src/host/http.rs`: `Router`, `RouteError`, `Ingress`, routing error branch and missing-workload-handle branch. Historical API comparison: the same path at peeled v2.8.0 commit `5c4ec4a3d008b3f401d9e763515f434deebc9936` already exposes `Router` and `Unavailable → 503`.
- **[S7] Health/deployment:** upstream `crates/wash-runtime/src/host/probes.rs`, `runtime-operator/internal/controller/runtime/workload_route_controller.go`; changes #5502, #5530 and #5536. Do not substitute the earlier probe PR's intended traffic behavior for the tagged routing implementation.
- **[S8] P3:** WAMN `evidence/perf/2026.09/p3-probe.md`, `components/ingress/http-route/src/guest.rs`, and the current journey publication recipes.
- **[S9] Optional native dispatch:** upstream `crates/wash-runtime/src/engine/dispatch.rs`, change #5545.
- **[S10] Registry lineage:** WAMN root `Cargo.toml`, `crates/platform/component-virtualizer/Cargo.toml`, `docs/architecture/2a-capability-registry.md`, and native-alignment ledger row 5. At [S1], WASI-Virt is separately pinned to `448f6df8f688cee5d6995e96b1ffc31f9bf00742`; adapter SHA-256 `28eff8a2255812b440fbad2784a5a87660321e667331c17fb9a95f29caa85632`.
- **[S11] Events RBAC:** upstream [S3] `charts/runtime-operator/templates/operator/{clusterrole,workload-namespace-role,role}.yaml`; WAMN native-alignment ledger row 8. These tagged templates grant core-group Events only; render and test the actual deployment before changing the overlay.
