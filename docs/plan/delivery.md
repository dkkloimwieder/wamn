# WAMN development, testing and delivery

**Status:** draft 0.4 for review · 2026-09-12  
**Scope:** Rust-first platform and application development, validation, testing, publication and deployment.

Replaces the incremental-loop draft’s fresh-environment-per-run and publication-per-edit assumptions. This is a proposal, not an implementation-status report. It changes workflow, not the application’s security or business contracts.

## 1. Decision

**Development runs local code. Tests establish behavior. Repository commands build and qualify releases; CI/CD automates their execution and publication. Deployment selects an effective release and runs the exact artifacts it pins.**

```text
Local:   edit → incremental build/check → run or targeted test
Review:  validate the change → required tests → integration permitted
Release: build selected integrated revision → test artifacts → publish
Deploy:  select qualified effective release → configure target → load pinned artifacts → readiness/smoke checks
```

Cargo owns Rust dependencies and targets; application declarations own application contracts; repository commands own build/check/test behavior; CI configuration schedules those commands. No particular source host, CI service or registry provider is required. Other representations are derived output or documentation. No package-role registry, duplicate dependency graph, gate catalog or additional approval lifecycle. Use Cargo’s existing metadata and test selection. [3]

## 2. Minimum correctness requirements

These are requirements on the product and its release checks, **not instructions to rerun every check on every save**.

| Boundary | Required now |
|---|---|
| **Build and contracts** | Compile supported native and guest targets. Generate from authoritative inputs. Verify the complete selected application SQL corpus against its effective PostgreSQL schema; reject unsupported value types and invalid operation/route/wiring references. The executed SQL must match the verified corpus. |
| **Authority** | Enforce authentication, direct/nested operation permissions, invocation scope, admitted capabilities, database privileges/RLS and destination restrictions through the actual runtime. CI success, a local candidate or a cache hit grants no additional authority. |
| **State and outcomes** | Preserve command transaction ownership, rollback/cleanup, concurrency rules and declared idempotency. Distinguish refusal, confirmed completion, partial completion and unknown completion. No automatic mutation replay merely because a request failed; retries follow the operation’s declared replay contract. |
| **Artifact and target consistency** | Identify the exact built artifacts, load a coherent application assembly, and establish the target’s required schema, permissions and bindings before serving it. Missing or incompatible output fails explicitly. |
| **Executable evidence** | Run meaningful success and refusal/failure tests against production code. Required tests fail on errors, unavailable prerequisites or zero expected cases executed. Compilation-only, ignored and self-skipped tests are not passing evidence. |

SQLx verifies SQL/type compatibility; it does not replace execution tests for permissions, business invariants, locks or rollback. Preserve that distinction without adding another SQL parser or verification system. [1]

## 3. Short development loop

### One disposable session, not one environment per edit

Keep the required services and application database running during interactive development. Saved, uncommitted source may run locally. No OCI upload, publication endpoint, permanent release record or handwritten evidence is required to execute an edit.

Separate **application assembly/loading** from **registry retrieval and publication**. Use the same validators, loader, capabilities and authorization in both paths. Reuse the generated application description that identifies components, contracts, SQL and wiring; do not create a development-only model.

| Change | Repeat in development |
|---|---|
| **Rust implementation only** | Cargo rebuilds affected targets; rerun selected tests and load changed components after their artifact/import checks. Reuse unchanged schema, SQL metadata and generated contracts. |
| **Named SQL** | Refresh the relevant generation and database verification; rebuild and run the affected command/query tests. Reuse the schema environment. |
| **Contract, route or wiring** | Regenerate/check the affected package and its consumers. Package-wide invalidation is acceptable. |
| **Schema/bootstrap inputs, including base/overlay migrations** | Recreate the owned application database, then regenerate and verify. Do not introduce in-place migration support. |
| **Permissions or bindings** | Apply and verify the changed target configuration; invalidate affected authorization results and retire obsolete resources. An old build check does not approve new grants. |

Build before replacing the serving candidate. Keep the previous candidate usable while its target remains intact; otherwise mark the session unavailable. Restarting is sufficient. Bound completion/cancellation during replacement; never silently replay interrupted mutations. Database resets invalidate client records, revisions and pending submissions.

Provide reset and clean-check operations. Reset only owned disposable resources. **Database-backed tests always get their own test-owned databases, which may use the same PostgreSQL server, and must never connect to, read, write, migrate, reset or drop the interactive session’s database.**

### Reuse existing tools first

Use Cargo incremental builds and SQLx `.sqlx` metadata for offline compilation. Set `SQLX_OFFLINE=true` for ordinary builds; refresh metadata on SQL/schema changes and check it online in CI with `cargo sqlx prepare --check`. Use the pinned CLI and real verifier targets/features, not every possible feature combination. [4]

SQLx metadata is not a permission verdict. WAMN reuse must account for the check’s actual inputs, including schema, grants and checker changes where relevant. Missing or stale required metadata causes refresh or failure, never unchecked execution.

Start with coarse package/stage reuse; uncertainty means rerunning more work. Keep caches disposable; commit `.sqlx` only where required for builds. No dependency analyzer or replacement for Cargo’s freshness logic.

## 4. Provider-independent delivery automation

Use the chosen hosted or self-hosted CI runner to invoke the same repository build/check/test commands used locally. Source hosting, CI execution, artifact storage and the deployment target are separate choices. GitHub, a pull-request API and a particular registry service are not product dependencies.

Configure checks before integration and qualification/publication after integration to the selected branch, initially `main`; a release tag or explicit invocation may also select a release candidate. Provider-specific configuration handles triggers, credentials, job ordering and artifact retention. It must not contain a second implementation of WAMN validation or require WAMN to introduce a CI abstraction framework.

Automation is the recommended delivery workflow, not a runtime dependency. Without a CI service, an authorized operator can run the same commands from the exact selected source on a controlled machine. Required checks and artifact identity remain the same; manual execution is not an unchecked publication path.

| Point | Minimum work |
|---|---|
| **Before merge** | Compile and run the ordinary unit/property suites, SQL/schema checks and affected application integration tests. Run deployed tests when the changed boundary requires them. Keep selection broad initially; shared runtime, generator or security changes exercise all supported application fixtures. |
| **Publication branch** | Qualify the actual integrated revision using pinned tools and locked dependencies. Reconstruct clean schema/test state, verify the selected corpus and contracts, build deployment artifacts, and execute required artifact-backed application and packaging tests. Publish only a passing candidate. |
| **Deployment** | Select the qualified **effective release** by its existing scoped `effective_release_id` and immutable manifest digest. Fetch only the artifacts it pins, establish target configuration, load the release, then check readiness and a meaningful authenticated smoke interaction. Do not rebuild. Serialize deployments per environment; activation follows the selected release and the platform’s existing release/activation ordering, never CI run order. |

**The deployable selection is a release.** The existing scoped `effective_release_id` names one immutable manifest and the artifacts it pins; use that platform identity, not a new CI-specific release identifier. Qualification evidence names that exact release. Re-qualifying it does not change its identity; a rebuilt candidate with different contents must receive a distinct release identity through the existing minting path. The same source commit can therefore produce separate releases without making them interchangeable. Before activation, confirm the requested release is still the environment’s selected deployment; a superseded release must not activate because its run finished later. Source revision, CI run ID and completion time are provenance, not deployment precedence. No new ordering service or approval lifecycle is introduced.

Qualify the selected integrated bytes, not merely an earlier review build. The runner may reuse dependency/build caches, not local cached “passed” verdicts. Uploading a candidate for deployment tests is allowed; it does not qualify the release or activate a shared environment.

Automatically record **source revision → build/test execution → effective release identity and manifest digest → deployed effective release**, plus test logs and failure inputs; the manifest supplies the pinned artifact identities. A CI run identifier may be included but is not required by the runtime. Keep referenced release artifacts and bounded execution diagnostics. No handwritten hash reports, committed run narratives or manually reconciled receipts.

Keep publication/deployment credentials out of artifacts, logs and jobs executing untrusted changes. Supply endpoints, secrets and target bindings at deployment. A digest identifies bytes; it does not authorize deployment.

**Schema lifecycle remains fresh-install-only.** Code-only replacement may reuse a compatible database. Schema-changing deployment must explicitly provision a new disposable/test target or refuse an unsupported existing-data upgrade. No silent database reset, implied in-place migration, or claim that code rollback reverses database changes.

## 5. Testing: complementary methods, one implementation

Applications own fixtures and business assertions; shared test support owns infrastructure and invocation. Use readable Rust, not an invariant registry, test DSL or mandatory per-test mutation matrix.

| Method | Minimum use | Limit |
|---|---|---|
| **Unit and property tests** | Examples, boundary inputs, meaningful refusals and regressions for real decision functions. Retain bounded Proptest coverage where input combinations matter. | Generated samples are not a proof over every possible input. |
| **Stateful histories / DST** | Retain existing router simulation and application history tests. Generate bounded command sequences; control time/effects only where the test needs that control. | A seed does not control PostgreSQL’s internal scheduling. Full-platform DST is not a prerequisite. |
| **Guest + real PostgreSQL** | Exercise supported commands through actual guest/capability code with production-equivalent grants: commit, intermediate rollback, idempotency, authorization and controlled contention where applicable. | Native-only or mocked tests cannot establish this boundary. |
| **Broker and deployed tests** | Exercise real delivery for event features, plus a small packaged application journey. Run process/lifecycle failures when those guarantees are affected. | A successful local invocation does not prove packaging, broker configuration or shutdown behavior. |
| **UI interactions** | Test request construction and the implemented submission/retry states where UI code changes. | A compiling generated client does not establish workflow correctness. |

Property tests generate and shrink inputs; DST controls the environment. They may be used together, not adopted as competing platforms. Proptest supports generated transition sequences against a small reference model. [2, 5]

Receiving’s minimum cases cover quantity/status rules, stale revisions, same-key replay, changed-body refusal, competing receipts, rollback and commit-with-lost-response. Inspect committed state independently of returned JSON. Its event tests require actual redelivery, idempotent inspection creation and poison handling with later progress. Test the implemented failure-record and retention behavior. When native-alignment C lands, update those assertions to broker advisories and retention-limited payload recovery; this workflow change does not wait for C.

Use synthetic fixtures under §3’s test-database isolation rule. Coordinate contention on separate connections rather than rely on timing luck. Replay stays test-only; no production capture.

Retain minimized regressions and reproduction inputs. Required failures block their checks; “known” or “classified” is not passed. Repair or remove obsolete tests, not build a baseline-exception system.

## 6. Useful later—not prerequisites

| Addition | When justified |
|---|---|
| Per-operation caching, database templates, remote build caches, precise change-based test selection | Coarse reuse and the shorter workflow still leave a measured bottleneck. |
| Broader DST, additional replay adapters, large generated campaigns | A specific state/failure guarantee lacks economical coverage. Keep bounded existing tests now. |
| Automated mutation campaigns and coverage-guided fuzzing | Targeted defects justify them. Focused negative controls remain useful now; a mutant must fail for the intended reason. |
| Kani, other formal verification, Bolero | Deferred. No automatic reopening trigger or dependency on current testing. |
| Advanced attestations, multi-stage promotion, GitOps controllers, canaries, automatic rollback | An actual deployment or supply-chain requirement demands them. Basic artifact integrity, credential isolation and deployment authorization remain required now. |

No percentage-coverage target, multi-environment rollout program or repeated performance campaign is required to shorten the loop.

## 7. Delivery and acceptance

Deliver **local loading without publication; persistent disposable development with coarse reuse; provider-independent release commands automated by the chosen CI/CD runner**. Reuse existing tests and remove superseded orchestration/instructions. Neither a complete cache framework nor full DST is a prerequisite.

The change is complete when:

1. A code-only edit runs from saved source without registry/publication service access or database recreation; unchanged SQL is not checked online again. Changed invalid SQL, incompatible contracts and forbidden imports still fail before use.
2. A schema change rebuilds only the owned disposable target. Test setup, execution and cleanup use only test-owned databases; a test runner pointed at the interactive session’s database refuses before running. Cache-empty and incremental checks agree on deterministic generated artifacts and validation verdicts, including removed operations and missing/corrupt cached output.
3. Required change checks block real failures. Qualification, publication and deployment agree on the existing effective release identity, manifest digest and pinned artifacts; deployment does not rebuild. Two builds of the same commit cannot overwrite one release’s contents, and a late-finishing run cannot activate a superseded release. Those commands also run directly on a machine with the documented prerequisites, without a source-host or CI-provider API.
4. The supported application’s real command and packaged journey tests execute, with skips and unavailable legs reported honestly. No unrelated inventories, handwritten hash reports or new approval machinery are required.

Record before/after stage timings for code, SQL and schema edits. No 200-operation fixture or fixed latency claim is a prerequisite.

## Basis

The owner discussion supplies the proposed workflow and superseding decisions. On approval, replace overlapping instructions rather than maintain parallel specifications.

[1] Supplied `sqlx-data-access-spec(1).md`, “Compile-time proves / integration still proves.” Retains the shared SQL corpus and transaction/authority boundary; arbitrary SQL exploration remains out of scope.

[2] `wamn_testing_spec.md`, draft 0.2, and supplied `deterministic-testing-spec.md`: application-owned invariants, bounded histories, synthetic replay and live-test limits. This proposal simplifies delivery/evidence procedure and applies the later Kani deferral and broker-advisory decision.

[3] Cargo documentation: [metadata](https://doc.rust-lang.org/cargo/commands/cargo-metadata.html) and [test selection](https://doc.rust-lang.org/cargo/commands/cargo-test.html).

[4] [SQLx CLI documentation](https://docs.rs/crate/sqlx-cli/latest): offline query metadata and CI checks. Implement with the repository’s compatible pinned version; no SQLx upgrade is implied.

[5] [Proptest state-machine testing](https://proptest-rs.github.io/proptest/proptest/state-machine.html).
